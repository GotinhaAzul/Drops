//! Builds the JSON context bundle sent to the agent for an anchor.

use serde_json::{json, Value};

use crate::hit::{client_monitor, client_visible, is_buddy_surface, window_label};
use crate::model::{Client, Monitor, Snapshot};
use crate::procfs::ProcessInfo;

/// Context for a window anchor.
///
/// Always includes class, title, PID, workspace, monitor, floating state, and
/// geometry. Process working directory and command line are attached for the
/// anchored window's own PID only.
pub fn window_context(
    snapshot: &Snapshot,
    client: &Client,
    process: Option<&ProcessInfo>,
) -> Value {
    let monitor = client_monitor(snapshot, client);
    let process = process
        .map(|process| serde_json::to_value(process).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);

    json!({
        "kind": "window",
        "app_kind": crate::workspace::app_kind(&client.class),
        "screen_locked": snapshot.monitors.iter().any(|m|m.solitary_blocked_by.iter().any(|reason|reason=="LOCK")),
        "window": {
            "address": client.address,
            "class": client.class,
            "initial_class": client.initial_class,
            "title": client.title,
            "label": window_label(client),
            "pid": client.pid,
            "workspace": workspace_ref(client),
            "monitor_id": monitor.map(|monitor| monitor.id),
            "floating": client.floating,
            "pinned": client.pinned,
            "fullscreen": client.fullscreen,
            "geometry": {
                "x": client.at[0],
                "y": client.at[1],
                "width": client.size[0],
                "height": client.size[1],
            },
        },
        "workspace": workspace_ref(client),
        "monitor": monitor.map(monitor_json),
        "visible": client_visible(snapshot, client),
        "process": process,
    })
}

/// Context for a workspace anchor, including the windows currently on it.
pub fn workspace_context(snapshot: &Snapshot, id: i64) -> Value {
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id);

    let monitor = workspace
        .and_then(|workspace| {
            snapshot
                .monitors
                .iter()
                .find(|monitor| !workspace.monitor.is_empty() && monitor.name == workspace.monitor)
        })
        .or_else(|| {
            snapshot
                .monitors
                .iter()
                .find(|monitor| monitor.active_workspace.id == id)
        })
        .or_else(|| {
            workspace.and_then(|workspace| {
                snapshot
                    .monitors
                    .iter()
                    .find(|monitor| monitor.id == workspace.monitor_id)
            })
        });

    let mut windows: Vec<&Client> = snapshot
        .clients
        .iter()
        .filter(|client| client.workspace_id() == id)
        .filter(|client| client.mapped && !client.hidden)
        .filter(|client| !is_buddy_surface(client))
        .collect();
    windows.sort_by_key(|client| {
        let focus = if client.focus_history_id < 0 {
            u32::MAX
        } else {
            client.focus_history_id as u32
        };
        (focus, client.address.clone())
    });

    let windows: Vec<Value> = windows
        .into_iter()
        .map(|client| {
            json!({
                "address": client.address,
                "class": client.class,
                "title": client.title,
                "label": window_label(client),
                "pid": client.pid,
                "floating": client.floating,
                "pinned": client.pinned,
                "fullscreen": client.fullscreen,
                "geometry": {
                    "x": client.at[0],
                    "y": client.at[1],
                    "width": client.size[0],
                    "height": client.size[1],
                },
            })
        })
        .collect();

    json!({
        "kind": "workspace",
        "workspace": {
            "id": id,
            "name": workspace
                .map(|workspace| workspace.name.clone())
                .unwrap_or_else(|| id.to_string()),
        },
        "monitor": monitor.map(monitor_json),
        "visible": crate::hit::visible_workspaces(&snapshot.monitors).contains(&id),
        "windows": windows,
    })
}

fn workspace_ref(client: &Client) -> Value {
    json!({
        "id": client.workspace.id,
        "name": client.workspace.name,
    })
}

fn monitor_json(monitor: &Monitor) -> Value {
    json!({
        "id": monitor.id,
        "name": monitor.name,
        "x": monitor.x,
        "y": monitor.y,
        "width": monitor.width,
        "height": monitor.height,
        "scale": monitor.scale,
        "focused": monitor.focused,
        "active_workspace": {
            "id": monitor.active_workspace.id,
            "name": monitor.active_workspace.name,
        },
        "special_workspace": {
            "id": monitor.special_workspace.id,
            "name": monitor.special_workspace.name,
        },
    })
}
