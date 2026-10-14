//! 要不要弹气泡、弹什么（设计文档 6.3）：安静模式只在出错时弹，标准模式另加开始与保存。

use crate::tray_model::format_elapsed;
use record_core::config::NotifyMode;
use record_core::session::EndReason;
use record_engine::events::Alert;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Started {
        dir: String,
    },
    Saved {
        reason: EndReason,
        meeting: Option<String>,
        duration_secs: f64,
        warnings: usize,
    },
    StartFailed(String),
    Alert(Alert),
    /// 每个没注册成功的热键一行。
    HotkeyConflict(Vec<String>),
    /// 启动时修复了上次中断的录制。
    Recovered {
        count: usize,
        failed: usize,
    },
    ConfigInvalid(String),
}

impl Notice {
    /// 出错一定弹提示，不受安静模式影响；开始、以及用户自己触发的保存，只在标准模式弹。
    pub fn always(&self) -> bool {
        match self {
            Notice::Started { .. } => false,
            Notice::Saved { reason, .. } => *reason != EndReason::User,
            Notice::Alert(a) => a.is_warning(),
            Notice::StartFailed(_) | Notice::HotkeyConflict(_) | Notice::Recovered { .. } | Notice::ConfigInvalid(_) => true,
        }
    }

    /// （标题，正文）
    pub fn text(&self) -> (String, String) {
        match self {
            Notice::Started { dir } => ("开始录制".into(), format!("保存到 {dir}")),
            Notice::Saved { reason, meeting, duration_secs, warnings } => {
                let mut body = format!("时长 {}", format_elapsed(Duration::from_secs_f64(*duration_secs)));
                match meeting {
                    Some(path) => body.push_str(&format!("，{path}")),
                    None => body.push_str("，没有生成成品，原始轨道文件还在会话目录里"),
                }
                if *warnings > 0 {
                    body.push_str(&format!("；本场有 {warnings} 条告警，详见日志"));
                }
                (reason.describe().to_string(), body)
            }
            Notice::StartFailed(why) => ("没能开始录制".into(), why.clone()),
            Notice::Alert(a) => (if a.is_warning() { "录制出了问题" } else { "录制提示" }.into(), a.describe()),
            Notice::HotkeyConflict(lines) => ("有热键没注册成功".into(), format!("{}。可以在设置里换一个组合", lines.join("；"))),
            Notice::Recovered { count, failed } => {
                let mut body = format!("已修复并生成 {count} 场上次中断的录制");
                if *failed > 0 {
                    body.push_str(&format!("，另有 {failed} 场没能修复，详见日志"));
                }
                ("已恢复上次中断的录制".into(), body)
            }
            Notice::ConfigInvalid(why) => ("配置文件有误，这次按默认值运行".into(), format!("{why}。原文件没有被改动")),
        }
    }
}

pub fn should_show(mode: NotifyMode, notice: &Notice) -> bool {
    notice.always() || mode == NotifyMode::Standard
}

#[cfg(test)]
mod tests {
    use super::*;
    use record_core::session::TrackKind;

    fn saved(reason: EndReason) -> Notice {
        Notice::Saved { reason, meeting: Some(r"D:\rec\a\meeting.mp4".into()), duration_secs: 3725.0, warnings: 0 }
    }

    #[test]
    fn quiet_mode_shows_only_what_went_wrong() {
        let quiet = |n: &Notice| should_show(NotifyMode::Quiet, n);
        assert!(!quiet(&Notice::Started { dir: "x".into() }));
        assert!(!quiet(&saved(EndReason::User)), "用户自己按的保存不用再提示");
        assert!(!quiet(&Notice::Alert(Alert::HdrActive)));
        assert!(!quiet(&Notice::Alert(Alert::HungRecovered { track: TrackKind::Mic })));
        for reason in [EndReason::MaxDuration, EndReason::DiskLow, EndReason::Sleep, EndReason::Shutdown, EndReason::Logoff, EndReason::WriteError, EndReason::CaptureFailure] {
            assert!(quiet(&saved(reason)), "{reason:?} 不是用户触发的，必须提示");
        }
        assert!(quiet(&Notice::StartFailed("磁盘不可写".into())));
        assert!(quiet(&Notice::Alert(Alert::TrackInterrupted { track: TrackKind::System })));
        assert!(quiet(&Notice::HotkeyConflict(vec!["开始录制".into()])));
        assert!(quiet(&Notice::Recovered { count: 1, failed: 0 }));
        assert!(quiet(&Notice::ConfigInvalid("第 3 行".into())));
    }

    #[test]
    fn standard_mode_shows_everything() {
        for n in [Notice::Started { dir: "x".into() }, saved(EndReason::User), Notice::Alert(Alert::HdrActive)] {
            assert!(should_show(NotifyMode::Standard, &n));
        }
    }

    #[test]
    fn the_texts_carry_what_the_user_needs() {
        let (title, body) = saved(EndReason::Sleep).text();
        assert_eq!(title, "因睡眠已保存");
        assert_eq!(body, r"时长 1:02:05，D:\rec\a\meeting.mp4");
        let (_, body) = Notice::Saved { reason: EndReason::User, meeting: None, duration_secs: 5.0, warnings: 2 }.text();
        assert!(body.contains("没有生成成品") && body.contains("2 条告警"), "{body}");
        let (title, body) = Notice::Alert(Alert::Hung { track: TrackKind::Video, secs: 12 }).text();
        assert_eq!(title, "录制出了问题");
        assert!(body.contains("画面") && body.contains("12"), "{body}");
        assert_eq!(Notice::Alert(Alert::HdrActive).text().0, "录制提示");
        let (_, body) = Notice::HotkeyConflict(vec!["开始录制（Ctrl+Alt+R）：被占用".into(), "保存（Ctrl+Alt+S）：被占用".into()]).text();
        assert!(body.contains("开始录制") && body.contains("保存") && body.contains("设置"), "{body}");
        assert!(Notice::Recovered { count: 2, failed: 1 }.text().1.contains("另有 1 场"));
        assert!(Notice::StartFailed("没有找到显示器".into()).text().1.contains("显示器"));
    }
}
