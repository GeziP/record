# Record

Windows 11 上的会议录屏小工具：常驻托盘，热键开始、热键保存。画面、对方的声音、我的声音分轨落盘，保存后合成 `meeting.mp4`。以后的子项目会加上本地转写与发言人区分。

状态：子项目 A（静默录屏器）实施中。设计见 `docs/superpowers/specs/`，实施计划见 `docs/superpowers/plans/`。

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
