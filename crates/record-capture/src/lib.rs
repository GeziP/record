//! Windows 采集层：QPC 时钟、音频设备、WASAPI 采集、显示器与画面采集。
#![cfg(windows)]

pub mod com;
pub mod qpc;
