//! Pure hit-testing over a [`Snapshot`].
//!
//! Everything in this module is synchronous and has no I/O, so drop behaviour
//! can be tested from recorded `hyprctl` fixtures without a compositor.

use std::collections::BTreeSet;

use crate::model::{Anchor, Client, Monitor, Placement, Snapshot};

/// Window classes that belong to Drops itself and must never become anchors.
///
/// Quickshell surfaces are excluded by class because layer-shell surfaces do
/// not appear in `hyprctl clients`; the panel's own windows do.
pub const IGNORED_SURFACE_PREFIXES: [&str; 3] = ["drops", "buddy", "quickshell"];

/// A logical-pixel rectangle from `hyprctl` (`x`, `y`, `width`, `height`).
///
/// Rects are half-open: the right and bottom edges are outside the rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        if self.width <= 0 || self.height <= 0 {
            return false;
        }
        let right = self.x as i64 + self.width as i64;
        let bottom = self.y as i64 + self.height as i64;
        let (x, y) = (x as i64, y as i64);
        x >= self.x as i64 && y >= self.y as i64 && x < right && y < bottom
    }
}

/// Rectangle of a window in logical layout coordinates.
pub fn client_rect(client: &Client) -> Rect {
    Rect::new(client.at[0], client.at[1], client.size[0], client.size[1])
}

/// Rectangle of a monitor in logical layout coordinates.
pub fn monitor_rect(monitor: &Monitor) -> Rect {
    Rect::new(monitor.x, monitor.y, monitor.width, monitor.height)
}

/// Workspaces that are actually on screen right now, across every enabled
/// monitor. This includes a visible scratchpad (`special:*`) workspace.
pub fn visible_workspaces(monitors: &[Monitor]) -> BTreeSet<i64> {
    let mut visible = BTreeSet::new();
    for monitor in monitors.iter().filter(|monitor| monitor.is_enabled()) {
        if monitor.active_workspace.id != 0 {
            visible.insert(monitor.active_workspace.id);
        }
        if monitor.special_workspace.id != 0 {
            visible.insert(monitor.special_workspace.id);
        }
    }
    visible
}

/// The enabled monitor under a global logical-pixel coordinate.
pub fn monitor_at(monitors: &[Monitor], x: i32, y: i32) -> Option<&Monitor> {
    monitors
        .iter()
        .find(|monitor| monitor.is_enabled() && monitor_rect(monitor).contains(x, y))
}

/// Resolve the monitor a client is drawn on.
///
/// `client.monitor` is authoritative when Hyprland reports it. Otherwise fall
/// back to the workspace's monitor name/ID, then to the monitor that currently
/// shows the client's workspace.
pub fn client_monitor<'a>(snapshot: &'a Snapshot, client: &Client) -> Option<&'a Monitor> {
    if client.monitor >= 0 {
        if let Some(monitor) = snapshot
            .monitors
            .iter()
            .find(|monitor| monitor.id == client.monitor)
        {
            return Some(monitor);
        }
    }

    if let Some(workspace) = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.id == client.workspace_id())
    {
        if let Some(monitor) = snapshot
            .monitors
            .iter()
            .find(|monitor| !workspace.monitor.is_empty() && monitor.name == workspace.monitor)
        {
            return Some(monitor);
        }
        if let Some(monitor) = snapshot
            .monitors
            .iter()
            .find(|monitor| monitor.id == workspace.monitor_id)
        {
            return Some(monitor);
        }
    }

    snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.active_workspace.id == client.workspace_id())
}

/// Whether a window is currently rendered on some monitor.
///
/// Hidden, unmapped, and off-screen-workspace clients are not visible. Pinned
/// floating windows stay visible on their monitor even when another workspace
/// is active.
pub fn client_visible(snapshot: &Snapshot, client: &Client) -> bool {
    if !client.mapped || client.hidden {
        return false;
    }
    let Some(monitor) = client_monitor(snapshot, client) else {
        return false;
    };
    if !monitor.is_enabled() {
        return false;
    }
    if client.pinned {
        return monitor.active_workspace.id != 0;
    }
    visible_workspaces(&snapshot.monitors).contains(&client.workspace_id())
}

/// True for Drops' own panel/blob windows, which must be transparent to
/// drop hit-testing.
pub fn is_buddy_surface(client: &Client) -> bool {
    [client.class.as_str(), client.initial_class.as_str()]
        .iter()
        .any(|name| {
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty() {
                return false;
            }
            IGNORED_SURFACE_PREFIXES
                .iter()
                .any(|prefix| name == *prefix || surface_prefix_matches(&name, prefix))
        })
}

fn surface_prefix_matches(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .map(|rest| {
            rest.starts_with('-')
                || rest.starts_with('.')
                || rest.starts_with('_')
                || rest.starts_with(' ')
                || rest.starts_with(':')
        })
        .unwrap_or(false)
}

/// A human-readable label for a window: its title, falling back to class and
/// then to the address.
pub fn window_label(client: &Client) -> String {
    for candidate in [
        client.title.as_str(),
        client.class.as_str(),
        client.address.as_str(),
    ] {
        let candidate = candidate.trim();
        if !candidate.is_empty() {
            return candidate.to_string();
        }
    }
    "window".to_string()
}

