# Record

Windows 11 上的会议录屏小工具：常驻托盘，热键开始、热键保存。画面、对方的声音、我的声音分轨落盘，保存后合成 `meeting.mp4`。以后的子项目会加上本地转写与发言人区分。

状态：子项目 A（静默录屏器）已实现：常驻托盘，热键开始与保存，三轨落盘并生成 `meeting.mp4`，崩溃后自动恢复。设计见 `docs/superpowers/specs/`，实施计划见 `docs/superpowers/plans/`。哪些情形没有真机验证过，见设计文档 §10.5。

## 使用

1. 构建：`pnpm --dir app install`、`pnpm --dir app build`，再 `cargo build --release -p record-shell`，得到 `target\release\record.exe`。
2. ffmpeg：运行 `scripts\fetch-ffmpeg.ps1`，或把 LGPL 构建的 `ffmpeg.exe` 放在 `record.exe` 旁边，或设环境变量 `RECORD_FFMPEG`。
3. 运行 `record.exe`：没有窗口，只有托盘图标。第一次运行会弹出设置向导（保存位置、热键、开机自启）。
4. 默认热键：`Ctrl+Alt+R` 开始，`Ctrl+Alt+S` 保存并结束本场，`Ctrl+Alt+Q` 保存并退出程序，都可以在设置里改。
5. 每场录制一个目录，在保存位置下（默认 `<视频库>\Record`）：`video.mkv`、`mic.mka`、`system.mka`、`meeting.mp4`、`session.json`。配置在 `%APPDATA%\record\config.toml`，日志在 `%LOCALAPPDATA%\record\logs\`。
6. 录制期间，显示器和系统不会因为久不动鼠标而休眠（显示器一休眠，桌面复制就拿不到画面，成品里只剩声音）；保存或结束后恢复系统自己的设置。这只拦空闲计时：自己点“睡眠”或合上盖子照常睡眠，睡眠前本场会先保存（用窗口消息模拟验证过，真机睡眠没有验证，见设计文档 §10.5）。

## 使用边界

录会议涉及他人的声音和画面，请在当地法律和公司制度允许、并告知与会者的前提下使用。本软件不提供完全隐形模式：托盘图标始终存在，出错一定提示。

这是公开仓库：不要提交录音录像、转写稿、模型文件和密钥（`.gitignore` 已经挡住常见的文件类型）。许可证尚未确定。

## 开发

- 前置：Windows 11 x64；Rust 1.95 以上（MSVC 工具链）与 Visual Studio 2022 生成工具（C++ 工作负载）；Git。
- 采集与引擎的测试需要 ffmpeg：运行 `scripts/fetch-ffmpeg.ps1` 下载到 `vendor/ffmpeg/`（不入库）。
- 运行外壳需要 Node 20 以上、pnpm 和 WebView2 运行时。

```powershell
cargo test --workspace
```
