//! 定位 ffmpeg 可执行文件。顺序：环境变量 `RECORD_FFMPEG` → 程序旁边 → 开发时的 `vendor/ffmpeg/bin/`。
//! 找不到时报错并列出查找过的位置，不去 PATH 里碰运气（避免悄悄用上带 GPL 组件的别的 ffmpeg）。

use crate::MuxError;
use std::env;
use std::path::{Path, PathBuf};

pub const ENV_VAR: &str = "RECORD_FFMPEG";

pub fn find_ffmpeg() -> Result<PathBuf, MuxError> {
    let exe_dir = env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    let cwd = env::current_dir().ok();
    find_ffmpeg_in(env::var_os(ENV_VAR).map(PathBuf::from), exe_dir.as_deref(), cwd.as_deref())
}

pub fn find_ffmpeg_in(env_value: Option<PathBuf>, exe_dir: Option<&Path>, cwd: Option<&Path>) -> Result<PathBuf, MuxError> {
    let mut tried: Vec<String> = Vec::new();
    let check = |p: PathBuf, tried: &mut Vec<String>| -> Option<PathBuf> {
        if p.is_file() {
            Some(p)
        } else {
            tried.push(p.display().to_string());
            None
        }
    };

    if let Some(p) = env_value
        && let Some(found) = check(p, &mut tried)
    {
        return Ok(found);
    }
    if let Some(dir) = exe_dir
        && let Some(found) = check(dir.join("ffmpeg.exe"), &mut tried)
    {
        return Ok(found);
    }
    for start in [exe_dir, cwd].into_iter().flatten() {
        for dir in start.ancestors() {
            let candidate = dir.join("vendor").join("ffmpeg").join("bin").join("ffmpeg.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        tried.push(format!("{}\\…\\vendor\\ffmpeg\\bin\\ffmpeg.exe", start.display()));
    }
    Err(MuxError::NotFound(tried.join("；")))
}

/// 与 ffmpeg 放在一起的 ffprobe（仅测试与诊断工具使用）。
pub fn ffprobe_for(ffmpeg: &Path) -> PathBuf {
    ffmpeg.with_file_name("ffprobe.exe")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn env_variable_wins_when_the_file_exists() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("my-ffmpeg.exe");
        fs::write(&f, b"x").unwrap();
        assert_eq!(find_ffmpeg_in(Some(f.clone()), None, None).unwrap(), f);
    }

    #[test]
    fn sibling_of_the_executable_is_next() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("ffmpeg.exe");
        fs::write(&f, b"x").unwrap();
        assert_eq!(find_ffmpeg_in(None, Some(d.path()), None).unwrap(), f);
    }

    #[test]
    fn vendor_directory_is_found_by_walking_up() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("vendor").join("ffmpeg").join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("ffmpeg.exe"), b"x").unwrap();
        let deep = d.path().join("target").join("debug");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_ffmpeg_in(None, Some(&deep), None).unwrap(), bin.join("ffmpeg.exe"));
        assert_eq!(find_ffmpeg_in(None, None, Some(&deep)).unwrap(), bin.join("ffmpeg.exe"));
    }

    #[test]
    fn not_found_lists_where_it_looked() {
        let d = tempfile::tempdir().unwrap();
        let err = find_ffmpeg_in(Some(d.path().join("nope.exe")), Some(d.path()), None).unwrap_err().to_string();
        assert!(err.contains("nope.exe") && err.contains("ffmpeg.exe") && err.contains("vendor"), "{err}");
    }
}
