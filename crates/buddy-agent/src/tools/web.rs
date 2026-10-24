//! Keyless search and bounded public-web reading. No browser state or cookies.
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{redirect::Policy, Url};
use scraper::{Html, Selector};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{optional_u64, string_arg, ToolOutcome};

const MAX_BODY: usize = 1024 * 1024;
const SEARCH_URL: &str = "https://html.duckduckgo.com/html/";

pub(super) fn definition(search: bool) -> Value {
    json!({"type":"function", "function": {
        "name":if search {"web_search"} else {"web_fetch"},
        "description":if search {
            "Search DuckDuckGo for public information, returning titles, URLs and snippets. No API key needed. Sends the query to DuckDuckGo and requires user approval. Never include private file contents or secrets. Challenge/rate-limit responses are errors."
        } else {
            "Read a public HTTP(S) page as bounded text, without JavaScript, browser logins or cookies. Returns source URL, title and text. Requires approval before sending the URL. Private/local network addresses and nonstandard ports are blocked."
        },
        "parameters":if search {json!({"type":"object", "properties": {
            "query":{"type":"string","minLength":1,"maxLength":500},
            "limit":{"type":"integer","minimum":1,"maximum":10,"description":"Maximum results, default 5."}
        }, "required":["query"]})} else {json!({"type":"object", "properties":{
            "url":{"type":"string","description":"Public HTTP(S) URL. No credentials."},
            "max_chars":{"type":"integer","minimum":100,"maximum":12000,"description":"Maximum text characters, default 8000."}
        },"required":["url"]})}
    }})
}

pub(super) async fn execute(args: &Value, search: bool, cancel: &CancellationToken) -> ToolOutcome {
    let operation = async {
        let (url, limit) = if search {
            let query = string_arg(args, "query").map_err(|e| anyhow::anyhow!(e.content))?;
            if query.trim().is_empty() || query.chars().count() > 500 {
                bail!("query must contain 1–500 characters");
            }
            let mut url = Url::parse(SEARCH_URL)?;
            url.query_pairs_mut().append_pair("q", query);
            (
                url,
                optional_u64(args, "limit").unwrap_or(5).clamp(1, 10) as usize,
            )
        } else {
            let url = string_arg(args, "url").map_err(|e| anyhow::anyhow!(e.content))?;
            (
                Url::parse(url).context("invalid URL")?,
                optional_u64(args, "max_chars")
                    .unwrap_or(8000)
                    .clamp(100, 12000) as usize,
            )
        };
        let (final_url, kind, bytes) = fetch(url, search).await?;
        let text = std::str::from_utf8(&bytes).context("page is not UTF-8 text")?;
        if search {
            parse_search(text, limit)
        } else {
            parse_page(&final_url, &kind, text, limit)
        }
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(anyhow::anyhow!("cancelled")),
        result = tokio::time::timeout(Duration::from_secs(25), operation) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("request timed out after 25 seconds"))),
    };
    match result {
        Ok(value) => ToolOutcome::ok(bounded_output(value)),
        Err(error) => ToolOutcome::error(format!("web: {error:#}")),
    }
}

// Keep structured JSON intact under the registry's 60 KiB output cap.
fn bounded_output(mut value: Value) -> String {
    loop {
        let output = value.to_string();
        if output.len() <= 55_000 {
            return output;
        }
        value["truncated"] = json!(true);
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            value["text"] = json!(short(text, text.chars().count() / 2));
        } else if let Some(results) = value.get_mut("results").and_then(Value::as_array_mut) {
            results.pop();
        } else {
            return "{\"truncated\":true}".to_string();
        }
    }
}

fn validate_url(url: &Url) -> Result<()> {
    if url.as_str().len() > 4096 {
        bail!("URL exceeds 4096 bytes");
    }
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only HTTP(S) URLs are supported");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("URLs with credentials are blocked");
    }
    if !matches!(
        (url.scheme(), url.port_or_known_default()),
        ("http", Some(80)) | ("https", Some(443))
    ) {
        bail!("nonstandard ports are blocked");
    }
    if url.host_str().is_none() {
        bail!("URL needs a host");
    }
    Ok(())
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            // Only native global unicast. Excludes mapped IPv4, local, multicast,
            // translation, 6to4, Teredo, benchmarking and documentation space.
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

