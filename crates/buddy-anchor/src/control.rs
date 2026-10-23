//! Cheap direct control-socket queries for geometry tracking.
use crate::Client;
use anyhow::{bail, Context, Result};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
};

pub async fn clients(cached: &mut Option<PathBuf>) -> Result<Vec<Client>> {
    if cached.is_none() {
        *cached = inherited_socket();
    }
    if let Some(path) = cached.as_ref() {
        if let Ok(result) = query(path).await {
            return Ok(result);
        }
    }
    // Discovery runs only after a socket failure, never on the idle hot path.
    *cached = None;
    let mut command = Command::new("hyprctl");
    command.args(["instances", "-j"]).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(1), command.output()).await??;
    let instances: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let signatures: Vec<&str> = instances
        .as_array()
        .context("invalid instances")?
        .iter()
        .filter_map(|v| v["instance"].as_str())
        .collect();
    let attached = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").unwrap_or_default();
    let signature = if signatures.contains(&attached.as_str()) {
        attached.as_str()
    } else if let [only] = signatures.as_slice() {
        only
    } else {
        bail!("no unambiguous compositor instance")
    };
    let path = socket_paths(signature)
        .into_iter()
        .find(|p| p.exists())
        .context("control socket missing")?;
    let result = query(&path).await?;
    *cached = Some(path);
    Ok(result)
}
fn inherited_socket() -> Option<PathBuf> {
    socket_paths(&std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?)
        .into_iter()
        .find(|p| p.exists())
}
fn socket_paths(signature: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        paths.push(
            PathBuf::from(runtime)
                .join("hypr")
                .join(signature)
                .join(".socket.sock"),
        );
    }
    paths.push(
        PathBuf::from("/tmp/hypr")
            .join(signature)
            .join(".socket.sock"),
    );
    paths
}
async fn query(path: &std::path::Path) -> Result<Vec<Client>> {
    tokio::time::timeout(Duration::from_millis(400), async {
        let mut socket = UnixStream::connect(path).await?;
        socket.write_all(b"j/clients").await?;
        socket.shutdown().await?;
        let mut response = Vec::new();
        socket
            .take(2 * 1024 * 1024)
            .read_to_end(&mut response)
            .await?;
        Ok(serde_json::from_slice(&response)?)
    })
    .await?
}
