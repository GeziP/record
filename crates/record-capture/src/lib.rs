//! Windows 采集层：QPC 时钟、音频设备、WASAPI 采集、显示器与画面采集。
#![cfg(windows)]

pub mod audio;
pub mod com;
pub mod cpu;
pub mod devices;
pub mod monitor;
pub mod pipeline;
pub mod qpc;
pub mod render;
pub mod testwin;
pub mod tonemap;
pub mod video;
