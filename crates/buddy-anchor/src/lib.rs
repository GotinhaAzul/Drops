//! Hyprland anchoring for Drops.
//!
//! This crate answers four questions for the daemon:
//!
//! 1. What is under the cursor when a blob is dropped? ([`hit_test`])
//! 2. What placement and indicator rectangle should preview a drop? ([`preview`])
//! 3. Where is an anchored blob now, and is it visible? ([`refresh`])
//! 4. What text context belongs to that anchor? ([`context`])
//!
//! It also forwards Hyprland socket2 events through [`subscribe`] so the
//! daemon can re-query state after moves, closes, and workspace switches.
//! Subscriptions recover from compositor restarts instead of ending.
//!
//! ## Headless behaviour
//!
//! Anchors are stored as window addresses and workspace IDs, never screen
//! coordinates. Every operation that needs the compositor first checks for a
//! live Hyprland session and returns [`AnchorError::DesktopUnavailable`] when
//! there is none. When the inherited instance signature went stale because
//! Hyprland restarted, a sole live instance reported by `hyprctl instances` is
//! adopted per command; multiple live sessions are never guessed between. This
//! crate never invents window or workspace state, which keeps headless test
//! runs honest. The pure hit-testing layer in [`hit`] is fixture-driven and
//! needs no compositor.
//!
//! ## Read-only guarantee
//!
//! Only `hyprctl -j` queries and a socket2 connection are used. No dispatch
//! commands run, no configuration is written, and no desktop setting changes.

mod app_context;
pub mod brief;
pub mod bundle;
pub mod capture;
mod error;
pub mod hit;
mod hypr;
pub mod model;
pub mod procfs;
pub mod workspace;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub use error::AnchorError;
pub use model::{Anchor, Client, CursorPos, Monitor, Placement, Snapshot, Workspace, WorkspaceRef};

/// Check that a live Hyprland session is observable.
pub fn desktop_available() -> Result<(), AnchorError> {
    hypr::ensure_desktop().map(|_| ())
}

/// Read the current cursor position in global logical pixels.
pub async fn cursor_position() -> Result<(i32, i32)> {
    hypr::cursor_position().await
}

/// Query clients, workspaces, and monitors as one snapshot.
pub async fn snapshot() -> Result<Snapshot> {
    hypr::snapshot().await
}

/// Decide what a blob dropped at `(x, y)` should attach to.
///
/// Floating windows win over tiled windows, fullscreen windows win over
/// everything, and Drops' own surfaces are ignored. Empty space on a visible
/// workspace yields a workspace anchor. A point outside every enabled monitor
/// is an error, not a fake workspace.
///
/// The returned `Placement` uses the anchor's reference point (window top-left
/// or monitor origin), so it is identical to what [`refresh`] reports.
pub async fn hit_test(x: i32, y: i32) -> Result<Placement> {
    let snapshot = hypr::snapshot().await?;
    Ok(hit::hit_test(&snapshot, x, y).ok_or(AnchorError::NotOnDesktop { x, y })?)
}

/// Fixed drop-indicator size used for workspace previews.
pub const WORKSPACE_PREVIEW_SIZE: (i32, i32) = (160, 56);

/// Preview a drop at `(x, y)`: the placement a blob would receive and the
/// rectangle the drag feedback should draw.
///
/// This resolves everything from one compositor snapshot and the same pure
/// [`hit::hit_test`] as [`hit_test`], and never reads process context. Window
/// previews use the target window's client rectangle. Workspace previews keep
/// the workspace placement's monitor-origin reference point (unchanged
/// [`Placement`] semantics) but draw a fixed 160x56 indicator at the cursor so
/// feedback appears where the pointer is instead of jumping to the origin.
pub async fn preview(x: i32, y: i32) -> Result<Value> {
    let snapshot = hypr::snapshot().await?;
    preview_from_snapshot(&snapshot, x, y)
}

/// Pure preview resolution against an existing snapshot.
pub fn preview_from_snapshot(snapshot: &Snapshot, x: i32, y: i32) -> Result<Value> {
    let placement = hit::hit_test(snapshot, x, y).ok_or(AnchorError::NotOnDesktop { x, y })?;

    let geometry = match &placement.anchor {
        Anchor::Window { address } => {
            let client = find_client(snapshot, address)
                .ok_or_else(|| AnchorError::WindowNotFound(address.clone()))?;
            let rect = hit::client_rect(client);
            serde_json::json!({
                "x": rect.x,
                "y": rect.y,
                "width": rect.width,
                "height": rect.height,
            })
        }
        Anchor::Workspace { .. } => serde_json::json!({
            "x": x,
            "y": y,
            "width": WORKSPACE_PREVIEW_SIZE.0,
            "height": WORKSPACE_PREVIEW_SIZE.1,
        }),
    };

    Ok(serde_json::json!({
        "placement": placement,
        "geometry": geometry,
    }))
}

/// Re-resolve an anchor's position, visibility, and label.
///
/// A window that has closed falls back to the focused workspace so the blob
/// can stay on the desktop instead of vanishing. A workspace anchor that no
/// longer exists is an error, because there is nothing meaningful to fall back
/// to.
pub async fn refresh(anchor: &Anchor) -> Result<Placement> {
    let snapshot = hypr::snapshot().await?;
    refresh_from_snapshot(&snapshot, anchor)
}

