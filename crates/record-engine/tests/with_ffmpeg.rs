//! 用真实 ffmpeg 验证收尾与崩溃恢复：不依赖采集硬件，音轨用合成的正弦波。

use chrono::{DateTime, Utc};
use record_core::config::Encoder;
use record_core::ffmpeg_args::{AudioSpec, VideoSpec};
use record_core::session::{EndReason, OutputMeta, SessionMeta, SessionState, TrackKind, TrackMeta, load_meta, save_meta};
use record_engine::lock::LOCK_FILE;
use record_engine::meeting::make_meeting;
use record_engine::recovery::{candidates, recover};
use record_mux::locate::{ffprobe_for, find_ffmpeg};
use record_mux::probe::count_video_frames;
use record_mux::writer::{AudioWriter, VideoWriter};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const RATE: u32 = 48_000;

fn ffmpeg() -> PathBuf {
    find_ffmpeg().unwrap_or_else(|e| panic!("集成测试需要 ffmpeg：{e}\n请先运行 scripts\\fetch-ffmpeg.ps1，或设置环境变量 RECORD_FFMPEG"))
}

fn probe_duration(path: &Path) -> f64 {
    let out = Command::new(ffprobe_for(&ffmpeg())).args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]).arg(path).output().expect("启动 ffprobe");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or_else(|_| panic!("{} 没有有效的时长", path.display()))
}

/// 写一个 `seconds` 秒的单声道正弦波音轨。
fn write_tone(path: &Path, seconds: f64) {
    let w = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 1 }, path, 10).unwrap();
    let frames = (seconds * f64::from(RATE)) as usize;
    let samples: Vec<f32> = (0..frames).map(|n| (2.0 * std::f64::consts::PI * 440.0 * n as f64 / f64::from(RATE)).sin() as f32 * 0.25).collect();
    for chunk in samples.chunks(480) {
        w.push(chunk);
    }
    let r = w.finish(Duration::from_secs(20));
    assert_eq!((r.exit_code, r.failed.clone()), (Some(0), None), "{r:?}");
}

fn meta_with_tracks(state: SessionState, tracks: &[(TrackKind, i64)]) -> SessionMeta {
    let mut m = SessionMeta::new("2026-01-01_000000", "0", Utc::now(), 10_000_000);
    m.state = state;
    for &(kind, t0) in tracks {
        m.tracks.set(kind, TrackMeta::new(kind.file_name(), Some(t0)));
    }
    m
}

#[test]
fn the_meeting_is_trimmed_to_the_stop_time_so_a_late_loopback_tail_does_not_stretch_it() {
    let dir = tempfile::tempdir().unwrap();
    write_tone(&dir.path().join("mic.mka"), 3.0);
    write_tone(&dir.path().join("system.mka"), 3.4); // 回环音轨比停止时刻晚结束
    let t0 = 5_000_000_000i64;
    let mut meta = meta_with_tracks(SessionState::Recording, &[(TrackKind::Mic, t0), (TrackKind::System, t0 + 10_000)]);
    meta.stopped_hns = Some(t0 + 30_000_000); // 停止时刻 = 起点后 3.0 秒
    let meeting = make_meeting(&ffmpeg(), dir.path(), &meta).unwrap().expect("有可用轨道");
    let d = probe_duration(&meeting);
    assert!((d - 3.0).abs() < 0.08, "成品应按停止时刻裁成 3.0 秒，实际 {d}");

    meta.stopped_hns = None;
    std::fs::remove_file(&meeting).unwrap();
    let untrimmed = probe_duration(&make_meeting(&ffmpeg(), dir.path(), &meta).unwrap().unwrap());
    assert!(untrimmed > 3.3, "没有停止时刻（崩溃的会话）时不裁剪，实际 {untrimmed}");
}

