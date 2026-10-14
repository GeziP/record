//! 崩溃恢复的执行（设计文档 5.8）。判定在 `record_core::recovery`，这里调 ffmpeg、改文件。
//!
//! 启动时扫描保存目录；崩溃、被杀、断电留下的会话（`state = recording` 且没有进程占着）：
//! 每个轨道文件做拷贝式修复，补做 `meeting.mp4`，标为 `recovered`。

use crate::lock;
use crate::meeting::make_meeting;
use chrono::{DateTime, Utc};
use record_core::recovery::{RecoveryPlan, plan};
use record_core::session::{EndReason, SESSION_FILE, load_meta, save_meta};
use record_mux::finalize::repair_track;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const REPAIR_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryReport {
    pub id: String,
    pub dir: PathBuf,
    pub plan: RecoveryPlan,
    /// 修复成功的轨道文件（编码器重启过的轨道含每一段）。
    pub repaired: Vec<String>,
    /// 修复失败的轨道文件：原文件原样保留。
    pub repair_errors: Vec<(String, String)>,
    pub meeting: Option<PathBuf>,
    /// 其他失败（读写 `session.json`、生成成品）。
    pub errors: Vec<String>,
}

fn has_file(dir: &Path) -> impl Fn(&str) -> bool + '_ {
    move |name| dir.join(name).is_file()
}

/// 需要恢复的会话目录：有 `session.json`、没有进程占着、计划里有事要做。读不懂的 `session.json` 不动，由调用方另行报告。
pub fn candidates(base: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(base) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(SESSION_FILE).is_file())
        .filter(|p| !lock::is_active(p))
        .filter(|p| load_meta(p).is_ok_and(|m| !plan(&m, has_file(p)).is_noop()))
        .collect();
    dirs.sort();
    dirs
}

/// 修复一个会话。没有事要做或者正被别的进程录制时返回 `Ok(None)`。
pub fn recover(ffmpeg: &Path, dir: &Path) -> std::io::Result<Option<RecoveryReport>> {
    if lock::is_active(dir) {
        return Ok(None);
    }
    let mut meta = load_meta(dir)?;
    let todo = plan(&meta, has_file(dir));
    if todo.is_noop() {
        return Ok(None);
    }
    // 结束时间取各轨文件最后写入的时刻，比“现在”更接近真实；必须在修复改写这些文件之前读
    let ended = todo.repair.iter().filter_map(|f| std::fs::metadata(dir.join(f)).and_then(|m| m.modified()).ok()).max().unwrap_or_else(SystemTime::now);
    let mut report = RecoveryReport { id: meta.id.clone(), dir: dir.to_path_buf(), plan: todo.clone(), repaired: Vec::new(), repair_errors: Vec::new(), meeting: None, errors: Vec::new() };
    for file in &todo.repair {
        match repair_track(ffmpeg, &dir.join(file), REPAIR_TIMEOUT) {
            Ok(()) => report.repaired.push(file.clone()),
            Err(e) => report.repair_errors.push((file.clone(), e.to_string())),
        }
    }
    if todo.remux {
        match make_meeting(ffmpeg, dir, &meta) {
            Ok(path) => report.meeting = path,
            Err(e) => report.errors.push(format!("生成 meeting.mp4 失败：{e}")),
        }
    }
    if todo.mark_recovered {
        meta.finalize(EndReason::Recovered, DateTime::<Utc>::from(ended));
        if let Err(e) = save_meta(dir, &meta) {
            report.errors.push(format!("写 session.json 失败：{e}"));
        }
        // 崩溃遗留的、没人占用的锁文件
        let _ = std::fs::remove_file(dir.join(lock::LOCK_FILE));
    }
    Ok(Some(report))
}

