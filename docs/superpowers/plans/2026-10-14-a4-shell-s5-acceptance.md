# 子项目 A 实施计划 4/4：外壳与 S5 spike、前端、真机检查与验收

> **给执行者：** REQUIRED SUB-SKILL: 用 superpowers:executing-plans 在当前会话内逐任务执行本计划（**不派子代理**）。步骤用复选框（`- [ ]`）跟踪。

**目标：** 做出运行外壳：常驻托盘、没有窗口，热键开始 / 保存 / 保存并退出，设置窗口按需创建，睡眠、关机与崩溃后的处理，把控制器接到录制引擎上；用 `record-s5` 先完成 S5 spike；用 `scripts/shell-check.ps1` 在真机上检查整个外壳；最后对照设计文档 §10.4 做子项目 A 的验收，并如实标出哪些没有验证。

**架构：** 纯逻辑与胶水分开：`tray_model`（托盘外观）、`notice`（通知取舍）、`hotkeys`（注册计划）、`runner`（把热键、菜单、自动保护、系统事件都排进同一个队列，由一个线程串行交给 `record-core` 的 `Controller`）都不依赖 Tauri，可以直接单元测试；`app` 是装配 Tauri 插件、托盘、热键与设置窗口的胶水，靠真机脚本验证。设置窗口用 TypeScript + Vite + React，按需创建，常驻时不占 WebView2 的内存。

**技术栈：** Rust；Tauri v2（托盘图标、全局热键、单实例、开机自启、通知插件）；TypeScript + Vite + React + Vitest；pnpm；PowerShell 检查脚本。

**本计划的位置：** 共四份计划：A1 地基 → A2 采集与 S1/S2 → A3 写入、引擎与 S3 → A4 外壳与 S5、验收（本份）。需要先完成 A3。

## 全局约束

以下条目来自设计文档 `docs/superpowers/specs/2026-10-13-meeting-recorder-design.md`，数值与措辞不要擅自改动；与它冲突时以设计文档为准，并先回头改文档。

- **平台与工具链**：Windows 11 x64（开发机为 Windows 11 build 26200）；v1 不做其他平台。Rust 1.95 以上、edition 2024；Tauri v2；前端 TypeScript + Vite + React（§2、§12）。
- **统一时钟**：所有数据用 QPC（`QueryPerformanceCounter`）打时间戳，换算为 **100 ns** 单位。`t0_session` = 热键按下时的 QPC；每轨 `t0_stream` = 该轨第一份数据的 QPC；收尾时每轨偏移 = `t0_stream` − 所有轨 `t0_stream` 的最小值（§5.1）。
- **只存原始音频**：采集阶段不降噪、不做回声消除、不做自动增益（§5.1）。
- **画面**：Desktop Duplication；恒定 **15 fps**（可设 10 / 15 / 30）；输出长边 ≤ 1920、短边 ≤ 1080，保持宽高比，宽高取偶数；缩放与 BGRA → NV12 在 GPU 上做；中途显示模式变化时按比例缩放加黑边，输出尺寸不变（§5.2）。
- **声音**：系统音走 WASAPI 回环，麦克风走 WASAPI 采集；都是 **48 kHz / float32**，系统音立体声，麦克风下混为单声道；事件驱动共享模式；采集线程是普通线程 + 事件句柄 + MMCSS（任务名 `Audio`），不用 RTWQ（§5.3、§5.4、§6.6）。
- **落盘**：每轨一个 ffmpeg 子进程，stdin 喂原始数据；`video.mkv`（H.264，无声）、`mic.mka`（FLAC）、`system.mka`（FLAC）；ffmpeg 用 LGPL 构建，开发时放在 `vendor/ffmpeg/`，不入库（§5.6）。
- **编码器**：启动时按 NVENC → QSV → AMF 探测；都没有就明确报错；软件编码只能由用户显式选择，**不悄悄降级**（§5.2、§9）。
- **时间线修复**：补静音阈值初值 40 ms、带滞回；回环空闲时每 10 ms 检查一次并补静音；单次补静音超过 5 秒记 `large_gap` 并告警；音频绝不丢，视频队列满时丢最旧帧（§5.5、§5.9）。
- **成品**：`meeting.mp4` = 画面直接拷贝 + 两轨按偏移对齐后等增益相加并限幅、编码为 AAC（1 路画面 + 1 路混音）；三轨文件在保存后几秒内即可用，`meeting.mp4` 稍后生成（§5.7）。
- **崩溃恢复**：启动时扫描 `state=recording` 的会话，对每个 MKV / MKA 做拷贝式修复，`state=recovered`，补做 `meeting.mp4`，通知用户；`state=finalized` 但缺 `meeting.mp4` 的会话补做收尾（§5.8）。
- **不静默失败**：回环“通但全是静音”、设备挂起、补静音过长、持续低帧率都要探测；告警动作是托盘变琥珀、写日志、弹提示、`session.json` 计数加一；**不自动切换采集方案**（§5.10、§9）。
- **状态机与热键**：待命 → 启动中 → 录制中 → 收尾中 → 待命；所有命令进入同一个控制器串行处理，500 ms 内的重复触发被忽略；热键默认 `Ctrl+Alt+R`（开始）、`Ctrl+Alt+S`（保存并结束本场）、`Ctrl+Alt+Q`（保存并退出程序），均可改、均幂等；三个热键常驻注册（§6.1、§6.2）。
- **静默程度**：安静模式为默认——没有弹窗和提示音，只有托盘图标变化（灰 = 待命、红点 = 录制中、琥珀 = 告警）；标准模式另加开始 / 保存的气泡；出错一定弹提示；**不提供“完全隐形”模式**（§6.3）。
- **自动保护**：单场上限 6 小时、磁盘剩余空间低于 5 GB 时自动保存并结束；睡眠 = 先保存，唤醒后提示“因睡眠已保存”；关机、注销先保存再退出（§6.5）。
- **位置**：配置 `%APPDATA%\record\config.toml`；日志 `%LOCALAPPDATA%\record\logs\`（按天滚动）；保存目录默认 `<视频库>\Record`（§4.3、§4.4）。
- **公开仓库与合规**：不提交密钥、录音录像、转写稿、模型文件和研究产物；测试产物只放 `%TEMP%\record-*` 并在结束时删除；验证画面与声音靠合成信号和统计量，不去看、不去听用户的真实录制；对第三方软件不拷贝代码（OBS GPLv2、Cap AGPLv3、Sokuji AGPL-3.0、Screenpipe 源码可见许可的项目只借鉴思路）；录制需取得与会者同意（§14）。
- **工程约定**：每个任务先写失败的测试、再实现；`rustfmt.toml` 与 `clippy -D warnings` 是质量门；提交信息用 `类型(范围): 中文说明`；不改系统环境变量。

## 前置条件

- 计划 A3 已完成：`cargo test --workspace` 全绿，`record-engine` 与命令行工具 `record-cli` 都在，`vendor/ffmpeg/` 已装好。
- 开发机需要 Node 20 以上、pnpm 和 WebView2 运行时（Windows 11 自带）。
- S5 与 `shell-check.ps1` 会模拟键盘按键、在屏幕上放窗口、弹系统通知、短暂写入并还原开机自启项、录几秒真实的桌面；运行期间不要使用这台电脑，也不要在正在开会时跑。

## 文件结构

本计划新建的文件及各自的职责（修改已有文件的地方写在各任务里）：

- `app/src-tauri/Cargo.toml`：record-shell 的依赖与可执行文件
- `app/src-tauri/build.rs`：构建脚本：前端没构建时放一个占位页
- `app/src-tauri/tauri.conf.json`：Tauri 配置：没有窗口、不打安装包
- `app/src-tauri/icons/icon.ico`
- `app/src-tauri/src/lib.rs`：模块声明
- `app/src-tauri/src/bin/s5.rs`：S5 外壳 spike：无窗口常驻、托盘图标、全局热键、单实例、开机自启与通知
- `scripts/s5-check.ps1`：S5 外壳 spike 的自动检查
- `app/src-tauri/src/tray_model.rs`：托盘图标与提示文字：由控制器状态和告警数量决定的纯函数，不碰 Tauri
- `app/src-tauri/src/notice.rs`：要不要弹气泡、弹什么：安静模式只在出错时弹，标准模式另加开始与保存
- `app/src-tauri/src/hotkeys.rs`：三个热键的注册计划与结果
- `app/src-tauri/src/runner.rs`：控制器的执行者：热键、菜单、自动保护、系统事件都变成 [`Msg`] 排进同一个队列，由一个线程串行交给 [`Controller`]，再按它返回的动作去启动、结束录制
- `app/src-tauri/src/backend.rs`：真实的录制后端：把执行者的 [`Backend`] 接到 `record-engine`；以及启动时的崩溃恢复
- `app/src-tauri/src/logging.rs`：日志：按天滚动写到 `%LOCALAPPDATA%\record\logs\record.log.<日期>`
- `app/src-tauri/src/power.rs`：睡眠、关机、注销时保存本场
- `app/src-tauri/src/app.rs`：外壳的装配：插件、托盘、热键、设置窗口，以及它们与执行者的连接
- `app/src-tauri/src/main.rs`：Record：会议录制，常驻托盘，热键开始与保存
- `app/package.json`：前端依赖与脚本
- `app/tsconfig.json`：TypeScript 配置
- `app/vite.config.ts`：Vite 与 Vitest 配置
- `app/index.html`：设置窗口的页面入口
- `app/pnpm-lock.yaml`
- `app/src/hotkey.test.ts`：热键字符串的测试
- `app/src/hotkey.ts`：键盘事件转 Tauri 热键字符串
- `app/src/types.ts`：与外壳命令对应的类型
- `app/src/api.ts`：对 Tauri invoke 的薄封装
- `app/src/Settings.tsx`：向导与完整设置界面
- `app/src/main.tsx`：前端入口
- `app/src/styles.css`：样式
- `scripts/shell-check.ps1`：外壳的真机检查：热键、托盘、系统事件、失败处理与设置窗口

---

## 任务 1：record-shell 骨架（Tauri v2）

**目标：** 运行外壳的 crate：Tauri v2、托盘与四个插件（全局热键、单实例、开机自启、通知）。窗口列表是空的——程序常驻托盘、没有窗口，设置窗口在需要时才创建。

**文件：**

- 修改：`Cargo.toml`
- 创建：`app/src-tauri/Cargo.toml` — record-shell 的依赖与可执行文件
- 创建：`app/src-tauri/build.rs` — 构建脚本：前端没构建时放一个占位页
- 创建：`app/src-tauri/tauri.conf.json` — Tauri 配置：没有窗口、不打安装包
- 创建：`app/src-tauri/icons/icon.ico`
- 创建：`app/src-tauri/src/lib.rs` — 模块声明

- [ ] **步骤 1：修改 `Cargo.toml`**

工作区成员从这里起包含外壳。

把这一段：

```toml
members = ["crates/*"]
```

改成：

```toml
members = ["crates/*", "app/src-tauri"]
```

- [ ] **步骤 2：创建 `app/src-tauri/Cargo.toml`**

先只有库，没有可执行文件；`record-s5` 与 `record` 两个可执行文件在后面的任务里加。

```toml
[package]
name = "record-shell"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish.workspace = true
build = "build.rs"

[lib]
name = "record_shell"
path = "src/lib.rs"

[build-dependencies]
tauri-build = "2"

[dependencies]
record-core = { path = "../../crates/record-core" }
record-mux = { path = "../../crates/record-mux" }
record-capture = { path = "../../crates/record-capture" }
record-engine = { path = "../../crates/record-engine" }
tauri = { version = "2", features = ["tray-icon"] }
tauri-plugin-global-shortcut = "2"
tauri-plugin-single-instance = "2"
tauri-plugin-autostart = "2"
tauri-plugin-notification = "2"
chrono.workspace = true
crossbeam-channel.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
tracing-appender.workspace = true

[target.'cfg(windows)'.dependencies.windows]
version = "0.62"
features = ["Win32_Foundation", "Win32_System_LibraryLoader", "Win32_System_Power", "Win32_UI_WindowsAndMessaging"]

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **步骤 3：创建 `app/src-tauri/build.rs`**

Tauri 编译时要求 `frontendDist` 目录存在：前端还没构建时（比如刚克隆下来先跑 `cargo test`），构建脚本放一个占位页，免得编译失败。

```rust
fn main() {
    // 前端还没构建时（比如刚克隆下来先跑 cargo test），Tauri 也要求 frontendDist 目录存在：放一个占位页
    let dist = std::path::Path::new("../dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(dist).expect("创建 app/dist");
        std::fs::write(dist.join("index.html"), "<!doctype html><meta charset=utf-8><title>Record</title><p>前端还没有构建：在 app 目录运行 pnpm install && pnpm build。</p>").expect("写占位页");
    }
    tauri_build::build()
}
```

- [ ] **步骤 4：创建 `app/src-tauri/tauri.conf.json`**

`windows` 为空：启动时不创建任何窗口。`bundle.active` 为 false：本计划只产出可执行文件，不做安装包。

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Record",
  "version": "0.1.0",
  "identifier": "io.github.gezip.record",
  "build": {
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [],
    "security": {
      "csp": null
    }
  },
  "bundle": {
    "active": false,
    "icon": ["icons/icon.ico"]
  }
}
```

- [ ] **步骤 5：创建 `app/src-tauri/icons/icon.ico`**

Tauri 在 Windows 上要一个 .ico 来生成可执行文件的资源。这是一个很小的占位图标（下面是它的 Base64，写出来就是一个 766 字节的二进制文件）；托盘图标是程序运行时按状态画出来的圆点，不用这个文件。

```powershell
New-Item -ItemType Directory -Force app\src-tauri\icons | Out-Null
[IO.File]::WriteAllBytes((Join-Path $PWD 'app\src-tauri\icons\icon.ico'), [Convert]::FromBase64String('AAABAAEAICAQAAAAAADoAgAAFgAAACgAAAAgAAAAQAAAAAEABAAAAAAAgAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAACAAAAAgIAAgAAAAIAAgACAgAAAgICAAMDAwAAAAP8AAP8AAAD//wD/AAAA/wD/AP//AAD///8AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABEREAAAAAAAAAAAAAAAAZmZmZmRAAAAAAAAAAAAAJmZmZmZmZEAAAAAAAAAABmZmZmZmZmZEAAAAAAAAAGZmZmZmZmZmZEAAAAAAAAJmZmZmZmZmZmZEAAAAAAAGZmZmZmZmZmZmZAAAAAAAZmZmZmZmZmZmZmRAAAAAAGZmZmZmZmZmZmZmQAAAAAJmZmZmZmZmZmZmZkAAAAACZmZmZmZmZmZmZmZEAAAABmZmZmZmZmZmZmZmRAAAAAZmZmZmZmZmZmZmZkQAAAAGZmZmZmZmZmZmZmZEAAAAAmZmZmZmZmZmZmZmRAAAAAJmZmZmZmZmZmZmZkAAAAAAZmZmZmZmZmZmZmRAAAAAACZmZmZmZmZmZmZkQAAAAAAGZmZmZmZmZmZmRAAAAAAAAmZmZmZmZmZmZkAAAAAAAAAmZmZmZmZmZmRAAAAAAAAAAmZmZmZmZmZEAAAAAAAAAAAGZmZmZmZEAAAAAAAAAAAAAAZmZmZEQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD///////////////////////gP///gA///gAD//wAAf/4AAD/+AAA//AAAH/wAAB/4AAAP+AAAD/gAAA/4AAAP+AAAD/gAAA/8AAAP/AAAH/wAAB/+AAA//wAAf/8AAH//wAH//+AD///8H////////////////////////////w=='))
```

预期：命令成功结束，`app\src-tauri\icons\icon.ico` 大小 766 字节。

- [ ] **步骤 6：创建 `app/src-tauri/src/lib.rs`**

先只有说明和平台限定，模块声明由后面各任务逐条加。

```rust
//! 运行外壳（子项目 A）：托盘、热键、设置窗口、系统事件，把控制器接到录制引擎上。
//!
//! 纯逻辑模块（`tray_model`、`notice`、`hotkeys`、`runner`）不依赖 Tauri，可以直接单元测试；
//! `app` 是把它们装配到 Tauri 上的胶水，靠 `scripts/shell-check.ps1` 在真机上验证。
#![cfg(windows)]
```

- [ ] **步骤 7：确认骨架能编译**

```powershell
cargo check -p record-shell
```

预期：最后一行是 `Finished`（第一次要下载并编译 Tauri 和它的依赖，需要几分钟）。

- [ ] **步骤 8：提交**

```powershell
git add -A
git commit -m "chore(shell): record-shell 骨架（Tauri v2、托盘与插件依赖）"
```

---

## 任务 2：S5 外壳 spike：无窗口常驻、托盘、热键、单实例、开机自启与通知

**目标：** 在写正式的外壳之前，先用一个小程序 `record-s5` 把设计文档 §10.1 的 S5 问清楚：程序没有窗口时能不能常驻、托盘图标能不能出来、全局热键在各种前台窗口下能不能触发、热键冲突能不能被检测到、单实例、开机自启、系统通知。`scripts/s5-check.ps1` 启动它，用模拟按键触发热键，只读它自己的日志来判定。

检查脚本会**模拟键盘按键**（Ctrl+Alt+R / S / Q / F9），并在屏幕上短暂放一个铺满屏幕的置顶窗口，运行约半分钟；期间不要动键盘和鼠标，也不要让别的程序抢焦点。它还会弹一条系统通知、临时写入再还原一条开机自启项，结束时把自己启动的进程收掉，并确认没有留下开机自启项。

其中 `Ctrl+Alt+F9` 只是探针里用来“开关一个窗口”的测试热键，产品不用它；产品的默认热键仍是 `Ctrl+Alt+R / S / Q`。最初这个测试热键用的是 `Ctrl+Alt+W`，但在作者的机器上它在探针启动前就已被别的程序占着（`RegisterHotKey` 错误码 1409；那台机器上运行着微信，它默认的“激活微信”快捷键正是这个组合，很可能就是它，但没有逐一确认持有者），“热键打开窗口”等 4 项检查因此失败。把 Ctrl+Alt+ 加 A–Z、F1–F12 逐个探测了一遍，只有 W 被占，所以换成了 F9。你的机器上如果 F9 也被占（脚本第一部分会报“启动前 Ctrl+Alt+F9 没被占用”失败），同样换一个空闲的组合，`s5.rs`（注册与判断两处）和脚本（探测表、注册检查、模拟按键三处）要一起改。

| 设计文档 §10.1 S5 的检查项 | 怎么验证 | 本任务里 |
|---|---|---|
| 热键冲突检测 | 启动前探测默认组合有没有被别的程序占着；同一组合再注册一次必须明确失败（错误码 1409） | 自动 |
| 全屏窗口在前台时热键是否生效 | 用“置顶、无边框、铺满屏幕”的窗体模拟全屏的会议软件，再按热键 | 自动（模拟）；**真正的独占全屏（游戏类）没有测** |
| 管理员权限窗口在前台时热键是否生效 | 需要先以管理员身份运行一个窗口放在前台再按热键 | **人工，未验证**（Windows 对高权限窗口有输入隔离） |
| 常驻注册 vs 按状态注册 | 本 spike 采用常驻注册（启动时一次注册，一直保持）；热键冲突、失效都由注册结果直接反映 | 自动，决策已写进设计文档 §6.2 |
| Tauri 无窗口常驻 | 无窗口启动；用热键开一个窗口再关掉，进程必须继续常驻、WebView2 子进程全部退出 | 自动 |
| 单实例 | 第二个实例必须自己退出，第一个实例收到它的参数 | 自动 |
| 开机自启 | 启用 → 停用往返，注册表 Run 项必须回到原样；**真正重启电脑后是否自启没有测** | 自动 / **人工（重启）** |
| 托盘图标 | 创建成功，资源管理器已登记；**图标的外观、右键菜单、Windows 11 把图标收进溢出区的情形要人眼看** | 自动 / **人工** |

“人工”和“未验证”的条目**不会因为脚本全绿而算数**：做完要把结果写进设计文档，做不了就在设计文档里明确写“未验证”。

**文件：**

- 修改：`app/src-tauri/Cargo.toml` — record-shell 的依赖与可执行文件
- 创建：`app/src-tauri/src/bin/s5.rs`
- 创建：`scripts/s5-check.ps1` — S5 外壳 spike 的自动检查

- [ ] **步骤 1：修改 `app/src-tauri/Cargo.toml`**

加上 `record-s5` 可执行文件。

把这一段：

```toml
[build-dependencies]
```

改成：

```toml
[[bin]]
name = "record-s5"
path = "src/bin/s5.rs"

[build-dependencies]
```

- [ ] **步骤 2：创建 `app/src-tauri/src/bin/s5.rs`**

`record-s5`：不录任何东西，只把“托盘、热键、单实例、开机自启、通知、开关窗口”各做一遍，每件事的结果带着时间写进日志。

```rust
//! S5 外壳 spike：无窗口常驻、托盘图标、全局热键、单实例、开机自启与通知。
//!
//! 用法：`record-s5 [--log 文件] [--hotkeys "Ctrl+Alt+R,Ctrl+Alt+S,Ctrl+Alt+Q"] [--secs N] [--notify] [--autostart-roundtrip]`
//!
//! 每一件事的结果都带着时间写进日志（同时打印），由脚本去检查；程序本身不录任何东西。
//! 热键 `Ctrl+Alt+F9` 额外用来开关一个窗口，用来验证“关掉窗口后进程仍然常驻”（不用 `Ctrl+Alt+W`：它常被即时通讯软件占作“激活窗口”）。

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, ShortcutState};
use tauri_plugin_notification::NotificationExt;

struct Log {
    started: Instant,
    file: Option<Mutex<std::fs::File>>,
}

