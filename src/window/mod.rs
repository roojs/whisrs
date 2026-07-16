//! Window tracking: detect focused window and restore focus.
//!
//! Auto-detects the compositor at runtime and provides the appropriate backend.

pub mod dbus;
pub mod gnome_shell;
pub mod hyprland;
pub mod niri;
pub mod sway;
pub mod x11;

use tracing::{info, warn};

use crate::gnome_shell as gnome_shell_util;

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
/// 1. GNOME Shell extension (`org.whisrs.Input`) when present
/// 2. `$HYPRLAND_INSTANCE_SIGNATURE` → Hyprland
/// 3. `$NIRI_SOCKET` → Niri
/// 4. `$SWAYSOCK` → Sway
/// 5. `$XDG_SESSION_TYPE == x11` → X11 (skipped on GNOME — use Shell)
/// 6. Fallback → NoopTracker
pub fn detect_tracker() -> Box<dyn WindowTracker> {
    if gnome_shell_util::is_gnome_desktop() {
        if let Some(tracker) = gnome_shell::GnomeShellTracker::try_new() {
            info!("detected GNOME Shell extension for window tracking");
            return Box::new(tracker);
        }
        // GNOME without the extension: do not fall through to X11 — that fights
        // the "Shell owns the desktop" split. Type at current focus instead.
        info!(
            "GNOME desktop without whisrs Shell extension — window tracking disabled \
             (stay in your target field while recording)"
        );
        return Box::new(NoopTracker);
    }

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

    // KDE on Wayland: no portable window-focus API without compositor hooks.
    if let Ok(desktop) = std::env::var("XDG_CURRENT_DESKTOP") {
        let desktop_lower = desktop.to_lowercase();
        if desktop_lower.contains("kde") {
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
