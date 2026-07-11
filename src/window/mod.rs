//! Window tracking: detect focused window and restore focus.
//!
//! Auto-detects the compositor at runtime and provides the appropriate backend.

pub mod dbus;
pub mod hyprland;
pub mod niri;
pub mod sway;
pub mod x11;

use tracing::{info, warn};

/// Trait for tracking and restoring window focus.
pub trait WindowTracker: Send + Sync {
    /// Get the identifier of the currently focused window.
    fn get_focused_window(&self) -> anyhow::Result<String>;

    /// Focus the window with the given identifier.
    fn focus_window(&self, id: &str) -> anyhow::Result<()>;

    /// Get the window class of the currently focused window (e.g. "Alacritty", "firefox").
    /// Returns `None` if the compositor does not support this query.
    fn get_focused_window_class(&self) -> Option<String> {
        None
    }

    /// Whether this backend can re-activate a captured window before typing.
    /// When `false`, whisrs types at the current keyboard focus (the field
    /// that was active when you toggled recording).
    fn supports_focus_restore(&self) -> bool {
        true
    }
}

/// A no-op tracker that always succeeds without doing anything.
///
/// Used as a graceful fallback when compositor detection fails.
pub struct NoopTracker;

impl WindowTracker for NoopTracker {
    fn get_focused_window(&self) -> anyhow::Result<String> {
        Ok("noop".to_string())
    }

    fn focus_window(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn supports_focus_restore(&self) -> bool {
        false
    }
}

/// Auto-detect the compositor and return the appropriate `WindowTracker`.
///
/// Detection order:
/// 1. `$HYPRLAND_INSTANCE_SIGNATURE` → Hyprland
/// 2. `$NIRI_SOCKET` → Niri
/// 3. `$SWAYSOCK` → Sway
/// 4. `$XDG_SESSION_TYPE == x11` → X11
/// 5. Fallback → NoopTracker
pub fn detect_tracker() -> Box<dyn WindowTracker> {
    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() {
        info!("detected Hyprland compositor for window tracking");
        return Box::new(hyprland::HyprlandTracker::new());
    }

    if std::env::var("NIRI_SOCKET").is_ok() {
        info!("detected Niri compositor for window tracking");
        return Box::new(niri::NiriTracker::new());
    }

    if std::env::var("SWAYSOCK").is_ok() {
        info!("detected Sway compositor for window tracking");
        return Box::new(sway::SwayTracker::new());
    }

    if std::env::var("XDG_SESSION_TYPE")
        .map(|v| v == "x11")
        .unwrap_or(false)
    {
        info!("detected X11 session for window tracking");
        match x11::X11Tracker::new() {
            Ok(tracker) => return Box::new(tracker),
            Err(e) => {
                warn!("failed to initialize X11 tracker: {e}; falling back to noop");
            }
        }
    }

    // GNOME / KDE on Wayland: no portable window-focus API without compositor-
    // specific hooks. whisrs types at whatever has keyboard focus when you
    // toggle — keep the cursor in your target field while dictating.
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        let desktop_lower = desktop.to_lowercase();
        if desktop_lower.contains("gnome") || desktop_lower.contains("kde") {
            info!(
                "detected {desktop} on Wayland — typing at current keyboard focus \
                 (no window refocus; stay in your target field while recording)"
            );
            return Box::new(NoopTracker);
        }
    }

    warn!("could not detect compositor — window tracking disabled (using noop)");
    Box::new(NoopTracker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_tracker_always_succeeds() {
        let tracker = NoopTracker;
        let id = tracker.get_focused_window().unwrap();
        assert_eq!(id, "noop");
        tracker.focus_window("anything").unwrap();
    }

    #[test]
    fn detect_tracker_returns_something() {
        // In a test environment, we may not have any compositor running,
        // but detect_tracker should never panic — it should return NoopTracker.
        let tracker = detect_tracker();
        // Just verify it doesn't panic on use.
        let _ = tracker.get_focused_window();
    }
}