impl Log {
    fn line(&self, text: impl AsRef<str>) {
        let line = format!("[{:8.3}] {}", self.started.elapsed().as_secs_f64(), text.as_ref());
        println!("{line}");
        if let Some(Ok(mut f)) = self.file.as_ref().map(|f| f.lock()) {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

/// 32×32 的纯色圆点，托盘图标用（以后按状态换颜色）。
fn dot(rgb: [u8; 3]) -> tauri::image::Image<'static> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 + 0.5 - 16.0, y as f32 + 0.5 - 16.0);
            if dx * dx + dy * dy <= 12.0 * 12.0 {
                px[(y * N + x) * 4..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    tauri::image::Image::new_owned(px, N as u32, N as u32)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let hotkeys = opt("--hotkeys").unwrap_or_else(|| "Ctrl+Alt+R,Ctrl+Alt+S,Ctrl+Alt+Q".into());
    let secs: Option<u64> = opt("--secs").and_then(|s| s.parse().ok());
    let (notify, roundtrip) = (flag("--notify"), flag("--autostart-roundtrip"));
    let file = opt("--log").map(|p| Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(p).expect("打开日志文件")));
    let log = Arc::new(Log { started: Instant::now(), file });
    log.line(format!("启动，参数 {args:?}"));

    let app = tauri::Builder::default()
        // 单实例插件必须第一个注册：第二个实例会把参数交给第一个，然后自己退出
        .plugin(tauri_plugin_single_instance::init({
            let log = Arc::clone(&log);
            move |_app, argv, cwd| log.line(format!("第二个实例被拦下，参数 {argv:?}，目录 {cwd}"))
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler({
                    let log = Arc::clone(&log);
                    move |app, shortcut, event| {
                        log.line(format!("热键 {shortcut} {:?}", event.state()));
                        if event.state() == ShortcutState::Pressed && shortcut.matches(Modifiers::CONTROL | Modifiers::ALT, Code::F9) {
                            match app.get_webview_window("probe") {
                                Some(w) => log.line(format!("关闭窗口：{:?}", w.close())),
                                None => log.line(format!(
                                    "打开窗口：{:?}",
                                    WebviewWindowBuilder::new(app, "probe", WebviewUrl::App("index.html".into())).title("Record S5 窗口").inner_size(480.0, 320.0).build().map(|_| ())
                                )),
                            }
                        }
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_notification::init())
        .setup({
            let log = Arc::clone(&log);
            move |app| {
                let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&quit])?;
                let tray = TrayIconBuilder::with_id("s5").icon(dot([220, 20, 60])).tooltip("Record S5 探测").menu(&menu).on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                });
                app.manage(tray.build(app)?);
                log.line("托盘图标已创建");

                for text in hotkeys.split(',').map(str::trim).chain(["Ctrl+Alt+F9"]) {
                    match app.global_shortcut().register(text) {
                        Ok(()) => log.line(format!("热键 {text} 注册成功")),
                        Err(e) => log.line(format!("热键 {text} 注册失败：{e}")),
                    }
                }
                if notify {
                    log.line(format!("通知：{:?}", app.notification().builder().title("Record S5").body("通知测试").show()));
                }
                if roundtrip {
                    let m = app.autolaunch();
                    log.line(format!("开机自启 起始状态：{:?}", m.is_enabled()));
                    log.line(format!("开机自启 enable：{:?}，之后：{:?}", m.enable(), m.is_enabled()));
                    log.line(format!("开机自启 disable：{:?}，之后：{:?}", m.disable(), m.is_enabled()));
                }
                if let Some(secs) = secs {
                    let (handle, log) = (app.handle().clone(), Arc::clone(&log));
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(secs));
                        log.line("到时间，退出");
                        handle.exit(0);
                    });
                }
                log.line("就绪");
                Ok(())
            }
        })
        .build(tauri::generate_context!())
        .expect("创建 Tauri 应用");

    app.run({
        let log = Arc::clone(&log);
        move |_app, event| match event {
            // 没有窗口时进程也要常驻：只有明确的 exit(code) 才放行
            RunEvent::ExitRequested { code, api, .. } if code.is_none() => {
                log.line("收到退出请求（最后一个窗口关闭），已拦下，继续常驻");
                api.prevent_exit();
            }
            RunEvent::Exit => log.line("退出"),
            _ => {}
        }
    });
}
```

- [ ] **步骤 3：创建 `scripts/s5-check.ps1`**

```powershell
# S5 外壳 spike 的自动检查：无窗口常驻、托盘、热键（含冲突与前台窗口）、单实例、开机自启、通知、内存。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\s5-check.ps1 [-Exe target\debug\record-s5.exe]
# 它会启动 record-s5，用模拟按键触发热键，只读它自己的日志；结束时把自己启动的进程收掉，并确认没有留下开机自启项。
param([string]$Exe = (Join-Path $PSScriptRoot '..\target\debug\record-s5.exe'))
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$Exe = (Resolve-Path $Exe).Path

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class S5Native {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll", SetLastError = true)] public static extern bool RegisterHotKey(IntPtr hWnd, int id, uint mods, uint vk);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr hWnd, int id);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@
Add-Type -AssemblyName System.Windows.Forms

$results = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    $results.Add([pscustomobject]@{ 项目 = $name; 结果 = $(if ($ok) { '通过' } else { '失败' }); 说明 = $detail })
    '{0}  {1}  {2}' -f $(if ($ok) { '[通过]' } else { '[失败]' }), $name, $detail
}

# 模拟 Ctrl+Alt+<key>：先按修饰键，再按主键，再倒序松开
function Press-Chord([byte]$key) {
    foreach ($vk in 0x11, 0x12, $key) { [S5Native]::keybd_event($vk, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
    foreach ($vk in $key, 0x12, 0x11) { [S5Native]::keybd_event($vk, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
}
function Read-Log { if (Test-Path $log) { Get-Content $log -Encoding UTF8 } else { @() } }
function Wait-Log([string]$pattern, [int]$sinceLine = 0, [int]$timeoutMs = 3000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do {
        $hit = @(Read-Log | Select-Object -Skip $sinceLine | Where-Object { $_ -match $pattern })
        if ($hit.Count -gt 0) { return $hit }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::Now -lt $until)
    return @()
}

$work = Join-Path $env:TEMP 'record-s5'
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
New-Item -ItemType Directory $work | Out-Null
$log = Join-Path $work 's5.log'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runBefore = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object

# ---- 0. 启动前：默认热键有没有被别的软件占着 ----
$mods = 0x0002 -bor 0x0001 -bor 0x4000   # Ctrl + Alt + NoRepeat
foreach ($k in @{ R = 0x52; S = 0x53; Q = 0x51; F9 = 0x78 }.GetEnumerator()) {
    $ok = [S5Native]::RegisterHotKey([IntPtr]::Zero, 100, $mods, $k.Value)
    $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    if ($ok) { [void][S5Native]::UnregisterHotKey([IntPtr]::Zero, 100) }
    Check "启动前 Ctrl+Alt+$($k.Key) 没被占用" $ok $(if ($ok) { '' } else { "RegisterHotKey 失败，错误码 $err（1409 表示已被别的程序注册）" })
}

# ---- 1. 启动 ----
$proc = Start-Process -FilePath $Exe -ArgumentList '--log', $log, '--notify', '--autostart-roundtrip' -PassThru
try {
    $ready = Wait-Log '就绪' 0 20000
    Check '进程启动并就绪（无窗口）' ($ready.Count -gt 0 -and -not $proc.HasExited) ''
    $lines = Read-Log
    Check '托盘图标 API 创建成功' ([bool]($lines -match '托盘图标已创建')) ''
    foreach ($h in 'Ctrl+Alt+R', 'Ctrl+Alt+S', 'Ctrl+Alt+Q', 'Ctrl+Alt+F9') {
        Check "热键 $h 注册" ([bool]($lines -match "热键 $([regex]::Escape($h)) 注册成功")) (($lines -match "热键 $([regex]::Escape($h)) 注册失败") -join '')
    }
    Check '通知 API 调用成功' ([bool]($lines -match '通知：Ok')) (($lines -match '通知：') -join '')
    $auto = @($lines -match '开机自启')
    Check '开机自启 enable → 启用，disable → 停用' ([bool]($auto -match 'enable：Ok\(\(\)\)，之后：Ok\(true\)') -and [bool]($auto -match 'disable：Ok\(\(\)\)，之后：Ok\(false\)')) ($auto -join ' | ')
    $runAfter = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
    Check '往返后注册表 Run 项与启动前一致' ((($runBefore -join ',') -eq ($runAfter -join ',')) -and -not $proc.HasExited) "前 $($runBefore.Count) 项，后 $($runAfter.Count) 项"

    # 托盘：Windows 11 为每个通知区图标在 NotifyIconSettings 下留一条记录，说明资源管理器已收下这个图标
    $tray = @(Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings' -ErrorAction SilentlyContinue | ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object { $_.ExecutablePath -eq $Exe })
    Check '资源管理器已登记托盘图标（NotifyIconSettings）' ($tray.Count -gt 0) $(if ($tray.Count) { "提示文字：$($tray[0].InitialTooltip)，是否固定在任务栏：$($tray[0].IsPromoted)" } else { '未找到记录' })

    # ---- 2. 空闲占用 ----
    Start-Sleep -Seconds 2
    $p0 = Get-Process -Id $proc.Id
    $cpu0 = $p0.TotalProcessorTime.TotalSeconds
    Start-Sleep -Seconds 10
    $p1 = Get-Process -Id $proc.Id
    $cpuPct = ($p1.TotalProcessorTime.TotalSeconds - $cpu0) / 10 * 100
    'INFO 空闲：工作集 {0:N1} MB，私有内存 {1:N1} MB，线程 {2}，10 秒内 CPU {3:N2}%' -f ($p1.WorkingSet64 / 1MB), ($p1.PrivateMemorySize64 / 1MB), $p1.Threads.Count, $cpuPct
    Check '空闲 CPU 低于 1%' ($cpuPct -lt 1.0) ('{0:N2}%' -f $cpuPct)

    # ---- 3. 热键：桌面前台 ----
    foreach ($k in @{ R = 0x52; S = 0x53; Q = 0x51 }.GetEnumerator()) {
        $n = (Read-Log).Count
        Press-Chord $k.Value
        $hit = Wait-Log "热键 .* Pressed" $n 2000
        Check "热键 Ctrl+Alt+$($k.Key) 触发（桌面前台）" ($hit.Count -gt 0) ($hit -join ' ')
    }

    # ---- 4. 热键：另一个程序的窗口在前台（置顶、无边框、铺满屏幕的窗体，模拟全屏的会议软件）----
    $form = New-Object System.Windows.Forms.Form
    $form.Text = 'S5 前台窗口'
    $form.FormBorderStyle = 'None'
    $form.WindowState = 'Maximized'
    $form.TopMost = $true
    $form.Show()
    $form.Activate()
    Start-Sleep -Milliseconds 600
    [uint32]$fgPid = 0
    [void][S5Native]::GetWindowThreadProcessId([S5Native]::GetForegroundWindow(), [ref]$fgPid)
    Check '测试窗口确实在前台' ($fgPid -eq $PID) "前台进程 $fgPid，本进程 $PID"
    $n = (Read-Log).Count
    Press-Chord 0x53
    $hit = Wait-Log "热键 .* Pressed" $n 2000
    Check '热键在铺满屏幕的置顶窗口前台时触发' ($hit.Count -gt 0) ($hit -join ' ')
    $form.Close()
    $form.Dispose()

    # ---- 5. 热键冲突：同一个组合再注册一次会明确失败 ----
    $ok = [S5Native]::RegisterHotKey([IntPtr]::Zero, 101, $mods, 0x52)
    $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
    if ($ok) { [void][S5Native]::UnregisterHotKey([IntPtr]::Zero, 101) }
    Check '组合已被占用时 RegisterHotKey 明确失败（错误码 1409）' ((-not $ok) -and $err -eq 1409) "成功=$ok，错误码=$err"

    # ---- 6. 单实例 ----
    $n = (Read-Log).Count
    $second = Start-Process -FilePath $Exe -ArgumentList '--log', (Join-Path $work 'second.log'), '--minimized' -PassThru
    $exited = $second.WaitForExit(10000)
    Check '第二个实例自己退出' $exited $(if ($exited) { "退出码 $($second.ExitCode)" } else { '10 秒内没退出' })
    $hit = Wait-Log '第二个实例被拦下' $n 3000
    Check '第一个实例收到第二个实例的参数' ($hit.Count -gt 0) ($hit -join ' ')
    Check '第一个实例仍在运行' (-not $proc.HasExited) ''

    # ---- 7. 窗口：打开后关掉，进程应继续常驻 ----
    $n = (Read-Log).Count
    Press-Chord 0x78   # F9
    $open = Wait-Log '打开窗口：Ok' $n 8000
    Check '热键打开窗口' ($open.Count -gt 0) ($open -join ' ')
    Start-Sleep -Seconds 3
    $web = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" | Where-Object { $_.CommandLine -match [regex]::Escape('record-s5') -or $_.CommandLine -match 'io.github.gezip.record' })
    $p2 = Get-Process -Id $proc.Id
    'INFO 开着窗口：主进程工作集 {0:N1} MB，私有内存 {1:N1} MB；WebView2 子进程 {2} 个，合计工作集 {3:N1} MB' -f ($p2.WorkingSet64 / 1MB), ($p2.PrivateMemorySize64 / 1MB), $web.Count, (($web | ForEach-Object { (Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue).WorkingSet64 } | Measure-Object -Sum).Sum / 1MB)
    $n = (Read-Log).Count
    [void](Get-Process -Id $proc.Id).CloseMainWindow()   # 像用户点了窗口右上角的 ×
    $intercepted = Wait-Log '已拦下' $n 5000
    Start-Sleep -Milliseconds 500
    Check '关掉最后一个窗口后进程继续常驻' ((-not $proc.HasExited) -and $intercepted.Count -gt 0) ($intercepted -join ' ')
    Start-Sleep -Seconds 4
    $webAfter = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" | Where-Object { $_.CommandLine -match 'io.github.gezip.record' })
    Check '关窗后 WebView2 子进程全部退出（常驻时不占那几百 MB）' ($webAfter.Count -eq 0) "还剩 $($webAfter.Count) 个"
    $p3 = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    if ($p3) { 'INFO 关窗后：主进程工作集 {0:N1} MB，私有内存 {1:N1} MB' -f ($p3.WorkingSet64 / 1MB), ($p3.PrivateMemorySize64 / 1MB) }
    Check '关窗后热键仍然有效' $(
        $n = (Read-Log).Count; Press-Chord 0x51; (Wait-Log "热键 .* Pressed" $n 2000).Count -gt 0
    ) ''
}
finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
    Get-Process -Name 'record-s5' -ErrorAction SilentlyContinue | Stop-Process -Force
}
$runEnd = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
Check '结束时没有留下开机自启项' (($runBefore -join ',') -eq ($runEnd -join ',')) ''
''
'汇总：通过 {0} 项，失败 {1} 项' -f @($results | Where-Object { $_.结果 -eq '通过' }).Count, @($results | Where-Object { $_.结果 -eq '失败' }).Count
'日志：' + $log
```

这个文件必须是 UTF-8 **带 BOM**，见下一步。

- [ ] **步骤 4：给 `scripts/s5-check.ps1` 补上 UTF-8 BOM**

Windows PowerShell 5.1 读含中文的脚本要靠 BOM，否则会按系统代码页解码成乱码。

```powershell
$p = 'scripts\s5-check.ps1'; [IO.File]::WriteAllText($p, [IO.File]::ReadAllText($p), (New-Object Text.UTF8Encoding($true)))
```

预期：命令成功结束（退出码 0）。

- [ ] **步骤 5：编译 record-s5**

```powershell
cargo build -p record-shell --bin record-s5
```

预期：最后一行是 `Finished`（第一次要编译整个 Tauri，几分钟）。

- [ ] **步骤 6：运行 S5 检查脚本**

约半分钟（第一次编译之后）。期间会模拟按键、放一个全屏置顶窗口、弹一条通知；不要动键盘鼠标。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\s5-check.ps1
```

预期：每项一行 `[通过]`，没有 `[失败]`，最后 `汇总：通过 29 项，失败 0 项`。中间有几行 `INFO`，是空闲与开着窗口时的内存和 CPU 数字，要记进设计文档。参考（本机）：空闲时工作集约 24 MB、私有内存约 4 MB、10 秒内 CPU 0.00%；开着设置窗口时主进程约 34 MB，另有 6 个 WebView2 子进程合计约 356～375 MB（两次测量）；关窗后子进程全部退出，主进程仍约 34 MB。**有 `[失败]` 就先看它后面的说明：热键被别的程序占着、通知被系统勿扰模式吞掉都是环境原因，处理环境，不要删检查项、不要放宽判断来让它通过；只有探针里用来开关窗口的测试热键 `Ctrl+Alt+F9` 被占着时，才可以换一个空闲的组合（见上面的说明）。产品的 `Ctrl+Alt+R / S / Q` 被占着是要如实报告的发现，不能这样绕。**

**核对设计文档里的结论**（§6.2 的“S5 结果”、§10.5 S5 一行、§12）：热键默认值 `Ctrl+Alt+R / S / Q` 保持；注册策略用“常驻注册”；管理员权限窗口在前台时热键是否生效、独占全屏、重启后自启、托盘外观这几项没有自动覆盖，写“未验证”；Windows 11 默认把新托盘图标收进溢出区，首次运行向导里要提示用户把它拖到任务栏；WebView2 只在设置窗口打开期间占内存（脚本里的 `INFO` 行），常驻时不占。作者已经按这些写好了；你这次的数字不同就改成你的，并写明日期（用 `Get-Date` 取）。

- [ ] **步骤 7：提交**

```powershell
git add -A
git commit -m "docs(spike): S5 外壳 spike"
```

设计文档里已经有这一步的结论。如果你上面核对后改了文档，这次提交会带上改动；没改的话 git 会说 `nothing to commit`，这是正常的，直接做下一步，不要为了提交而提交。

---

## 任务 3：record-shell：tray_model

**目标：** 托盘图标与提示文字（设计文档 6.3）：由控制器状态和告警数量决定的纯函数，不碰 Tauri。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/tray_model.rs`

**接口：**

- 依赖：`record_core::controller::State`
- 提供：`pub enum Look`；`pub fn look(state: State, warnings: usize) -> Look`；`pub fn rgb(look: Look) -> [u8; 3]`；`pub const ICON_SIZE: u32`；`pub fn icon_rgba(rgb: [u8; 3]) -> Vec<u8>`；`pub struct MenuEnabled`；`pub fn menu_enabled(state: State) -> MenuEnabled`；`pub fn format_elapsed(elapsed: Duration) -> String`；`pub fn format_size(bytes: u64) -> String`；`pub const TOOLTIP_MAX_CHARS: usize`；`pub fn tooltip(state: State, elapsed: Duration, bytes: u64, warnings: &[String]) -> String`
- 测试：6 个：`the_icon_is_grey_when_idle_red_when_recording_and_amber_with_warnings`、`the_icon_is_a_coloured_dot_on_a_transparent_square`、`only_the_applicable_menu_items_are_enabled`、`elapsed_time_and_size_are_short_and_readable`、`the_tooltip_shows_progress_and_the_latest_warning_only_while_a_session_is_on`、`a_long_warning_is_cut_to_what_the_tray_can_show`

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod tray_model;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/tray_model.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_grey_when_idle_red_when_recording_and_amber_with_warnings() {
        assert_eq!(look(State::Idle, 0), Look::Standby);
        assert_eq!(look(State::Idle, 3), Look::Standby, "待命时不显示上一场的告警");
        assert_eq!(look(State::Recording, 0), Look::Recording);
        assert_eq!(look(State::Recording, 1), Look::Warning);
        assert_eq!(look(State::Finalizing, 0), Look::Recording);
        assert_eq!(look(State::Starting, 2), Look::Warning);
        let colors = [rgb(Look::Standby), rgb(Look::Recording), rgb(Look::Warning)];
        assert!(colors[0] != colors[1] && colors[1] != colors[2] && colors[0] != colors[2]);
    }

    #[test]
    fn the_icon_is_a_coloured_dot_on_a_transparent_square() {
        let px = icon_rgba(rgb(Look::Recording));
        let n = ICON_SIZE as usize;
        assert_eq!(px.len(), n * n * 4);
        let at = |x: usize, y: usize| &px[(y * n + x) * 4..][..4];
        assert_eq!(at(n / 2, n / 2), &[220, 20, 60, 255]);
        assert_eq!(at(0, 0)[3], 0, "角上是透明的");
        assert_eq!(at(n - 1, n - 1)[3], 0);
    }

    #[test]
    fn only_the_applicable_menu_items_are_enabled() {
        assert_eq!(menu_enabled(State::Idle), MenuEnabled { start: true, stop: false });
        assert_eq!(menu_enabled(State::Recording), MenuEnabled { start: false, stop: true });
        assert_eq!(menu_enabled(State::Starting), MenuEnabled { start: false, stop: false });
        assert_eq!(menu_enabled(State::Finalizing), MenuEnabled { start: false, stop: false });
    }

    #[test]
    fn elapsed_time_and_size_are_short_and_readable() {
        let secs = Duration::from_secs;
        assert_eq!(format_elapsed(secs(0)), "0:00");
        assert_eq!(format_elapsed(secs(65)), "1:05");
        assert_eq!(format_elapsed(secs(3599)), "59:59");
        assert_eq!(format_elapsed(secs(3600)), "1:00:00");
        assert_eq!(format_elapsed(secs(45_296)), "12:34:56");
        assert_eq!(format_size(0), "0 MB");
        assert_eq!(format_size(5 * 1_048_576), "5 MB");
        assert_eq!(format_size(1_610_612_736), "1.50 GB");
    }

    #[test]
    fn the_tooltip_shows_progress_and_the_latest_warning_only_while_a_session_is_on() {
        assert_eq!(tooltip(State::Idle, Duration::ZERO, 0, &[]), "Record：待命");
        assert_eq!(tooltip(State::Recording, Duration::from_secs(125), 150 * 1_048_576, &[]), "Record：录制中 2:05，已写入 150 MB");
        let w = vec!["麦克风中断了".to_string(), "画面帧率偏低".to_string()];
        assert_eq!(tooltip(State::Recording, Duration::from_secs(1), 0, &w), "Record：录制中 0:01，已写入 0 MB\n注意：画面帧率偏低");
        assert_eq!(tooltip(State::Idle, Duration::ZERO, 0, &w), "Record：待命");
        assert_eq!(tooltip(State::Finalizing, Duration::ZERO, 0, &[]), "Record：正在保存…");
    }

    #[test]
    fn a_long_warning_is_cut_to_what_the_tray_can_show() {
        let long = vec!["很长的告警".repeat(40)];
        let t = tooltip(State::Recording, Duration::from_secs(1), 0, &long);
        assert_eq!(t.chars().count(), TOOLTIP_MAX_CHARS);
        assert!(t.ends_with('…'));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-shell --lib tray_model::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `app/src-tauri/src/tray_model.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 托盘图标与提示文字（设计文档 6.3）：由控制器状态和告警数量决定的纯函数，不碰 Tauri。

use record_core::controller::State;
use std::time::Duration;

/// 托盘图标的样子：灰（待命）、红点（录制中）、琥珀（录制中但有告警）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Standby,
    Recording,
    Warning,
}

pub fn look(state: State, warnings: usize) -> Look {
    match state {
        State::Idle => Look::Standby,
        _ if warnings > 0 => Look::Warning,
        _ => Look::Recording,
    }
}

pub fn rgb(look: Look) -> [u8; 3] {
    match look {
        Look::Standby => [136, 136, 136],
        Look::Recording => [220, 20, 60],
        Look::Warning => [255, 176, 0],
    }
}

pub const ICON_SIZE: u32 = 32;

/// 纯色圆点（RGBA，`ICON_SIZE` 见方），托盘图标用；圆外透明。
pub fn icon_rgba(rgb: [u8; 3]) -> Vec<u8> {
    let n = ICON_SIZE as usize;
    let center = n as f32 / 2.0;
    let radius = center - 4.0;
    let mut pixels = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
            if dx * dx + dy * dy <= radius * radius {
                pixels[(y * n + x) * 4..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    pixels
}

/// 托盘菜单里“开始”“保存并结束”能不能点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuEnabled {
    pub start: bool,
    pub stop: bool,
}

pub fn menu_enabled(state: State) -> MenuEnabled {
    MenuEnabled { start: state == State::Idle, stop: state == State::Recording }
}

/// `1:05`、`1:00:00`。
pub fn format_elapsed(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

pub fn format_size(bytes: u64) -> String {
    const MB: f64 = 1_048_576.0;
    const GB: f64 = 1_073_741_824.0;
    let b = bytes as f64;
    if b >= GB { format!("{:.2} GB", b / GB) } else { format!("{:.0} MB", b / MB) }
}

/// Windows 托盘提示最多 127 个字符：留一点余量，多出的部分换成省略号。
pub const TOOLTIP_MAX_CHARS: usize = 120;

pub fn tooltip(state: State, elapsed: Duration, bytes: u64, warnings: &[String]) -> String {
    let head = match state {
        State::Idle => "Record：待命".to_string(),
        State::Starting => "Record：正在启动…".to_string(),
        State::Recording => format!("Record：录制中 {}，已写入 {}", format_elapsed(elapsed), format_size(bytes)),
        State::Finalizing => "Record：正在保存…".to_string(),
    };
    let text = match warnings.last() {
        Some(w) if state != State::Idle => format!("{head}\n注意：{w}"),
        _ => head,
    };
    if text.chars().count() > TOOLTIP_MAX_CHARS { text.chars().take(TOOLTIP_MAX_CHARS - 1).collect::<String>() + "…" } else { text }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-shell --lib tray_model::tests::
```

预期：通过，输出里有 `test result: ok. 6 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(shell): 托盘图标与提示文字（纯逻辑）"
```

---

## 任务 4：record-shell：notice

**目标：** 要不要弹气泡、弹什么（设计文档 6.3）：安静模式只在出错时弹，标准模式另加开始与保存。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/notice.rs`

**接口：**

- 依赖：`crate::tray_model::format_elapsed`、`record_core::config::NotifyMode`、`record_core::session::EndReason`、`record_engine::events::Alert`
- 提供：`pub enum Notice`；`Notice::always`；`Notice::text`；`pub fn should_show(mode: NotifyMode, notice: &Notice) -> bool`
- 测试：3 个：`quiet_mode_shows_only_what_went_wrong`、`standard_mode_shows_everything`、`the_texts_carry_what_the_user_needs`

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod notice;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/notice.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use record_core::session::TrackKind;

    fn saved(reason: EndReason) -> Notice {
        Notice::Saved { reason, meeting: Some(r"D:\rec\a\meeting.mp4".into()), duration_secs: 3725.0, warnings: 0 }
    }

    #[test]
    fn quiet_mode_shows_only_what_went_wrong() {
        let quiet = |n: &Notice| should_show(NotifyMode::Quiet, n);
        assert!(!quiet(&Notice::Started { dir: "x".into() }));
        assert!(!quiet(&saved(EndReason::User)), "用户自己按的保存不用再提示");
        assert!(!quiet(&Notice::Alert(Alert::HdrActive)));
        assert!(!quiet(&Notice::Alert(Alert::HungRecovered { track: TrackKind::Mic })));
        for reason in [EndReason::MaxDuration, EndReason::DiskLow, EndReason::Sleep, EndReason::Shutdown, EndReason::Logoff, EndReason::WriteError, EndReason::CaptureFailure] {
            assert!(quiet(&saved(reason)), "{reason:?} 不是用户触发的，必须提示");
        }
        assert!(quiet(&Notice::StartFailed("磁盘不可写".into())));
        assert!(quiet(&Notice::Alert(Alert::TrackInterrupted { track: TrackKind::System })));
        assert!(quiet(&Notice::HotkeyConflict(vec!["开始录制".into()])));
        assert!(quiet(&Notice::Recovered { count: 1, failed: 0 }));
        assert!(quiet(&Notice::ConfigInvalid("第 3 行".into())));
    }

    #[test]
    fn standard_mode_shows_everything() {
        for n in [Notice::Started { dir: "x".into() }, saved(EndReason::User), Notice::Alert(Alert::HdrActive)] {
            assert!(should_show(NotifyMode::Standard, &n));
        }
    }

    #[test]
    fn the_texts_carry_what_the_user_needs() {
        let (title, body) = saved(EndReason::Sleep).text();
        assert_eq!(title, "因睡眠已保存");
        assert_eq!(body, r"时长 1:02:05，D:\rec\a\meeting.mp4");
        let (_, body) = Notice::Saved { reason: EndReason::User, meeting: None, duration_secs: 5.0, warnings: 2 }.text();
        assert!(body.contains("没有生成成品") && body.contains("2 条告警"), "{body}");
        let (title, body) = Notice::Alert(Alert::Hung { track: TrackKind::Video, secs: 12 }).text();
        assert_eq!(title, "录制出了问题");
        assert!(body.contains("画面") && body.contains("12"), "{body}");
        assert_eq!(Notice::Alert(Alert::HdrActive).text().0, "录制提示");
        let (_, body) = Notice::HotkeyConflict(vec!["开始录制（Ctrl+Alt+R）：被占用".into(), "保存（Ctrl+Alt+S）：被占用".into()]).text();
        assert!(body.contains("开始录制") && body.contains("保存") && body.contains("设置"), "{body}");
        assert!(Notice::Recovered { count: 2, failed: 1 }.text().1.contains("另有 1 场"));
        assert!(Notice::StartFailed("没有找到显示器".into()).text().1.contains("显示器"));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-shell --lib notice::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `app/src-tauri/src/notice.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 要不要弹气泡、弹什么（设计文档 6.3）：安静模式只在出错时弹，标准模式另加开始与保存。

use crate::tray_model::format_elapsed;
use record_core::config::NotifyMode;
use record_core::session::EndReason;
use record_engine::events::Alert;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Started {
        dir: String,
    },
    Saved {
        reason: EndReason,
        meeting: Option<String>,
        duration_secs: f64,
        warnings: usize,
    },
    StartFailed(String),
    Alert(Alert),
    /// 每个没注册成功的热键一行。
    HotkeyConflict(Vec<String>),
    /// 启动时修复了上次中断的录制。
    Recovered {
        count: usize,
        failed: usize,
    },
    ConfigInvalid(String),
}

impl Notice {
    /// 出错一定弹提示，不受安静模式影响；开始、以及用户自己触发的保存，只在标准模式弹。
    pub fn always(&self) -> bool {
        match self {
            Notice::Started { .. } => false,
            Notice::Saved { reason, .. } => *reason != EndReason::User,
            Notice::Alert(a) => a.is_warning(),
            Notice::StartFailed(_) | Notice::HotkeyConflict(_) | Notice::Recovered { .. } | Notice::ConfigInvalid(_) => true,
        }
    }

    /// （标题，正文）
    pub fn text(&self) -> (String, String) {
        match self {
            Notice::Started { dir } => ("开始录制".into(), format!("保存到 {dir}")),
            Notice::Saved { reason, meeting, duration_secs, warnings } => {
                let mut body = format!("时长 {}", format_elapsed(Duration::from_secs_f64(*duration_secs)));
                match meeting {
                    Some(path) => body.push_str(&format!("，{path}")),
                    None => body.push_str("，没有生成成品，原始轨道文件还在会话目录里"),
                }
                if *warnings > 0 {
                    body.push_str(&format!("；本场有 {warnings} 条告警，详见日志"));
                }
                (reason.describe().to_string(), body)
            }
            Notice::StartFailed(why) => ("没能开始录制".into(), why.clone()),
            Notice::Alert(a) => (if a.is_warning() { "录制出了问题" } else { "录制提示" }.into(), a.describe()),
            Notice::HotkeyConflict(lines) => ("有热键没注册成功".into(), format!("{}。可以在设置里换一个组合", lines.join("；"))),
            Notice::Recovered { count, failed } => {
                let mut body = format!("已修复并生成 {count} 场上次中断的录制");
                if *failed > 0 {
                    body.push_str(&format!("，另有 {failed} 场没能修复，详见日志"));
                }
                ("已恢复上次中断的录制".into(), body)
            }
            Notice::ConfigInvalid(why) => ("配置文件有误，这次按默认值运行".into(), format!("{why}。原文件没有被改动")),
        }
    }
}

pub fn should_show(mode: NotifyMode, notice: &Notice) -> bool {
    notice.always() || mode == NotifyMode::Standard
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-shell --lib notice::tests::
```

预期：通过，输出里有 `test result: ok. 3 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(shell): 气泡通知的取舍（安静模式只在出错时弹）"
```

---

## 任务 5：record-shell：hotkeys

**目标：** 三个热键的注册计划与结果（设计文档 6.2）。注册本身在 `app.rs`，这里只有可以脱离 Tauri 测试的部分。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/hotkeys.rs`

**接口：**

- 依赖：`record_core::config::Config`、`record_core::controller::Command`
- 提供：`pub enum HotkeyStatus`；`pub struct HotkeyEntry`；`pub fn command_name(command: Command) -> &'static str`；`pub fn wanted(config: &Config) -> Result<Vec<(Command, String, String)>, String>`；`pub fn menu_label(command: Command, entry: Option<&HotkeyEntry>) -> String`；`pub fn conflict_lines(entries: &[HotkeyEntry]) -> Vec<String>`；`pub fn explain_failure(raw: &str) -> String`
- 测试：6 个：`the_defaults_become_three_plugin_strings`、`the_windows_key_is_spelled_the_way_the_plugin_wants`、`bad_or_duplicate_hotkeys_are_explained_instead_of_registered`、`the_menu_shows_the_hotkey_and_flags_the_ones_that_failed`、`hotkeys_that_failed_for_the_same_reason_share_one_line`、`the_plugins_english_error_is_translated_when_it_means_the_combination_is_taken`

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod hotkeys;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/hotkeys.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_become_three_plugin_strings() {
        let w = wanted(&Config::default()).unwrap();
        assert_eq!(w.iter().map(|(c, ..)| *c).collect::<Vec<_>>(), vec![Command::Start, Command::Stop, Command::Quit]);
        assert_eq!(w.iter().map(|(_, text, _)| text.as_str()).collect::<Vec<_>>(), vec!["Ctrl+Alt+R", "Ctrl+Alt+S", "Ctrl+Alt+Q"]);
    }

    #[test]
    fn the_windows_key_is_spelled_the_way_the_plugin_wants() {
        let mut c = Config::default();
        c.hotkeys.start = "alt+win+9".into();
        let w = wanted(&c).unwrap();
        assert_eq!((w[0].1.as_str(), w[0].2.as_str()), ("Alt+Win+9", "Alt+Super+9"));
    }

    #[test]
    fn bad_or_duplicate_hotkeys_are_explained_instead_of_registered() {
        let mut c = Config::default();
        c.hotkeys.stop = c.hotkeys.start.clone();
        assert!(wanted(&c).unwrap_err().contains("不能相同"));
        c.hotkeys.stop = "S".into();
        assert!(wanted(&c).unwrap_err().contains("hotkeys.stop"));
    }

    #[test]
    fn the_menu_shows_the_hotkey_and_flags_the_ones_that_failed() {
        let ok = HotkeyEntry { command: Command::Start, text: "Ctrl+Alt+R".into(), status: HotkeyStatus::Registered };
        let bad = HotkeyEntry { command: Command::Stop, text: "Ctrl+Alt+S".into(), status: HotkeyStatus::Failed("x".into()) };
        assert_eq!(menu_label(Command::Start, Some(&ok)), "开始录制（Ctrl+Alt+R）");
        assert_eq!(menu_label(Command::Stop, Some(&bad)), "保存并结束本场（Ctrl+Alt+S 没注册成功，被占用）");
        assert_eq!(menu_label(Command::Quit, None), "保存并退出程序");
        assert_eq!(conflict_lines(&[ok, bad]), vec!["保存并结束本场（Ctrl+Alt+S）：x".to_string()]);
    }

    #[test]
    fn hotkeys_that_failed_for_the_same_reason_share_one_line() {
        let failed = |command, text: &str, why: &str| HotkeyEntry { command, text: text.into(), status: HotkeyStatus::Failed(why.into()) };
        let all_wrong = [failed(Command::Start, "R", "没有修饰键"), failed(Command::Stop, "Ctrl+Alt+S", "没有修饰键"), failed(Command::Quit, "Ctrl+Alt+Q", "没有修饰键")];
        assert_eq!(conflict_lines(&all_wrong), vec!["开始录制（R）、保存并结束本场（Ctrl+Alt+S）、保存并退出程序（Ctrl+Alt+Q）：没有修饰键".to_string()]);
        let mixed = [failed(Command::Start, "Ctrl+Alt+R", "被占用"), failed(Command::Stop, "Ctrl+Alt+S", "不认识"), failed(Command::Quit, "Ctrl+Alt+Q", "被占用")];
        assert_eq!(conflict_lines(&mixed), vec!["开始录制（Ctrl+Alt+R）、保存并退出程序（Ctrl+Alt+Q）：被占用".to_string(), "保存并结束本场（Ctrl+Alt+S）：不认识".to_string()]);
        assert!(conflict_lines(&[]).is_empty());
    }

    #[test]
    fn the_plugins_english_error_is_translated_when_it_means_the_combination_is_taken() {
        let raw = "HotKey already registered: HotKey { mods: Modifiers(ALT | SUPER), key: Digit9, id: 536936462 }";
        assert_eq!(explain_failure(raw), "已被别的程序占用，或被系统保留");
        assert_eq!(explain_failure("别的错误"), "别的错误");
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-shell --lib hotkeys::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `app/src-tauri/src/hotkeys.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 三个热键的注册计划与结果（设计文档 6.2）。注册本身在 `app.rs`，这里只有可以脱离 Tauri 测试的部分。

use record_core::config::Config;
use record_core::controller::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyStatus {
    Registered,
    /// 没注册成功的原因，原样给用户看。
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyEntry {
    pub command: Command,
    /// 规范写法，如 `Ctrl+Alt+R`。
    pub text: String,
    pub status: HotkeyStatus,
}

pub fn command_name(command: Command) -> &'static str {
    match command {
        Command::Start => "开始录制",
        Command::Stop => "保存并结束本场",
        Command::Quit => "保存并退出程序",
    }
}

/// 配置里要注册的三个热键：（命令，规范写法，交给插件的写法）。热键写错或重复时返回说明。
pub fn wanted(config: &Config) -> Result<Vec<(Command, String, String)>, String> {
    let set = config.hotkey_set()?;
    Ok(vec![
        (Command::Start, set.start.to_string(), set.start.plugin_string()),
        (Command::Stop, set.stop.to_string(), set.stop.plugin_string()),
        (Command::Quit, set.quit.to_string(), set.quit.plugin_string()),
    ])
}

/// 托盘菜单里的文字：写上热键，没注册成功的标出来。
pub fn menu_label(command: Command, entry: Option<&HotkeyEntry>) -> String {
    let name = command_name(command);
    match entry {
        None => name.to_string(),
        Some(HotkeyEntry { status: HotkeyStatus::Registered, text, .. }) => format!("{name}（{text}）"),
        Some(HotkeyEntry { status: HotkeyStatus::Failed(_), text, .. }) => format!("{name}（{text} 没注册成功，被占用）"),
    }
}

/// 给“有热键没注册成功”的提示用：原因相同的热键合并成一行（配置写错时三个热键是同一个原因，不必说三遍）。
pub fn conflict_lines(entries: &[HotkeyEntry]) -> Vec<String> {
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for entry in entries {
        if let HotkeyStatus::Failed(why) = &entry.status {
            let name = format!("{}（{}）", command_name(entry.command), entry.text);
            match groups.iter_mut().find(|(reason, _)| reason == why) {
                Some((_, names)) => names.push(name),
                None => groups.push((why, vec![name])),
            }
        }
    }
    groups.into_iter().map(|(reason, names)| format!("{}：{reason}", names.join("、"))).collect()
}

/// 注册失败时插件给的是英文原文，里面最有用的只有“被占用”这个事实。
pub fn explain_failure(raw: &str) -> String {
    if raw.contains("already registered") { "已被别的程序占用，或被系统保留".to_string() } else { raw.to_string() }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-shell --lib hotkeys::tests::
```

预期：通过，输出里有 `test result: ok. 6 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(shell): 热键注册计划与结果"
```

---

## 任务 6：record-shell：runner

**目标：** 控制器的执行者（设计文档 6.1）：热键、菜单、自动保护、系统事件都变成 [`Msg`] 排进同一个队列，由一个线程串行交给 [`Controller`]，再按它返回的动作去启动、结束录制。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/runner.rs`

**接口：**

- 依赖：`crate::notice::Notice`、`record_core::controller::{Action, Command, Controller, Input, State}`、`record_core::session::EndReason`、`record_engine::events::{Alert, EngineEvent}`
- 提供：`pub struct Progress`；`pub struct Saved`；`pub trait Backend: Send + 'static`；`pub enum Msg`；`pub enum Ui`；`pub struct Runner<B: Backend>`；`Runner::new`；`Runner::state`；`Runner::handle`；`Runner::run_loop`；`pub struct Handle`；`Handle::command`；`Handle::auto`；`Handle::end_session`；`pub fn spawn<B: Backend>(backend: B, ui: impl FnMut(Ui) + Send + 'static) -> Handle`
- 测试：9 个：`start_then_stop_walks_through_the_states_and_tells_the_user`、`a_start_that_fails_leaves_nothing_running_and_says_why`、`the_engine_can_ask_for_a_stop_and_the_reason_reaches_the_notice`、`warnings_pile_up_during_a_session_and_clear_for_the_next_one`、`quit_while_recording_saves_first_then_quits`、`sleep_saves_the_session_signals_the_waiter_and_keeps_the_app_alive`、`ending_the_session_saves_then_quits_and_signals`、`progress_is_reported_only_while_recording`、`the_spawned_runner_serves_commands_from_other_threads`

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod runner;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/runner.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use record_core::session::TrackKind;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
        fail_start: Option<String>,
        ticks: u64,
    }

    impl Backend for Fake {
        fn start(&mut self, _events: Sender<EngineEvent>) -> Result<PathBuf, String> {
            if let Some(why) = self.fail_start.take() {
                return Err(why);
            }
            self.calls.lock().unwrap().push("start".into());
            Ok(PathBuf::from(r"D:\rec\one"))
        }

        fn progress(&mut self) -> Option<Progress> {
            self.ticks += 1;
            Some(Progress { elapsed: Duration::from_secs(self.ticks), bytes: self.ticks * 1000 })
        }

        fn stop(&mut self, reason: EndReason) -> Saved {
            self.calls.lock().unwrap().push(format!("stop {reason:?}"));
            Saved { dir: PathBuf::from(r"D:\rec\one"), meeting: Some(PathBuf::from(r"D:\rec\one\meeting.mp4")), duration_secs: 3.0, warnings: Vec::new() }
        }
    }

    struct Rig {
        runner: Runner<Fake>,
        ui: Arc<Mutex<Vec<Ui>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Rig {
        fn new() -> Self {
            Self::with(Fake::default())
        }

        fn with(backend: Fake) -> Self {
            let calls = backend.calls.clone();
            let ui = Arc::new(Mutex::new(Vec::new()));
            let sink = ui.clone();
            let (tx, _rx) = unbounded();
            Self { runner: Runner::new(backend, tx, move |e| sink.lock().unwrap().push(e)), ui, calls }
        }

        fn send(&mut self, msg: Msg) {
            self.runner.handle(msg);
        }

        fn events(&self) -> Vec<Ui> {
            self.ui.lock().unwrap().clone()
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn states(&self) -> Vec<State> {
            self.events().into_iter().filter_map(|e| if let Ui::State(s) = e { Some(s) } else { None }).collect()
        }

        fn notices(&self) -> Vec<Notice> {
            self.events().into_iter().filter_map(|e| if let Ui::Notice(n) = e { Some(n) } else { None }).collect()
        }
    }

    #[test]
    fn start_then_stop_walks_through_the_states_and_tells_the_user() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        assert_eq!(rig.runner.state(), State::Recording);
        rig.send(Msg::Command(Command::Stop, 5_000));
        assert_eq!(rig.runner.state(), State::Idle);
        assert_eq!(rig.states(), vec![State::Recording, State::Finalizing, State::Idle]);
        assert_eq!(rig.calls(), vec!["start".to_string(), "stop User".to_string()]);
        assert_eq!(
            rig.notices(),
            vec![Notice::Started { dir: r"D:\rec\one".into() }, Notice::Saved { reason: EndReason::User, meeting: Some(r"D:\rec\one\meeting.mp4".into()), duration_secs: 3.0, warnings: 0 },]
        );
    }

    #[test]
    fn a_start_that_fails_leaves_nothing_running_and_says_why() {
        let mut rig = Rig::with(Fake { fail_start: Some("磁盘剩余空间不足".into()), ..Fake::default() });
        rig.send(Msg::Command(Command::Start, 1_000));
        assert_eq!(rig.runner.state(), State::Idle);
        assert_eq!(rig.notices(), vec![Notice::StartFailed("磁盘剩余空间不足".into())]);
        assert!(rig.calls().is_empty(), "没开始就不该有 stop");
        // 失败后还能再来一次
        rig.send(Msg::Command(Command::Start, 3_000));
        assert_eq!(rig.runner.state(), State::Recording);
    }

    #[test]
    fn the_engine_can_ask_for_a_stop_and_the_reason_reaches_the_notice() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Engine(EngineEvent::RequestStop(EndReason::DiskLow)));
        assert_eq!(rig.runner.state(), State::Idle);
        assert_eq!(rig.calls().last().unwrap(), "stop DiskLow");
        assert!(matches!(rig.notices().last(), Some(Notice::Saved { reason: EndReason::DiskLow, .. })));
    }

    #[test]
    fn warnings_pile_up_during_a_session_and_clear_for_the_next_one() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Engine(EngineEvent::Alert(Alert::Hung { track: TrackKind::Mic, secs: 10 })));
        rig.send(Msg::Engine(EngineEvent::Alert(Alert::HdrActive)));
        let warnings: Vec<Vec<String>> = rig.events().into_iter().filter_map(|e| if let Ui::Warnings(w) = e { Some(w) } else { None }).collect();
        assert_eq!(warnings.len(), 2, "开始时清空一次，Hung 一次；HDR 只是提示，不算告警");
        assert!(warnings[0].is_empty() && warnings[1].len() == 1);
        assert_eq!(rig.notices().iter().filter(|n| matches!(n, Notice::Alert(_))).count(), 2, "两条都要走通知，是否弹出由通知模式决定");
        rig.send(Msg::Command(Command::Stop, 5_000));
        rig.send(Msg::Command(Command::Start, 9_000));
        assert!(matches!(rig.events().iter().rev().find(|e| matches!(e, Ui::Warnings(_))), Some(Ui::Warnings(w)) if w.is_empty()));
    }

    #[test]
    fn quit_while_recording_saves_first_then_quits() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Command(Command::Quit, 5_000));
        assert_eq!(rig.calls().last().unwrap(), "stop User");
        let events = rig.events();
        assert_eq!(events.last(), Some(&Ui::Quit));
        let saved_at = events.iter().position(|e| matches!(e, Ui::Notice(Notice::Saved { .. }))).unwrap();
        assert!(saved_at < events.len() - 1, "先保存再退出");
    }

    #[test]
    fn sleep_saves_the_session_signals_the_waiter_and_keeps_the_app_alive() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        let (done, wait) = unbounded();
        rig.send(Msg::Auto(EndReason::Sleep, Some(done)));
        assert_eq!(rig.calls().last().unwrap(), "stop Sleep");
        assert!(wait.try_recv().is_ok(), "保存完要通知等着的系统事件线程");
        assert!(!rig.events().contains(&Ui::Quit));
        assert_eq!(rig.runner.state(), State::Idle);
        // 睡眠时本来就没在录：立刻放行
        let (done, wait) = unbounded();
        rig.send(Msg::Auto(EndReason::Sleep, Some(done)));
        assert!(wait.try_recv().is_ok());
    }

    #[test]
    fn ending_the_session_saves_then_quits_and_signals() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        let (done, wait) = unbounded();
        rig.send(Msg::EndSession(EndReason::Shutdown, Some(done)));
        assert_eq!(rig.calls().last().unwrap(), "stop Shutdown");
        assert_eq!(rig.events().last(), Some(&Ui::Quit));
        assert!(wait.try_recv().is_ok());
        // 没在录时也要退出并放行
        let mut idle = Rig::new();
        let (done, wait) = unbounded();
        idle.send(Msg::EndSession(EndReason::Logoff, Some(done)));
        assert_eq!(idle.events(), vec![Ui::Quit]);
        assert!(wait.try_recv().is_ok());
    }

    #[test]
    fn progress_is_reported_only_while_recording() {
        let mut rig = Rig::new();
        rig.send(Msg::Tick);
        assert!(rig.events().is_empty());
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Tick);
        rig.send(Msg::Tick);
        let progress: Vec<Progress> = rig.events().into_iter().filter_map(|e| if let Ui::Progress(p) = e { Some(p) } else { None }).collect();
        assert_eq!(progress.len(), 2);
        assert!(progress[1].elapsed > progress[0].elapsed);
        rig.send(Msg::Command(Command::Stop, 5_000));
        let before = rig.events().len();
        rig.send(Msg::Tick);
        assert_eq!(rig.events().len(), before);
    }

    #[test]
    fn the_spawned_runner_serves_commands_from_other_threads() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ui = Arc::new(Mutex::new(Vec::new()));
        let sink = ui.clone();
        let handle = spawn(Fake { calls: calls.clone(), ..Fake::default() }, move |e| sink.lock().unwrap().push(e));
        handle.command(Command::Start);
        let wait = handle.auto(EndReason::Sleep);
        wait.recv_timeout(Duration::from_secs(5)).expect("保存完成的信号");
        assert_eq!(*calls.lock().unwrap(), vec!["start".to_string(), "stop Sleep".to_string()]);
        assert!(ui.lock().unwrap().iter().any(|e| matches!(e, Ui::Notice(Notice::Saved { reason: EndReason::Sleep, .. }))));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-shell --lib runner::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `app/src-tauri/src/runner.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 控制器的执行者（设计文档 6.1）：热键、菜单、自动保护、系统事件都变成 [`Msg`] 排进同一个队列，
//! 由一个线程串行交给 [`Controller`]，再按它返回的动作去启动、结束录制。
//!
//! 真正的录制通过 [`Backend`] 完成，这样状态流转可以不碰设备来测试。

use crate::notice::Notice;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use record_core::controller::{Action, Command, Controller, Input, State};
use record_core::session::EndReason;
use record_engine::events::{Alert, EngineEvent};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 录制中的进度，托盘提示用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub elapsed: Duration,
    pub bytes: u64,
}

/// 一场录制收尾后的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub dir: PathBuf,
    pub meeting: Option<PathBuf>,
    pub duration_secs: f64,
    pub warnings: Vec<String>,
}

pub trait Backend: Send + 'static {
    /// 创建会话并开始采集，引擎事件发到 `events`；返回会话目录。
    fn start(&mut self, events: Sender<EngineEvent>) -> Result<PathBuf, String>;
    fn progress(&mut self) -> Option<Progress>;
    /// 停止并生成成品。总会返回：出错也要尽量把已有内容留下。
    fn stop(&mut self, reason: EndReason) -> Saved;
}

pub enum Msg {
    Command(Command, u64),
    /// 自动保护、睡眠等需要结束本场的原因；`done` 在本场保存完（或本来就没在录）后收到一个信号。
    Auto(EndReason, Option<Sender<()>>),
    /// 关机、注销：保存本场并退出程序。
    EndSession(EndReason, Option<Sender<()>>),
    Engine(EngineEvent),
    Tick,
}

/// 发给界面（托盘、气泡）的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Ui {
    State(State),
    Progress(Progress),
    /// 本场当前的全部告警，新一场开始时清空。
    Warnings(Vec<String>),
    Notice(Notice),
    Quit,
}

pub struct Runner<B: Backend> {
    backend: B,
    controller: Controller,
    ui: Box<dyn FnMut(Ui) + Send>,
    msgs: Sender<Msg>,
    warnings: Vec<String>,
    waiting: Vec<Sender<()>>,
    last_saved: Option<Saved>,
    published: State,
}

impl<B: Backend> Runner<B> {
    pub fn new(backend: B, msgs: Sender<Msg>, ui: impl FnMut(Ui) + Send + 'static) -> Self {
        Self { backend, controller: Controller::new(), ui: Box::new(ui), msgs, warnings: Vec::new(), waiting: Vec::new(), last_saved: None, published: State::Idle }
    }

    pub fn state(&self) -> State {
        self.controller.state()
    }

    pub fn handle(&mut self, msg: Msg) {
        let actions = match msg {
            Msg::Command(command, at_ms) => self.controller.handle(Input::Command(command), at_ms),
            Msg::Auto(reason, done) => {
                self.waiting.extend(done);
                self.controller.handle(Input::AutoStop(reason), 0)
            }
            Msg::EndSession(reason, done) => {
                self.waiting.extend(done);
                self.controller.handle(Input::EndSession(reason), 0)
            }
            Msg::Engine(event) => self.engine_event(event),
            Msg::Tick => {
                self.tick();
                Vec::new()
            }
        };
        self.run(actions);
        self.publish_state();
        if self.controller.state() == State::Idle {
            for done in self.waiting.drain(..) {
                let _ = done.send(());
            }
        }
    }

    fn run(&mut self, actions: Vec<Action>) {
        let mut queue: VecDeque<Action> = actions.into();
        while let Some(action) = queue.pop_front() {
            let more = match action {
                Action::BeginStart => self.begin_start(),
                Action::BeginFinalize(reason) => self.begin_finalize(reason),
                Action::NotifyStartFailed(why) => {
                    self.ui(Ui::Notice(Notice::StartFailed(why)));
                    Vec::new()
                }
                Action::NotifySaved(reason) => {
                    if let Some(saved) = self.last_saved.take() {
                        let notice = Notice::Saved { reason, meeting: saved.meeting.map(|p| p.display().to_string()), duration_secs: saved.duration_secs, warnings: saved.warnings.len() };
                        self.ui(Ui::Notice(notice));
                    }
                    Vec::new()
                }
                Action::QuitApp => {
                    // 退出之前，托盘先回到待命：用户看到的最后一个状态应当是“已保存完”
                    self.publish_state();
                    self.ui(Ui::Quit);
                    Vec::new()
                }
            };
            queue.extend(more);
        }
    }

    fn begin_start(&mut self) -> Vec<Action> {
        self.warnings.clear();
        self.ui(Ui::Warnings(Vec::new()));
        let (events, incoming) = unbounded();
        match self.backend.start(events) {
            Ok(dir) => {
                // 引擎结束后会丢掉发送端，转发线程随之退出
                let msgs = self.msgs.clone();
                std::thread::spawn(move || {
                    for event in incoming {
                        if msgs.send(Msg::Engine(event)).is_err() {
                            break;
                        }
                    }
                });
                self.ui(Ui::Notice(Notice::Started { dir: dir.display().to_string() }));
                self.controller.handle(Input::StartSucceeded, 0)
            }
            Err(why) => self.controller.handle(Input::StartFailed(why), 0),
        }
    }

    fn begin_finalize(&mut self, reason: EndReason) -> Vec<Action> {
        // 先让托盘显示“正在保存”，停止要花零点几秒到几秒
        self.publish_state();
        self.last_saved = Some(self.backend.stop(reason));
        self.controller.handle(Input::FinalizeDone, 0)
    }

    fn engine_event(&mut self, event: EngineEvent) -> Vec<Action> {
        match event {
            EngineEvent::Info(_) => Vec::new(),
            EngineEvent::Alert(alert) => {
                self.alert(alert);
                Vec::new()
            }
            EngineEvent::RequestStop(reason) => self.controller.handle(Input::AutoStop(reason), 0),
        }
    }

    fn alert(&mut self, alert: Alert) {
        if alert.is_warning() {
            self.warnings.push(alert.describe());
            self.ui(Ui::Warnings(self.warnings.clone()));
        }
        self.ui(Ui::Notice(Notice::Alert(alert)));
    }

    fn tick(&mut self) {
        if self.controller.state() == State::Recording
            && let Some(progress) = self.backend.progress()
        {
            self.ui(Ui::Progress(progress));
        }
    }

    fn publish_state(&mut self) {
        let now = self.controller.state();
        if now != self.published {
            self.published = now;
            self.ui(Ui::State(now));
        }
    }

    fn ui(&mut self, event: Ui) {
        (self.ui)(event);
    }

    /// 在当前线程里一直循环处理，空闲时每 500 ms 补一个 [`Msg::Tick`]。
    /// 执行者自己留着一个发送端（转发引擎事件要用），所以队列不会因为“都关闭了”而结束：线程随进程一起退出。
    pub fn run_loop(mut self, incoming: Receiver<Msg>) {
        loop {
            match incoming.recv_timeout(Duration::from_millis(500)) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => self.handle(Msg::Tick),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

/// 外壳里各处（热键、菜单、系统事件）向执行者投递消息的入口，可以克隆。
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Msg>,
    epoch: Instant,
}

impl Handle {
    pub fn command(&self, command: Command) {
        let at_ms = self.epoch.elapsed().as_millis() as u64;
        let _ = self.tx.send(Msg::Command(command, at_ms));
    }

    /// 结束本场；返回的接收端在保存完成（或本来就没在录）时收到一个信号。
    pub fn auto(&self, reason: EndReason) -> Receiver<()> {
        let (done, wait) = unbounded();
        let _ = self.tx.send(Msg::Auto(reason, Some(done)));
        wait
    }

    pub fn end_session(&self, reason: EndReason) -> Receiver<()> {
        let (done, wait) = unbounded();
        let _ = self.tx.send(Msg::EndSession(reason, Some(done)));
        wait
    }
}

/// 启动执行者线程（常驻，随进程退出）。
pub fn spawn<B: Backend>(backend: B, ui: impl FnMut(Ui) + Send + 'static) -> Handle {
    let (tx, incoming) = unbounded();
    let runner = Runner::new(backend, tx.clone(), ui);
    std::thread::Builder::new().name("runner".into()).spawn(move || runner.run_loop(incoming)).expect("启动执行者线程");
    Handle { tx, epoch: Instant::now() }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-shell --lib runner::tests::
```

预期：通过，输出里有 `test result: ok. 9 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(shell): 控制器执行者（热键、菜单、自动保护与系统事件排进同一个队列）"
```

---

## 任务 7：record-shell：backend

**目标：** 真实的录制后端：把执行者的 [`Backend`] 接到 `record-engine`；以及启动时的崩溃恢复。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/backend.rs`

**接口：**

- 依赖：`crate::runner::{Backend, Progress, Saved}`、`record_core::config::{Config, Encoder}`、`record_core::paths::resolve_output_dir`、`record_core::session::EndReason`、`record_engine::events::EngineEvent`、`record_engine::recorder::{Recorder, StartParams, TrackSelect, start}`、`record_engine::recovery::{candidates, recover}`
- 提供：`pub type SharedConfig = Arc<RwLock<Config>>`；`pub struct EngineBackend`；`EngineBackend::new`；`pub fn set_aside_broken(path: &Path) -> Option<PathBuf>`；`pub fn recover_on_startup(ffmpeg: &Path, base: &Path) -> (usize, usize)`
- 测试：3 个：`stopping_without_a_session_reports_it_instead_of_pretending`、`a_config_that_cannot_be_read_is_kept_aside_before_it_is_overwritten`、`a_missing_or_empty_save_folder_has_nothing_to_recover`

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod backend;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/backend.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_without_a_session_reports_it_instead_of_pretending() {
        let mut backend = EngineBackend::new(Arc::new(RwLock::new(Config::default())), PathBuf::from("."));
        let saved = backend.stop(EndReason::User);
        assert!(saved.meeting.is_none());
        assert_eq!(saved.warnings, vec!["没有进行中的录制".to_string()]);
        assert!(backend.progress().is_none());
    }

    #[test]
    fn a_config_that_cannot_be_read_is_kept_aside_before_it_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        assert_eq!(set_aside_broken(&file), None, "文件不存在：没有什么要留底");
        std::fs::write(&file, "autostart = true\n").unwrap();
        assert_eq!(set_aside_broken(&file), None, "能读懂的配置不动");
        assert!(file.exists());
        std::fs::write(&file, "hotkeys = [这不是 toml").unwrap();
        let backup = set_aside_broken(&file).expect("读不懂的要留底");
        assert!(!file.exists() && backup.exists());
        assert_eq!(std::fs::read_to_string(backup).unwrap(), "hotkeys = [这不是 toml");
    }

    #[test]
    fn a_missing_or_empty_save_folder_has_nothing_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        let ffmpeg = Path::new("ffmpeg-that-is-never-called.exe");
        assert_eq!(recover_on_startup(ffmpeg, dir.path()), (0, 0));
        assert_eq!(recover_on_startup(ffmpeg, &dir.path().join("not-created-yet")), (0, 0));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-shell --lib backend::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `app/src-tauri/src/backend.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 真实的录制后端：把执行者的 [`Backend`] 接到 `record-engine`；以及启动时的崩溃恢复。

use crate::runner::{Backend, Progress, Saved};
use crossbeam_channel::Sender;
use record_core::config::{Config, Encoder};
use record_core::paths::resolve_output_dir;
use record_core::session::EndReason;
use record_engine::events::EngineEvent;
use record_engine::recorder::{Recorder, StartParams, TrackSelect, start};
use record_engine::recovery::{candidates, recover};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 设置界面保存配置之后，下一场录制就用新配置。
pub type SharedConfig = Arc<RwLock<Config>>;

pub struct EngineBackend {
    config: SharedConfig,
    videos_dir: PathBuf,
    recorder: Option<Recorder>,
    /// 探测编码器要起 ffmpeg 试编码，结果按（ffmpeg 路径，配置里的编码器）缓存，第二场起开始得更快。
    resolved: Option<(PathBuf, Encoder, Encoder)>,
}

impl EngineBackend {
    pub fn new(config: SharedConfig, videos_dir: PathBuf) -> Self {
        Self { config, videos_dir, recorder: None, resolved: None }
    }

    fn encoder_for(&mut self, ffmpeg: &Path, wanted: Encoder) -> Result<Encoder, String> {
        if let Some((path, asked, resolved)) = &self.resolved
            && path == ffmpeg
            && *asked == wanted
        {
            return Ok(*resolved);
        }
        let resolved = record_mux::probe::resolve_encoder(ffmpeg, wanted).map_err(|e| e.to_string())?;
        self.resolved = Some((ffmpeg.to_path_buf(), wanted, resolved));
        Ok(resolved)
    }
}

impl Backend for EngineBackend {
    fn start(&mut self, events: Sender<EngineEvent>) -> Result<PathBuf, String> {
        let config = self.config.read().map_err(|_| "配置锁已损坏".to_string())?.clone();
        let ffmpeg = record_mux::locate::find_ffmpeg().map_err(|e| e.to_string())?;
        let encoder = self.encoder_for(&ffmpeg, config.video.encoder)?;
        let output_dir = resolve_output_dir(&config, &self.videos_dir);
        let params = StartParams { config, ffmpeg, encoder, output_dir, app_version: env!("CARGO_PKG_VERSION").to_string(), tracks: TrackSelect::default(), monitor: None };
        let recorder = start(params, events).map_err(|e| e.to_string())?;
        let dir = recorder.dir().to_path_buf();
        self.recorder = Some(recorder);
        Ok(dir)
    }

    fn progress(&mut self) -> Option<Progress> {
        self.recorder.as_ref().map(|r| {
            let status = r.status();
            Progress { elapsed: status.elapsed, bytes: status.bytes }
        })
    }

    fn stop(&mut self, reason: EndReason) -> Saved {
        match self.recorder.take() {
            Some(recorder) => {
                let outcome = recorder.stop(reason);
                tracing::info!(session = %outcome.id, ?reason, secs = outcome.duration_secs, meeting = ?outcome.meeting, warnings = outcome.warnings.len(), timings = ?outcome.timings, "本场已收尾");
                for warning in &outcome.warnings {
                    tracing::warn!(session = %outcome.id, "收尾时的警告：{warning}");
                }
                Saved { dir: outcome.dir, meeting: outcome.meeting, duration_secs: outcome.duration_secs, warnings: outcome.warnings }
            }
            None => Saved { dir: PathBuf::new(), meeting: None, duration_secs: 0.0, warnings: vec!["没有进行中的录制".to_string()] },
        }
    }
}

/// 配置文件读不懂时，保存之前先把它改名留底，免得用户手改的内容被覆盖掉；返回留底文件的路径。
pub fn set_aside_broken(path: &Path) -> Option<PathBuf> {
    if Config::load(path).is_ok() {
        return None;
    }
    let backup = path.with_extension("toml.broken");
    std::fs::rename(path, &backup).ok().map(|()| backup)
}

/// 启动时修复上次崩溃、被杀、断电留下的会话（设计文档 5.8）：返回（修复成功几场，没能修复几场）。
pub fn recover_on_startup(ffmpeg: &Path, base: &Path) -> (usize, usize) {
    let (mut repaired, mut failed) = (0, 0);
    for dir in candidates(base) {
        match recover(ffmpeg, &dir) {
            Ok(Some(report)) if report.repair_errors.is_empty() && report.errors.is_empty() => {
                tracing::info!(session = %report.id, "已修复上次中断的录制：{}", dir.display());
                repaired += 1;
            }
            Ok(Some(report)) => {
                tracing::warn!(session = %report.id, repair_errors = ?report.repair_errors, errors = ?report.errors, "修复上次中断的录制时有失败：{}", dir.display());
                failed += 1;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("修复 {} 失败：{e}", dir.display());
                failed += 1;
            }
        }
    }
    (repaired, failed)
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-shell --lib backend::tests::
```

预期：通过，输出里有 `test result: ok. 3 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(shell): 接到录制引擎的后端与启动时的崩溃恢复"
```

---

## 任务 8：record-shell：logging

**目标：** 日志（设计文档 4.4）：按天滚动写到 `%LOCALAPPDATA%\record\logs\record.log.<日期>`。程序没有控制台，所以日志只进文件；用环境变量 `RECORD_LOG` 调级别（默认 info）。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/logging.rs`

**接口：**

- 依赖：无（不依赖本工作区的其他模块）
- 提供：`pub fn init(dir: &Path) -> Option<WorkerGuard>`
- 测试：1 个：`lines_logged_after_init_end_up_in_the_daily_file`

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod logging;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/logging.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_logged_after_init_end_up_in_the_daily_file() {
        let dir = tempfile::tempdir().unwrap();
        let guard = init(dir.path()).expect("初始化日志");
        tracing::info!("日志自检 {}", 42);
        drop(guard); // 冲刷缓冲
        let text: String = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| std::fs::read_to_string(e.path()).unwrap()).collect();
        assert!(text.contains("日志自检 42"), "{text}");
        assert!(!text.contains('\u{1b}'), "文件里不应该有颜色转义");
        let stamp = text.lines().next().and_then(|line| line.get(..23)).unwrap_or_default();
        assert!(chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S%.3f").is_ok(), "行首应是本地时间：{text}");
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-shell --lib logging::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `app/src-tauri/src/logging.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 日志（设计文档 4.4）：按天滚动写到 `%LOCALAPPDATA%\record\logs\record.log.<日期>`。
//! 程序没有控制台，所以日志只进文件；用环境变量 `RECORD_LOG` 调级别（默认 info）。

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

/// 日志里的时间用本机本地时间（默认是 UTC，对着自己的时钟看日志会差几个小时）。
struct LocalClock;

impl FormatTime for LocalClock {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}

/// 返回的守卫要一直拿着：它掉了，缓冲里还没写完的日志就丢了。目录建不出来时返回 `None`，程序照常运行，只是没有日志。
pub fn init(dir: &Path) -> Option<WorkerGuard> {
    std::fs::create_dir_all(dir).ok()?;
    let (writer, guard) = tracing_appender::non_blocking(tracing_appender::rolling::daily(dir, "record.log"));
    let filter = EnvFilter::try_from_env("RECORD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_timer(LocalClock).with_env_filter(filter).try_init().ok()?;
    Some(guard)
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-shell --lib logging::tests::
```

预期：通过，输出里有 `test result: ok. 1 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(shell): 按天滚动的日志"
```

---

## 任务 9：record-shell：power

**目标：** 睡眠、关机、注销时保存本场（设计文档 6.5）。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/power.rs`

**接口：**

- 依赖：`record_core::session::EndReason`
- 提供：`pub const CLASS_NAME: PCWSTR`；`pub enum Event`；`pub fn classify(msg: u32, wparam: usize, lparam: isize) -> Option<Event>`；`pub fn spawn(on_event: impl Fn(Event) + Send + Sync + 'static) -> std::io::Result<()>`
- 测试：2 个：`only_suspend_and_a_confirmed_end_of_session_are_events`、`the_hidden_window_turns_real_system_messages_into_events`

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod power;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/power.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageW};

    #[test]
    fn only_suspend_and_a_confirmed_end_of_session_are_events() {
        assert_eq!(classify(WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0), Some(Event::Suspend));
        assert_eq!(classify(WM_POWERBROADCAST, 18, 0), None, "唤醒不是要保存的事件");
        assert_eq!(classify(WM_ENDSESSION, 1, 0), Some(Event::EndSession(EndReason::Shutdown)));
        assert_eq!(classify(WM_ENDSESSION, 1, ENDSESSION_LOGOFF as isize), Some(Event::EndSession(EndReason::Logoff)));
        assert_eq!(classify(WM_ENDSESSION, 1, (ENDSESSION_LOGOFF | 1) as isize), Some(Event::EndSession(EndReason::Logoff)), "带上 CLOSEAPP 等其他标志也一样");
        assert_eq!(classify(WM_ENDSESSION, 0, 0), None, "关机被取消");
        assert_eq!(classify(WM_QUERYENDSESSION, 1, 0), None);
    }

    #[test]
    fn the_hidden_window_turns_real_system_messages_into_events() {
        let (tx, rx) = unbounded();
        spawn(move |event| {
            let _ = tx.send(event);
        })
        .expect("建系统事件窗口");
        assert!(spawn(|_| {}).is_err(), "只允许建一次");
        let hwnd = unsafe { FindWindowW(CLASS_NAME, None) }.expect("按类名找到隐藏窗口");
        let send = |msg: u32, wparam: usize, lparam: isize| unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))) };

        assert_eq!(send(WM_QUERYENDSESSION, 0, 0).0, 1, "同意关机");
        assert!(rx.try_recv().is_err());
        send(WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
        assert_eq!(rx.try_recv(), Ok(Event::Suspend));
        send(WM_ENDSESSION, 1, ENDSESSION_LOGOFF as isize);
        assert_eq!(rx.try_recv(), Ok(Event::EndSession(EndReason::Logoff)));
        send(WM_ENDSESSION, 1, 0);
        assert_eq!(rx.try_recv(), Ok(Event::EndSession(EndReason::Shutdown)));
        send(WM_ENDSESSION, 0, 0);
        assert!(rx.try_recv().is_err());
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-shell --lib power::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `app/src-tauri/src/power.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 睡眠、关机、注销时保存本场（设计文档 6.5）。
//!
//! 一个隐藏的顶层窗口接收 `WM_POWERBROADCAST` 与 `WM_ENDSESSION`：仅用消息窗口（`HWND_MESSAGE`）收不到
//! `WM_QUERYENDSESSION`/`WM_ENDSESSION`（S5 实测），所以建的是普通顶层窗口，只是从不显示。
//! 窗口过程在“保存完成”之前不返回，系统因此会等本场存完再睡眠或关机。

use record_core::session::EndReason;
use std::sync::OnceLock;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::RegisterSuspendResumeNotification;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DispatchMessageW, ENDSESSION_LOGOFF, GetMessageW, MSG, PBT_APMSUSPEND, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
    WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

pub const CLASS_NAME: PCWSTR = w!("RecordSessionEvents");

/// 需要保存本场的系统事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Suspend,
    EndSession(EndReason),
}

/// 把窗口消息翻译成事件；与保存无关的消息返回 `None`。
/// `WM_ENDSESSION` 的 `wparam` 为 0 表示系统结束会话的决定被取消，那就什么都不做。
pub fn classify(msg: u32, wparam: usize, lparam: isize) -> Option<Event> {
    match msg {
        WM_POWERBROADCAST if wparam as u32 == PBT_APMSUSPEND => Some(Event::Suspend),
        WM_ENDSESSION if wparam != 0 => Some(Event::EndSession(if (lparam as u32) & ENDSESSION_LOGOFF != 0 { EndReason::Logoff } else { EndReason::Shutdown })),
        _ => None,
    }
}

type Callback = Box<dyn Fn(Event) + Send + Sync>;
static CALLBACK: OnceLock<Callback> = OnceLock::new();

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(event) = classify(msg, wparam.0, lparam.0)
        && let Some(callback) = CALLBACK.get()
    {
        callback(event);
    }
    match msg {
        // 同意关机：真正的保存在 WM_ENDSESSION 里做，这里不做耗时的事
        WM_QUERYENDSESSION | WM_POWERBROADCAST => LRESULT(1),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_window() -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: CLASS_NAME, ..Default::default() };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS_NAME, w!("Record"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(instance.into()), None)?;
        // 新式待机（Modern Standby）的机器上，睡眠通知要登记才有；旧式广播也会来，重复的睡眠事件由执行者幂等地忽略
        if let Err(e) = RegisterSuspendResumeNotification(HANDLE(hwnd.0), DEVICE_NOTIFY_WINDOW_HANDLE) {
            tracing::warn!("登记睡眠通知失败，只能依赖系统广播：{e}");
        }
        Ok(hwnd)
    }
}

/// 在新线程里建窗口并跑消息循环；窗口建好后才返回。整个进程只能调用一次。
pub fn spawn(on_event: impl Fn(Event) + Send + Sync + 'static) -> std::io::Result<()> {
    if CALLBACK.set(Box::new(on_event)).is_err() {
        return Err(std::io::Error::other("系统事件窗口已经建过了"));
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("session-events".into()).spawn(move || {
        let created = create_window();
        let _ = ready_tx.send(created.map(|_| ()).map_err(|e| e.to_string()));
        let mut msg = MSG::default();
        // 窗口永不关闭：循环随进程结束
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    })?;
    ready_rx.recv().map_err(|_| std::io::Error::other("系统事件线程提前结束"))?.map_err(std::io::Error::other)
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-shell --lib power::tests::
```

预期：通过，输出里有 `test result: ok. 2 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(shell): 睡眠、关机、注销时保存"
```

---

## 任务 10：record-shell：app

**目标：** 外壳的装配（设计文档 6）：插件、托盘、热键、设置窗口，以及它们与执行者的连接。

**文件：**

- 修改：`app/src-tauri/src/lib.rs` — 模块声明
- 创建：`app/src-tauri/src/app.rs`

**接口：**

- 依赖：`crate::backend::{EngineBackend, SharedConfig, recover_on_startup, set_aside_broken}`、`crate::hotkeys::{self, HotkeyEntry, HotkeyStatus}`、`crate::notice::{Notice, should_show}`、`crate::runner::{self, Handle, Progress, Ui}`、`crate::tray_model::{self, Look}`、`crate::{logging, power}`、`record_capture::devices::{Flow, list_active}`、`record_core::config::Config`、`record_core::controller::{Command, State as RunState}`、`record_core::paths::{Paths, resolve_output_dir}`、`record_core::session::EndReason`
- 提供：`pub fn run()`
- 测试：没有单元测试：它是把上面这些模块装配到 Tauri 上的胶水，要真实的托盘、热键和窗口；由后面的真机检查脚本 `shell-check.ps1` 验证。

- [ ] **步骤 1：在 `app/src-tauri/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod app;
```

- [ ] **步骤 2：创建 `app/src-tauri/src/app.rs`**

```rust
//! 外壳的装配（设计文档 6）：插件、托盘、热键、设置窗口，以及它们与执行者的连接。
//!
//! 这里只有胶水：每个决定都在 `tray_model`、`notice`、`hotkeys`、`runner` 里，那些模块有单元测试；
//! 这一层靠 `scripts/shell-check.ps1` 在真机上验证。

use crate::backend::{EngineBackend, SharedConfig, recover_on_startup, set_aside_broken};
use crate::hotkeys::{self, HotkeyEntry, HotkeyStatus};
use crate::notice::{Notice, should_show};
use crate::runner::{self, Handle, Progress, Ui};
use crate::tray_model::{self, Look};
use crate::{logging, power};
use record_capture::devices::{Flow, list_active};
use record_core::config::Config;
use record_core::controller::{Command, State as RunState};
use record_core::paths::{Paths, resolve_output_dir};
use record_core::session::EndReason;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{PCWSTR, w};

const TRAY_ID: &str = "record";
const SETTINGS_WINDOW: &str = "settings";
/// 睡眠通知后最多等多久让本场存完：系统给的时间本来就不多，来不及的由下次启动的崩溃恢复补上。
const SLEEP_WAIT: Duration = Duration::from_secs(2);
const END_SESSION_WAIT: Duration = Duration::from_secs(10);

struct TrayItems {
    start: MenuItem<Wry>,
    stop: MenuItem<Wry>,
    quit: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

#[derive(Default)]
struct Registered {
    entries: Vec<HotkeyEntry>,
    /// 插件给每个热键的编号 → 命令，热键触发时反查。
    ids: Vec<(u32, Command)>,
}

struct View {
    state: RunState,
    progress: Progress,
    warnings: Vec<String>,
    /// 托盘上现在显示的图标样子与提示，没变就不去动托盘。
    shown: Option<(Look, String)>,
}

struct Shared {
    paths: Paths,
    videos_dir: PathBuf,
    config: SharedConfig,
    config_error: Mutex<Option<String>>,
    handle: Handle,
    hotkeys: Mutex<Registered>,
    view: Mutex<View>,
    items: TrayItems,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn current_config(shared: &Shared) -> Config {
    shared.config.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

fn fatal(text: &str) -> ! {
    tracing::error!("{text}");
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { MessageBoxW(None, PCWSTR(wide.as_ptr()), w!("Record 无法启动"), MB_OK | MB_ICONERROR) };
    std::process::exit(1)
}

// ───────────────────────────── 托盘与通知 ─────────────────────────────

fn tray_icon(look: Look) -> tauri::image::Image<'static> {
    tauri::image::Image::new_owned(tray_model::icon_rgba(tray_model::rgb(look)), tray_model::ICON_SIZE, tray_model::ICON_SIZE)
}

/// 按当前状态刷新托盘图标、提示和菜单项；和上次显示的一样就不动。
fn refresh_tray(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let (state, look, tooltip) = {
        let mut view = lock(&shared.view);
        let look = tray_model::look(view.state, view.warnings.len());
        let tooltip = tray_model::tooltip(view.state, view.progress.elapsed, view.progress.bytes, &view.warnings);
        if view.shown.as_ref() == Some(&(look, tooltip.clone())) {
            return;
        }
        if view.shown.as_ref().map(|(shown_look, _)| *shown_look) == Some(look) {
            tracing::debug!(%tooltip, "托盘提示更新");
        } else {
            tracing::info!(?look, state = ?view.state, %tooltip, "托盘图标更新");
        }
        view.shown = Some((look, tooltip.clone()));
        (view.state, look, tooltip)
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(tray_icon(look)));
        let _ = tray.set_tooltip(Some(tooltip));
    }
    let enabled = tray_model::menu_enabled(state);
    let _ = shared.items.start.set_enabled(enabled.start);
    let _ = shared.items.stop.set_enabled(enabled.stop);
}

/// 菜单项上写热键；没注册成功的标出来。
fn refresh_menu_labels(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let registered = lock(&shared.hotkeys);
    let label = |command| hotkeys::menu_label(command, registered.entries.iter().find(|e| e.command == command));
    let (start, stop, quit) = (label(Command::Start), label(Command::Stop), label(Command::Quit));
    tracing::debug!(%start, %stop, %quit, "托盘菜单文字");
    let _ = shared.items.start.set_text(start);
    let _ = shared.items.stop.set_text(stop);
    let _ = shared.items.quit.set_text(quit);
}

fn notify(app: &AppHandle, notice: Notice) {
    let shared = app.state::<Shared>();
    let mode = current_config(&shared).notify.mode;
    let show = should_show(mode, &notice);
    let (title, body) = notice.text();
    tracing::info!(show, %title, %body, "通知");
    if show && let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("弹出系统通知失败：{e}");
    }
}

fn on_ui(app: &AppHandle, event: Ui) {
    let shared = app.state::<Shared>();
    match event {
        Ui::State(state) => {
            let mut view = lock(&shared.view);
            view.state = state;
            if state == RunState::Recording {
                view.progress = Progress::default();
            }
            drop(view);
            refresh_tray(app);
        }
        Ui::Progress(progress) => {
            lock(&shared.view).progress = progress;
            refresh_tray(app);
        }
        Ui::Warnings(warnings) => {
            lock(&shared.view).warnings = warnings;
            refresh_tray(app);
        }
        Ui::Notice(notice) => notify(app, notice),
        Ui::Quit => {
            tracing::info!("退出");
            app.exit(0);
        }
    }
}

// ───────────────────────────── 热键、自启、文件夹 ─────────────────────────────

/// 按配置（重新）注册三个热键，返回每个的结果。常驻注册：不论当前是待命还是录制中，三个热键始终有效。
fn apply_hotkeys(app: &AppHandle, config: &Config) -> Vec<HotkeyEntry> {
    let shortcuts = app.global_shortcut();
    if let Err(e) = shortcuts.unregister_all() {
        tracing::warn!("清除旧热键失败：{e}");
    }
    let mut entries = Vec::new();
    let mut ids = Vec::new();
    match hotkeys::wanted(config) {
        Ok(wanted) => {
            for (command, text, plugin_text) in wanted {
                let status = match plugin_text.parse::<Shortcut>() {
                    Ok(shortcut) => match shortcuts.register(shortcut) {
                        Ok(()) => {
                            ids.push((shortcut.id(), command));
                            HotkeyStatus::Registered
                        }
                        Err(e) => HotkeyStatus::Failed(hotkeys::explain_failure(&e.to_string())),
                    },
                    Err(e) => HotkeyStatus::Failed(e.to_string()),
                };
                entries.push(HotkeyEntry { command, text, status });
            }
        }
        Err(why) => {
            let typed = [(Command::Start, &config.hotkeys.start), (Command::Stop, &config.hotkeys.stop), (Command::Quit, &config.hotkeys.quit)];
            entries.extend(typed.into_iter().map(|(command, text)| HotkeyEntry { command, text: text.clone(), status: HotkeyStatus::Failed(why.clone()) }));
        }
    }
    for entry in &entries {
        match &entry.status {
            HotkeyStatus::Registered => tracing::info!(command = ?entry.command, hotkey = %entry.text, "热键已注册"),
            HotkeyStatus::Failed(why) => tracing::warn!(command = ?entry.command, hotkey = %entry.text, "热键没注册成功：{why}"),
        }
    }
    *lock(&app.state::<Shared>().hotkeys) = Registered { entries: entries.clone(), ids };
    entries
}

/// 让系统的开机自启项与配置一致。
fn sync_autostart(app: &AppHandle, want: bool) {
    let launcher = app.autolaunch();
    match launcher.is_enabled() {
        Ok(now) if now == want => {}
        Ok(_) => {
            let result = if want { launcher.enable() } else { launcher.disable() };
            match result {
                Ok(()) => tracing::info!(want, "开机自启已同步"),
                Err(e) => tracing::warn!(want, "同步开机自启失败：{e}"),
            }
        }
        Err(e) => tracing::warn!("读取开机自启状态失败：{e}"),
    }
}

fn open_in_explorer(path: &Path) {
    if let Err(e) = std::fs::create_dir_all(path) {
        tracing::warn!("创建文件夹 {} 失败：{e}", path.display());
    }
    // explorer 打开文件夹时退出码常常是 1，不用理会
    if let Err(e) = std::process::Command::new("explorer.exe").arg(path).spawn() {
        tracing::warn!("打开文件夹 {} 失败：{e}", path.display());
    }
}

fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Err(e) = WebviewWindowBuilder::new(app, SETTINGS_WINDOW, WebviewUrl::App("index.html".into())).title("Record 设置").inner_size(760.0, 860.0).min_inner_size(560.0, 480.0).build() {
        tracing::error!("打开设置窗口失败：{e}");
    }
}

fn toggle_autostart(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let want = shared.items.autostart.is_checked().unwrap_or(false);
    let mut config = current_config(&shared);
    config.autostart = want;
    match persist(&shared, config) {
        Ok(()) => sync_autostart(app, want),
        Err(e) => {
            tracing::warn!("保存开机自启设置失败：{e}");
            let _ = shared.items.autostart.set_checked(!want);
        }
    }
}

/// 写入配置文件并让内存里的配置同步；读不懂的旧文件先留底。
fn persist(shared: &Shared, config: Config) -> Result<(), String> {
    if let Some(backup) = set_aside_broken(&shared.paths.config_file) {
        tracing::warn!("原配置文件读不懂，已改名保留为 {}", backup.display());
    }
    config.save(&shared.paths.config_file).map_err(|e| e.to_string())?;
    *shared.config.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
    *lock(&shared.config_error) = None;
    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    let shared = app.state::<Shared>();
    match id {
        "start" => shared.handle.command(Command::Start),
        "stop" => shared.handle.command(Command::Stop),
        "quit" => shared.handle.command(Command::Quit),
        "recordings" => open_in_explorer(&resolve_output_dir(&current_config(&shared), &shared.videos_dir)),
        "logs" => open_in_explorer(&shared.paths.log_dir),
        "settings" => show_settings(app),
        "autostart" => toggle_autostart(app),
        other => tracing::warn!("没有处理的菜单项：{other}"),
    }
}

// ───────────────────────────── 设置窗口用的命令 ─────────────────────────────

#[derive(Serialize)]
struct IssueDto {
    key: String,
    message: String,
}

#[derive(Serialize)]
struct HotkeyDto {
    command: &'static str,
    text: String,
    ok: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct SettingsDto {
    config: Config,
    /// 还没有配置文件：设置窗口按首次使用向导显示。
    first_run: bool,
    /// 配置文件读不懂时的原因；此时 `config` 是默认值。
    config_error: Option<String>,
    default_output_dir: String,
    hotkeys: Vec<HotkeyDto>,
    config_path: String,
    version: &'static str,
}

#[derive(Serialize)]
struct SaveResultDto {
    issues: Vec<IssueDto>,
    hotkeys: Vec<HotkeyDto>,
}

#[derive(Serialize)]
struct DeviceDto {
    id: String,
    name: String,
    /// `render`（播放，系统音从这里回环）或 `capture`（录音，麦克风）。
    flow: &'static str,
    is_default: bool,
}

fn hotkey_dtos(entries: &[HotkeyEntry]) -> Vec<HotkeyDto> {
    entries
        .iter()
        .map(|e| HotkeyDto {
            command: match e.command {
                Command::Start => "start",
                Command::Stop => "stop",
                Command::Quit => "quit",
            },
            text: e.text.clone(),
            ok: e.status == HotkeyStatus::Registered,
            error: if let HotkeyStatus::Failed(why) = &e.status { Some(why.clone()) } else { None },
        })
        .collect()
}

/// 配置本身的问题，加上保存目录能不能建出来（建得出来就顺手建好，开始录制时不必再等）。
fn save_issues(config: &Config, videos_dir: &Path) -> Vec<IssueDto> {
    let mut issues: Vec<IssueDto> = config.validate().into_iter().map(|i| IssueDto { key: i.key.to_string(), message: i.message }).collect();
    let dir = resolve_output_dir(config, videos_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        issues.push(IssueDto { key: "output_dir".into(), message: format!("保存目录不能使用（{}）：{e}", dir.display()) });
    }
    issues
}

#[tauri::command]
fn load_settings(shared: State<'_, Shared>) -> SettingsDto {
    SettingsDto {
        config: current_config(&shared),
        first_run: !shared.paths.config_file.exists(),
        config_error: lock(&shared.config_error).clone(),
        default_output_dir: shared.videos_dir.join("Record").display().to_string(),
        hotkeys: hotkey_dtos(&lock(&shared.hotkeys).entries),
        config_path: shared.paths.config_file.display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<'_, Shared>, config: Config) -> Result<SaveResultDto, String> {
    let issues = save_issues(&config, &shared.videos_dir);
    if !issues.is_empty() {
        return Ok(SaveResultDto { issues, hotkeys: hotkey_dtos(&lock(&shared.hotkeys).entries) });
    }
    persist(&shared, config.clone())?;
    let entries = apply_hotkeys(&app, &config);
    let conflicts = hotkeys::conflict_lines(&entries);
    refresh_menu_labels(&app);
    let _ = shared.items.autostart.set_checked(config.autostart);
    sync_autostart(&app, config.autostart);
    tracing::info!("设置已保存");
    if !conflicts.is_empty() {
        notify(&app, Notice::HotkeyConflict(conflicts));
    }
    Ok(SaveResultDto { issues: Vec::new(), hotkeys: hotkey_dtos(&entries) })
}

#[tauri::command]
fn list_devices() -> Result<Vec<DeviceDto>, String> {
    list_active().map(|devices| devices.into_iter().map(|d| DeviceDto { id: d.id, name: d.name, flow: if d.flow == Flow::Render { "render" } else { "capture" }, is_default: d.is_default }).collect())
}

#[tauri::command]
fn open_folder(shared: State<'_, Shared>, which: String) -> Result<(), String> {
    match which.as_str() {
        "recordings" => open_in_explorer(&resolve_output_dir(&current_config(&shared), &shared.videos_dir)),
        "logs" => open_in_explorer(&shared.paths.log_dir),
        other => return Err(format!("不认识的文件夹：{other}")),
    }
    Ok(())
}

/// 设置窗口里录入热键期间暂停全局热键：否则按下已经注册的组合，会真的开始或结束一场录制。
#[tauri::command]
fn pause_hotkeys(app: AppHandle) {
    match app.global_shortcut().unregister_all() {
        Ok(()) => tracing::info!("热键已暂停：设置窗口正在录入热键"),
        Err(e) => tracing::warn!("暂停热键失败：{e}"),
    }
}

#[tauri::command]
fn resume_hotkeys(app: AppHandle) {
    resume(&app);
}

/// 按已保存的配置重新注册热键。设置窗口关闭时也会走一遍，免得窗口在录入中途被关掉后热键一直失效。
fn resume(app: &AppHandle) {
    let shared = app.state::<Shared>();
    apply_hotkeys(app, &current_config(&shared));
    refresh_menu_labels(app);
}

#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.close();
    }
}

// ───────────────────────────── 启动 ─────────────────────────────

pub fn run() {
    let paths = Paths::from_env().unwrap_or_else(|e| fatal(&e.to_string()));
    let log_guard = logging::init(&paths.log_dir);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %paths.config_file.display(), "启动");
    // COM 的多线程单元要在整个进程生命周期里都存在：每场录制结束、采集线程全部退出时，它不能跟着被拆掉，
    // 否则下一条系统通知会撞上已经被卸载的组件而崩溃（原因见 record_capture::com）。
    record_capture::com::keep_mta_alive();

    let (config, config_error) = match Config::load(&paths.config_file) {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };
    let first_run = config_error.is_none() && !paths.config_file.exists();
    let shared_config: SharedConfig = Arc::new(RwLock::new(config.clone()));

    let app = tauri::Builder::default()
        // 单实例插件必须第一个注册：再次启动只会唤起已运行实例的设置窗口
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            tracing::info!(?argv, "又启动了一个实例，已拦下，打开设置窗口");
            show_settings(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let shared = app.state::<Shared>();
                    let command = lock(&shared.hotkeys).ids.iter().find(|(id, _)| *id == shortcut.id()).map(|(_, command)| *command);
                    if let Some(command) = command {
                        tracing::info!(?command, %shortcut, "热键");
                        shared.handle.command(command);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![load_settings, save_settings, list_devices, open_folder, pause_hotkeys, resume_hotkeys, close_settings])
        .on_window_event(|window, event| {
            if window.label() == SETTINGS_WINDOW && matches!(event, tauri::WindowEvent::Destroyed) {
                resume(window.app_handle());
            }
        })
        .setup(move |app| {
            let videos_dir = app.path().video_dir().unwrap_or_else(|_| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Videos"));
            let ui_app = app.handle().clone();
            let handle = runner::spawn(EngineBackend::new(shared_config.clone(), videos_dir.clone()), move |event| on_ui(&ui_app, event));

            let start = MenuItem::with_id(app, "start", hotkeys::command_name(Command::Start), true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", hotkeys::command_name(Command::Stop), false, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", hotkeys::command_name(Command::Quit), true, None::<&str>)?;
            let autostart = CheckMenuItem::with_id(app, "autostart", "开机自启", true, config.autostart, None::<&str>)?;
            let recordings = MenuItem::with_id(app, "recordings", "打开录音文件夹", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
            let logs = MenuItem::with_id(app, "logs", "打开日志文件夹", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&start, &stop, &PredefinedMenuItem::separator(app)?, &recordings, &settings, &logs, &autostart, &PredefinedMenuItem::separator(app)?, &quit])?;
            let tray = TrayIconBuilder::with_id(TRAY_ID)
                .icon(tray_icon(Look::Standby))
                .tooltip(tray_model::tooltip(RunState::Idle, Duration::ZERO, 0, &[]))
                .menu(&menu)
                .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
                .build(app)?;
            app.manage(tray);
            tracing::info!("托盘图标已创建");

            app.manage(Shared {
                paths: paths.clone(),
                videos_dir: videos_dir.clone(),
                config: shared_config.clone(),
                config_error: Mutex::new(config_error.clone()),
                handle: handle.clone(),
                hotkeys: Mutex::new(Registered::default()),
                view: Mutex::new(View {
                    state: RunState::Idle,
                    progress: Progress::default(),
                    warnings: Vec::new(),
                    shown: Some((Look::Standby, tray_model::tooltip(RunState::Idle, Duration::ZERO, 0, &[]))),
                }),
                items: TrayItems { start, stop, quit, autostart },
            });

            let app_handle = app.handle().clone();
            let entries = apply_hotkeys(&app_handle, &config);
            refresh_menu_labels(&app_handle);
            let conflicts = hotkeys::conflict_lines(&entries);
            if !conflicts.is_empty() {
                notify(&app_handle, Notice::HotkeyConflict(conflicts));
            }
            if let Some(why) = &config_error {
                notify(&app_handle, Notice::ConfigInvalid(why.clone()));
            }
            // 首次使用：把自启项交给向导决定，之后才与配置同步；已经有配置的，每次启动都对一遍（重装、换路径后自启项会过期）
            if !first_run {
                sync_autostart(&app_handle, config.autostart);
            }

            let power_handle = handle.clone();
            power::spawn(move |event| match event {
                power::Event::Suspend => {
                    tracing::info!("系统即将睡眠，保存本场");
                    let _ = power_handle.auto(EndReason::Sleep).recv_timeout(SLEEP_WAIT);
                }
                power::Event::EndSession(reason) => {
                    tracing::info!(?reason, "系统结束会话，保存本场并退出");
                    let _ = power_handle.end_session(reason).recv_timeout(END_SESSION_WAIT);
                }
            })?;

            // 崩溃恢复放在后台：不拖慢托盘和热键就绪
            let recovery_app = app_handle.clone();
            let recovery_dir = resolve_output_dir(&config, &videos_dir);
            std::thread::spawn(move || match record_mux::locate::find_ffmpeg() {
                Ok(ffmpeg) => {
                    let (repaired, failed) = recover_on_startup(&ffmpeg, &recovery_dir);
                    if repaired + failed > 0 {
                        notify(&recovery_app, Notice::Recovered { count: repaired, failed });
                    }
                }
                Err(e) => tracing::warn!("找不到 ffmpeg，跳过崩溃恢复检查：{e}"),
            });

            if first_run {
                show_settings(&app_handle);
            }
            tracing::info!("就绪");
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| fatal(&format!("创建应用失败：{e}")));

    let mut log_guard = log_guard;
    app.run(move |_app, event| match event {
        // 没有窗口时进程也要常驻：只有明确的 exit(code) 才放行
        RunEvent::ExitRequested { code, api, .. } if code.is_none() => api.prevent_exit(),
        RunEvent::Exit => drop(log_guard.take()),
        _ => {}
    });
}
```

- [ ] **步骤 3：确认能编译**

```powershell
cargo check -p record-shell
```

预期：通过，输出里有 `Finished`。

- [ ] **步骤 4：提交**

```powershell
git add -A
git commit -m "feat(shell): 外壳装配（插件、托盘、热键、设置窗口、前端命令）"
```

---

## 任务 11：record-shell：入口与 record 可执行文件

**目标：** 加上程序入口：没有控制台窗口，调 `app::run()`。

**文件：**

- 修改：`app/src-tauri/Cargo.toml` — record-shell 的依赖与可执行文件
- 创建：`app/src-tauri/src/main.rs`

- [ ] **步骤 1：修改 `app/src-tauri/Cargo.toml`**

加上 `record` 可执行文件，放在 `record-s5` 之前。

把这一段：

```toml
[[bin]]
name = "record-s5"
```

改成：

```toml
[[bin]]
name = "record"
path = "src/main.rs"

[[bin]]
name = "record-s5"
```

- [ ] **步骤 2：创建 `app/src-tauri/src/main.rs`**

```rust
//! Record：会议录制，常驻托盘，热键开始与保存。没有控制台窗口。
#![windows_subsystem = "windows"]

fn main() {
    record_shell::app::run();
}
```

- [ ] **步骤 3：编译 record**

```powershell
cargo build -p record-shell --bin record
```

预期：最后一行是 `Finished`。这一版嵌进去的是 `build.rs` 放的占位页，前端在下面的任务里做。

- [ ] **步骤 4：提交**

```powershell
git add -A
git commit -m "feat(shell): record 可执行文件（无控制台窗口）"
```

---

## 任务 12：前端骨架与热键录入（设置窗口，上）

**目标：** 设置窗口用 TypeScript + Vite + React，在需要时才由外壳创建（常驻时不占 WebView2 的内存）。先搭起前端工程，再用测试驱动写“把键盘事件变成 Tauri 热键字符串”的纯函数——它是设置界面里录入热键的核心。

**文件：**

- 创建：`app/package.json` — 前端依赖与脚本
- 创建：`app/tsconfig.json` — TypeScript 配置
- 创建：`app/vite.config.ts` — Vite 与 Vitest 配置
- 创建：`app/index.html` — 设置窗口的页面入口
- 创建：`app/pnpm-lock.yaml`
- 创建：`app/src/hotkey.test.ts` — 热键字符串的测试
- 创建：`app/src/hotkey.ts` — 键盘事件转 Tauri 热键字符串

- [ ] **步骤 1：创建 `app/package.json`**

```json
{
  "name": "record-app",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "test": "vitest run"
  },
  "dependencies": {
    "@tauri-apps/api": "^2.12.3",
    "react": "^19.3.0",
    "react-dom": "^19.3.0"
  },
  "devDependencies": {
    "@types/react": "^19.3.0",
    "@types/react-dom": "^19.3.0",
    "@vitejs/plugin-react": "^6.1.2",
    "typescript": "^7.0.2",
    "vite": "^8.3.4",
    "vitest": "^5.0.4"
  }
}
```

- [ ] **步骤 2：创建 `app/tsconfig.json`**

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "isolatedModules": true,
    "skipLibCheck": true,
    "noEmit": true,
    "types": ["vite/client"]
  },
  "include": ["src", "vite.config.ts"]
}
```

- [ ] **步骤 3：创建 `app/vite.config.ts`**

WebView2 是常青版 Chromium，不需要为旧浏览器降级编译。

```ts
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

// WebView2 是常青版 Chromium，不需要为旧浏览器降级编译
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: 'chrome120', outDir: 'dist', emptyOutDir: true },
  test: { environment: 'node', include: ['src/**/*.test.ts'] },
});
```

- [ ] **步骤 4：创建 `app/index.html`**

```html
<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Record 设置</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **步骤 5：安装前端依赖**

