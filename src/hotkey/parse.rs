//! Parse hotkey strings like "Super+Shift+D" into evdev key sets.

use evdev::Key;

/// A parsed hotkey binding: a set of modifier keys + one trigger key.
#[derive(Debug, Clone)]
pub struct HotkeyBinding {
    /// Modifier keys that must be held (e.g. Super, Shift, Ctrl, Alt).
    pub modifiers: Vec<Key>,
    /// The trigger key that fires the hotkey when pressed while modifiers are held.
    pub trigger: Key,
}

/// Parse a hotkey string like "Super+Shift+D" into a `HotkeyBinding`.
///
/// Format: `Modifier+Modifier+Key` (case-insensitive).
/// Supported modifiers: Super, Alt, Ctrl, Shift.
pub fn parse_hotkey(s: &str) -> anyhow::Result<HotkeyBinding> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        anyhow::bail!("empty hotkey string");
    }
    if parts.len() < 2 {
        anyhow::bail!(
            "hotkey must have at least one modifier and a key (e.g. \"Super+D\"), got: {s}"
        );
    }

    let mut modifiers = Vec::new();
    for part in &parts[..parts.len() - 1] {
        let key = parse_modifier(part).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown modifier '{part}' in hotkey '{s}'. Valid: Super, Alt, Ctrl, Shift"
            )
        })?;
        modifiers.push(key);
    }

    let trigger_str = parts.last().unwrap();
    let trigger = parse_key(trigger_str)
        .ok_or_else(|| anyhow::anyhow!("unknown key '{trigger_str}' in hotkey '{s}'"))?;

    Ok(HotkeyBinding { modifiers, trigger })
}

