//! ffmpeg 子进程层：定位、写入、收尾、修复、编码器探测（设计文档 5.6–5.8）。

pub mod error;
pub mod locate;
pub mod process;
pub mod queue;

pub use error::MuxError;