```powershell
pnpm --dir app install
```

预期：安装成功，生成 `app/pnpm-lock.yaml`（要提交）。需要 Node 20 以上与 pnpm；`node_modules` 被 `.gitignore` 挡住。

- [ ] **步骤 6：创建 `app/src/hotkey.test.ts`**

先写测试：修饰键的规范写法与顺序、功能键、方向键、没有修饰键时不算热键，等等。

```ts
import { describe, expect, it } from 'vitest';
import { hotkeyFromEvent, type KeyLike } from './hotkey';

const press = (code: string, mods: Partial<Omit<KeyLike, 'code'>> = {}): KeyLike => ({ code, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...mods });

describe('hotkeyFromEvent', () => {
  it('writes modifiers in a fixed order whatever the order they were pressed in', () => {
    expect(hotkeyFromEvent(press('KeyR', { ctrlKey: true, altKey: true }))).toBe('Ctrl+Alt+R');
    expect(hotkeyFromEvent(press('F12', { shiftKey: true, ctrlKey: true }))).toBe('Ctrl+Shift+F12');
    expect(hotkeyFromEvent(press('Digit9', { metaKey: true, altKey: true }))).toBe('Alt+Win+9');
  });

  it('accepts the same main keys as the Rust side: letters, digits, F1-F24, Space', () => {
    expect(hotkeyFromEvent(press('Space', { ctrlKey: true, altKey: true }))).toBe('Ctrl+Alt+Space');
    expect(hotkeyFromEvent(press('F24', { altKey: true }))).toBe('Alt+F24');
    expect(hotkeyFromEvent(press('F1', { altKey: true }))).toBe('Alt+F1');
    expect(hotkeyFromEvent(press('F25', { altKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('F0', { altKey: true }))).toBeNull();
  });

  it('keeps waiting while only a modifier is down, or the key is not supported', () => {
    expect(hotkeyFromEvent(press('ControlLeft', { ctrlKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('AltRight', { altKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('Enter', { ctrlKey: true }))).toBeNull();
    expect(hotkeyFromEvent(press('Numpad1', { ctrlKey: true }))).toBeNull();
  });

  it('refuses a key without any modifier, which would swallow ordinary typing', () => {
    expect(hotkeyFromEvent(press('KeyR'))).toBeNull();
    expect(hotkeyFromEvent(press('F5'))).toBeNull();
  });
});
```

