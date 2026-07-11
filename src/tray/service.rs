//! System tray implementation using ksni (StatusNotifierItem).

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Icon, ToolTip, TrayMethods};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::hotkey::format_hotkey_display;
use crate::{HotkeyConfig, State};

/// 16x16 ARGB icon data for each state.
/// Format: each pixel is 4 bytes (ARGB, big-endian).
mod icons {
    /// Generate a simple 16x16 solid circle icon with the given ARGB color.
    pub fn circle_icon(argb: u32) -> Vec<u8> {
        let size = 16;
        let center = size as f32 / 2.0;
        let radius = 6.0;
        let mut pixels = Vec::with_capacity(size * size * 4);

        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 + 0.5 - center;
                let dy = y as f32 + 0.5 - center;
                let dist = (dx * dx + dy * dy).sqrt();

                if dist <= radius {
                    pixels.extend_from_slice(&argb.to_be_bytes());
                } else if dist <= radius + 1.0 {
                    let alpha = ((radius + 1.0 - dist) * 255.0) as u8;
                    let [_, r, g, b] = argb.to_be_bytes();
                    pixels.extend_from_slice(&[alpha, r, g, b]);
                } else {
                    pixels.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        pixels
    }

    pub fn idle() -> Vec<u8> {
        circle_icon(0xFF_88_88_88)
    }

    pub fn recording() -> Vec<u8> {
        circle_icon(0xFF_E0_40_40)
    }

    pub fn transcribing() -> Vec<u8> {
        circle_icon(0xFF_E0_A0_20)
    }

    /// Read-aloud: synthesizing speech (blue/purple).
    pub fn synthesizing() -> Vec<u8> {
        circle_icon(0xFF_7C_5C_FF)
    }

    /// Read-aloud: playing speech (green).
    pub fn speaking() -> Vec<u8> {
        circle_icon(0xFF_34_D3_99)
    }
}

/// Small mutable state owned by the tray service itself.
///
/// Keeping this directly on the tray object is important: `ksni::Handle::update`
/// expects the closure to mutate the tray instance so the host knows which
/// properties changed. When the state lives out-of-band, some tray hosts can
/// miss icon refreshes and leave the old color visible.
struct TrayState {
    current: State,
    hotkeys: Option<HotkeyConfig>,
}

/// The ksni tray implementation.
struct WhisrsTray {
    state: TrayState,
}

impl WhisrsTray {
    fn status_text(state: State) -> &'static str {
        match state {
            State::Idle => "Idle — ready to record",
            State::Recording => "Recording — press toggle again to stop",
            State::Transcribing => "Transcribing…",
            State::Synthesizing => "Synthesizing speech…",
            State::Speaking => "Reading selection aloud…",
        }
    }

    fn summary(state: State, hotkeys: &Option<HotkeyConfig>) -> String {
        let status = Self::status_text(state);
        if let Some(toggle) = hotkeys.as_ref().and_then(|hk| hk.toggle.as_deref()) {
            format!("{status}\nRecord/stop: {}", format_hotkey_display(toggle))
        } else {
            format!("{status}\nNo hotkey configured")
        }
    }

    fn info_menu_item(label: String) -> MenuItem<Self> {
        StandardItem {
            label,
            enabled: false,
            activate: Box::new(|_| {}),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for WhisrsTray {
    /// On GNOME the tray host opens the menu on click rather than showing
    /// hover tooltips, so a menu is the reliable way to surface shortcuts.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "whisrs".to_string()
    }

    fn title(&self) -> String {
        if let Some(toggle) = self
            .state
            .hotkeys
            .as_ref()
            .and_then(|hk| hk.toggle.as_deref())
        {
            format!(
                "whisrs — {} ({})",
                Self::status_text(self.state.current),
                format_hotkey_display(toggle)
            )
        } else {
            format!("whisrs — {}", Self::status_text(self.state.current))
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let data = match self.state.current {
            State::Idle => icons::idle(),
            State::Recording => icons::recording(),
            State::Transcribing => icons::transcribing(),
            State::Synthesizing => icons::synthesizing(),
            State::Speaking => icons::speaking(),
        };
        vec![Icon {
            width: 16,
            height: 16,
            data,
        }]
    }

    fn tool_tip(&self) -> ToolTip {
        let description = Self::summary(self.state.current, &self.state.hotkeys);
        ToolTip {
            title: "whisrs".to_string(),
            description,
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = vec![Self::info_menu_item(Self::status_text(
            self.state.current,
        ))];

        if let Some(hotkeys) = &self.state.hotkeys {
            if let Some(toggle) = hotkeys.toggle.as_deref() {
                items.push(Self::info_menu_item(format!(
                    "Record/stop: {}",
                    format_hotkey_display(toggle)
                )));
            }
            if let Some(cancel) = hotkeys.cancel.as_deref() {
                items.push(Self::info_menu_item(format!(
                    "Cancel: {}",
                    format_hotkey_display(cancel)
                )));
            }
            if let Some(command) = hotkeys.command.as_deref() {
                items.push(Self::info_menu_item(format!(
                    "Command mode: {}",
                    format_hotkey_display(command)
                )));
            }
            if let Some(speak) = hotkeys.speak.as_deref() {
                items.push(Self::info_menu_item(format!(
                    "Read aloud: {}",
                    format_hotkey_display(speak)
                )));
            }
        } else {
            items.push(Self::info_menu_item(
                "No hotkeys in config — run whisrs toggle".to_string(),
            ));
        }

        items.push(MenuItem::Separator);
        items.push(Self::info_menu_item(
            "On GNOME: click this icon for shortcuts (no hover tooltip)".to_string(),
        ));

        items
    }
}

/// Maximum number of attempts to connect to the SNI tray host.
const TRAY_MAX_RETRIES: u32 = 10;

/// Initial retry delay (doubles each attempt, capped at 10 s).
const TRAY_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Spawn the system tray indicator.
///
/// Runs in the background and updates the icon whenever the daemon state changes.
/// Retries with exponential backoff if the SNI host isn't available yet (common
/// on boot when the daemon starts before the desktop environment is fully ready).
pub async fn spawn_tray(mut state_rx: watch::Receiver<State>, hotkeys: Option<HotkeyConfig>) {
    // Retry spawning the tray with exponential backoff.
    let mut delay = TRAY_INITIAL_DELAY;
    let mut handle = None;

    for attempt in 1..=TRAY_MAX_RETRIES {
        let tray = WhisrsTray {
            state: TrayState {
                current: *state_rx.borrow(),
                hotkeys: hotkeys.clone(),
            },
        };

        match tray.spawn().await {
            Ok(h) => {
                info!("system tray started (attempt {attempt})");
                handle = Some(h);
                break;
            }
            Err(e) => {
                if attempt == TRAY_MAX_RETRIES {
                    warn!(
                        "failed to start system tray after {TRAY_MAX_RETRIES} attempts: {e} — continuing without tray"
                    );
                    return;
                }
                info!(
                    "tray host not available (attempt {attempt}/{TRAY_MAX_RETRIES}): {e} — retrying in {delay:?}"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(10));
            }
        }
    }

    let handle = handle.expect("handle must be set after successful spawn");

    // Watch for state changes and update the tray.
    tokio::spawn(async move {
        while state_rx.changed().await.is_ok() {
            let new_state = *state_rx.borrow();
            debug!("tray state update: {new_state:?}");
            // Mutate the tray object itself so ksni emits the corresponding
            // D-Bus property changes for title, tooltip, and icon pixmap.
            handle
                .update(|tray| {
                    tray.state.current = new_state;
                })
                .await;
        }
    });
}