/// Workspace name, falling back to the numeric ID.
pub fn workspace_label(snapshot: &Snapshot, id: i64) -> String {
    snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .map(|workspace| workspace.name.trim())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

/// The workspace a blob falls back to when its window closes: the focused
/// monitor's active workspace, then any focused workspace, then the first
/// usable monitor.
pub fn focused_workspace(snapshot: &Snapshot) -> Option<(i64, &Monitor)> {
    snapshot
        .monitors
        .iter()
        .find(|monitor| monitor.is_enabled() && monitor.focused && monitor.active_workspace.id != 0)
        .or_else(|| {
            let focused_id = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.focused)
                .map(|workspace| workspace.id)?;
            snapshot
                .monitors
                .iter()
                .find(|monitor| monitor.is_enabled() && monitor.active_workspace.id == focused_id)
        })
        .or_else(|| {
            snapshot
                .monitors
                .iter()
                .find(|monitor| monitor.is_enabled() && monitor.active_workspace.id != 0)
        })
        .map(|monitor| (monitor.active_workspace.id, monitor))
}

/// Build the placement for a known window.
///
/// `x`/`y` are the anchor's reference point on screen. Passing the window's
/// own origin keeps a drop and every later [`refresh`](crate::refresh)
/// identical, so a blob does not jump after its first event.
pub fn placement_for_window(snapshot: &Snapshot, client: &Client, x: i32, y: i32) -> Placement {
    Placement {
        anchor: Anchor::Window {
            address: client.address.clone(),
        },
        x,
        y,
        visible: client_visible(snapshot, client),
        label: window_label(client),
    }
}

/// The monitor that displays a workspace.
pub fn monitor_for_workspace(snapshot: &Snapshot, id: i64) -> Option<&Monitor> {
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id);

    workspace
        .and_then(|workspace| {
            snapshot
                .monitors
                .iter()
                .find(|monitor| !workspace.monitor.is_empty() && monitor.name == workspace.monitor)
        })
        .or_else(|| {
            workspace.and_then(|workspace| {
                snapshot
                    .monitors
                    .iter()
                    .find(|monitor| monitor.id == workspace.monitor_id)
            })
        })
        .or_else(|| {
            snapshot
                .monitors
                .iter()
                .find(|monitor| monitor.active_workspace.id == id)
        })
        .or_else(|| {
            if snapshot.monitors.len() == 1 {
                snapshot.monitors.first()
            } else {
                None
            }
        })
}

/// Build the placement for a workspace.
///
/// `x`/`y` are the anchor's reference point on screen; callers pass the
/// monitor origin for both fresh drops and refreshes.
pub fn placement_for_workspace(snapshot: &Snapshot, id: i64, x: i32, y: i32) -> Option<Placement> {
    monitor_for_workspace(snapshot, id)?;

    Some(Placement {
        anchor: Anchor::Workspace { id },
        x,
        y,
        visible: visible_workspaces(&snapshot.monitors).contains(&id),
        label: workspace_label(snapshot, id),
    })
}

/// Decide what is under a drop point.
///
/// Order: a fullscreen window on that monitor, then the topmost floating
/// window, then the topmost tiled window, then the workspace itself. Drops'
/// own surfaces are ignored at every stage. Returns `None` when the point is
/// outside every enabled monitor.
pub fn hit_test(snapshot: &Snapshot, x: i32, y: i32) -> Option<Placement> {
    let monitor = monitor_at(&snapshot.monitors, x, y)?;

    let fullscreen = fullscreen_window(snapshot, monitor);

    let mut candidates: Vec<&Client> = snapshot
        .clients
        .iter()
        .filter(|client| !is_buddy_surface(client))
        .filter(|client| client_visible(snapshot, client))
        .filter(|client| {
            client_monitor(snapshot, client).map(|monitor| monitor.id) == Some(monitor.id)
        })
        .filter(|client| fullscreen.is_none_or(|fullscreen| std::ptr::eq(*client, fullscreen)))
        .filter(|client| client_rect(client).contains(x, y))
        .collect();

    candidates.sort_by(|left, right| stack_key(left).cmp(&stack_key(right)));

    if let Some(client) = candidates.first() {
        return Some(placement_for_window(
            snapshot,
            client,
            client.at[0],
            client.at[1],
        ));
    }

    // Empty desktop space: the workspace itself is the anchor. The reference
    // point is the monitor origin, matching `refresh` for workspace anchors.
    let id = monitor.active_workspace.id;
    if id == 0 {
        return None;
    }
    Some(Placement {
        anchor: Anchor::Workspace { id },
        x: monitor.x,
        y: monitor.y,
        visible: true,
        label: workspace_label(snapshot, id),
    })
}

