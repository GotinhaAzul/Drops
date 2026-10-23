//! Pure hit-testing tests driven by recorded `hyprctl -j` fixtures.
//!
//! These tests never touch Hyprland. They prove drop behaviour for the
//! two-monitor, floating-over-tiled, scratchpad, and fullscreen cases.

use buddy_anchor::hit::{
    client_monitor, client_visible, focused_workspace, hit_test, monitor_at,
    placement_for_workspace, visible_workspaces,
};
use buddy_anchor::model::{Anchor, Client, Monitor, Placement, Snapshot, Workspace};

fn two_monitors() -> Snapshot {
    serde_json::from_str(include_str!("fixtures/hyprland-two-monitors.json"))
        .expect("two-monitor fixture should match the hyprctl shape")
}

fn fullscreen() -> Snapshot {
    serde_json::from_str(include_str!("fixtures/hyprland-fullscreen.json"))
        .expect("fullscreen fixture should match the hyprctl shape")
}

fn window_address(placement: &Placement) -> &str {
    match &placement.anchor {
        Anchor::Window { address } => address,
        other => panic!("expected a window anchor, got {other:?}"),
    }
}

fn workspace_id(placement: &Placement) -> i64 {
    match &placement.anchor {
        Anchor::Workspace { id } => *id,
        other => panic!("expected a workspace anchor, got {other:?}"),
    }
}

#[test]
fn fixture_parses_all_query_shapes_and_ignores_unknown_fields() {
    let snapshot = two_monitors();
    assert_eq!(snapshot.monitors.len(), 2);
    assert_eq!(snapshot.workspaces.len(), 4);
    assert_eq!(snapshot.clients.len(), 7);

    let laptop = &snapshot.monitors[0];
    assert_eq!(laptop.name, "eDP-1");
    assert_eq!(laptop.active_workspace.id, 1);
    assert_eq!(laptop.special_workspace.id, -99);
    assert!(laptop.focused);

    let term = snapshot
        .clients
        .iter()
        .find(|client| client.address == "0x1001")
        .expect("terminal");
    assert_eq!(term.class, "Alacritty");
    assert_eq!(term.pid, 111);
    assert_eq!(term.workspace.id, 1);
    assert!(!term.floating);
    assert_eq!(term.focus_history_id, 1);
}

#[test]
fn floating_window_wins_over_tiled_window_under_it() {
    let placement = hit_test(&two_monitors(), 900, 300).expect("hit");
    assert_eq!(window_address(&placement), "0x1002");
    assert_eq!(placement.label, "Docs");
    assert!(placement.visible);
    assert_eq!(
        (placement.x, placement.y),
        (800, 200),
        "placement uses the window origin, matching refresh"
    );
}

#[test]
fn tiled_window_is_hit_where_no_float_covers() {
    let placement = hit_test(&two_monitors(), 50, 50).expect("hit");
    assert_eq!(window_address(&placement), "0x1001");
}

#[test]
fn hidden_window_never_wins_even_though_it_covers_the_screen() {
    let snapshot = two_monitors();
    let hidden = snapshot
        .clients
        .iter()
        .find(|client| client.address == "0x1003")
        .expect("hidden client");
    assert!(hidden.hidden);
    assert!(!client_visible(&snapshot, hidden));

    let placement = hit_test(&snapshot, 50, 50).expect("hit");
    assert_eq!(window_address(&placement), "0x1001");
}

#[test]
fn window_on_an_invisible_workspace_is_not_hittable() {
    let snapshot = two_monitors();
    let offscreen = snapshot
        .clients
        .iter()
        .find(|client| client.address == "0x3001")
        .expect("workspace 3 client");
    assert_eq!(offscreen.workspace.id, 3);
    assert!(!client_visible(&snapshot, offscreen));

    let placement = hit_test(&snapshot, 50, 50).expect("hit");
    assert_eq!(window_address(&placement), "0x1001");
}

#[test]
fn drops_and_quickshell_surfaces_are_transparent_to_drops() {
    let snapshot = two_monitors();
    let panel = snapshot
        .clients
        .iter()
        .find(|client| client.address == "0x4001")
        .expect("quickshell surface");
    assert!(panel.is_buddy_surface());

    // Only the panel and empty space are here; the workspace is the anchor.
    let placement = hit_test(&snapshot, 1750, 950).expect("hit");
    assert_eq!(workspace_id(&placement), 1);
    assert_eq!(placement.label, "1");
    assert!(placement.visible);
}