async fn fetch(mut url: Url, search: bool) -> Result<(Url, String, Vec<u8>)> {
    for _ in 0..=5 {
        validate_url(&url)?;
        if search && url.host_str() != Some("html.duckduckgo.com") {
            bail!("unexpected search redirect");
        }
        let host = url.host_str().context("missing host")?;
        let port = url.port_or_known_default().context("missing port")?;
        let addresses: Vec<SocketAddr> =
            if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
                vec![SocketAddr::new(ip, port)]
            } else {
                tokio::net::lookup_host((host, port))
                    .await
                    .context("DNS lookup failed")?
                    .collect()
            };
        if addresses.is_empty() || addresses.iter().any(|addr| !public_ip(addr.ip())) {
            bail!("private or reserved network destination blocked");
        }
        // Pin the validated DNS answers and disable proxies and automatic redirects.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .resolve_to_addrs(host, &addresses)
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20))
            .user_agent("Drops/0.1 (desktop assistant)")
            .build()?;
        let response = client
            .get(url.clone())
            .header("Accept", "text/html, text/plain, application/json")
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP request failed"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .context("redirect missing location")?
                .to_str()?;
            url = url.join(location).context("invalid redirect")?;
            continue;
        }
        if !response.status().is_success() {
            bail!("HTTP status {}", response.status().as_u16());
        }
        let kind = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(kind.starts_with("text/")
            || kind.starts_with("application/json")
            || kind.starts_with("application/xhtml+xml"))
        {
            bail!("unsupported content type; only text, HTML and JSON are supported");
        }
        let bytes = read_body(response).await?;
        return Ok((url, kind, bytes));
    }
    bail!("too many redirects")
}