#[test]
fn a_crashed_session_is_repaired_remixed_and_dated_by_its_last_write() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("2026-01-01_000000");
    std::fs::create_dir_all(&dir).unwrap();
    write_tone(&dir.join("mic.mka"), 2.0);
    let meta = meta_with_tracks(SessionState::Recording, &[(TrackKind::Mic, 1_000_000_000)]);
    save_meta(&dir, &meta).unwrap();
    std::fs::write(dir.join(LOCK_FILE), b"").unwrap(); // 崩溃遗留的锁文件，没有进程占着
    // 把轨道文件的最后写入时间调到一小时前：结束时间必须取它，而不是“现在”（修复会改写文件）
    let last_write = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options().write(true).open(dir.join("mic.mka")).unwrap().set_modified(last_write).unwrap();

    assert_eq!(candidates(base.path()), vec![dir.clone()]);
    let report = recover(&ffmpeg(), &dir).unwrap().expect("要处理");
    assert_eq!((report.repaired.clone(), report.repair_errors.len(), report.errors.len()), (vec!["mic.mka".to_string()], 0, 0), "{report:?}");
    let meeting = report.meeting.expect("补做了成品");
    assert!((probe_duration(&meeting) - 2.0).abs() < 0.08);

    let m = load_meta(&dir).unwrap();
    assert_eq!((m.state, m.end_reason), (SessionState::Recovered, Some(EndReason::Recovered)));
    let ended: DateTime<Utc> = m.ended_utc.as_deref().unwrap().parse().unwrap();
    let expected: DateTime<Utc> = last_write.into();
    assert!((ended - expected).num_seconds().abs() <= 2, "结束时间 {ended} 应接近最后写入时间 {expected}");
    assert!(!dir.join(LOCK_FILE).exists());
    assert!(candidates(base.path()).is_empty(), "处理过的会话不能再被扫到");
}

/// 画面：640×360、15 帧/秒的合成帧，从全局序号 `from` 写到 `to`（不含）；`base` 是这一段的第一个序号。
fn write_video_segment(path: &Path, base: u64, from: u64, to: u64) {
    let spec = VideoSpec { width: 640, height: 360, fps: 15, encoder: Encoder::Nvenc };
    let w = VideoWriter::spawn_from(&ffmpeg(), &spec, path, 32, base).unwrap();
    for i in from..to {
        let mut f = vec![128u8; 640 * 360 * 3 / 2];
        for (n, px) in f[..640 * 360].iter_mut().enumerate() {
            *px = ((n % 640 + n / 640 + i as usize * 7) % 220 + 16) as u8;
        }
        w.submit(i, Arc::from(f));
    }
    let r = w.finish(Duration::from_secs(20));
    assert_eq!((r.exit_code, r.failed.clone()), (Some(0), None), "{r:?}");
}

/// 编码器各重启过一次的会话：画面与麦克风都是两段。
/// 画面 0–2 秒、中间丢 1 秒（冻结）、再到 5 秒；麦克风 0–2 秒、静音 1 秒、3–5 秒。
fn restarted_session(dir: &Path) -> SessionMeta {
    let t0 = 5_000_000_000i64;
    write_video_segment(&dir.join("video.mkv"), 0, 0, 30);
    write_video_segment(&dir.join("video.2.mkv"), 30, 45, 75);
    write_tone(&dir.join("mic.mka"), 2.0);
    write_tone(&dir.join("mic.2.mka"), 2.0);
    let mut m = meta_with_tracks(SessionState::Recording, &[]);
    m.output = Some(OutputMeta { width: 640, height: 360, fps: 15, encoder: "h264_nvenc".into() });
    let mut video = TrackMeta::new("video.mkv", Some(t0));
    video.push_segment("video.2.mkv".into(), t0 + 20_000_000, Some(30));
    m.tracks.set(TrackKind::Video, video);
    let mut mic = TrackMeta::new("mic.mka", Some(t0));
    mic.push_segment("mic.2.mka".into(), t0 + 30_000_000, None);
    m.tracks.set(TrackKind::Mic, mic);
    m.stopped_hns = Some(t0 + 50_000_000);
    m
}

