//! 生成 `meeting.mp4`：收尾与崩溃恢复共用（设计文档 5.7、5.8）。

use record_core::ffmpeg_args::{FinalizePlan, VIDEO_CONCAT_FILE, concat_list};
use record_core::session::{SegmentMeta, SessionMeta, TrackKind, TrackStatus};
use record_mux::MuxError;
use record_mux::finalize::finalize_meeting;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 混音输出的采样率，与采集一致。
pub const AUDIO_RATE: u32 = 48_000;
/// 生成成品的超时：1 小时录音的音频转码与封装通常在一分钟内完成，这里留足余量。
pub const FINALIZE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

fn non_empty(dir: &Path, name: &str) -> bool {
    std::fs::metadata(dir.join(name)).is_ok_and(|m| m.len() > 0)
}

/// 编码器重启留下的一段是否有内容：文件非空，且不是已知一帧都没写出来的画面段。
fn segment_usable(dir: &Path, s: &SegmentMeta) -> bool {
    s.frames != Some(0) && non_empty(dir, &s.file)
}

/// 这一轨有可用的数据：文件存在且非空，元数据没有标为缺失。编码器重启过的轨道，任何一段有内容就算。
pub fn track_usable(dir: &Path, meta: &SessionMeta, kind: TrackKind) -> bool {
    meta.tracks.get(kind).is_some_and(|t| t.status != TrackStatus::Missing && if t.segments.is_empty() { non_empty(dir, &t.file) } else { t.segments.iter().any(|s| segment_usable(dir, s)) })
}

/// 去掉编码器重启轨道里没有内容的段。拼接和混音只认有内容的段。
fn without_empty_segments(dir: &Path, meta: &SessionMeta) -> SessionMeta {
    let mut m = meta.clone();
    for kind in TrackKind::ALL {
        if let Some(t) = m.tracks.get_mut(kind) {
            t.segments.retain(|s| segment_usable(dir, s));
        }
    }
    m
}

