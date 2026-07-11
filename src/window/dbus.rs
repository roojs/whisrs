//! D-Bus window tracking stub (reserved for future KDE integration).

use tracing::warn;

use super::WindowTracker;

/// Stub window tracker for KDE desktops via D-Bus.
pub struct DbusTracker {
    desktop: String,
}

impl DbusTracker {
    pub fn new(desktop: &str) -> Self {
        Self {
            desktop: desktop.to_string(),
        }
    }
}

impl WindowTracker for DbusTracker {
    fn get_focused_window(&self) -> anyhow::Result<String> {
        warn!(
            "{} window tracking not yet supported — text will be typed at current cursor",
            self.desktop
        );
        // Return a placeholder so the flow doesn't break.
        Ok("dbus-stub".to_string())
    }

    fn focus_window(&self, _id: &str) -> anyhow::Result<()> {
        warn!(
            "{} window focus restoration not yet supported — skipping",
            self.desktop
        );
        // Don't fail — graceful degradation.
        Ok(())
    }

    fn supports_focus_restore(&self) -> bool {
        false
    }
}