/// The visible fullscreen window on a monitor, if any. Fullscreen hides every
/// other window on that monitor.
pub fn fullscreen_window<'a>(snapshot: &'a Snapshot, monitor: &Monitor) -> Option<&'a Client> {
    snapshot
        .clients
        .iter()
        .filter(|client| client.is_fullscreen())
        .filter(|client| client_visible(snapshot, client))
        .filter(|client| !is_buddy_surface(client))
        .filter(|client| {
            client_monitor(snapshot, client).map(|monitor| monitor.id) == Some(monitor.id)
        })
        .min_by_key(|client| focus_rank(client))
}

/// Stacking rank used for overlapping windows: floating/pinned first, then
/// most recently focused, then a stable address ordering.
fn stack_key(client: &Client) -> (u8, u32, &str) {
    let layer = if client.floating || client.pinned {
        0
    } else {
        1
    };
    (layer, focus_rank(client), client.address.as_str())
}

fn focus_rank(client: &Client) -> u32 {
    if client.focus_history_id < 0 {
        u32::MAX
    } else {
        client.focus_history_id as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_edges_are_half_open() {
        let rect = Rect::new(10, 20, 100, 50);
        assert!(rect.contains(10, 20));
        assert!(rect.contains(109, 69));
        assert!(!rect.contains(110, 40));
        assert!(!rect.contains(50, 70));
        assert!(!rect.contains(9, 20));
    }

    #[test]
    fn empty_rect_never_contains() {
        assert!(!Rect::new(0, 0, 0, 100).contains(0, 0));
        assert!(!Rect::new(0, 0, 100, -1).contains(0, 0));
    }

    #[test]
    fn buddy_surfaces_are_recognized_by_class_prefix() {
        let mut client = Client::default();
        for class in [
            "drops",
            "Drops-panel",
            "buddy",
            "buddy-anchor",
            "quickshell",
            "Quickshell:bar",
        ] {
            client.class = class.to_string();
            client.initial_class.clear();
            assert!(is_buddy_surface(&client), "{class} should be ignored");
        }

        for class in ["Alacritty", "buddybuddy", "dropship", "org.quickshell.app"] {
            client.class = class.to_string();
            client.initial_class.clear();
            assert!(!is_buddy_surface(&client), "{class} should not be ignored");
        }
    }

    #[test]
    fn buddy_surfaces_can_match_initial_class() {
        let client = Client {
            class: String::new(),
            initial_class: "quickshell".to_string(),
            ..Client::default()
        };
        assert!(is_buddy_surface(&client));
    }

    #[test]
    fn visible_workspaces_track_every_monitor_and_scratchpad() {
        let monitors = vec![
            Monitor {
                id: 0,
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                active_workspace: crate::model::WorkspaceRef {
                    id: 1,
                    name: "1".into(),
                },
                special_workspace: crate::model::WorkspaceRef {
                    id: -99,
                    name: "special:scratch".into(),
                },
                ..Monitor::default()
            },
            Monitor {
                id: 1,
                x: 1920,
                y: 0,
                width: 1920,
                height: 1080,
                active_workspace: crate::model::WorkspaceRef {
                    id: 2,
                    name: "2".into(),
                },
                ..Monitor::default()
            },
            Monitor {
                id: 2,
                x: 3840,
                y: 0,
                width: 1920,
                height: 1080,
                disabled: true,
                active_workspace: crate::model::WorkspaceRef {
                    id: 3,
                    name: "3".into(),
                },
                ..Monitor::default()
            },
        ];

        let visible = visible_workspaces(&monitors);
        assert!(visible.contains(&1));
        assert!(visible.contains(&2));
        assert!(visible.contains(&-99));
        assert!(!visible.contains(&3));
    }

    #[test]
    fn workspace_label_falls_back_to_id() {
        let snapshot = Snapshot {
            workspaces: vec![crate::model::Workspace {
                id: 4,
                name: "web".into(),
                ..crate::model::Workspace::default()
            }],
            ..Snapshot::default()
        };
        assert_eq!(workspace_label(&snapshot, 4), "web");
        assert_eq!(workspace_label(&snapshot, 9), "9");
    }

    #[test]
    fn focused_workspace_prefers_focused_monitor() {
        let snapshot = Snapshot {
            monitors: vec![
                Monitor {
                    id: 0,
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    active_workspace: crate::model::WorkspaceRef {
                        id: 1,
                        name: "1".into(),
                    },
                    ..Monitor::default()
                },
                Monitor {
                    id: 1,
                    x: 1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    focused: true,
                    active_workspace: crate::model::WorkspaceRef {
                        id: 7,
                        name: "7".into(),
                    },
                    ..Monitor::default()
                },
            ],
            ..Snapshot::default()
        };
        let (id, monitor) = focused_workspace(&snapshot).expect("focused workspace");
        assert_eq!(id, 7);
        assert_eq!(monitor.id, 1);
    }

    #[test]
    fn window_label_prefers_title_then_class_then_address() {
        let mut client = Client {
            title: "  ".into(),
            class: "kitty".into(),
            address: "0x1".into(),
            ..Client::default()
        };
        assert_eq!(window_label(&client), "kitty");
        client.class.clear();
        assert_eq!(window_label(&client), "0x1");
        client.address.clear();
        assert_eq!(window_label(&client), "window");
    }
}
