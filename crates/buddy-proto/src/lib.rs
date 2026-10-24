//! Versioned newline-delimited JSON protocol shared by the daemon and clients.
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const MAX_BLOBS: usize = 3;
pub const MAX_FRAME: usize = 1024 * 1024;

/// An API key carried over IPC.
///
/// `Debug` is always redacted so a log line cannot expose the key, while
/// serde keeps the wire format a plain JSON string.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Status,
    Watch,
    Pointer,
    Toggle,
    /// Run tools that normally need approval without asking first.
    Sudo {
        enabled: bool,
    },
    Lively {
        enabled: bool,
    },
    Configure {
        /// `custom` keeps the original endpoint fields; `opencode_go` uses the
        /// Opencode Go gateway, its model list, and a stored key.
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        base_url: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        api_key_env: Option<String>,
        /// New key to store. Omitted or empty keeps the saved Opencode Go key.
        #[serde(default)]
        api_key: Option<ApiKey>,
        #[serde(default)]
        vision: Option<bool>,
        #[serde(default)]
        capture_content: Option<bool>,
        #[serde(default)]
        screenshots: Option<bool>,
    },
    /// Fetch the active Opencode Go model list. `api_key` lets the settings
    /// screen test a key before saving it.
    Models {
        #[serde(default)]
        api_key: Option<ApiKey>,
    },
    Create {
        #[serde(default)]
        appearance: DropAppearance,
    },
    Style {
        id: String,
        appearance: DropAppearance,
    },
    Preview {
        x: i32,
        y: i32,
        #[serde(default)]
        use_cursor: bool,
    },
    Drop {
        x: i32,
        y: i32,
        #[serde(default)]
        use_cursor: bool,
        #[serde(default)]
        id: Option<String>,
    },
    Send {
        id: String,
        text: String,
    },
    Confirm {
        id: String,
        approve: bool,
    },
    Cancel {
        #[serde(default)]
        id: Option<String>,
    },
    Recall {
        id: String,
    },
    Remove {
        id: String,
    },
    Context {
        id: String,
    },
    Export {
        id: String,
    },
    Clear {
        id: String,
    },
    Fork {
        id: String,
    },
}
/// Small, named visual presets persist independently for each drop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropAppearance {
    #[default]
    Drop,
    Cat,
    Sprout,
    Bunny,
    Robot,
    Cloud,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blob {
    #[serde(default)]
    pub appearance: DropAppearance,
    /// Decoration offset from the coordinate-free anchor reference.
    #[serde(default)]
    pub offset: [i32; 2],
    pub id: String,
    pub placement: Value,
    pub state: String,
    pub history: Vec<Value>,
    pub context: Value,
    pub approval: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// Explicit opt-in "sudo" mode: mutating and dangerous tools run without a
    /// per-call approval prompt. Defaults off, including for old state files.
    #[serde(default)]
    pub sudo: bool,
    /// Opt-in expressive motion and status cues, persisted with app state.
    #[serde(default)]
    pub lively: bool,
    #[serde(default)]
    pub connection: Value,
    #[serde(default)]
    pub appearance: Value,
    pub panel_open: bool,
    pub blobs: Vec<Blob>,
    pub version: u32,
}
pub fn socket_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("DROPS_SOCKET") {
        return p.into();
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "drops-{}",
                std::env::var("USER").unwrap_or_else(|_| "user".into())
            ))
        })
        .join("drops.sock")
}

/// Avoid trusting a predictable socket under a shared temporary directory.
pub fn validate_socket_environment() -> std::io::Result<()> {
    if std::env::var_os("DROPS_SOCKET").is_none() && std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        return Err(std::io::Error::other(
            "XDG_RUNTIME_DIR is unset; configure an explicit private DROPS_SOCKET path",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_keys_are_redacted_in_command_debug_output() {
        let command = Command::Configure {
            mode: Some("opencode_go".into()),
            base_url: None,
            model: Some("kimi-k3".into()),
            api_key_env: None,
            api_key: Some(ApiKey::new("super-secret-key")),
            vision: None,
            capture_content: None,
            screenshots: None,
        };
        let debug = format!("{command:?}");
        assert!(!debug.contains("super-secret-key"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn api_keys_stay_plain_strings_on_the_wire() {
        let command: Command =
            serde_json::from_str(r#"{"type":"models","api_key":"wire-secret"}"#).expect("command");
        let Command::Models { api_key } = command else {
            panic!("expected models");
        };
        assert_eq!(api_key.expect("key").expose(), "wire-secret");
        assert_eq!(
            serde_json::to_value(Command::Models {
                api_key: Some(ApiKey::new("wire-secret"))
            })
            .expect("serialize"),
            serde_json::json!({"type":"models","api_key":"wire-secret"})
        );
    }
}
