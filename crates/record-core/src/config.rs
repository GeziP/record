//! `config.toml` 的数据模型、校验与读写（设计文档 16.1）。
//!
//! - 缺省的键用默认值；文件里不认识的键原样保留，升级或降级后不丢用户配置。
//! - 格式有误时返回错误，**不覆盖**原文件；由外壳提示用户，并以默认值运行。
//! - 校验返回全部问题而不是第一个，设置界面可以一次显示完。

use crate::hotkey::Hotkey;
use crate::timeline::{AudioFormat, TimeSource, TimelineConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("读取配置文件失败（{path}）：{source}")]
    Read { path: String, source: std::io::Error },
    #[error("配置文件格式有误（{path}）：{message}")]
    Parse { path: String, message: String },
    #[error("写入配置文件失败（{path}）：{source}")]
    Write { path: String, source: std::io::Error },
    #[error("配置序列化失败：{0}")]
    Serialize(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub key: &'static str,
    pub message: String,
}

fn issue(key: &'static str, message: impl Into<String>) -> ConfigIssue {
    ConfigIssue { key, message: message.into() }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 保存目录；没设置时用 `Paths` 解析出的默认目录（视频库下的 Record）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    pub hotkeys: Hotkeys,
    pub video: VideoConfig,
    pub monitor: MonitorSel,
    pub audio: AudioConfig,
    pub timeline: TimelineSettings,
    pub health: HealthSettings,
    pub notify: NotifyConfig,
    pub limits: Limits,
    pub autostart: bool,
    /// 不认识的键，原样保留（子项目 B、C 的 `[speech]`、`[llm]` 等）。
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub start: String,
    pub stop: String,
    pub quit: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self { start: "Ctrl+Alt+R".into(), stop: "Ctrl+Alt+S".into(), quit: "Ctrl+Alt+Q".into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoder {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "h264_nvenc")]
    Nvenc,
    #[serde(rename = "h264_qsv")]
    Qsv,
    #[serde(rename = "h264_amf")]
    Amf,
    /// 软件编码，只能由用户显式选择，不会悄悄降级。
    #[serde(rename = "libopenh264")]
    Software,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub fps: u32,
    pub max_long_edge: u32,
    pub max_short_edge: u32,
    pub encoder: Encoder,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self { fps: 15, max_long_edge: 1920, max_short_edge: 1080, encoder: Encoder::Auto }
    }
}

/// `"foreground"`（按下热键时前台窗口所在的显示器）或从 1 开始的显示器序号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MonitorSel {
    Named(String),
    Index(u32),
}

impl Default for MonitorSel {
    fn default() -> Self {
        MonitorSel::Named("foreground".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicRole {
    Communications,
    Console,
}

/// 系统音（回环）方案，S1 实测后决定默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemCapture {
    DefaultEndpoint,
    CommunicationsEndpoint,
    AllEndpoints,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// `"default"` 或设备 ID。
    pub mic_device: String,
    pub system_device: String,
    pub mic_role: MicRole,
    pub system_capture: SystemCapture,
    pub mic_time_source: TimeSource,
    pub system_time_source: TimeSource,
    /// 为回环另开一路静音渲染流保活（S1 对比后决定，默认关）。
    pub system_keepalive: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            mic_device: "default".into(),
            system_device: "default".into(),
            mic_role: MicRole::Communications,
            system_capture: SystemCapture::DefaultEndpoint,
            mic_time_source: TimeSource::ArrivalMinusDuration,
            system_time_source: TimeSource::DeviceQpc,
            system_keepalive: false,
        }
    }
}

/// 时间线修复的阈值，S1 调定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineSettings {
    pub tolerance_ms: u32,
    pub idle_tolerance_ms: u32,
    pub settle_ms: u32,
    pub large_gap_ms: u32,
}

impl Default for TimelineSettings {
    fn default() -> Self {
        Self { tolerance_ms: 40, idle_tolerance_ms: 100, settle_ms: 10, large_gap_ms: 5_000 }
    }
}

