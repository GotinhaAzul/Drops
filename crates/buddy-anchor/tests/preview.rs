//! Pure preview tests driven by recorded `hyprctl -j` fixtures.
//!
//! Preview must agree with hit-testing for the placement, use the target
//! window's client rect, and draw workspace feedback at the cursor.

use buddy_anchor::model::Snapshot;
use buddy_anchor::preview_from_snapshot;
use serde_json::json;

fn two_monitors() -> Snapshot {
    serde_json::from_str(include_str!("fixtures/hyprland-two-monitors.json"))
        .expect("two-monitor fixture should match the hyprctl shape")
}

fn fullscreen() -> Snapshot {
    serde_json::from_str(include_str!("fixtures/hyprland-fullscreen.json"))
        .expect("fullscreen fixture should match the hyprctl shape")
}

#[test]
fn window_preview_uses_the_target_client_rect() {
    let preview = preview_from_snapshot(&two_monitors(), 900, 300).expect("preview");

    assert_eq!(
        preview["placement"]["anchor"],
        json!({"kind": "window", "address": "0x1002"})
    );
    assert_eq!(preview["placement"]["x"], 800);
    assert_eq!(preview["placement"]["y"], 200);
    assert_eq!(preview["placement"]["label"], "Docs");
    assert_eq!(
        preview["geometry"],
        json!({"x": 800, "y": 200, "width": 500, "height": 400})
    );
}

#[test]
fn workspace_preview_keeps_monitor_origin_placement_but_follows_the_cursor() {
    let preview = preview_from_snapshot(&two_monitors(), 1700, 700).expect("preview");

    assert_eq!(
        preview["placement"]["anchor"],
        json!({"kind": "workspace", "id": 1})
    );
    assert_eq!(
        (
            preview["placement"]["x"].as_i64(),
            preview["placement"]["y"].as_i64()
        ),
        (Some(0), Some(0)),
        "workspace placements keep the monitor origin as their reference point"
    );
    assert_eq!(
        preview["geometry"],
        json!({"x": 1700, "y": 700, "width": 160, "height": 56})
    );
}

#[test]
fn fullscreen_preview_uses_the_fullscreen_window() {
    let preview = preview_from_snapshot(&fullscreen(), 200, 200).expect("preview");
    assert_eq!(preview["placement"]["anchor"]["address"], "0x9001");
}

#[test]
fn preview_outside_every_monitor_is_an_error() {
    let error = preview_from_snapshot(&two_monitors(), 5000, 5000).expect_err("off desktop");
    assert!(
        error.downcast_ref::<buddy_anchor::AnchorError>().is_some(),
        "expected an AnchorError, got {error}"
    );
}
