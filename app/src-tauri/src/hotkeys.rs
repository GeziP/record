//! 三个热键的注册计划与结果（设计文档 6.2）。注册本身在 `app.rs`，这里只有可以脱离 Tauri 测试的部分。

use record_core::config::Config;
use record_core::controller::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyStatus {
    Registered,
    /// 没注册成功的原因，原样给用户看。
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyEntry {
    pub command: Command,
    /// 规范写法，如 `Ctrl+Alt+R`。
    pub text: String,
    pub status: HotkeyStatus,
}

pub fn command_name(command: Command) -> &'static str {
    match command {
        Command::Start => "开始录制",
        Command::Stop => "保存并结束本场",
        Command::Quit => "保存并退出程序",
    }
}

/// 配置里要注册的三个热键：（命令，规范写法，交给插件的写法）。热键写错或重复时返回说明。
pub fn wanted(config: &Config) -> Result<Vec<(Command, String, String)>, String> {
    let set = config.hotkey_set()?;
    Ok(vec![
        (Command::Start, set.start.to_string(), set.start.plugin_string()),
        (Command::Stop, set.stop.to_string(), set.stop.plugin_string()),
        (Command::Quit, set.quit.to_string(), set.quit.plugin_string()),
    ])
}

/// 托盘菜单里的文字：写上热键，没注册成功的标出来。
pub fn menu_label(command: Command, entry: Option<&HotkeyEntry>) -> String {
    let name = command_name(command);
    match entry {
        None => name.to_string(),
        Some(HotkeyEntry { status: HotkeyStatus::Registered, text, .. }) => format!("{name}（{text}）"),
        Some(HotkeyEntry { status: HotkeyStatus::Failed(_), text, .. }) => format!("{name}（{text} 没注册成功，被占用）"),
    }
}

/// 给“有热键没注册成功”的提示用：原因相同的热键合并成一行（配置写错时三个热键是同一个原因，不必说三遍）。
pub fn conflict_lines(entries: &[HotkeyEntry]) -> Vec<String> {
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for entry in entries {
        if let HotkeyStatus::Failed(why) = &entry.status {
            let name = format!("{}（{}）", command_name(entry.command), entry.text);
            match groups.iter_mut().find(|(reason, _)| reason == why) {
                Some((_, names)) => names.push(name),
                None => groups.push((why, vec![name])),
            }
        }
    }
    groups.into_iter().map(|(reason, names)| format!("{}：{reason}", names.join("、"))).collect()
}

/// 注册失败时插件给的是英文原文，里面最有用的只有“被占用”这个事实。
pub fn explain_failure(raw: &str) -> String {
    if raw.contains("already registered") { "已被别的程序占用，或被系统保留".to_string() } else { raw.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_become_three_plugin_strings() {
        let w = wanted(&Config::default()).unwrap();
        assert_eq!(w.iter().map(|(c, ..)| *c).collect::<Vec<_>>(), vec![Command::Start, Command::Stop, Command::Quit]);
        assert_eq!(w.iter().map(|(_, text, _)| text.as_str()).collect::<Vec<_>>(), vec!["Ctrl+Alt+R", "Ctrl+Alt+S", "Ctrl+Alt+Q"]);
    }

    #[test]
    fn the_windows_key_is_spelled_the_way_the_plugin_wants() {
        let mut c = Config::default();
        c.hotkeys.start = "alt+win+9".into();
        let w = wanted(&c).unwrap();
        assert_eq!((w[0].1.as_str(), w[0].2.as_str()), ("Alt+Win+9", "Alt+Super+9"));
    }

    #[test]
    fn bad_or_duplicate_hotkeys_are_explained_instead_of_registered() {
        let mut c = Config::default();
        c.hotkeys.stop = c.hotkeys.start.clone();
        assert!(wanted(&c).unwrap_err().contains("不能相同"));
        c.hotkeys.stop = "S".into();
        assert!(wanted(&c).unwrap_err().contains("hotkeys.stop"));
    }

    #[test]
    fn the_menu_shows_the_hotkey_and_flags_the_ones_that_failed() {
        let ok = HotkeyEntry { command: Command::Start, text: "Ctrl+Alt+R".into(), status: HotkeyStatus::Registered };
        let bad = HotkeyEntry { command: Command::Stop, text: "Ctrl+Alt+S".into(), status: HotkeyStatus::Failed("x".into()) };
        assert_eq!(menu_label(Command::Start, Some(&ok)), "开始录制（Ctrl+Alt+R）");
        assert_eq!(menu_label(Command::Stop, Some(&bad)), "保存并结束本场（Ctrl+Alt+S 没注册成功，被占用）");
        assert_eq!(menu_label(Command::Quit, None), "保存并退出程序");
        assert_eq!(conflict_lines(&[ok, bad]), vec!["保存并结束本场（Ctrl+Alt+S）：x".to_string()]);
    }

    #[test]
    fn hotkeys_that_failed_for_the_same_reason_share_one_line() {
        let failed = |command, text: &str, why: &str| HotkeyEntry { command, text: text.into(), status: HotkeyStatus::Failed(why.into()) };
        let all_wrong = [failed(Command::Start, "R", "没有修饰键"), failed(Command::Stop, "Ctrl+Alt+S", "没有修饰键"), failed(Command::Quit, "Ctrl+Alt+Q", "没有修饰键")];
        assert_eq!(conflict_lines(&all_wrong), vec!["开始录制（R）、保存并结束本场（Ctrl+Alt+S）、保存并退出程序（Ctrl+Alt+Q）：没有修饰键".to_string()]);
        let mixed = [failed(Command::Start, "Ctrl+Alt+R", "被占用"), failed(Command::Stop, "Ctrl+Alt+S", "不认识"), failed(Command::Quit, "Ctrl+Alt+Q", "被占用")];
        assert_eq!(conflict_lines(&mixed), vec!["开始录制（Ctrl+Alt+R）、保存并退出程序（Ctrl+Alt+Q）：被占用".to_string(), "保存并结束本场（Ctrl+Alt+S）：不认识".to_string()]);
        assert!(conflict_lines(&[]).is_empty());
    }

    #[test]
    fn the_plugins_english_error_is_translated_when_it_means_the_combination_is_taken() {
        let raw = "HotKey already registered: HotKey { mods: Modifiers(ALT | SUPER), key: Digit9, id: 536936462 }";
        assert_eq!(explain_failure(raw), "已被别的程序占用，或被系统保留");
        assert_eq!(explain_failure("别的错误"), "别的错误");
    }
}
