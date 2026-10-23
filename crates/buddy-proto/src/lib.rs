//! Versioned newline-delimited JSON protocol shared by the daemon and clients.
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const MAX_BLOBS: usize = 3;
pub const MAX_FRAME: usize = 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Status,
    Watch,
    Pointer,
    Toggle,
    Configure {
        base_url: String,
        model: String,
        #[serde(default)]
        api_key_env: String,
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