pub fn recover_all(ffmpeg: &Path, base: &Path) -> Vec<RecoveryReport> {
    candidates(base).iter().filter_map(|d| recover(ffmpeg, d).ok().flatten()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use record_core::session::{SessionMeta, SessionState, TrackKind, TrackMeta};

    fn session(base: &Path, name: &str, state: SessionState, files: &[&str]) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut m = SessionMeta::new(name, "0", Utc::now(), 10_000_000);
        m.state = state;
        for kind in TrackKind::ALL.into_iter().filter(|k| files.contains(&k.file_name())) {
            m.tracks.set(kind, TrackMeta::new(kind.file_name(), Some(0)));
        }
        save_meta(&dir, &m).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        dir
    }

    #[test]
    fn only_unfinished_or_unmixed_sessions_are_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let crashed = session(tmp.path(), "a-crashed", SessionState::Recording, &["video.mkv"]);
        let unmixed = session(tmp.path(), "b-unmixed", SessionState::Finalized, &["mic.mka"]);
        session(tmp.path(), "c-done", SessionState::Finalized, &["mic.mka", "meeting.mp4"]);
        session(tmp.path(), "d-empty", SessionState::Finalized, &[]);
        std::fs::create_dir_all(tmp.path().join("e-not-ours")).unwrap();
        assert_eq!(candidates(tmp.path()), vec![crashed, unmixed]);
    }

    #[test]
    fn a_session_another_process_is_recording_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let live = session(tmp.path(), "live", SessionState::Recording, &["video.mkv"]);
        let _lock = lock::SessionLock::acquire(&live).unwrap();
        assert!(candidates(tmp.path()).is_empty());
        assert!(matches!(recover(Path::new("ffmpeg-不存在.exe"), &live), Ok(None)));
        assert_eq!(load_meta(&live).unwrap().state, SessionState::Recording, "不能改动正在录的会话");
    }

    #[test]
    fn a_crashed_session_with_no_data_is_marked_recovered_without_needing_ffmpeg() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path(), "empty", SessionState::Recording, &[]);
        std::fs::write(dir.join(lock::LOCK_FILE), b"").unwrap();
        let report = recover(Path::new("ffmpeg-不存在.exe"), &dir).unwrap().expect("要处理");
        assert!(report.errors.is_empty() && report.repaired.is_empty() && report.meeting.is_none());
        let m = load_meta(&dir).unwrap();
        assert_eq!((m.state, m.end_reason), (SessionState::Recovered, Some(EndReason::Recovered)));
        assert!(!dir.join(lock::LOCK_FILE).exists(), "崩溃遗留的锁文件要清掉");
        assert!(candidates(tmp.path()).is_empty(), "处理过的会话不能再被扫到");
    }

    #[test]
    fn every_segment_file_of_a_restarted_track_gets_a_repair_attempt() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path(), "seg", SessionState::Recording, &["video.mkv", "video.2.mkv", "mic.mka"]);
        let mut m = load_meta(&dir).unwrap();
        let mut t = TrackMeta::new("video.mkv", Some(0));
        t.push_segment("video.2.mkv".into(), 5, Some(10));
        m.tracks.set(TrackKind::Video, t);
        save_meta(&dir, &m).unwrap();
        let report = recover(Path::new("ffmpeg-不存在.exe"), &dir).unwrap().unwrap();
        let names: Vec<&str> = report.repair_errors.iter().map(|(f, _)| f.as_str()).collect();
        assert_eq!(names, vec!["video.mkv", "video.2.mkv", "mic.mka"], "重启过的轨道每一段都要修");
    }

    #[test]
    fn a_failed_repair_keeps_the_original_file_and_reports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = session(tmp.path(), "bad", SessionState::Recording, &["mic.mka"]);
        let report = recover(Path::new("ffmpeg-不存在.exe"), &dir).unwrap().unwrap();
        assert_eq!(report.repair_errors.len(), 1);
        assert_eq!(std::fs::read(dir.join("mic.mka")).unwrap(), b"x", "修复失败时原文件原样保留");
        assert!(!report.errors.is_empty(), "也要报告成品没生成出来");
    }
}
