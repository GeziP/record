//! 热键字符串的解析与规范化（如 `ctrl+alt+r` → `Ctrl+Alt+R`）。
//!
//! 只接受最保守的一组按键：字母、数字、F1–F24、Space，并且至少带一个修饰键，
//! 避免把普通打字吞掉。交给 Tauri 的 global-shortcut 插件时用 [`Hotkey::plugin_string`]（插件不认 `Win`，要写 `Super`）。

use std::fmt;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HotkeyError {
    #[error("热键为空")]
    Empty,
    #[error("热键“{0}”里有空的部分")]
    EmptyPart(String),
    #[error("热键“{0}”没有主键")]
    NoKey(String),
    #[error("热键“{0}”有多个主键")]
    MultipleKeys(String),
    #[error("热键“{0}”没有修饰键（Ctrl、Alt、Shift、Win 之一），会吞掉普通打字")]
    NoModifier(String),
    #[error("不支持的按键“{0}”（支持字母、数字、F1–F24 和 Space）")]
    UnsupportedKey(String),
}

impl Hotkey {
    pub fn parse(text: &str) -> Result<Self, HotkeyError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(HotkeyError::Empty);
        }
        let mut hk = Hotkey { ctrl: false, alt: false, shift: false, win: false, key: String::new() };
        for part in text.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return Err(HotkeyError::EmptyPart(text.to_string()));
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hk.ctrl = true,
                "alt" | "option" => hk.alt = true,
                "shift" => hk.shift = true,
                "win" | "super" | "meta" | "cmd" | "command" => hk.win = true,
                _ => {
                    if !hk.key.is_empty() {
                        return Err(HotkeyError::MultipleKeys(text.to_string()));
                    }
                    hk.key = canonical_key(part)?;
                }
            }
        }
        if hk.key.is_empty() {
            return Err(HotkeyError::NoKey(text.to_string()));
        }
        if !(hk.ctrl || hk.alt || hk.shift || hk.win) {
            return Err(HotkeyError::NoModifier(text.to_string()));
        }
        Ok(hk)
    }

    /// 交给 global-shortcut 插件的写法：它的解析器只认 `Super`，不认 `Win`（S5 实测），其余与 [`Display`](fmt::Display) 相同。
    pub fn plugin_string(&self) -> String {
        self.to_string().replace("Win+", "Super+")
    }
}

fn canonical_key(part: &str) -> Result<String, HotkeyError> {
    let upper = part.to_ascii_uppercase();
    let b = upper.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Ok(upper);
    }
    if upper == "SPACE" {
        return Ok("Space".to_string());
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|d| d.parse::<u8>().ok())
        && (1..=24).contains(&n)
    {
        return Ok(format!("F{n}"));
    }
    Err(HotkeyError::UnsupportedKey(part.to_string()))
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.win {
            f.write_str("Win+")?;
        }
        f.write_str(&self.key)
    }
}

impl FromStr for Hotkey {
    type Err = HotkeyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Hotkey::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        Hotkey::parse(s).unwrap().to_string()
    }

    #[test]
    fn normalizes_case_spacing_and_order() {
        assert_eq!(norm("ctrl+alt+r"), "Ctrl+Alt+R");
        assert_eq!(norm("  Alt + Ctrl + S "), "Ctrl+Alt+S");
        assert_eq!(norm("CONTROL+SHIFT+f5"), "Ctrl+Shift+F5");
        assert_eq!(norm("win+space"), "Win+Space");
        assert_eq!(norm("Ctrl+Alt+Q"), "Ctrl+Alt+Q");
        assert_eq!(norm("ctrl+ctrl+1"), "Ctrl+1");
    }

    #[test]
    fn rejects_bad_hotkeys() {
        assert_eq!(Hotkey::parse(""), Err(HotkeyError::Empty));
        assert!(matches!(Hotkey::parse("R"), Err(HotkeyError::NoModifier(_))));
        assert!(matches!(Hotkey::parse("Ctrl+Alt"), Err(HotkeyError::NoKey(_))));
        assert!(matches!(Hotkey::parse("Ctrl+"), Err(HotkeyError::EmptyPart(_))));
        assert!(matches!(Hotkey::parse("Ctrl+A+B"), Err(HotkeyError::MultipleKeys(_))));
        assert!(matches!(Hotkey::parse("Ctrl+F25"), Err(HotkeyError::UnsupportedKey(_))));
        assert!(matches!(Hotkey::parse("Ctrl+Enter"), Err(HotkeyError::UnsupportedKey(_))));
        assert!(matches!(Hotkey::parse("Ctrl+Alt+中"), Err(HotkeyError::UnsupportedKey(_))));
    }

    #[test]
    fn function_keys_cover_one_to_twenty_four() {
        for n in 1..=24 {
            assert_eq!(norm(&format!("alt+f{n}")), format!("Alt+F{n}"));
        }
    }

    #[test]
    fn the_plugin_wants_super_where_we_write_win() {
        assert_eq!(Hotkey::parse("alt+win+9").unwrap().plugin_string(), "Alt+Super+9");
        assert_eq!(Hotkey::parse("ctrl+alt+r").unwrap().plugin_string(), "Ctrl+Alt+R");
        assert_eq!(Hotkey::parse("Win+Space").unwrap().plugin_string(), "Super+Space");
    }

    #[test]
    fn display_round_trips() {
        for s in ["Ctrl+Alt+R", "Ctrl+Shift+F12", "Alt+Win+9"] {
            assert_eq!(norm(s), s);
        }
    }
}
