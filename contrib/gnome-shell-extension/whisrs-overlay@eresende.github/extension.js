import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const OVERLAY_IFACE = 'org.whisrs.Overlay';
const OVERLAY_PATH = '/org/whisrs/Overlay';
const STATE_SIGNAL = 'StateChanged';
const LEVEL_SIGNAL = 'LevelChanged';
const THEME_SIGNAL = 'ThemeChanged';
const TEXT_SIGNAL = 'TextChanged';

const INPUT_IFACE_XML = `
<node>
  <interface name="org.whisrs.Input">
    <method name="Ping">
      <arg type="s" direction="out" name="status"/>
    </method>
    <method name="TypeText">
      <arg type="s" direction="in" name="text"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="Backspace">
      <arg type="u" direction="in" name="count"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="SelectLeft">
      <arg type="u" direction="in" name="count"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="SendShortcut">
      <arg type="s" direction="in" name="shortcut"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="GetFocusedWindow">
      <arg type="s" direction="out" name="id"/>
      <arg type="s" direction="out" name="wm_class"/>
    </method>
    <method name="FocusWindow">
      <arg type="s" direction="in" name="id"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
  </interface>
</node>`;

const CONTROL_DEST = 'org.whisrs.Control';
const CONTROL_PATH = '/org/whisrs/Control';
const CONTROL_IFACE = 'org.whisrs.Control';

const TERMINAL_WM_CLASSES = [
    'gnome-terminal', 'gnome-terminal-server', 'kgx', 'tilix', 'kitty',
    'alacritty', 'terminator', 'xterm', 'konsole', 'foot', 'wezterm',
    'st', 'sakura', 'xfce4-terminal', 'mate-terminal', 'lxterminal',
    'guake', 'tilda', 'cool-retro-term', 'ptyxis',
];

const KEY_LOOKUP = {
    Control_L: Clutter.KEY_Control_L,
    Shift_L: Clutter.KEY_Shift_L,
    Alt_L: Clutter.KEY_Alt_L,
    Super_L: Clutter.KEY_Super_L,
    v: Clutter.KEY_v,
    c: Clutter.KEY_c,
    a: Clutter.KEY_a,
    k: Clutter.KEY_k,
    x: Clutter.KEY_x,
    Insert: Clutter.KEY_Insert,
    BackSpace: Clutter.KEY_BackSpace,
    Left: Clutter.KEY_Left,
};

const OVERLAY_WIDTH = 100;
const OVERLAY_HEIGHT = 40;
const TEXT_PANEL_WIDTH = 380;
const TEXT_LINE_HEIGHT = 17;
const TEXT_PADDING = 12;
const TEXT_GAP = 8;
const TEXT_MAX_LINES = 6;
const BOTTOM_MARGIN = 16;
const BAR_COUNT = 7;
const BAR_W = 3;
const BAR_GAP = 2;
const BAR_BASELINE = 6;
const BAR_VPAD = 6;

const SPAWN_IN_MS = 220;
const SPAWN_OUT_MS = 140;
const SPAWN_PILL_MIN_H = 4;
const BARS_GRACE_MS = 80;
const BARS_FADE_MS = 80;

const KNOWN_THEMES = ['ember', 'carbon', 'cyan'];
const HOTKEY_ACTIONS = ['toggle', 'cancel', 'command', 'speak'];

/** Bounce: . → .. → ... → .. → . … (preview-only, never pasted). */
const SILENCE_DOT_FRAMES = ['.', '..', '...', '..'];
const SILENCE_DOT_MS = 380;
/** Smoothed bar level below this counts as silence for the dots. */
const SILENCE_LEVEL_THRESHOLD = 0.045;