- [ ] **步骤 7：运行测试，确认它失败**

```powershell
pnpm --dir app test
```

预期：**失败**（找不到 `./hotkey`）。

- [ ] **步骤 8：创建 `app/src/hotkey.ts`**

```ts
/** `KeyboardEvent` 里这里用到的部分，方便不依赖 DOM 来测试。 */
export interface KeyLike {
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/** 与 Rust 端 `Hotkey::parse` 一致：字母、数字、F1–F24、Space。 */
function mainKey(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1] ?? null;
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1] ?? null;
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  if (code === 'Space') return 'Space';
  return null;
}

/**
 * 把一次按键翻译成配置里的写法（如 `Ctrl+Alt+R`，修饰键顺序固定为 Ctrl、Alt、Shift、Win）。
 * 只按了修饰键、没有修饰键、或主键不被支持时返回 `null`，界面继续等下一次按键。
 */
export function hotkeyFromEvent(e: KeyLike): string | null {
  const key = mainKey(e.code);
  if (!key) return null;
  const mods = [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Win'].filter((m): m is string => Boolean(m));
  if (mods.length === 0) return null;
  return [...mods, key].join('+');
}
```

- [ ] **步骤 9：运行测试，确认通过**

```powershell
pnpm --dir app test
```

预期：`Tests  4 passed (4)`。

