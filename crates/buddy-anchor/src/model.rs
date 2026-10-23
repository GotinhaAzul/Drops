use serde::{Deserialize, Serialize};

/// What a blob is attached to.
///
/// Anchors never store screen coordinates; they store a stable desktop
/// identity so the blob can be re-placed after moves and workspace switches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    /// A specific Hyprland window, identified by its `hyprctl` address.
    Window { address: String },
    /// A workspace, identified by its numeric Hyprland workspace ID.
    Workspace { id: i64 },
}

/// Current screen placement of an anchored blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub anchor: Anchor,
    pub x: i32,
    pub y: i32,
    /// Whether the anchor's workspace is currently visible on a monitor.
    pub visible: bool,
    /// Human-readable label (window title or workspace name).
    pub label: String,
}

/// A point-in-time view of the compositor, shaped like the read-only
/// `hyprctl -j` queries this crate makes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub clients: Vec<Client>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub monitors: Vec<Monitor>,
}

/// Minimal workspace reference embedded in clients and monitors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
}

/// One entry from `hyprctl -j clients`.
///
/// Unknown fields are ignored deliberately: Hyprland adds fields between
/// releases and an anchor crate should not break on a newer compositor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Client {
    #[serde(default)]
    pub address: String,
    #[serde(default = "default_true")]
    pub mapped: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub at: [i32; 2],
    #[serde(default)]
    pub size: [i32; 2],
    #[serde(default)]
    pub workspace: WorkspaceRef,
    #[serde(default)]
    pub floating: bool,
    #[serde(default)]
    pub pinned: bool,
    /// `0` tiled, `1` maximized, `2` fullscreen, `3` fullscreen + maximized.
    #[serde(default)]
    pub fullscreen: i32,
    /// Monitor ID; `-1` when Hyprland did not report one.
    #[serde(default = "default_monitor_id")]
    pub monitor: i32,
    #[serde(default)]
    pub class: String,
    #[serde(default, rename = "initialClass")]
    pub initial_class: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "initialTitle")]
    pub initial_title: String,
    #[serde(default)]
    pub pid: i32,
    #[serde(default)]
    pub xwayland: bool,
    /// `0` is the most recently focused window; `-1` means never focused.
    #[serde(default = "default_focus_history", rename = "focusHistoryID")]
    pub focus_history_id: i32,
}

/// One entry from `hyprctl -j workspaces`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub monitor: String,
    #[serde(default, rename = "monitorID")]
    pub monitor_id: i32,
    #[serde(default)]
    pub windows: u32,
    #[serde(default)]
    pub hasfullscreen: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub lastwindow: String,
    #[serde(default)]
    pub lastwindowtitle: String,
}

/// One entry from `hyprctl -j monitors`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    #[serde(default, rename = "solitaryBlockedBy")]
    pub solitary_blocked_by: Vec<String>,
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, rename = "activeWorkspace")]
    pub active_workspace: WorkspaceRef,
    #[serde(default, rename = "specialWorkspace")]
    pub special_workspace: WorkspaceRef,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            solitary_blocked_by: Vec::new(),
            id: 0,
            name: String::new(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            scale: 1.0,
            focused: false,
            disabled: false,
            active_workspace: WorkspaceRef::default(),
            special_workspace: WorkspaceRef::default(),
        }
    }
}

impl Default for Client {
    fn default() -> Self {
        Client {
            address: String::new(),
            mapped: true,
            hidden: false,
            at: [0, 0],
            size: [0, 0],
            workspace: WorkspaceRef::default(),
            floating: false,
            pinned: false,
            fullscreen: 0,
            monitor: -1,
            class: String::new(),
            initial_class: String::new(),
            title: String::new(),
            initial_title: String::new(),
            pid: 0,
            xwayland: false,
            focus_history_id: -1,
        }
    }
}

impl Monitor {
    /// A monitor can be used for hit-testing only when it is enabled and has
    /// a non-empty logical area.
    pub fn is_enabled(&self) -> bool {
        !self.disabled && self.width > 0 && self.height > 0
    }
}

impl Client {
    /// The workspace this client lives on.
    pub fn workspace_id(&self) -> i64 {
        self.workspace.id
    }

    /// Fullscreen modes that cover the monitor and hide other windows.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen >= 2
    }

    pub fn is_buddy_surface(&self) -> bool {
        crate::hit::is_buddy_surface(self)
    }
}

fn default_true() -> bool {
    true
}

fn default_scale() -> f64 {
    1.0
}

fn default_monitor_id() -> i32 {
    -1
}

fn default_focus_history() -> i32 {
    -1
}

/// Shape of `hyprctl -j cursorpos`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPos {
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
}
