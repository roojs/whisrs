//! Window tracking via the GNOME Shell extension (`org.whisrs.Input`).

use tracing::debug;

use super::WindowTracker;
use crate::gnome_shell;

/// Focus get/restore through Shell — no X11 client APIs.
pub struct GnomeShellTracker;

impl GnomeShellTracker {
    pub fn new() -> Self {
        Self
    }

    pub fn try_new() -> Option<Self> {
        if gnome_shell::input_available() {
            Some(Self)
        } else {
            None
        }
    }
}

impl WindowTracker for GnomeShellTracker {
    fn get_focused_window(&self) -> anyhow::Result<String> {
        let (id, _class) = gnome_shell::get_focused_window()?;
        if id.is_empty() {
            anyhow::bail!("no focused window from GNOME Shell");
        }
        debug!("GNOME Shell focused window id={id}");
        Ok(id)
    }

    fn focus_window(&self, id: &str) -> anyhow::Result<()> {
        gnome_shell::focus_window(id)
    }

    fn get_focused_window_class(&self) -> Option<String> {
        gnome_shell::get_focused_window()
            .ok()
            .map(|(_, class)| class)
            .filter(|c| !c.is_empty())
    }

    fn supports_focus_restore(&self) -> bool {
        true
    }
}