- [ ] **步骤 10：提交**

```powershell
git add -A
git commit -m "feat(app): 前端工程与热键录入"
```

---

## 任务 13：设置窗口（下）：接口、界面与样式

**目标：** 设置窗口本身：首次运行的向导（保存位置、热键、开机自启三项，加同意提示与“把图标拖到任务栏”的提示）和完整设置（画面、声音、提示方式、自动保护、热键）。录入热键时通知外壳暂停全局热键，录完或关窗后恢复；保存时由外壳校验并重新注册。

**文件：**

- 创建：`app/src/types.ts` — 与外壳命令对应的类型
- 创建：`app/src/api.ts` — 对 Tauri invoke 的薄封装
- 创建：`app/src/Settings.tsx` — 向导与完整设置界面
- 创建：`app/src/main.tsx` — 前端入口
- 创建：`app/src/styles.css` — 样式

- [ ] **步骤 1：创建 `app/src/types.ts`**

与外壳的 `load_settings` / `save_settings` / `list_devices` 命令对应的类型。

```ts
export type Encoder = 'auto' | 'h264_nvenc' | 'h264_qsv' | 'h264_amf' | 'libopenh264';
export type SystemCapture = 'default_endpoint' | 'communications_endpoint' | 'all_endpoints';
export type NotifyMode = 'quiet' | 'standard';

/**
 * 与 Rust 端的 `Config` 一一对应。这里只写设置界面会改的键；其余（timeline、health、
 * 以后子项目 B、C 的键）在索引签名里原样带回去，保存时不会丢。
 */
export interface Config {
  output_dir?: string | null;
  hotkeys: { start: string; stop: string; quit: string };
  video: { fps: number; max_long_edge: number; max_short_edge: number; encoder: Encoder };
  /** `"foreground"` 或从 1 开始的显示器序号。 */
  monitor: string | number;
  audio: { mic_device: string; system_device: string; system_capture: SystemCapture; [key: string]: unknown };
  notify: { mode: NotifyMode };
  limits: { max_hours: number; min_free_gb: number };
  autostart: boolean;
  [key: string]: unknown;
}
```

