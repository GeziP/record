use record_core::ffmpeg_args::ArgsError;
use std::io;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MuxError {
    #[error("找不到 ffmpeg，已查找：{0}")]
    NotFound(String),
    #[error("启动 ffmpeg 失败：{0}")]
    Spawn(io::Error),
    #[error("ffmpeg 执行失败（退出码 {code:?}）：{tail}")]
    Failed { code: Option<i32>, tail: String },
    #[error("ffmpeg 超过 {0:?} 仍未结束，已强制结束")]
    Timeout(Duration),
    #[error("文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Args(#[from] ArgsError),
    #[error("没有可用的硬件 H.264 编码器：{0}")]
    NoEncoder(String),
}