/// 按 `session.json` 里的起点与停止时刻生成成品。没有任何可用轨道时返回 `Ok(None)`。
pub fn make_meeting(ffmpeg: &Path, dir: &Path, meta: &SessionMeta) -> Result<Option<PathBuf>, MuxError> {
    // 可用与否按原始元数据判断：一条轨道的段全是空的，去掉空段之后列表为空，会被误当成“没重启过”
    let cleaned = without_empty_segments(dir, meta);
    let plan = FinalizePlan::from_meta(&cleaned, |k| track_usable(dir, meta, k));
    if plan.video.is_none() && plan.mic.is_none() && plan.system.is_none() {
        return Ok(None);
    }
    if let Some(segs) = plan.segments.get(&TrackKind::Video).filter(|s| s.len() > 1) {
        let fps = meta.output.as_ref().map(|o| o.fps).ok_or_else(|| MuxError::Failed { code: None, tail: "session.json 里没有画面输出参数，没法拼接画面的各段".into() })?;
        std::fs::write(dir.join(VIDEO_CONCAT_FILE), concat_list(segs, fps))?;
    }
    finalize_meeting(ffmpeg, dir, &plan, AUDIO_RATE, FINALIZE_TIMEOUT).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use record_core::session::{OutputMeta, TrackMeta};

    fn meta_with(kind: TrackKind, status: TrackStatus) -> SessionMeta {
        let mut m = SessionMeta::new("x", "0", Utc::now(), 10_000_000);
        m.tracks.set(kind, TrackMeta { status, ..TrackMeta::new(kind.file_name(), Some(0)) });
        m
    }

    fn restarted_video(first_frames: Option<u64>) -> SessionMeta {
        let mut m = SessionMeta::new("x", "0", Utc::now(), 10_000_000);
        m.output = Some(OutputMeta { width: 640, height: 360, fps: 15, encoder: "h264_nvenc".into() });
        let mut t = TrackMeta::new("video.mkv", Some(0));
        t.push_segment("video.2.mkv".into(), 90_000_000, first_frames);
        m.tracks.set(TrackKind::Video, t);
        m
    }

    #[test]
    fn a_track_needs_a_non_empty_file_and_a_present_status() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = meta_with(TrackKind::Mic, TrackStatus::Ok);
        assert!(!track_usable(tmp.path(), &ok, TrackKind::Mic), "文件不存在");
        std::fs::write(tmp.path().join("mic.mka"), b"").unwrap();
        assert!(!track_usable(tmp.path(), &ok, TrackKind::Mic), "空文件");
        std::fs::write(tmp.path().join("mic.mka"), b"x").unwrap();
        assert!(track_usable(tmp.path(), &ok, TrackKind::Mic));
        assert!(!track_usable(tmp.path(), &meta_with(TrackKind::Mic, TrackStatus::Missing), TrackKind::Mic), "标为缺失");
        assert!(!track_usable(tmp.path(), &ok, TrackKind::System), "元数据里没有这一轨");
    }

    #[test]
    fn an_interrupted_track_is_still_used() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("system.mka"), b"x").unwrap();
        assert!(track_usable(tmp.path(), &meta_with(TrackKind::System, TrackStatus::Interrupted), TrackKind::System));
    }

    #[test]
    fn nothing_to_mix_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = SessionMeta::new("x", "0", Utc::now(), 10_000_000);
        assert!(matches!(make_meeting(Path::new("ffmpeg-不存在.exe"), tmp.path(), &meta), Ok(None)));
    }

    #[test]
    fn a_restarted_track_is_usable_if_any_segment_has_content() {
        let tmp = tempfile::tempdir().unwrap();
        let m = restarted_video(Some(40));
        assert!(!track_usable(tmp.path(), &m, TrackKind::Video), "两段都没文件");
        std::fs::write(tmp.path().join("video.2.mkv"), b"x").unwrap();
        assert!(track_usable(tmp.path(), &m, TrackKind::Video), "只有后一段有内容也算");
    }

    #[test]
    fn a_first_segment_known_to_hold_no_frames_does_not_count_even_if_its_file_has_a_header() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("video.mkv"), b"header only").unwrap();
        assert!(!track_usable(tmp.path(), &restarted_video(Some(0)), TrackKind::Video), "第 1 段 0 帧、第 2 段没文件：这一轨没有内容");
        std::fs::write(tmp.path().join("video.2.mkv"), b"x").unwrap();
        assert!(track_usable(tmp.path(), &restarted_video(Some(0)), TrackKind::Video));
    }

    #[test]
    fn empty_segments_are_dropped_before_mixing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("video.mkv"), b"header only").unwrap();
        std::fs::write(tmp.path().join("video.2.mkv"), b"x").unwrap();
        let cleaned = without_empty_segments(tmp.path(), &restarted_video(Some(0)));
        let files: Vec<_> = cleaned.tracks.video.unwrap().segments.into_iter().map(|s| s.file).collect();
        assert_eq!(files, vec!["video.2.mkv"]);
    }

    #[test]
    fn the_video_segments_are_listed_for_the_concat_demuxer_before_ffmpeg_runs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("video.mkv"), b"x").unwrap();
        std::fs::write(tmp.path().join("video.2.mkv"), b"x").unwrap();
        // ffmpeg 不存在：生成必然失败，但拼接清单已经写好了
        assert!(make_meeting(Path::new("ffmpeg-不存在.exe"), tmp.path(), &restarted_video(Some(40))).is_err());
        let list = std::fs::read_to_string(tmp.path().join(VIDEO_CONCAT_FILE)).unwrap();
        assert_eq!(list, "ffconcat version 1.0\nfile 'video.mkv'\nduration 2.6666667\nfile 'video.2.mkv'\n");
    }

    #[test]
    fn a_single_remaining_segment_needs_no_concat_list() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("video.mkv"), b"header only").unwrap();
        std::fs::write(tmp.path().join("video.2.mkv"), b"x").unwrap();
        assert!(make_meeting(Path::new("ffmpeg-不存在.exe"), tmp.path(), &restarted_video(Some(0))).is_err());
        assert!(!tmp.path().join(VIDEO_CONCAT_FILE).exists());
    }
}
