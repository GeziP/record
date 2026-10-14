//! 运行外壳（子项目 A）：托盘、热键、设置窗口、系统事件，把控制器接到录制引擎上。
//!
//! 纯逻辑模块（`tray_model`、`notice`、`hotkeys`、`runner`）不依赖 Tauri，可以直接单元测试；
//! `app` 是把它们装配到 Tauri 上的胶水，靠 `scripts/shell-check.ps1` 在真机上验证。
#![cfg(windows)]

pub mod hotkeys;
pub mod notice;
pub mod runner;
pub mod tray_model;
