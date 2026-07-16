# whisrs GNOME Shell extension

GNOME integration for whisrs:

- Bottom recording overlay (listens to `org.whisrs.Overlay`)
- Text injection into the focused widget (`org.whisrs.Input.TypeText` — clipboard + compositor virtual keyboard)
- Global hotkeys via the extension keybinding API (calls `org.whisrs.Control`)

This path does **not** need `/dev/uinput` or udev rules.

## Install locally

```bash
EXT_DIR=~/.local/share/gnome-shell/extensions/whisrs-overlay@eresende.github
mkdir -p ~/.local/share/gnome-shell/extensions
rm -rf "$EXT_DIR"
cp -r contrib/gnome-shell-extension/whisrs-overlay@eresende.github "$EXT_DIR"
glib-compile-schemas "$EXT_DIR/schemas"
gnome-extensions enable whisrs-overlay@eresende.github
```

On GNOME Wayland, log out and back in (or restart the session) so Shell reloads `extension.js`. Then:

```toml
# ~/.config/whisrs/config.toml
[general]
overlay = true

[input]
backend = "gnome-shell"   # or "auto" on GNOME

[hotkeys]
toggle = "Super+Shift+W"
cancel = "Super+Shift+D"
```

Run the daemon from a build tree (no systemd / no system install):

```bash
cargo build
./target/debug/whisrsd
```

## Updating

Clear the Shell extension cache and restart the session:

```bash
rm -rf ~/.cache/gnome-shell/extensions/whisrs-overlay@eresende.github
# copy files again, glib-compile-schemas, then log out/in
```

`stylesheet.css`-only changes sometimes reload with disable/enable; `extension.js` needs a session restart on Wayland.