export default class WhisrsOverlayExtension extends Extension {
    enable() {
        this._theme = 'carbon';
        this._transcript = '';
        this._settings = this.getSettings();
        this._savedClipboard = null;
        this._restoreClipboardId = 0;
        this._keybindingNames = [];

        this._textBox = new St.Label({
            style_class: 'whisrs-overlay-text',
            text: '',
            x_align: Clutter.ActorAlign.FILL,
            y_align: Clutter.ActorAlign.END,
        });
        this._textBox.clutter_text.set({
            line_wrap: true,
        });
        this._textBox.hide();

        this._actor = new St.Widget({
            style_class: 'whisrs-overlay whisrs-overlay-hidden whisrs-theme-carbon',
            layout_manager: new Clutter.FixedLayout(),
            reactive: false,
            visible: false,
        });

        this._barsBox = new St.BoxLayout({
            style_class: 'whisrs-overlay-bars',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._bars = [];
        for (let i = 0; i < BAR_COUNT; i++) {
            const bar = new St.Widget({
                style_class: 'whisrs-overlay-bar',
                y_align: Clutter.ActorAlign.CENTER,
                y_expand: false,
            });
            this._bars.push(bar);
            this._barsBox.add_child(bar);
        }

        this._actor.add_child(this._textBox);
        this._actor.add_child(this._barsBox);
        Main.uiGroup.add_child(this._actor);

        this._monitorsChangedId = Main.layoutManager.connect(
            'monitors-changed',
            () => this._position()
        );

        this._signalId = Gio.DBus.session.signal_subscribe(
            null, OVERLAY_IFACE, STATE_SIGNAL, OVERLAY_PATH, null,
            Gio.DBusSignalFlags.NONE,
            (_c, _s, _p, _i, _sig, parameters) => {
                const [state] = parameters.deep_unpack();
                this._setState(state);
            }
        );
        this._levelSignalId = Gio.DBus.session.signal_subscribe(
            null, OVERLAY_IFACE, LEVEL_SIGNAL, OVERLAY_PATH, null,
            Gio.DBusSignalFlags.NONE,
            (_c, _s, _p, _i, _sig, parameters) => {
                const [level] = parameters.deep_unpack();
                this._setLevel(level);
            }
        );
        this._themeSignalId = Gio.DBus.session.signal_subscribe(
            null, OVERLAY_IFACE, THEME_SIGNAL, OVERLAY_PATH, null,
            Gio.DBusSignalFlags.NONE,
            (_c, _s, _p, _i, _sig, parameters) => {
                const [theme] = parameters.deep_unpack();
                this._setTheme(theme);
            }
        );
        this._textSignalId = Gio.DBus.session.signal_subscribe(
            null, OVERLAY_IFACE, TEXT_SIGNAL, OVERLAY_PATH, null,
            Gio.DBusSignalFlags.NONE,
            (_c, _s, _p, _i, _sig, parameters) => {
                const [text] = parameters.deep_unpack();
                this._setText(String(text ?? ''));
            }
        );

        this._exportInputBus();
        this._watchControlBus();
        this._syncHotkeysFromDaemon();
        this._registerKeybindings();
        this._createPanelIndicator();

        this._state = 'idle';
        this._level = 0;
        this._targetLevel = 0;
        this._levelVelocity = 0;
        this._lastUpdateMs = 0;
        this._frame = 0;
        this._silenceDotId = 0;
        this._silenceDotFrame = 0;
        this._silenceDotsActive = false;

        this._position();
        this._updatePanelIndicator('idle');
    }

    disable() {
        this._stopSilenceDots();
        this._stopAnimation();
        this._clearKeybindings();
        this._destroyPanelIndicator();

        if (this._restoreClipboardId) {
            GLib.source_remove(this._restoreClipboardId);
            this._restoreClipboardId = 0;
        }
        if (this._hotkeyRetryId) {
            GLib.source_remove(this._hotkeyRetryId);
            this._hotkeyRetryId = 0;
        }
        if (this._nameOwnerId) {
            Gio.DBus.session.signal_unsubscribe(this._nameOwnerId);
            this._nameOwnerId = 0;
        }

        for (const id of [
            this._signalId,
            this._levelSignalId,
            this._themeSignalId,
            this._textSignalId,
        ]) {
            if (id) Gio.DBus.session.signal_unsubscribe(id);
        }
        this._signalId = 0;
        this._levelSignalId = 0;
        this._themeSignalId = 0;
        this._textSignalId = 0;

        if (this._monitorsChangedId) {
            Main.layoutManager.disconnect(this._monitorsChangedId);
            this._monitorsChangedId = 0;
        }

        if (this._inputBus) {
            this._inputBus.unexport();
            this._inputBus = null;
        }
        if (this._inputBusNameId) {
            Gio.bus_unown_name(this._inputBusNameId);
            this._inputBusNameId = 0;
        }

        this._actor?.destroy();
        this._actor = null;
        this._bars = [];
        this._barsBox = null;
        this._textBox = null;
        this._settings = null;
    }

    // ── D-Bus Input (daemon → extension) ─────────────────────────────────

    _exportInputBus() {
        this._inputBus = Gio.DBusExportedObject.wrapJSObject(INPUT_IFACE_XML, this);
        this._inputBus.export(Gio.DBus.session, '/org/whisrs/Input');
        this._inputBusNameId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            'org.whisrs.Input',
            Gio.BusNameOwnerFlags.NONE,
            null,
            null,
            null
        );
    }

    Ping() {
        return 'ok';
    }

    TypeText(text) {
        try {
            const value = String(text ?? '');
            if (!value)
                return true;
            this._pasteText(value);
            return true;
        } catch (e) {
            console.error(`whisrs TypeText failed: ${e}`);
            return false;
        }
    }

    Backspace(count) {
        try {
            const n = Math.max(0, Number(count) || 0);
            for (let i = 0; i < n; i++)
                this._dispatchKeystroke({modifiers: [], key: 'BackSpace'});
            return true;
        } catch (e) {
            console.error(`whisrs Backspace failed: ${e}`);
            return false;
        }
    }

    SelectLeft(count) {
        try {
            const n = Math.max(0, Number(count) || 0);
            for (let i = 0; i < n; i++)
                this._dispatchKeystroke({modifiers: ['Shift_L'], key: 'Left'});
            return true;
        } catch (e) {
            console.error(`whisrs SelectLeft failed: ${e}`);
            return false;
        }
    }

    SendShortcut(shortcut) {
        try {
            const recipe = this._parseShortcut(String(shortcut ?? ''));
            if (!recipe)
                return false;
            this._dispatchKeystroke(recipe);
            return true;
        } catch (e) {
            console.error(`whisrs SendShortcut failed: ${e}`);
            return false;
        }
    }

    GetFocusedWindow() {
        try {
            const win = global.display.get_focus_window();
            if (!win)
                return ['', ''];
            return [String(win.get_id()), win.get_wm_class() || ''];
        } catch (e) {
            console.error(`whisrs GetFocusedWindow failed: ${e}`);
            return ['', ''];
        }
    }

    FocusWindow(id) {
        try {
            const target = Number(id);
            if (!Number.isFinite(target))
                return false;
            const actors = global.get_window_actors();
            for (const actor of actors) {
                const win = actor.get_meta_window();
                if (win && win.get_id() === target) {
                    win.activate(global.get_current_time());
                    return true;
                }
            }
            return false;
        } catch (e) {
            console.error(`whisrs FocusWindow failed: ${e}`);
            return false;
        }
    }

    // ── Panel indicator (replaces AppIndicator on GNOME) ─────────────────

    _createPanelIndicator() {
        try {
            if (Main.panel.statusArea.whisrs)
                Main.panel.statusArea.whisrs.destroy();

            this._hotkeyLabels = this._hotkeyLabels || {
                toggle: '',
                cancel: '',
                command: '',
                speak: '',
            };
            this._panelState = this._panelState || 'idle';

            this._indicator = new PanelMenu.Button(0.0, 'whisrs', false);
            this._panelIcon = new St.Icon({
                icon_name: 'audio-input-microphone-symbolic',
                style_class: 'system-status-icon whisrs-panel-icon whisrs-panel-idle',
            });
            this._indicator.add_child(this._panelIcon);

            // Left-click toggles; right-click opens the menu.
            this._indicator.connect('button-press-event', (_actor, event) => {
                if (event.get_button() === 1) {
                    this._callControl('Toggle');
                    return Clutter.EVENT_STOP;
                }
                return Clutter.EVENT_PROPAGATE;
            });

            this._createPanelTooltip();
            this._indicator.connect('enter-event', () => this._showPanelTooltip());
            this._indicator.connect('leave-event', () => this._hidePanelTooltip());

            this._rebuildPanelMenu();
            this._updatePanelTooltip();
            Main.panel.addToStatusArea('whisrs', this._indicator, 1, 'right');
            console.log('whisrs: panel indicator added');
        } catch (e) {
            console.error(`whisrs: failed to create panel indicator: ${e}`);
            this._destroyPanelTooltip();
            this._indicator = null;
            this._panelIcon = null;
        }
    }

    _createPanelTooltip() {
        this._destroyPanelTooltip();
        if (!Main.layoutManager?.uiGroup)
            return;

        this._panelTooltip = new St.BoxLayout({
            style_class: 'whisrs-tooltip',
            style: 'background-color: rgba(0, 0, 0, 0.8); padding: 6px 10px; border-radius: 6px;',
            visible: false,
            opacity: 0,
        });
        this._panelTooltipLabel = new St.Label({
            text: '',
            style: 'color: #ffffff;',
        });
        this._panelTooltip.add_child(this._panelTooltipLabel);
        Main.layoutManager.uiGroup.add_child(this._panelTooltip);
    }

    _destroyPanelTooltip() {
        try {
            this._panelTooltip?.destroy();
        } catch (_e) {
            // already gone
        }
        this._panelTooltip = null;
        this._panelTooltipLabel = null;
    }

    _panelTooltipText() {
        const toggle = (this._hotkeyLabels?.toggle || '').trim();
        const state = String(this._panelState || 'idle').toLowerCase();

        if (state === 'recording') {
            return toggle
                ? `whisrs — recording\nPress ${toggle} to stop`
                : 'whisrs — recording\nClick to stop';
        }
        if (state === 'transcribing' || state === 'synthesizing')
            return 'whisrs — transcribing…';
        if (state === 'speaking')
            return 'whisrs — speaking…';

        return toggle
            ? `whisrs\nPress ${toggle} to start`
            : 'whisrs\nClick to start';
    }

    _updatePanelTooltip() {
        if (!this._panelTooltipLabel)
            return;
        this._panelTooltipLabel.text = this._panelTooltipText();
    }

    _showPanelTooltip() {
        if (!this._panelTooltip || !this._indicator)
            return;
        this._updatePanelTooltip();
        const [x, y] = this._indicator.get_transformed_position();
        const [, height] = this._indicator.get_transformed_size();
        this._panelTooltip.set_position(Math.round(x), Math.round(y + height + 5));
        this._panelTooltip.opacity = 255;
        this._panelTooltip.visible = true;
    }

    _hidePanelTooltip() {
        if (!this._panelTooltip)
            return;
        this._panelTooltip.opacity = 0;
        this._panelTooltip.visible = false;
    }

    _menuLabel(action, shortcut) {
        const sc = shortcut && String(shortcut).trim();
        return sc ? `${action} (${sc})` : action;
    }

    _rebuildPanelMenu() {
        if (!this._indicator?.menu)
            return;

        this._indicator.menu.removeAll();

        const header = new PopupMenu.PopupMenuItem('whisrs', {
            reactive: false,
            can_focus: false,
        });
        header.setSensitive(false);
        this._indicator.menu.addMenuItem(header);
        this._indicator.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const labels = this._hotkeyLabels || {};

        const toggleItem = new PopupMenu.PopupMenuItem(
            this._menuLabel('Start / stop recording', labels.toggle)
        );
        toggleItem.connect('activate', () => this._callControl('Toggle'));
        this._indicator.menu.addMenuItem(toggleItem);

        // Cancel = discard in-progress recording (no transcription). Only show
        // when a cancel hotkey is configured so the menu stays meaningful.
        if (labels.cancel && String(labels.cancel).trim()) {
            const cancelItem = new PopupMenu.PopupMenuItem(
                this._menuLabel('Discard recording', labels.cancel)
            );
            cancelItem.connect('activate', () => this._callControl('Cancel'));
            this._indicator.menu.addMenuItem(cancelItem);
        }

        if (labels.command && String(labels.command).trim()) {
            const commandItem = new PopupMenu.PopupMenuItem(
                this._menuLabel('Command mode', labels.command)
            );
            commandItem.connect('activate', () => this._callControl('Command'));
            this._indicator.menu.addMenuItem(commandItem);
        }

        if (labels.speak && String(labels.speak).trim()) {
            const speakItem = new PopupMenu.PopupMenuItem(
                this._menuLabel('Read selection aloud', labels.speak)
            );
            speakItem.connect('activate', () => this._callControl('Speak'));
            this._indicator.menu.addMenuItem(speakItem);
        }
    }

    _destroyPanelIndicator() {
        this._hidePanelTooltip();
        this._destroyPanelTooltip();
        try {
            this._indicator?.destroy();
        } catch (_e) {
            // already gone
        }
        this._indicator = null;
        this._panelIcon = null;
    }

    _updatePanelIndicator(state) {
        const normalized = String(state).toLowerCase();
        this._panelState = normalized;
        this._updatePanelTooltip();

        if (!this._panelIcon)
            return;
        for (const cls of [
            'whisrs-panel-idle',
            'whisrs-panel-recording',
            'whisrs-panel-transcribing',
            'whisrs-panel-speaking',
        ])
            this._panelIcon.remove_style_class_name(cls);

        if (normalized === 'recording')
            this._panelIcon.add_style_class_name('whisrs-panel-recording');
        else if (normalized === 'transcribing' || normalized === 'synthesizing')
            this._panelIcon.add_style_class_name('whisrs-panel-transcribing');
        else if (normalized === 'speaking')
            this._panelIcon.add_style_class_name('whisrs-panel-speaking');
        else
            this._panelIcon.add_style_class_name('whisrs-panel-idle');
    }

    _pasteText(text) {
        const clipboard = St.Clipboard.get_default();
        clipboard.set_text(St.ClipboardType.CLIPBOARD, text);
        this._dispatchKeystroke(this._pasteRecipe());
    }

    _pasteRecipe() {
        const focusWin = global.display.get_focus_window();
        const wmClass = focusWin?.get_wm_class()?.toLowerCase() ?? '';
        const isTerminal = TERMINAL_WM_CLASSES.some(c => wmClass.includes(c));
        return isTerminal
            ? {modifiers: ['Control_L', 'Shift_L'], key: 'v'}
            : {modifiers: ['Control_L'], key: 'v'};
    }

    _parseShortcut(shortcut) {
        const parts = shortcut.toLowerCase().split('+').map(p => p.trim()).filter(Boolean);
        if (parts.length === 0)
            return null;
        const keyName = parts.pop();
        const modifiers = [];
        for (const part of parts) {
            if (part === 'ctrl' || part === 'control')
                modifiers.push('Control_L');
            else if (part === 'shift')
                modifiers.push('Shift_L');
            else if (part === 'alt')
                modifiers.push('Alt_L');
            else if (part === 'super' || part === 'meta' || part === 'mod4')
                modifiers.push('Super_L');
            else
                return null;
        }
        const keyMap = {
            v: 'v', c: 'c', a: 'a', k: 'k', x: 'x',
            insert: 'Insert',
            backspace: 'BackSpace',
            left: 'Left',
        };
        const key = keyMap[keyName];
        if (!key || !(key in KEY_LOOKUP))
            return null;
        return {modifiers, key};
    }

    _dispatchKeystroke(recipe) {
        const seat = Clutter.get_default_backend().get_default_seat();
        const vk = seat.create_virtual_device(
            Clutter.InputDeviceType.KEYBOARD_DEVICE
        );
        const time = Clutter.CURRENT_TIME;
        for (const mod of recipe.modifiers)
            vk.notify_keyval(time, KEY_LOOKUP[mod], Clutter.KeyState.PRESSED);
        vk.notify_keyval(time, KEY_LOOKUP[recipe.key], Clutter.KeyState.PRESSED);
        vk.notify_keyval(time, KEY_LOOKUP[recipe.key], Clutter.KeyState.RELEASED);
        for (const mod of [...recipe.modifiers].reverse())
            vk.notify_keyval(time, KEY_LOOKUP[mod], Clutter.KeyState.RELEASED);
    }

    // ── Hotkeys (extension → daemon Control) ─────────────────────────────

    _watchControlBus() {
        this._nameOwnerId = Gio.DBus.session.signal_subscribe(
            'org.freedesktop.DBus',
            'org.freedesktop.DBus',
            'NameOwnerChanged',
            '/org/freedesktop/DBus',
            CONTROL_DEST,
            Gio.DBusSignalFlags.NONE,
            (_c, _s, _p, _i, _sig, parameters) => {
                const [, , newOwner] = parameters.deep_unpack();
                if (newOwner)
                    this._syncHotkeysFromDaemon();
            }
        );
    }

    _syncHotkeysFromDaemon() {
        Gio.DBus.session.call(
            CONTROL_DEST,
            CONTROL_PATH,
            CONTROL_IFACE,
            'GetHotkeys',
            null,
            new GLib.VariantType('(ssss)'),
            Gio.DBusCallFlags.NONE,
            2000,
            null,
            (conn, result) => {
                try {
                    const reply = conn.call_finish(result);
                    const [toggle, cancel, command, speak] = reply.deep_unpack();
                    if (this._hotkeyRetryId) {
                        GLib.source_remove(this._hotkeyRetryId);
                        this._hotkeyRetryId = 0;
                    }
                    this._hotkeyLabels = {
                        toggle: toggle || '',
                        cancel: cancel || '',
                        command: command || '',
                        speak: speak || '',
                    };
                    this._applyHotkeySetting('toggle', toggle);
                    this._applyHotkeySetting('cancel', cancel);
                    this._applyHotkeySetting('command', command);
                    this._applyHotkeySetting('speak', speak);
                    this._registerKeybindings();
                    this._rebuildPanelMenu();
                    this._updatePanelTooltip();
                } catch (_e) {
                    // Daemon not up yet — retry a few times.
                    if (!this._hotkeyRetryId) {
                        let tries = 0;
                        this._hotkeyRetryId = GLib.timeout_add_seconds(
                            GLib.PRIORITY_DEFAULT, 2, () => {
                                tries += 1;
                                this._syncHotkeysFromDaemon();
                                if (tries >= 15) {
                                    this._hotkeyRetryId = 0;
                                    return GLib.SOURCE_REMOVE;
                                }
                                return GLib.SOURCE_CONTINUE;
                            }
                        );
                    }
                }
            }
        );
    }

    _applyHotkeySetting(name, whisrsBinding) {
        if (!this._settings)
            return;
        const accel = this._toGnomeAccel(whisrsBinding);
        this._settings.set_strv(name, accel);
    }

    _toGnomeAccel(binding) {
        if (!binding || !String(binding).trim())
            return [];
        const parts = String(binding).split('+').map(p => p.trim()).filter(Boolean);
        if (parts.length === 0)
            return [];
        const key = parts.pop();
        let accel = '';
        for (const part of parts) {
            const lower = part.toLowerCase();
            if (lower === 'super' || lower === 'mod4' || lower === 'meta')
                accel += '<Super>';
            else if (lower === 'shift')
                accel += '<Shift>';
            else if (lower === 'ctrl' || lower === 'control')
                accel += '<Control>';
            else if (lower === 'alt' || lower === 'mod1')
                accel += '<Alt>';
            else
                accel += `<${part}>`;
        }
        const keyOut = key.length === 1 ? key.toLowerCase() : key;
        return [`${accel}${keyOut}`];
    }

    _registerKeybindings() {
        this._clearKeybindings();
        if (!this._settings)
            return;
        for (const name of HOTKEY_ACTIONS) {
            const shortcuts = this._settings.get_strv(name);
            if (!shortcuts || shortcuts.length === 0)
                continue;
            Main.wm.addKeybinding(
                name,
                this._settings,
                Meta.KeyBindingFlags.NONE,
                Shell.ActionMode.ALL,
                () => this._callControl(this._controlMethod(name))
            );
            this._keybindingNames.push(name);
        }
    }

    _clearKeybindings() {
        for (const name of this._keybindingNames)
            Main.wm.removeKeybinding(name);
        this._keybindingNames = [];
    }

    _controlMethod(name) {
        switch (name) {
        case 'toggle': return 'Toggle';
        case 'cancel': return 'Cancel';
        case 'command': return 'Command';
        case 'speak': return 'Speak';
        default: return 'Toggle';
        }
    }

    _callControl(method) {
        Gio.DBus.session.call(
            CONTROL_DEST,
            CONTROL_PATH,
            CONTROL_IFACE,
            method,
            null,
            null,
            Gio.DBusCallFlags.NONE,
            -1,
            null,
            null
        );
    }

    // ── Overlay UI (unchanged behavior) ──────────────────────────────────

    _setTheme(theme) {
        if (!this._actor) return;
        const next = KNOWN_THEMES.includes(String(theme)) ? String(theme) : 'carbon';
        if (next === this._theme) return;
        this._actor.remove_style_class_name(`whisrs-theme-${this._theme}`);
        this._actor.add_style_class_name(`whisrs-theme-${next}`);
        this._theme = next;
    }

    _wrapTranscript(text) {
        const words = String(text).trim().split(/\s+/).filter(Boolean);
        if (words.length === 0) return '';

        const lines = [];
        let current = '';
        const maxChars = 52;
        for (const word of words) {
            const candidate = current ? `${current} ${word}` : word;
            if (candidate.length <= maxChars || !current) {
                current = candidate;
            } else {
                lines.push(current);
                current = word;
            }
        }
        if (current) lines.push(current);
        return lines.slice(-TEXT_MAX_LINES).join('\n');
    }

    _setText(text) {
        this._transcript = String(text ?? '');
        this._refreshTextDisplay();
    }

    _displayTranscript() {
        const base = String(this._transcript ?? '').trimEnd();
        if (this._state === 'recording' && this._silenceDotsActive) {
            const dots = SILENCE_DOT_FRAMES[
                this._silenceDotFrame % SILENCE_DOT_FRAMES.length
            ];
            return base ? `${base} ${dots}` : dots;
        }
        return base;
    }

    _refreshTextDisplay() {
        if (!this._textBox)
            return;

        const wrapped = this._wrapTranscript(this._displayTranscript());
        this._textBox.text = wrapped;
        if (wrapped) {
            this._textBox.show();
        } else {
            this._textBox.hide();
        }
        this._position();
    }

    _textPanelHeight() {
        const display = this._displayTranscript();
        if (!String(display).trim())
            return 0;
        const lines = this._wrapTranscript(display).split('\n').filter(Boolean);
        if (lines.length === 0)
            return 0;
        return TEXT_PADDING * 2 + lines.length * TEXT_LINE_HEIGHT;
    }

    _startSilenceDots() {
        if (this._silenceDotId)
            return;
        this._silenceDotFrame = 0;
        this._silenceDotsActive = false;
        this._silenceDotId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT,
            SILENCE_DOT_MS,
            () => {
                if (this._state !== 'recording') {
                    this._silenceDotId = 0;
                    this._silenceDotsActive = false;
                    return GLib.SOURCE_REMOVE;
                }
                const silent = (this._level ?? 0) < SILENCE_LEVEL_THRESHOLD;
                if (silent) {
                    if (!this._silenceDotsActive) {
                        this._silenceDotsActive = true;
                        this._silenceDotFrame = 0;
                    } else {
                        this._silenceDotFrame =
                            (this._silenceDotFrame + 1) % SILENCE_DOT_FRAMES.length;
                    }
                    this._refreshTextDisplay();
                } else if (this._silenceDotsActive) {
                    this._silenceDotsActive = false;
                    this._refreshTextDisplay();
                }
                return GLib.SOURCE_CONTINUE;
            }
        );
    }

