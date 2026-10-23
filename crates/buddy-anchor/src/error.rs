use std::fmt;
use std::io;

/// Errors surfaced by the anchor crate.
///
/// The important distinction is between a session we cannot observe
/// ([`AnchorError::DesktopUnavailable`]) and a live session that simply does
/// not contain the requested window or workspace. Nothing in this crate
/// fabricates a desktop state to paper over a missing Hyprland session.
#[derive(Debug)]
pub enum AnchorError {
    /// No usable Hyprland session (headless machine, wrong user, stale
    /// environment). The message explains what was missing.
    DesktopUnavailable(String),
    /// A drop coordinate was not inside any enabled monitor.
    NotOnDesktop { x: i32, y: i32 },
    /// The requested window address is not present in `hyprctl clients`.
    WindowNotFound(String),
    /// The requested workspace ID is not present in `hyprctl workspaces` and
    /// is not the active workspace of any monitor.
    WorkspaceNotFound(i64),
    /// A window closed and there is no active workspace to fall back to.
    NoActiveWorkspace,
    /// `hyprctl` failed or returned something that was not valid JSON.
    Hyprctl(String),
    /// Local I/O failed while reading `/proc` or the event socket.
    Io(io::Error),
}

impl fmt::Display for AnchorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnchorError::DesktopUnavailable(message) => {
                write!(f, "Hyprland is not available: {message}")
            }
            AnchorError::NotOnDesktop { x, y } => {
                write!(f, "point ({x}, {y}) is not on any enabled monitor")
            }
            AnchorError::WindowNotFound(address) => {
                write!(f, "window {address} is not on the desktop")
            }
            AnchorError::WorkspaceNotFound(id) => {
                write!(f, "workspace {id} is not on the desktop")
            }
            AnchorError::NoActiveWorkspace => {
                write!(f, "no active workspace is available for the fallback")
            }
            AnchorError::Hyprctl(message) => write!(f, "hyprctl failed: {message}"),
            AnchorError::Io(source) => write!(f, "I/O error: {source}"),
        }
    }
}

impl std::error::Error for AnchorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AnchorError::Io(source) => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for AnchorError {
    fn from(source: io::Error) -> Self {
        AnchorError::Io(source)
    }
}
