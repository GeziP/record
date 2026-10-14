//! 真实的录制后端：把执行者的 [`Backend`] 接到 `record-engine`；以及启动时的崩溃恢复。

use crate::runner::{Backend, Progress, Saved};
use crossbeam_channel::Sender;
use record_core::config::{Config, Encoder};
use record_core::paths::resolve_output_dir;
use record_core::session::EndReason;
use record_engine::events::EngineEvent;
use record_engine::recorder::{Recorder, StartParams, TrackSelect, start};
use record_engine::recovery::{candidates, recover};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 设置界面保存配置之后，下一场录制就用新配置。
pub type SharedConfig = Arc<RwLock<Config>>;

pub struct EngineBackend {
    config: SharedConfig,
    videos_dir: PathBuf,
    recorder: Option<Recorder>,
    /// 探测编码器要起 ffmpeg 试编码，结果按（ffmpeg 路径，配置里的编码器）缓存，第二场起开始得更快。
    resolved: Option<(PathBuf, Encoder, Encoder)>,
}

impl EngineBackend {
    pub fn new(config: SharedConfig, videos_dir: PathBuf) -> Self {
        Self { config, videos_dir, recorder: None, resolved: None }
    }

    fn encoder_for(&mut self, ffmpeg: &Path, wanted: Encoder) -> Result<Encoder, String> {
        if let Some((path, asked, resolved)) = &self.resolved
            && path == ffmpeg
            && *asked == wanted
        {
            return Ok(*resolved);
        }
        let resolved = record_mux::probe::resolve_encoder(ffmpeg, wanted).map_err(|e| e.to_string())?;
        self.resolved = Some((ffmpeg.to_path_buf(), wanted, resolved));
        Ok(resolved)
    }
}

impl Backend for EngineBackend {
    fn start(&mut self, events: Sender<EngineEvent>) -> Result<PathBuf, String> {
        let config = self.config.read().map_err(|_| "配置锁已损坏".to_string())?.clone();
        let ffmpeg = record_mux::locate::find_ffmpeg().map_err(|e| e.to_string())?;
        let encoder = self.encoder_for(&ffmpeg, config.video.encoder)?;
        let output_dir = resolve_output_dir(&config, &self.videos_dir);
        let params = StartParams { config, ffmpeg, encoder, output_dir, app_version: env!("CARGO_PKG_VERSION").to_string(), tracks: TrackSelect::default(), monitor: None };
        let recorder = start(params, events).map_err(|e| e.to_string())?;
        let dir = recorder.dir().to_path_buf();
        self.recorder = Some(recorder);
        Ok(dir)
    }

    fn progress(&mut self) -> Option<Progress> {
        self.recorder.as_ref().map(|r| {
            let status = r.status();
            Progress { elapsed: status.elapsed, bytes: status.bytes }
        })
    }

    fn stop(&mut self, reason: EndReason) -> Saved {
        match self.recorder.take() {
            Some(recorder) => {
                let outcome = recorder.stop(reason);
                tracing::info!(session = %outcome.id, ?reason, secs = outcome.duration_secs, meeting = ?outcome.meeting, warnings = outcome.warnings.len(), timings = ?outcome.timings, "本场已收尾");
                for warning in &outcome.warnings {
                    tracing::warn!(session = %outcome.id, "收尾时的警告：{warning}");
                }
                Saved { dir: outcome.dir, meeting: outcome.meeting, duration_secs: outcome.duration_secs, warnings: outcome.warnings }
            }
            None => Saved { dir: PathBuf::new(), meeting: None, duration_secs: 0.0, warnings: vec!["没有进行中的录制".to_string()] },
        }
    }
}

/// 配置文件读不懂时，保存之前先把它改名留底，免得用户手改的内容被覆盖掉；返回留底文件的路径。
pub fn set_aside_broken(path: &Path) -> Option<PathBuf> {
    if Config::load(path).is_ok() {
        return None;
    }
    let backup = path.with_extension("toml.broken");
    std::fs::rename(path, &backup).ok().map(|()| backup)
}

/// 启动时修复上次崩溃、被杀、断电留下的会话（设计文档 5.8）：返回（修复成功几场，没能修复几场）。
pub fn recover_on_startup(ffmpeg: &Path, base: &Path) -> (usize, usize) {
    let (mut repaired, mut failed) = (0, 0);
    for dir in candidates(base) {
        match recover(ffmpeg, &dir) {
            Ok(Some(report)) if report.repair_errors.is_empty() && report.errors.is_empty() => {
                tracing::info!(session = %report.id, "已修复上次中断的录制：{}", dir.display());
                repaired += 1;
            }
            Ok(Some(report)) => {
                tracing::warn!(session = %report.id, repair_errors = ?report.repair_errors, errors = ?report.errors, "修复上次中断的录制时有失败：{}", dir.display());
                failed += 1;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("修复 {} 失败：{e}", dir.display());
                failed += 1;
            }
        }
    }
    (repaired, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_without_a_session_reports_it_instead_of_pretending() {
        let mut backend = EngineBackend::new(Arc::new(RwLock::new(Config::default())), PathBuf::from("."));
        let saved = backend.stop(EndReason::User);
        assert!(saved.meeting.is_none());
        assert_eq!(saved.warnings, vec!["没有进行中的录制".to_string()]);
        assert!(backend.progress().is_none());
    }

    #[test]
    fn a_config_that_cannot_be_read_is_kept_aside_before_it_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        assert_eq!(set_aside_broken(&file), None, "文件不存在：没有什么要留底");
        std::fs::write(&file, "autostart = true\n").unwrap();
        assert_eq!(set_aside_broken(&file), None, "能读懂的配置不动");
        assert!(file.exists());
        std::fs::write(&file, "hotkeys = [这不是 toml").unwrap();
        let backup = set_aside_broken(&file).expect("读不懂的要留底");
        assert!(!file.exists() && backup.exists());
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "hotkeys = [这不是 toml");
    }

    #[test]
    fn a_missing_or_empty_save_folder_has_nothing_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = Path::new("ffmpeg-that-is-never-called.exe");
        assert_eq!(recover_on_startup(ffmpeg, dir.path()), (0, 0));
        assert_eq!(recover_on_startup(ffmpeg, &dir.path().join("not-created-yet")), (0, 0));
    }
}
