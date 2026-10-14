//! 会话目录与 `session.json`（设计文档 4.3、5.9、16.2）。
//!
//! 子项目 A 不引入数据库：会话状态以 `session.json` 为准，启动时扫描修复。
//! 文件用“临时文件 + 改名”原子写入，崩溃时不会留下半个元数据文件。

use crate::pacer::FrameStats;
use crate::timeline::TimelineStats;
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const SESSION_FILE: &str = "session.json";
pub const SESSION_VERSION: u32 = 1;
pub const MEETING_FILE: &str = "meeting.mp4";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Mic,
    System,
}

impl TrackKind {
    pub const ALL: [TrackKind; 3] = [TrackKind::Video, TrackKind::Mic, TrackKind::System];

    pub fn file_name(self) -> &'static str {
        match self {
            TrackKind::Video => "video.mkv",
            TrackKind::Mic => "mic.mka",
            TrackKind::System => "system.mka",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            TrackKind::Video => "video",
            TrackKind::Mic => "mic",
            TrackKind::System => "system",
        }
    }

    /// 第 `n` 段的文件名（从 1 起）。第 1 段就是 [`TrackKind::file_name`]；
    /// 编码器重启后写第 2 段起：`video.2.mkv`、`mic.2.mka`……
    pub fn segment_file_name(self, n: u32) -> String {
        let base = self.file_name();
        if n <= 1 {
            return base.to_string();
        }
        let (stem, ext) = base.rsplit_once('.').expect("轨道文件名带扩展名");
        format!("{stem}.{n}.{ext}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// 录制中；启动时仍是这个状态说明上次没有正常收尾。
    Recording,
    Finalized,
    /// 崩溃后已修复。
    Recovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    User,
    MaxDuration,
    DiskLow,
    Sleep,
    Shutdown,
    Logoff,
    WriteError,
    CaptureFailure,
    Recovered,
}

impl EndReason {
    /// 托盘提示用的说明。
    pub fn describe(self) -> &'static str {
        match self {
            EndReason::User => "已保存",
            EndReason::MaxDuration => "已达到单场时长上限，已自动保存",
            EndReason::DiskLow => "磁盘剩余空间不足，已自动保存",
            EndReason::Sleep => "因睡眠已保存",
            EndReason::Shutdown => "因关机已保存",
            EndReason::Logoff => "因注销已保存",
            EndReason::WriteError => "写入出错，已保存已有内容",
            EndReason::CaptureFailure => "采集失败，已保存已有内容",
            EndReason::Recovered => "已恢复上次中断的录制",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackStatus {
    Ok,
    /// 录制中该轨中断过（设备丢失超过 10 秒、编码器重启仍失败等）。
    Interrupted,
    /// 没有这一轨（例如没有麦克风）。
    Missing,
}

/// 编码器中途重启后，一条轨道由多个文件（段）接成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub file: String,
    /// 这一段第一个采样（帧）的 QPC 时间，100 ns 单位。
    pub t0_hns: i64,
    /// 画面轨：已经关闭的段里的帧数（下一段从这个序号接着写，成品用它拼接）。最后一段（仍在写、或崩溃时）为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackMeta {
    /// 第一段的文件名。
    pub file: String,
    /// 该轨起点的 QPC 时间，100 ns 单位；该轨始终没有数据时为空。
    pub t0_hns: Option<i64>,
    pub status: TrackStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 编码器重启过的轨道才有：按顺序列出每一段。没有重启过时为空，只有 `file` 一个文件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<SegmentMeta>,
}

impl TrackMeta {
    pub fn new(file: impl Into<String>, t0_hns: Option<i64>) -> Self {
        Self { file: file.into(), t0_hns, status: TrackStatus::Ok, note: None, segments: Vec::new() }
    }

    /// 这条轨道的全部文件名，按先后顺序。
    pub fn files(&self) -> Vec<String> {
        if self.segments.is_empty() { vec![self.file.clone()] } else { self.segments.iter().map(|s| s.file.clone()).collect() }
    }

    /// 编码器重启时，下一段要写的文件名：没重启过的轨道是第 1 段，下一段就是第 2 段。
    pub fn next_segment_file(&self, kind: TrackKind) -> String {
        kind.segment_file_name(self.segments.len().max(1) as u32 + 1)
    }

    /// 新写入器已经换上之后登记新的一段。首次重启时先把原来的文件登记为第 1 段；
    /// `closed_frames` 是被替换那一段实际含有的帧数（画面），音频传 `None`。
    pub fn push_segment(&mut self, file: String, t0_hns: i64, closed_frames: Option<u64>) {
        if self.segments.is_empty() {
            self.segments.push(SegmentMeta { file: self.file.clone(), t0_hns: self.t0_hns.unwrap_or(t0_hns), frames: closed_frames });
        } else if let Some(last) = self.segments.last_mut() {
            last.frames = closed_frames;
        }
        self.segments.push(SegmentMeta { file, t0_hns, frames: None });
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<TrackMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic: Option<TrackMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<TrackMeta>,
}

impl Tracks {
    pub fn get(&self, kind: TrackKind) -> Option<&TrackMeta> {
        match kind {
            TrackKind::Video => self.video.as_ref(),
            TrackKind::Mic => self.mic.as_ref(),
            TrackKind::System => self.system.as_ref(),
        }
    }

    pub fn get_mut(&mut self, kind: TrackKind) -> Option<&mut TrackMeta> {
        match kind {
            TrackKind::Video => self.video.as_mut(),
            TrackKind::Mic => self.mic.as_mut(),
            TrackKind::System => self.system.as_mut(),
        }
    }

    pub fn set(&mut self, kind: TrackKind, meta: TrackMeta) {
        match kind {
            TrackKind::Video => self.video = Some(meta),
            TrackKind::Mic => self.mic = Some(meta),
            TrackKind::System => self.system = Some(meta),
        }
    }

    /// 各轨起点相对最早起点的偏移（100 ns），只含有起点的轨。
    pub fn offsets_hns(&self) -> BTreeMap<TrackKind, i64> {
        let starts: Vec<(TrackKind, i64)> = TrackKind::ALL.iter().filter_map(|&k| self.get(k).and_then(|t| t.t0_hns).map(|t| (k, t))).collect();
        let Some(min) = starts.iter().map(|&(_, t)| t).min() else {
            return BTreeMap::new();
        };
        starts.into_iter().map(|(k, t)| (k, t - min)).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorMeta {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMeta {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedVideoFrames {
    pub encoder: u64,
    pub error: u64,
    pub queue: u64,
    pub resync: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatedVideoFrames {
    pub secure_desktop: u64,
    pub duplication_rebuild: u64,
    pub duplication_failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestrictedInterval {
    pub start_ms: u64,
    pub end_ms: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Diagnostics {
    pub dropped_video_frames: DroppedVideoFrames,
    pub repeated_video_frames: RepeatedVideoFrames,
    pub silence_filled_ms: u64,
    pub dropped_audio_samples: u64,
    pub device_changes: u32,
    pub encoder_restarts: u32,
    pub capture_silent_alerts: u32,
    pub large_gap_events: u32,
    pub hung_alerts: u32,
    pub low_fps_alerts: u32,
    pub restricted_intervals: Vec<RestrictedInterval>,
    /// 每条音轨时间线的完整计数，键为 `mic`、`system`。
    pub audio: BTreeMap<String, TimelineStats>,
}

impl Diagnostics {
    pub fn absorb_video(&mut self, s: &FrameStats) {
        self.dropped_video_frames = DroppedVideoFrames { encoder: s.dropped_encoder, error: s.dropped_error, queue: s.dropped_queue, resync: s.dropped_resync };
        self.repeated_video_frames =
            RepeatedVideoFrames { secure_desktop: s.repeat_secure_desktop, duplication_rebuild: s.repeat_duplication_rebuild, duplication_failed: s.repeat_duplication_failed };
    }

    /// 记入一条音轨的时间线计数，并重新汇总异常补静音时长与总丢弃采样数。
    /// `silence_filled_ms` 只统计**异常**的补静音（包间空档 + 流中断）：首包前的静音是打开设备的耗时，
    /// 空闲补齐是回环设备无声时的常态，两者都不该让“录制质量”看起来有问题，它们在 `audio` 里有各自的计数。
    pub fn absorb_audio(&mut self, track: &str, rate: u32, stats: &TimelineStats) {
        self.audio.insert(track.to_string(), stats.clone());
        let filled: u64 = self.audio.values().map(|s| s.gap_filled_frames + s.outage_filled_frames).sum();
        // 各轨采样率相同（48 kHz），总时长按同一采样率折算
        self.silence_filled_ms = filled * 1000 / u64::from(rate.max(1));
        self.dropped_audio_samples = self.audio.values().map(|s| s.dropped_frames).sum();
        self.large_gap_events = self.audio.values().map(|s| s.large_gaps as u32).sum();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,
    pub id: String,
    pub app_version: String,
    pub state: SessionState,
    #[serde(default)]
    pub end_reason: Option<EndReason>,
    pub started_utc: String,
    #[serde(default)]
    pub ended_utc: Option<String>,
    pub qpc_frequency: i64,
    #[serde(default)]
    pub monitor: Option<MonitorMeta>,
    #[serde(default)]
    pub output: Option<OutputMeta>,
    #[serde(default)]
    pub tracks: Tracks,
    #[serde(default)]
    pub diagnostics: Diagnostics,
    /// 收到停止请求时的 QPC（100 ns）。`meeting.mp4` 的时长按它裁剪（回环音轨会比停止时刻晚结束约一个设备延迟）。
    /// 崩溃的会话没有这一项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_hns: Option<i64>,
    /// 不认识的字段原样保留（子项目 B、C 会往里加转写、纪要的状态）。
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl SessionMeta {
    pub fn new(id: &str, app_version: &str, started: DateTime<Utc>, qpc_frequency: i64) -> Self {
        Self {
            version: SESSION_VERSION,
            id: id.to_string(),
            app_version: app_version.to_string(),
            state: SessionState::Recording,
            end_reason: None,
            started_utc: rfc3339(started),
            ended_utc: None,
            qpc_frequency,
            monitor: None,
            output: None,
            tracks: Tracks::default(),
            diagnostics: Diagnostics::default(),
            stopped_hns: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn finalize(&mut self, reason: EndReason, ended: DateTime<Utc>) {
        self.state = if reason == EndReason::Recovered { SessionState::Recovered } else { SessionState::Finalized };
        self.end_reason = Some(reason);
        self.ended_utc = Some(rfc3339(ended));
    }
}

pub fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 会话目录名：本地时间 `YYYY-MM-DD_HHMMSS`。
pub fn session_id(local: NaiveDateTime) -> String {
    local.format("%Y-%m-%d_%H%M%S").to_string()
}

/// 在 `base` 下建立会话目录。同一秒内重名时加 `-2`、`-3` 后缀，不覆盖已有目录。
/// 返回 `(目录路径, 实际使用的 id)`。
pub fn create_session_dir(base: &Path, id: &str) -> io::Result<(PathBuf, String)> {
    fs::create_dir_all(base)?;
    for n in 1..=99u32 {
        let name = if n == 1 { id.to_string() } else { format!("{id}-{n}") };
        let dir = base.join(&name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok((dir, name)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("同一秒内已有太多个会话目录：{id}")))
}

pub fn save_meta(dir: &Path, meta: &SessionMeta) -> io::Result<()> {
    let text = serde_json::to_string_pretty(meta).map_err(io::Error::other)?;
    let tmp = dir.join(format!("{SESSION_FILE}.tmp"));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(SESSION_FILE))
}

pub fn load_meta(dir: &Path) -> io::Result<SessionMeta> {
    let text = fs::read_to_string(dir.join(SESSION_FILE))?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}：{e}", dir.join(SESSION_FILE).display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    fn t(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 13, h, m, s).unwrap()
    }

    fn track(file: &str, t0: Option<i64>) -> TrackMeta {
        TrackMeta::new(file, t0)
    }

    #[test]
    fn id_is_local_time_in_the_documented_format() {
        let local = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap().and_hms_opt(20, 30, 0).unwrap();
        assert_eq!(session_id(local), "2026-10-13_203000");
    }

    #[test]
    fn utc_timestamps_are_rfc3339_with_z() {
        assert_eq!(rfc3339(t(12, 30, 0)), "2026-10-13T12:30:00Z");
    }

    #[test]
    fn new_session_starts_in_recording_state() {
        let m = SessionMeta::new("2026-10-13_203000", "0.1.0", t(12, 30, 0), 10_000_000);
        assert_eq!((m.state, m.end_reason, m.ended_utc.clone()), (SessionState::Recording, None, None));
        assert_eq!(m.version, 1);
    }

    #[test]
    fn finalize_sets_state_reason_and_end_time() {
        let mut m = SessionMeta::new("x", "0.1.0", t(12, 30, 0), 10_000_000);
        m.finalize(EndReason::DiskLow, t(13, 31, 12));
        assert_eq!(m.state, SessionState::Finalized);
        assert_eq!(m.end_reason, Some(EndReason::DiskLow));
        assert_eq!(m.ended_utc.as_deref(), Some("2026-10-13T13:31:12Z"));
        m.finalize(EndReason::Recovered, t(14, 0, 0));
        assert_eq!(m.state, SessionState::Recovered);
    }

    #[test]
    fn json_uses_snake_case_names_from_the_spec() {
        let mut m = SessionMeta::new("x", "0.1.0", t(12, 30, 0), 10_000_000);
        m.finalize(EndReason::MaxDuration, t(13, 0, 0));
        m.tracks.set(TrackKind::Video, track("video.mkv", Some(123)));
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        assert_eq!(v["state"], "finalized");
        assert_eq!(v["end_reason"], "max_duration");
        assert_eq!(v["tracks"]["video"]["t0_hns"], 123);
        assert_eq!(v["tracks"]["video"]["status"], "ok");
        assert!(v["tracks"].get("mic").is_none(), "没有的轨不写出");
        for k in [
            "dropped_video_frames",
            "repeated_video_frames",
            "silence_filled_ms",
            "dropped_audio_samples",
            "device_changes",
            "encoder_restarts",
            "capture_silent_alerts",
            "large_gap_events",
            "restricted_intervals",
        ] {
            assert!(v["diagnostics"].get(k).is_some(), "诊断缺少 {k}");
        }
    }

    #[test]
    fn offsets_are_relative_to_the_earliest_track() {
        let mut tr = Tracks::default();
        tr.set(TrackKind::Video, track("video.mkv", Some(1_000_500_000)));
        tr.set(TrackKind::Mic, track("mic.mka", Some(1_000_000_000)));
        tr.set(TrackKind::System, track("system.mka", None));
        let o = tr.offsets_hns();
        assert_eq!(o.get(&TrackKind::Video), Some(&500_000));
        assert_eq!(o.get(&TrackKind::Mic), Some(&0));
        assert_eq!(o.get(&TrackKind::System), None);
        assert!(Tracks::default().offsets_hns().is_empty());
    }

    #[test]
    fn diagnostics_aggregate_video_and_audio_counters() {
        let mut d = Diagnostics::default();
        let fs = FrameStats { repeat_secure_desktop: 3, repeat_duplication_rebuild: 2, dropped_queue: 5, dropped_encoder: 1, ..Default::default() };
        d.absorb_video(&fs);
        assert_eq!((d.repeated_video_frames.secure_desktop, d.repeated_video_frames.duplication_rebuild, d.dropped_video_frames.queue, d.dropped_video_frames.encoder), (3, 2, 5, 1));
        d.absorb_audio("mic", 48_000, &TimelineStats { gap_filled_frames: 48_000, dropped_frames: 10, large_gaps: 1, ..Default::default() });
        d.absorb_audio("system", 48_000, &TimelineStats { outage_filled_frames: 96_000, dropped_frames: 5, large_gaps: 2, ..Default::default() });
        assert_eq!(d.silence_filled_ms, 3_000);
        assert_eq!(d.dropped_audio_samples, 15);
        assert_eq!(d.large_gap_events, 3);
        // 同一轨再次汇总是覆盖而不是累加
        d.absorb_audio("mic", 48_000, &TimelineStats::default());
        assert_eq!(d.silence_filled_ms, 2_000);
    }

    #[test]
    fn lead_in_and_idle_silence_do_not_count_as_filled_silence() {
        let mut d = Diagnostics::default();
        let st = TimelineStats { silence_filled_frames: 200_000, lead_in_frames: 30_000, idle_filled_frames: 170_000, idle_episodes: 4, ..Default::default() };
        d.absorb_audio("system", 48_000, &st);
        assert_eq!(d.silence_filled_ms, 0);
        assert_eq!(d.audio["system"].idle_episodes, 4);
    }

    #[test]
    fn save_and_load_round_trip_with_unknown_fields_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SessionMeta::new("2026-10-13_203000", "0.1.0", t(12, 30, 0), 10_000_000);
        m.monitor = Some(MonitorMeta { name: "\\\\.\\DISPLAY1".into(), width: 3840, height: 2160 });
        m.output = Some(OutputMeta { width: 1920, height: 1080, fps: 15, encoder: "h264_nvenc".into() });
        m.tracks.set(TrackKind::Mic, track("mic.mka", Some(42)));
        m.extra.insert("transcript".into(), serde_json::json!({ "status": "pending" }));
        save_meta(dir.path(), &m).unwrap();
        let back = load_meta(dir.path()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.extra["transcript"]["status"], "pending");
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(leftovers, vec![SESSION_FILE.to_string()]);
    }

    #[test]
    fn loading_garbage_reports_the_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SESSION_FILE), "{ not json").unwrap();
        let e = load_meta(dir.path()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains(SESSION_FILE));
    }

    #[test]
    fn colliding_directory_names_get_a_suffix_and_nothing_is_overwritten() {
        let base = tempfile::tempdir().unwrap();
        let (d1, id1) = create_session_dir(base.path(), "2026-10-13_203000").unwrap();
        fs::write(d1.join("keep.txt"), "x").unwrap();
        let (d2, id2) = create_session_dir(base.path(), "2026-10-13_203000").unwrap();
        let (_, id3) = create_session_dir(base.path(), "2026-10-13_203000").unwrap();
        assert_eq!((id1.as_str(), id2.as_str(), id3.as_str()), ("2026-10-13_203000", "2026-10-13_203000-2", "2026-10-13_203000-3"));
        assert!(d1.join("keep.txt").exists() && d2.exists());
    }

    #[test]
    fn the_base_directory_is_created_on_demand() {
        let base = tempfile::tempdir().unwrap();
        let nested = base.path().join("a").join("b");
        let (dir, _) = create_session_dir(&nested, "s").unwrap();
        assert!(dir.is_dir());
    }

    #[test]
    fn track_files_match_the_documented_names() {
        let names: Vec<_> = TrackKind::ALL.iter().map(|k| k.file_name()).collect();
        assert_eq!(names, ["video.mkv", "mic.mka", "system.mka"]);
        assert_eq!(MEETING_FILE, "meeting.mp4");
    }

    #[test]
    fn segments_only_appear_in_json_after_an_encoder_restart() {
        let mut t = track("mic.mka", Some(5));
        assert!(serde_json::to_value(&t).unwrap().get("segments").is_none(), "没重启过就不写这一项，旧版本的 session.json 保持原样");
        assert_eq!(t.files(), vec!["mic.mka"]);
        t.segments = vec![SegmentMeta { file: "mic.mka".into(), t0_hns: 5, frames: None }, SegmentMeta { file: "mic.2.mka".into(), t0_hns: 900, frames: None }];
        let back: TrackMeta = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.files(), vec!["mic.mka", "mic.2.mka"]);
    }

    #[test]
    fn the_first_restart_registers_the_original_file_as_segment_one() {
        let mut t = TrackMeta::new("video.mkv", Some(1_000));
        assert_eq!(t.next_segment_file(TrackKind::Video), "video.2.mkv");
        t.push_segment("video.2.mkv".into(), 9_000, Some(40));
        assert_eq!(t.segments, vec![SegmentMeta { file: "video.mkv".into(), t0_hns: 1_000, frames: Some(40) }, SegmentMeta { file: "video.2.mkv".into(), t0_hns: 9_000, frames: None },]);
        assert_eq!(t.files(), vec!["video.mkv", "video.2.mkv"]);
        assert_eq!(t.next_segment_file(TrackKind::Video), "video.3.mkv");
    }

    #[test]
    fn a_later_restart_closes_the_previous_segment_and_audio_segments_have_no_frame_count() {
        let mut t = TrackMeta::new("mic.mka", Some(5));
        t.push_segment("mic.2.mka".into(), 50, None);
        t.push_segment("mic.3.mka".into(), 90, None);
        assert_eq!(t.segments.iter().map(|s| (s.file.as_str(), s.t0_hns, s.frames)).collect::<Vec<_>>(), vec![("mic.mka", 5, None), ("mic.2.mka", 50, None), ("mic.3.mka", 90, None)]);
    }

    #[test]
    fn a_track_that_never_got_an_anchor_starts_its_first_segment_where_the_second_begins() {
        let mut t = TrackMeta::new("system.mka", None);
        t.push_segment("system.2.mka".into(), 70, None);
        assert_eq!(t.segments[0].t0_hns, 70);
    }

    #[test]
    fn tracks_can_be_edited_in_place() {
        let mut tracks = Tracks::default();
        tracks.set(TrackKind::Mic, TrackMeta::new("mic.mka", Some(3)));
        tracks.get_mut(TrackKind::Mic).unwrap().status = TrackStatus::Interrupted;
        assert_eq!(tracks.get(TrackKind::Mic).unwrap().status, TrackStatus::Interrupted);
        assert!(tracks.get_mut(TrackKind::Video).is_none());
    }

    #[test]
    fn a_session_json_from_before_segments_existed_still_loads() {
        let old = r#"{"file":"video.mkv","t0_hns":7,"status":"ok"}"#;
        let t: TrackMeta = serde_json::from_str(old).unwrap();
        assert_eq!((t.segments.len(), t.t0_hns), (0, Some(7)));
    }
}
