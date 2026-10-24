//! Bounded workspace discovery without granting shell access.
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{optional_u64, string_arg, ToolContext, ToolOutcome};
use crate::paths;

pub(super) fn definition(search: bool) -> Value {
    let mut properties = json!({
        "path": {"type":"string", "description":"Workspace directory, or file for search. Defaults to '.' ."},
        "limit": {"type":"integer", "minimum":1, "maximum":200, "description":"Maximum entries or matching lines (default 100)."}
    });
    if search {
        properties["query"] = json!({"type":"string", "minLength":1, "description":"Case-sensitive literal text; not a regular expression."});
    }
    json!({"type":"function", "function": {
        "name": if search {"search_files"} else {"list_files"},
        "description": if search {
            "Search UTF-8 workspace text recursively, returning paths, 1-based line numbers and excerpts. Skips hidden entries, symlinks, binary/large files. Bounded scan; truncated=true means results are incomplete."
        } else {
            "List one workspace directory, including hidden names, with entry types and sizes. Does not follow symlinks. Use returned directory paths to explore further."
        },
        "parameters":{"type":"object", "properties":properties, "required":if search {vec!["query"]} else {vec![]}}
    }})
}

pub(super) fn execute(
    ctx: &ToolContext,
    args: &Value,
    search: bool,
    cancel: &CancellationToken,
) -> ToolOutcome {
    match run(ctx, args, search, cancel) {
        Ok(value) => ToolOutcome::ok(value.to_string()),
        Err(err) => ToolOutcome::error(format!("file discovery: {err:#}")),
    }
}

fn run(
    ctx: &ToolContext,
    args: &Value,
    search: bool,
    cancel: &CancellationToken,
) -> anyhow::Result<Value> {
    let requested = match args.get("path") {
        None => ".",
        Some(value) => value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path must be a string"))?,
    };
    let start = paths::confine(&ctx.root, requested)?;
    let limit = optional_u64(args, "limit").unwrap_or(100).clamp(1, 200) as usize;
    let query = if search {
        string_arg(args, "query").map_err(|e| anyhow::anyhow!(e.content))?
    } else {
        ""
    };
    if search && query.is_empty() {
        anyhow::bail!("query must not be empty");
    }
    let mut pending = vec![start];
    let mut results = Vec::new();
    let mut visited = 0;
    let mut bytes_read = 0;
    let mut skipped = 0;
    let mut truncated = false;
    while let Some(path) = pending.pop() {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        visited += 1;
        if visited > 5000 {
            truncated = true;
            break;
        }
        // Re-check every descendant; never follow directory symlinks.
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            skipped += 1;
            continue;
        }
        let path = paths::confine(&ctx.root, &path.to_string_lossy())?;
        if metadata.is_dir() {
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&path)? {
                if entries.len() >= 5000 {
                    truncated = true;
                    break;
                }
                entries.push(entry?);
            }
            entries.sort_by_key(|entry| entry.file_name());
            if search {
                for entry in entries.into_iter().rev() {
                    if entry.file_name().to_string_lossy().starts_with('.') {
                        skipped += 1;
                        continue;
                    }
                    if pending.len() >= 5000 {
                        truncated = true;
                        break;
                    }
                    pending.push(entry.path());
                }
            } else {
                for entry in entries {
                    if results.len() == limit {
                        truncated = true;
                        break;
                    }
                    let meta = std::fs::symlink_metadata(entry.path())?;
                    results.push(json!({"path":paths::display_path(&ctx.root, &entry.path()),
                        "type":if meta.file_type().is_symlink() {"symlink"} else if meta.is_dir() {"directory"} else if meta.is_file() {"file"} else {"other"},
                        "bytes":meta.len()}));
                }
                break;
            }
        } else if search && metadata.is_file() {
            if metadata.len() > 1024 * 1024 {
                skipped += 1;
                continue;
            }
            bytes_read += metadata.len();
            if bytes_read > 16 * 1024 * 1024 {
                truncated = true;
                break;
            }
            // Read at most 1 MiB even if the file grows after metadata was read.
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&path)?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 || bytes.contains(&0) {
                skipped += 1;
                continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                skipped += 1;
                continue;
            };
            for (index, line) in text.lines().enumerate() {
                if line.contains(query) {
                    if results.len() == limit {
                        truncated = true;
                        break;
                    }
                    results.push(json!({"path":paths::display_path(&ctx.root, &path), "line":index+1,
                        "text":line.chars().take(200).collect::<String>(), "line_truncated":line.chars().count()>200}));
                }
            }
            if truncated {
                break;
            }
        } else if !search {
            anyhow::bail!("list_files requires a directory");
        }
    }
    Ok(json!({"results":results, "truncated":truncated, "skipped":skipped}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn discovers_and_searches_with_limits() {
        let temp = TempDir::new();
        let ctx = ToolContext {
            root: paths::canonical_root(temp.path()).unwrap(),
            allow_shell: false,
            skills: vec![],
        };
        std::fs::create_dir(ctx.root.join("notes")).unwrap();
        std::fs::write(
            ctx.root.join("notes/daily.txt"),
            "hello coworker\nhello again",
        )
        .unwrap();
        std::fs::write(ctx.root.join(".hidden"), "hello secret").unwrap();
        let cancel = CancellationToken::new();
        let listing = run(&ctx, &json!({}), false, &cancel).unwrap();
        assert_eq!(listing["results"].as_array().unwrap().len(), 2);
        let found = run(&ctx, &json!({"query":"hello", "limit":1}), true, &cancel).unwrap();
        assert_eq!(found["results"][0]["path"], "notes/daily.txt");
        assert_eq!(found["results"][0]["line"], 1);
        assert_eq!(found["truncated"], true);
        assert!(run(&ctx, &json!({"path":"../outside"}), false, &cancel).is_err());
        assert!(run(&ctx, &json!({"query":""}), true, &cancel).is_err());
        cancel.cancel();
        assert!(run(&ctx, &json!({}), false, &cancel).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn search_skips_symlinks_and_binary_files() {
        let temp = TempDir::new();
        let root = paths::canonical_root(&temp.path().join("root")).unwrap();
        std::fs::write(temp.path().join("outside"), "needle").unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
        std::fs::write(root.join("binary"), b"needle\0").unwrap();
        let ctx = ToolContext {
            root,
            allow_shell: false,
            skills: vec![],
        };
        let found = run(
            &ctx,
            &json!({"query":"needle"}),
            true,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(found["results"], json!([]));
        assert_eq!(found["skipped"], 3);
    }
}
