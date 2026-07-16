//! GNOME Shell integration: text injection + control D-Bus for the extension.
//!
//! The extension owns `org.whisrs.Input` and inserts text via the compositor's
//! virtual keyboard (clipboard + Ctrl+V). The daemon owns `org.whisrs.Control`
//! so extension hotkeys can toggle recording without `/dev/uinput` or evdev.

use std::time::Duration;

use anyhow::{anyhow, Context as _};
use tokio::sync::mpsc;
use tracing::{info, warn};
use xkb_type::KeyInjector;
use zbus::interface;

use crate::{Command, HotkeyConfig};

const INPUT_DEST: &str = "org.whisrs.Input";
const INPUT_PATH: &str = "/org/whisrs/Input";
const INPUT_IFACE: &str = "org.whisrs.Input";

const CONTROL_PATH: &str = "/org/whisrs/Control";
const CONTROL_NAME: &str = "org.whisrs.Control";

/// Run a blocking zbus call off the Tokio runtime.
///
/// `zbus::blocking` spins its own runtime; calling it from `#[tokio::main]`
/// panics with "Cannot start a runtime from within a runtime".
fn with_blocking_session<T, F>(f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&zbus::blocking::Connection) -> anyhow::Result<T> + Send + 'static,
{
    std::thread::Builder::new()
        .name("whisrs-gnome-dbus".into())
        .spawn(move || {
            let conn = zbus::blocking::Connection::session()
                .context("failed to connect to session bus for GNOME Shell")?;
            f(&conn)
        })
        .context("failed to spawn GNOME Shell D-Bus thread")?
        .join()
        .map_err(|_| anyhow!("GNOME Shell D-Bus thread panicked"))?
}

/// Whether the GNOME Shell extension is exporting the Input interface.
pub fn input_available() -> bool {
    with_blocking_session(|conn| {
        let reply = conn.call_method(
            Some(INPUT_DEST),
            INPUT_PATH,
            Some(INPUT_IFACE),
            "Ping",
            &(),
        )?;
        let pong: String = reply.body().deserialize().unwrap_or_default();
        Ok(pong == "ok")
    })
    .unwrap_or(false)
}

/// KeyInjector that routes typing through the GNOME Shell extension over D-Bus.
pub struct GnomeShellKeyboard;

impl GnomeShellKeyboard {
    pub fn new() -> anyhow::Result<Self> {
        let pong = with_blocking_session(|conn| {
            let reply = conn
                .call_method(Some(INPUT_DEST), INPUT_PATH, Some(INPUT_IFACE), "Ping", &())
                .context(
                    "GNOME Shell input unavailable — is the whisrs extension enabled?\n\
                     Install: contrib/gnome-shell-extension/whisrs-overlay@eresende.github",
                )?;
            let pong: String = reply.body().deserialize().unwrap_or_default();
            Ok(pong)
        })?;
        if pong != "ok" {
            anyhow::bail!("GNOME Shell Input.Ping returned unexpected value: {pong:?}");
        }
        Ok(Self)
    }

    fn call_bool<B>(&self, method: &str, body: B) -> anyhow::Result<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType + Send + 'static,
    {
        let method = method.to_string();
        with_blocking_session(move |conn| {
            let reply = conn
                .call_method(
                    Some(INPUT_DEST),
                    INPUT_PATH,
                    Some(INPUT_IFACE),
                    method.as_str(),
                    &body,
                )
                .with_context(|| format!("GNOME Shell Input.{method} failed"))?;
            let ok: bool = reply
                .body()
                .deserialize()
                .with_context(|| format!("decode Input.{method} reply"))?;
            if ok {
                Ok(())
            } else {
                Err(anyhow!("GNOME Shell Input.{method} returned false"))
            }
        })
    }
}

impl KeyInjector for GnomeShellKeyboard {
    fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.call_bool("TypeText", text.to_string())
    }

    fn backspace(&mut self, count: usize) -> anyhow::Result<()> {
        if count == 0 {
            return Ok(());
        }
        self.call_bool("Backspace", count as u32)
    }

    fn select_left(&mut self, count: usize) -> anyhow::Result<()> {
        if count == 0 {
            return Ok(());
        }
        self.call_bool("SelectLeft", count as u32)
    }

    fn send_combo(&mut self, keys: &[evdev::Key]) -> anyhow::Result<()> {
        let shortcut = combo_to_shortcut(keys)
            .ok_or_else(|| anyhow!("unsupported key combo for GNOME Shell injector: {keys:?}"))?;
        self.call_bool("SendShortcut", shortcut)
    }

    fn set_key_delay(&mut self, _delay: Duration) {
        // Clipboard paste path — inter-key delay does not apply.
    }
}

