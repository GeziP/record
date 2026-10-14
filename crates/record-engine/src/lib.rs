//! 录制引擎：把采集、写入、健康监测、收尾与崩溃恢复串成“开始 / 停止”两个动作。
#![cfg(windows)]

pub mod disk;
pub mod events;
pub mod fault;

pub use events::{Alert, EngineEvent, StartError, describe_audio_event, describe_video_event};
pub use fault::Fault;