/// 健康监测的阈值，S1、S2、S3 调定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthSettings {
    pub silent_confirm_s: u32,
    pub hung_s: u32,
    pub low_fps_ratio: f32,
    pub low_fps_s: u32,
}

impl Default for HealthSettings {
    fn default() -> Self {
        Self { silent_confirm_s: 5, hung_s: 10, low_fps_ratio: 0.8, low_fps_s: 10 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyMode {
    Quiet,
    Standard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub mode: NotifyMode,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self { mode: NotifyMode::Quiet }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_hours: f64,
    pub min_free_gb: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_hours: 6.0, min_free_gb: 5.0 }
    }
}

/// 三个热键解析后的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySet {
    pub start: Hotkey,
    pub stop: Hotkey,
    pub quit: Hotkey,
}

impl Config {
    pub fn parse(text: &str, origin: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse { path: origin.to_string(), message: e.to_string() })
    }

    /// 文件不存在返回默认配置；格式有误返回错误，原文件原样保留。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, &path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read { path: path.display().to_string(), source }),
        }
    }

    /// 先写同目录的临时文件再改名，进程在写入中途被杀也不会留下半个配置文件。
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        let werr = |source| ConfigError::Write { path: path.display().to_string(), source };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(werr)?;
        }
        let tmp: PathBuf = path.with_extension("toml.tmp");
        let mut f = fs::File::create(&tmp).map_err(werr)?;
        f.write_all(text.as_bytes()).map_err(werr)?;
        f.sync_all().map_err(werr)?;
        drop(f);
        fs::rename(&tmp, path).map_err(werr)
    }

    pub fn hotkey_set(&self) -> Result<HotkeySet, String> {
        let parse = |name: &str, s: &str| Hotkey::parse(s).map_err(|e| format!("{name}：{e}"));
        let set = HotkeySet { start: parse("hotkeys.start", &self.hotkeys.start)?, stop: parse("hotkeys.stop", &self.hotkeys.stop)?, quit: parse("hotkeys.quit", &self.hotkeys.quit)? };
        if set.start == set.stop || set.start == set.quit || set.stop == set.quit {
            return Err("hotkeys：三个热键不能相同".to_string());
        }
        Ok(set)
    }

    pub fn timeline_config(&self, format: AudioFormat, source: TimeSource) -> TimelineConfig {
        let t = &self.timeline;
        TimelineConfig {
            time_source: source,
            tolerance_ms: t.tolerance_ms,
            idle_tolerance_ms: t.idle_tolerance_ms,
            settle_ms: t.settle_ms,
            large_gap_ms: t.large_gap_ms,
            ..TimelineConfig::new(format)
        }
    }

    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut v = Vec::new();
        if let Err(message) = self.hotkey_set() {
            v.push(issue("hotkeys", message));
        }
        if !matches!(self.video.fps, 10 | 15 | 30) {
            v.push(issue("video.fps", format!("帧率只能是 10、15 或 30，当前为 {}", self.video.fps)));
        }
        let (long, short) = (self.video.max_long_edge, self.video.max_short_edge);
        if !(240..=7_680).contains(&short) || !(240..=7_680).contains(&long) || long < short {
            v.push(issue("video.max_long_edge", format!("分辨率上限不合理：长边 {long}，短边 {short}")));
        }
        match &self.monitor {
            MonitorSel::Named(n) if n == "foreground" => {}
            MonitorSel::Named(n) => v.push(issue("monitor", format!("monitor 只能是 \"foreground\" 或显示器序号，当前为 \"{n}\""))),
            MonitorSel::Index(0) => v.push(issue("monitor", "显示器序号从 1 开始")),
            MonitorSel::Index(_) => {}
        }
        if self.audio.mic_device.trim().is_empty() || self.audio.system_device.trim().is_empty() {
            v.push(issue("audio.mic_device", "设备不能为空，用 \"default\" 表示默认设备"));
        }
        let t = &self.timeline;
        if !(5..=500).contains(&t.tolerance_ms) {
            v.push(issue("timeline.tolerance_ms", "应在 5–500 毫秒之间"));
        }
        if t.idle_tolerance_ms < t.tolerance_ms || t.idle_tolerance_ms > 1_000 {
            v.push(issue("timeline.idle_tolerance_ms", "应不小于 tolerance_ms，且不超过 1000 毫秒"));
        }
        if t.settle_ms >= t.tolerance_ms {
            v.push(issue("timeline.settle_ms", "应小于 tolerance_ms"));
        }
        if t.large_gap_ms < 1_000 {
            v.push(issue("timeline.large_gap_ms", "不应小于 1000 毫秒"));
        }
        let h = &self.health;
        if !(2..=60).contains(&h.silent_confirm_s) {
            v.push(issue("health.silent_confirm_s", "应在 2–60 秒之间"));
        }
        if !(3..=120).contains(&h.hung_s) {
            v.push(issue("health.hung_s", "应在 3–120 秒之间"));
        }
        if !(0.1..=0.95).contains(&h.low_fps_ratio) {
            v.push(issue("health.low_fps_ratio", "应在 0.1–0.95 之间"));
        }
        if !(3..=120).contains(&h.low_fps_s) {
            v.push(issue("health.low_fps_s", "应在 3–120 秒之间"));
        }
        if !(self.limits.max_hours > 0.0 && self.limits.max_hours <= 24.0) {
            v.push(issue("limits.max_hours", "应大于 0 且不超过 24 小时"));
        }
        if !(1.0..=1_000.0).contains(&self.limits.min_free_gb) {
            v.push(issue("limits.min_free_gb", "应在 1–1000 GB 之间"));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid_and_matches_the_spec() {
        let c = Config::default();
        assert!(c.validate().is_empty(), "{:?}", c.validate());
        assert_eq!((c.hotkeys.start.as_str(), c.hotkeys.stop.as_str(), c.hotkeys.quit.as_str()), ("Ctrl+Alt+R", "Ctrl+Alt+S", "Ctrl+Alt+Q"));
        assert_eq!((c.video.fps, c.video.max_long_edge, c.video.max_short_edge), (15, 1920, 1080));
        assert_eq!(c.video.encoder, Encoder::Auto);
        assert_eq!(c.monitor, MonitorSel::Named("foreground".into()));
        assert_eq!(c.audio.mic_role, MicRole::Communications);
        assert_eq!(c.health.silent_confirm_s, 5);
        assert_eq!(c.notify.mode, NotifyMode::Quiet);
        assert_eq!((c.limits.max_hours, c.limits.min_free_gb), (6.0, 5.0));
        assert!(!c.autostart);
    }

    #[test]
    fn default_config_round_trips_through_toml() {
        let c = Config::default();
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(!text.contains("output_dir"), "未设置的保存目录不应写出：\n{text}");
        assert_eq!(Config::parse(&text, "mem").unwrap(), c);
    }

    #[test]
    fn partial_files_fill_in_defaults() {
        let c = Config::parse("[video]\nfps = 30\n\n[limits]\nmax_hours = 2\n", "mem").unwrap();
        assert_eq!(c.video.fps, 30);
        assert_eq!(c.video.max_long_edge, 1920);
        assert_eq!(c.limits.max_hours, 2.0);
        assert_eq!(c.limits.min_free_gb, 5.0);
        assert_eq!(c.hotkeys, Hotkeys::default());
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let text = "autostart = true\nfuture_flag = 7\n\n[speech]\nauto_process = true\ndiarize_mic = false\n";
        let c = Config::parse(text, "mem").unwrap();
        assert!(c.autostart);
        let out = toml::to_string_pretty(&c).unwrap();
        let again = Config::parse(&out, "mem").unwrap();
        assert_eq!(again.extra.get("future_flag"), Some(&toml::Value::Integer(7)));
        let speech = again.extra.get("speech").and_then(|v| v.as_table()).expect("speech 表应保留");
        assert_eq!(speech.get("auto_process"), Some(&toml::Value::Boolean(true)));
    }

    #[test]
    fn monitor_accepts_name_or_index() {
        assert_eq!(Config::parse("monitor = \"foreground\"", "m").unwrap().monitor, MonitorSel::Named("foreground".into()));
        assert_eq!(Config::parse("monitor = 2", "m").unwrap().monitor, MonitorSel::Index(2));
        let bad = Config::parse("monitor = \"left\"", "m").unwrap();
        assert!(bad.validate().iter().any(|i| i.key == "monitor"));
        let zero = Config::parse("monitor = 0", "m").unwrap();
        assert!(zero.validate().iter().any(|i| i.key == "monitor"));
    }

    #[test]
    fn encoder_names_match_ffmpeg() {
        let c = Config::parse("[video]\nencoder = \"h264_nvenc\"\n", "m").unwrap();
        assert_eq!(c.video.encoder, Encoder::Nvenc);
        assert!(Config::parse("[video]\nencoder = \"x264\"\n", "m").is_err());
        let text = toml::to_string_pretty(&Config { video: VideoConfig { encoder: Encoder::Software, ..Default::default() }, ..Default::default() }).unwrap();
        assert!(text.contains("encoder = \"libopenh264\""), "{text}");
    }

    #[test]
    fn validation_reports_every_problem() {
        let text = "[hotkeys]\nstart = \"R\"\n\n[video]\nfps = 24\n\n[limits]\nmax_hours = 0\nmin_free_gb = 0.1\n\n[timeline]\nsettle_ms = 80\n";
        let keys: Vec<_> = Config::parse(text, "m").unwrap().validate().into_iter().map(|i| i.key).collect();
        for k in ["hotkeys", "video.fps", "limits.max_hours", "limits.min_free_gb", "timeline.settle_ms"] {
            assert!(keys.contains(&k), "缺少 {k}：{keys:?}");
        }
    }

    #[test]
    fn duplicate_hotkeys_are_rejected() {
        let c = Config::parse("[hotkeys]\nstop = \"ctrl+alt+r\"\n", "m").unwrap();
        let err = c.hotkey_set().unwrap_err();
        assert!(err.contains("不能相同"), "{err}");
    }

    #[test]
    fn hotkey_set_parses_the_defaults() {
        let s = Config::default().hotkey_set().unwrap();
        assert_eq!((s.start.to_string(), s.stop.to_string(), s.quit.to_string()), ("Ctrl+Alt+R".into(), "Ctrl+Alt+S".into(), "Ctrl+Alt+Q".into()));
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(&dir.path().join("config.toml")).unwrap(), Config::default());
    }

    #[test]
    fn broken_file_is_an_error_and_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "video = [oops").unwrap();
        let err = Config::load(&p).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(err.to_string().contains("config.toml"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "video = [oops");
    }

    #[test]
    fn save_then_load_is_lossless_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("config.toml");
        let mut c = Config { output_dir: Some("E:\\Records".into()), ..Config::default() };
        c.video.fps = 30;
        c.audio.system_capture = SystemCapture::AllEndpoints;
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
        let names: Vec<_> = fs::read_dir(p.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, vec!["config.toml".to_string()]);
    }

    #[test]
    fn timeline_settings_flow_into_the_timeline_config() {
        let mut c = Config::default();
        c.timeline.tolerance_ms = 30;
        let t = c.timeline_config(AudioFormat { rate: 48_000, channels: 2 }, TimeSource::DeviceQpc);
        assert_eq!((t.tolerance_ms, t.idle_tolerance_ms, t.settle_ms, t.large_gap_ms), (30, 100, 10, 5_000));
    }
}