fn combo_to_shortcut(keys: &[evdev::Key]) -> Option<String> {
    use evdev::Key;
    let mut mods = Vec::new();
    let mut main = None;
    for key in keys {
        match *key {
            Key::KEY_LEFTCTRL | Key::KEY_RIGHTCTRL => mods.push("ctrl"),
            Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT => mods.push("shift"),
            Key::KEY_LEFTALT | Key::KEY_RIGHTALT => mods.push("alt"),
            Key::KEY_LEFTMETA | Key::KEY_RIGHTMETA => mods.push("super"),
            other => {
                let name = match other {
                    Key::KEY_C => "c",
                    Key::KEY_V => "v",
                    Key::KEY_A => "a",
                    Key::KEY_K => "k",
                    Key::KEY_X => "x",
                    Key::KEY_INSERT => "insert",
                    Key::KEY_BACKSPACE => "backspace",
                    Key::KEY_LEFT => "left",
                    _ => return None,
                };
                main = Some(name);
            }
        }
    }
    let main = main?;
    if mods.is_empty() {
        Some(main.to_string())
    } else {
        Some(format!("{}+{main}", mods.join("+")))
    }
}

struct ControlBus {
    cmd_tx: mpsc::Sender<Command>,
    hotkeys: HotkeyConfig,
}

#[interface(name = "org.whisrs.Control")]
impl ControlBus {
    #[zbus(name = "Toggle")]
    async fn toggle(&self) {
        let _ = self.cmd_tx.send(Command::Toggle).await;
    }

    #[zbus(name = "Cancel")]
    async fn cancel(&self) {
        let _ = self.cmd_tx.send(Command::Cancel).await;
    }

    #[zbus(name = "Command")]
    async fn command(&self) {
        let _ = self.cmd_tx.send(Command::CommandMode).await;
    }

    #[zbus(name = "Speak")]
    async fn speak(&self) {
        let _ = self.cmd_tx.send(Command::Speak).await;
    }

    #[zbus(name = "GetHotkeys")]
    fn get_hotkeys(&self) -> (String, String, String, String) {
        (
            self.hotkeys.toggle.clone().unwrap_or_default(),
            self.hotkeys.cancel.clone().unwrap_or_default(),
            self.hotkeys.command.clone().unwrap_or_default(),
            self.hotkeys.speak.clone().unwrap_or_default(),
        )
    }

    #[zbus(name = "Ping")]
    fn ping(&self) -> &'static str {
        "ok"
    }
}

/// Serve `org.whisrs.Control` and forward extension hotkey commands on `cmd_tx`.
pub async fn serve_control(
    cmd_tx: mpsc::Sender<Command>,
    hotkeys: HotkeyConfig,
) -> anyhow::Result<()> {
    let bus = ControlBus { cmd_tx, hotkeys };
    let _conn = zbus::connection::Builder::session()?
        .name(CONTROL_NAME)?
        .serve_at(CONTROL_PATH, bus)?
        .build()
        .await?;
    info!("GNOME Shell control D-Bus started ({CONTROL_NAME})");
    std::future::pending::<()>().await;
    Ok(())
}

/// Spawn the control bus; logs and returns if the name is already taken.
pub fn spawn_control(cmd_tx: mpsc::Sender<Command>, hotkeys: Option<HotkeyConfig>) {
    let hotkeys = hotkeys.unwrap_or_default();
    tokio::spawn(async move {
        if let Err(e) = serve_control(cmd_tx, hotkeys).await {
            warn!("GNOME Shell control D-Bus unavailable: {e:#}");
        }
    });
}

/// True when the session looks like GNOME.
pub fn is_gnome_desktop() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|value| {
            value
                .split(':')
                .any(|part| part.eq_ignore_ascii_case("GNOME"))
        })
        .unwrap_or(false)
}

/// Shared helper used by Auto backend selection.
pub fn probe_for_auto() -> Option<GnomeShellKeyboard> {
    if !is_gnome_desktop() {
        return None;
    }
    match GnomeShellKeyboard::new() {
        Ok(kb) => {
            info!("using GNOME Shell injection backend");
            Some(kb)
        }
        Err(e) => {
            warn!("GNOME Shell input unavailable, continuing Auto fallback: {e:#}");
            None
        }
    }
}

/// Focused window id + wm_class from the Shell extension.
pub fn get_focused_window() -> anyhow::Result<(String, String)> {
    with_blocking_session(|conn| {
        let reply = conn
            .call_method(
                Some(INPUT_DEST),
                INPUT_PATH,
                Some(INPUT_IFACE),
                "GetFocusedWindow",
                &(),
            )
            .context("GNOME Shell Input.GetFocusedWindow failed")?;
        let (id, wm_class): (String, String) = reply
            .body()
            .deserialize()
            .context("decode GetFocusedWindow reply")?;
        Ok((id, wm_class))
    })
}

/// Activate a window previously returned by [`get_focused_window`].
pub fn focus_window(id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    with_blocking_session(move |conn| {
        let reply = conn
            .call_method(
                Some(INPUT_DEST),
                INPUT_PATH,
                Some(INPUT_IFACE),
                "FocusWindow",
                &id,
            )
            .context("GNOME Shell Input.FocusWindow failed")?;
        let ok: bool = reply
            .body()
            .deserialize()
            .context("decode FocusWindow reply")?;
        if ok {
            Ok(())
        } else {
            Err(anyhow!("GNOME Shell could not focus window id={id}"))
        }
    })
}
