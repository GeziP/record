//! 托盘图标与提示文字（设计文档 6.3）：由控制器状态和告警数量决定的纯函数，不碰 Tauri。

use record_core::controller::State;
use std::time::Duration;

/// 托盘图标的样子：灰（待命）、红点（录制中）、琥珀（录制中但有告警）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Standby,
    Recording,
    Warning,
}

pub fn look(state: State, warnings: usize) -> Look {
    match state {
        State::Idle => Look::Standby,
        _ if warnings > 0 => Look::Warning,
        _ => Look::Recording,
    }
}

pub fn rgb(look: Look) -> [u8; 3] {
    match look {
        Look::Standby => [136, 136, 136],
        Look::Recording => [220, 20, 60],
        Look::Warning => [255, 176, 0],
    }
}

pub const ICON_SIZE: u32 = 32;

/// 纯色圆点（RGBA，`ICON_SIZE` 见方），托盘图标用；圆外透明。
pub fn icon_rgba(rgb: [u8; 3]) -> Vec<u8> {
    let n = ICON_SIZE as usize;
    let center = n as f32 / 2.0;
    let radius = center - 4.0;
    let mut pixels = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
            if dx * dx + dy * dy <= radius * radius {
                pixels[(y * n + x) * 4..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    pixels
}

/// 托盘菜单里“开始”“保存并结束”能不能点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuEnabled {
    pub start: bool,
    pub stop: bool,
}

pub fn menu_enabled(state: State) -> MenuEnabled {
    MenuEnabled { start: state == State::Idle, stop: state == State::Recording }
}

/// `1:05`、`1:00:00`。
pub fn format_elapsed(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

pub fn format_size(bytes: u64) -> String {
    const MB: f64 = 1_048_576.0;
    const GB: f64 = 1_073_741_824.0;
    let b = bytes as f64;
    if b >= GB { format!("{:.2} GB", b / GB) } else { format!("{:.0} MB", b / MB) }
}

/// Windows 托盘提示最多 127 个字符：留一点余量，多出的部分换成省略号。
pub const TOOLTIP_MAX_CHARS: usize = 120;

pub fn tooltip(state: State, elapsed: Duration, bytes: u64, warnings: &[String]) -> String {
    let head = match state {
        State::Idle => "Record：待命".to_string(),
        State::Starting => "Record：正在启动…".to_string(),
        State::Recording => format!("Record：录制中 {}，已写入 {}", format_elapsed(elapsed), format_size(bytes)),
        State::Finalizing => "Record：正在保存…".to_string(),
    };
    let text = match warnings.last() {
        Some(w) if state != State::Idle => format!("{head}\n注意：{w}"),
        _ => head,
    };
    if text.chars().count() > TOOLTIP_MAX_CHARS { text.chars().take(TOOLTIP_MAX_CHARS - 1).collect::<String>() + "…" } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_grey_when_idle_red_when_recording_and_amber_with_warnings() {
        assert_eq!(look(State::Idle, 0), Look::Standby);
        assert_eq!(look(State::Idle, 3), Look::Standby, "待命时不显示上一场的告警");
        assert_eq!(look(State::Recording, 0), Look::Recording);
        assert_eq!(look(State::Recording, 1), Look::Warning);
        assert_eq!(look(State::Finalizing, 0), Look::Recording);
        assert_eq!(look(State::Starting, 2), Look::Warning);
        let colors = [rgb(Look::Standby), rgb(Look::Recording), rgb(Look::Warning)];
        assert!(colors[0] != colors[1] && colors[1] != colors[2] && colors[0] != colors[2]);
    }

    #[test]
    fn the_icon_is_a_coloured_dot_on_a_transparent_square() {
        let px = icon_rgba(rgb(Look::Recording));
        let n = ICON_SIZE as usize;
        assert_eq!(px.len(), n * n * 4);
        let at = |x: usize, y: usize| &px[(y * n + x) * 4..][..4];
        assert_eq!(at(n / 2, n / 2), &[220, 20, 60, 255]);
        assert_eq!(at(0, 0)[3], 0, "角上是透明的");
        assert_eq!(at(n - 1, n - 1)[3], 0);
    }

    #[test]
    fn only_the_applicable_menu_items_are_enabled() {
        assert_eq!(menu_enabled(State::Idle), MenuEnabled { start: true, stop: false });
        assert_eq!(menu_enabled(State::Recording), MenuEnabled { start: false, stop: true });
        assert_eq!(menu_enabled(State::Starting), MenuEnabled { start: false, stop: false });
        assert_eq!(menu_enabled(State::Finalizing), MenuEnabled { start: false, stop: false });
    }

    #[test]
    fn elapsed_time_and_size_are_short_and_readable() {
        let secs = Duration::from_secs;
        assert_eq!(format_elapsed(secs(0)), "0:00");
        assert_eq!(format_elapsed(secs(65)), "1:05");
        assert_eq!(format_elapsed(secs(3599)), "59:59");
        assert_eq!(format_elapsed(secs(3600)), "1:00:00");
        assert_eq!(format_elapsed(secs(45_296)), "12:34:56");
        assert_eq!(format_size(0), "0 MB");
        assert_eq!(format_size(5 * 1_048_576), "5 MB");
        assert_eq!(format_size(1_610_612_736), "1.50 GB");
    }

    #[test]
    fn the_tooltip_shows_progress_and_the_latest_warning_only_while_a_session_is_on() {
        assert_eq!(tooltip(State::Idle, Duration::ZERO, 0, &[]), "Record：待命");
        assert_eq!(tooltip(State::Recording, Duration::from_secs(125), 150 * 1_048_576, &[]), "Record：录制中 2:05，已写入 150 MB");
        let w = vec!["麦克风中断了".to_string(), "画面帧率偏低".to_string()];
        assert_eq!(tooltip(State::Recording, Duration::from_secs(1), 0, &w), "Record：录制中 0:01，已写入 0 MB\n注意：画面帧率偏低");
        assert_eq!(tooltip(State::Idle, Duration::ZERO, 0, &w), "Record：待命");
        assert_eq!(tooltip(State::Finalizing, Duration::ZERO, 0, &[]), "Record：正在保存…");
    }

    #[test]
    fn a_long_warning_is_cut_to_what_the_tray_can_show() {
        let long = vec!["很长的告警".repeat(40)];
        let t = tooltip(State::Recording, Duration::from_secs(1), 0, &long);
        assert_eq!(t.chars().count(), TOOLTIP_MAX_CHARS);
        assert!(t.ends_with('…'));
    }
}
