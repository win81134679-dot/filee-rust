//! Keys, modifiers and mouse buttons as settings.json and the gesture detector name them.
//! Key names follow upstream's (SharpHook `KeyCode` without the `Vc` prefix): "Space", "F2", "Q", "BackQuote".

use serde_json::Value;

pub const CTRL: u8 = 1;
pub const ALT: u8 = 2;
pub const SHIFT: u8 = 4;
pub const META: u8 = 8;

/// Keys the donut itself uses; they never flip it (`DonutSettings.ReservedFlipKeys`).
pub const RESERVED_FLIP_KEYS: [&str; 8] = ["Escape", "Enter", "Space", "Left", "Right", "Up", "Down", "Backspace"];
pub const DEFAULT_FLIP_KEY: &str = "Tab";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Button {
    #[default]
    None,
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl Button {
    /// Bit in the held-buttons mask the hook keeps.
    pub fn bit(self) -> u8 {
        match self {
            Button::None => 0,
            Button::Left => 1,
            Button::Right => 2,
            Button::Middle => 4,
            Button::X1 => 8,
            Button::X2 => 16,
        }
    }

    /// The held button out of a mask; Left > Right > Middle > X1 > X2 like upstream's `HeldButton`.
    pub fn held(mask: u8) -> Button {
        [Button::Left, Button::Right, Button::Middle, Button::X1, Button::X2]
            .into_iter()
            .find(|b| mask & b.bit() != 0)
            .unwrap_or(Button::None)
    }

    pub fn parse(value: &Value) -> Button {
        match value {
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "left" => Button::Left,
                "right" => Button::Right,
                "middle" => Button::Middle,
                "x1" => Button::X1,
                "x2" => Button::X2,
                _ => Button::None,
            },
            Value::Number(n) => match n.as_i64() {
                Some(1) => Button::Left,
                Some(2) => Button::Right,
                Some(3) => Button::Middle,
                Some(4) => Button::X1,
                Some(5) => Button::X2,
                _ => Button::None,
            },
            _ => Button::None,
        }
    }
}

/// `"Ctrl, Alt"` (how upstream writes a flags enum), `"Ctrl+Alt"`, or the number 3.
pub fn parse_modifiers(value: &Value) -> u8 {
    match value {
        Value::Number(n) => (n.as_u64().unwrap_or(0) & 0x0F) as u8,
        Value::String(s) => s
            .split(|c: char| c == ',' || c == '+' || c == '|' || c.is_whitespace())
            .map(|part| part.trim().to_ascii_lowercase())
            .fold(0u8, |acc, part| {
                acc | match part.as_str() {
                    "ctrl" | "control" => CTRL,
                    "alt" => ALT,
                    "shift" => SHIFT,
                    "meta" | "win" | "windows" | "cmd" => META,
                    _ => 0,
                }
            }),
        _ => 0,
    }
}

/// Key name for a Windows virtual-key code, in upstream's spelling. `None` for keys settings can't name
/// (modifiers, media keys, ...).
pub fn key_name(vk: u32) -> Option<String> {
    let name = match vk {
        0x41..=0x5A => return Some(((vk as u8) as char).to_string()),
        0x30..=0x39 => return Some(((vk as u8) as char).to_string()),
        0x60..=0x69 => return Some(format!("NumPad{}", vk - 0x60)),
        0x70..=0x87 => return Some(format!("F{}", vk - 0x6F)),
        0x20 => "Space",
        0x0D => "Enter",
        0x09 => "Tab",
        0x1B => "Escape",
        0x08 => "Backspace",
        0x2D => "Insert",
        0x2E => "Delete",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x25 => "Left",
        0x27 => "Right",
        0x26 => "Up",
        0x28 => "Down",
        0xC0 => "BackQuote",
        0xBD => "Minus",
        0xBB => "Equals",
        0xDB => "OpenBracket",
        0xDD => "CloseBracket",
        0xDC => "Backslash",
        0xBA => "Semicolon",
        0xDE => "Quote",
        0xBC => "Comma",
        0xBE => "Period",
        0xBF => "Slash",
        0x2C => "PrintScreen",
        0x13 => "Pause",
        _ => return None,
    };
    Some(name.to_string())
}

/// The key that flips the donut now (`DonutSettings.EffectiveFlipKey`): none when the user cleared it; `Tab` in place
/// of a key the donut uses or a key chord took; none when Tab isn't allowed either.
/// `chord_keys` are the `key` of every KeyChord trigger.
pub fn effective_flip_key(flip_key: &str, chord_keys: &[String]) -> Option<String> {
    let allowed = |key: &str| {
        !key.trim().is_empty()
            && !RESERVED_FLIP_KEYS.iter().any(|r| r.eq_ignore_ascii_case(key))
            && !chord_keys.iter().any(|k| k.eq_ignore_ascii_case(key))
    };
    let trimmed = flip_key.trim();
    if trimmed.is_empty() {
        return None;
    }
    if allowed(trimmed) {
        return Some(trimmed.to_string());
    }
    allowed(DEFAULT_FLIP_KEY).then(|| DEFAULT_FLIP_KEY.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn modifiers_parse_flag_strings() {
        assert_eq!(parse_modifiers(&json!("Ctrl")), CTRL);
        assert_eq!(parse_modifiers(&json!("Ctrl, Alt")), CTRL | ALT);
        assert_eq!(parse_modifiers(&json!("shift+meta")), SHIFT | META);
        assert_eq!(parse_modifiers(&json!("None")), 0);
        assert_eq!(parse_modifiers(&json!(5)), CTRL | SHIFT);
        assert_eq!(parse_modifiers(&json!(null)), 0);
    }

    #[test]
    fn buttons_parse_and_held_priority() {
        assert_eq!(Button::parse(&json!("Left")), Button::Left);
        assert_eq!(Button::parse(&json!("x2")), Button::X2);
        assert_eq!(Button::parse(&json!("None")), Button::None);
        assert_eq!(Button::held(Button::Right.bit() | Button::X1.bit()), Button::Right);
        assert_eq!(Button::held(Button::Left.bit() | Button::Right.bit()), Button::Left);
        assert_eq!(Button::held(0), Button::None);
    }

    #[test]
    fn key_names_match_upstream_spelling() {
        assert_eq!(key_name(0x20).as_deref(), Some("Space"));
        assert_eq!(key_name(0x51).as_deref(), Some("Q"));
        assert_eq!(key_name(0x35).as_deref(), Some("5"));
        assert_eq!(key_name(0x71).as_deref(), Some("F2"));
        assert_eq!(key_name(0x87).as_deref(), Some("F24"));
        assert_eq!(key_name(0x63).as_deref(), Some("NumPad3"));
        assert_eq!(key_name(0xC0).as_deref(), Some("BackQuote"));
        assert_eq!(key_name(0xA2), None); // Ctrl itself
    }

    #[test]
    fn flip_key_rules() {
        assert_eq!(effective_flip_key("Tab", &[]).as_deref(), Some("Tab"));
        assert_eq!(effective_flip_key("  Q ", &[]).as_deref(), Some("Q"));
        assert_eq!(effective_flip_key("", &[]), None);
        assert_eq!(effective_flip_key("Escape", &[]).as_deref(), Some("Tab"));
        assert_eq!(effective_flip_key("space", &[]).as_deref(), Some("Tab"));
        assert_eq!(effective_flip_key("Q", &["q".into()]).as_deref(), Some("Tab"));
        assert_eq!(effective_flip_key("Q", &["Tab".into(), "Q".into()]), None);
    }
}
