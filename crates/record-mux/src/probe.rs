//! 编码器探测（设计文档 5.2）：启动时探测，顺序 NVENC → QSV → AMF；都没有时明确报错，
//! 软件编码只能由用户在设置里显式选择，这里不会悄悄选它。

use crate::MuxError;
use crate::process::FfmpegProc;
use record_core::config::Encoder;
use record_core::ffmpeg_args::{count_frames_args, parse_progress_frames, probe_args};
use std::path::Path;
use std::time::Duration;

const AUTO_ORDER: [Encoder; 3] = [Encoder::Nvenc, Encoder::Qsv, Encoder::Amf];

/// 实际编一帧，成功才算可用（ffmpeg 里编进了编码器，不等于这台机器的硬件能用）。
pub fn probe_encoder(ffmpeg: &Path, enc: Encoder) -> Result<(), String> {
    let args = probe_args(enc).map_err(|e| e.to_string())?;
    let mut proc = FfmpegProc::spawn(ffmpeg, &args, false, false).map_err(|e| format!("无法启动 ffmpeg：{e}"))?;
    match proc.wait_timeout(Duration::from_secs(20)) {
        Ok(Some(status)) if status.success() => Ok(()),
        Ok(Some(_)) => Err(last_line(&proc.stderr_tail())),
        Ok(None) => {
            proc.kill();
            Err("探测超过 20 秒没有结束".into())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// 画面文件里的帧数。编码器重启时用它确定下一段从第几帧接着写。
pub fn count_video_frames(ffmpeg: &Path, file: &Path, timeout: Duration) -> Result<u64, MuxError> {
    let mut proc = FfmpegProc::spawn(ffmpeg, &count_frames_args(file), false, true).map_err(MuxError::Spawn)?;
    match proc.wait_timeout(timeout)? {
        Some(status) if status.success() => parse_progress_frames(&proc.stderr_tail()).ok_or_else(|| MuxError::Failed { code: Some(0), tail: format!("没有读到帧数：{}", proc.stderr_tail()) }),
        Some(status) => Err(MuxError::Failed { code: status.code(), tail: proc.stderr_tail() }),
        None => {
            proc.kill();
            Err(MuxError::Timeout(timeout))
        }
    }
}

fn last_line(tail: &str) -> String {
    tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("没有输出").trim().to_string()
}

/// `Encoder::Auto` 时按顺序探测；其他值只验证它能用。
pub fn resolve_encoder(ffmpeg: &Path, wanted: Encoder) -> Result<Encoder, MuxError> {
    if wanted != Encoder::Auto {
        return probe_encoder(ffmpeg, wanted).map(|()| wanted).map_err(|why| MuxError::NoEncoder(format!("{wanted:?}：{why}")));
    }
    let mut reasons = Vec::new();
    for enc in AUTO_ORDER {
        match probe_encoder(ffmpeg, enc) {
            Ok(()) => return Ok(enc),
            Err(why) => reasons.push(format!("{enc:?}：{why}")),
        }
    }
    Err(MuxError::NoEncoder(format!("{}。可以在设置里显式选择软件编码", reasons.join("；"))))
}