/// Render a config hotkey string for humans (tooltips, tray menus).
///
/// `Super`/`Meta`/`Win` become "Windows key"; other parts keep familiar labels.
pub fn format_hotkey_display(s: &str) -> String {
    s.split('+')
        .map(|part| format_hotkey_part(part.trim()))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn format_hotkey_part(part: &str) -> String {
    match part.to_lowercase().as_str() {
        "super" | "meta" | "win" | "hyper" => "Windows key".to_string(),
        "ctrl" | "control" => "Ctrl".to_string(),
        "alt" => "Alt".to_string(),
        "shift" => "Shift".to_string(),
        "space" => "Space".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "escape" | "esc" => "Esc".to_string(),
        "tab" => "Tab".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" | "del" => "Delete".to_string(),
        "insert" | "ins" => "Insert".to_string(),
        "pageup" | "pgup" => "Page Up".to_string(),
        "pagedown" | "pgdn" => "Page Down".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "f1" => "F1".to_string(),
        "f2" => "F2".to_string(),
        "f3" => "F3".to_string(),
        "f4" => "F4".to_string(),
        "f5" => "F5".to_string(),
        "f6" => "F6".to_string(),
        "f7" => "F7".to_string(),
        "f8" => "F8".to_string(),
        "f9" => "F9".to_string(),
        "f10" => "F10".to_string(),
        "f11" => "F11".to_string(),
        "f12" => "F12".to_string(),
        other if other.len() == 1 => other.to_uppercase(),
        other => part.to_string(),
    }
}

fn parse_modifier(s: &str) -> Option<Key> {
    match s.to_lowercase().as_str() {
        "super" | "meta" | "win" | "hyper" => Some(Key::KEY_LEFTMETA),
        "alt" => Some(Key::KEY_LEFTALT),
        "ctrl" | "control" => Some(Key::KEY_LEFTCTRL),
        "shift" => Some(Key::KEY_LEFTSHIFT),
        _ => None,
    }
}

fn parse_key(s: &str) -> Option<Key> {
    // Single letter keys.
    if s.len() == 1 {
        let ch = s.to_uppercase().chars().next()?;
        return match ch {
            'A' => Some(Key::KEY_A),
            'B' => Some(Key::KEY_B),
            'C' => Some(Key::KEY_C),
            'D' => Some(Key::KEY_D),
            'E' => Some(Key::KEY_E),
            'F' => Some(Key::KEY_F),
            'G' => Some(Key::KEY_G),
            'H' => Some(Key::KEY_H),
            'I' => Some(Key::KEY_I),
            'J' => Some(Key::KEY_J),
            'K' => Some(Key::KEY_K),
            'L' => Some(Key::KEY_L),
            'M' => Some(Key::KEY_M),
            'N' => Some(Key::KEY_N),
            'O' => Some(Key::KEY_O),
            'P' => Some(Key::KEY_P),
            'Q' => Some(Key::KEY_Q),
            'R' => Some(Key::KEY_R),
            'S' => Some(Key::KEY_S),
            'T' => Some(Key::KEY_T),
            'U' => Some(Key::KEY_U),
            'V' => Some(Key::KEY_V),
            'W' => Some(Key::KEY_W),
            'X' => Some(Key::KEY_X),
            'Y' => Some(Key::KEY_Y),
            'Z' => Some(Key::KEY_Z),
            _ => None,
        };
    }

    // Named keys (case-insensitive).
    match s.to_lowercase().as_str() {
        "space" => Some(Key::KEY_SPACE),
        "enter" | "return" => Some(Key::KEY_ENTER),
        "escape" | "esc" => Some(Key::KEY_ESC),
        "tab" => Some(Key::KEY_TAB),
        "backspace" => Some(Key::KEY_BACKSPACE),
        "delete" | "del" => Some(Key::KEY_DELETE),
        "insert" | "ins" => Some(Key::KEY_INSERT),
        "home" => Some(Key::KEY_HOME),
        "end" => Some(Key::KEY_END),
        "pageup" | "pgup" => Some(Key::KEY_PAGEUP),
        "pagedown" | "pgdn" => Some(Key::KEY_PAGEDOWN),
        "up" => Some(Key::KEY_UP),
        "down" => Some(Key::KEY_DOWN),
        "left" => Some(Key::KEY_LEFT),
        "right" => Some(Key::KEY_RIGHT),
        "f1" => Some(Key::KEY_F1),
        "f2" => Some(Key::KEY_F2),
        "f3" => Some(Key::KEY_F3),
        "f4" => Some(Key::KEY_F4),
        "f5" => Some(Key::KEY_F5),
        "f6" => Some(Key::KEY_F6),
        "f7" => Some(Key::KEY_F7),
        "f8" => Some(Key::KEY_F8),
        "f9" => Some(Key::KEY_F9),
        "f10" => Some(Key::KEY_F10),
        "f11" => Some(Key::KEY_F11),
        "f12" => Some(Key::KEY_F12),
        "0" => Some(Key::KEY_0),
        "1" => Some(Key::KEY_1),
        "2" => Some(Key::KEY_2),
        "3" => Some(Key::KEY_3),
        "4" => Some(Key::KEY_4),
        "5" => Some(Key::KEY_5),
        "6" => Some(Key::KEY_6),
        "7" => Some(Key::KEY_7),
        "8" => Some(Key::KEY_8),
        "9" => Some(Key::KEY_9),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_super_d() {
        let binding = parse_hotkey("Super+D").unwrap();
        assert_eq!(binding.modifiers, vec![Key::KEY_LEFTMETA]);
        assert_eq!(binding.trigger, Key::KEY_D);
    }

    #[test]
    fn parse_super_shift_c() {
        let binding = parse_hotkey("Super+Shift+C").unwrap();
        assert_eq!(binding.modifiers.len(), 2);
        assert_eq!(binding.trigger, Key::KEY_C);
    }

    #[test]
    fn parse_ctrl_alt_f5() {
        let binding = parse_hotkey("Ctrl+Alt+F5").unwrap();
        assert_eq!(binding.modifiers.len(), 2);
        assert_eq!(binding.trigger, Key::KEY_F5);
    }

    #[test]
    fn parse_case_insensitive() {
        let binding = parse_hotkey("super+shift+d").unwrap();
        assert_eq!(binding.trigger, Key::KEY_D);
    }

    #[test]
    fn parse_no_modifier_fails() {
        assert!(parse_hotkey("D").is_err());
    }

    #[test]
    fn parse_unknown_key_fails() {
        assert!(parse_hotkey("Super+Unknown").is_err());
    }

    #[test]
    fn format_hotkey_display_super_is_windows_key() {
        assert_eq!(
            format_hotkey_display("Super+Shift+W"),
            "Windows key + Shift + W"
        );
    }

    #[test]
    fn format_hotkey_display_ctrl_alt() {
        assert_eq!(format_hotkey_display("Ctrl+Shift+W"), "Ctrl + Shift + W");
    }
}
