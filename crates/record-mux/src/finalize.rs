//! 收尾与修复（设计文档 5.7、5.8）：生成 `meeting.mp4`、拷贝式修复 MKV / MKA。

use crate::MuxError;
use crate::process::FfmpegProc;
use record_core::ffmpeg_args::{FinalizePlan, display, finalize_args, meeting_part_path, repair_args};
use record_core::session::MEETING_FILE;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 运行一条 ffmpeg 命令直到结束。超时强杀；失败时带上 stderr 末尾的内容。
pub fn run(ffmpeg: &Path, args: &[OsString], timeout: Duration, low_priority: bool) -> Result<(), MuxError> {
    let cmdline = display(args);
    tracing::debug!("ffmpeg {cmdline}");
    let mut proc = FfmpegProc::spawn(ffmpeg, args, false, low_priority).map_err(MuxError::Spawn)?;
    match proc.wait_timeout(timeout)? {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(MuxError::Failed { code: status.code(), tail: proc.stderr_tail() }),
        None => {
            proc.kill();
            Err(MuxError::Timeout(timeout))
        }
    }
}

/// 生成 `meeting.mp4`：先写 `.part`，成功后改名，半成品不会被当成成品。
pub fn finalize_meeting(ffmpeg: &Path, dir: &Path, plan: &FinalizePlan, audio_rate: u32, timeout: Duration) -> Result<PathBuf, MuxError> {
    let args = finalize_args(dir, plan, audio_rate)?;
    let part = meeting_part_path(dir);
    let _ = fs::remove_file(&part);
    if let Err(e) = run(ffmpeg, &args, timeout, true) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    let out = dir.join(MEETING_FILE);
    fs::rename(&part, &out)?;
    Ok(out)
}

/// 拷贝式修复一个轨道文件，成功后替换原文件；失败时原文件原样保留。
pub fn repair_track(ffmpeg: &Path, file: &Path, timeout: Duration) -> Result<(), MuxError> {
    let mut dst = file.as_os_str().to_owned();
    dst.push(".repair");
    let dst = PathBuf::from(dst);
    let _ = fs::remove_file(&dst);
    let result = run(ffmpeg, &repair_args(file, &dst), timeout, true).and_then(|()| {
        if fs::metadata(&dst)?.len() == 0 {
            return Err(MuxError::Failed { code: Some(0), tail: "修复结果为空文件，保留原文件".into() });
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            fs::rename(&dst, file)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&dst);
            Err(e)
        }
    }
}
