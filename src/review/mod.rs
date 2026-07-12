//! Review dialog shown after dictation, before text is pasted.
//!
//! Uses the desktop's native dialog tools (`zenity`, `yad`, or `kdialog`) so
//! the user can edit the full transcription before it reaches the insert target.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use tracing::{debug, info, warn};

/// Preferred dialog width/height in pixels.
const DIALOG_WIDTH: u32 = 640;
const DIALOG_HEIGHT: u32 = 260;
/// Gap between the insert target and the dialog top edge.
const BELOW_TARGET_GAP: i32 = 12;

/// Screen position for the review dialog, if we could estimate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogPlacement {
    pub x: i32,
    pub y: i32,
}

/// Show an editable review dialog and return the user's final text.
///
/// Returns `Ok(None)` when the user cancels or dismisses the dialog.
pub fn show_review_dialog(text: &str) -> anyhow::Result<Option<String>> {
    let placement = estimate_dialog_placement();
    if let Some(p) = placement {
        debug!("review dialog placement: {p:?}");
    }

    if let Some(result) = try_zenity(text, placement)? {
        return Ok(Some(result));
    }
    if let Some(result) = try_yad(text, placement)? {
        return Ok(Some(result));
    }
    if let Some(result) = try_kdialog(text)? {
        return Ok(Some(result));
    }

    anyhow::bail!(
        "no review dialog tool found — install zenity, yad, or kdialog \
         (on Ubuntu: sudo apt install zenity)"
    )
}

/// Best-effort placement just below the active window (X11) or near the bottom
/// of the primary monitor when geometry is unavailable (GNOME Wayland, etc.).
fn estimate_dialog_placement() -> Option<DialogPlacement> {
    if let Ok(Some(rect)) = x11_active_window_rect() {
        let x = rect.x + (rect.width as i32 - DIALOG_WIDTH as i32) / 2;
        let y = rect.y + rect.height as i32 + BELOW_TARGET_GAP;
        return Some(DialogPlacement { x: x.max(0), y: y.max(0) });
    }

    if let Ok(Some((screen_w, screen_h))) = primary_screen_size() {
        let x = (screen_w as i32 - DIALOG_WIDTH as i32) / 2;
        // Lower third of the screen — near where users type, below typical fields.
        let y = screen_h as i32 * 2 / 3;
        return Some(DialogPlacement {
            x: x.max(0),
            y: y.max(0),
        });
    }

    None
}

#[derive(Debug, Clone, Copy)]
struct WindowRect {
    x: i32,
    y: i32,
    width: u16,
    height: u16,
}

fn x11_active_window_rect() -> anyhow::Result<Option<WindowRect>> {
    if std::env::var_os("DISPLAY").is_none() {
        return Ok(None);
    }

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let root = conn.setup().roots[screen_num].root;
    let atom = conn
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
        .reply()?
        .atom;

    let reply = conn
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)?
        .reply()?;
    let Some(bytes) = reply.value.get(..4) else {
        return Ok(None);
    };
    let window = u32::from_ne_bytes(bytes.try_into().expect("4 bytes"));
    if window == 0 {
        return Ok(None);
    }

    let geometry = conn.get_geometry(window)?.reply()?;
    let translated = conn.translate_coordinates(window, root, 0, 0)?.reply()?;
    if !translated.same_screen {
        return Ok(None);
    }

    Ok(Some(WindowRect {
        x: i32::from(translated.dst_x),
        y: i32::from(translated.dst_y),
        width: geometry.width,
        height: geometry.height,
    }))
}

fn primary_screen_size() -> anyhow::Result<Option<(u32, u32)>> {
    if std::env::var_os("DISPLAY").is_none() {
        return Ok(None);
    }

    use x11rb::connection::Connection;
    use x11rb::rust_connection::RustConnection;

    let (conn, screen_num) = match RustConnection::connect(None) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let screen = &conn.setup().roots[screen_num];
    Ok(Some((
        u32::from(screen.width_in_pixels),
        u32::from(screen.height_in_pixels),
    )))
}