    _stopSilenceDots() {
        if (this._silenceDotId) {
            GLib.source_remove(this._silenceDotId);
            this._silenceDotId = 0;
        }
        if (this._silenceDotsActive) {
            this._silenceDotsActive = false;
            this._refreshTextDisplay();
        }
    }

    _setState(state) {
        const normalized = String(state).toLowerCase();
        this._updatePanelIndicator(normalized);

        if (!this._actor) return;

        const wasIdle = this._state === 'idle';

        this._actor.remove_style_class_name('whisrs-overlay-recording');
        this._actor.remove_style_class_name('whisrs-overlay-transcribing');
        this._actor.remove_style_class_name('whisrs-overlay-hidden');

        if (normalized === 'recording') {
            this._state = 'recording';
            this._actor.add_style_class_name('whisrs-overlay-recording');
            this._actor.visible = true;
            if (wasIdle) this._spawnIn();
            this._startAnimation();
            this._startSilenceDots();
        } else if (normalized === 'transcribing') {
            this._state = 'transcribing';
            this._stopSilenceDots();
            this._actor.add_style_class_name('whisrs-overlay-transcribing');
            this._actor.visible = true;
            if (wasIdle) this._spawnIn();
            this._startAnimation();
            this._refreshTextDisplay();
        } else {
            this._state = 'idle';
            this._stopSilenceDots();
            this._actor.add_style_class_name('whisrs-overlay-hidden');
            if (!wasIdle) this._spawnOut();
            this._stopAnimation();
            this._setText('');
        }
    }