- [ ] **步骤 2：创建 `app/src/api.ts`**

对 Tauri `invoke` 的薄封装：读写设置、列设备、打开文件夹、暂停与恢复热键、关闭设置窗口。

```ts
import { invoke } from '@tauri-apps/api/core';
import type { Config } from './types';

export interface Issue {
  key: string;
  message: string;
}

export interface HotkeyState {
  command: 'start' | 'stop' | 'quit';
  text: string;
  ok: boolean;
  error: string | null;
}

export interface Settings {
  config: Config;
  first_run: boolean;
  config_error: string | null;
  default_output_dir: string;
  hotkeys: HotkeyState[];
  config_path: string;
  version: string;
}

export interface SaveResult {
  issues: Issue[];
  hotkeys: HotkeyState[];
}

export interface Device {
  id: string;
  name: string;
  flow: 'render' | 'capture';
  is_default: boolean;
}

export const loadSettings = () => invoke<Settings>('load_settings');
export const saveSettings = (config: Config) => invoke<SaveResult>('save_settings', { config });
export const listDevices = () => invoke<Device[]>('list_devices');
export const openFolder = (which: 'recordings' | 'logs') => invoke<void>('open_folder', { which });
export const closeSettings = () => invoke<void>('close_settings');
/** 录入热键时先暂停：否则按下已经注册的组合，会真的开始或结束一场录制。 */
export const pauseHotkeys = () => invoke<void>('pause_hotkeys');
export const resumeHotkeys = () => invoke<void>('resume_hotkeys');
```

- [ ] **步骤 3：创建 `app/src/Settings.tsx`**

```tsx
import { useEffect, useState, type ReactNode } from 'react';
import { closeSettings, listDevices, loadSettings, openFolder, pauseHotkeys, resumeHotkeys, saveSettings, type Device, type HotkeyState, type Issue, type Settings } from './api';
import { hotkeyFromEvent } from './hotkey';
import type { Config, Encoder, NotifyMode, SystemCapture } from './types';

const RESOLUTIONS = [
  { label: '1920 × 1080（推荐）', long: 1920, short: 1080 },
  { label: '1280 × 720（文件更小）', long: 1280, short: 720 },
  { label: '2560 × 1440', long: 2560, short: 1440 },
];

const ENCODERS: [Encoder, string][] = [
  ['auto', '自动选择（NVENC → QSV → AMF）'],
  ['h264_nvenc', 'NVIDIA NVENC'],
  ['h264_qsv', 'Intel QSV'],
  ['h264_amf', 'AMD AMF'],
  ['libopenh264', '软件编码（占用 CPU 多，没有硬件编码器时才手动选）'],
];

const SYSTEM_CAPTURES: [SystemCapture, string][] = [
  ['default_endpoint', '默认播放设备（推荐）'],
  ['communications_endpoint', '默认通信播放设备（会议软件常用）'],
];

const HOTKEY_ROWS = [
  { command: 'start', label: '开始录制' },
  { command: 'stop', label: '保存并结束本场' },
  { command: 'quit', label: '保存并退出程序' },
] as const;

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h2>{title}</h2>
      {children}
    </section>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="row">
      <label>{label}</label>
      <div className="control">
        {children}
        {hint && <p className="hint">{hint}</p>}
      </div>
    </div>
  );
}

/** 点进去后按下想用的组合键即可；录入期间暂停全局热键，免得按下旧组合时真的开始或结束录制。 */
function HotkeyRow({ label, value, state, onChange }: { label: string; value: string; state?: HotkeyState; onChange: (hotkey: string) => void }) {
  const [capturing, setCapturing] = useState(false);
  return (
    <Row label={label}>
      <input
        readOnly
        className={capturing ? 'hotkey capturing' : 'hotkey'}
        value={capturing ? '请按下新的组合键…' : value}
        onFocus={() => {
          setCapturing(true);
          void pauseHotkeys();
        }}
        onBlur={() => {
          setCapturing(false);
          void resumeHotkeys();
        }}
        onKeyDown={(e) => {
          e.preventDefault();
          const hotkey = hotkeyFromEvent(e.nativeEvent);
          if (hotkey) {
            onChange(hotkey);
            e.currentTarget.blur(); // 录入完成：失焦后显示新组合，并恢复全局热键
          }
        }}
      />
      {state && (state.ok ? <span className="badge ok">已注册</span> : <span className="badge bad" title={state.error ?? ''}>{state.error ?? '没注册成功'}</span>)}
    </Row>
  );
}

function DeviceSelect({ value, devices, defaultLabel, onChange }: { value: string; devices: Device[]; defaultLabel: string; onChange: (id: string) => void }) {
  const known = value === 'default' || devices.some((d) => d.id === value);
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      <option value="default">{defaultLabel}</option>
      {devices.map((d) => (
        <option key={d.id} value={d.id}>
          {d.name}
          {d.is_default ? '（当前默认）' : ''}
        </option>
      ))}
      {!known && <option value={value}>{value}（现在找不到这个设备）</option>}
    </select>
  );
}

export function SettingsView() {
  const [data, setData] = useState<Settings | null>(null);
  const [config, setConfig] = useState<Config | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [hotkeys, setHotkeys] = useState<HotkeyState[]>([]);
  const [issues, setIssues] = useState<Issue[]>([]);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    loadSettings()
      .then((s) => {
        setData(s);
        setConfig(s.config);
        setHotkeys(s.hotkeys);
      })
      .catch((e) => setError(String(e)));
    listDevices()
      .then(setDevices)
      .catch(() => setDevices([]));
  }, []);

  if (error && !config) return <main className="page"><p className="error">读取设置失败：{error}</p></main>;
  if (!data || !config) return <main className="page"><p>正在读取设置…</p></main>;

  const firstRun = data.first_run;
  const patch = (change: Partial<Config>) => setConfig({ ...config, ...change });
  const resolution = RESOLUTIONS.findIndex((r) => r.long === config.video.max_long_edge && r.short === config.video.max_short_edge);

  async function save() {
    if (!config) return;
    setBusy(true);
    setMessage('');
    setError('');
    try {
      const result = await saveSettings(config);
      setIssues(result.issues);
      setHotkeys(result.hotkeys);
      if (result.issues.length > 0) return;
      if (firstRun) {
        await closeSettings();
        return;
      }
      setMessage(result.hotkeys.some((h) => !h.ok) ? '已保存，但有热键没注册成功，见上面的提示。' : '已保存。画面、声音、保护方面的修改从下一场录制开始生效。');
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const hotkeySection = (
    <Section title="热键">
      {HOTKEY_ROWS.map(({ command, label }) => (
        <HotkeyRow key={command} label={label} value={config.hotkeys[command]} state={hotkeys.find((h) => h.command === command)} onChange={(hotkey) => patch({ hotkeys: { ...config.hotkeys, [command]: hotkey } })} />
      ))}
      <p className="hint">点一下输入框，再按下想用的组合键（至少带 Ctrl、Alt、Shift、Win 之一；主键可以是字母、数字、F1–F24 或空格）。热键在任何程序前台时都有效，包括全屏的会议软件。</p>
    </Section>
  );

  const folderSection = (
    <Section title="保存位置">
      <Row label="保存目录" hint={`留空则保存到 ${data.default_output_dir}`}>
        <input type="text" value={config.output_dir ?? ''} placeholder={data.default_output_dir} onChange={(e) => patch({ output_dir: e.target.value })} />
        <button type="button" onClick={() => void openFolder('recordings')}>打开</button>
      </Row>
    </Section>
  );

  const autostartSection = (
    <Section title="开机自启">
      <Row label="登录 Windows 后自动运行">
        <input type="checkbox" checked={config.autostart} onChange={(e) => patch({ autostart: e.target.checked })} />
      </Row>
    </Section>
  );

  return (
    <main className="page">
      <h1>{firstRun ? '欢迎使用 Record' : 'Record 设置'}</h1>
      {data.config_error && <p className="error">配置文件读不懂（{data.config_error}），现在显示的是默认值。保存时会把原文件改名为 config.toml.broken 留底。</p>}
      {firstRun && (
        <div className="intro">
          <p>Record 常驻在系统托盘里，用热键开始和保存会议录制，同时录下屏幕画面、麦克风和对方的声音。先确认下面三件事，之后随时可以从托盘菜单的“设置…”再改。</p>
          <p>录制前请告知与会者并取得同意。Record 没有隐身模式：录制期间托盘图标会变红。</p>
          <p>Windows 11 默认把新图标收在任务栏右下角的“^”里：点开“^”，把 Record 的图标拖到任务栏上，录制状态就能一直看到。</p>
        </div>
      )}
      {issues.length > 0 && (
        <div className="error">
          <p>没有保存，请先改正：</p>
          <ul>{issues.map((i) => <li key={i.key + i.message}>{i.message}</li>)}</ul>
        </div>
      )}
      {error && <p className="error">{error}</p>}

      {folderSection}
      {hotkeySection}

      {!firstRun && (
        <>
          <Section title="画面">
            <Row label="帧率">
              <select value={config.video.fps} onChange={(e) => patch({ video: { ...config.video, fps: Number(e.target.value) } })}>
                {[10, 15, 30].map((fps) => <option key={fps} value={fps}>{fps} 帧/秒{fps === 15 ? '（推荐）' : ''}</option>)}
              </select>
            </Row>
            <Row label="分辨率上限" hint="超过的画面会缩小后再编码；比这个小的不放大。">
              <select value={resolution} onChange={(e) => { const r = RESOLUTIONS[Number(e.target.value)]; if (r) patch({ video: { ...config.video, max_long_edge: r.long, max_short_edge: r.short } }); }}>
                {resolution < 0 && <option value={-1}>自定义（{config.video.max_long_edge} × {config.video.max_short_edge}）</option>}
                {RESOLUTIONS.map((r, i) => <option key={r.label} value={i}>{r.label}</option>)}
              </select>
            </Row>
            <Row label="视频编码器">
              <select value={config.video.encoder} onChange={(e) => patch({ video: { ...config.video, encoder: e.target.value as Encoder } })}>
                {ENCODERS.map(([id, label]) => <option key={id} value={id}>{label}</option>)}
              </select>
            </Row>
            <Row label="录哪块显示器">
              <select value={String(config.monitor)} onChange={(e) => patch({ monitor: e.target.value === 'foreground' ? 'foreground' : Number(e.target.value) })}>
                <option value="foreground">按下热键时前台窗口所在的显示器</option>
                {[1, 2, 3, 4].map((n) => <option key={n} value={String(n)}>显示器 {n}</option>)}
              </select>
            </Row>
          </Section>

          <Section title="声音">
            <Row label="麦克风">
              <DeviceSelect value={config.audio.mic_device} devices={devices.filter((d) => d.flow === 'capture')} defaultLabel="系统默认麦克风" onChange={(id) => patch({ audio: { ...config.audio, mic_device: id } })} />
            </Row>
            <Row label="系统声音（对方的声音）" hint="从这个播放设备回环采集；戴蓝牙耳机开会时，播放设备就是耳机。">
              <DeviceSelect value={config.audio.system_device} devices={devices.filter((d) => d.flow === 'render')} defaultLabel="系统默认播放设备" onChange={(id) => patch({ audio: { ...config.audio, system_device: id } })} />
            </Row>
            <Row label="系统声音的采集方案">
              <select value={config.audio.system_capture} onChange={(e) => patch({ audio: { ...config.audio, system_capture: e.target.value as SystemCapture } })}>
                {SYSTEM_CAPTURES.map(([id, label]) => <option key={id} value={id}>{label}</option>)}
              </select>
            </Row>
          </Section>

          <Section title="提示方式">
            <Row label="气泡通知" hint="两种模式下，出错、自动保存、热键没注册成功都会提示。">
              <select value={config.notify.mode} onChange={(e) => patch({ notify: { mode: e.target.value as NotifyMode } })}>
                <option value="quiet">安静：只有托盘图标变化，出错时才弹提示</option>
                <option value="standard">标准：另外在开始和保存时弹提示</option>
              </select>
            </Row>
          </Section>

          <Section title="自动保护">
            <Row label="单场最长（小时）" hint="到时间自动保存并结束。">
              <input type="number" min={0.5} max={24} step={0.5} value={config.limits.max_hours} onChange={(e) => patch({ limits: { ...config.limits, max_hours: Number(e.target.value) } })} />
            </Row>
            <Row label="磁盘剩余下限（GB）" hint="开始前和录制中剩余空间低于它，就不开始或自动保存。">
              <input type="number" min={1} max={1000} step={1} value={config.limits.min_free_gb} onChange={(e) => patch({ limits: { ...config.limits, min_free_gb: Number(e.target.value) } })} />
            </Row>
          </Section>
        </>
      )}

      {autostartSection}

      <footer>
        <button type="button" className="primary" disabled={busy} onClick={() => void save()}>{firstRun ? '完成' : '保存'}</button>
        {!firstRun && <button type="button" onClick={() => void closeSettings()}>关闭</button>}
        {!firstRun && <button type="button" onClick={() => void openFolder('logs')}>打开日志文件夹</button>}
        {message && <span className="message">{message}</span>}
      </footer>
      <p className="hint">版本 {data.version} · 配置文件 {data.config_path}</p>
    </main>
  );
}
```

- [ ] **步骤 4：创建 `app/src/main.tsx`**

```tsx
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { SettingsView } from './Settings';
import './styles.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <SettingsView />
  </StrictMode>,
);
```

- [ ] **步骤 5：创建 `app/src/styles.css`**

```css
:root {
  color-scheme: light dark;
  font-family: 'Segoe UI', 'Microsoft YaHei UI', system-ui, sans-serif;
  font-size: 14px;
  --line: color-mix(in srgb, currentColor 18%, transparent);
  --accent: #2563eb;
  --ok: #15803d;
  --bad: #b91c1c;
}

body {
  margin: 0;
}

.page {
  max-width: 720px;
  margin: 0 auto;
  padding: 20px 24px 32px;
}

h1 {
  font-size: 20px;
  margin: 0 0 12px;
}

h2 {
  font-size: 13px;
  margin: 24px 0 4px;
  padding-bottom: 4px;
  border-bottom: 1px solid var(--line);
  opacity: 0.8;
}

.row {
  display: flex;
  gap: 16px;
  align-items: flex-start;
  padding: 8px 0;
}

.row > label {
  flex: 0 0 180px;
  padding-top: 5px;
}

.control {
  flex: 1;
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}

.control > input[type='text'],
.control > select,
.control > input.hotkey {
  flex: 1 1 260px;
  min-width: 0;
}

input,
select,
button {
  font: inherit;
  padding: 4px 8px;
}

input.hotkey {
  cursor: pointer;
  font-family: Consolas, monospace;
}

input.capturing {
  outline: 2px solid var(--accent);
}

.hint {
  flex-basis: 100%;
  margin: 0;
  font-size: 12px;
  opacity: 0.7;
}

section > .hint {
  margin-top: 4px;
}

.badge {
  font-size: 12px;
  padding: 2px 8px;
  border-radius: 10px;
}

.badge.ok {
  color: var(--ok);
  border: 1px solid var(--ok);
}

.badge.bad {
  color: var(--bad);
  border: 1px solid var(--bad);
}

.intro {
  padding: 4px 12px;
  margin-bottom: 8px;
  border-left: 3px solid var(--accent);
  background: color-mix(in srgb, var(--accent) 8%, transparent);
}

.error {
  color: var(--bad);
  border-left: 3px solid var(--bad);
  padding: 4px 12px;
  margin: 8px 0;
}

.error p {
  margin: 4px 0;
}

footer {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-top: 28px;
}

button.primary {
  background: var(--accent);
  color: white;
  border: 0;
  border-radius: 4px;
  padding: 6px 20px;
}

.message {
  color: var(--ok);
}
```

- [ ] **步骤 6：类型检查并构建**

```powershell
pnpm --dir app build
```

预期：先是 `tsc --noEmit` 无输出，再是 Vite 的 `✓ built in …`，生成 `app/dist/`（被 `.gitignore` 挡住）。

- [ ] **步骤 7：提交**

```powershell
git add -A
git commit -m "feat(app): 设置窗口（向导与完整设置）"
```

---

## 任务 14：外壳的真机检查脚本 shell-check

**目标：** 外壳的价值在于“热键、托盘、系统事件、设置窗口”这些东西真的工作，单元测试覆盖不到。`scripts/shell-check.ps1` 启动真实的 `record.exe`，用模拟按键触发热键，只读它的日志、会话目录和设置窗口自己的页面（通过 WebView2 的远程调试端口，只连本机、只连这个程序的窗口），分四部分检查。

| 部分 | 检查什么 |
|---|---|
| `basic` | 热键开始 → 托盘变录制中、连按只开一场、录制中 `session.json` 是 recording、托盘提示随时间更新、日志写明已请求保持唤醒且整机电源状态里“系统不休眠”“显示器常亮”两位都在；热键保存 → 回到待命、日志写明已撤销保持唤醒、`meeting.mp4` 里 1 路画面 1 路混音；保存并退出；被杀后系统自动收回电源请求，重启后自动修复并通知（安静模式也通知）；标准模式弹开始 / 保存通知，安静模式不弹；标准模式下连续再录 5 场（每场的开始与保存都弹系统通知）进程不崩溃——这是验收时实测抓到的崩溃，原因与修法见计划 A2 的 `com` 一节 |
| `events` | 用窗口消息模拟睡眠、唤醒、关机：保存完才返回、提示“因睡眠已保存”、关机保存后进程退出、唤醒后热键仍可用 |
| `failures` | 磁盘剩余空间不足时开始失败并弹通知、不留会话目录；热键被别的程序占用时写明原因、菜单里标出冲突、另外两个照常；热键配置写错时三个都不注册并只说一遍原因、不改配置文件；配置读不懂时仍能启动并保留原文件 |
| `ui` | 没有配置文件时自动弹向导（三项、同意提示、托盘固定提示）、点“完成”写配置并关窗；单实例唤起设置窗口；录入热键时全局热键暂停、录完恢复、相同组合不保存、被占用的组合保存后标出；保存后新热键生效旧的失效；不认识的配置项原样保留；开机自启勾选与取消对应 Run 项；录入中途关窗恢复热键 |

脚本用环境变量把 `APPDATA`、`LOCALAPPDATA` 指到 `%TEMP%\record-shell` 下，所以**不会碰你真实的配置、日志和保存目录**；会录几秒真实的桌面（standard 模式的检查里会弹真实的系统通知），录出来的文件只用 ffprobe 看结构，不打开、不播放，跑完整个目录删除。

**文件：**

- 创建：`scripts/shell-check.ps1` — 外壳的真机检查：热键、托盘、系统事件、失败处理与设置窗口

- [ ] **步骤 1：创建 `scripts/shell-check.ps1`**

