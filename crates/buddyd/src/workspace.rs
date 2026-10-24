//! Select a tool root without overriding an explicit configured boundary.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub async fn select(configured: Option<&Path>, fallback: PathBuf, context: &mut Value) -> PathBuf {
    let detected = context
        .pointer("/working_context/root")
        .and_then(Value::as_str);
    let mut warning = None;
    let (root, source) = if let Some(configured) = configured {
        (configured.to_path_buf(), "configured")
    } else if context
        .pointer("/working_context/status")
        .and_then(Value::as_str)
        == Some("detected")
    {
        match detected {
            Some(path) => match tokio::fs::canonicalize(path).await {
                Ok(path) if usable(&path) && path.is_dir() => (path, "anchor"),
                _ => {
                    warning = Some("Detected directory is unavailable or too broad; using the private Drop workspace");
                    (fallback, "private")
                }
            },
            None => (fallback, "private"),
        }
    } else {
        (fallback, "private")
    };
    if source == "configured" {
        if let (Some(detected), Ok(boundary)) = (detected, tokio::fs::canonicalize(&root).await) {
            if !Path::new(detected).starts_with(&boundary) {
                warning = Some("The anchored application workspace is outside configured project_dir; file tools remain confined to the configured directory");
            }
        }
    }
    context["tool_workspace"] = json!({"root":root,"source":source,"warning":warning,
        "note":"File tools are confined to this root. Working context is refreshed for each message; older conversation paths may belong to a different workspace."});
    root
}

fn usable(path: &Path) -> bool {
    if path.parent().is_none() {
        return false;
    }
    if std::env::var_os("HOME").is_some_and(|home| Path::new(&home) == path) {
        return false;
    }
    ![
        "/proc", "/sys", "/dev", "/etc", "/usr", "/var", "/boot", "/root",
    ]
    .iter()
    .any(|root| path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn configuration_wins_and_ambiguity_keeps_private_root() {
        let fallback = PathBuf::from("/tmp/private-drop");
        let mut context = json!({"working_context":{"status":"detected","root":"/tmp"}});
        assert_eq!(
            select(Some(Path::new("/chosen")), fallback.clone(), &mut context).await,
            PathBuf::from("/chosen")
        );
        assert_eq!(context["tool_workspace"]["source"], "configured");
        assert_eq!(
            select(None, fallback.clone(), &mut context).await,
            PathBuf::from("/tmp")
        );
        context["working_context"]["status"] = json!("ambiguous");
        assert_eq!(select(None, fallback.clone(), &mut context).await, fallback);
    }
    #[tokio::test]
    async fn refuses_root_and_home_as_automatic_workspaces() {
        for path in [
            PathBuf::from("/"),
            PathBuf::from("/etc"),
            std::env::var_os("HOME").map(PathBuf::from).unwrap(),
        ] {
            let fallback = PathBuf::from("/tmp/private-drop");
            let mut context = json!({"working_context":{"status":"detected","root":path}});
            assert_eq!(select(None, fallback.clone(), &mut context).await, fallback);
            assert_eq!(context["tool_workspace"]["source"], "private");
        }
    }
}