/// Resolve multiple anchors against a single compositor snapshot.
pub fn refresh_from_snapshot(snapshot: &Snapshot, anchor: &Anchor) -> Result<Placement> {
    match anchor {
        Anchor::Window { address } => {
            if let Some(client) = find_client(snapshot, address) {
                Ok(hit::placement_for_window(
                    snapshot,
                    client,
                    client.at[0],
                    client.at[1],
                ))
            } else {
                let (workspace_id, monitor) =
                    hit::focused_workspace(snapshot).ok_or(AnchorError::NoActiveWorkspace)?;
                Ok(Placement {
                    anchor: Anchor::Workspace { id: workspace_id },
                    x: monitor.x,
                    y: monitor.y,
                    visible: true,
                    label: hit::workspace_label(snapshot, workspace_id),
                })
            }
        }
        Anchor::Workspace { id } => {
            let monitor = hit::monitor_for_workspace(snapshot, *id)
                .ok_or(AnchorError::WorkspaceNotFound(*id))?;
            hit::placement_for_workspace(snapshot, *id, monitor.x, monitor.y)
                .ok_or_else(|| AnchorError::WorkspaceNotFound(*id).into())
        }
    }
}

/// Build the context bundle for an anchor.
///
/// Window anchors include the process's working directory and command line,
/// read from `/proc/<pid>`. Terminal workspace detection also inspects the bounded
/// descendant process tree and a uniquely matched tmux client. Workspace anchors
/// include a compact list of windows on the workspace.
pub async fn context(anchor: &Anchor) -> Result<Value> {
    let snapshot = hypr::snapshot().await?;
    match anchor {
        Anchor::Window { address } => {
            let client = find_client(&snapshot, address)
                .ok_or_else(|| AnchorError::WindowNotFound(address.clone()))?;
            let process = procfs::process_info(client.pid).await;
            let shared_pid = snapshot
                .clients
                .iter()
                .filter(|other| other.pid == client.pid && other.mapped && !other.hidden)
                .count()
                > 1;
            let mut context = bundle::window_context(&snapshot, client, Some(&process));
            let working = if let Some(document) =
                workspace::detect_document(&client.class, &client.title, &process).await
            {
                document
            } else {
                workspace::detect_window(&context, &process, shared_pid).await
            };
            context["working_context"] = working;
            Ok(context)
        }
        Anchor::Workspace { id } => {
            let known = snapshot
                .workspaces
                .iter()
                .any(|workspace| workspace.id == *id)
                || snapshot
                    .monitors
                    .iter()
                    .any(|monitor| monitor.active_workspace.id == *id);
            if !known {
                return Err(AnchorError::WorkspaceNotFound(*id).into());
            }
            Ok(bundle::workspace_context(&snapshot, *id))
        }
    }
}

/// Subscribe to Hyprland socket2 and signal on anchor-relevant changes.
///
/// The daemon should call [`snapshot`] after each signal. Every successful
/// connection signals a refresh (including the first), so anchors converge
/// when the daemon starts before the compositor. Socket loss and compositor
/// restarts are retried with a cancel-aware bounded backoff. Cancellation ends
/// the subscription cleanly.
pub async fn subscribe(events: UnboundedSender<()>, cancel: CancellationToken) -> Result<()> {
    hypr::subscribe(events, cancel).await
}

fn find_client<'a>(snapshot: &'a Snapshot, address: &str) -> Option<&'a Client> {
    let needle = normalize_address(address);
    snapshot
        .clients
        .iter()
        .find(|client| normalize_address(&client.address) == needle)
}

fn normalize_address(address: &str) -> String {
    let address = address.trim().to_ascii_lowercase();
    address
        .strip_prefix("0x")
        .map(str::to_string)
        .unwrap_or(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_match_with_and_without_prefix_and_case() {
        assert_eq!(normalize_address("0xABC123"), "abc123");
        assert_eq!(normalize_address("0xabc123"), normalize_address("ABC123"));
        assert_ne!(normalize_address("0xabc"), normalize_address("0xdef"));
    }

    #[test]
    fn finds_client_by_normalized_address() {
        let snapshot = Snapshot {
            clients: vec![Client {
                address: "0xDEAD".into(),
                ..Client::default()
            }],
            ..Snapshot::default()
        };
        assert!(find_client(&snapshot, "dead").is_some());
        assert!(find_client(&snapshot, "0xdead").is_some());
        assert!(find_client(&snapshot, "0xbeef").is_none());
    }

    #[test]
    fn anchor_serde_uses_snake_case_kind() {
        let window: Anchor =
            serde_json::from_str(r#"{"kind":"window","address":"0x1"}"#).expect("window anchor");
        assert_eq!(
            window,
            Anchor::Window {
                address: "0x1".into()
            }
        );

        let workspace: Anchor =
            serde_json::from_str(r#"{"kind":"workspace","id":-99}"#).expect("workspace anchor");
        assert_eq!(workspace, Anchor::Workspace { id: -99 });

        let value = serde_json::to_value(Anchor::Workspace { id: 2 }).expect("serialize");
        assert_eq!(value, serde_json::json!({"kind": "workspace", "id": 2}));
    }

    #[test]
    fn placement_serde_round_trips() {
        let placement = Placement {
            anchor: Anchor::Window {
                address: "0x1".into(),
            },
            x: 10,
            y: 20,
            visible: true,
            label: "term".into(),
        };
        let json = serde_json::to_string(&placement).expect("serialize");
        let parsed: Placement = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, placement);
    }
}

/// Low-overhead control queries used by the daemon geometry watcher.
pub mod control;
