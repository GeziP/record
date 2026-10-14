//! 崩溃恢复的计划（设计文档 5.8）。
//!
//! 纯判定：根据 `session.json` 和目录里有哪些文件，决定要做什么。执行（调 ffmpeg、改文件）在 `record-engine`。
//! 启动时扫描保存目录，对每个会话目录算一份计划：
//! - `state=recording`：上次没有正常收尾。每个存在的轨道文件（编码器重启过的轨道含每一段）做拷贝式修复，标为 `recovered`，补做 `meeting.mp4`。
//! - `state=finalized` 或 `recovered` 但缺少 `meeting.mp4`：只补做 `meeting.mp4`。
//! - 没有 `session.json` 的目录不是我们的会话（或者元数据已损坏），不动它，由调用方报告。

use crate::session::{MEETING_FILE, SessionMeta, SessionState, TrackKind};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryPlan {
    /// 要做拷贝式修复的轨道文件名（存在的，含编码器重启后的各段）。
    pub repair: Vec<String>,
    /// 要（重新）生成 `meeting.mp4`。
    pub remux: bool,
    /// 完成后把会话标为 `recovered`。
    pub mark_recovered: bool,
}

impl RecoveryPlan {
    /// 什么都不用做。
    pub fn is_noop(&self) -> bool {
        self.repair.is_empty() && !self.remux && !self.mark_recovered
    }
}

/// `exists` 判断会话目录里是否有某个文件（按文件名）。元数据里没有登记的轨道，按默认文件名找。
pub fn plan(meta: &SessionMeta, exists: impl Fn(&str) -> bool) -> RecoveryPlan {
    let present: Vec<String> = TrackKind::ALL
        .iter()
        .flat_map(|&k| match meta.tracks.get(k) {
            Some(t) => t.files(),
            None => vec![k.file_name().to_string()],
        })
        .filter(|f| exists(f))
        .collect();
    let any = !present.is_empty();
    match meta.state {
        SessionState::Recording => RecoveryPlan { repair: present, remux: any, mark_recovered: true },
        SessionState::Finalized | SessionState::Recovered => RecoveryPlan { repair: Vec::new(), remux: any && !exists(MEETING_FILE), mark_recovered: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn meta(state: SessionState) -> SessionMeta {
        let mut m = SessionMeta::new("2026-10-13_203000", "0.1.0", Utc::now(), 10_000_000);
        m.state = state;
        m
    }

    fn has<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |n| names.contains(&n)
    }

    #[test]
    fn an_interrupted_session_repairs_every_track_that_exists_then_remuxes() {
        let p = plan(&meta(SessionState::Recording), has(&["video.mkv", "mic.mka", "system.mka", "session.json"]));
        assert_eq!(p, RecoveryPlan { repair: vec!["video.mkv".into(), "mic.mka".into(), "system.mka".into()], remux: true, mark_recovered: true });
    }

    #[test]
    fn missing_tracks_are_not_repaired() {
        let p = plan(&meta(SessionState::Recording), has(&["video.mkv", "system.mka"]));
        assert_eq!(p.repair, vec!["video.mkv", "system.mka"]);
        assert!(p.remux);
    }

    #[test]
    fn an_interrupted_session_with_no_data_is_only_marked() {
        let p = plan(&meta(SessionState::Recording), has(&["session.json"]));
        assert_eq!(p, RecoveryPlan { repair: vec![], remux: false, mark_recovered: true });
        assert!(!p.is_noop(), "状态要从 recording 改掉，否则每次启动都会再扫到它");
    }

    #[test]
    fn every_segment_of_a_restarted_track_is_repaired() {
        let mut m = meta(SessionState::Recording);
        let mut t = crate::session::TrackMeta::new("video.mkv", Some(1));
        t.segments = vec![crate::session::SegmentMeta { file: "video.mkv".into(), t0_hns: 1, frames: Some(40) }, crate::session::SegmentMeta { file: "video.2.mkv".into(), t0_hns: 9, frames: None }];
        m.tracks.set(TrackKind::Video, t);
        let p = plan(&m, has(&["video.mkv", "video.2.mkv", "mic.mka"]));
        assert_eq!(p.repair, vec!["video.mkv", "video.2.mkv", "mic.mka"]);
        assert!(p.remux);
    }

    #[test]
    fn a_segment_file_that_never_got_written_is_skipped() {
        let mut m = meta(SessionState::Recording);
        let mut t = crate::session::TrackMeta::new("mic.mka", Some(1));
        t.segments = vec![crate::session::SegmentMeta { file: "mic.mka".into(), t0_hns: 1, frames: None }, crate::session::SegmentMeta { file: "mic.2.mka".into(), t0_hns: 9, frames: None }];
        m.tracks.set(TrackKind::Mic, t);
        assert_eq!(plan(&m, has(&["mic.2.mka"])).repair, vec!["mic.2.mka"]);
    }

    #[test]
    fn a_finalized_session_without_meeting_only_remuxes() {
        let p = plan(&meta(SessionState::Finalized), has(&["video.mkv", "mic.mka"]));
        assert_eq!(p, RecoveryPlan { repair: vec![], remux: true, mark_recovered: false });
    }

    #[test]
    fn a_recovered_session_without_meeting_remuxes_again() {
        let p = plan(&meta(SessionState::Recovered), has(&["system.mka"]));
        assert!(p.remux && !p.mark_recovered && p.repair.is_empty());
    }

    #[test]
    fn a_complete_session_needs_nothing() {
        for state in [SessionState::Finalized, SessionState::Recovered] {
            assert!(plan(&meta(state), has(&["video.mkv", "mic.mka", "system.mka", "meeting.mp4"])).is_noop(), "{state:?}");
        }
    }

    #[test]
    fn a_finalized_session_with_nothing_to_mix_is_left_alone() {
        assert!(plan(&meta(SessionState::Finalized), has(&["session.json"])).is_noop());
    }
}
