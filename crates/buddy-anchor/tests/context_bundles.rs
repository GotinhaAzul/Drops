//! Pure tests for the context bundle built from a Hyprland fixture.

use buddy_anchor::bundle::{window_context, workspace_context};
use buddy_anchor::hit::client_visible;
use buddy_anchor::model::Snapshot;
use buddy_anchor::procfs::ProcessInfo;
use serde_json::Value;

fn snapshot() -> Snapshot {
    serde_json::from_str(include_str!("fixtures/hyprland-two-monitors.json")).expect("fixture")
}

fn client<'a>(snapshot: &'a Snapshot, address: &str) -> &'a buddy_anchor::Client {
    snapshot
        .clients
        .iter()
        .find(|client| client.address == address)
        .expect("client in fixture")
}

fn addresses(windows: &Value) -> Vec<String> {
    windows
        .as_array()
        .expect("windows array")
        .iter()
        .map(|window| window["address"].as_str().expect("address").to_string())
        .collect()
}

#[test]
fn window_context_carries_the_always_on_fields() {
    let snapshot = snapshot();
    let term = client(&snapshot, "0x1001");
    let process = ProcessInfo {
        pid: 111,
        cwd: Some("/home/dasadesktop/Projects/Drops".into()),
        cmdline: vec!["bash".into(), "-l".into()],
        available: true,
        error: None,
    };

    let context = window_context(&snapshot, term, Some(&process));
    assert_eq!(context["kind"], "window");
    assert_eq!(context["window"]["address"], "0x1001");
    assert_eq!(context["window"]["class"], "Alacritty");
    assert_eq!(context["window"]["title"], "bash");
    assert_eq!(context["window"]["pid"], 111);
    assert_eq!(context["window"]["workspace"]["id"], 1);
    assert_eq!(context["window"]["monitor_id"], 0);
    assert_eq!(context["window"]["floating"], false);
    assert_eq!(context["window"]["fullscreen"], 0);
    assert_eq!(context["window"]["geometry"]["x"], 0);
    assert_eq!(context["window"]["geometry"]["y"], 0);
    assert_eq!(context["window"]["geometry"]["width"], 1000);
    assert_eq!(context["window"]["geometry"]["height"], 500);
    assert_eq!(context["monitor"]["name"], "eDP-1");
    assert_eq!(context["visible"], true);

    assert_eq!(context["process"]["pid"], 111);
    assert_eq!(
        context["process"]["cwd"],
        "/home/dasadesktop/Projects/Drops"
    );
    assert_eq!(context["process"]["cmdline"][0], "bash");
    assert_eq!(context["process"]["available"], true);
}

#[test]
fn window_context_omits_process_data_when_unavailable() {
    let snapshot = snapshot();
    let term = client(&snapshot, "0x1001");

    let context = window_context(&snapshot, term, None);
    assert!(context["process"].is_null());
}

#[test]
fn window_context_reports_invisible_windows_as_hidden() {
    let snapshot = snapshot();
    let editor = client(&snapshot, "0x3001");
    assert!(!client_visible(&snapshot, editor));

    let context = window_context(&snapshot, editor, None);
    assert_eq!(context["visible"], false);
    assert_eq!(context["workspace"]["id"], 3);
}

#[test]
fn workspace_context_lists_only_that_workspaces_user_windows() {
    let snapshot = snapshot();
    let context = workspace_context(&snapshot, 1);

    assert_eq!(context["kind"], "workspace");
    assert_eq!(context["workspace"]["id"], 1);
    assert_eq!(context["workspace"]["name"], "1");
    assert_eq!(context["monitor"]["name"], "eDP-1");
    assert_eq!(context["visible"], true);

    let windows = &context["windows"];
    assert_eq!(windows.as_array().map(Vec::len), Some(2));
    assert_eq!(addresses(windows), vec!["0x1002", "0x1001"]);

    // Drops' own surface and the hidden client are excluded.
    for excluded in ["0x4001", "0x1003"] {
        assert!(!addresses(windows).contains(&excluded.to_string()));
    }
}

#[test]
fn workspace_context_tracks_visibility_across_monitors() {
    let snapshot = snapshot();

    let second = workspace_context(&snapshot, 2);
    assert_eq!(second["monitor"]["name"], "HDMI-A-1");
    assert_eq!(second["visible"], true);
    assert_eq!(addresses(&second["windows"]), vec!["0x2001"]);

    let third = workspace_context(&snapshot, 3);
    assert_eq!(third["visible"], false);
}

#[test]
fn workspace_context_includes_scratchpad_windows() {
    let snapshot = snapshot();
    let context = workspace_context(&snapshot, -99);
    assert_eq!(context["workspace"]["name"], "special:scratch");
    assert_eq!(context["visible"], true);
    assert_eq!(addresses(&context["windows"]), vec!["0x5001"]);
}

#[test]
fn workspace_context_does_not_borrow_process_data_between_windows() {
    let snapshot = snapshot();
    let context = workspace_context(&snapshot, 1);
    for window in context["windows"].as_array().expect("windows") {
        assert!(
            window.get("process").is_none(),
            "workspace listings only carry window metadata, not other processes"
        );
    }
}