#[test]
fn visible_scratchpad_is_hittable() {
    let placement = hit_test(&two_monitors(), 1450, 150).expect("hit");
    assert_eq!(window_address(&placement), "0x5001");
    assert_eq!(placement.label, "htop");
}

#[test]
fn second_monitor_uses_its_own_visible_workspace() {
    let placement = hit_test(&two_monitors(), 2000, 100).expect("hit");
    assert_eq!(window_address(&placement), "0x2001");
    assert_eq!(placement.label, "Firefox");
}

#[test]
fn empty_desktop_space_anchors_the_workspace_at_the_monitor_origin() {
    let placement = hit_test(&two_monitors(), 1700, 700).expect("hit");
    assert_eq!(workspace_id(&placement), 1);
    assert_eq!(
        (placement.x, placement.y),
        (0, 0),
        "workspace placements use the monitor origin"
    );
    assert!(placement.visible);
}

#[test]
fn drop_outside_every_monitor_is_not_invented() {
    let snapshot = two_monitors();
    assert!(hit_test(&snapshot, -10, 500).is_none());
    assert!(hit_test(&snapshot, 3840, 0).is_none());
    assert!(hit_test(&snapshot, 0, 1080).is_none());
}

#[test]
fn monitor_bounds_are_half_open_across_the_layout() {
    let snapshot = two_monitors();
    assert_eq!(monitor_at(&snapshot.monitors, 0, 0).map(|m| m.id), Some(0));
    assert_eq!(
        monitor_at(&snapshot.monitors, 1919, 1079).map(|m| m.id),
        Some(0)
    );
    assert_eq!(
        monitor_at(&snapshot.monitors, 1920, 1079).map(|m| m.id),
        Some(1)
    );
    assert_eq!(monitor_at(&snapshot.monitors, 1920, 1080), None);
    assert_eq!(
        monitor_at(&snapshot.monitors, 3839, 0).map(|m| m.id),
        Some(1)
    );
}

#[test]
fn visible_workspaces_cover_all_monitors_and_special_workspaces() {
    let visible = visible_workspaces(&two_monitors().monitors);
    assert_eq!(visible.into_iter().collect::<Vec<_>>(), vec![-99, 1, 2]);
}

#[test]
fn disabled_monitor_hides_its_workspace_and_rejects_drops() {
    let mut snapshot = two_monitors();
    snapshot.monitors[1].disabled = true;

    assert_eq!(
        visible_workspaces(&snapshot.monitors)
            .into_iter()
            .collect::<Vec<_>>(),
        vec![-99, 1]
    );
    assert!(hit_test(&snapshot, 2000, 100).is_none());
    assert!(!client_visible(&snapshot, &snapshot.clients[4]));

    let placement = placement_for_workspace(&two_monitors(), 2, 1920, 0).expect("workspace");
    assert!(placement.visible);

    let hidden = placement_for_workspace(&snapshot, 2, 1920, 0).expect("workspace");
    assert!(!hidden.visible, "disabled monitor workspace must be hidden");
}

#[test]
fn pinned_window_on_another_workspace_stays_visible_on_its_monitor() {
    let mut snapshot = two_monitors();
    {
        let pinned = snapshot
            .clients
            .iter_mut()
            .find(|client| client.address == "0x3001")
            .expect("workspace 3 client");
        pinned.pinned = true;
    }
    let pinned = snapshot
        .clients
        .iter()
        .find(|client| client.address == "0x3001")
        .expect("workspace 3 client");

    assert!(client_visible(&snapshot, pinned));
    let placement = hit_test(&snapshot, 50, 50).expect("hit");
    assert_eq!(window_address(&placement), "0x3001");
}

#[test]
fn client_monitor_falls_back_to_workspace_metadata() {
    let mut snapshot = two_monitors();
    {
        let client = snapshot
            .clients
            .iter_mut()
            .find(|client| client.address == "0x2001")
            .expect("second-monitor client");
        client.monitor = -1;
    }
    let client = snapshot
        .clients
        .iter()
        .find(|client| client.address == "0x2001")
        .expect("second-monitor client");

    let monitor = client_monitor(&snapshot, client).expect("monitor");
    assert_eq!(monitor.id, 1);
    assert_eq!(monitor.name, "HDMI-A-1");
}

