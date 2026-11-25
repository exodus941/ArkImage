//! Custom keyboard and mouse bindings, as on Honeyview's Keyboard and Mouse pages:
//! 11 hotkey slots (enable, key, action) and 7 mouse slots (enable, action).

use winit::keyboard::{KeyCode, ModifiersState};

/// Bindable actions: id (stored in the config file) and label (shown in the dialog).
/// The first 30 follow Honeyview's action list; the rest are ArkImage extras.
pub const ACTIONS: &[(&str, &str)] = &[
    ("none", "Do nothing"),
    ("fullscreen", "Full Screen"),
    ("maximize", "Maximize"),
    ("minimize", "Minimize"),
    ("exit", "Exit"),
    ("open", "Open File"),
    ("open_folder", "Open Folder"),
    ("next", "Next Image"),
    ("prev", "Previous Image"),
    ("rnext", "Next Random Image"),
    ("rprev", "Previous Random Image"),
    ("first", "First Image"),
    ("last", "Last Image"),
    ("next10", "Next 10th Image"),
    ("prev10", "Previous 10th Image"),
    ("scroll_up", "Scroll Up"),
    ("scroll_down", "Scroll Down"),
    ("scroll_left", "Scroll Left"),
    ("scroll_right", "Scroll Right"),
    ("zin", "Zoom In"),
    ("zout", "Zoom Out"),
    ("copy1", "Copy to Photo Folder"),
    ("move1", "Move to Photo Folder"),
    ("copy2", "Copy to Photo Folder2"),
    ("nextdir", "Next Compressed File/Folder"),
    ("prevdir", "Previous Compressed File/Folder"),
    ("context", "Context Menu"),
    ("info", "Show File/EXIF information"),
    ("z100", "Zoom 100%"),
    ("mode1", "Fit in Window"),
    ("rotr", "Rotate 90˚ clockwise"),
    ("rotl", "Rotate 90˚ counter-clockwise"),
    ("clip", "Copy to Clipboard"),
    ("delete", "Delete"),
    ("pageselect", "Select Image"),
    ("reveal", "Show in Explorer"),
    ("reveal_all", "Select Open Images from Same Folder in Explorer"),
    ("closedup", "Close Duplicates"),
];

pub fn label(id: &str) -> &'static str {
    ACTIONS.iter().find(|a| a.0 == id).map(|a| a.1).unwrap_or("Do nothing")
}

pub fn index(id: &str) -> usize {
    ACTIONS.iter().position(|a| a.0 == id).unwrap_or(0)
}

pub const KEY_SLOTS: usize = 11;

/// Mouse slots in Honeyview's order. (config key, label, default action, enabled by default)
pub const MOUSE_SLOTS: [(&str, &str, &str, bool); 7] = [
    ("mouse_left", "Left button", "none", false),
    ("mouse_middle", "Middle button", "fullscreen", true),
    ("mouse_right", "Right button", "context", true),
    ("mouse_x", "X button (back)", "prev", true),
    ("mouse_y", "Y button (forward)", "next", true),
    ("wheel_up", "Mousewheel up", "prev", true),
    ("wheel_down", "Mousewheel down", "next", true),
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bind {
    pub on: bool,
    /// Hotkey text such as "Ctrl+Shift+KeyQ"; empty for none. Unused for mouse slots.
    pub key: String,
    pub action: String,
}

impl Bind {
    /// "1|Ctrl+KeyQ|exit" (keys) or "1|next" (mouse).
    pub fn parse(v: &str, with_key: bool) -> Option<Self> {
        let mut it = v.split('|');
        let on = it.next()?.trim() == "1";
        let key = if with_key { it.next()?.trim().to_string() } else { String::new() };
        let action = it.next().unwrap_or("none").trim().to_string();
        Some(Self { on, key, action })
    }

    pub fn format(&self, with_key: bool) -> String {
        if with_key {
            format!("{}|{}|{}", self.on as u8, self.key, self.action)
        } else {
            format!("{}|{}", self.on as u8, self.action)
        }
    }
}

pub fn default_mouse() -> Vec<Bind> {
    MOUSE_SLOTS.iter().map(|m| Bind { on: m.3, key: String::new(), action: m.2.to_string() }).collect()
}

pub fn default_keys() -> Vec<Bind> {
    vec![Bind { on: false, key: String::new(), action: "none".into() }; KEY_SLOTS]
}

/// Canonical text for a key press: modifiers in fixed order, then winit's key code name.
pub fn key_text(mods: ModifiersState, code: KeyCode) -> String {
    let mut s = String::new();
    if mods.control_key() {
        s.push_str("Ctrl+");
    }
    if mods.alt_key() {
        s.push_str("Alt+");
    }
    if mods.shift_key() {
        s.push_str("Shift+");
    }
    if mods.super_key() {
        s.push_str("Cmd+");
    }
    s.push_str(&format!("{code:?}"));
    s
}

/// True for keys that are only modifiers; they cannot be a hotkey on their own.
pub fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ControlLeft | KeyCode::ControlRight | KeyCode::ShiftLeft | KeyCode::ShiftRight | KeyCode::AltLeft | KeyCode::AltRight | KeyCode::SuperLeft | KeyCode::SuperRight
    )
}

/// Readable form: "Ctrl+KeyQ" -> "Ctrl+Q", "Digit5" -> "5".
pub fn display(key: &str) -> String {
    if key.is_empty() {
        return "None".into();
    }
    let (mods, last) = match key.rfind('+') {
        Some(i) if i + 1 < key.len() => (&key[..=i], &key[i + 1..]),
        _ => ("", key),
    };
    let last = last.strip_prefix("Key").or_else(|| last.strip_prefix("Digit")).unwrap_or(last);
    let last = last.strip_prefix("Arrow").unwrap_or(last);
    // macOS keyboards label Alt as Option
    let mods = if cfg!(target_os = "macos") { mods.replace("Alt+", "Option+") } else { mods.to_string() };
    format!("{mods}{last}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_text() {
        let b = Bind { on: true, key: "Ctrl+KeyQ".into(), action: "exit".into() };
        assert_eq!(Bind::parse(&b.format(true), true), Some(b));
        let m = Bind { on: false, key: String::new(), action: "next".into() };
        assert_eq!(Bind::parse(&m.format(false), false), Some(m));
        assert_eq!(key_text(ModifiersState::CONTROL | ModifiersState::SHIFT, KeyCode::KeyQ), "Ctrl+Shift+KeyQ");
        assert_eq!(display("Ctrl+Shift+KeyQ"), "Ctrl+Shift+Q");
        assert_eq!(display("ArrowLeft"), "Left");
        assert_eq!(display(""), "None");
        assert_eq!(label("next10"), "Next 10th Image");
        // 37: the two double-page actions are gone (a saved slot naming one loads as none);
        // 38 with Close Duplicates
        assert_eq!(ACTIONS.len(), 38);
        assert_eq!(label("closedup"), "Close Duplicates");
        assert_eq!(label("next1"), "Do nothing");
    }
}