fn try_zenity(text: &str, placement: Option<DialogPlacement>) -> anyhow::Result<Option<String>> {
    let zenity = which::zenity_path();
    let Some(zenity) = zenity else {
        return Ok(None);
    };

    let tmp_path = std::env::temp_dir().join(format!("whisrs-review-{}.txt", std::process::id()));
    std::fs::write(&tmp_path, text)?;

    let mut cmd = Command::new(zenity);
    cmd.args([
        "--text-info",
        "--editable",
        "--title=whisrs — review transcription",
        "--width",
        &DIALOG_WIDTH.to_string(),
        "--height",
        &DIALOG_HEIGHT.to_string(),
        "--timeout",
        "600",
        "--filename",
    ]);
    cmd.arg(&tmp_path);
    if let Some(p) = placement {
        cmd.arg(format!(
            "--geometry={}x{}+{}+{}",
            DIALOG_WIDTH, DIALOG_HEIGHT, p.x, p.y
        ));
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let output = cmd.output();
    let output = output?;
    let edited_on_ok = if output.status.code() == Some(0) {
        let from_stdout = String::from_utf8(output.stdout.clone())?
            .trim()
            .to_string();
        if from_stdout.is_empty() {
            std::fs::read_to_string(&tmp_path)
                .unwrap_or_default()
                .trim()
                .to_string()
        } else {
            from_stdout
        }
    } else {
        String::new()
    };
    let _ = std::fs::remove_file(&tmp_path);

    match output.status.code() {
        Some(0) => {
            info!("review dialog accepted ({} chars)", edited_on_ok.len());
            Ok(Some(edited_on_ok))
        }
        Some(1) => {
            debug!("review dialog cancelled");
            Ok(None)
        }
        _ => {
            warn!(
                "zenity review dialog failed (exit {:?})",
                output.status.code()
            );
            Ok(None)
        }
    }
}

fn try_yad(text: &str, placement: Option<DialogPlacement>) -> anyhow::Result<Option<String>> {
    let yad = which::yad_path();
    let Some(yad) = yad else {
        return Ok(None);
    };

    let mut cmd = Command::new(yad);
    cmd.args([
        "--text-info",
        "--editable",
        "--title=whisrs — review transcription",
        "--width",
        &DIALOG_WIDTH.to_string(),
        "--height",
        &DIALOG_HEIGHT.to_string(),
        "--button=Paste:0",
        "--button=Cancel:1",
    ]);
    if let Some(p) = placement {
        cmd.args([
            "--posx",
            &p.x.to_string(),
            "--posy",
            &p.y.to_string(),
        ]);
    }
    cmd.arg("--text").arg(text);

    let output = cmd.output()?;
    match output.status.code() {
        Some(0) => {
            let edited = String::from_utf8(output.stdout)?.trim().to_string();
            info!("review dialog accepted via yad ({} chars)", edited.len());
            Ok(Some(edited))
        }
        Some(1) => Ok(None),
        _ => Ok(None),
    }
}

fn try_kdialog(text: &str) -> anyhow::Result<Option<String>> {
    let kdialog = which::kdialog_path();
    let Some(kdialog) = kdialog else {
        return Ok(None);
    };

    let output = Command::new(kdialog)
        .args([
            "--title",
            "whisrs — review transcription",
            "--textbox",
            "/dev/stdin",
            &DIALOG_WIDTH.to_string(),
            &DIALOG_HEIGHT.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(text.as_bytes())?;
            }
            child.wait_with_output()
        })?;

    match output.status.code() {
        Some(0) => {
            let edited = String::from_utf8(output.stdout)?.trim().to_string();
            Ok(Some(edited))
        }
        _ => Ok(None),
    }
}

mod which {
    use std::path::PathBuf;

    pub fn zenity_path() -> Option<PathBuf> {
        ["zenity", "/usr/bin/zenity"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }

    pub fn yad_path() -> Option<PathBuf> {
        ["yad", "/usr/bin/yad"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }

    pub fn kdialog_path() -> Option<PathBuf> {
        ["kdialog", "/usr/bin/kdialog"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }
}

/// Block up to `timeout` waiting for a review tool to become available.
#[allow(dead_code)]
pub fn wait_for_dialog_tool(timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if which::zenity_path().is_some()
            || which::yad_path().is_some()
            || which::kdialog_path().is_some()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_below_window_centers_horizontally() {
        let rect = WindowRect {
            x: 100,
            y: 200,
            width: 800,
            height: 400,
        };
        let x = rect.x + (rect.width as i32 - DIALOG_WIDTH as i32) / 2;
        let y = rect.y + rect.height as i32 + BELOW_TARGET_GAP;
        assert_eq!(x, 180);
        assert_eq!(y, 612);
    }
}
