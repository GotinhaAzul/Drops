//! Public API tests for window content capture.
//!
//! These tests use a mock helper command and synthetic context bundles. They do
//! not read the owner's windows, screenshots, browser profiles, or credentials.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use buddy_anchor::capture::{enrich, CaptureConfig};
use serde_json::{json, Value};
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

/// Serialize tests that override command paths through process-wide env vars.
async fn env_guard() -> MutexGuard<'static, ()> {
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());
    ENV_LOCK.lock().await
}

fn window_context() -> Value {
    json!({
        "kind": "window",
        "window": {
            "address": "0x1001",
            "class": "Alacritty",
            "title": "bash",
            "pid": 111,
            "workspace": {"id": 1, "name": "1"},
            "monitor_id": 0,
            "geometry": {"x": 0, "y": 0, "width": 1000, "height": 500},
        },
        "workspace": {"id": 1, "name": "1"},
        "monitor": {"name": "eDP-1"},
        "visible": true,
        "process": {"pid": 111, "cwd": "/tmp", "available": true},
    })
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!(
        "buddy-capture-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn capture_config_accepts_partial_json() {
    let config: CaptureConfig = serde_json::from_str(r#"{"ocr":false}"#).expect("partial config");
    assert!(config.enabled);
    assert!(config.accessibility);
    assert!(config.terminal);
    assert!(config.screenshots);
    assert!(!config.ocr);
    assert_eq!(config.max_text_chars, 24_000);
    assert_eq!(config.max_image_edge, 1_280);
    assert_eq!(config.timeout_ms, 2_500);
}

#[tokio::test]
async fn workspace_and_unknown_contexts_are_not_captured() {
    let cancel = CancellationToken::new();
    for mut context in [
        json!({"kind": "workspace", "workspace": {"id": 1}}),
        json!({"kind": "headless"}),
        json!({"unrelated": true}),
    ] {
        enrich(
            &mut context,
            &CaptureConfig::default(),
            true,
            cancel.clone(),
        )
        .await
        .expect("capture is best effort");
        assert!(
            context.get("content").is_none(),
            "non-window contexts are left untouched: {context}"
        );
    }
}

#[tokio::test]
async fn disabled_capture_reports_status_without_running_commands() {
    let mut context = window_context();
    let config = CaptureConfig {
        enabled: false,
        ..CaptureConfig::default()
    };

    enrich(&mut context, &config, false, CancellationToken::new())
        .await
        .expect("disabled capture");

    assert_eq!(context["content"]["status"], "disabled");
    assert_eq!(context["content"]["text"], "");
    assert_eq!(context["content"]["sources"], json!([]));
    assert_eq!(context["content"]["warnings"], json!([]));
    assert!(context["content"].get("image").is_none());
}

#[tokio::test]
async fn malformed_window_context_reports_unavailable() {
    let mut context = json!({"kind": "window", "window": {}});
    enrich(
        &mut context,
        &CaptureConfig::default(),
        true,
        CancellationToken::new(),
    )
    .await
    .expect("malformed context");

    assert_eq!(context["content"]["status"], "unavailable");
    assert!(context["content"]["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|warning| warning
            .as_str()
            .unwrap_or_default()
            .starts_with("capture_unavailable:")));
}

#[tokio::test]
async fn cancelled_capture_returns_a_warning_not_stale_content() {
    let mut context = window_context();
    let cancel = CancellationToken::new();
    cancel.cancel();

    enrich(&mut context, &CaptureConfig::default(), true, cancel)
        .await
        .expect("cancelled capture");

    assert_eq!(context["content"]["status"], "unavailable");
    assert!(context["content"]["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|warning| warning
            .as_str()
            .unwrap_or_default()
            .starts_with("capture_cancelled:")));
}

#[tokio::test]
async fn mock_helper_text_is_bounded_and_vision_gated() {
    let _guard = env_guard().await;
    let dir = temp_dir("helper");
    let input_path = dir.join("helper-input.json");
    let script_path = dir.join("mock-python");
    let helper_json = r#"{"sources":[{"kind":"accessibility","scope":"matched-window","text":"hello fixture text"},{"kind":"tmux","scope":"terminal-scrollback","text":"tmux fixture line"}],"warnings":["fixture warning"]}"#;
    let script = format!(
        "#!/bin/sh\ncat > \"{}\"\nprintf '%s\\n' '{}'\n",
        input_path.display(),
        helper_json
    );
    fs::write(&script_path, script).expect("write mock helper");
    let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("chmod mock helper");

    std::env::set_var("DROPS_CAPTURE_PYTHON", &script_path);

    // A disabled config must not start the mock helper at all.
    let mut disabled = window_context();
    let disabled_config = CaptureConfig {
        enabled: false,
        ..CaptureConfig::default()
    };
    enrich(
        &mut disabled,
        &disabled_config,
        false,
        CancellationToken::new(),
    )
    .await
    .expect("disabled mock capture");
    assert_eq!(disabled["content"]["status"], "disabled");
    assert!(
        !input_path.exists(),
        "disabled capture must not invoke the helper"
    );

    let config = CaptureConfig {
        screenshots: false,
        ocr: false,
        max_text_chars: 12,
        ..CaptureConfig::default()
    };

    let mut context = window_context();
    enrich(&mut context, &config, false, CancellationToken::new())
        .await
        .expect("mock capture");

    std::env::remove_var("DROPS_CAPTURE_PYTHON");

    let content = &context["content"];
    assert_eq!(content["status"], "captured");
    assert_eq!(content["text"], "hello fixtur");
    assert!(content.get("image").is_none());

    let sources = content["sources"].as_array().expect("sources");
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["kind"], "accessibility");
    assert_eq!(sources[0]["chars"], 12);
    assert_eq!(sources[0]["truncated"], true);
    assert_eq!(sources[1]["kind"], "tmux");
    assert_eq!(sources[1]["chars"], 0);
    assert_eq!(sources[1]["truncated"], true);

    let warnings = content["warnings"].as_array().expect("warnings");
    assert!(warnings.iter().any(|warning| warning
        .as_str()
        .unwrap_or_default()
        .starts_with("text_helper:")));
    assert!(warnings.iter().any(|warning| warning
        .as_str()
        .unwrap_or_default()
        .starts_with("text_truncated:")));

    // The helper saw the ordinary context bundle, no previous content, and
    // helper-side feature flags.
    let input = fs::read_to_string(&input_path).expect("helper stdin");
    assert!(input.contains("\"address\":\"0x1001\""), "{input}");
    assert!(!input.contains("\"content\""), "{input}");
    assert!(input.contains("\"capture_options\""), "{input}");
    assert!(input.contains("\"accessibility\":true"), "{input}");
    assert!(input.contains("\"terminal\":true"), "{input}");

    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn locked_context_refuses_before_the_helper_starts() {
    // The lock-refusal decision with a positive compositor flag is covered by
    // pure unit tests. Here only exercise the fail-closed headless path.
    if buddy_anchor::desktop_available().is_ok() {
        return;
    }

    let _guard = env_guard().await;
    let dir = temp_dir("locked");
    let marker = dir.join("helper-ran");
    let script_path = dir.join("mock-python");
    let script = format!(
        "#!/bin/sh\ntouch \"{}\"\nprintf '%s\\n' '{{}}'\n",
        marker.display()
    );
    fs::write(&script_path, script).expect("write mock helper");
    let mut permissions = fs::metadata(&script_path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("chmod mock helper");

    std::env::set_var("DROPS_CAPTURE_PYTHON", &script_path);

    let mut context = window_context();
    context["screen_locked"] = json!(true);
    let result = enrich(
        &mut context,
        &CaptureConfig::default(),
        true,
        CancellationToken::new(),
    )
    .await;

    std::env::remove_var("DROPS_CAPTURE_PYTHON");

    result.expect("locked capture is reported, not failed");
    assert_eq!(context["content"]["status"], "unavailable");
    assert!(context["content"]["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .any(|warning| warning
            .as_str()
            .unwrap_or_default()
            .starts_with("screen_locked:")));
    assert!(
        !marker.exists(),
        "the text helper must not start while the lock state is unconfirmed"
    );

    let _ = fs::remove_dir_all(dir);
}