    _spawnIn() {
        if (!this._actor) return;

        this._actor.set_easing_duration(0);
        this._actor.set_pivot_point(0.5, 1.0);
        this._actor.set_scale(1.0, SPAWN_PILL_MIN_H / OVERLAY_HEIGHT);
        this._actor.opacity = 0;
        if (this._barsBox) this._barsBox.opacity = 0;

        this._actor.set_easing_mode(Clutter.AnimationMode.EASE_OUT_BACK);
        this._actor.set_easing_duration(SPAWN_IN_MS);
        this._actor.set_scale(1.0, 1.0);

        this._actor.set_easing_mode(Clutter.AnimationMode.EASE_OUT_QUAD);
        this._actor.set_easing_duration(Math.round(SPAWN_IN_MS * 0.64));
        this._actor.opacity = 255;

        this._barsGraceUntil = Date.now() + BARS_GRACE_MS + BARS_FADE_MS;
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, BARS_GRACE_MS, () => {
            if (this._barsBox && this._state !== 'idle') {
                this._barsBox.set_easing_mode(Clutter.AnimationMode.EASE_OUT_QUAD);
                this._barsBox.set_easing_duration(BARS_FADE_MS);
                this._barsBox.opacity = 255;
            }
            return GLib.SOURCE_REMOVE;
        });
    }

    _spawnOut() {
        if (!this._actor) return;

        if (this._barsBox) {
            this._barsBox.set_easing_mode(Clutter.AnimationMode.EASE_IN_QUAD);
            this._barsBox.set_easing_duration(Math.round(SPAWN_OUT_MS * 0.7));
            this._barsBox.opacity = 0;
        }

        this._actor.set_easing_mode(Clutter.AnimationMode.EASE_IN_CUBIC);
        this._actor.set_easing_duration(SPAWN_OUT_MS);
        this._actor.set_scale(1.0, SPAWN_PILL_MIN_H / OVERLAY_HEIGHT);
        this._actor.opacity = 0;

        GLib.timeout_add(GLib.PRIORITY_DEFAULT, SPAWN_OUT_MS + 20, () => {
            if (this._state === 'idle' && this._actor) this._actor.visible = false;
            return GLib.SOURCE_REMOVE;
        });
    }

    _position() {
        if (!this._actor) return;

        const monitor = Main.layoutManager.primaryMonitor;
        const textH = this._textPanelHeight();
        const totalW = Math.max(OVERLAY_WIDTH, textH > 0 ? TEXT_PANEL_WIDTH : OVERLAY_WIDTH);
        const totalH = OVERLAY_HEIGHT + (textH > 0 ? textH + TEXT_GAP : 0);
        const x = Math.floor(monitor.x + (monitor.width - totalW) / 2);
        const y = Math.floor(monitor.y + monitor.height - totalH - BOTTOM_MARGIN);

        this._actor.set_position(Math.max(monitor.x, x), Math.max(monitor.y, y));
        this._actor.set_size(totalW, totalH);

        const pillY = textH > 0 ? textH + TEXT_GAP : 0;
        const cy = Math.floor(pillY + OVERLAY_HEIGHT / 2);
        const barBlock = BAR_COUNT * BAR_W + (BAR_COUNT - 1) * BAR_GAP;
        const barsX = Math.floor((totalW - barBlock) / 2);
        const maxH = OVERLAY_HEIGHT - BAR_VPAD * 2;

        if (this._textBox) {
            this._textBox.set_position(0, 0);
            this._textBox.set_size(totalW, textH || 0);
        }

        if (this._barsBox) {
            this._barsBox.set_position(barsX, cy - Math.floor(maxH / 2));
            this._barsBox.set_size(barBlock, maxH);
        }
    }

    _startAnimation() {
        if (this._animationId) return;

        const STIFFNESS = 360;
        const DAMPING = 32;
        this._lastUpdateMs = GLib.get_monotonic_time() / 1000;
        this._animationId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 16, () => {
            this._frame++;
            const nowMs = GLib.get_monotonic_time() / 1000;
            const dt = Math.min(0.1, Math.max(0, (nowMs - this._lastUpdateMs) / 1000));
            this._lastUpdateMs = nowMs;
            const target = this._targetLevel ?? 0;
            if (dt > 0) {
                const force = (target - this._level) * STIFFNESS;
                const drag = this._levelVelocity * DAMPING;
                this._levelVelocity += (force - drag) * dt;
                this._level = Math.max(0, Math.min(1, this._level + this._levelVelocity * dt));
            }
            this._updateBars();
            return GLib.SOURCE_CONTINUE;
        });
        this._updateBars();
    }

    _stopAnimation() {
        if (this._animationId) {
            GLib.Source.remove(this._animationId);
            this._animationId = 0;
        }
        this._levelVelocity = 0;
        this._level = 0;
    }

    _taper(i) {
        if (BAR_COUNT <= 1) return 1;
        const center = (BAR_COUNT - 1) / 2;
        const d = (i - center) / center;
        const envelope = Math.exp(-d * d);
        const wave = 0.75 + 0.25 * Math.cos(Math.PI * (i - center));
        return envelope * wave;
    }

    _updateBars() {
        if (!this._bars || this._bars.length === 0) return;

        const maxH = OVERLAY_HEIGHT - 10;

        if (this._state === 'recording') {
            const grace = this._barsGraceUntil && Date.now() < this._barsGraceUntil;
            const raw = grace ? 0 : (Number.isFinite(this._level) ? this._level : 0);
            const level = Math.max(0, Math.min(1, raw));
            for (let i = 0; i < this._bars.length; i++) {
                const taper = this._taper(i);
                const effective = Math.min(1, Math.max(0, level * taper));
                const h = Math.max(BAR_BASELINE, Math.round(BAR_BASELINE + effective * (maxH - BAR_BASELINE)));
                this._bars[i].set_height(h);
                this._bars[i].opacity = 255;
            }
        } else if (this._state === 'transcribing') {
            const cycle = BAR_COUNT * 2 - 2;
            const pos = Math.floor(this._frame / 3) % Math.max(1, cycle);
            const active = pos < BAR_COUNT ? pos : cycle - pos;
            for (let i = 0; i < this._bars.length; i++) {
                const taper = this._taper(i);
                const dist = Math.abs(i - active);
                const intensity = Math.max(0.15, Math.exp(-(dist * dist) / 4));
                const dynamic = intensity * taper;
                const h = Math.max(BAR_BASELINE, Math.round(BAR_BASELINE + dynamic * (maxH - BAR_BASELINE) * 0.85));
                this._bars[i].set_height(h);
                this._bars[i].opacity = Math.round(255 * (0.3 + 0.7 * intensity));
            }
        }
    }

    _setLevel(level) {
        const numeric = Number(level);
        if (!Number.isFinite(numeric)) return;
        this._targetLevel = Math.max(0, Math.min(1, numeric));
    }
}