async fn read_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        bail!("page exceeds 1 MiB limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("failed to read response"))?
    {
        if bytes.len() + chunk.len() > MAX_BODY {
            bail!("page exceeds 1 MiB limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn selector(text: &str) -> Selector {
    Selector::parse(text).expect("static selector")
}
fn short(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn result_url(href: &str) -> Option<Url> {
    let base = Url::parse(SEARCH_URL).ok()?;
    let mut url = base.join(href).ok()?;
    if url
        .host_str()
        .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
    {
        let target = url
            .query_pairs()
            .find(|(key, _)| key == "uddg")?
            .1
            .into_owned();
        url = Url::parse(&target).ok()?;
    }
    validate_url(&url).ok()?;
    Some(url)
}

fn parse_search(text: &str, limit: usize) -> Result<Value> {
    let html = Html::parse_document(text);
    if html
        .select(&selector(
            "#challenge-form, #anomaly-modal, .anomaly-modal, form[action*='anomaly']",
        ))
        .next()
        .is_some()
    {
        bail!("DuckDuckGo presented a bot challenge; search is temporarily unavailable");
    }
    let mut results = Vec::new();
    for row in html.select(&selector(".result")) {
        let Some(link) = row.select(&selector("a.result__a")).next() else {
            continue;
        };
        let Some(url) = link.value().attr("href").and_then(result_url) else {
            continue;
        };
        let title = normalized(&link.text().collect::<String>());
        let snippet = row
            .select(&selector(".result__snippet"))
            .next()
            .map(|el| normalized(&el.text().collect::<String>()))
            .unwrap_or_default();
        results.push(
            json!({"title":short(&title,300), "url":url.as_str(), "snippet":short(&snippet,1000)}),
        );
        if results.len() >= limit {
            break;
        }
    }
    if results.is_empty()
        && html
            .select(&selector(".no-results, .no-results__message"))
            .next()
            .is_none()
    {
        bail!("DuckDuckGo returned no recognizable results (blocked or page format changed)");
    }
    Ok(json!({"source":"DuckDuckGo", "results":results, "untrusted":true}))
}

fn parse_page(url: &Url, kind: &str, text: &str, limit: usize) -> Result<Value> {
    let (title, text) = if kind.contains("html") {
        let html = Html::parse_document(text);
        let title = html
            .select(&selector("title"))
            .next()
            .map(|el| normalized(&el.text().collect::<String>()))
            .unwrap_or_default();
        let mut visible = String::new();
        for node in html.tree.nodes() {
            if let scraper::node::Node::Text(value) = node.value() {
                if node.ancestors().any(|parent| matches!(parent.value(), scraper::node::Node::Element(el) if matches!(el.name(), "script"|"style"|"noscript"|"template"|"svg"|"head"))) {continue;}
                visible.push_str(value);
                visible.push('\n');
            }
        }
        (
            title,
            visible
                .lines()
                .map(normalized)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        (String::new(), text.to_string())
    };
    if text.trim().is_empty() {
        bail!("page has no readable text; JavaScript and authenticated pages are not supported");
    }
    Ok(
        json!({"url":url.as_str(), "title":short(&title,300), "text":short(&text,limit), "truncated":text.chars().count()>limit, "untrusted":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddg_results_decode_links_entities_and_report_challenges() {
        let html = r#"<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fdoc">Rust &amp; tools</a><a class="result__snippet">A <b>useful</b> page</a></div>"#;
        let found = parse_search(html, 5).unwrap();
        assert_eq!(found["results"][0]["url"], "https://example.com/doc");
        assert_eq!(found["results"][0]["title"], "Rust & tools");
        assert_eq!(found["results"][0]["snippet"], "A useful page");
        assert!(parse_search("<form id='challenge-form'></form>", 5).is_err());
        assert!(parse_search("unknown format", 5).is_err());
        assert_eq!(
            parse_search("<div class='no-results'></div>", 5).unwrap()["results"],
            json!([])
        );
        assert!(result_url("javascript:alert(1)").is_none());
    }

    #[test]
    fn extracts_text_without_scripts_and_marks_truncation() {
        let url = Url::parse("https://example.com/").unwrap();
        let page = parse_page(&url,"text/html", "<title>Example</title><style>hidden</style><p>Visible &amp; useful</p><script>secret script</script>",100).unwrap();
        assert_eq!(page["title"], "Example");
        assert_eq!(page["text"], "Visible & useful");
        assert_eq!(
            parse_page(&url, "text/plain", "abcdef", 3).unwrap()["truncated"],
            true
        );
    }

    #[test]
    fn blocks_local_reserved_and_credentialed_destinations() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.168.1.1",
            "198.18.0.1",
            "::1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "2001:db8::1",
            "2002:0808:0808::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip(std::net::Ipv4Addr::new(8, 8, 8, 8).into()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
        for url in [
            "file:///etc/passwd",
            "http://user:pass@example.com",
            "https://example.com:8080",
        ] {
            assert!(validate_url(&Url::parse(url).unwrap()).is_err());
        }
    }

    #[tokio::test]
    async fn refuses_local_fetch_and_honors_cancellation() {
        let cancel = CancellationToken::new();
        let result = execute(&json!({"url":"http://127.0.0.1/"}), false, &cancel).await;
        assert!(result.is_error);
        assert!(result.content.contains("destination blocked"));
        cancel.cancel();
        assert!(execute(&json!({"query":"rust"}), true, &cancel)
            .await
            .content
            .contains("cancelled"));
    }

    #[tokio::test]
    async fn body_limit_rejects_oversized_responses() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        // Only this body-reader test bypasses the production public-address check.
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        assert!(read_body(response)
            .await
            .unwrap_err()
            .to_string()
            .contains("1 MiB"));
        server.await.unwrap();
        let output = bounded_output(json!({"text":"\u{0001}".repeat(12000)}));
        assert!(output.len() < 55000);
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["truncated"],
            true
        );
    }

    #[tokio::test]
    #[ignore]
    async fn live_public_page() {
        let result = execute(
            &json!({"url":"https://doc.rust-lang.org/book/"}),
            false,
            &CancellationToken::new(),
        )
        .await;
        println!("{}", result.content);
        assert!(!result.is_error, "{}", result.content);
        let value: Value = serde_json::from_str(&result.content).unwrap();
        assert!(value["title"].as_str().unwrap().contains("Rust"));
        assert!(value["text"].as_str().unwrap().contains("Rust"));
    }

    // Explicitly opt-in: contacts DuckDuckGo with only a fixed public query.
    #[tokio::test]
    #[ignore]
    async fn live_ddg_search() {
        let result = execute(
            &json!({"query":"Rust programming language official", "limit":3}),
            true,
            &CancellationToken::new(),
        )
        .await;
        println!("{}", result.content);
        assert!(!result.is_error, "{}", result.content);
        let value: Value = serde_json::from_str(&result.content).unwrap();
        assert!(!value["results"].as_array().unwrap().is_empty());
    }
}
