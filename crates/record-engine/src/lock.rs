//! 会话锁：录制期间进程独占打开会话目录里的 `.recording`（共享模式 0）。
//!
//! 进程被杀或崩溃时系统会关掉句柄，锁文件还在但没人占用；崩溃恢复靠这一点分清
//! “正在被别的进程录制”和“上次崩溃留下的半成品”，不会把正在录的会话当成崩溃去修复。

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const LOCK_FILE: &str = ".recording";
const ERROR_SHARING_VIOLATION: i32 = 32;

#[derive(Debug)]
pub struct SessionLock {
    file: Option<File>,
    path: PathBuf,
}

impl SessionLock {
    /// 创建并独占锁文件。已被别的进程占用时返回错误。
    pub fn acquire(dir: &Path) -> io::Result<Self> {
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new().write(true).create(true).truncate(true).share_mode(0).open(&path)?;
        Ok(Self { file: Some(file), path })
    }

    /// 正常结束：放手并删除锁文件。
    pub fn release(mut self) {
        self.file = None;
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 会话目录当前是否被某个进程占着录制。没有锁文件、或锁文件没人占（崩溃遗留）都是 `false`。
pub fn is_active(dir: &Path) -> bool {
    match OpenOptions::new().write(true).share_mode(0).open(dir.join(LOCK_FILE)) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(ERROR_SHARING_VIOLATION),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_makes_the_session_active_and_blocks_a_second_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = SessionLock::acquire(tmp.path()).unwrap();
        assert!(is_active(tmp.path()));
        assert!(SessionLock::acquire(tmp.path()).is_err(), "不能有两个进程同时录同一个会话");
        lock.release();
        assert!(!is_active(tmp.path()));
        assert!(!tmp.path().join(LOCK_FILE).exists(), "正常结束要删掉锁文件");
    }

    #[test]
    fn a_leftover_lock_file_nobody_holds_is_a_crash_not_an_active_session() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(LOCK_FILE), b"").unwrap();
        assert!(!is_active(tmp.path()));
    }

    #[test]
    fn a_directory_without_a_lock_file_is_not_active() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_active(tmp.path()));
    }

    #[test]
    fn dropping_the_lock_without_releasing_frees_it_like_a_crash_would() {
        let tmp = tempfile::tempdir().unwrap();
        drop(SessionLock::acquire(tmp.path()).unwrap());
        assert!(!is_active(tmp.path()));
        assert!(tmp.path().join(LOCK_FILE).exists());
    }
}
