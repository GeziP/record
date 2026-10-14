//! 配置、日志、模型的位置（设计文档 4.4）。保存目录默认在“视频”库下的 `Record`。
//!
//! 视频库可能被重定向（OneDrive 等），所以由外壳用已知文件夹 API 解析后传进来，这里不猜。

use crate::config::Config;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_file: PathBuf,
    pub log_dir: PathBuf,
    pub models_dir: PathBuf,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathsError {
    #[error("环境变量 {0} 未设置，无法确定配置与日志位置")]
    MissingEnv(&'static str),
}

impl Paths {
    /// `roaming` 对应 `%APPDATA%`，`local` 对应 `%LOCALAPPDATA%`。
    pub fn from_dirs(roaming: &Path, local: &Path) -> Self {
        Self { config_file: roaming.join("record").join("config.toml"), log_dir: local.join("record").join("logs"), models_dir: local.join("record").join("models") }
    }

    pub fn from_env_values(appdata: Option<OsString>, localappdata: Option<OsString>) -> Result<Self, PathsError> {
        let roaming = appdata.filter(|v| !v.is_empty()).ok_or(PathsError::MissingEnv("APPDATA"))?;
        let local = localappdata.filter(|v| !v.is_empty()).ok_or(PathsError::MissingEnv("LOCALAPPDATA"))?;
        Ok(Self::from_dirs(Path::new(&roaming), Path::new(&local)))
    }

    pub fn from_env() -> Result<Self, PathsError> {
        Self::from_env_values(std::env::var_os("APPDATA"), std::env::var_os("LOCALAPPDATA"))
    }
}

/// 保存目录：配置里设置了就用配置的，否则是“视频”库下的 `Record`。
pub fn resolve_output_dir(cfg: &Config, videos_dir: &Path) -> PathBuf {
    match cfg.output_dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => videos_dir.join("Record"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_follow_the_spec() {
        let p = Paths::from_dirs(Path::new(r"C:\Users\a\AppData\Roaming"), Path::new(r"C:\Users\a\AppData\Local"));
        assert_eq!(p.config_file, Path::new(r"C:\Users\a\AppData\Roaming\record\config.toml"));
        assert_eq!(p.log_dir, Path::new(r"C:\Users\a\AppData\Local\record\logs"));
        assert_eq!(p.models_dir, Path::new(r"C:\Users\a\AppData\Local\record\models"));
    }

    #[test]
    fn missing_environment_is_reported_by_name() {
        assert_eq!(Paths::from_env_values(None, Some("x".into())), Err(PathsError::MissingEnv("APPDATA")));
        assert_eq!(Paths::from_env_values(Some("x".into()), Some("".into())), Err(PathsError::MissingEnv("LOCALAPPDATA")));
        assert!(Paths::from_env_values(Some("a".into()), Some("b".into())).is_ok());
    }

    #[test]
    fn output_dir_defaults_to_the_videos_library() {
        let c = Config::default();
        assert_eq!(resolve_output_dir(&c, Path::new(r"D:\Videos")), Path::new(r"D:\Videos\Record"));
        let mut c = Config { output_dir: Some(r"E:\Records".into()), ..Config::default() };
        assert_eq!(resolve_output_dir(&c, Path::new(r"D:\Videos")), Path::new(r"E:\Records"));
        c.output_dir = Some("   ".into());
        assert_eq!(resolve_output_dir(&c, Path::new(r"D:\Videos")), Path::new(r"D:\Videos\Record"));
    }
}