#[test]
fn a_session_whose_encoders_were_restarted_is_joined_from_every_segment() {
    let dir = tempfile::tempdir().unwrap();
    let meta = restarted_session(dir.path());
    let meeting = make_meeting(&ffmpeg(), dir.path(), &meta).unwrap().expect("有可用轨道");
    assert!((probe_duration(&meeting) - 5.0).abs() < 0.1, "两条轨道都是 5 秒：{}", probe_duration(&meeting));
    assert_eq!(count_video_frames(&ffmpeg(), &meeting, Duration::from_secs(30)).unwrap(), 75, "画面的两段首尾相接，一帧不少");
    let decode = Command::new(ffmpeg()).args(["-v", "error", "-i"]).arg(&meeting).args(["-f", "null", "-"]).output().unwrap();
    assert!(decode.status.success() && decode.stderr.is_empty(), "成品应能无错误地完整解码：{}", String::from_utf8_lossy(&decode.stderr));
}

#[test]
fn a_segment_that_never_got_any_data_is_left_out_of_the_meeting() {
    let dir = tempfile::tempdir().unwrap();
    let mut meta = restarted_session(dir.path());
    // 画面第 1 段其实一帧也没写出来：只留第 2 段，画面从序号 0 开始（前 1 秒由第 2 段的第一帧补齐）
    std::fs::write(dir.path().join("video.mkv"), b"").unwrap();
    let video = meta.tracks.get_mut(TrackKind::Video).unwrap();
    video.segments[0].frames = Some(0);
    write_video_segment(&dir.path().join("video.2.mkv"), 0, 30, 75);
    let meeting = make_meeting(&ffmpeg(), dir.path(), &meta).unwrap().expect("有可用轨道");
    assert_eq!(count_video_frames(&ffmpeg(), &meeting, Duration::from_secs(30)).unwrap(), 75);
    assert!(!dir.path().join("video.concat.txt").exists(), "只剩一段，不需要拼接清单");
}

#[test]
fn a_crashed_session_with_restarted_tracks_gets_every_segment_repaired() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("2026-01-01_000000");
    std::fs::create_dir_all(&dir).unwrap();
    let t0 = 5_000_000_000i64;
    write_tone(&dir.join("mic.mka"), 2.0);
    // 第 2 段没来得及收尾就崩了：ffmpeg 被杀
    let w = AudioWriter::spawn(&ffmpeg(), &AudioSpec { rate: RATE, channels: 1 }, &dir.join("mic.2.mka"), 30).unwrap();
    for chunk in vec![0.1f32; 4 * RATE as usize].chunks(480) {
        w.push(chunk);
    }
    std::thread::sleep(Duration::from_millis(800));
    w.kill_process();
    let _ = w.finish(Duration::from_secs(20));
    let mut meta = meta_with_tracks(SessionState::Recording, &[]);
    let mut mic = TrackMeta::new("mic.mka", Some(t0));
    mic.push_segment("mic.2.mka".into(), t0 + 30_000_000, None);
    meta.tracks.set(TrackKind::Mic, mic);
    save_meta(&dir, &meta).unwrap();

    let report = recover(&ffmpeg(), &dir).unwrap().expect("要处理");
    assert_eq!(report.repaired, vec!["mic.mka".to_string(), "mic.2.mka".to_string()], "{report:?}");
    assert!(report.repair_errors.is_empty() && report.errors.is_empty(), "{report:?}");
    let d = probe_duration(&report.meeting.expect("补做了成品"));
    assert!(d > 3.5, "第 1 段 2 秒 + 空档 1 秒 + 第 2 段至少 0.5 秒，实际 {d}");
}

#[test]
fn a_finalized_session_that_lost_its_meeting_gets_it_back_without_touching_the_tracks() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    write_tone(&dir.join("system.mka"), 1.5);
    let before = std::fs::read(dir.join("system.mka")).unwrap();
    let mut meta = meta_with_tracks(SessionState::Finalized, &[(TrackKind::System, 1_000_000_000)]);
    meta.end_reason = Some(EndReason::User);
    save_meta(&dir, &meta).unwrap();
    let report = recover(&ffmpeg(), &dir).unwrap().expect("缺成品要补");
    assert!(report.repaired.is_empty(), "已正常收尾的轨道不修复");
    assert!(report.meeting.is_some());
    assert_eq!(std::fs::read(dir.join("system.mka")).unwrap(), before);
    assert_eq!(load_meta(&dir).unwrap().state, SessionState::Finalized, "状态保持不变");
}