```powershell
# 外壳（record.exe）的真机检查：热键开始与保存、托盘与通知的决定、录制期间保持唤醒、退出与崩溃恢复、睡眠与关机事件、
# 启动失败、热键冲突、配置错误、首次向导与设置窗口。
#
# 用法：
#   $env:RECORD_FFMPEG = '<ffmpeg.exe 的完整路径>'
#   powershell -ExecutionPolicy Bypass -File scripts\shell-check.ps1 [-Exe target\debug\record.exe] [-Parts basic,events,failures,ui]
#
# 隔离：被测程序的 APPDATA、LOCALAPPDATA 都指向 %TEMP%\record-shell 下的目录，保存目录也在那里，
# 不碰真实的配置、日志和录音；结束时整个目录删掉。检查过程中会真的录几秒屏幕和声音，
# 但录出来的文件从不打开查看，只用 ffprobe 看流的结构，用完即删。
# 设置窗口的检查借助 WebView2 的远程调试端口（只对这次启动的进程开，端口绑在本机回环地址）。
param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\debug\record.exe'),
    [string[]]$Parts = @('basic', 'events', 'failures', 'ui'),
    [int]$CdpPort = 9333,
    [switch]$Keep
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$Parts = @($Parts | ForEach-Object { $_ -split ',' })   # powershell -File 传进来的 basic,events 是一整个字符串
$Exe = (Resolve-Path $Exe).Path
if (-not $env:RECORD_FFMPEG -or -not (Test-Path $env:RECORD_FFMPEG)) { throw '先把环境变量 RECORD_FFMPEG 设为 ffmpeg.exe 的完整路径' }
$Ffmpeg = (Resolve-Path $env:RECORD_FFMPEG).Path
$Ffprobe = Join-Path (Split-Path $Ffmpeg) 'ffprobe.exe'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ShellNative {
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll", SetLastError = true)] public static extern bool RegisterHotKey(IntPtr hWnd, int id, uint mods, uint vk);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr hWnd, int id);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("powrprof.dll")] static extern uint CallNtPowerInformation(int level, IntPtr input, uint inputLen, out uint output, uint outputLen);
    // 整机汇总的“执行状态”（各进程电源请求叠加的结果）：0x1 系统不因空闲休眠，0x2 显示器常亮。不需要管理员权限。
    public static uint ExecutionState() {
        uint state;
        uint status = CallNtPowerInformation(16, IntPtr.Zero, 0, out state, 4);   // 16 = SystemExecutionState
        if (status != 0) throw new Exception("CallNtPowerInformation 失败：0x" + status.ToString("X"));
        return state;
    }
    delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, System.Text.StringBuilder sb, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, System.Text.StringBuilder sb, int n);
    [DllImport("user32.dll")] static extern int GetWindowLongW(IntPtr h, int index);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr h, uint cmd);
    // 进程的全部顶层窗口，每个一行："可见|隐藏 宽x高 类名 标题"；会出现在任务栏上的（可见、无所有者、不是工具窗口或明确要求显示）前面加 [任务栏]
    public static string[] WindowsOf(uint pid) {
        var list = new System.Collections.Generic.List<string>();
        EnumWindows((h, p) => {
            uint owner; GetWindowThreadProcessId(h, out owner);
            if (owner == pid) {
                var cls = new System.Text.StringBuilder(256); GetClassNameW(h, cls, 256);
                var title = new System.Text.StringBuilder(256); GetWindowTextW(h, title, 256);
                RECT r; GetWindowRect(h, out r);
                int ex = GetWindowLongW(h, -20);
                bool visible = IsWindowVisible(h);
                bool taskbar = visible && GetWindow(h, 4) == IntPtr.Zero && ((ex & 0x80) == 0 || (ex & 0x40000) != 0);
                list.Add((taskbar ? "[任务栏] " : "") + (visible ? "可见" : "隐藏") + " " + (r.Right - r.Left) + "x" + (r.Bottom - r.Top) + " " + cls + " " + title);
            }
            return true;
        }, IntPtr.Zero);
        return list.ToArray();
    }
}
'@

$results = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [bool]$ok, [string]$detail = '') {
    $results.Add([pscustomobject]@{ 项目 = $name; 结果 = $(if ($ok) { '通过' } else { '失败' }); 说明 = $detail })
    '{0}  {1}  {2}' -f $(if ($ok) { '[通过]' } else { '[失败]' }), $name, $detail
}
function Info([string]$text) { 'INFO ' + $text }
# 整机电源状态（各进程电源请求叠加的结果）里“之前没有、现在有”的位，只看 0x1 系统不休眠与 0x2 显示器常亮。
# 别的程序也可能占着请求，所以“撤销了没有”只看多出来的位：它们只能是本程序造成的。
function Power-Leftover([int]$before, [int]$now) { $now -band 3 -band (3 -bxor ($before -band 3)) }

# ───────────── 辅助：隔离环境、启动、日志、按键 ─────────────
$work = Join-Path $env:TEMP 'record-shell'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$runBefore = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
$procs = New-Object System.Collections.Generic.List[object]

function New-Sandbox([string]$name) {
    $root = Join-Path $work $name
    $box = [pscustomobject]@{
        Name = $name; Root = $root
        Appdata = Join-Path $root 'appdata'; Local = Join-Path $root 'local'; Out = Join-Path $root 'out'
    }
    foreach ($d in $box.Appdata, $box.Local, $box.Out) { New-Item -ItemType Directory -Force $d | Out-Null }
    $box | Add-Member ConfigFile (Join-Path $box.Appdata 'record\config.toml')
    $box | Add-Member LogDir (Join-Path $box.Local 'record\logs')
    $box
}
function Write-Config($box, [string]$body) {
    New-Item -ItemType Directory -Force (Split-Path $box.ConfigFile) | Out-Null
    [System.IO.File]::WriteAllText($box.ConfigFile, $body, (New-Object System.Text.UTF8Encoding($false)))
}
function Standard-Config($box, [string]$mode = 'standard', [string]$extra = '') {
    "output_dir = '$($box.Out)'`n$extra`n[notify]`nmode = `"$mode`"`n"
}
function Start-App($box, [switch]$Cdp) {
    $saved = @{}
    foreach ($k in 'APPDATA', 'LOCALAPPDATA', 'RECORD_FFMPEG', 'RECORD_LOG', 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS') { $saved[$k] = [Environment]::GetEnvironmentVariable($k) }
    $env:APPDATA = $box.Appdata; $env:LOCALAPPDATA = $box.Local; $env:RECORD_FFMPEG = $Ffmpeg
    $env:RECORD_LOG = 'info,record_shell=debug'
    if ($Cdp) { $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$CdpPort" }
    try { $p = Start-Process -FilePath $Exe -PassThru }
    finally { foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
    $procs.Add($p)
    $p
}
function Stop-App($p) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force; [void]$p.WaitForExit(5000) } }
function Read-Log($box) {
    if (-not (Test-Path $box.LogDir)) { return @() }
    @(Get-ChildItem $box.LogDir -Filter 'record.log*' | Sort-Object Name | ForEach-Object {
            $fs = [System.IO.File]::Open($_.FullName, 'Open', 'Read', 'ReadWrite')
            try { $sr = New-Object System.IO.StreamReader($fs, [System.Text.Encoding]::UTF8); $sr.ReadToEnd() -split "`r?`n" } finally { $fs.Dispose() }
        } | Where-Object { $_ })
}
function Wait-Log($box, [string]$pattern, [int]$since = 0, [int]$timeoutMs = 5000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do {
        $hit = @(Read-Log $box | Select-Object -Skip $since | Where-Object { $_ -match $pattern })
        if ($hit.Count -gt 0) { return $hit }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::Now -lt $until)
    return @()
}
function Wait-Until([scriptblock]$cond, [int]$timeoutMs = 10000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do { if (& $cond) { return $true }; Start-Sleep -Milliseconds 100 } while ([DateTime]::Now -lt $until)
    return $false
}
function Ready($box, $p) {
    $hit = Wait-Log $box '就绪' 0 30000
    ($hit.Count -gt 0) -and -not $p.HasExited
}
$VK = @{ Ctrl = 0x11; Alt = 0x12; Shift = 0x10; R = 0x52; S = 0x53; Q = 0x51; F8 = 0x77; F9 = 0x78 }
function Press-Chord([string[]]$names) {
    $codes = @($names | ForEach-Object { [byte]$VK[$_] })
    foreach ($c in $codes) { [ShellNative]::keybd_event($c, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
    [array]::Reverse($codes)
    foreach ($c in $codes) { [ShellNative]::keybd_event($c, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 30 }
}
function Sessions($box) { @(Get-ChildItem $box.Out -Directory -ErrorAction SilentlyContinue | Where-Object { Test-Path (Join-Path $_.FullName 'session.json') } | Sort-Object Name) }
function Read-Session($dir) { Get-Content (Join-Path $dir 'session.json') -Raw -Encoding UTF8 | ConvertFrom-Json }
function Probe-Streams([string]$mp4) {
    $json = & $Ffprobe -v error -show_entries 'stream=codec_type,codec_name:format=duration' -of json $mp4 | ConvertFrom-Json
    [pscustomobject]@{ Video = @($json.streams | Where-Object { $_.codec_type -eq 'video' }).Count; Audio = @($json.streams | Where-Object { $_.codec_type -eq 'audio' }).Count; Duration = [double]$json.format.duration }
}
function Cpu-Percent($p, [int]$secs) {
    $a = (Get-Process -Id $p.Id).TotalProcessorTime.TotalSeconds
    Start-Sleep -Seconds $secs
    ((Get-Process -Id $p.Id).TotalProcessorTime.TotalSeconds - $a) / $secs * 100
}

# ───────────── 辅助：通过 WebView2 的远程调试端口操作设置窗口 ─────────────
function Connect-Cdp([string]$titleMatch, [int]$timeoutMs = 20000) {
    $until = [DateTime]::Now.AddMilliseconds($timeoutMs)
    do {
        try {
            $t = @(Invoke-RestMethod "http://127.0.0.1:$CdpPort/json" -TimeoutSec 2 | Where-Object { $_.type -eq 'page' -and $_.title -match $titleMatch })
            if ($t.Count -gt 0) {
                $ws = New-Object System.Net.WebSockets.ClientWebSocket
                $ws.ConnectAsync([Uri]$t[0].webSocketDebuggerUrl, [Threading.CancellationToken]::None).Wait()
                return [pscustomobject]@{ Ws = $ws; Id = 0 }
            }
        } catch { }
        Start-Sleep -Milliseconds 300
    } while ([DateTime]::Now -lt $until)
    $null
}
function Cdp-Call($c, [string]$method, $params = @{}) {
    $c.Id++
    $bytes = [Text.Encoding]::UTF8.GetBytes((@{ id = $c.Id; method = $method; params = $params } | ConvertTo-Json -Depth 20 -Compress))
    $c.Ws.SendAsync([ArraySegment[byte]]$bytes, 'Text', $true, [Threading.CancellationToken]::None).Wait()
    while ($true) {
        $sb = New-Object Text.StringBuilder
        do {
            $buf = New-Object byte[] 65536
            $r = $c.Ws.ReceiveAsync([ArraySegment[byte]]$buf, [Threading.CancellationToken]::None).Result
            [void]$sb.Append([Text.Encoding]::UTF8.GetString($buf, 0, $r.Count))
        } while (-not $r.EndOfMessage)
        $msg = $sb.ToString() | ConvertFrom-Json
        if ($msg.id -eq $c.Id) {
            if ($msg.error) { throw "CDP $method 出错：$($msg.error.message)" }
            return $msg.result
        }
    }
}
function Cdp-Eval($c, [string]$js) {
    $r = Cdp-Call $c 'Runtime.evaluate' @{ expression = $js; returnByValue = $true; awaitPromise = $true }
    if ($r.exceptionDetails) { throw "页面脚本出错：$($r.exceptionDetails.exception.description)" }
    $r.result.value
}
function Cdp-Shot($c, [string]$path) {
    $r = Cdp-Call $c 'Page.captureScreenshot' @{ format = 'png' }
    [System.IO.File]::WriteAllBytes($path, [Convert]::FromBase64String($r.data))
}
# 页面里用的小工具：按标签找行、给受控输入框赋值、点按钮
$jsLib = @'
window.__t = {
  row: (label) => [...document.querySelectorAll('.row')].find((r) => r.querySelector('label').innerText.trim() === label),
  set: (el, v) => { const d = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value'); d.set.call(el, v); el.dispatchEvent(new Event(el.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true })); },
  click: (text) => { const b = [...document.querySelectorAll('button')].find((x) => x.innerText.trim() === text); if (!b) throw new Error('没有按钮 ' + text); b.click(); },
  key: (el, code, mods) => el.dispatchEvent(new KeyboardEvent('keydown', Object.assign({ code, bubbles: true, cancelable: true }, mods))),
  text: () => document.body.innerText,
};
'@

Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $work | Out-Null
$free = [math]::Round((Get-PSDrive -Name ($work.Substring(0, 1))).Free / 1GB)

try {
    # ───────────────────────── basic：热键、托盘、通知、退出、崩溃恢复 ─────────────────────────
    if ($Parts -contains 'basic') {
        '=== basic ==='
        $mods = 0x0002 -bor 0x0001 -bor 0x4000
        foreach ($k in @{ R = 0x52; S = 0x53; Q = 0x51 }.GetEnumerator()) {
            $ok = [ShellNative]::RegisterHotKey([IntPtr]::Zero, 100, $mods, $k.Value)
            if ($ok) { [void][ShellNative]::UnregisterHotKey([IntPtr]::Zero, 100) }
            Check "启动前 Ctrl+Alt+$($k.Key) 没被别的程序占用" $ok ''
        }
        $box = New-Sandbox 'basic'
        Write-Config $box (Standard-Config $box 'standard')
        $app = Start-App $box
        Check '进程启动并就绪' (Ready $box $app) ''
        $log = Read-Log $box
        Check '三个热键都注册成功' (@($log | Where-Object { $_ -match '热键已注册' }).Count -eq 3) (($log | Where-Object { $_ -match '热键' }) -join ' | ')
        $wins = [ShellNative]::WindowsOf([uint32]$app.Id)
        # Tauri 和 tao 内部有几个 15×15 的“可见”工具窗口（单实例、事件循环），不会出现在任务栏上；这里按任务栏的规则判断
        Check '没有会出现在任务栏上的窗口（托盘常驻）' (@($wins | Where-Object { $_ -like '`[任务栏`]*' }).Count -eq 0) ($wins -join ' | ')
        # 资源管理器收到图标后才在注册表里留记录，要等一会儿
        $trayNow = { @(Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings' -ErrorAction SilentlyContinue | ForEach-Object { Get-ItemProperty $_.PSPath } | Where-Object { $_.ExecutablePath -eq $Exe }) }
        [void](Wait-Until { (& $trayNow).Count -gt 0 } 8000)
        $tray = & $trayNow
        Check '托盘图标创建成功' ([bool]($log -match '托盘图标已创建')) ''
        # 资源管理器只在第一次见到这个路径时留记录，清理后重复运行不一定再有，所以只报告不判定
        Info ('资源管理器的托盘登记：' + $(if ($tray.Count) { "有，提示文字 $($tray[0].InitialTooltip)" } else { '没有（重复运行时正常）' }))

        Start-Sleep -Seconds 2
        $p = Get-Process -Id $app.Id
        Info ('待命：工作集 {0:N1} MB，私有内存 {1:N1} MB' -f ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB))
        $idleCpu = Cpu-Percent $app 8
        Check '待命时 CPU 低于 1%' ($idleCpu -lt 1) ('{0:N2}%' -f $idleCpu)

        # 热键开始：连按两次（间隔约 0.2 秒）只应开始一场
        $power0 = [int][ShellNative]::ExecutionState()
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Press-Chord 'Ctrl', 'Alt', 'R'
        $icon = Wait-Log $box '托盘图标更新.*Recording' $n 10000
        Check '热键开始 → 托盘变成录制中' ($icon.Count -gt 0) ($icon -join ' ')
        $started = Wait-Log $box '通知.*show=true.*开始录制' $n 3000
        Check '标准模式：开始时弹通知' ($started.Count -gt 0) ($started -join ' ')
        Start-Sleep -Milliseconds 800
        Check '连按两次只开始一场' ((Sessions $box).Count -eq 1) "会话数 $((Sessions $box).Count)"
        $dir = (Sessions $box)[0].FullName
        Check '录制中 session.json 的状态是 recording' ((Read-Session $dir).state -eq 'recording') ''
        $held = Wait-Log $box '已请求保持唤醒' $n 3000
        Check '录制中：日志写明已请求保持唤醒（显示器常亮、系统不因空闲休眠）' ($held.Count -gt 0) ($held -join ' ')
        $power1 = [int][ShellNative]::ExecutionState()
        Check '录制中：整机电源状态里“系统不休眠”“显示器常亮”两位都在' (($power1 -band 3) -eq 3) ('录制前 0x{0:X}，录制中 0x{1:X}' -f $power0, $power1)
        Start-Sleep -Seconds 4
        $tips = @(Read-Log $box | Where-Object { $_ -match '托盘提示更新.*录制中' })
        Check '录制中托盘提示随时间更新' ($tips.Count -ge 2) "提示更新 $($tips.Count) 次，最后一条：$($tips[-1])"
        # ffmpeg 写文件不刷盘，目录项里的大小会一直是 0；提示里的“已写入”要读真实大小才会增长
        $shownMb = if ($tips.Count -gt 0 -and $tips[-1] -match '已写入 (\d+) MB') { [int]$Matches[1] } else { -1 }
        Check '录制中托盘提示里的“已写入”随录制增长（不是一直 0 MB）' ($shownMb -gt 0) "最后一条：$($tips[-1])"
        $recCpu = Cpu-Percent $app 6
        Info ('录制中：外壳进程 CPU {0:N1}%（单核口径）' -f $recCpu)

        # 热键保存
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'S'
        $idle = Wait-Log $box '托盘图标更新.*Standby' $n 20000
        Check '热键保存 → 托盘回到待命' ($idle.Count -gt 0) ''
        $released = Wait-Log $box '已撤销保持唤醒' $n 5000
        $power2 = [int][ShellNative]::ExecutionState()
        Check '保存后：日志写明已撤销保持唤醒，录制前没有的电源位都清掉了' (($released.Count -gt 0) -and ((Power-Leftover $power0 $power2) -eq 0)) ('录制前 0x{0:X}，录制中 0x{1:X}，保存后 0x{2:X}' -f $power0, $power1, $power2)
        $saved = Wait-Log $box '通知.*show=true.*已保存' $n 3000
        Check '标准模式：保存时弹通知' ($saved.Count -gt 0) ($saved -join ' ')
        $meeting = Join-Path $dir 'meeting.mp4'
        Check '生成了 meeting.mp4' (Test-Path $meeting) ''
        $meta = Read-Session $dir
        Check 'session.json：finalized，原因 user' (($meta.state -eq 'finalized') -and ($meta.end_reason -eq 'user')) "$($meta.state) / $($meta.end_reason)"
        if (Test-Path $meeting) {
            $s = Probe-Streams $meeting
            Check '成品里有 1 路画面和 1 路混音，时长 ≥ 8 秒' (($s.Video -eq 1) -and ($s.Audio -eq 1) -and ($s.Duration -ge 8)) ('画面 {0}，声音 {1}，{2:N1} 秒' -f $s.Video, $s.Audio, $s.Duration)
        }

        # 连续再录几场：标准模式下每场的开始与保存都弹系统通知。每场结束时采集线程全部退出，
        # 曾经因此让下一条通知撞上被卸载的组件，进程以 0xC0000005 崩溃（见 record_capture::com）
        $rounds = 5
        $alive = $true
        for ($i = 1; $i -le $rounds -and $alive; $i++) {
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            $alive = (Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0
            if (-not $alive) { break }
            Start-Sleep -Seconds 2
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'S'
            $alive = (Wait-Log $box '托盘图标更新.*Standby' $n 20000).Count -gt 0
            Start-Sleep -Milliseconds 400
        }
        Check "标准模式连续再录 $rounds 场（每场开始与保存都弹通知），进程不崩溃" ($alive -and -not $app.HasExited) $(if ($app.HasExited) { "进程已退出，退出码 $($app.ExitCode)" } else { '' })
        $all = @(Sessions $box)
        $unfinished = @($all | Where-Object { (Read-Session $_.FullName).state -ne 'finalized' })
        Check '这几场都正常保存（finalized）' (($all.Count -eq $rounds + 1) -and ($unfinished.Count -eq 0)) "共 $($all.Count) 场，没保存完的 $($unfinished.Count) 场"

        # 热键退出：录制中按 Ctrl+Alt+Q，保存后进程自己退出
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '第二场开始' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        Start-Sleep -Seconds 3
        Press-Chord 'Ctrl', 'Alt', 'Q'
        $exited = $app.WaitForExit(30000)
        Check '录制中按“保存并退出”：保存后进程自己退出' $exited $(if ($exited) { "退出码 $($app.ExitCode)" } else { '30 秒内没退出' })
        $second = (Sessions $box)[-1].FullName
        $meta2 = Read-Session $second
        Check '第二场已保存（finalized，user，有成品）' (($meta2.state -eq 'finalized') -and ($meta2.end_reason -eq 'user') -and (Test-Path (Join-Path $second 'meeting.mp4'))) "$($meta2.state) / $($meta2.end_reason)"

        # 崩溃恢复：安静模式，录制中强行结束进程，再启动
        Write-Config $box (Standard-Config $box 'quiet')
        $app = Start-App $box
        [void](Ready $box $app)
        $powerK = [int][ShellNative]::ExecutionState()
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '第三场开始' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        $quietStart = Wait-Log $box '通知.*show=false.*开始录制' $n 2000
        Check '安静模式：开始时不弹通知' ($quietStart.Count -gt 0) ''
        Start-Sleep -Seconds 4
        $third = (Sessions $box)[-1].FullName
        Stop-Process -Id $app.Id -Force
        [void]$app.WaitForExit(5000)
        Start-Sleep -Seconds 2
        $powerKilled = [int][ShellNative]::ExecutionState()
        Check '被杀后：系统自动收回了电源请求（录制前没有的位都清掉了）' ((Power-Leftover $powerK $powerKilled) -eq 0) ('录制前 0x{0:X}，被杀后 0x{1:X}' -f $powerK, $powerKilled)
        Check '被杀后 session.json 仍是 recording（没来得及收尾）' ((Read-Session $third).state -eq 'recording') ''
        $app = Start-App $box
        Check '重新启动并就绪' (Ready $box $app) ''
        $fixed = Wait-Log $box '通知.*show=true.*已恢复上次中断的录制' 0 60000
        Check '启动时自动修复，安静模式下也弹通知' ($fixed.Count -gt 0) ($fixed -join ' ')
        $meta3 = Read-Session $third
        Check '被杀的那场：recovered，有成品' (($meta3.state -eq 'recovered') -and (Test-Path (Join-Path $third 'meeting.mp4'))) "$($meta3.state)"
        Stop-App $app
    }

    # ───────────────────────── events：睡眠与关机 ─────────────────────────
    if ($Parts -contains 'events') {
        '=== events ==='
        $box = New-Sandbox 'events'
        Write-Config $box (Standard-Config $box 'quiet')
        $app = Start-App $box
        Check '启动并就绪' (Ready $box $app) ''
        $hwnd = [ShellNative]::FindWindowW('RecordSessionEvents', [NullString]::Value)
        Check '找到接收系统事件的隐藏窗口' ($hwnd -ne [IntPtr]::Zero) ''

        # 睡眠（模拟 WM_POWERBROADCAST / PBT_APMSUSPEND）：存下本场，程序继续运行
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '开始一场' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        Start-Sleep -Seconds 3
        $t = [Diagnostics.Stopwatch]::StartNew()
        [void][ShellNative]::SendMessageW($hwnd, 0x218, [IntPtr]4, [IntPtr]::Zero)
        $t.Stop()
        Info ('睡眠通知的处理（含保存）用了 {0:N2} 秒' -f $t.Elapsed.TotalSeconds)
        $dir = (Sessions $box)[-1].FullName
        Check '睡眠：窗口过程返回时本场已保存（原因 sleep，有成品）' ((((Read-Session $dir).end_reason) -eq 'sleep') -and (Test-Path (Join-Path $dir 'meeting.mp4'))) "$((Read-Session $dir).state) / $((Read-Session $dir).end_reason)"
        Check '睡眠：安静模式下也弹“因睡眠已保存”' ((Wait-Log $box '通知.*show=true.*因睡眠已保存' $n 3000).Count -gt 0) ''
        Check '睡眠：程序仍在运行' (-not $app.HasExited) ''
        $n = (Read-Log $box).Count
        Press-Chord 'Ctrl', 'Alt', 'R'
        Check '“唤醒”后热键仍能开始新的一场' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
        Start-Sleep -Seconds 3

        # 关机（模拟 WM_ENDSESSION）：存下本场并退出
        $t = [Diagnostics.Stopwatch]::StartNew()
        [void][ShellNative]::SendMessageW($hwnd, 0x16, [IntPtr]1, [IntPtr]::Zero)
        $t.Stop()
        Info ('关机通知的处理（含保存）用了 {0:N2} 秒' -f $t.Elapsed.TotalSeconds)
        $exited = $app.WaitForExit(20000)
        Check '关机：保存后进程退出' $exited ''
        $dir2 = (Sessions $box)[-1].FullName
        Check '关机：本场已保存（原因 shutdown，有成品）' ((((Read-Session $dir2).end_reason) -eq 'shutdown') -and (Test-Path (Join-Path $dir2 'meeting.mp4'))) "$((Read-Session $dir2).state) / $((Read-Session $dir2).end_reason)"
        Check '关机：先后两场是不同的会话' ($dir -ne $dir2) ''
        Stop-App $app
    }

    # ───────────────────────── failures：启动失败、热键冲突、配置错误 ─────────────────────────
    if ($Parts -contains 'failures') {
        '=== failures ==='
        # 磁盘剩余空间低于下限：不开始，不留会话目录
        if ($free -lt 1000) {
            $box = New-Sandbox 'disk'
            Write-Config $box (Standard-Config $box 'quiet' "[limits]`nmin_free_gb = 1000`nmax_hours = 6.0`n")
            $app = Start-App $box
            [void](Ready $box $app)
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            $fail = Wait-Log $box '通知.*show=true.*没能开始录制' $n 15000
            Check '磁盘剩余空间不足：开始失败并弹通知（安静模式也弹）' ($fail.Count -gt 0) ($fail -join ' ')
            Check '失败后没有留下会话目录' ((Sessions $box).Count -eq 0) ''
            Check '失败后托盘仍是待命，热键还能用' ((Wait-Log $box '托盘图标更新.*Recording' $n 1500).Count -eq 0 -and -not $app.HasExited) ''
            Stop-App $app
        } else { Info "本机 $($work.Substring(0, 2)) 盘剩余 $free GB，超过校验上限 1000 GB，跳过“磁盘不足”检查" }

        # 热键被别的程序占着：其余热键照常，冲突的明确提示
        $box = New-Sandbox 'conflict'
        Write-Config $box (Standard-Config $box 'quiet')
        $mods = 0x0002 -bor 0x0001 -bor 0x4000
        $held = [ShellNative]::RegisterHotKey([IntPtr]::Zero, 200, $mods, 0x52)
        Check '（准备）测试进程先占住 Ctrl+Alt+R' $held ''
        $app = Start-App $box
        [void](Ready $box $app)
        $log = Read-Log $box
        Check 'Ctrl+Alt+R 注册失败并写明原因' ([bool]($log -match '热键没注册成功.*Start.*Ctrl\+Alt\+R')) (($log -match '没注册成功') -join ' ')
        Check '另外两个热键照常注册' (@($log | Where-Object { $_ -match '热键已注册' }).Count -eq 2) ''
        $menu = @($log | Where-Object { $_ -match '托盘菜单文字' })
        Check '托盘菜单里写明冲突的热键，其余写着热键' (($menu.Count -ge 1) -and ($menu[-1] -match 'start=开始录制（Ctrl\+Alt\+R 没注册成功，被占用）') -and ($menu[-1] -match 'stop=保存并结束本场（Ctrl\+Alt\+S）')) ($menu[-1])
        Check '热键冲突的通知弹出（安静模式也弹）' ((Wait-Log $box '通知.*show=true.*有热键没注册成功' 0 3000).Count -gt 0) ''
        [void][ShellNative]::UnregisterHotKey([IntPtr]::Zero, 200)
        Stop-App $app

        # 配置里的热键写错：三个都不注册，说明原因，配置文件不被改动
        $box = New-Sandbox 'badkey'
        $body = Standard-Config $box 'quiet' "[hotkeys]`nstart = `"R`"`nstop = `"Ctrl+Alt+S`"`nquit = `"Ctrl+Alt+Q`"`n"
        Write-Config $box $body
        $before = (Get-FileHash $box.ConfigFile).Hash
        $app = Start-App $box
        [void](Ready $box $app)
        $log = Read-Log $box
        Check '热键配置写错：三个都没注册，并说明原因' (@($log | Where-Object { $_ -match 'WARN.*热键没注册成功' }).Count -eq 3 -and [bool]($log -match '没有修饰键')) ''
        $line = @($log | Where-Object { $_ -match '通知.*有热键没注册成功' })
        Check '三个热键同一个原因时，通知里只说一遍' (($line.Count -eq 1) -and (([regex]::Matches($line[0], '没有修饰键')).Count -eq 1)) ($line -join ' ')
        Check '配置文件没有被改动' ((Get-FileHash $box.ConfigFile).Hash -eq $before) ''
        Stop-App $app

        # 配置文件读不懂：按默认值运行，提示用户，原文件原样保留
        $box = New-Sandbox 'badtoml'
        Write-Config $box "hotkeys = [这不是 toml`n"
        $before = (Get-FileHash $box.ConfigFile).Hash
        $app = Start-App $box
        Check '配置读不懂时仍能启动' (Ready $box $app) ''
        $log = Read-Log $box
        Check '弹出“配置文件有误”的通知' ((Wait-Log $box '通知.*show=true.*配置文件有误' 0 3000).Count -gt 0) ''
        Check '按默认热键运行（三个都注册）' (@($log | Where-Object { $_ -match '热键已注册' }).Count -eq 3) ''
        Check '原配置文件原样保留' ((Get-FileHash $box.ConfigFile).Hash -eq $before) ''
        Stop-App $app
    }

    # ───────────────────────── ui：首次向导与设置窗口 ─────────────────────────
    if ($Parts -contains 'ui') {
        '=== ui ==='
        $box = New-Sandbox 'ui'
        # 先占住 Ctrl+Alt+F8，稍后在设置里试“这个组合被别的程序占用”
        [void][ShellNative]::RegisterHotKey([IntPtr]::Zero, 201, (0x0002 -bor 0x0001 -bor 0x4000), 0x77)
        $app = Start-App $box -Cdp
        Check '没有配置文件时能启动' (Ready $box $app) ''
        $c = Connect-Cdp 'Record'
        Check '首次使用：自动打开设置窗口（向导）' ($null -ne $c) ''
        if ($c) {
            [void](Cdp-Call $c 'Emulation.setFocusEmulationEnabled' @{ enabled = $true })
            [void](Cdp-Eval $c $jsLib)
            Check '向导页面加载完成' (Wait-Until { (Cdp-Eval $c "document.querySelector('h1') && document.querySelector('h1').innerText") -eq '欢迎使用 Record' } 10000) ''
            $text = Cdp-Eval $c '__t.text()'
            $sections = (Cdp-Eval $c "[...document.querySelectorAll('h2')].map((h) => h.innerText).join(',')")
            Check '向导只有保存位置、热键、开机自启三项' ($sections -eq '保存位置,热键,开机自启') $sections
            Check '向导里有同意提示和托盘固定提示' (($text -match '取得同意') -and ($text -match '拖到任务栏')) ''
            Check '三个热键都显示“已注册”' ((Cdp-Eval $c "document.querySelectorAll('.badge.ok').length") -eq 3) ''
            Cdp-Shot $c (Join-Path $work 'wizard.png')
            [void](Cdp-Eval $c "__t.click('完成')")
            Check '点“完成”后写入配置文件' (Wait-Until { Test-Path $box.ConfigFile } 8000) ''
            Check '点“完成”后设置窗口关闭' (Wait-Until { @(Invoke-RestMethod "http://127.0.0.1:$CdpPort/json" | Where-Object { $_.type -eq 'page' -and $_.title -match 'Record' }).Count -eq 0 } 8000) ''
            Check '关窗后程序仍在运行，热键仍然有效' (-not $app.HasExited) ''
            $cfg = Get-Content $box.ConfigFile -Raw -Encoding UTF8
            Check '配置里有默认热键和 autostart = false' (($cfg -match 'start = "Ctrl\+Alt\+R"') -and ($cfg -match 'autostart = false')) ''
            $c.Ws.Dispose()
        }

        # 配置文件里有程序不认识的键（以后子项目 B、C 的设置）：重启后，设置界面保存时必须原样保留
        Stop-App $app
        [System.IO.File]::AppendAllText($box.ConfigFile, "`n[speech]`nmodel = `"keep-me`"`n", (New-Object System.Text.UTF8Encoding($false)))
        $app = Start-App $box -Cdp
        Check '有配置文件时重新启动并就绪（不再弹向导）' ((Ready $box $app) -and ($null -eq (Connect-Cdp 'Record' 1500))) ''

        # 再启动一次 = 唤起设置窗口
        $second = Start-Process -FilePath $Exe -PassThru
        $procs.Add($second)
        Check '第二个实例自己退出' ($second.WaitForExit(15000)) ''
        Check '第一个实例记下“又启动了一个实例”' ((Wait-Log $box '又启动了一个实例' 0 3000).Count -gt 0) ''
        $c = Connect-Cdp 'Record'
        Check '第二个实例唤起了设置窗口' ($null -ne $c) ''
        if ($c) {
            [void](Cdp-Call $c 'Emulation.setFocusEmulationEnabled' @{ enabled = $true })
            [void](Cdp-Eval $c $jsLib)
            Check '已有配置：显示完整设置' (Wait-Until { (Cdp-Eval $c "document.querySelector('h1') && document.querySelector('h1').innerText") -eq 'Record 设置' } 10000) ''
            $text = Cdp-Eval $c '__t.text()'
            Check '完整设置里有画面、声音、提示方式、自动保护' (($text -match '画面') -and ($text -match '声音') -and ($text -match '提示方式') -and ($text -match '自动保护')) ''
            $devs = Cdp-Eval $c "document.querySelectorAll('select')[4].options.length"
            Info "麦克风下拉框有 $devs 个选项（含系统默认那一项）"
            Cdp-Shot $c (Join-Path $work 'settings.png')

            # 录入热键：聚焦时暂停全局热键，按键后写入输入框，失焦后恢复
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            Check '聚焦热键输入框 → 全局热键暂停' ((Wait-Log $box '热键已暂停' $n 3000).Count -gt 0) ''
            Check '录入中输入框提示“请按下新的组合键”' ((Cdp-Eval $c "__t.row('开始录制').querySelector('input').value") -match '请按下') ''
            $before = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            Start-Sleep -Milliseconds 800
            Check '暂停期间按旧组合不会开始录制' ((Sessions $box).Count -eq 0 -and (Wait-Log $box '热键 command=Start' $before 500).Count -eq 0) ''
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'F9', { ctrlKey: true, altKey: true })")
            Check '按下 Ctrl+Alt+F9 → 输入框显示新组合' ((Cdp-Eval $c "__t.row('开始录制').querySelector('input').value") -eq 'Ctrl+Alt+F9') ''
            Check '录入完成自动失焦 → 按已保存的配置恢复热键' ((Wait-Log $box '热键已注册' $n 3000).Count -eq 3) ''

            # 重复的热键：不保存，说明原因
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'KeyS', { ctrlKey: true, altKey: true })")
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '开始与保存用同一个组合：不保存，提示“不能相同”' (Wait-Until { (Cdp-Eval $c '__t.text()') -match '不能相同' } 5000) ''
            Check '配置文件没被改动' ((Get-Content $box.ConfigFile -Raw -Encoding UTF8) -match 'start = "Ctrl\+Alt\+R"') ''

            # 被别的程序占用的组合：保存成功，但明确标出
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'F8', { ctrlKey: true, altKey: true })")
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '组合被别的程序占用：保存后标出“已被别的程序占用”' (Wait-Until { (Cdp-Eval $c '__t.text()') -match '已被别的程序占用' } 5000) ''
            Check '同时弹出热键冲突通知' ((Wait-Log $box '通知.*show=true.*有热键没注册成功' $n 3000).Count -gt 0) ''
            Cdp-Shot $c (Join-Path $work 'conflict.png')

            # 换成真正空闲的组合并改提示方式：保存成功，新组合立刻生效，旧组合失效
            [void](Cdp-Eval $c "__t.row('开始录制').querySelector('input').focus()")
            [void](Cdp-Eval $c "__t.key(__t.row('开始录制').querySelector('input'), 'F9', { ctrlKey: true, altKey: true })")
            [void](Cdp-Eval $c "__t.set(__t.row('气泡通知').querySelector('select'), 'standard')")
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '保存成功并提示已保存' (Wait-Until { (Cdp-Eval $c '__t.text()') -match '已保存' } 5000) ''
            $cfg = Get-Content $box.ConfigFile -Raw -Encoding UTF8
            Check '配置文件里是新热键和新的提示方式' (($cfg -match 'start = "Ctrl\+Alt\+F9"') -and ($cfg -match 'mode = "standard"')) ''
            Check '三个热键都重新注册成功' ((Wait-Log $box '热键已注册' $n 3000).Count -ge 3) ''
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'R'
            Start-Sleep -Milliseconds 800
            Check '旧组合 Ctrl+Alt+R 不再有效' ((Sessions $box).Count -eq 0) ''
            Press-Chord 'Ctrl', 'Alt', 'F9'
            Check '新组合 Ctrl+Alt+F9 开始录制' ((Wait-Log $box '托盘图标更新.*Recording' $n 10000).Count -gt 0) ''
            Start-Sleep -Seconds 2
            $n = (Read-Log $box).Count
            Press-Chord 'Ctrl', 'Alt', 'S'
            Check '保存后回到待命' ((Wait-Log $box '托盘图标更新.*Standby' $n 20000).Count -gt 0) ''
            Check '程序不认识的键原样保留' ((Get-Content $box.ConfigFile -Raw -Encoding UTF8) -match '(?s)\[speech\]\s*model = "keep-me"') ''

            # 开机自启：勾选并保存 → 系统的 Run 项里出现本程序；取消并保存 → 移除
            $autoRow = "__t.row('登录 Windows 后自动运行').querySelector('input')"
            $runValues = { (Get-ItemProperty $runKey).PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' -and [string]$_.Value -like "*$Exe*" } }
            Check '（准备）开机自启起始为未启用' (@(& $runValues).Count -eq 0) ''
            [void](Cdp-Eval $c "$autoRow.click()")
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '勾选“开机自启”并保存 → Run 项里有本程序' (Wait-Until { @(& $runValues).Count -eq 1 } 5000) ((& $runValues | ForEach-Object { "$($_.Name) = $($_.Value)" }) -join ' ')
            Check '配置里 autostart = true' ((Get-Content $box.ConfigFile -Raw -Encoding UTF8) -match 'autostart = true') ''
            [void](Cdp-Eval $c "$autoRow.click()")
            [void](Cdp-Eval $c "__t.click('保存')")
            Check '取消勾选并保存 → Run 项被移除' (Wait-Until { @(& $runValues).Count -eq 0 } 5000) ''

            # 直接关闭窗口（不点保存）也应当恢复热键
            [void](Cdp-Eval $c "__t.row('保存并结束本场').querySelector('input').focus()")
            $n = (Read-Log $box).Count
            [void](Cdp-Eval $c "__t.click('关闭')")
            Check '录入中途关掉窗口：热键被恢复' ((Wait-Log $box '热键已注册' $n 5000).Count -ge 3) ''
            $c.Ws.Dispose()
        }
        [void][ShellNative]::UnregisterHotKey([IntPtr]::Zero, 201)
        Stop-App $app
    }
}
finally {
    foreach ($p in $procs) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    Get-Process -Name 'record' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Exe } | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 1
    # 资源管理器为托盘图标留的登记；万一中途失败，也不要把本程序留在开机自启里
    Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings' -ErrorAction SilentlyContinue | Where-Object { (Get-ItemProperty $_.PSPath).ExecutablePath -eq $Exe } | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    (Get-ItemProperty $runKey).PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' -and [string]$_.Value -like "*$Exe*" } | ForEach-Object { Remove-ItemProperty -Path $runKey -Name $_.Name -ErrorAction SilentlyContinue }
}
$runEnd = (Get-ItemProperty $runKey).PSObject.Properties.Name | Where-Object { $_ -notlike 'PS*' } | Sort-Object
Check '结束时没有动开机自启项' (($runBefore -join ',') -eq ($runEnd -join ',')) ''
''
'汇总：通过 {0} 项，失败 {1} 项' -f @($results | Where-Object { $_.结果 -eq '通过' }).Count, @($results | Where-Object { $_.结果 -eq '失败' }).Count
foreach ($shot in 'wizard.png', 'settings.png', 'conflict.png') { $f = Join-Path $work $shot; if (Test-Path $f) { Copy-Item $f (Join-Path $env:TEMP "record-shell-$shot") -Force } }
'截图（设置窗口自己的画面）：' + ((Get-ChildItem $env:TEMP -Filter 'record-shell-*.png' | ForEach-Object { $_.FullName }) -join '，')
if (-not $Keep) { Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue } else { '保留工作目录：' + $work }
```

这个文件必须是 UTF-8 **带 BOM**，见下一步。

- [ ] **步骤 2：给 `scripts/shell-check.ps1` 补上 UTF-8 BOM**

Windows PowerShell 5.1 读含中文的脚本要靠 BOM，否则会按系统代码页解码成乱码。

```powershell
$p = 'scripts\shell-check.ps1'; [IO.File]::WriteAllText($p, [IO.File]::ReadAllText($p), (New-Object Text.UTF8Encoding($true)))
```

预期：命令成功结束（退出码 0）。

- [ ] **步骤 3：重新编译 record，把刚才构建好的前端嵌进去**

```powershell
cargo build -p record-shell --bin record
```

预期：`Finished`。Tauri 在编译时把 `app/dist` 嵌进可执行文件，所以前端改了就要重新编译一次。

- [ ] **步骤 4：运行外壳检查脚本**

约 1.5～2 分钟（第一次运行会多几十秒），**期间不要动键盘和鼠标**，也不要关掉弹出的窗口：脚本会模拟按键、录几秒真实的桌面、弹真实的系统通知，设置窗口会出现又消失。`RECORD_FFMPEG` 只对这个窗口有效，不改系统环境变量。

```powershell
$env:RECORD_FFMPEG = (Resolve-Path vendor\ffmpeg\bin\ffmpeg.exe).Path
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\shell-check.ps1
```

预期：每项一行 `[通过]`，没有 `[失败]`，最后 `汇总：通过 97 项，失败 0 项`，并列出设置窗口截图的位置。参考（本机，两次测量）：待命时工作集 19.8～24.4 MB、私有内存 6.6～7.2 MB；录制中外壳进程 CPU 5.2%～7.0%（单核口径，真正吃 CPU 的是 ffmpeg，见计划 A3 的 S2 压力测试）；睡眠、关机通知的处理（含保存本场）各约 0.3～0.4 秒。**失败时先看那一项后面的说明；热键被别的程序占着、系统通知被“勿扰”吞掉都是环境原因，处理环境，不要改脚本让它通过。**

- [ ] **步骤 5：删除脚本留在临时目录的截图**

截图是设置窗口自己的画面（不含桌面内容），用完也删。

```powershell
Remove-Item $env:TEMP\record-shell-*.png -ErrorAction SilentlyContinue
```

预期：命令成功结束（退出码 0）。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "test(shell): 外壳的真机检查脚本"
```

---

## 任务 15：使用说明与完整验收

**目标：** 更新 README 的状态与使用方法；再对照设计文档 §10.4 的子项目 A 完成标准逐条过一遍，分清自动覆盖的、人工的和没验证的，并做一次发布构建。

| 设计文档 §10.4 的完成标准 | 怎么验证 | 状态 |
|---|---|---|
| 热键可开始与保存；静默无弹窗；托盘状态正确 | `shell-check` 的 `basic`（含通知取舍、托盘图标状态的日志）；托盘图标外观与右键菜单需人眼看 | 自动 / **人工（外观）** |
| 60 分钟录制：无丢音，音画偏差 ≤ 80 ms，CPU 占用 < 5%，文件可播放 | 60 分钟人工长跑（见下），30 秒与 40 秒的自动版本已在 A3 | **人工，约 1 小时** |
| 强杀进程后可恢复 | `shell-check` 的 `basic`（杀掉 → 重启 → recovered 且有成品）与 A3 的强杀恢复 | 自动 |
| 设备切换 | A3 的“拔掉设备”人工项；`device_changes` 计数 | **人工，未验证** |
| 锁屏、UAC | 录制中锁屏、触发 UAC，看重复帧计数与是否继续录 | **人工，未验证** |
| 睡眠、关机 | `shell-check` 的 `events` 用窗口消息模拟；真正睡眠、真正关机 | 自动（模拟）/ **人工（真机）** |
| 磁盘不足 | `shell-check` 的 `failures` 里模拟剩余空间不足 | 自动（模拟） |

**文件：**

- 修改：`README.md`

- [ ] **步骤 1：修改 `README.md`**

把这一段：

```markdown
状态：子项目 A（静默录屏器）实施中。设计见 `docs/superpowers/specs/`，实施计划见 `docs/superpowers/plans/`。
```

改成：

```markdown
状态：子项目 A（静默录屏器）已实现：常驻托盘，热键开始与保存，三轨落盘并生成 `meeting.mp4`，崩溃后自动恢复。设计见 `docs/superpowers/specs/`，实施计划见 `docs/superpowers/plans/`。哪些情形没有真机验证过，见设计文档 §10.5。
```

- [ ] **步骤 2：修改 `README.md`**

把这一段：

```markdown
## 使用边界
```

改成：

```markdown
## 使用

1. 构建：`pnpm --dir app install`、`pnpm --dir app build`，再 `cargo build --release -p record-shell`，得到 `target\release\record.exe`。
2. ffmpeg：运行 `scripts\fetch-ffmpeg.ps1`，或把 LGPL 构建的 `ffmpeg.exe` 放在 `record.exe` 旁边，或设环境变量 `RECORD_FFMPEG`。
3. 运行 `record.exe`：没有窗口，只有托盘图标。第一次运行会弹出设置向导（保存位置、热键、开机自启）。
4. 默认热键：`Ctrl+Alt+R` 开始，`Ctrl+Alt+S` 保存并结束本场，`Ctrl+Alt+Q` 保存并退出程序，都可以在设置里改。
5. 每场录制一个目录，在保存位置下（默认 `<视频库>\Record`）：`video.mkv`、`mic.mka`、`system.mka`、`meeting.mp4`、`session.json`。配置在 `%APPDATA%\record\config.toml`，日志在 `%LOCALAPPDATA%\record\logs\`。
6. 录制期间，显示器和系统不会因为久不动鼠标而休眠（显示器一休眠，桌面复制就拿不到画面，成品里只剩声音）；保存或结束后恢复系统自己的设置。这只拦空闲计时：自己点“睡眠”或合上盖子照常睡眠，睡眠前本场会先保存（用窗口消息模拟验证过，真机睡眠没有验证，见设计文档 §10.5）。

## 使用边界
```

- [ ] **步骤 3：看一眼 README**

```powershell
Get-Content README.md -Encoding UTF8
```

预期：通过，输出里有 `已实现`、`## 使用`、`## 使用边界`。

- [ ] **步骤 4：发布构建**

```powershell
cargo build --release -p record-shell
```

预期：``Finished `release` profile``。第一次要把所有依赖按发布配置（thin LTO）重新编译一遍，约 5～15 分钟。

- [ ] **步骤 5：在发布构建上再跑一遍 basic**

约 2 分钟，期间不要动键盘鼠标。发布配置与调试配置的行为不应不同；这一步确认它们没有不同。

```powershell
$env:RECORD_FFMPEG = (Resolve-Path vendor\ffmpeg\bin\ffmpeg.exe).Path
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\shell-check.ps1 -Exe target\release\record.exe -Parts basic
```

预期：通过，输出里有 `汇总：通过`、`失败 0 项`。

- [ ] **步骤 6：看一眼发布版大小**

```powershell
(Get-Item target\release\record.exe).Length / 1MB
```

预期：一个数字（MB）；本机约十几 MB，记下来即可，不是通过标准。

- [ ] **步骤 7：（人工）60 分钟长跑**

```powershell
Start-Process target\release\record.exe
```

**这一步需要真机：** 1. 启动 `target\release\record.exe`，按 `Ctrl+Alt+R` 开始；2. 另开一个 PowerShell 窗口，运行 `cargo run -q -p record-cli -- s3 stimulus --secs 3560` 产生闪屏与测试音；3. 开始后约 1 分钟记下 `record` 与全部 `ffmpeg` 进程的 `TotalProcessorTime` 之和与当时时刻，结束前（约 59 分钟）再记一次；4. 按 `Ctrl+Alt+S` 保存；5. 对新生成的 `meeting.mp4` 运行 `cargo run -q -p record-cli -- s3 analyze <meeting.mp4路径>`。

预期：`s3 analyze` 的结论为“通过”（偏移在 80 ms 内、没有随时间漂移）；两次 CPU 时间之差除以经过的秒数与逻辑处理器数，得到整机占用，要 < 5%；`session.json` 里 `dropped_audio_samples` 为 0、`large_gap_events` 为 0；`meeting.mp4` 能播放（用 ffprobe 看结构即可，不要去听）。结果与未通过的项一起写进设计文档 10.5 里“10.4 完成标准的现状”那张表。

**核对并补全设计文档：** 设计文档的 10.5（spike 与验收的结果）、12（本机环境，已脱敏）、16.3（实现与原设计的差别）已经记录了作者在同一台开发机上做完这些步骤的结果。你这次跑出的数字如果不一样，就改文档里的数字（脱敏：不写设备名与 GUID）；10.1 与 10.4 里每个“人工 / 未验证”的条目，做完一条，就在 10.5 里把它从“未验证”改成“已验证（日期、结果）”，没做的保持“未验证”并写明原因。日期用命令取（`Get-Date`），不要凭感觉写。

- [ ] **步骤 8：提交**

```powershell
git add -A
git commit -m "docs: 子项目 A 的使用说明与验收记录"
```

---

## 任务 16：质量门：格式、clippy、全部测试

**目标：** 子项目 A 全部就位。最后一遍：格式、clippy、Rust 全部测试、前端测试与构建，并确认工作区干净、没有把录制产物和构建产物带进仓库。

- [ ] **步骤 1：检查格式**

```powershell
cargo fmt --all -- --check
```

预期：没有输出，退出码 0（有差异就先运行 `cargo fmt --all` 再检查）。

- [ ] **步骤 2：运行 clippy（警告当错误）**

```powershell
cargo clippy --workspace --all-targets -- -D warnings
```

预期：通过，输出里有 `Finished`。

- [ ] **步骤 3：运行全部测试**

```powershell
cargo test --workspace
```

预期：各 crate 的 `test result: ok.` 行，没有 `FAILED`。测试要用到本机的音频端点、显示器、显卡、硬件编码器与 ffmpeg；在没有这些的机器（比如云上的 CI）上会失败，那是预期，不是代码有错。

- [ ] **步骤 4：前端测试**

```powershell
pnpm --dir app test
```

预期：通过，输出里有 `4 passed`。

- [ ] **步骤 5：前端类型检查与构建**

```powershell
pnpm --dir app build
```

预期：通过，输出里有 `built in`。

- [ ] **步骤 6：确认工作区干净**

```powershell
if (git status --short) { git status --short; throw "工作区不干净：有没提交的改动或产物" }
"工作区干净"
```

预期：输出 `工作区干净`。`target`、`vendor`、`app/dist`、`app/node_modules` 都被 `.gitignore` 挡住。

- [ ] **步骤 7：确认仓库里没有录音录像和模型文件**

```powershell
git status --short --ignored | Select-String -Pattern "\.(mkv|mka|mp4|wav|onnx|gguf)$"
```

预期：没有任何输出。

- [ ] **步骤 8：推送**

这是公开仓库：推送之前再看一眼 `git log --stat` 与 `git diff origin/main --stat`，确认没有录音录像、转写稿、模型、密钥。

```powershell
git push origin main
```

预期：命令成功结束（退出码 0）。

---