#[test]
fn workspace_placement_uses_monitor_origin_and_visibility() {
    let snapshot = two_monitors();

    let workspace_two = placement_for_workspace(&snapshot, 2, 1920, 0).expect("workspace 2");
    assert_eq!(workspace_two.anchor, Anchor::Workspace { id: 2 });
    assert_eq!((workspace_two.x, workspace_two.y), (1920, 0));
    assert!(workspace_two.visible);
    assert_eq!(workspace_two.label, "2");

    let workspace_three = placement_for_workspace(&snapshot, 3, 0, 0).expect("workspace 3");
    assert!(!workspace_three.visible);

    assert!(placement_for_workspace(&snapshot, 42, 0, 0).is_none());
}

#[test]
fn single_monitor_workspace_can_fall_back_to_that_monitor() {
    let mut snapshot = two_monitors();
    snapshot.monitors.truncate(1);
    snapshot.workspaces.retain(|workspace| workspace.id == 1);

    let placement = placement_for_workspace(&snapshot, 1, 0, 0).expect("workspace 1");
    assert_eq!(placement.label, "1");
}

#[test]
fn closed_window_fallback_is_the_focused_monitor_workspace() {
    let snapshot = two_monitors();
    let (id, monitor) = focused_workspace(&snapshot).expect("focused workspace");
    assert_eq!(id, 1);
    assert_eq!(monitor.id, 0);
    assert_eq!(monitor.x, 0);
}

#[test]
fn focused_workspace_falls_back_to_a_focused_workspace_entry() {
    let mut snapshot = two_monitors();
    for monitor in &mut snapshot.monitors {
        monitor.focused = false;
    }
    for workspace in &mut snapshot.workspaces {
        workspace.focused = false;
    }
    snapshot.workspaces[1].focused = true; // workspace 2 on HDMI-A-1

    let (id, monitor) = focused_workspace(&snapshot).expect("focused workspace");
    assert_eq!(id, 2);
    assert_eq!(monitor.id, 1);
}

#[test]
fn focused_workspace_falls_back_to_first_enabled_monitor() {
    let mut snapshot = two_monitors();
    for monitor in &mut snapshot.monitors {
        monitor.focused = false;
    }
    for workspace in &mut snapshot.workspaces {
        workspace.focused = false;
    }
    snapshot.monitors[0].disabled = true;

    let (id, monitor) = focused_workspace(&snapshot).expect("focused workspace");
    assert_eq!(id, 2);
    assert_eq!(monitor.id, 1);
}

#[test]
fn fullscreen_window_wins_over_a_recently_focused_float() {
    let snapshot = fullscreen();
    let placement = hit_test(&snapshot, 200, 200).expect("hit");
    assert_eq!(window_address(&placement), "0x9001");
}

#[test]
fn maximized_window_does_not_hide_floating_windows() {
    let mut snapshot = fullscreen();
    let maximized = snapshot
        .clients
        .iter_mut()
        .find(|client| client.address == "0x9001")
        .expect("chromium");
    maximized.fullscreen = 1;

    let placement = hit_test(&snapshot, 200, 200).expect("hit");
    assert_eq!(
        window_address(&placement),
        "0x9002",
        "fullscreen=1 is maximized, not fullscreen"
    );
}

#[test]
fn floats_are_stacked_by_focus_history() {
    let monitor = Monitor {
        id: 0,
        name: "eDP-1".into(),
        width: 1920,
        height: 1080,
        focused: true,
        active_workspace: buddy_anchor::WorkspaceRef {
            id: 1,
            name: "1".into(),
        },
        ..Monitor::default()
    };
    let workspace = Workspace {
        id: 1,
        name: "1".into(),
        monitor: "eDP-1".into(),
        monitor_id: 0,
        focused: true,
        ..Workspace::default()
    };
    let float = |address: &str, focus: i32| Client {
        address: address.into(),
        at: [100, 100],
        size: [400, 300],
        workspace: buddy_anchor::WorkspaceRef {
            id: 1,
            name: "1".into(),
        },
        floating: true,
        fullscreen: 0,
        monitor: 0,
        class: "float".into(),
        title: address.into(),
        focus_history_id: focus,
        ..Client::default()
    };
    let snapshot = Snapshot {
        monitors: vec![monitor],
        workspaces: vec![workspace],
        clients: vec![float("0x1", 4), float("0x2", 0)],
    };

    let placement = hit_test(&snapshot, 150, 150).expect("hit");
    assert_eq!(window_address(&placement), "0x2");
}

#[test]
fn empty_active_workspace_id_does_not_produce_a_fake_anchor() {
    let mut snapshot = two_monitors();
    snapshot.monitors[0].active_workspace.id = 0;
    snapshot.monitors[0].special_workspace.id = 0;
    snapshot.clients.clear();
    snapshot.workspaces.clear();

    assert!(hit_test(&snapshot, 100, 100).is_none());
}
