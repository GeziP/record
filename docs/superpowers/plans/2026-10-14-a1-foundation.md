# 子项目 A 实施计划 1/4：地基（工作区脚手架与 record-core）

> **给执行者：** REQUIRED SUB-SKILL: 用 superpowers:executing-plans 在当前会话内逐任务执行本计划（**不派子代理**）。步骤用复选框（`- [ ]`）跟踪。

**目标：** 建好 Rust 工作区，并用 TDD 做出 `record-core`：全部不依赖系统接口的纯逻辑——统一时钟、时间线修复、健康检测、配置、会话文件、ffmpeg 参数、控制器状态机、自动保护、崩溃恢复计划等。后面三份计划都建立在它之上；它没有任何 Windows 专有代码，所以在这一份计划里一切都能自动验证。

**架构：** `record-core` 是纯逻辑库：输入是时间戳、字节数、配置等普通数据，输出是决策（补多少静音、要不要告警、下一个状态、ffmpeg 参数列表）。系统接口（WASAPI、Desktop Duplication、ffmpeg 进程、Tauri）放在后面的 crate 里，只做“取数据 / 执行决策”，这样绝大部分行为可以在没有设备的机器上测。

**技术栈：** Rust 1.95（edition 2024）、Cargo 工作区；serde / serde_json / toml（序列化）、thiserror、chrono、tempfile（测试）；`cargo fmt`、`cargo clippy -D warnings`。

**本计划的位置：** 共四份计划：A1 地基（本份）→ A2 采集与 S1/S2 → A3 写入、引擎与 S3 → A4 外壳与 S5、验收。每份结束时工作区都能编译、测试全绿；只覆盖子项目 A（静默录屏器），不含语音与会后处理。

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

- Windows 11 x64；已安装 Git、Rust 1.95 以上（MSVC 工具链）和 Visual Studio 2022 生成工具（C++ 工作负载）。
- 仓库 `E:\gezi\record`（GitHub：`GeziP/record`）已克隆，里面只有设计文档；`git config user.name` / `user.email` 已设置。
- 在 PowerShell 里执行所有命令，工作目录是仓库根。

## 文件结构

本计划新建的文件及各自的职责（修改已有文件的地方写在各任务里）：

- `.gitignore`：不入库的东西：构建产物、ffmpeg、录音录像类文件、模型、密钥
- `.gitattributes`：行尾统一为 LF
- `rustfmt.toml`：格式规则（edition 2024，行宽 200）
- `Cargo.toml`：工作区根：成员、统一的版本与依赖
- `crates/record-core/Cargo.toml`：record-core 的依赖
- `crates/record-core/src/lib.rs`：模块声明
- `README.md`：项目说明、使用边界、开发前置
- `crates/record-core/src/clock.rs`：统一时钟：QPC 计数与 100 ns 单位之间的换算
- `crates/record-core/src/dsp.rs`：测量用的小型信号处理：Goertzel 单频检测、脉冲串检测、间隔与漂移估计
- `crates/record-core/src/geometry.rs`：画面输出尺寸与黑边布局
- `crates/record-core/src/hotkey.rs`：热键字符串的解析与规范化（如 `ctrl+alt+r` → `Ctrl+Alt+R`）
- `crates/record-core/src/syncprobe.rs`：画面与声音的同步测量
- `crates/record-core/src/timeline.rs`：时间线修复：补静音与漂移修正
- `crates/record-core/src/health.rs`：采集健康监测：不静默失败
- `crates/record-core/src/pacer.rs`：画面的恒定帧率节拍与按原因分类的帧统计
- `crates/record-core/src/testpattern.rs`：画面自检用的测试图案：四个已知颜色的分区，加一条随时间匀速移动的亮条
- `crates/record-core/src/config.rs`：`config.toml` 的数据模型、校验与读写
- `crates/record-core/src/session.rs`：会话目录与 `session.json`
- `crates/record-core/src/controller.rs`：控制器：待命 → 启动中 → 录制中 → 收尾中 → 待命
- `crates/record-core/src/ffmpeg_args.rs`：ffmpeg 命令行的构造
- `crates/record-core/src/limits.rs`：自动保护：单场时长上限、磁盘剩余空间下限
- `crates/record-core/src/paths.rs`：配置、日志、模型的位置
- `crates/record-core/src/recovery.rs`：崩溃恢复的计划

---

## 任务 1：工作区脚手架与 record-core 骨架

**目标：** 建立 Cargo 工作区、格式规则、忽略规则（公开仓库，先把“不入库”的东西挡住）和 `record-core` 的空骨架，并确认依赖能下载、能编译。

**文件：**

- 创建：`.gitignore` — 不入库的东西：构建产物、ffmpeg、录音录像类文件、模型、密钥
- 创建：`.gitattributes` — 行尾统一为 LF
- 创建：`rustfmt.toml` — 格式规则（edition 2024，行宽 200）
- 创建：`Cargo.toml` — 工作区根：成员、统一的版本与依赖
- 创建：`crates/record-core/Cargo.toml` — record-core 的依赖
- 创建：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`README.md` — 项目说明、使用边界、开发前置

- [ ] **步骤 1：确认工具链**

```powershell
rustc --version
cargo --version
git --version
```

预期：三行版本号；`rustc` 需要 1.95 以上（`rustup update stable` 可以升级）。

- [ ] **步骤 2：创建 `.gitignore`**

先写忽略规则，再做第一次 `git add -A`：构建产物、ffmpeg、录音录像类文件、模型和密钥都不能进库。

```
# 构建产物
/target/
/app/dist/
/app/node_modules/
/app/src-tauri/gen/

# 开发用的 ffmpeg：scripts/fetch-ffmpeg.ps1 下载，不入库
/vendor/ffmpeg/

# 公开仓库：绝不提交录音录像、转写稿、模型和密钥
*.mkv
*.mka
*.mp4
*.wav
*.flac
*.onnx
*.gguf
/models/
*.toml.broken
.env
.env.*
*.pem
*.pfx
*.log
```

- [ ] **步骤 3：创建 `.gitattributes`**

```
* text=auto eol=lf
*.ps1 text eol=lf
*.ico binary
*.png binary
```

- [ ] **步骤 4：创建 `rustfmt.toml`**

```toml
edition = "2024"
max_width = 200
use_small_heuristics = "Max"
newline_style = "Unix"
```

- [ ] **步骤 5：创建 `Cargo.toml`**

工作区根。`app/src-tauri`（外壳）要到计划 A4 才存在，到时候再把它加进 `members`。

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.95"
publish = false

[workspace.dependencies]
anyhow = "1"
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "1"
crossbeam-channel = "0.5"
parking_lot = "0.12"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt", "env-filter"] }
tracing-appender = "0.2"
tempfile = "3"
chrono = { version = "0.4", default-features = false, features = ["clock", "std"] }

[profile.dev]
opt-level = 1

[profile.release]
lto = "thin"
codegen-units = 4
```

- [ ] **步骤 6：创建 `crates/record-core/Cargo.toml`**

```toml
[package]
name = "record-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
toml.workspace = true
thiserror.workspace = true
chrono.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **步骤 7：建一个空的 `lib.rs`**

```powershell
New-Item -ItemType Directory -Force crates\record-core\src | Out-Null
New-Item -ItemType File -Force crates\record-core\src\lib.rs | Out-Null
```

预期：没有输出。模块声明由后面各任务一行一行加进去。

- [ ] **步骤 8：确认依赖能下载、骨架能编译**

```powershell
cargo check -p record-core
```

预期：最后一行是 `Finished`（第一次会下载并编译 serde、toml、chrono 等，要几分钟）；同时生成 `Cargo.lock`，它要一起提交。

- [ ] **步骤 9：创建 `README.md`**

````markdown
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
````

- [ ] **步骤 10：提交**

```powershell
git add -A
git commit -m "chore: Rust 工作区与 record-core 骨架"
```

---

## 任务 2：record-core：clock

**目标：** 统一时钟：QPC 计数与 100 ns 单位之间的换算（设计文档 5.1）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/clock.rs`

**接口：**

- 依赖：无（不依赖本工作区的其他模块）
- 提供：`pub type Hns = i64`；`pub const HNS_PER_SEC: i64`；`pub struct QpcClock`；`QpcClock::new`；`QpcClock::freq`；`QpcClock::ticks_to_hns`；`QpcClock::hns_to_ticks`；`pub fn hns_to_frames(hns: Hns, rate: u32) -> i64`；`pub fn frames_to_hns(frames: i64, rate: u32) -> Hns`；`pub const fn ms_to_hns(ms: i64) -> Hns`
- 测试：6 个：`identity_when_freq_is_10mhz`、`converts_common_frequencies`、`rounds_to_nearest_and_is_symmetric`、`does_not_overflow_on_huge_tick_counts`、`rejects_non_positive_frequency`、`frame_and_time_conversions`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod clock;
```

- [ ] **步骤 2：创建 `crates/record-core/src/clock.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_when_freq_is_10mhz() {
        let c = QpcClock::new(10_000_000);
        assert_eq!(c.ticks_to_hns(123_456_789), 123_456_789);
        assert_eq!(c.hns_to_ticks(123_456_789), 123_456_789);
    }

    #[test]
    fn converts_common_frequencies() {
        // 24 MHz：24_000_000 计数正好 1 秒
        let c = QpcClock::new(24_000_000);
        assert_eq!(c.ticks_to_hns(24_000_000), HNS_PER_SEC);
        assert_eq!(c.hns_to_ticks(HNS_PER_SEC), 24_000_000);
        // 3.0 GHz 量级
        let c = QpcClock::new(3_000_000_000);
        assert_eq!(c.ticks_to_hns(3_000_000_000), HNS_PER_SEC);
    }

    #[test]
    fn rounds_to_nearest_and_is_symmetric() {
        let c = QpcClock::new(3); // 1 计数 = 3_333_333.33.. 个 100ns
        assert_eq!(c.ticks_to_hns(1), 3_333_333);
        assert_eq!(c.ticks_to_hns(2), 6_666_667);
        assert_eq!(c.ticks_to_hns(-2), -6_666_667);
    }

    #[test]
    fn does_not_overflow_on_huge_tick_counts() {
        // 运行 30 年的计数值：约 1e9 秒 × 1e7 Hz
        let c = QpcClock::new(10_000_000);
        let ticks = 9_500_000_000_000_000i64;
        assert_eq!(c.ticks_to_hns(ticks), ticks);
        let c = QpcClock::new(24_000_000);
        let ticks = 24_000_000i64 * 1_000_000_000;
        assert_eq!(c.ticks_to_hns(ticks), HNS_PER_SEC * 1_000_000_000);
    }

    #[test]
    #[should_panic(expected = "QPC 频率必须为正数")]
    fn rejects_non_positive_frequency() {
        let _ = QpcClock::new(0);
    }

    #[test]
    fn frame_and_time_conversions() {
        assert_eq!(hns_to_frames(ms_to_hns(10), 48_000), 480);
        assert_eq!(frames_to_hns(480, 48_000), ms_to_hns(10));
        assert_eq!(hns_to_frames(HNS_PER_SEC, 48_000), 48_000);
        // 向零截断
        assert_eq!(hns_to_frames(-ms_to_hns(1), 44_100), -44);
        assert_eq!(hns_to_frames(ms_to_hns(1), 44_100), 44);
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib clock::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/clock.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 统一时钟：QPC 计数与 100 ns 单位之间的换算（设计文档 5.1）。
//!
//! 全程序所有数据都用 100 ns 为单位的时间戳。WASAPI 的 `qpcPosition` 本身就是这个单位；
//! `QueryPerformanceCounter` 的原始计数要靠 [`QpcClock`] 换算。

/// 100 ns 为单位的时间戳或时长。
pub type Hns = i64;

/// 每秒的 100 ns 单位数。
pub const HNS_PER_SEC: i64 = 10_000_000;

/// QPC 计数换算器。`freq` 是 `QueryPerformanceFrequency` 返回的每秒计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QpcClock {
    freq: i64,
}

impl QpcClock {
    /// # Panics
    /// `freq` 不是正数时 panic（系统不会给出这种值，出现说明调用方写错了）。
    pub fn new(freq: i64) -> Self {
        assert!(freq > 0, "QPC 频率必须为正数，实际为 {freq}");
        Self { freq }
    }

    pub fn freq(&self) -> i64 {
        self.freq
    }

    /// QPC 计数 → 100 ns 单位，四舍五入到最近值。中间用 128 位运算，不会溢出。
    pub fn ticks_to_hns(&self, ticks: i64) -> Hns {
        div_round(i128::from(ticks) * i128::from(HNS_PER_SEC), i128::from(self.freq)) as i64
    }

    /// 100 ns 单位 → QPC 计数，四舍五入到最近值。
    pub fn hns_to_ticks(&self, hns: Hns) -> i64 {
        div_round(i128::from(hns) * i128::from(self.freq), i128::from(HNS_PER_SEC)) as i64
    }
}

/// 把 100 ns 时长换算为采样帧数，向零截断（宁可少算，不多算）。
pub fn hns_to_frames(hns: Hns, rate: u32) -> i64 {
    (i128::from(hns) * i128::from(rate) / i128::from(HNS_PER_SEC)) as i64
}

/// 把采样帧数换算为 100 ns 时长，向零截断。
pub fn frames_to_hns(frames: i64, rate: u32) -> Hns {
    (i128::from(frames) * i128::from(HNS_PER_SEC) / i128::from(rate)) as i64
}

/// 毫秒换算为 100 ns 单位。
pub const fn ms_to_hns(ms: i64) -> Hns {
    ms * 10_000
}

fn div_round(num: i128, den: i128) -> i128 {
    let half = den / 2;
    if num >= 0 { (num + half) / den } else { (num - half) / den }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib clock::tests::
```

预期：通过，输出里有 `test result: ok. 6 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 统一时钟（QPC 与 100 ns 换算）"
```

---

## 任务 3：record-core：dsp

**目标：** 测量用的小型信号处理：Goertzel 单频检测、脉冲串检测、间隔与漂移估计。S1、S3 的分析工具与自动化测试共用，不用于录制路径。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/dsp.rs`

**接口：**

- 依赖：无（不依赖本工作区的其他模块）
- 提供：`pub fn tone_level(window: &[f32], freq: f64, rate: u32) -> f64`；`pub struct Burst`；`pub fn detect_bursts(mono: &[f32], rate: u32, freq: f64, threshold: f64) -> Vec<Burst>`；`pub struct Spacing`；`pub fn spacing(bursts: &[Burst], expected_interval_s: f64) -> Option<Spacing>`
- 测试：8 个：`tone_level_reads_the_amplitude_of_the_matching_frequency_only`、`bursts_are_found_with_millisecond_accuracy`、`silence_and_other_tones_are_not_bursts`、`a_burst_cut_by_the_end_of_the_buffer_is_still_reported`、`spacing_of_a_perfect_train_has_no_drift`、`spacing_reports_drift_in_ppm`、`spacing_flags_a_missing_burst_as_a_large_deviation`、`spacing_needs_at_least_three_bursts`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod dsp;
```

- [ ] **步骤 2：创建 `crates/record-core/src/dsp.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(freq: f64, secs: f64, amp: f32) -> Vec<f32> {
        (0..(secs * f64::from(RATE)) as usize).map(|n| (2.0 * PI * freq * n as f64 / f64::from(RATE)).sin() as f32 * amp).collect()
    }

    /// 总长 `total` 秒，每 `interval` 秒一个 150 ms 的 1 kHz 脉冲，第一个在 `first` 秒。
    fn beep_train(first: f64, interval: f64, total: f64, amp: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; (total * f64::from(RATE)) as usize];
        let tone = sine(1000.0, 0.15, amp);
        let mut t = first;
        while t + 0.15 <= total {
            let s = (t * f64::from(RATE)) as usize;
            v[s..s + tone.len()].copy_from_slice(&tone);
            t += interval;
        }
        v
    }

    #[test]
    fn tone_level_reads_the_amplitude_of_the_matching_frequency_only() {
        let w = sine(1000.0, 0.02, 0.4);
        assert!((tone_level(&w, 1000.0, RATE) - 0.4).abs() < 0.02);
        assert!(tone_level(&w, 2000.0, RATE) < 0.02);
        assert_eq!(tone_level(&[], 1000.0, RATE), 0.0);
    }

    #[test]
    fn bursts_are_found_with_millisecond_accuracy() {
        let v = beep_train(0.5, 1.0, 10.0, 0.4);
        let b = detect_bursts(&v, RATE, 1000.0, 0.2);
        assert_eq!(b.len(), 10, "0.5、1.5 … 9.5 秒各一个");
        for (k, burst) in b.iter().enumerate() {
            let expect = 0.5 + k as f64;
            assert!((burst.start_s - expect).abs() < 0.012, "第 {k} 个：{} 期望 {expect}", burst.start_s);
            assert!((burst.end_s - burst.start_s - 0.15).abs() < 0.02);
        }
    }

    #[test]
    fn silence_and_other_tones_are_not_bursts() {
        assert!(detect_bursts(&vec![0.0; 48_000], RATE, 1000.0, 0.2).is_empty());
        assert!(detect_bursts(&sine(440.0, 1.0, 0.5), RATE, 1000.0, 0.2).is_empty());
    }

    #[test]
    fn a_burst_cut_by_the_end_of_the_buffer_is_still_reported() {
        let mut v = vec![0.0f32; 48_000];
        let tone = sine(1000.0, 0.3, 0.4);
        v[40_000..].copy_from_slice(&tone[..8_000]);
        let b = detect_bursts(&v, RATE, 1000.0, 0.2);
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn spacing_of_a_perfect_train_has_no_drift() {
        let v = beep_train(0.5, 1.0, 20.0, 0.4);
        let s = spacing(&detect_bursts(&v, RATE, 1000.0, 0.2), 1.0).unwrap();
        assert_eq!(s.count, 20);
        assert!(s.drift_ppm.abs() < 500.0, "{s:?}");
        assert!(s.max_dev_ms < 8.0, "{s:?}");
    }

    #[test]
    fn spacing_reports_drift_in_ppm() {
        // 间隔 1.001 s：快慢 1000 ppm
        let bursts: Vec<Burst> = (0..30).map(|k| Burst { start_s: 0.2 + 1.001 * k as f64, end_s: 0.35 + 1.001 * k as f64 }).collect();
        let s = spacing(&bursts, 1.0).unwrap();
        assert!((s.drift_ppm - 1000.0).abs() < 1.0, "{s:?}");
        assert!((s.mean_interval_s - 1.001).abs() < 1e-9);
    }

    #[test]
    fn spacing_flags_a_missing_burst_as_a_large_deviation() {
        let bursts: Vec<Burst> = [0.0, 1.0, 2.0, 4.0, 5.0, 6.0].iter().map(|&t| Burst { start_s: t, end_s: t + 0.15 }).collect();
        let s = spacing(&bursts, 1.0).unwrap();
        assert!(s.max_dev_ms > 200.0, "{s:?}");
    }

    #[test]
    fn spacing_needs_at_least_three_bursts() {
        assert!(spacing(&[Burst { start_s: 0.0, end_s: 0.1 }; 2], 1.0).is_none());
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib dsp::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/dsp.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 测量用的小型信号处理：Goertzel 单频检测、脉冲串检测、间隔与漂移估计。
//! S1、S3 的分析工具与自动化测试共用，不用于录制路径。

use std::f64::consts::PI;

/// 加汉宁窗的 Goertzel：返回 `freq` 处的幅度估计，纯正弦时约等于其振幅。
pub fn tone_level(window: &[f32], freq: f64, rate: u32) -> f64 {
    let n = window.len();
    if n < 2 {
        return 0.0;
    }
    let (mut re, mut im, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
    for (i, &s) in window.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
        let phase = 2.0 * PI * freq * i as f64 / f64::from(rate);
        re += f64::from(s) * w * phase.cos();
        im -= f64::from(s) * w * phase.sin();
        wsum += w;
    }
    2.0 * (re * re + im * im).sqrt() / wsum
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burst {
    pub start_s: f64,
    pub end_s: f64,
}

/// 找出 `freq` 的脉冲串：10 ms 窗、5 ms 步长，音量达到 `threshold`（建议取脉冲振幅的一半）算有声。
/// 起止时刻取窗口中心，精度约 5 ms。
pub fn detect_bursts(mono: &[f32], rate: u32, freq: f64, threshold: f64) -> Vec<Burst> {
    let win = (rate / 100) as usize;
    let hop = (win / 2).max(1);
    let mut out = Vec::new();
    let mut open: Option<f64> = None;
    let mut last_active = 0.0;
    let mut i = 0;
    while i + win <= mono.len() {
        let t = (i + win / 2) as f64 / f64::from(rate);
        if tone_level(&mono[i..i + win], freq, rate) >= threshold {
            open.get_or_insert(t);
            last_active = t;
        } else if let Some(start) = open.take() {
            out.push(Burst { start_s: start, end_s: last_active });
        }
        i += hop;
    }
    if let Some(start) = open {
        out.push(Burst { start_s: start, end_s: last_active });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacing {
    pub count: usize,
    pub mean_interval_s: f64,
    /// 各脉冲相对“起点 + k × 期望间隔”的最大偏差（毫秒）。
    pub max_dev_ms: f64,
    /// 间隔相对期望值的漂移（百万分率）：正数表示实际间隔更长。
    pub drift_ppm: f64,
}

/// 用最小二乘拟合 `onset_k = a + b × k`，得到平均间隔、漂移与最大偏差。
pub fn spacing(bursts: &[Burst], expected_interval_s: f64) -> Option<Spacing> {
    let n = bursts.len();
    if n < 3 {
        return None;
    }
    let nf = n as f64;
    let (sx, sy): (f64, f64) = bursts.iter().enumerate().fold((0.0, 0.0), |(sx, sy), (k, b)| (sx + k as f64, sy + b.start_s));
    let (mx, my) = (sx / nf, sy / nf);
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for (k, b) in bursts.iter().enumerate() {
        sxx += (k as f64 - mx) * (k as f64 - mx);
        sxy += (k as f64 - mx) * (b.start_s - my);
    }
    let slope = sxy / sxx;
    let a = my - slope * mx;
    let max_dev = bursts.iter().enumerate().map(|(k, b)| (b.start_s - (a + expected_interval_s * k as f64)).abs()).fold(0.0, f64::max);
    // 最大偏差相对“期望间隔”的理想网格；允许整体平移，所以以拟合截距为基准
    Some(Spacing { count: n, mean_interval_s: slope, max_dev_ms: max_dev * 1000.0, drift_ppm: (slope / expected_interval_s - 1.0) * 1e6 })
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib dsp::tests::
```

预期：通过，输出里有 `test result: ok. 8 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 测量用信号处理（Goertzel、脉冲串检测、间隔与漂移估计）"
```

---

## 任务 4：record-core：geometry

**目标：** 画面输出尺寸与黑边布局（设计文档 5.2）。纯计算，不依赖任何系统接口。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/geometry.rs`

**接口：**

- 依赖：无（不依赖本工作区的其他模块）
- 提供：`pub struct Rect`；`Rect::right`；`Rect::bottom`；`Rect::center`；`pub fn output_size(src_w: u32, src_h: u32, max_long: u32, max_short: u32) -> (u32, u32)`；`pub fn letterbox(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Rect`；`pub fn map_to_output(r: Rect, src_w: u32, src_h: u32, layout: Rect) -> Rect`；`pub fn nv12_frame_bytes(w: u32, h: u32) -> usize`；`pub fn bars(layout: Rect, out_w: u32, out_h: u32) -> Vec<Rect>`
- 测试：16 个：`output_size_halves_4k_and_scales_1440p_to_1080p`、`output_size_never_enlarges`、`output_size_keeps_the_aspect_ratio_for_ultrawide_and_16_by_10`、`output_size_handles_portrait_screens`、`output_size_rounds_odd_sizes_down_to_even`、`letterbox_fills_the_output_when_the_aspect_matches`、`letterbox_adds_bars_on_the_sides_for_a_narrower_source`、`letterbox_adds_bars_on_top_and_bottom_for_a_wider_source`、`letterbox_enlarges_a_small_source_to_fit`、`layouts_are_even_and_inside_the_output_for_many_sizes` 等

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod geometry;
```

- [ ] **步骤 2：创建 `crates/record-core/src/geometry.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_size_halves_4k_and_scales_1440p_to_1080p() {
        assert_eq!(output_size(3840, 2160, 1920, 1080), (1920, 1080));
        assert_eq!(output_size(2560, 1440, 1920, 1080), (1920, 1080));
    }

    #[test]
    fn output_size_never_enlarges() {
        assert_eq!(output_size(1920, 1080, 1920, 1080), (1920, 1080));
        assert_eq!(output_size(1366, 768, 1920, 1080), (1366, 768));
    }

    #[test]
    fn output_size_keeps_the_aspect_ratio_for_ultrawide_and_16_by_10() {
        assert_eq!(output_size(3440, 1440, 1920, 1080), (1920, 804));
        assert_eq!(output_size(1920, 1200, 1920, 1080), (1728, 1080));
    }

    #[test]
    fn output_size_handles_portrait_screens() {
        assert_eq!(output_size(1080, 1920, 1920, 1080), (1080, 1920));
        assert_eq!(output_size(2160, 3840, 1920, 1080), (1080, 1920));
    }

    #[test]
    fn output_size_rounds_odd_sizes_down_to_even() {
        assert_eq!(output_size(1365, 767, 1920, 1080), (1364, 766));
        assert_eq!(output_size(2, 2, 1920, 1080), (2, 2));
        assert_eq!(output_size(1, 1, 1920, 1080), (2, 2));
    }

    #[test]
    fn letterbox_fills_the_output_when_the_aspect_matches() {
        assert_eq!(letterbox(3840, 2160, 1920, 1080), Rect { x: 0, y: 0, w: 1920, h: 1080 });
        assert_eq!(letterbox(1920, 1080, 1920, 1080), Rect { x: 0, y: 0, w: 1920, h: 1080 });
    }

    #[test]
    fn letterbox_adds_bars_on_the_sides_for_a_narrower_source() {
        // 4:3 进 16:9：左右黑边
        let r = letterbox(1024, 768, 1920, 1080);
        assert_eq!((r.y, r.h), (0, 1080));
        assert_eq!(r.w, 1440);
        assert_eq!(r.x, 240);
    }

    #[test]
    fn letterbox_adds_bars_on_top_and_bottom_for_a_wider_source() {
        let r = letterbox(3440, 1440, 1920, 1080);
        assert_eq!((r.x, r.w), (0, 1920));
        assert_eq!(r.h, 804);
        assert_eq!(r.y, 138);
    }

    #[test]
    fn letterbox_enlarges_a_small_source_to_fit() {
        // 显示模式从 4K 中途降到 1080p：输出尺寸不变，画面放大
        assert_eq!(letterbox(1280, 720, 1920, 1080), Rect { x: 0, y: 0, w: 1920, h: 1080 });
    }

    #[test]
    fn layouts_are_even_and_inside_the_output_for_many_sizes() {
        let mut seed = 0x9E37_79B9_u32;
        let mut next = |lo: u32, hi: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            lo + (seed >> 8) % (hi - lo + 1)
        };
        for _ in 0..2_000 {
            let (sw, sh) = (next(1, 8_000), next(1, 8_000));
            let (ow, oh) = output_size(sw, sh, 1920, 1080);
            assert!(ow.is_multiple_of(2) && oh.is_multiple_of(2) && ow >= 2 && oh >= 2, "{sw}x{sh} -> {ow}x{oh}");
            assert!(ow.max(oh) <= 1920 && ow.min(oh) <= 1080, "{sw}x{sh} -> {ow}x{oh}");
            // 显示模式变化后的另一个源尺寸，放进同一个输出
            let (mw, mh) = (next(1, 8_000), next(1, 8_000));
            let r = letterbox(mw, mh, ow, oh);
            assert!(r.x.is_multiple_of(2) && r.y.is_multiple_of(2) && r.w.is_multiple_of(2) && r.h.is_multiple_of(2), "{r:?}");
            assert!(r.right() <= ow && r.bottom() <= oh, "{mw}x{mh} 进 {ow}x{oh}：{r:?}");
        }
    }

    #[test]
    fn mapping_a_source_rect_follows_scale_and_offset() {
        let layout = letterbox(1024, 768, 1920, 1080); // x=240, w=1440, 缩放 1.40625
        let r = map_to_output(Rect { x: 512, y: 384, w: 256, h: 192 }, 1024, 768, layout);
        assert_eq!(r, Rect { x: 960, y: 540, w: 360, h: 270 });
        // 整屏映射回画面区域本身
        assert_eq!(map_to_output(Rect { x: 0, y: 0, w: 1024, h: 768 }, 1024, 768, layout), layout);
    }

    #[test]
    fn nv12_frame_size_is_one_and_a_half_bytes_per_pixel() {
        assert_eq!(nv12_frame_bytes(1920, 1080), 3_110_400);
        assert_eq!(nv12_frame_bytes(2, 2), 6);
    }

    #[test]
    fn a_full_frame_has_no_bars() {
        let layout = letterbox(3840, 2160, 1920, 1080);
        assert!(bars(layout, 1920, 1080).is_empty());
    }

    #[test]
    fn side_bars_are_left_and_right_of_the_picture() {
        let layout = letterbox(1024, 768, 1920, 1080); // x=240, w=1440
        assert_eq!(bars(layout, 1920, 1080), vec![Rect { x: 0, y: 0, w: 240, h: 1080 }, Rect { x: 1680, y: 0, w: 240, h: 1080 }]);
    }

    #[test]
    fn top_and_bottom_bars_leave_the_picture_rows_alone() {
        let layout = letterbox(3440, 1440, 1920, 1080); // y=138, h=804
        assert_eq!(bars(layout, 1920, 1080), vec![Rect { x: 0, y: 0, w: 1920, h: 138 }, Rect { x: 0, y: 942, w: 1920, h: 138 }]);
    }

    #[test]
    fn bars_and_picture_tile_the_output_exactly() {
        let mut seed = 0xC0FF_EE11_u32;
        let mut next = |lo: u32, hi: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            lo + (seed >> 8) % (hi - lo + 1)
        };
        for _ in 0..500 {
            let (ow, oh) = (next(1, 1000) * 2, next(1, 1000) * 2);
            let layout = letterbox(next(1, 8_000), next(1, 8_000), ow, oh);
            let area: u64 = bars(layout, ow, oh).iter().map(|r| u64::from(r.w) * u64::from(r.h)).sum::<u64>() + u64::from(layout.w) * u64::from(layout.h);
            assert_eq!(area, u64::from(ow) * u64::from(oh), "{layout:?} 在 {ow}x{oh}");
        }
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib geometry::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/geometry.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 画面输出尺寸与黑边布局（设计文档 5.2）。纯计算，不依赖任何系统接口。
//!
//! NV12 的色度按 2×2 采样，所以输出的宽、高以及画面区域的位置和大小都必须是偶数。

/// 像素矩形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn right(&self) -> u32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> u32 {
        self.y + self.h
    }

    pub fn center(&self) -> (u32, u32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

fn even(v: u32) -> u32 {
    v & !1
}

/// 录制开始时确定输出尺寸：保持宽高比，长边 ≤ `max_long`，短边 ≤ `max_short`，不放大；宽高取偶数且不小于 2。
pub fn output_size(src_w: u32, src_h: u32, max_long: u32, max_short: u32) -> (u32, u32) {
    assert!(src_w > 0 && src_h > 0, "源画面尺寸必须为正");
    assert!(max_long >= 2 && max_short >= 2, "输出上限至少为 2");
    let (long, short) = (src_w.max(src_h), src_w.min(src_h));
    // 边长不超过 16384，f64 的误差远小于 1 像素
    let scale = (f64::from(max_long) / f64::from(long)).min(f64::from(max_short) / f64::from(short)).min(1.0);
    let w = even((f64::from(src_w) * scale).round() as u32).max(2);
    let h = even((f64::from(src_h) * scale).round() as u32).max(2);
    (w, h)
}

/// 把 `src` 等比缩放后居中放进 `dst`（输出尺寸固定，显示模式中途变化时靠它加黑边），返回画面区域。
/// 源比输出小时会放大。`dst_w`、`dst_h` 必须是偶数。
pub fn letterbox(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Rect {
    assert!(src_w > 0 && src_h > 0, "源画面尺寸必须为正");
    assert!(dst_w >= 2 && dst_h >= 2 && dst_w.is_multiple_of(2) && dst_h.is_multiple_of(2), "输出尺寸必须是不小于 2 的偶数");
    let scale = (f64::from(dst_w) / f64::from(src_w)).min(f64::from(dst_h) / f64::from(src_h));
    let w = even((f64::from(src_w) * scale).round() as u32).clamp(2, dst_w);
    let h = even((f64::from(src_h) * scale).round() as u32).clamp(2, dst_h);
    Rect { x: even((dst_w - w) / 2), y: even((dst_h - h) / 2), w, h }
}

/// 源画面里的矩形 `r`，在输出画面中的位置。`layout` 是 [`letterbox`] 的结果。
pub fn map_to_output(r: Rect, src_w: u32, src_h: u32, layout: Rect) -> Rect {
    let sx = f64::from(layout.w) / f64::from(src_w);
    let sy = f64::from(layout.h) / f64::from(src_h);
    let x0 = f64::from(layout.x) + f64::from(r.x) * sx;
    let y0 = f64::from(layout.y) + f64::from(r.y) * sy;
    let x1 = f64::from(layout.x) + f64::from(r.right()) * sx;
    let y1 = f64::from(layout.y) + f64::from(r.bottom()) * sy;
    Rect { x: x0.round() as u32, y: y0.round() as u32, w: (x1.round() - x0.round()) as u32, h: (y1.round() - y0.round()) as u32 }
}

/// 一帧 NV12 的字节数：Y 平面 `w*h`，UV 平面 `w*h/2`。
pub fn nv12_frame_bytes(w: u32, h: u32) -> usize {
    w as usize * h as usize * 3 / 2
}

/// 输出画面里没被源画面盖住的黑边（上、下、左、右；宽或高为 0 的不返回）。
pub fn bars(layout: Rect, out_w: u32, out_h: u32) -> Vec<Rect> {
    [
        Rect { x: 0, y: 0, w: out_w, h: layout.y },
        Rect { x: 0, y: layout.bottom(), w: out_w, h: out_h.saturating_sub(layout.bottom()) },
        Rect { x: 0, y: layout.y, w: layout.x, h: layout.h },
        Rect { x: layout.right(), y: layout.y, w: out_w.saturating_sub(layout.right()), h: layout.h },
    ]
    .into_iter()
    .filter(|r| r.w > 0 && r.h > 0)
    .collect()
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib geometry::tests::
```

预期：通过，输出里有 `test result: ok. 16 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 输出尺寸与黑边布局"
```

---

## 任务 5：record-core：hotkey

**目标：** 热键字符串的解析与规范化（如 `ctrl+alt+r` → `Ctrl+Alt+R`）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/hotkey.rs`

**接口：**

- 依赖：无（不依赖本工作区的其他模块）
- 提供：`pub struct Hotkey`；`pub enum HotkeyError`；`Hotkey::parse`；`Hotkey::plugin_string`
- 测试：5 个：`normalizes_case_spacing_and_order`、`rejects_bad_hotkeys`、`function_keys_cover_one_to_twenty_four`、`the_plugin_wants_super_where_we_write_win`、`display_round_trips`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod hotkey;
```

- [ ] **步骤 2：创建 `crates/record-core/src/hotkey.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        Hotkey::parse(s).unwrap().to_string()
    }

    #[test]
    fn normalizes_case_spacing_and_order() {
        assert_eq!(norm("ctrl+alt+r"), "Ctrl+Alt+R");
        assert_eq!(norm("  Alt + Ctrl + S "), "Ctrl+Alt+S");
        assert_eq!(norm("CONTROL+SHIFT+f5"), "Ctrl+Shift+F5");
        assert_eq!(norm("win+space"), "Win+Space");
        assert_eq!(norm("Ctrl+Alt+Q"), "Ctrl+Alt+Q");
        assert_eq!(norm("ctrl+ctrl+1"), "Ctrl+1");
    }

    #[test]
    fn rejects_bad_hotkeys() {
        assert_eq!(Hotkey::parse(""), Err(HotkeyError::Empty));
        assert!(matches!(Hotkey::parse("R"), Err(HotkeyError::NoModifier(_))));
        assert!(matches!(Hotkey::parse("Ctrl+Alt"), Err(HotkeyError::NoKey(_))));
        assert!(matches!(Hotkey::parse("Ctrl+"), Err(HotkeyError::EmptyPart(_))));
        assert!(matches!(Hotkey::parse("Ctrl+A+B"), Err(HotkeyError::MultipleKeys(_))));
        assert!(matches!(Hotkey::parse("Ctrl+F25"), Err(HotkeyError::UnsupportedKey(_))));
        assert!(matches!(Hotkey::parse("Ctrl+Enter"), Err(HotkeyError::UnsupportedKey(_))));
        assert!(matches!(Hotkey::parse("Ctrl+Alt+中"), Err(HotkeyError::UnsupportedKey(_))));
    }

    #[test]
    fn function_keys_cover_one_to_twenty_four() {
        for n in 1..=24 {
            assert_eq!(norm(&format!("alt+f{n}")), format!("Alt+F{n}"));
        }
    }

    #[test]
    fn the_plugin_wants_super_where_we_write_win() {
        assert_eq!(Hotkey::parse("alt+win+9").unwrap().plugin_string(), "Alt+Super+9");
        assert_eq!(Hotkey::parse("ctrl+alt+r").unwrap().plugin_string(), "Ctrl+Alt+R");
        assert_eq!(Hotkey::parse("Win+Space").unwrap().plugin_string(), "Super+Space");
    }

    #[test]
    fn display_round_trips() {
        for s in ["Ctrl+Alt+R", "Ctrl+Shift+F12", "Alt+Win+9"] {
            assert_eq!(norm(s), s);
        }
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib hotkey::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/hotkey.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 热键字符串的解析与规范化（如 `ctrl+alt+r` → `Ctrl+Alt+R`）。
//!
//! 只接受最保守的一组按键：字母、数字、F1–F24、Space，并且至少带一个修饰键，
//! 避免把普通打字吞掉。交给 Tauri 的 global-shortcut 插件时用 [`Hotkey::plugin_string`]（插件不认 `Win`，要写 `Super`）。

use std::fmt;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HotkeyError {
    #[error("热键为空")]
    Empty,
    #[error("热键“{0}”里有空的部分")]
    EmptyPart(String),
    #[error("热键“{0}”没有主键")]
    NoKey(String),
    #[error("热键“{0}”有多个主键")]
    MultipleKeys(String),
    #[error("热键“{0}”没有修饰键（Ctrl、Alt、Shift、Win 之一），会吞掉普通打字")]
    NoModifier(String),
    #[error("不支持的按键“{0}”（支持字母、数字、F1–F24 和 Space）")]
    UnsupportedKey(String),
}

impl Hotkey {
    pub fn parse(text: &str) -> Result<Self, HotkeyError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(HotkeyError::Empty);
        }
        let mut hk = Hotkey { ctrl: false, alt: false, shift: false, win: false, key: String::new() };
        for part in text.split('+') {
            let part = part.trim();
            if part.is_empty() {
                return Err(HotkeyError::EmptyPart(text.to_string()));
            }
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hk.ctrl = true,
                "alt" | "option" => hk.alt = true,
                "shift" => hk.shift = true,
                "win" | "super" | "meta" | "cmd" | "command" => hk.win = true,
                _ => {
                    if !hk.key.is_empty() {
                        return Err(HotkeyError::MultipleKeys(text.to_string()));
                    }
                    hk.key = canonical_key(part)?;
                }
            }
        }
        if hk.key.is_empty() {
            return Err(HotkeyError::NoKey(text.to_string()));
        }
        if !(hk.ctrl || hk.alt || hk.shift || hk.win) {
            return Err(HotkeyError::NoModifier(text.to_string()));
        }
        Ok(hk)
    }

    /// 交给 global-shortcut 插件的写法：它的解析器只认 `Super`，不认 `Win`（S5 实测），其余与 [`Display`](fmt::Display) 相同。
    pub fn plugin_string(&self) -> String {
        self.to_string().replace("Win+", "Super+")
    }
}

fn canonical_key(part: &str) -> Result<String, HotkeyError> {
    let upper = part.to_ascii_uppercase();
    let b = upper.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Ok(upper);
    }
    if upper == "SPACE" {
        return Ok("Space".to_string());
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|d| d.parse::<u8>().ok())
        && (1..=24).contains(&n)
    {
        return Ok(format!("F{n}"));
    }
    Err(HotkeyError::UnsupportedKey(part.to_string()))
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.win {
            f.write_str("Win+")?;
        }
        f.write_str(&self.key)
    }
}

impl FromStr for Hotkey {
    type Err = HotkeyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Hotkey::parse(s)
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib hotkey::tests::
```

预期：通过，输出里有 `test result: ok. 5 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 热键字符串的解析与规范化"
```

---

## 任务 6：record-core：syncprobe

**目标：** 画面与声音的同步测量（设计文档 10 的 S3）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/syncprobe.rs`

**接口：**

- 依赖：无（不依赖本工作区的其他模块）
- 提供：`pub fn flash_onsets(lum: &[u8], fps: f64, start: f64) -> Vec<f64>`；`pub struct ToneLevels`；`ToneLevels::new`；`ToneLevels::onsets`；`ToneLevels::onset_near`；`pub fn tone_onsets(samples: &[f32], rate: u32, freq: f64, start: f64) -> Vec<f64>`；`pub struct Pair`；`Pair::delta`；`pub fn pair_flashes(tone: &ToneLevels, flashes: &[f64], radius: f64) -> Vec<Pair>`；`pub fn keep_consistent(pairs: &[Pair], tolerance: f64) -> (Vec<Pair>, usize)`；`pub struct Estimate`；`pub fn estimate(pairs: &[Pair], fps: f64) -> Option<Estimate>`；`pub fn within(e: &Estimate, limit: f64) -> bool`
- 测试：13 个：`a_flash_shows_up_as_the_first_bright_frame`、`the_series_start_time_shifts_every_onset`、`a_picture_with_no_flashes_gives_nothing`、`noise_around_the_threshold_does_not_make_extra_edges`、`the_onset_of_a_test_tone_is_found_within_three_milliseconds`、`other_frequencies_and_silence_are_not_mistaken_for_the_tone`、`a_beep_is_found_around_its_flash_even_when_something_louder_sounds_elsewhere`、`a_sustained_tone_or_a_window_that_starts_mid_sound_gives_no_onset`、`each_flash_is_paired_with_the_beep_around_it`、`pairs_far_from_the_median_are_dropped` 等

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod syncprobe;
```

- [ ] **步骤 2：创建 `crates/record-core/src/syncprobe.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FPS: f64 = 15.0;
    const RATE: u32 = 48_000;

    /// 在 `fps` 下合成亮度序列：`flashes` 里每个时刻开始变白，持续 `on` 秒；第 i 帧看到的是 `t_i` 时刻的屏幕。
    fn frames(flashes: &[f64], on: f64, secs: f64, start: f64) -> Vec<u8> {
        (0..(secs * FPS) as usize)
            .map(|i| {
                let t = start + i as f64 / FPS;
                if flashes.iter().any(|&s| t >= s && t < s + on) { 235 } else { 16 }
            })
            .collect()
    }

    /// 合成单声道音频：`beeps` 里每个时刻起一声 `dur` 秒的 `freq` 测试音，2 毫秒淡入。
    fn audio(beeps: &[f64], freq: f64, dur: f64, secs: f64) -> Vec<f32> {
        (0..(secs * f64::from(RATE)) as usize)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                beeps.iter().find(|&&b| t >= b && t < b + dur).map_or(0.0, |&b| ((t - b) / 0.002).min(1.0) as f32 * 0.3 * (2.0 * std::f64::consts::PI * freq * t).sin() as f32)
            })
            .collect()
    }

    #[test]
    fn a_flash_shows_up_as_the_first_bright_frame() {
        let lum = frames(&[1.0, 3.0], 0.3, 5.0, 0.0);
        let on = flash_onsets(&lum, FPS, 0.0);
        assert_eq!(on.len(), 2);
        // 1.0 秒正好是第 15 帧；3.0 秒是第 45 帧
        assert!((on[0] - 1.0).abs() < 1e-9 && (on[1] - 3.0).abs() < 1e-9, "{on:?}");
    }

    #[test]
    fn the_series_start_time_shifts_every_onset() {
        let lum = frames(&[1.0], 0.3, 3.0, 0.0);
        assert!((flash_onsets(&lum, FPS, 10.0)[0] - 11.0).abs() < 1e-9);
    }

    #[test]
    fn a_picture_with_no_flashes_gives_nothing() {
        assert!(flash_onsets(&[120u8; 100], FPS, 0.0).is_empty());
        assert!(flash_onsets(&[], FPS, 0.0).is_empty());
        let weak: Vec<u8> = (0..100).map(|i| if i % 10 < 5 { 100 } else { 120 }).collect();
        assert!(flash_onsets(&weak, FPS, 0.0).is_empty(), "亮暗只差 20 级不算闪屏");
    }

    #[test]
    fn noise_around_the_threshold_does_not_make_extra_edges() {
        let mut lum = frames(&[1.0], 0.5, 3.0, 0.0);
        for (i, v) in lum.iter_mut().enumerate() {
            *v = v.saturating_add((i % 3) as u8 * 4).saturating_sub(4);
        }
        assert_eq!(flash_onsets(&lum, FPS, 0.0).len(), 1);
    }

    #[test]
    fn the_onset_of_a_test_tone_is_found_within_three_milliseconds() {
        // 测试音有 2 毫秒淡入，幅度到一半要 1 毫秒，加上 1 毫秒的步进，所以偏晚 1–2 毫秒是正常的
        let beeps = [0.5, 2.5, 4.5];
        let on = tone_onsets(&audio(&beeps, 1000.0, 0.15, 6.0), RATE, 1000.0, 0.0);
        assert_eq!(on.len(), 3, "{on:?}");
        for (found, want) in on.iter().zip(beeps) {
            assert!((found - want).abs() < 0.003, "找到 {found}，真实 {want}");
        }
    }

    #[test]
    fn other_frequencies_and_silence_are_not_mistaken_for_the_tone() {
        assert!(tone_onsets(&audio(&[1.0], 3000.0, 0.15, 3.0), RATE, 1000.0, 0.0).is_empty());
        assert!(tone_onsets(&vec![0.0; 48_000], RATE, 1000.0, 0.0).is_empty());
        assert!(tone_onsets(&[0.0; 10], RATE, 1000.0, 0.0).is_empty());
    }

    #[test]
    fn a_beep_is_found_around_its_flash_even_when_something_louder_sounds_elsewhere() {
        // 3.0 秒处有一声很响的同频率声音（别的程序），0.5 秒和 5.5 秒是我们的测试音
        let mut samples = audio(&[0.5, 5.5], 1000.0, 0.15, 7.0);
        for (s, loud) in samples.iter_mut().zip(audio(&[3.0], 1000.0, 0.15, 7.0)) {
            *s += loud * 3.0;
        }
        let tone = ToneLevels::new(&samples, RATE, 1000.0, 0.0);
        assert!((tone.onset_near(0.55, 0.3).unwrap() - 0.5).abs() < 0.003);
        assert!((tone.onset_near(5.6, 0.3).unwrap() - 5.5).abs() < 0.003);
        assert_eq!(tone.onset_near(3.5, 0.2), None, "这个范围里没有突起的声音");
        // 全局找会被响的那声压住：阈值按峰值的一半算，我们自己的测试音（幅度只有 1/3）不到一半
        assert_eq!(tone_onsets(&samples, RATE, 1000.0, 0.0).len(), 1);
    }

    #[test]
    fn a_sustained_tone_or_a_window_that_starts_mid_sound_gives_no_onset() {
        let hum = audio(&[0.0], 1000.0, 4.0, 4.0);
        let tone = ToneLevels::new(&hum, RATE, 1000.0, 0.0);
        assert_eq!(tone.onset_near(2.0, 1.0), None, "持续不断的声音没有起点");
        assert_eq!(tone.onset_near(0.1, 0.05), None, "窗口一开始就在响");
        assert_eq!(tone.onset_near(100.0, 1.0), None, "窗口在录音之外");
        assert_eq!(ToneLevels::new(&[], RATE, 1000.0, 0.0).onset_near(1.0, 1.0), None);
    }

    #[test]
    fn each_flash_is_paired_with_the_beep_around_it() {
        let tone = ToneLevels::new(&audio(&[1.05, 3.62], 1000.0, 0.15, 6.0), RATE, 1000.0, 0.0);
        let pairs = pair_flashes(&tone, &[1.00, 3.00, 5.0], 1.0);
        assert_eq!(pairs.len(), 2, "5.0 秒的闪屏附近没有测试音");
        assert!((pairs[0].delta() + 0.05).abs() < 0.003 && (pairs[1].delta() + 0.62).abs() < 0.003, "{pairs:?}");
    }

    #[test]
    fn pairs_far_from_the_median_are_dropped() {
        let pair = |d: f64| Pair { video: 10.0 + d, audio: 10.0 };
        let (kept, dropped) = keep_consistent(&[pair(-0.03), pair(-0.05), pair(-0.02), pair(0.4), pair(-0.04)], 0.13);
        assert_eq!((kept.len(), dropped), (4, 1));
        assert!(kept.iter().all(|p| p.delta() < 0.0));
        assert_eq!(keep_consistent(&[], 0.1), (Vec::new(), 0));
    }

    /// 端到端的合成：画面比声音早 40 毫秒（负偏移），闪烁间隔与帧周期错开，估计值应落在真实值附近。
    fn simulate(true_offset: f64, interval: f64, count: usize) -> Estimate {
        let first = 1.0;
        let flashes: Vec<f64> = (0..count).map(|k| first + k as f64 * interval).collect();
        // 声音在闪屏之后 `-true_offset` 秒出现（真实偏移 = 画面可见时刻 − 声音可闻时刻）
        let beeps: Vec<f64> = flashes.iter().map(|s| s - true_offset).collect();
        let secs = first + count as f64 * interval + 1.0;
        let video = flash_onsets(&frames(&flashes, 0.3, secs, 0.0), FPS, 0.0);
        let tone = ToneLevels::new(&audio(&beeps, 1000.0, 0.15, secs), RATE, 1000.0, 0.0);
        let pairs = pair_flashes(&tone, &video, 1.0);
        assert_eq!(pairs.len(), count, "每次闪烁都应配上对");
        estimate(&pairs, FPS).unwrap()
    }

    #[test]
    fn the_estimate_recovers_the_true_offset_when_the_flash_phase_sweeps_the_frame_grid() {
        // 间隔 2 + 1/120 秒：相位每次错开 1/8 帧
        for true_offset in [-0.040, 0.0, 0.060] {
            let e = simulate(true_offset, 2.0 + 1.0 / 120.0, 16);
            assert!((e.offset - true_offset).abs() < 0.008, "真实 {true_offset}，估计 {e:?}");
            assert!(e.spread > 0.05 && e.spread < 0.075, "相位扫满一个帧周期，差值的范围应约等于 66.7 ms：{e:?}");
        }
    }

    #[test]
    fn a_fixed_phase_cannot_be_corrected_which_is_why_the_interval_must_not_be_a_whole_number_of_frames() {
        // 间隔恰好是整数帧：每次落在帧格上的同一位置，修正“半帧”会带来最多 ±33 ms 的系统误差，且 spread 为 0
        let e = simulate(0.0, 2.0, 8);
        assert!(e.spread < 1e-6, "{e:?}");
    }

    #[test]
    fn the_verdict_accounts_for_the_uncertainty() {
        let e = Estimate { n: 4, offset: 0.070, spread: 0.067, raw_mean: 0.1, uncertainty: 0.01 };
        assert!(!within(&e, 0.080), "70 ms 加两倍不确定度 20 ms 已经超过 80 ms");
        assert!(within(&Estimate { offset: -0.050, ..e }, 0.080));
        assert!(estimate(&[], FPS).is_none());
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib syncprobe::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/syncprobe.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 画面与声音的同步测量（设计文档 10 的 S3）。
//!
//! 测试时放一个整块黑白闪烁的窗口，每次变白的同时播放一声 1 kHz 的短音。录完之后从成品里找：
//! 画面亮度序列的上升沿（闪屏），声音里测试音的起点；配对后得到“画面比声音晚多少秒”。
//!
//! 画面只有每秒 15 帧：闪屏出现在某两帧之间，成品里“第一个亮帧”的时刻总是晚于真实时刻，平均晚半帧。
//! 所以闪烁的间隔要和帧周期错开（相位均匀扫过），再把平均值减掉半帧；声音的起点精度是毫秒级，不用修正。

/// 取 `data` 的 `p` 分位数（0.0–1.0）。
fn percentile(data: &[u8], p: f64) -> f64 {
    let mut v = data.to_vec();
    v.sort_unstable();
    f64::from(v[((v.len() - 1) as f64 * p).round() as usize])
}

/// 画面亮度序列里“暗 → 亮”的上升沿：返回每次第一个亮帧的时刻（秒）。
/// `lum[i]` 是第 i 帧的平均亮度，`start` 是第 0 帧的时刻。亮暗差不到 40 级时认为没有闪屏，返回空。
pub fn flash_onsets(lum: &[u8], fps: f64, start: f64) -> Vec<f64> {
    if lum.is_empty() {
        return Vec::new();
    }
    let (lo, hi) = (percentile(lum, 0.05), percentile(lum, 0.95));
    if hi - lo < 40.0 {
        return Vec::new();
    }
    // 迟滞：要冲过上沿才算亮，掉到下沿以下才算暗，编码噪声不会造成假的上升沿
    let (mid, quarter) = ((lo + hi) / 2.0, (hi - lo) / 4.0);
    let mut bright = f64::from(lum[0]) > mid;
    let mut onsets = Vec::new();
    for (i, &v) in lum.iter().enumerate() {
        let v = f64::from(v);
        if !bright && v > mid + quarter {
            bright = true;
            onsets.push(start + i as f64 / fps);
        } else if bright && v < mid - quarter {
            bright = false;
        }
    }
    onsets
}

/// 汉宁窗加权的 Goertzel：`window` 里 `freq` 处的幅度估计（纯正弦时约等于振幅）。
fn tone_level(window: &[f32], freq: f64, rate: f64) -> f64 {
    let n = window.len();
    let (mut re, mut im, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
    for (i, &s) in window.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
        let phase = 2.0 * std::f64::consts::PI * freq * i as f64 / rate;
        re += f64::from(s) * w * phase.cos();
        im -= f64::from(s) * w * phase.sin();
        wsum += w;
    }
    2.0 * (re * re + im * im).sqrt() / wsum
}

/// 低于这个幅度（满量程为 1）认为没有测试音。
const MIN_LEVEL: f64 = 0.01;

/// 单声道音频在测试音频率处的幅度序列：每 1 毫秒看一个 10 毫秒的窗口。
pub struct ToneLevels {
    levels: Vec<f64>,
    start: f64,
    hop: f64,
    half_window: f64,
}

impl ToneLevels {
    /// `start` 是第 0 个采样的时刻（秒）。
    pub fn new(samples: &[f32], rate: u32, freq: f64, start: f64) -> Self {
        let (win, hop) = ((rate / 100) as usize, (rate / 1000) as usize);
        let levels = if samples.len() < win + hop { Vec::new() } else { (0..=samples.len() - win).step_by(hop).map(|i| tone_level(&samples[i..i + win], freq, f64::from(rate))).collect() };
        Self { levels, start, hop: hop as f64 / f64::from(rate), half_window: win as f64 / 2.0 / f64::from(rate) }
    }

    /// 第 `k` 个窗口的中心时刻。窗口一半被测试音盖住时幅度正好是一半，所以幅度过半时的窗口中心就是起点。
    fn time_of(&self, k: usize) -> f64 {
        self.start + k as f64 * self.hop + self.half_window
    }

    /// 全部测试音的起点（秒）：幅度第一次超过峰值的一半。适合安静环境里的单一信号；有别的声音时用 [`onset_near`](Self::onset_near)。
    pub fn onsets(&self) -> Vec<f64> {
        let peak = self.levels.iter().copied().fold(0.0, f64::max);
        if peak < MIN_LEVEL {
            return Vec::new();
        }
        let (up, down) = (peak * 0.5, peak * 0.25);
        let mut sounding = self.levels[0] > up;
        let mut onsets = Vec::new();
        for (k, &l) in self.levels.iter().enumerate() {
            if !sounding && l > up {
                sounding = true;
                onsets.push(self.time_of(k));
            } else if sounding && l < down {
                sounding = false;
            }
        }
        onsets
    }

    /// `center` 前后各 `radius` 秒之内那一声的起点：范围内要有一个明显突起的声音（峰值不低于 [`MIN_LEVEL`]，
    /// 且至少是范围内中位数的 4 倍），起点取从“安静”冲过峰值一半的位置。一开始就在响的、持续不断的声音返回 `None`。
    pub fn onset_near(&self, center: f64, radius: f64) -> Option<f64> {
        let index = |t: f64| ((t - self.start - self.half_window) / self.hop).floor().max(0.0) as usize;
        let (lo, hi) = (index(center - radius).min(self.levels.len()), (index(center + radius) + 1).min(self.levels.len()));
        let part = self.levels.get(lo..hi).filter(|p| !p.is_empty())?;
        let peak = part.iter().copied().fold(0.0, f64::max);
        let mut sorted = part.to_vec();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        if peak < MIN_LEVEL || peak < 4.0 * median {
            return None;
        }
        let (up, down) = (peak * 0.5, peak * 0.25);
        let mut quiet = false;
        for (i, &l) in part.iter().enumerate() {
            if l < down {
                quiet = true;
            } else if quiet && l > up {
                return Some(self.time_of(lo + i));
            }
        }
        None
    }
}

/// 单声道音频里 `freq` 测试音的起点（秒），见 [`ToneLevels::onsets`]。
pub fn tone_onsets(samples: &[f32], rate: u32, freq: f64, start: f64) -> Vec<f64> {
    ToneLevels::new(samples, rate, freq, start).onsets()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pair {
    pub video: f64,
    pub audio: f64,
}

impl Pair {
    /// 画面比声音晚的秒数；负数表示画面先到。
    pub fn delta(&self) -> f64 {
        self.video - self.audio
    }
}

/// 每个闪屏配上它前后 `radius` 秒之内的测试音；附近听不到测试音的闪屏跳过。`radius` 要小于刺激的间隔的一半以内，免得配到邻居。
pub fn pair_flashes(tone: &ToneLevels, flashes: &[f64], radius: f64) -> Vec<Pair> {
    flashes.iter().filter_map(|&video| tone.onset_near(video, radius).map(|audio| Pair { video, audio })).collect()
}

/// 去掉与中位数差得超过 `tolerance` 秒的配对（别的声音或误检造成），返回保留的配对和去掉的个数。
pub fn keep_consistent(pairs: &[Pair], tolerance: f64) -> (Vec<Pair>, usize) {
    if pairs.is_empty() {
        return (Vec::new(), 0);
    }
    let mut deltas: Vec<f64> = pairs.iter().map(Pair::delta).collect();
    deltas.sort_by(f64::total_cmp);
    let median = deltas[deltas.len() / 2];
    let kept: Vec<Pair> = pairs.iter().copied().filter(|p| (p.delta() - median).abs() <= tolerance).collect();
    let dropped = pairs.len() - kept.len();
    (kept, dropped)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub n: usize,
    /// 修正帧量化之后的偏移（秒）：画面比声音晚为正。
    pub offset: f64,
    /// 各对之间最大与最小差值之差。相位扫得均匀时应接近一个帧周期；明显更大说明有抖动。
    pub spread: f64,
    /// 修正前的平均差值。
    pub raw_mean: f64,
    /// 因“只能看到帧”带来的不确定度（一倍标准差，秒）。
    pub uncertainty: f64,
}

/// 由配好的对估计偏移。`fps` 是画面帧率。没有配对时返回 `None`。
pub fn estimate(pairs: &[Pair], fps: f64) -> Option<Estimate> {
    if pairs.is_empty() {
        return None;
    }
    let period = 1.0 / fps;
    let deltas: Vec<f64> = pairs.iter().map(Pair::delta).collect();
    let n = deltas.len();
    let raw_mean = deltas.iter().sum::<f64>() / n as f64;
    let (min, max) = deltas.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &d| (lo.min(d), hi.max(d)));
    Some(Estimate { n, offset: raw_mean - period / 2.0, spread: max - min, raw_mean, uncertainty: period / 12f64.sqrt() / (n as f64).sqrt() })
}

/// 判定：偏移的绝对值不超过 `limit`（秒）。把不确定度算进去：偏移加减两倍标准差仍在限内才算通过。
pub fn within(e: &Estimate, limit: f64) -> bool {
    e.offset.abs() + 2.0 * e.uncertainty <= limit
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib syncprobe::tests::
```

预期：通过，输出里有 `test result: ok. 13 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 画面与声音的同步测量"
```

---

## 任务 7：record-core：timeline

**目标：** 时间线修复：补静音与漂移修正（设计文档 5.5）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/timeline.rs`

**接口：**

- 依赖：`crate::clock::{Hns, frames_to_hns, hns_to_frames, ms_to_hns}`
- 提供：`pub struct AudioFormat`；`pub enum TimeSource`；`pub struct PacketFlags`；`pub struct AudioPacket<'a>`；`pub struct TimelineConfig`；`TimelineConfig::new`；`pub struct TimelineStats`；`pub struct Report`；`pub struct AudioTimeline`；`AudioTimeline::new`；`AudioTimeline::anchor`；`AudioTimeline::written_frames`；`AudioTimeline::stats`；`AudioTimeline::take_peak`；`AudioTimeline::push_packet`；`AudioTimeline::stream_started`；`AudioTimeline::tick`；`AudioTimeline::tick_outage`；`AudioTimeline::finish`
- 测试：30 个：`contiguous_packets_pass_through_unchanged`、`leading_gap_is_filled_and_leaves_settle_residual`、`timestamp_jitter_inside_tolerance_is_ignored`、`idle_ticks_keep_the_timeline_continuous`、`packet_after_idle_continues_without_big_correction`、`short_delivery_stall_does_not_lose_real_audio`、`long_delivery_stall_trades_late_audio_for_silence_and_stays_aligned`、`finish_pads_exactly_to_the_end_time`、`fast_device_clock_is_corrected_by_dropping`、`slow_device_clock_is_corrected_by_filling` 等

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod timeline;
```

- [ ] **步骤 2：创建 `crates/record-core/src/timeline.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::HNS_PER_SEC;

    const RATE: u32 = 48_000;
    const CH: u16 = 2;

    fn cfg() -> TimelineConfig {
        TimelineConfig::new(AudioFormat { rate: RATE, channels: CH })
    }

    fn dur(frames: usize) -> Hns {
        frames_to_hns(frames as i64, RATE)
    }

    /// 构造一个设备时间戳为 `t` 的包，数据全部为 `value`；到达时间为包结束时刻。
    fn packet(t: Hns, frames: usize, data: &[f32]) -> AudioPacket<'_> {
        AudioPacket { device_qpc: Some(t), arrival_qpc: t + dur(frames), frames, samples: data, flags: PacketFlags::default() }
    }

    fn run_push(tl: &mut AudioTimeline, pkt: &AudioPacket<'_>, out: &mut Vec<f32>) -> Report {
        tl.push_packet(pkt, &mut |s| out.extend_from_slice(s))
    }

    #[test]
    fn contiguous_packets_pass_through_unchanged() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.25f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..100 {
            let r = run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
            assert_eq!(r, Report::default());
        }
        assert_eq!(out.len(), 100 * 480 * 2);
        assert!(out.iter().all(|&s| s == 0.25));
        let st = tl.stats();
        assert_eq!(st.silence_filled_frames, 0);
        assert_eq!(st.dropped_frames, 0);
        assert_eq!(st.frames_written, 48_000);
    }

    #[test]
    fn leading_gap_is_filled_and_leaves_settle_residual() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        // 首包在起点之后 100 ms 才到：应补 100 ms − 10 ms 残差 = 90 ms 静音
        let r = run_push(&mut tl, &packet(ms_to_hns(100), 480, &data), &mut out);
        assert_eq!(r.filled_frames, 4_320);
        assert!(!r.large_gap);
        assert_eq!(out.len(), (4_320 + 480) * 2);
        assert!(out[..4_320 * 2].iter().all(|&s| s == 0.0));
        assert!(out[4_320 * 2..].iter().all(|&s| s == 0.5));
        assert_eq!(tl.written_frames(), 4_800);
    }

    #[test]
    fn timestamp_jitter_inside_tolerance_is_ignored() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..200i64 {
            // 时间戳在 ±30 ms 内抖动，但帧数是连续的
            let jitter = if k % 2 == 0 { ms_to_hns(30) } else { -ms_to_hns(30) };
            run_push(&mut tl, &packet(dur(480) * k + jitter, 480, &data), &mut out);
        }
        let st = tl.stats();
        assert_eq!(st.silence_filled_frames, 0);
        assert_eq!(st.dropped_frames, 0);
        assert_eq!(out.len(), 200 * 480 * 2);
    }

    #[test]
    fn idle_ticks_keep_the_timeline_continuous() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut total = 0usize;
        for step in 1..=1_000i64 {
            let now = ms_to_hns(10) * step; // 10 秒，每 10 ms 一次
            tl.tick(now, &mut |s| total += s.len());
            let exp = hns_to_frames(now, RATE);
            let lag = exp - tl.written_frames() as i64;
            assert!((0..=4_800).contains(&lag), "第 {step} 次：落后 {lag} 帧"); // 空闲阈值 100 ms
        }
        assert_eq!(total, tl.written_frames() as usize * 2);
        assert!(tl.stats().silence_filled_frames > 470_000);
    }

    #[test]
    fn packet_after_idle_continues_without_big_correction() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut out = Vec::new();
        for step in 1..=300i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s)); // 3 秒空闲
        }
        let before = tl.written_frames();
        let data = vec![0.5f32; 480 * 2];
        let r = run_push(&mut tl, &packet(ms_to_hns(3_000), 480, &data), &mut out);
        assert_eq!(r.dropped_frames, 0, "{r:?}");
        assert!(r.filled_frames <= 4_800, "{r:?}");
        assert_eq!(tl.written_frames() - before, 480 + r.filled_frames);
    }

    #[test]
    fn short_delivery_stall_does_not_lose_real_audio() {
        // 设备在 100 ms 内没出包，随后一次性交付这 100 ms 的数据：tick 不应抢先补静音
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        for step in 2..=11i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        assert_eq!(tl.stats().silence_filled_frames, 0);
        for k in 1..=10i64 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
        }
        let st = tl.stats();
        assert_eq!((st.silence_filled_frames, st.dropped_frames), (0, 0));
        assert_eq!(out.len(), 11 * 480 * 2);
        assert!(out.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn long_delivery_stall_trades_late_audio_for_silence_and_stays_aligned() {
        // 卡顿 300 ms：tick 先补了静音，随后交付的迟到数据里有一部分与已补的静音重叠，被丢弃。
        // 时间线始终与 QPC 一致，补的静音量与丢的数据量相抵。
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        for step in 2..=31i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        assert!(tl.stats().silence_filled_frames > 0);
        for k in 1..=99i64 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out); // 迟到的积压 + 后续数据
        }
        let st = tl.stats().clone();
        assert!(st.dropped_frames > 0, "{st:?}");
        let exp = hns_to_frames(dur(480) * 100, RATE);
        let diff = (tl.written_frames() as i64 - exp).abs();
        assert!(diff <= 1_920 + 480, "写入 {} 期望 {}", tl.written_frames(), exp);
    }

    #[test]
    fn finish_pads_exactly_to_the_end_time() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut total = 0usize;
        tl.tick(ms_to_hns(60), &mut |s| total += s.len());
        let r = tl.finish(ms_to_hns(75), &mut |s| total += s.len());
        assert_eq!(tl.written_frames() as i64, hns_to_frames(ms_to_hns(75), RATE));
        assert_eq!(total, tl.written_frames() as usize * 2);
        assert!(r.filled_frames > 0);
        // 已经超过结束时间则什么也不做
        assert_eq!(tl.finish(ms_to_hns(10), &mut |_| panic!("不应输出")), Report::default());
    }

    fn simulate_drift(ppm: f64, seconds: i64) -> (AudioTimeline, usize) {
        // 设备采样时钟比 QPC 快 `ppm`：每个 480 帧的包在 QPC 上只占 10 ms / (1 + ppm·1e-6)
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.3f32; 480 * 2];
        let mut total = 0usize;
        let step_real = 10.0 * 10_000.0 / (1.0 + ppm * 1e-6); // 100ns 单位
        let n = (seconds as f64 * 1_000.0 / (10.0 / (1.0 + ppm * 1e-6))) as i64;
        for k in 0..n {
            let t = (k as f64 * step_real).round() as Hns;
            let p = packet(t, 480, &data);
            tl.push_packet(&p, &mut |s| total += s.len());
        }
        (tl, total)
    }

    #[test]
    fn fast_device_clock_is_corrected_by_dropping() {
        let (tl, total) = simulate_drift(200.0, 600);
        let st = tl.stats();
        // 10 分钟 × 200 ppm = 120 ms 漂移，应该被修正掉，且是靠丢弃
        assert!(st.dropped_frames > 0 && st.silence_filled_frames == 0, "{st:?}");
        let exp = hns_to_frames(600 * HNS_PER_SEC, RATE);
        assert!((tl.written_frames() as i64 - exp).abs() <= 1_920 + 480, "写入 {} 期望 {}", tl.written_frames(), exp);
        assert_eq!(total, tl.written_frames() as usize * 2);
    }

    #[test]
    fn slow_device_clock_is_corrected_by_filling() {
        let (tl, _) = simulate_drift(-200.0, 600);
        let st = tl.stats();
        assert!(st.silence_filled_frames > 0 && st.dropped_frames == 0, "{st:?}");
        let exp = hns_to_frames(600 * HNS_PER_SEC, RATE);
        assert!((tl.written_frames() as i64 - exp).abs() <= 1_920 + 480);
    }

    #[test]
    fn long_stall_is_filled_in_one_second_chunks_and_flagged() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut max_chunk = 0usize;
        let mut total = 0usize;
        let r = tl.tick(6 * HNS_PER_SEC, &mut |s| {
            max_chunk = max_chunk.max(s.len());
            total += s.len();
        });
        assert!(r.large_gap);
        assert_eq!(tl.stats().large_gaps, 1);
        assert!(max_chunk <= RATE as usize * 2, "单块 {max_chunk}");
        assert_eq!(total, r.filled_frames as usize * 2);
        assert!(r.filled_frames > 5 * RATE as u64);
    }

    #[test]
    fn silent_flag_forces_zeros_even_if_buffer_has_data() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let garbage = vec![0.9f32; 480 * 2];
        let mut p = packet(0, 480, &garbage);
        p.flags.silent = true;
        let mut out = Vec::new();
        run_push(&mut tl, &p, &mut out);
        assert_eq!(out.len(), 480 * 2);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(tl.stats().silent_packets, 1);
        assert_eq!(tl.take_peak(), 0.0);
    }

    #[test]
    fn timestamp_error_falls_back_to_arrival_minus_duration() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.2f32; 480 * 2];
        let mut p = packet(ms_to_hns(5_000), 480, &data); // 设备时间戳明显有误
        p.arrival_qpc = dur(480); // 真实到达：第一个包
        p.flags.timestamp_error = true;
        let mut out = Vec::new();
        let r = run_push(&mut tl, &p, &mut out);
        assert_eq!(r, Report::default());
        assert_eq!(tl.stats().timestamp_fallbacks, 1);
        assert_eq!(out.len(), 480 * 2);
    }

    #[test]
    fn arrival_time_source_ignores_device_timestamps() {
        let mut c = cfg();
        c.time_source = TimeSource::ArrivalMinusDuration;
        let mut tl = AudioTimeline::new(c, 0);
        let data = vec![0.2f32; 480 * 2];
        let mut p = packet(ms_to_hns(5_000), 480, &data);
        p.arrival_qpc = dur(480);
        let mut out = Vec::new();
        let r = run_push(&mut tl, &p, &mut out);
        assert_eq!(r, Report::default());
        assert_eq!(tl.stats().timestamp_fallbacks, 0);
    }

    #[test]
    fn missing_device_timestamp_counts_as_fallback() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.2f32; 480 * 2];
        let mut p = packet(0, 480, &data);
        p.device_qpc = None;
        p.arrival_qpc = dur(480);
        run_push(&mut tl, &p, &mut Vec::new());
        assert_eq!(tl.stats().timestamp_fallbacks, 1);
    }

    #[test]
    fn pts_gap_and_overlap_are_counted() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        run_push(&mut tl, &packet(ms_to_hns(10) + ms_to_hns(100), 480, &data), &mut out); // 间隙 100 ms
        run_push(&mut tl, &packet(ms_to_hns(10) + ms_to_hns(100) + ms_to_hns(10) - ms_to_hns(90), 480, &data), &mut out); // 重叠 90 ms
        let st = tl.stats();
        assert_eq!(st.pts_gaps, 1);
        assert_eq!(st.pts_overlaps, 1);
    }

    #[test]
    fn peak_is_tracked_and_resets_when_taken() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut data = vec![0.1f32; 480 * 2];
        data[100] = -0.7;
        run_push(&mut tl, &packet(0, 480, &data), &mut Vec::new());
        assert!((tl.take_peak() - 0.7).abs() < 1e-6);
        assert_eq!(tl.take_peak(), 0.0);
    }

    #[test]
    fn discontinuity_flag_is_counted() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut p = packet(0, 480, &data);
        p.flags.discontinuity = true;
        run_push(&mut tl, &p, &mut Vec::new());
        assert_eq!(tl.stats().discontinuities, 1);
    }

    /// 四类补静音加起来必须等于总数，任何一次补静音都不能漏记。
    fn assert_categories_sum_up(st: &TimelineStats) {
        assert_eq!(st.silence_filled_frames, st.lead_in_frames + st.gap_filled_frames + st.latency_shift_frames + st.idle_filled_frames + st.outage_filled_frames + st.tail_frames, "{st:?}");
    }

    #[test]
    fn stream_start_fill_is_lead_in_the_first_time_and_outage_afterwards() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut out = Vec::new();
        // 打开设备花了 600 ms：精确补到启动时刻，记为首包前
        let r = tl.stream_started(ms_to_hns(600), &mut |s| out.extend_from_slice(s));
        assert_eq!(r.filled_frames, 600 * 48);
        let st = tl.stats();
        assert_eq!(st.lead_in_frames, 600 * 48);
        assert_eq!((st.fill_events, st.gap_filled_frames, st.idle_filled_frames, st.idle_episodes, st.outage_filled_frames), (0, 0, 0, 0, 0));
        // 紧接着的第一个包不再被当成空档
        let data = vec![0.5f32; 480 * 2];
        run_push(&mut tl, &packet(ms_to_hns(605), 480, &data), &mut out);
        assert_eq!(tl.stats().fill_events, 0);
        // 之后连接断了、重连成功：这段时间记为中断，而不是首包前
        let mut tl2 = AudioTimeline::new(cfg(), 0);
        tl2.stream_started(ms_to_hns(100), &mut |_| {});
        tl2.stream_started(ms_to_hns(1_100), &mut |_| {});
        let st2 = tl2.stats();
        assert_eq!((st2.lead_in_frames, st2.outage_filled_frames), (100 * 48, 1_000 * 48));
        assert_categories_sum_up(st2);
    }

    #[test]
    fn stream_started_does_nothing_when_the_timeline_is_already_ahead() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        tl.tick_outage(ms_to_hns(500), &mut |_| {});
        let before = tl.written_frames();
        let r = tl.stream_started(ms_to_hns(450), &mut |_| {});
        assert_eq!((r, tl.written_frames()), (Report::default(), before));
    }

    #[test]
    fn gap_between_packets_is_counted_as_a_gap() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        run_push(&mut tl, &packet(ms_to_hns(10) + ms_to_hns(200), 480, &data), &mut out); // 空档 200 ms
        let st = tl.stats();
        assert_eq!((st.fill_events, st.lead_in_frames, st.idle_filled_frames), (1, 0, 0));
        assert_eq!(st.gap_filled_frames, (200 - 10) * 48);
        assert_categories_sum_up(st);
    }

    #[test]
    fn consecutive_idle_ticks_are_one_episode_and_a_new_packet_starts_a_new_one() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        // 第一段空闲：2 秒内每 10 ms 一次 tick，会补很多次，但只算一段
        for step in 2..=200i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        assert_eq!(tl.stats().idle_episodes, 1);
        assert!(tl.stats().idle_filled_frames > 90_000);
        // 一个新包到达，随后再空闲：第二段
        let next = frames_to_hns(tl.written_frames() as i64, RATE);
        run_push(&mut tl, &packet(next, 480, &data), &mut out);
        for step in 1..=100i64 {
            tl.tick(next + dur(480) + ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        let st = tl.stats();
        assert_eq!(st.idle_episodes, 2, "{st:?}");
        assert_eq!((st.fill_events, st.gap_filled_frames, st.outage_filled_frames), (0, 0, 0));
        assert_categories_sum_up(st);
    }

    #[test]
    fn ticks_while_the_stream_is_down_are_outage_and_do_not_touch_idle() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut out = Vec::new();
        for step in 1..=300i64 {
            tl.tick_outage(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        let st = tl.stats();
        assert!(st.outage_filled_frames > 130_000, "{st:?}");
        assert_eq!((st.idle_filled_frames, st.idle_episodes, st.fill_events), (0, 0, 0));
        assert_categories_sum_up(st);
    }

    #[test]
    fn finish_tail_is_its_own_category_and_totals_still_add_up() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        tl.stream_started(ms_to_hns(300), &mut |s| out.extend_from_slice(s)); // 首包前 300 ms
        run_push(&mut tl, &packet(ms_to_hns(300), 480, &data), &mut out);
        tl.tick_outage(ms_to_hns(1_000), &mut |s| out.extend_from_slice(s)); // 中断
        tl.finish(ms_to_hns(1_500), &mut |s| out.extend_from_slice(s)); // 收尾
        let st = tl.stats();
        assert!(st.lead_in_frames > 0 && st.outage_filled_frames > 0 && st.tail_frames > 0, "{st:?}");
        assert_eq!((st.idle_filled_frames, st.idle_episodes), (0, 0), "收尾补齐不是空闲：{st:?}");
        assert_categories_sum_up(st);
        assert_eq!(st.frames_written, (out.len() / 2) as u64);
        assert_eq!(tl.written_frames(), 72_000); // 精确补到 1.5 s
    }

    #[test]
    fn waiting_for_the_first_packet_is_lead_in_whether_by_tick_or_by_late_timestamp() {
        // 回环一直没有声音：第一个包之前的 tick 补齐算首包前，不是空闲
        let mut tl = AudioTimeline::new(cfg(), 0);
        for step in 1..=300i64 {
            tl.tick(ms_to_hns(10) * step, &mut |_| {});
        }
        let st = tl.stats();
        assert!(st.lead_in_frames > 130_000, "{st:?}");
        assert_eq!((st.idle_filled_frames, st.idle_episodes, st.fill_events), (0, 0, 0));
        assert_categories_sum_up(st);
        // 第一个包的设备时间戳比采集开始晚 560 ms：同样是首包前，不是包间空档
        let mut late = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        run_push(&mut late, &packet(ms_to_hns(560), 480, &data), &mut Vec::new());
        let st = late.stats();
        assert_eq!((st.lead_in_frames, st.fill_events, st.gap_filled_frames), ((560 - 10) * 48, 0, 0));
    }

    #[test]
    fn a_constant_playback_latency_costs_one_lead_in_and_no_gaps() {
        // 回环：包在真实时间 t 读到，但设备时间戳（播放时刻）是 t + 560 ms
        let latency = ms_to_hns(560);
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..300i64 {
            run_push(&mut tl, &packet(dur(480) * k + latency, 480, &data), &mut out);
        }
        let st = tl.stats();
        assert_eq!((st.fill_events, st.pts_gaps, st.dropped_frames), (0, 0, 0), "{st:?}");
        assert_eq!(st.lead_in_frames, (560 - 10) * 48);
        // 在真实时间 3.0 s 停止：时间线已经写到 3.55 s，比停止时刻长一个播放延迟，收尾不应再补
        let r = tl.finish(ms_to_hns(3_000), &mut |s| out.extend_from_slice(s));
        assert_eq!(r, Report::default());
        assert!(tl.written_frames() > 3_000 * 48);
    }

    #[test]
    fn on_time_packets_with_a_growing_device_lead_are_latency_shift_not_gaps() {
        // 本机开保活流时的实测：包每 10 ms 准时到达，设备时间戳每包前进 20 ms（播放延迟每包 +10 ms），持续 56 个包
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.0f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..56i64 {
            let p = AudioPacket { device_qpc: Some(ms_to_hns(20) * (k + 1)), arrival_qpc: ms_to_hns(10) * (k + 1), frames: 480, samples: &data, flags: PacketFlags::default() };
            run_push(&mut tl, &p, &mut out);
        }
        let st = tl.stats();
        assert_eq!((st.fill_events, st.gap_filled_frames), (0, 0), "{st:?}");
        // 设备时间最终比到达时间领先 560 ms，时间线补上了这段（扣掉每次留下的 10 ms 残差和容差内的部分）
        assert!(st.latency_shift_frames > 18_000 && st.latency_shift_frames <= 560 * 48, "{st:?}");
        assert_categories_sum_up(st);
    }

    #[test]
    fn a_late_packet_after_a_stall_is_still_a_gap() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..10i64 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
        }
        // 设备停了 300 ms：下一个包到得晚，设备时间戳也跟着晚
        run_push(&mut tl, &packet(dur(480) * 10 + ms_to_hns(300), 480, &data), &mut out);
        let st = tl.stats();
        assert_eq!((st.fill_events, st.latency_shift_frames), (1, 0), "{st:?}");
        assert_eq!(st.gap_filled_frames, (300 - 10) * 48);
        assert_categories_sum_up(st);
    }

    #[test]
    fn stopping_a_healthy_stream_adds_no_idle_episode() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..100 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
        }
        // 最后一个包结束于 1.0 s，停止发生在 1.015 s
        tl.finish(ms_to_hns(1_015), &mut |s| out.extend_from_slice(s));
        let st = tl.stats();
        assert_eq!((st.tail_frames, st.idle_episodes, st.idle_filled_frames, st.fill_events), (15 * 48, 0, 0, 0));
        assert_categories_sum_up(st);
    }

    #[test]
    fn stats_from_an_older_session_json_without_the_new_fields_still_load() {
        let st: TimelineStats = serde_json::from_str(r#"{"packets": 5, "silence_filled_frames": 7}"#).unwrap();
        assert_eq!((st.packets, st.silence_filled_frames, st.lead_in_frames, st.idle_episodes), (5, 7, 0, 0));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib timeline::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/timeline.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 时间线修复：补静音与漂移修正（设计文档 5.5）。
//!
//! 每个音频数据包带一个 QPC 时间戳。已写入的采样数与“按 QPC 推算应写入的采样数”之差为 Δ：
//! - Δ 超过阈值：补 Δ 个静音采样（回环空闲、设备停摆、时钟偏慢）；
//! - Δ 低于负阈值：丢弃多余采样（时钟偏快）。
//!
//! 阈值带滞回：触发后不是修正到 0，而是留下 `settle` 的残差，避免时间戳抖动引起来回修正。
//! 没有数据包时，由调用方每 10 ms 调用一次 [`AudioTimeline::tick`]，让回环空闲期的时间线保持连续。
//!
//! 输出用回调按块交付（静音每块最多 1 秒），长时间卡顿后补静音也不会一次分配大量内存。

use crate::clock::{Hns, frames_to_hns, hns_to_frames, ms_to_hns};
use serde::{Deserialize, Serialize};

/// 交错 f32 采样的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub rate: u32,
    pub channels: u16,
}

/// 时间戳来源。每一路设备各选一种，由 S1 实测决定（设计文档 5.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeSource {
    /// 设备给出的 QPC 时间戳（`GetBuffer` 的 `qpcPosition`）。
    #[default]
    DeviceQpc,
    /// 包到达时间减去包时长。
    ArrivalMinusDuration,
}

/// WASAPI 包标志中与时间线有关的三个。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacketFlags {
    /// `AUDCLNT_BUFFERFLAGS_SILENT`：按全零处理，不读缓冲内容。
    pub silent: bool,
    /// `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`：设备时间戳不可信。
    pub timestamp_error: bool,
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`：只计数，由 Δ 补齐。
    pub discontinuity: bool,
}

/// 采集线程交给时间线的一个数据包。
#[derive(Debug, Clone, Copy)]
pub struct AudioPacket<'a> {
    /// 设备给出的该包首帧的 QPC 时间（100 ns）。设备没给时为 `None`。
    pub device_qpc: Option<Hns>,
    /// 采集线程取到该包时的 QPC 时间（100 ns）。
    pub arrival_qpc: Hns,
    pub frames: usize,
    /// 交错 f32，长度应为 `frames × channels`。`silent` 包可以传空切片。
    pub samples: &'a [f32],
    pub flags: PacketFlags,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineConfig {
    pub format: AudioFormat,
    pub time_source: TimeSource,
    /// 数据包到达时触发修正的偏差阈值（毫秒）。设计初值 40，由 S1 调定。
    pub tolerance_ms: u32,
    /// 没有数据包时（`tick`）触发补静音的阈值（毫秒）。比 `tolerance_ms` 宽，
    /// 否则设备数据只要晚到 40 ms，就会先被补上静音、迟到的真实数据再被当作重叠丢掉。初值 100。
    pub idle_tolerance_ms: u32,
    /// 修正后保留的残差（毫秒），即滞回宽度。初值 10。
    pub settle_ms: u32,
    /// 单次补静音达到该时长就记 `large_gap`（毫秒）。设计值 5000。
    pub large_gap_ms: u32,
}

impl TimelineConfig {
    pub fn new(format: AudioFormat) -> Self {
        Self { format, time_source: TimeSource::DeviceQpc, tolerance_ms: 40, idle_tolerance_ms: 100, settle_ms: 10, large_gap_ms: 5000 }
    }
}

/// 补静音的原因。分开统计，因为它们的含义完全不同：回环设备没有声音时没有包到达，空闲补齐是常态；
/// 麦克风出现空闲、或者包与包之间出现空档，才说明采集出了问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// 第一个数据包之前：打开设备的耗时，加上设备时间戳相对采集时刻的延迟
    /// （回环包的时间戳是“将在扬声器播放的时刻”，比读到它的时刻晚一个播放延迟；本机不开麦克风时约 0.56 秒）。
    LeadIn,
    /// 收到包时发现与上一个包之间有空档：包到得晚，说明设备或系统卡过。
    Gap,
    /// 包准时到达，但设备时间戳跑到了前面：播放延迟在增大（本机开保活流时开头 0.56 秒内每包 +10 ms）。
    /// 时间线仍然按设备时间对齐（补静音），但这不是异常。
    LatencyShift,
    /// 流正常、但没有包到达。
    Idle,
    /// 流已中断（设备被拔、切换默认设备、重连等待期间）。
    Outage,
    /// 收尾时补到停止时刻（最后一个包到停止之间，正常情况下不超过一个包的时长）。
    Tail,
}

/// 写入 `session.json` 诊断的计数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineStats {
    pub packets: u64,
    pub frames_written: u64,
    /// 合成静音总帧数 = `lead_in_frames` + `gap_filled_frames` + `latency_shift_frames` + `idle_filled_frames` + `outage_filled_frames` + `tail_frames`。
    pub silence_filled_frames: u64,
    pub dropped_frames: u64,
    /// 从开始采集到第一个数据包之间的静音：打开设备的耗时 + 设备时间戳相对采集时刻的延迟
    /// （回环是播放延迟）+ 回环无声时等待第一个包的时间。**不是异常**，但对热键到开始录音的延迟有参考价值。
    pub lead_in_frames: u64,
    /// 收到包时发现的包间空档，且包到得晚。**异常**：设备或系统卡顿。
    pub gap_filled_frames: u64,
    /// 包准时到达、但设备时间戳跑到了前面（播放延迟增大）而补的静音。**不是异常**。
    pub latency_shift_frames: u64,
    /// 没有包到达期间补的静音。回环设备无声时是**常态**，麦克风出现则不正常。
    pub idle_filled_frames: u64,
    /// 流中断期间补的静音。**异常**。
    pub outage_filled_frames: u64,
    /// 收尾时补到停止时刻的静音：最后一个包到停止之间，正常情况下不超过一个包的时长。
    pub tail_frames: u64,
    /// 包间空档的次数（对应 `gap_filled_frames`）。
    pub fill_events: u64,
    /// 空闲段数：连续的空闲补齐算一段，不是每次 tick 算一次。
    pub idle_episodes: u64,
    pub drop_events: u64,
    pub large_gaps: u64,
    /// 相邻两个包的时间戳之间出现的间隙 / 重叠次数（超过阈值才算）。
    pub pts_gaps: u64,
    pub pts_overlaps: u64,
    /// 设备时间戳不可用，改用“到达时间 − 包时长”的次数。
    pub timestamp_fallbacks: u64,
    pub discontinuities: u64,
    pub silent_packets: u64,
    pub max_abs_delta_frames: u64,
}

/// 一次 `push_packet` 或 `tick` 的结果，供调用方决定要不要告警。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub filled_frames: u64,
    pub dropped_frames: u64,
    /// 本次一次性补的静音达到了 `large_gap_ms`。
    pub large_gap: bool,
}

pub struct AudioTimeline {
    cfg: TimelineConfig,
    /// 该轨的时间起点：开始采集的 QPC。首包到来前的空档按补静音处理。
    anchor: Hns,
    written: u64,
    prev_end: Option<Hns>,
    /// 上一个包的到达时间，用来判断本包是否准时。
    prev_arrival: Option<Hns>,
    /// 正处在一段空闲补齐中（还没有新包到达）。
    idle_open: bool,
    /// 是否已经成功启动过流（区分“打开设备耗时”与“重连中断”）。
    started_once: bool,
    stats: TimelineStats,
    peak: f32,
    tol_frames: i64,
    idle_tol_frames: i64,
    settle_frames: i64,
    large_gap_frames: i64,
    zeros: Vec<f32>,
}

impl AudioTimeline {
    pub fn new(cfg: TimelineConfig, anchor: Hns) -> Self {
        let rate = cfg.format.rate;
        let ms = |v: u32| hns_to_frames(ms_to_hns(i64::from(v)), rate);
        Self {
            cfg,
            anchor,
            written: 0,
            prev_end: None,
            prev_arrival: None,
            idle_open: false,
            started_once: false,
            stats: TimelineStats::default(),
            peak: 0.0,
            tol_frames: ms(cfg.tolerance_ms),
            idle_tol_frames: ms(cfg.idle_tolerance_ms),
            settle_frames: ms(cfg.settle_ms),
            large_gap_frames: ms(cfg.large_gap_ms),
            zeros: Vec::new(),
        }
    }

    pub fn anchor(&self) -> Hns {
        self.anchor
    }

    pub fn written_frames(&self) -> u64 {
        self.written
    }

    pub fn stats(&self) -> &TimelineStats {
        &self.stats
    }

    /// 取走自上次调用以来输出采样的最大绝对值（健康监测每秒调用一次）。
    pub fn take_peak(&mut self) -> f32 {
        std::mem::take(&mut self.peak)
    }

    /// 处理一个数据包。需要写出的采样（可能先有补的静音，再有包内数据）通过 `emit` 按块交付。
    pub fn push_packet(&mut self, pkt: &AudioPacket<'_>, emit: &mut dyn FnMut(&[f32])) -> Report {
        let rate = self.cfg.format.rate;
        let ch = usize::from(self.cfg.format.channels);
        let dur = frames_to_hns(pkt.frames as i64, rate);

        self.stats.packets += 1;
        self.idle_open = false;
        if pkt.flags.discontinuity {
            self.stats.discontinuities += 1;
        }
        if pkt.flags.silent {
            self.stats.silent_packets += 1;
        }

        let start = self.packet_start(pkt, dur);

        let first = self.prev_end.is_none();
        let tol_hns = ms_to_hns(i64::from(self.cfg.tolerance_ms));
        // 到达间隔不超过“包时长 + 容差”算准时。包是否准时是区分“卡顿造成的空档”和“延迟增大”的依据
        let on_time = self.prev_arrival.is_none_or(|pa| pkt.arrival_qpc - pa <= dur + tol_hns);
        self.prev_arrival = Some(pkt.arrival_qpc);
        if let Some(prev_end) = self.prev_end {
            let gap = start - prev_end;
            if gap > tol_hns {
                self.stats.pts_gaps += 1;
            } else if gap < -tol_hns {
                self.stats.pts_overlaps += 1;
            }
        }
        self.prev_end = Some(start + dur);

        let delta = hns_to_frames(start - self.anchor, rate) - self.written as i64;
        self.stats.max_abs_delta_frames = self.stats.max_abs_delta_frames.max(delta.unsigned_abs());

        let mut report = Report::default();
        let mut skip = 0usize;
        if delta > self.tol_frames {
            let kind = if first {
                Fill::LeadIn
            } else if on_time {
                Fill::LatencyShift
            } else {
                Fill::Gap
            };
            report = self.fill_silence((delta - self.settle_frames) as u64, kind, emit);
        } else if delta < -self.tol_frames {
            let drop = ((-delta) - self.settle_frames).max(0) as usize;
            skip = drop.min(pkt.frames);
            report.dropped_frames = skip as u64;
            self.stats.dropped_frames += skip as u64;
            self.stats.drop_events += 1;
        }

        let keep = pkt.frames - skip;
        if keep > 0 {
            if pkt.flags.silent || pkt.samples.is_empty() {
                self.emit_zeros(keep, emit);
            } else {
                let data = &pkt.samples[skip * ch..pkt.frames * ch];
                for s in data {
                    let a = s.abs();
                    if a > self.peak {
                        self.peak = a;
                    }
                }
                emit(data);
            }
            self.written += keep as u64;
            self.stats.frames_written += keep as u64;
        }
        report
    }

    /// 每次成功启动采集流后立刻调用：把此前还没补的时间线精确补到 `now`。
    /// 第一次是打开设备的耗时，记为 `lead_in_frames`；之后（重连、切换默认设备）记为中断。
    pub fn stream_started(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        let kind = if self.started_once { Fill::Outage } else { Fill::LeadIn };
        self.started_once = true;
        let delta = hns_to_frames(now - self.anchor, self.cfg.format.rate) - self.written as i64;
        if delta > 0 {
            self.stats.max_abs_delta_frames = self.stats.max_abs_delta_frames.max(delta.unsigned_abs());
            self.fill_silence(delta as u64, kind, emit)
        } else {
            Report::default()
        }
    }

    /// 流正常、没有数据包期间由调用方每 10 ms 调用一次：按 QPC 推算，落后超过 `idle_tolerance_ms` 才补静音。
    pub fn tick(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        self.tick_as(now, Fill::Idle, emit)
    }

    /// 流已中断（重连等待期间）时的 `tick`：补的静音记为中断而不是空闲。
    pub fn tick_outage(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        self.tick_as(now, Fill::Outage, emit)
    }

    fn tick_as(&mut self, now: Hns, kind: Fill, emit: &mut dyn FnMut(&[f32])) -> Report {
        // 第一个包之前的等待都算首包前，不论是打开设备慢、设备延迟大，还是回环一直没有声音
        let kind = if kind == Fill::Idle && self.prev_end.is_none() { Fill::LeadIn } else { kind };
        let delta = hns_to_frames(now - self.anchor, self.cfg.format.rate) - self.written as i64;
        if delta > self.idle_tol_frames {
            self.stats.max_abs_delta_frames = self.stats.max_abs_delta_frames.max(delta.unsigned_abs());
            self.fill_silence((delta - self.settle_frames) as u64, kind, emit)
        } else {
            Report::default()
        }
    }

    /// 收尾时调用：把时间线精确补到 `now`（没有阈值，也不留残差），让这一轨的长度等于录制时长。
    /// 已经超过 `now` 时什么也不做。
    pub fn finish(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        let delta = hns_to_frames(now - self.anchor, self.cfg.format.rate) - self.written as i64;
        if delta > 0 { self.fill_silence(delta as u64, Fill::Tail, emit) } else { Report::default() }
    }

    fn packet_start(&mut self, pkt: &AudioPacket<'_>, dur: Hns) -> Hns {
        let from_arrival = pkt.arrival_qpc - dur;
        match self.cfg.time_source {
            TimeSource::ArrivalMinusDuration => from_arrival,
            TimeSource::DeviceQpc => match pkt.device_qpc {
                Some(t) if !pkt.flags.timestamp_error => t,
                _ => {
                    self.stats.timestamp_fallbacks += 1;
                    from_arrival
                }
            },
        }
    }

    fn fill_silence(&mut self, frames: u64, kind: Fill, emit: &mut dyn FnMut(&[f32])) -> Report {
        self.emit_zeros(frames as usize, emit);
        self.written += frames;
        self.stats.frames_written += frames;
        self.stats.silence_filled_frames += frames;
        match kind {
            Fill::LeadIn => self.stats.lead_in_frames += frames,
            Fill::Gap => {
                self.stats.gap_filled_frames += frames;
                self.stats.fill_events += 1;
            }
            Fill::Idle => {
                self.stats.idle_filled_frames += frames;
                if !self.idle_open {
                    self.idle_open = true;
                    self.stats.idle_episodes += 1;
                }
            }
            Fill::LatencyShift => self.stats.latency_shift_frames += frames,
            Fill::Outage => self.stats.outage_filled_frames += frames,
            Fill::Tail => self.stats.tail_frames += frames,
        }
        let large = frames as i64 >= self.large_gap_frames;
        if large {
            self.stats.large_gaps += 1;
        }
        Report { filled_frames: frames, dropped_frames: 0, large_gap: large }
    }

    fn emit_zeros(&mut self, mut frames: usize, emit: &mut dyn FnMut(&[f32])) {
        let ch = usize::from(self.cfg.format.channels);
        let chunk_frames = self.cfg.format.rate as usize; // 每块最多 1 秒
        if self.zeros.len() < chunk_frames.min(frames) * ch {
            self.zeros.resize(chunk_frames.min(frames) * ch, 0.0);
        }
        while frames > 0 {
            let n = frames.min(chunk_frames);
            emit(&self.zeros[..n * ch]);
            frames -= n;
        }
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib timeline::tests::
```

预期：通过，输出里有 `test result: ok. 30 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 时间线修复（补静音、漂移修正、延迟增大）"
```

---

## 任务 8：record-core：health

**目标：** 采集健康监测（设计文档 5.10）：不静默失败。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/health.rs`

**接口：**

- 依赖：`crate::clock::{HNS_PER_SEC, Hns}`
- 提供：`pub struct WatchdogConfig`；`pub enum WatchdogEvent`；`pub struct SilenceWatchdog`；`SilenceWatchdog::new`；`SilenceWatchdog::is_alerting`；`SilenceWatchdog::tick`；`pub enum HungEvent`；`pub struct HungDetector`；`HungDetector::new`；`HungDetector::progress`；`HungDetector::rearm`；`HungDetector::tick`；`pub enum LowRateEvent`；`pub struct LowRateDetector`；`LowRateDetector::new`；`LowRateDetector::tick`
- 测试：10 个：`alerts_after_five_consecutive_blind_seconds_and_only_once`、`an_opening_notification_sound_does_not_disarm_the_watchdog`、`one_audible_second_resets_the_counter`、`recovers_and_rearms_after_sound_returns`、`no_meter_means_no_report`、`quiet_room_is_not_an_alert`、`suspend_resets_the_counter`、`hung_detector_fires_once_and_recovers`、`hung_detector_is_disabled_while_suspended`、`low_rate_detector_needs_a_sustained_drop`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod health;
```

- [ ] **步骤 2：创建 `crates/record-core/src/health.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn wd() -> SilenceWatchdog {
        SilenceWatchdog::new(WatchdogConfig::default())
    }

    #[test]
    fn alerts_after_five_consecutive_blind_seconds_and_only_once() {
        // Teams 通话：播放设备电平 0.4，进程级回环采到 0
        let mut w = wd();
        for i in 1..=4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None, "第 {i} 秒");
        }
        assert_eq!(w.tick(Some(0.4), 0.0, true), Some(WatchdogEvent::Alert));
        for _ in 0..30 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None, "同一次异常只告警一次");
        }
        assert!(w.is_alerting());
    }

    #[test]
    fn an_opening_notification_sound_does_not_disarm_the_watchdog() {
        // OpenWhispr 的缺陷：开场采到一声通知音，之后整场静默都不再报告
        let mut w = wd();
        assert_eq!(w.tick(Some(0.6), 0.5, true), None); // 通知音，采到了
        let mut alert_at = None;
        for i in 1..=20 {
            if w.tick(Some(0.4), 0.0, true) == Some(WatchdogEvent::Alert) {
                alert_at = Some(i);
                break;
            }
        }
        assert_eq!(alert_at, Some(5));
    }

    #[test]
    fn one_audible_second_resets_the_counter() {
        let mut w = wd();
        for _ in 0..4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None);
        }
        assert_eq!(w.tick(Some(0.4), 0.2, true), None); // 采到了
        for _ in 0..4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None);
        }
        assert!(!w.is_alerting());
    }

    #[test]
    fn recovers_and_rearms_after_sound_returns() {
        let mut w = wd();
        for _ in 0..5 {
            w.tick(Some(0.4), 0.0, true);
        }
        assert!(w.is_alerting());
        assert_eq!(w.tick(Some(0.4), 0.3, true), Some(WatchdogEvent::Recovered));
        assert!(!w.is_alerting());
        let mut again = false;
        for _ in 0..5 {
            again |= w.tick(Some(0.4), 0.0, true) == Some(WatchdogEvent::Alert);
        }
        assert!(again, "恢复后应重新布防");
    }

    #[test]
    fn no_meter_means_no_report() {
        let mut w = wd();
        for _ in 0..60 {
            assert_eq!(w.tick(None, 0.0, true), None);
        }
    }

    #[test]
    fn quiet_room_is_not_an_alert() {
        // 系统里没声音，采集也没声音：正常
        let mut w = wd();
        for _ in 0..60 {
            assert_eq!(w.tick(Some(0.0), 0.0, true), None);
        }
    }

    #[test]
    fn suspend_resets_the_counter() {
        let mut w = wd();
        for _ in 0..4 {
            w.tick(Some(0.4), 0.0, true);
        }
        assert_eq!(w.tick(Some(0.4), 0.0, false), None); // 锁屏 / 睡眠 / 显示器关闭
        for _ in 0..4 {
            assert_eq!(w.tick(Some(0.4), 0.0, true), None);
        }
        assert_eq!(w.tick(Some(0.4), 0.0, true), Some(WatchdogEvent::Alert));
    }

    #[test]
    fn hung_detector_fires_once_and_recovers() {
        let mut d = HungDetector::new(10, 0);
        for s in 1..10 {
            assert_eq!(d.tick(s * HNS_PER_SEC, true), None);
        }
        assert_eq!(d.tick(10 * HNS_PER_SEC, true), Some(HungEvent::Hung { idle_secs: 10 }));
        assert_eq!(d.tick(11 * HNS_PER_SEC, true), None);
        assert_eq!(d.progress(12 * HNS_PER_SEC), Some(HungEvent::Recovered));
        assert_eq!(d.progress(13 * HNS_PER_SEC), None);
    }

    #[test]
    fn hung_detector_is_disabled_while_suspended() {
        let mut d = HungDetector::new(10, 0);
        // 睡眠 1 小时，期间 active = false
        for s in 1..=3_600 {
            assert_eq!(d.tick(s * HNS_PER_SEC, false), None);
        }
        // 唤醒后从最后一次“不活动”的时刻重新计时，不会立刻误报
        assert_eq!(d.tick(3_601 * HNS_PER_SEC, true), None);
        assert_eq!(d.tick(3_609 * HNS_PER_SEC, true), None);
        assert!(matches!(d.tick(3_610 * HNS_PER_SEC, true), Some(HungEvent::Hung { .. })));
    }

    #[test]
    fn low_rate_detector_needs_a_sustained_drop() {
        let mut d = LowRateDetector::new(0.8, 10);
        for _ in 0..9 {
            assert_eq!(d.tick(7, 15, true), None);
        }
        assert_eq!(d.tick(15, 15, true), None); // 恢复一秒，计数清零
        for _ in 0..9 {
            assert_eq!(d.tick(7, 15, true), None);
        }
        assert!(matches!(d.tick(7, 15, true), Some(LowRateEvent::Low { .. })));
        assert_eq!(d.tick(7, 15, true), None);
        assert_eq!(d.tick(15, 15, true), Some(LowRateEvent::Recovered));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib health::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/health.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 采集健康监测（设计文档 5.10）：不静默失败。
//!
//! 三个探测器都是纯状态机，由调用方每秒喂一次数据，返回要不要告警。
//! 睡眠、锁屏、显示器关闭期间，调用方传 `active = false`，所有基于超时的判定停用，
//! 恢复后重新布防，避免唤醒后误报（夸克的看门狗也是这样处理的）。

use crate::clock::{HNS_PER_SEC, Hns};

// ---------------------------------------------------------------------------
// 静默失效监测：流是通的，写进去的却全是静音
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WatchdogConfig {
    /// 播放设备电平表峰值高于它，记为“系统里有声”。
    pub meter_threshold: f32,
    /// 本轨这一秒输出的峰值高于它，记为“采到了声音”（约 −60 dBFS）。
    pub captured_threshold: f32,
    /// 连续多少秒“有声而没采到”才告警。
    pub confirm_ticks: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self { meter_threshold: 0.01, captured_threshold: 32.0 / 32768.0, confirm_ticks: 5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogEvent {
    /// 播放设备有声，本轨却连续静音。
    Alert,
    /// 告警之后，本轨又采到了声音。
    Recovered,
}

#[derive(Debug, Clone)]
pub struct SilenceWatchdog {
    cfg: WatchdogConfig,
    mismatch: u32,
    alerted: bool,
}

impl SilenceWatchdog {
    pub fn new(cfg: WatchdogConfig) -> Self {
        Self { cfg, mismatch: 0, alerted: false }
    }

    pub fn is_alerting(&self) -> bool {
        self.alerted
    }

    /// 每秒调用一次。
    /// - `meter_peak`：所有活动播放设备电平表的最大峰值；取不到电平表时为 `None`，此时不报告也不猜。
    /// - `captured_peak`：本轨这一秒内输出的峰值。
    /// - `active`：睡眠、锁屏、显示器关闭期间为 `false`。
    ///
    /// 只要有一秒采到声音，计数就清零。不能因为“已经采到过一次声音”就永久解除：
    /// OpenWhispr 的实现一度栽在这里，开场的一声通知音让整场的检查失效。
    pub fn tick(&mut self, meter_peak: Option<f32>, captured_peak: f32, active: bool) -> Option<WatchdogEvent> {
        if !active {
            self.mismatch = 0;
            return None;
        }
        if captured_peak > self.cfg.captured_threshold {
            self.mismatch = 0;
            if self.alerted {
                self.alerted = false;
                return Some(WatchdogEvent::Recovered);
            }
            return None;
        }
        let Some(meter) = meter_peak else {
            self.mismatch = 0;
            return None;
        };
        if meter > self.cfg.meter_threshold {
            self.mismatch += 1;
            if self.mismatch >= self.cfg.confirm_ticks && !self.alerted {
                self.alerted = true;
                return Some(WatchdogEvent::Alert);
            }
        } else {
            self.mismatch = 0;
        }
        None
    }
}

// ---------------------------------------------------------------------------
// 设备挂起：连续 N 秒没有任何包或帧
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HungEvent {
    Hung { idle_secs: u32 },
    Recovered,
}

/// 只用于“正常情况下一直有数据”的流：麦克风、视频。回环设备在没有声音播放时本来就不出包，
/// 不能用它判挂起；回环靠补静音与 `large_gap` 发现线程被饿死。
#[derive(Debug, Clone)]
pub struct HungDetector {
    limit: Hns,
    last_progress: Hns,
    alerted: bool,
}

impl HungDetector {
    pub fn new(limit_secs: u32, now: Hns) -> Self {
        Self { limit: i64::from(limit_secs) * HNS_PER_SEC, last_progress: now, alerted: false }
    }

    /// 收到包或帧时调用。
    pub fn progress(&mut self, now: Hns) -> Option<HungEvent> {
        self.last_progress = now;
        if self.alerted {
            self.alerted = false;
            return Some(HungEvent::Recovered);
        }
        None
    }

    /// 睡眠、锁屏恢复后调用，重新计时。
    pub fn rearm(&mut self, now: Hns) {
        self.last_progress = now;
    }

    /// 每秒调用一次。
    pub fn tick(&mut self, now: Hns, active: bool) -> Option<HungEvent> {
        if !active {
            self.last_progress = now;
            return None;
        }
        let idle = now - self.last_progress;
        if idle >= self.limit && !self.alerted {
            self.alerted = true;
            return Some(HungEvent::Hung { idle_secs: (idle / HNS_PER_SEC) as u32 });
        }
        None
    }
}

// ---------------------------------------------------------------------------
// 持续低帧率
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LowRateEvent {
    Low { ratio: f32 },
    Recovered,
}

#[derive(Debug, Clone)]
pub struct LowRateDetector {
    min_ratio: f32,
    window: u32,
    below: u32,
    alerted: bool,
}

impl LowRateDetector {
    /// 默认：连续 10 秒低于设定帧率的 80%。
    pub fn new(min_ratio: f32, window_secs: u32) -> Self {
        Self { min_ratio, window: window_secs, below: 0, alerted: false }
    }

    /// 每秒调用一次。`achieved` 是这一秒按时完成的帧数（不含追帧补出来的重复帧），`expected` 是设定帧率。
    pub fn tick(&mut self, achieved: u32, expected: u32, active: bool) -> Option<LowRateEvent> {
        if !active || expected == 0 {
            self.below = 0;
            return None;
        }
        let ratio = achieved as f32 / expected as f32;
        if ratio < self.min_ratio {
            self.below += 1;
            if self.below >= self.window && !self.alerted {
                self.alerted = true;
                return Some(LowRateEvent::Low { ratio });
            }
            return None;
        }
        self.below = 0;
        if self.alerted {
            self.alerted = false;
            return Some(LowRateEvent::Recovered);
        }
        None
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib health::tests::
```

预期：通过，输出里有 `test result: ok. 10 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 采集健康监测（端点电平对照等探测器）"
```

---

## 任务 9：record-core：pacer

**目标：** 画面的恒定帧率节拍与按原因分类的帧统计（设计文档 5.2、5.9）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/pacer.rs`

**接口：**

- 依赖：`crate::clock::{HNS_PER_SEC, Hns}`
- 提供：`pub struct FramePacer`；`FramePacer::new`；`FramePacer::fps`；`FramePacer::next_index`；`FramePacer::time_of`；`FramePacer::due`；`pub enum RepeatReason`；`pub struct FrameStats`；`FrameStats::record_repeat`；`FrameStats::total_emitted`
- 测试：7 个：`first_frame_is_due_immediately`、`frames_are_due_at_their_scheduled_times`、`a_late_caller_gets_all_missed_frames_at_once`、`exactly_fps_frames_per_second_over_a_long_run`、`time_of_is_the_exact_inverse_of_due`、`a_clock_before_t0_yields_nothing`、`frame_stats_count_every_frame_exactly_once`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod pacer;
```

- [ ] **步骤 2：创建 `crates/record-core/src/pacer.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ms_to_hns;

    #[test]
    fn first_frame_is_due_immediately() {
        let mut p = FramePacer::new(15, 1_000);
        assert_eq!(p.due(1_000), 0..1);
        assert_eq!(p.due(1_000), 1..1, "同一时刻不会重复交付");
    }

    #[test]
    fn frames_are_due_at_their_scheduled_times() {
        let mut p = FramePacer::new(15, 0);
        assert_eq!(p.due(0), 0..1);
        assert_eq!(p.due(ms_to_hns(60)), 1..1); // 第 1 帧在 66.67 ms
        assert_eq!(p.due(ms_to_hns(67)), 1..2);
        assert_eq!(p.due(ms_to_hns(133)), 2..2); // 第 2 帧在 133.33 ms
        assert_eq!(p.due(ms_to_hns(134)), 2..3);
    }

    #[test]
    fn a_late_caller_gets_all_missed_frames_at_once() {
        let mut p = FramePacer::new(15, 0);
        assert_eq!(p.due(0), 0..1);
        // 采集循环卡了 1 秒
        let r = p.due(ms_to_hns(1_000));
        assert_eq!(r, 1..16);
        assert_eq!(r.end - r.start, 15);
    }

    #[test]
    fn exactly_fps_frames_per_second_over_a_long_run() {
        let mut p = FramePacer::new(15, 0);
        let mut count = 0u64;
        // 每 7 ms 调用一次，跑 60 秒
        let mut now = 0;
        while now <= ms_to_hns(60_000) {
            count += p.due(now).count() as u64;
            now += ms_to_hns(7);
        }
        // 最后一次调用在 59.997 秒，此时第 899 帧已到期，第 900 帧（60.0 秒）还没有
        assert_eq!(count, 900);
    }

    #[test]
    fn time_of_is_the_exact_inverse_of_due() {
        for fps in [10, 15, 30] {
            let p = FramePacer::new(fps, 5_000);
            assert_eq!(p.time_of(0), 5_000);
            assert_eq!(p.time_of(u64::from(fps)), 5_000 + 10_000_000);
            for k in 1..200u64 {
                let mut before = FramePacer::new(fps, 5_000);
                assert_eq!(before.due(p.time_of(k) - 1).end, k, "fps {fps} 第 {k} 帧到期前一刻");
                let mut at = FramePacer::new(fps, 5_000);
                assert_eq!(at.due(p.time_of(k)).end, k + 1, "fps {fps} 第 {k} 帧到期时");
            }
        }
    }

    #[test]
    fn a_clock_before_t0_yields_nothing() {
        let mut p = FramePacer::new(30, 10_000);
        assert_eq!(p.due(5_000), 0..0);
    }

    #[test]
    fn frame_stats_count_every_frame_exactly_once() {
        let mut s = FrameStats::default();
        s.fresh += 3;
        s.record_repeat(RepeatReason::NoChange);
        s.record_repeat(RepeatReason::SecureDesktop);
        s.record_repeat(RepeatReason::DuplicationRebuild);
        s.record_repeat(RepeatReason::DuplicationFailed);
        s.catch_up += 2;
        assert_eq!(s.total_emitted(), 9);
        assert_eq!(s.repeat_secure_desktop, 1);
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib pacer::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/pacer.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 画面的恒定帧率节拍与按原因分类的帧统计（设计文档 5.2、5.9）。
//!
//! 帧序号 = floor((now − t0) × fps)。采集循环落后时，一次会有多帧同时到期，
//! 调用方用最近一帧补齐，保证输出文件的时间线与 QPC 一致。

use crate::clock::{HNS_PER_SEC, Hns};
use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct FramePacer {
    fps: u32,
    t0: Hns,
    next: u64,
}

impl FramePacer {
    pub fn new(fps: u32, t0: Hns) -> Self {
        assert!(fps > 0, "帧率必须为正数");
        Self { fps, t0, next: 0 }
    }

    pub fn fps(&self) -> u32 {
        self.fps
    }

    /// 下一个还没有交付的帧序号。
    pub fn next_index(&self) -> u64 {
        self.next
    }

    /// 序号为 `index` 的帧最早到期的时刻（向上取整，与 [`FramePacer::due`] 互逆）。
    pub fn time_of(&self, index: u64) -> Hns {
        let fps = i128::from(self.fps);
        self.t0 + ((i128::from(index) * i128::from(HNS_PER_SEC) + fps - 1) / fps) as i64
    }

    /// 到 `now` 为止应当已交付、但还没交付的帧序号。调用后这些帧视为已交付。
    pub fn due(&mut self, now: Hns) -> Range<u64> {
        if now < self.t0 {
            return self.next..self.next;
        }
        let elapsed = i128::from(now - self.t0);
        let last = (elapsed * i128::from(self.fps) / i128::from(HNS_PER_SEC)) as u64;
        let end = last + 1;
        let start = self.next.min(end);
        self.next = end.max(self.next);
        start..end
    }
}

/// 重复上一帧的原因。分原因计数，是为了出问题时能直接看出是哪一环（参考 Loom 的做法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatReason {
    /// 屏幕没有变化（`AcquireNextFrame` 超时），正常情况。
    NoChange,
    /// UAC、锁屏等安全桌面，系统不允许抓取。
    SecureDesktop,
    /// 复制对象失效，正在重建。
    DuplicationRebuild,
    /// 复制对象出错，没有重建成功。
    DuplicationFailed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameStats {
    pub fresh: u64,
    pub repeat_no_change: u64,
    pub repeat_secure_desktop: u64,
    pub repeat_duplication_rebuild: u64,
    pub repeat_duplication_failed: u64,
    /// 采集循环落后，用最近一帧补出来的帧。
    pub catch_up: u64,
    pub dropped_encoder: u64,
    pub dropped_error: u64,
    pub dropped_queue: u64,
    pub dropped_resync: u64,
}

impl FrameStats {
    pub fn record_repeat(&mut self, reason: RepeatReason) {
        match reason {
            RepeatReason::NoChange => self.repeat_no_change += 1,
            RepeatReason::SecureDesktop => self.repeat_secure_desktop += 1,
            RepeatReason::DuplicationRebuild => self.repeat_duplication_rebuild += 1,
            RepeatReason::DuplicationFailed => self.repeat_duplication_failed += 1,
        }
    }

    /// 所有帧的总数：每个到期的帧恰好属于其中一类。
    pub fn total_emitted(&self) -> u64 {
        self.fresh + self.repeat_no_change + self.repeat_secure_desktop + self.repeat_duplication_rebuild + self.repeat_duplication_failed + self.catch_up
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib pacer::tests::
```

预期：通过，输出里有 `test result: ok. 7 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 画面恒定帧率节拍与按原因分类的统计"
```

---

## 任务 10：record-core：testpattern

**目标：** 画面自检用的测试图案：四个已知颜色的分区，加一条随时间匀速移动的亮条。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/testpattern.rs`

**接口：**

- 依赖：`crate::clock::Hns`、`crate::geometry::Rect`
- 提供：`pub const PERIOD_HNS: Hns`；`pub const QUADRANT_RGB: [[u8; 3]; 4]`；`pub const STRIP_RGB: [u8; 3]`；`pub const BAR_RGB: [u8; 3]`；`pub fn strip_height(h: u32) -> u32`；`pub fn quadrants(w: u32, h: u32) -> [Rect; 4]`；`pub fn strip(w: u32, h: u32) -> Rect`；`pub fn bar_width(w: u32) -> u32`；`pub fn phase_at(t: Hns, t_ref: Hns) -> f64`；`pub fn bar_left(phase: f64, w: u32) -> u32`；`pub fn phase_of_bar(left: f64, w: u32) -> f64`；`pub fn time_of_phase(phase: f64, t_ref: Hns, near: Hns) -> Hns`；`pub fn expected_yuv(rgb: [u8; 3]) -> [f64; 3]`
- 测试：7 个：`quadrants_and_strip_tile_the_window_without_overlap`、`the_bar_sweeps_from_left_to_right_within_the_window`、`phase_wraps_every_period_and_handles_times_before_the_reference`、`the_time_is_recovered_from_the_bar_position_within_a_few_pixels`、`the_recovered_time_picks_the_period_closest_to_the_frame_time`、`expected_colors_match_known_bt709_limited_range_values`、`the_four_quadrant_colors_are_far_apart_in_yuv`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod testpattern;
```

- [ ] **步骤 2：创建 `crates/record-core/src/testpattern.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ms_to_hns;

    #[test]
    fn quadrants_and_strip_tile_the_window_without_overlap() {
        for (w, h) in [(960u32, 540u32), (801, 451), (64, 64), (1920, 1080)] {
            let mut covered = vec![0u8; (w * h) as usize];
            for r in quadrants(w, h).into_iter().chain([strip(w, h)]) {
                for y in r.y..r.bottom() {
                    for x in r.x..r.right() {
                        covered[(y * w + x) as usize] += 1;
                    }
                }
            }
            assert!(covered.iter().all(|&c| c == 1), "{w}×{h} 有像素没覆盖或重复覆盖");
        }
    }

    #[test]
    fn the_bar_sweeps_from_left_to_right_within_the_window() {
        let w = 960;
        assert_eq!(bar_left(0.0, w), 0);
        assert_eq!(bar_left(1.0, w), w - bar_width(w));
        let mut prev = 0;
        for i in 0..=100 {
            let x = bar_left(f64::from(i) / 100.0, w);
            assert!(x >= prev && x + bar_width(w) <= w);
            prev = x;
        }
    }

    #[test]
    fn phase_wraps_every_period_and_handles_times_before_the_reference() {
        let t_ref = 5_000_000;
        assert_eq!(phase_at(t_ref, t_ref), 0.0);
        assert!((phase_at(t_ref + PERIOD_HNS / 4, t_ref) - 0.25).abs() < 1e-12);
        assert!((phase_at(t_ref + PERIOD_HNS * 3 + PERIOD_HNS / 2, t_ref) - 0.5).abs() < 1e-12);
        assert!((phase_at(t_ref - PERIOD_HNS / 4, t_ref) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn the_time_is_recovered_from_the_bar_position_within_a_few_pixels() {
        let (w, t_ref) = (960, 123_456_789);
        for ms in (0..20_000).step_by(37) {
            let drawn = t_ref + ms_to_hns(ms);
            let left = f64::from(bar_left(phase_at(drawn, t_ref), w));
            // 这一帧的名义时刻比绘制时刻晚 30 ms
            let near = drawn + ms_to_hns(30);
            let back = time_of_phase(phase_of_bar(left, w), t_ref, near);
            let err_ms = (back - drawn) / 10_000;
            // 一个像素约 4 ms，取整再加余量
            assert!(err_ms.abs() <= 6, "第 {ms} ms：误差 {err_ms} ms");
        }
    }

    #[test]
    fn the_recovered_time_picks_the_period_closest_to_the_frame_time() {
        let t_ref = 0;
        let near = PERIOD_HNS * 7 + ms_to_hns(100);
        let t = time_of_phase(phase_at(near - ms_to_hns(50), t_ref), t_ref, near);
        assert_eq!((near - t) / 10_000, 50);
    }

    #[test]
    fn expected_colors_match_known_bt709_limited_range_values() {
        let near = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.6);
        assert!(near(expected_yuv([0, 0, 0]), [16.0, 128.0, 128.0]));
        assert!(near(expected_yuv([255, 255, 255]), [235.0, 128.0, 128.0]));
        // 纯红 / 纯绿 / 纯蓝的 BT.709 有限范围值（标准表）
        assert!(near(expected_yuv([255, 0, 0]), [63.0, 102.0, 240.0]));
        assert!(near(expected_yuv([0, 255, 0]), [173.0, 42.0, 26.0]));
        assert!(near(expected_yuv([0, 0, 255]), [32.0, 240.0, 118.0]));
    }

    #[test]
    fn the_four_quadrant_colors_are_far_apart_in_yuv() {
        let yuv: Vec<[f64; 3]> = QUADRANT_RGB.iter().map(|&c| expected_yuv(c)).collect();
        for i in 0..4 {
            for j in i + 1..4 {
                let d = yuv[i].iter().zip(yuv[j]).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
                assert!(d > 30.0, "分区 {i} 与 {j} 在 YUV 里只差 {d}");
            }
        }
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib testpattern::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/testpattern.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 画面自检用的测试图案：四个已知颜色的分区，加一条随时间匀速移动的亮条。
//!
//! 测试窗口按它绘制，分析器按它在采集到的 NV12 帧里取样：颜色验证 GPU 转换路径（色彩矩阵、范围、
//! UV 平面布局），亮条的位置反推出这一帧内容的绘制时刻，用来测“画面采集的陈旧程度”。
//! 纯计算，不依赖任何系统接口。

use crate::clock::Hns;
use crate::geometry::Rect;

/// 亮条走完一个来回（从最左到最右再瞬间回到最左）的时间：4 秒。
pub const PERIOD_HNS: Hns = 40_000_000;

/// 左上、右上、左下、右下四个分区的颜色（sRGB）。选的是饱和度高、彼此远离的颜色，
/// 色彩矩阵用错（BT.601 / BT.709）或范围用错（全范围 / 有限范围）都会明显偏离。
pub const QUADRANT_RGB: [[u8; 3]; 4] = [[230, 30, 30], [30, 200, 60], [30, 60, 230], [240, 240, 240]];
pub const STRIP_RGB: [u8; 3] = [24, 24, 24];
pub const BAR_RGB: [u8; 3] = [255, 255, 255];

/// 底部亮条区域的高度：窗口高度的 1/8，至少 8 像素。
pub fn strip_height(h: u32) -> u32 {
    (h / 8).max(8).min(h / 2)
}

/// 四个颜色分区，顺序同 [`QUADRANT_RGB`]，坐标相对窗口左上角。
pub fn quadrants(w: u32, h: u32) -> [Rect; 4] {
    let area_h = h - strip_height(h);
    let (hw, hh) = (w / 2, area_h / 2);
    [Rect { x: 0, y: 0, w: hw, h: hh }, Rect { x: hw, y: 0, w: w - hw, h: hh }, Rect { x: 0, y: hh, w: hw, h: area_h - hh }, Rect { x: hw, y: hh, w: w - hw, h: area_h - hh }]
}

pub fn strip(w: u32, h: u32) -> Rect {
    let sh = strip_height(h);
    Rect { x: 0, y: h - sh, w, h: sh }
}

/// 亮条宽度：窗口宽度的 1/50，至少 8 像素。
pub fn bar_width(w: u32) -> u32 {
    (w / 50).max(8)
}

/// 时刻 `t` 的相位（0..1）：自 `t_ref` 起每 [`PERIOD_HNS`] 一圈。
pub fn phase_at(t: Hns, t_ref: Hns) -> f64 {
    (t - t_ref).rem_euclid(PERIOD_HNS) as f64 / PERIOD_HNS as f64
}

/// 相位对应的亮条左边缘（窗口坐标）。
pub fn bar_left(phase: f64, w: u32) -> u32 {
    let travel = w.saturating_sub(bar_width(w));
    ((phase * f64::from(travel)).floor() as u32).min(travel)
}

/// 由亮条左边缘反推相位。
pub fn phase_of_bar(left: f64, w: u32) -> f64 {
    let travel = f64::from(w.saturating_sub(bar_width(w))).max(1.0);
    (left / travel).clamp(0.0, 1.0)
}

/// 由相位还原时刻：在 `t_ref + (k + phase) × PERIOD` 里取离 `near` 最近的一个。
/// `near` 是这一帧的名义时刻，内容的实际绘制时刻总在它附近（相差远小于半个周期）。
pub fn time_of_phase(phase: f64, t_ref: Hns, near: Hns) -> Hns {
    let k = ((near - t_ref) as f64 / PERIOD_HNS as f64 - phase).round();
    t_ref + ((k + phase) * PERIOD_HNS as f64).round() as Hns
}

/// sRGB（0..255）对应的 BT.709 有限范围 YUV（Y 16..235，U/V 16..240）。
pub fn expected_yuv(rgb: [u8; 3]) -> [f64; 3] {
    let (r, g, b) = (f64::from(rgb[0]) / 255.0, f64::from(rgb[1]) / 255.0, f64::from(rgb[2]) / 255.0);
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let u = (b - y) / 1.8556;
    let v = (r - y) / 1.5748;
    [16.0 + 219.0 * y, 128.0 + 224.0 * u, 128.0 + 224.0 * v]
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib testpattern::tests::
```

预期：通过，输出里有 `test result: ok. 7 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 画面自检用的测试图案"
```

---

## 任务 11：record-core：config

**目标：** `config.toml` 的数据模型、校验与读写（设计文档 16.1）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/config.rs`

**接口：**

- 依赖：`crate::hotkey::Hotkey`、`crate::timeline::{AudioFormat, TimeSource, TimelineConfig}`
- 提供：`pub enum ConfigError`；`pub struct ConfigIssue`；`pub struct Config`；`pub struct Hotkeys`；`pub enum Encoder`；`pub struct VideoConfig`；`pub enum MonitorSel`；`pub enum MicRole`；`pub enum SystemCapture`；`pub struct AudioConfig`；`pub struct TimelineSettings`；`pub struct HealthSettings`；`pub enum NotifyMode`；`pub struct NotifyConfig`；`pub struct Limits`；`pub struct HotkeySet`；`Config::parse`；`Config::load`；`Config::save`；`Config::hotkey_set`；`Config::timeline_config`；`Config::validate`
- 测试：13 个：`default_config_is_valid_and_matches_the_spec`、`default_config_round_trips_through_toml`、`partial_files_fill_in_defaults`、`unknown_keys_survive_a_round_trip`、`monitor_accepts_name_or_index`、`encoder_names_match_ffmpeg`、`validation_reports_every_problem`、`duplicate_hotkeys_are_rejected`、`hotkey_set_parses_the_defaults`、`missing_file_loads_defaults` 等

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod config;
```

- [ ] **步骤 2：创建 `crates/record-core/src/config.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid_and_matches_the_spec() {
        let c = Config::default();
        assert!(c.validate().is_empty(), "{:?}", c.validate());
        assert_eq!((c.hotkeys.start.as_str(), c.hotkeys.stop.as_str(), c.hotkeys.quit.as_str()), ("Ctrl+Alt+R", "Ctrl+Alt+S", "Ctrl+Alt+Q"));
        assert_eq!((c.video.fps, c.video.max_long_edge, c.video.max_short_edge), (15, 1920, 1080));
        assert_eq!(c.video.encoder, Encoder::Auto);
        assert_eq!(c.monitor, MonitorSel::Named("foreground".into()));
        assert_eq!(c.audio.mic_role, MicRole::Communications);
        assert_eq!(c.health.silent_confirm_s, 5);
        assert_eq!(c.notify.mode, NotifyMode::Quiet);
        assert_eq!((c.limits.max_hours, c.limits.min_free_gb), (6.0, 5.0));
        assert!(!c.autostart);
    }

    #[test]
    fn default_config_round_trips_through_toml() {
        let c = Config::default();
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(!text.contains("output_dir"), "未设置的保存目录不应写出：\n{text}");
        assert_eq!(Config::parse(&text, "mem").unwrap(), c);
    }

    #[test]
    fn partial_files_fill_in_defaults() {
        let c = Config::parse("[video]\nfps = 30\n\n[limits]\nmax_hours = 2\n", "mem").unwrap();
        assert_eq!(c.video.fps, 30);
        assert_eq!(c.video.max_long_edge, 1920);
        assert_eq!(c.limits.max_hours, 2.0);
        assert_eq!(c.limits.min_free_gb, 5.0);
        assert_eq!(c.hotkeys, Hotkeys::default());
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let text = "autostart = true\nfuture_flag = 7\n\n[speech]\nauto_process = true\ndiarize_mic = false\n";
        let c = Config::parse(text, "mem").unwrap();
        assert!(c.autostart);
        let out = toml::to_string_pretty(&c).unwrap();
        let again = Config::parse(&out, "mem").unwrap();
        assert_eq!(again.extra.get("future_flag"), Some(&toml::Value::Integer(7)));
        let speech = again.extra.get("speech").and_then(|v| v.as_table()).expect("speech 表应保留");
        assert_eq!(speech.get("auto_process"), Some(&toml::Value::Boolean(true)));
    }

    #[test]
    fn monitor_accepts_name_or_index() {
        assert_eq!(Config::parse("monitor = \"foreground\"", "m").unwrap().monitor, MonitorSel::Named("foreground".into()));
        assert_eq!(Config::parse("monitor = 2", "m").unwrap().monitor, MonitorSel::Index(2));
        let bad = Config::parse("monitor = \"left\"", "m").unwrap();
        assert!(bad.validate().iter().any(|i| i.key == "monitor"));
        let zero = Config::parse("monitor = 0", "m").unwrap();
        assert!(zero.validate().iter().any(|i| i.key == "monitor"));
    }

    #[test]
    fn encoder_names_match_ffmpeg() {
        let c = Config::parse("[video]\nencoder = \"h264_nvenc\"\n", "m").unwrap();
        assert_eq!(c.video.encoder, Encoder::Nvenc);
        assert!(Config::parse("[video]\nencoder = \"x264\"\n", "m").is_err());
        let text = toml::to_string_pretty(&Config { video: VideoConfig { encoder: Encoder::Software, ..Default::default() }, ..Default::default() }).unwrap();
        assert!(text.contains("encoder = \"libopenh264\""), "{text}");
    }

    #[test]
    fn validation_reports_every_problem() {
        let text = "[hotkeys]\nstart = \"R\"\n\n[video]\nfps = 24\n\n[limits]\nmax_hours = 0\nmin_free_gb = 0.1\n\n[timeline]\nsettle_ms = 80\n";
        let keys: Vec<_> = Config::parse(text, "m").unwrap().validate().into_iter().map(|i| i.key).collect();
        for k in ["hotkeys", "video.fps", "limits.max_hours", "limits.min_free_gb", "timeline.settle_ms"] {
            assert!(keys.contains(&k), "缺少 {k}：{keys:?}");
        }
    }

    #[test]
    fn duplicate_hotkeys_are_rejected() {
        let c = Config::parse("[hotkeys]\nstop = \"ctrl+alt+r\"\n", "m").unwrap();
        let err = c.hotkey_set().unwrap_err();
        assert!(err.contains("不能相同"), "{err}");
    }

    #[test]
    fn hotkey_set_parses_the_defaults() {
        let s = Config::default().hotkey_set().unwrap();
        assert_eq!((s.start.to_string(), s.stop.to_string(), s.quit.to_string()), ("Ctrl+Alt+R".into(), "Ctrl+Alt+S".into(), "Ctrl+Alt+Q".into()));
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(&dir.path().join("config.toml")).unwrap(), Config::default());
    }

    #[test]
    fn broken_file_is_an_error_and_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        fs::write(&p, "video = [oops").unwrap();
        let err = Config::load(&p).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(err.to_string().contains("config.toml"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "video = [oops");
    }

    #[test]
    fn save_then_load_is_lossless_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("config.toml");
        let mut c = Config { output_dir: Some("E:\\Records".into()), ..Config::default() };
        c.video.fps = 30;
        c.audio.system_capture = SystemCapture::AllEndpoints;
        c.save(&p).unwrap();
        assert_eq!(Config::load(&p).unwrap(), c);
        let names: Vec<_> = fs::read_dir(p.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, vec!["config.toml".to_string()]);
    }

    #[test]
    fn timeline_settings_flow_into_the_timeline_config() {
        let mut c = Config::default();
        c.timeline.tolerance_ms = 30;
        let t = c.timeline_config(AudioFormat { rate: 48_000, channels: 2 }, TimeSource::DeviceQpc);
        assert_eq!((t.tolerance_ms, t.idle_tolerance_ms, t.settle_ms, t.large_gap_ms), (30, 100, 10, 5_000));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib config::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/config.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! `config.toml` 的数据模型、校验与读写（设计文档 16.1）。
//!
//! - 缺省的键用默认值；文件里不认识的键原样保留，升级或降级后不丢用户配置。
//! - 格式有误时返回错误，**不覆盖**原文件；由外壳提示用户，并以默认值运行。
//! - 校验返回全部问题而不是第一个，设置界面可以一次显示完。

use crate::hotkey::Hotkey;
use crate::timeline::{AudioFormat, TimeSource, TimelineConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("读取配置文件失败（{path}）：{source}")]
    Read { path: String, source: std::io::Error },
    #[error("配置文件格式有误（{path}）：{message}")]
    Parse { path: String, message: String },
    #[error("写入配置文件失败（{path}）：{source}")]
    Write { path: String, source: std::io::Error },
    #[error("配置序列化失败：{0}")]
    Serialize(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub key: &'static str,
    pub message: String,
}

fn issue(key: &'static str, message: impl Into<String>) -> ConfigIssue {
    ConfigIssue { key, message: message.into() }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 保存目录；没设置时用 `Paths` 解析出的默认目录（视频库下的 Record）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    pub hotkeys: Hotkeys,
    pub video: VideoConfig,
    pub monitor: MonitorSel,
    pub audio: AudioConfig,
    pub timeline: TimelineSettings,
    pub health: HealthSettings,
    pub notify: NotifyConfig,
    pub limits: Limits,
    pub autostart: bool,
    /// 不认识的键，原样保留（子项目 B、C 的 `[speech]`、`[llm]` 等）。
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub start: String,
    pub stop: String,
    pub quit: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self { start: "Ctrl+Alt+R".into(), stop: "Ctrl+Alt+S".into(), quit: "Ctrl+Alt+Q".into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoder {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "h264_nvenc")]
    Nvenc,
    #[serde(rename = "h264_qsv")]
    Qsv,
    #[serde(rename = "h264_amf")]
    Amf,
    /// 软件编码，只能由用户显式选择，不会悄悄降级。
    #[serde(rename = "libopenh264")]
    Software,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub fps: u32,
    pub max_long_edge: u32,
    pub max_short_edge: u32,
    pub encoder: Encoder,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self { fps: 15, max_long_edge: 1920, max_short_edge: 1080, encoder: Encoder::Auto }
    }
}

/// `"foreground"`（按下热键时前台窗口所在的显示器）或从 1 开始的显示器序号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MonitorSel {
    Named(String),
    Index(u32),
}

impl Default for MonitorSel {
    fn default() -> Self {
        MonitorSel::Named("foreground".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicRole {
    Communications,
    Console,
}

/// 系统音（回环）方案，S1 实测后决定默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemCapture {
    DefaultEndpoint,
    CommunicationsEndpoint,
    AllEndpoints,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// `"default"` 或设备 ID。
    pub mic_device: String,
    pub system_device: String,
    pub mic_role: MicRole,
    pub system_capture: SystemCapture,
    pub mic_time_source: TimeSource,
    pub system_time_source: TimeSource,
    /// 为回环另开一路静音渲染流保活（S1 对比后决定，默认关）。
    pub system_keepalive: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            mic_device: "default".into(),
            system_device: "default".into(),
            mic_role: MicRole::Communications,
            system_capture: SystemCapture::DefaultEndpoint,
            mic_time_source: TimeSource::ArrivalMinusDuration,
            system_time_source: TimeSource::DeviceQpc,
            system_keepalive: false,
        }
    }
}

/// 时间线修复的阈值，S1 调定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineSettings {
    pub tolerance_ms: u32,
    pub idle_tolerance_ms: u32,
    pub settle_ms: u32,
    pub large_gap_ms: u32,
}

impl Default for TimelineSettings {
    fn default() -> Self {
        Self { tolerance_ms: 40, idle_tolerance_ms: 100, settle_ms: 10, large_gap_ms: 5_000 }
    }
}

/// 健康监测的阈值，S1、S2、S3 调定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthSettings {
    pub silent_confirm_s: u32,
    pub hung_s: u32,
    pub low_fps_ratio: f32,
    pub low_fps_s: u32,
}

impl Default for HealthSettings {
    fn default() -> Self {
        Self { silent_confirm_s: 5, hung_s: 10, low_fps_ratio: 0.8, low_fps_s: 10 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyMode {
    Quiet,
    Standard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub mode: NotifyMode,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self { mode: NotifyMode::Quiet }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub max_hours: f64,
    pub min_free_gb: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_hours: 6.0, min_free_gb: 5.0 }
    }
}

/// 三个热键解析后的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySet {
    pub start: Hotkey,
    pub stop: Hotkey,
    pub quit: Hotkey,
}

impl Config {
    pub fn parse(text: &str, origin: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse { path: origin.to_string(), message: e.to_string() })
    }

    /// 文件不存在返回默认配置；格式有误返回错误，原文件原样保留。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, &path.display().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read { path: path.display().to_string(), source }),
        }
    }

    /// 先写同目录的临时文件再改名，进程在写入中途被杀也不会留下半个配置文件。
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        let werr = |source| ConfigError::Write { path: path.display().to_string(), source };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(werr)?;
        }
        let tmp: PathBuf = path.with_extension("toml.tmp");
        let mut f = fs::File::create(&tmp).map_err(werr)?;
        f.write_all(text.as_bytes()).map_err(werr)?;
        f.sync_all().map_err(werr)?;
        drop(f);
        fs::rename(&tmp, path).map_err(werr)
    }

    pub fn hotkey_set(&self) -> Result<HotkeySet, String> {
        let parse = |name: &str, s: &str| Hotkey::parse(s).map_err(|e| format!("{name}：{e}"));
        let set = HotkeySet { start: parse("hotkeys.start", &self.hotkeys.start)?, stop: parse("hotkeys.stop", &self.hotkeys.stop)?, quit: parse("hotkeys.quit", &self.hotkeys.quit)? };
        if set.start == set.stop || set.start == set.quit || set.stop == set.quit {
            return Err("hotkeys：三个热键不能相同".to_string());
        }
        Ok(set)
    }

    pub fn timeline_config(&self, format: AudioFormat, source: TimeSource) -> TimelineConfig {
        let t = &self.timeline;
        TimelineConfig {
            time_source: source,
            tolerance_ms: t.tolerance_ms,
            idle_tolerance_ms: t.idle_tolerance_ms,
            settle_ms: t.settle_ms,
            large_gap_ms: t.large_gap_ms,
            ..TimelineConfig::new(format)
        }
    }

    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut v = Vec::new();
        if let Err(message) = self.hotkey_set() {
            v.push(issue("hotkeys", message));
        }
        if !matches!(self.video.fps, 10 | 15 | 30) {
            v.push(issue("video.fps", format!("帧率只能是 10、15 或 30，当前为 {}", self.video.fps)));
        }
        let (long, short) = (self.video.max_long_edge, self.video.max_short_edge);
        if !(240..=7_680).contains(&short) || !(240..=7_680).contains(&long) || long < short {
            v.push(issue("video.max_long_edge", format!("分辨率上限不合理：长边 {long}，短边 {short}")));
        }
        match &self.monitor {
            MonitorSel::Named(n) if n == "foreground" => {}
            MonitorSel::Named(n) => v.push(issue("monitor", format!("monitor 只能是 \"foreground\" 或显示器序号，当前为 \"{n}\""))),
            MonitorSel::Index(0) => v.push(issue("monitor", "显示器序号从 1 开始")),
            MonitorSel::Index(_) => {}
        }
        if self.audio.mic_device.trim().is_empty() || self.audio.system_device.trim().is_empty() {
            v.push(issue("audio.mic_device", "设备不能为空，用 \"default\" 表示默认设备"));
        }
        let t = &self.timeline;
        if !(5..=500).contains(&t.tolerance_ms) {
            v.push(issue("timeline.tolerance_ms", "应在 5–500 毫秒之间"));
        }
        if t.idle_tolerance_ms < t.tolerance_ms || t.idle_tolerance_ms > 1_000 {
            v.push(issue("timeline.idle_tolerance_ms", "应不小于 tolerance_ms，且不超过 1000 毫秒"));
        }
        if t.settle_ms >= t.tolerance_ms {
            v.push(issue("timeline.settle_ms", "应小于 tolerance_ms"));
        }
        if t.large_gap_ms < 1_000 {
            v.push(issue("timeline.large_gap_ms", "不应小于 1000 毫秒"));
        }
        let h = &self.health;
        if !(2..=60).contains(&h.silent_confirm_s) {
            v.push(issue("health.silent_confirm_s", "应在 2–60 秒之间"));
        }
        if !(3..=120).contains(&h.hung_s) {
            v.push(issue("health.hung_s", "应在 3–120 秒之间"));
        }
        if !(0.1..=0.95).contains(&h.low_fps_ratio) {
            v.push(issue("health.low_fps_ratio", "应在 0.1–0.95 之间"));
        }
        if !(3..=120).contains(&h.low_fps_s) {
            v.push(issue("health.low_fps_s", "应在 3–120 秒之间"));
        }
        if !(self.limits.max_hours > 0.0 && self.limits.max_hours <= 24.0) {
            v.push(issue("limits.max_hours", "应大于 0 且不超过 24 小时"));
        }
        if !(1.0..=1_000.0).contains(&self.limits.min_free_gb) {
            v.push(issue("limits.min_free_gb", "应在 1–1000 GB 之间"));
        }
        v
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib config::tests::
```

预期：通过，输出里有 `test result: ok. 13 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): config.toml 的数据模型、校验与读写"
```

---

## 任务 12：record-core：session

**目标：** 会话目录与 `session.json`（设计文档 4.3、5.9、16.2）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/session.rs`

**接口：**

- 依赖：`crate::pacer::FrameStats`、`crate::timeline::TimelineStats`
- 提供：`pub const SESSION_FILE: &str`；`pub const SESSION_VERSION: u32`；`pub const MEETING_FILE: &str`；`pub enum TrackKind`；`TrackKind::file_name`；`TrackKind::key`；`TrackKind::segment_file_name`；`pub enum SessionState`；`pub enum EndReason`；`EndReason::describe`；`pub enum TrackStatus`；`pub struct SegmentMeta`；`pub struct TrackMeta`；`TrackMeta::new`；`TrackMeta::files`；`TrackMeta::next_segment_file`；`TrackMeta::push_segment`；`pub struct Tracks`；`Tracks::get`；`Tracks::get_mut`；`Tracks::set`；`Tracks::offsets_hns`；`pub struct MonitorMeta`；`pub struct OutputMeta`；`pub struct DroppedVideoFrames`；`pub struct RepeatedVideoFrames`；`pub struct RestrictedInterval`；`pub struct Diagnostics`；`Diagnostics::absorb_video`；`Diagnostics::absorb_audio`；`pub struct SessionMeta`；`SessionMeta::new`；`SessionMeta::finalize`；`pub fn rfc3339(t: DateTime<Utc>) -> String`；`pub fn session_id(local: NaiveDateTime) -> String`；`pub fn create_session_dir(base: &Path, id: &str) -> io::Result<(PathBuf, String)>`；`pub fn save_meta(dir: &Path, meta: &SessionMeta) -> io::Result<()>`；`pub fn load_meta(dir: &Path) -> io::Result<SessionMeta>`
- 测试：19 个：`id_is_local_time_in_the_documented_format`、`utc_timestamps_are_rfc3339_with_z`、`new_session_starts_in_recording_state`、`finalize_sets_state_reason_and_end_time`、`json_uses_snake_case_names_from_the_spec`、`offsets_are_relative_to_the_earliest_track`、`diagnostics_aggregate_video_and_audio_counters`、`lead_in_and_idle_silence_do_not_count_as_filled_silence`、`save_and_load_round_trip_with_unknown_fields_preserved`、`loading_garbage_reports_the_file` 等

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod session;
```

- [ ] **步骤 2：创建 `crates/record-core/src/session.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    fn t(h: u32, m: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 13, h, m, s).unwrap()
    }

    fn track(file: &str, t0: Option<i64>) -> TrackMeta {
        TrackMeta::new(file, t0)
    }

    #[test]
    fn id_is_local_time_in_the_documented_format() {
        let local = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap().and_hms_opt(20, 30, 0).unwrap();
        assert_eq!(session_id(local), "2026-10-13_203000");
    }

    #[test]
    fn utc_timestamps_are_rfc3339_with_z() {
        assert_eq!(rfc3339(t(12, 30, 0)), "2026-10-13T12:30:00Z");
    }

    #[test]
    fn new_session_starts_in_recording_state() {
        let m = SessionMeta::new("2026-10-13_203000", "0.1.0", t(12, 30, 0), 10_000_000);
        assert_eq!((m.state, m.end_reason, m.ended_utc.clone()), (SessionState::Recording, None, None));
        assert_eq!(m.version, 1);
    }

    #[test]
    fn finalize_sets_state_reason_and_end_time() {
        let mut m = SessionMeta::new("x", "0.1.0", t(12, 30, 0), 10_000_000);
        m.finalize(EndReason::DiskLow, t(13, 31, 12));
        assert_eq!(m.state, SessionState::Finalized);
        assert_eq!(m.end_reason, Some(EndReason::DiskLow));
        assert_eq!(m.ended_utc.as_deref(), Some("2026-10-13T13:31:12Z"));
        m.finalize(EndReason::Recovered, t(14, 0, 0));
        assert_eq!(m.state, SessionState::Recovered);
    }

    #[test]
    fn json_uses_snake_case_names_from_the_spec() {
        let mut m = SessionMeta::new("x", "0.1.0", t(12, 30, 0), 10_000_000);
        m.finalize(EndReason::MaxDuration, t(13, 0, 0));
        m.tracks.set(TrackKind::Video, track("video.mkv", Some(123)));
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        assert_eq!(v["state"], "finalized");
        assert_eq!(v["end_reason"], "max_duration");
        assert_eq!(v["tracks"]["video"]["t0_hns"], 123);
        assert_eq!(v["tracks"]["video"]["status"], "ok");
        assert!(v["tracks"].get("mic").is_none(), "没有的轨不写出");
        for k in [
            "dropped_video_frames",
            "repeated_video_frames",
            "silence_filled_ms",
            "dropped_audio_samples",
            "device_changes",
            "encoder_restarts",
            "capture_silent_alerts",
            "large_gap_events",
            "restricted_intervals",
        ] {
            assert!(v["diagnostics"].get(k).is_some(), "诊断缺少 {k}");
        }
    }

    #[test]
    fn offsets_are_relative_to_the_earliest_track() {
        let mut tr = Tracks::default();
        tr.set(TrackKind::Video, track("video.mkv", Some(1_000_500_000)));
        tr.set(TrackKind::Mic, track("mic.mka", Some(1_000_000_000)));
        tr.set(TrackKind::System, track("system.mka", None));
        let o = tr.offsets_hns();
        assert_eq!(o.get(&TrackKind::Video), Some(&500_000));
        assert_eq!(o.get(&TrackKind::Mic), Some(&0));
        assert_eq!(o.get(&TrackKind::System), None);
        assert!(Tracks::default().offsets_hns().is_empty());
    }

    #[test]
    fn diagnostics_aggregate_video_and_audio_counters() {
        let mut d = Diagnostics::default();
        let fs = FrameStats { repeat_secure_desktop: 3, repeat_duplication_rebuild: 2, dropped_queue: 5, dropped_encoder: 1, ..Default::default() };
        d.absorb_video(&fs);
        assert_eq!((d.repeated_video_frames.secure_desktop, d.repeated_video_frames.duplication_rebuild, d.dropped_video_frames.queue, d.dropped_video_frames.encoder), (3, 2, 5, 1));
        d.absorb_audio("mic", 48_000, &TimelineStats { gap_filled_frames: 48_000, dropped_frames: 10, large_gaps: 1, ..Default::default() });
        d.absorb_audio("system", 48_000, &TimelineStats { outage_filled_frames: 96_000, dropped_frames: 5, large_gaps: 2, ..Default::default() });
        assert_eq!(d.silence_filled_ms, 3_000);
        assert_eq!(d.dropped_audio_samples, 15);
        assert_eq!(d.large_gap_events, 3);
        // 同一轨再次汇总是覆盖而不是累加
        d.absorb_audio("mic", 48_000, &TimelineStats::default());
        assert_eq!(d.silence_filled_ms, 2_000);
    }

    #[test]
    fn lead_in_and_idle_silence_do_not_count_as_filled_silence() {
        let mut d = Diagnostics::default();
        let st = TimelineStats { silence_filled_frames: 200_000, lead_in_frames: 30_000, idle_filled_frames: 170_000, idle_episodes: 4, ..Default::default() };
        d.absorb_audio("system", 48_000, &st);
        assert_eq!(d.silence_filled_ms, 0);
        assert_eq!(d.audio["system"].idle_episodes, 4);
    }

    #[test]
    fn save_and_load_round_trip_with_unknown_fields_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = SessionMeta::new("2026-10-13_203000", "0.1.0", t(12, 30, 0), 10_000_000);
        m.monitor = Some(MonitorMeta { name: "\\\\.\\DISPLAY1".into(), width: 3840, height: 2160 });
        m.output = Some(OutputMeta { width: 1920, height: 1080, fps: 15, encoder: "h264_nvenc".into() });
        m.tracks.set(TrackKind::Mic, track("mic.mka", Some(42)));
        m.extra.insert("transcript".into(), serde_json::json!({ "status": "pending" }));
        save_meta(dir.path(), &m).unwrap();
        let back = load_meta(dir.path()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.extra["transcript"]["status"], "pending");
        let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(leftovers, vec![SESSION_FILE.to_string()]);
    }

    #[test]
    fn loading_garbage_reports_the_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SESSION_FILE), "{ not json").unwrap();
        let e = load_meta(dir.path()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains(SESSION_FILE));
    }

    #[test]
    fn colliding_directory_names_get_a_suffix_and_nothing_is_overwritten() {
        let base = tempfile::tempdir().unwrap();
        let (d1, id1) = create_session_dir(base.path(), "2026-10-13_203000").unwrap();
        fs::write(d1.join("keep.txt"), "x").unwrap();
        let (d2, id2) = create_session_dir(base.path(), "2026-10-13_203000").unwrap();
        let (_, id3) = create_session_dir(base.path(), "2026-10-13_203000").unwrap();
        assert_eq!((id1.as_str(), id2.as_str(), id3.as_str()), ("2026-10-13_203000", "2026-10-13_203000-2", "2026-10-13_203000-3"));
        assert!(d1.join("keep.txt").exists() && d2.exists());
    }

    #[test]
    fn the_base_directory_is_created_on_demand() {
        let base = tempfile::tempdir().unwrap();
        let nested = base.path().join("a").join("b");
        let (dir, _) = create_session_dir(&nested, "s").unwrap();
        assert!(dir.is_dir());
    }

    #[test]
    fn track_files_match_the_documented_names() {
        let names: Vec<_> = TrackKind::ALL.iter().map(|k| k.file_name()).collect();
        assert_eq!(names, ["video.mkv", "mic.mka", "system.mka"]);
        assert_eq!(MEETING_FILE, "meeting.mp4");
    }

    #[test]
    fn segments_only_appear_in_json_after_an_encoder_restart() {
        let mut t = track("mic.mka", Some(5));
        assert!(serde_json::to_value(&t).unwrap().get("segments").is_none(), "没重启过就不写这一项，旧版本的 session.json 保持原样");
        assert_eq!(t.files(), vec!["mic.mka"]);
        t.segments = vec![SegmentMeta { file: "mic.mka".into(), t0_hns: 5, frames: None }, SegmentMeta { file: "mic.2.mka".into(), t0_hns: 900, frames: None }];
        let back: TrackMeta = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.files(), vec!["mic.mka", "mic.2.mka"]);
    }

    #[test]
    fn the_first_restart_registers_the_original_file_as_segment_one() {
        let mut t = TrackMeta::new("video.mkv", Some(1_000));
        assert_eq!(t.next_segment_file(TrackKind::Video), "video.2.mkv");
        t.push_segment("video.2.mkv".into(), 9_000, Some(40));
        assert_eq!(t.segments, vec![SegmentMeta { file: "video.mkv".into(), t0_hns: 1_000, frames: Some(40) }, SegmentMeta { file: "video.2.mkv".into(), t0_hns: 9_000, frames: None },]);
        assert_eq!(t.files(), vec!["video.mkv", "video.2.mkv"]);
        assert_eq!(t.next_segment_file(TrackKind::Video), "video.3.mkv");
    }

    #[test]
    fn a_later_restart_closes_the_previous_segment_and_audio_segments_have_no_frame_count() {
        let mut t = TrackMeta::new("mic.mka", Some(5));
        t.push_segment("mic.2.mka".into(), 50, None);
        t.push_segment("mic.3.mka".into(), 90, None);
        assert_eq!(t.segments.iter().map(|s| (s.file.as_str(), s.t0_hns, s.frames)).collect::<Vec<_>>(), vec![("mic.mka", 5, None), ("mic.2.mka", 50, None), ("mic.3.mka", 90, None)]);
    }

    #[test]
    fn a_track_that_never_got_an_anchor_starts_its_first_segment_where_the_second_begins() {
        let mut t = TrackMeta::new("system.mka", None);
        t.push_segment("system.2.mka".into(), 70, None);
        assert_eq!(t.segments[0].t0_hns, 70);
    }

    #[test]
    fn tracks_can_be_edited_in_place() {
        let mut tracks = Tracks::default();
        tracks.set(TrackKind::Mic, TrackMeta::new("mic.mka", Some(3)));
        tracks.get_mut(TrackKind::Mic).unwrap().status = TrackStatus::Interrupted;
        assert_eq!(tracks.get(TrackKind::Mic).unwrap().status, TrackStatus::Interrupted);
        assert!(tracks.get_mut(TrackKind::Video).is_none());
    }

    #[test]
    fn a_session_json_from_before_segments_existed_still_loads() {
        let old = r#"{"file":"video.mkv","t0_hns":7,"status":"ok"}"#;
        let t: TrackMeta = serde_json::from_str(old).unwrap();
        assert_eq!((t.segments.len(), t.t0_hns), (0, Some(7)));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib session::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/session.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 会话目录与 `session.json`（设计文档 4.3、5.9、16.2）。
//!
//! 子项目 A 不引入数据库：会话状态以 `session.json` 为准，启动时扫描修复。
//! 文件用“临时文件 + 改名”原子写入，崩溃时不会留下半个元数据文件。

use crate::pacer::FrameStats;
use crate::timeline::TimelineStats;
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const SESSION_FILE: &str = "session.json";
pub const SESSION_VERSION: u32 = 1;
pub const MEETING_FILE: &str = "meeting.mp4";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Mic,
    System,
}

impl TrackKind {
    pub const ALL: [TrackKind; 3] = [TrackKind::Video, TrackKind::Mic, TrackKind::System];

    pub fn file_name(self) -> &'static str {
        match self {
            TrackKind::Video => "video.mkv",
            TrackKind::Mic => "mic.mka",
            TrackKind::System => "system.mka",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            TrackKind::Video => "video",
            TrackKind::Mic => "mic",
            TrackKind::System => "system",
        }
    }

    /// 第 `n` 段的文件名（从 1 起）。第 1 段就是 [`TrackKind::file_name`]；
    /// 编码器重启后写第 2 段起：`video.2.mkv`、`mic.2.mka`……
    pub fn segment_file_name(self, n: u32) -> String {
        let base = self.file_name();
        if n <= 1 {
            return base.to_string();
        }
        let (stem, ext) = base.rsplit_once('.').expect("轨道文件名带扩展名");
        format!("{stem}.{n}.{ext}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// 录制中；启动时仍是这个状态说明上次没有正常收尾。
    Recording,
    Finalized,
    /// 崩溃后已修复。
    Recovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    User,
    MaxDuration,
    DiskLow,
    Sleep,
    Shutdown,
    Logoff,
    WriteError,
    CaptureFailure,
    Recovered,
}

impl EndReason {
    /// 托盘提示用的说明。
    pub fn describe(self) -> &'static str {
        match self {
            EndReason::User => "已保存",
            EndReason::MaxDuration => "已达到单场时长上限，已自动保存",
            EndReason::DiskLow => "磁盘剩余空间不足，已自动保存",
            EndReason::Sleep => "因睡眠已保存",
            EndReason::Shutdown => "因关机已保存",
            EndReason::Logoff => "因注销已保存",
            EndReason::WriteError => "写入出错，已保存已有内容",
            EndReason::CaptureFailure => "采集失败，已保存已有内容",
            EndReason::Recovered => "已恢复上次中断的录制",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackStatus {
    Ok,
    /// 录制中该轨中断过（设备丢失超过 10 秒、编码器重启仍失败等）。
    Interrupted,
    /// 没有这一轨（例如没有麦克风）。
    Missing,
}

/// 编码器中途重启后，一条轨道由多个文件（段）接成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub file: String,
    /// 这一段第一个采样（帧）的 QPC 时间，100 ns 单位。
    pub t0_hns: i64,
    /// 画面轨：已经关闭的段里的帧数（下一段从这个序号接着写，成品用它拼接）。最后一段（仍在写、或崩溃时）为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackMeta {
    /// 第一段的文件名。
    pub file: String,
    /// 该轨起点的 QPC 时间，100 ns 单位；该轨始终没有数据时为空。
    pub t0_hns: Option<i64>,
    pub status: TrackStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 编码器重启过的轨道才有：按顺序列出每一段。没有重启过时为空，只有 `file` 一个文件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<SegmentMeta>,
}

impl TrackMeta {
    pub fn new(file: impl Into<String>, t0_hns: Option<i64>) -> Self {
        Self { file: file.into(), t0_hns, status: TrackStatus::Ok, note: None, segments: Vec::new() }
    }

    /// 这条轨道的全部文件名，按先后顺序。
    pub fn files(&self) -> Vec<String> {
        if self.segments.is_empty() { vec![self.file.clone()] } else { self.segments.iter().map(|s| s.file.clone()).collect() }
    }

    /// 编码器重启时，下一段要写的文件名：没重启过的轨道是第 1 段，下一段就是第 2 段。
    pub fn next_segment_file(&self, kind: TrackKind) -> String {
        kind.segment_file_name(self.segments.len().max(1) as u32 + 1)
    }

    /// 新写入器已经换上之后登记新的一段。首次重启时先把原来的文件登记为第 1 段；
    /// `closed_frames` 是被替换那一段实际含有的帧数（画面），音频传 `None`。
    pub fn push_segment(&mut self, file: String, t0_hns: i64, closed_frames: Option<u64>) {
        if self.segments.is_empty() {
            self.segments.push(SegmentMeta { file: self.file.clone(), t0_hns: self.t0_hns.unwrap_or(t0_hns), frames: closed_frames });
        } else if let Some(last) = self.segments.last_mut() {
            last.frames = closed_frames;
        }
        self.segments.push(SegmentMeta { file, t0_hns, frames: None });
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<TrackMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic: Option<TrackMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<TrackMeta>,
}

impl Tracks {
    pub fn get(&self, kind: TrackKind) -> Option<&TrackMeta> {
        match kind {
            TrackKind::Video => self.video.as_ref(),
            TrackKind::Mic => self.mic.as_ref(),
            TrackKind::System => self.system.as_ref(),
        }
    }

    pub fn get_mut(&mut self, kind: TrackKind) -> Option<&mut TrackMeta> {
        match kind {
            TrackKind::Video => self.video.as_mut(),
            TrackKind::Mic => self.mic.as_mut(),
            TrackKind::System => self.system.as_mut(),
        }
    }

    pub fn set(&mut self, kind: TrackKind, meta: TrackMeta) {
        match kind {
            TrackKind::Video => self.video = Some(meta),
            TrackKind::Mic => self.mic = Some(meta),
            TrackKind::System => self.system = Some(meta),
        }
    }

    /// 各轨起点相对最早起点的偏移（100 ns），只含有起点的轨。
    pub fn offsets_hns(&self) -> BTreeMap<TrackKind, i64> {
        let starts: Vec<(TrackKind, i64)> = TrackKind::ALL.iter().filter_map(|&k| self.get(k).and_then(|t| t.t0_hns).map(|t| (k, t))).collect();
        let Some(min) = starts.iter().map(|&(_, t)| t).min() else {
            return BTreeMap::new();
        };
        starts.into_iter().map(|(k, t)| (k, t - min)).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorMeta {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputMeta {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedVideoFrames {
    pub encoder: u64,
    pub error: u64,
    pub queue: u64,
    pub resync: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepeatedVideoFrames {
    pub secure_desktop: u64,
    pub duplication_rebuild: u64,
    pub duplication_failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestrictedInterval {
    pub start_ms: u64,
    pub end_ms: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Diagnostics {
    pub dropped_video_frames: DroppedVideoFrames,
    pub repeated_video_frames: RepeatedVideoFrames,
    pub silence_filled_ms: u64,
    pub dropped_audio_samples: u64,
    pub device_changes: u32,
    pub encoder_restarts: u32,
    pub capture_silent_alerts: u32,
    pub large_gap_events: u32,
    pub hung_alerts: u32,
    pub low_fps_alerts: u32,
    pub restricted_intervals: Vec<RestrictedInterval>,
    /// 每条音轨时间线的完整计数，键为 `mic`、`system`。
    pub audio: BTreeMap<String, TimelineStats>,
}

impl Diagnostics {
    pub fn absorb_video(&mut self, s: &FrameStats) {
        self.dropped_video_frames = DroppedVideoFrames { encoder: s.dropped_encoder, error: s.dropped_error, queue: s.dropped_queue, resync: s.dropped_resync };
        self.repeated_video_frames =
            RepeatedVideoFrames { secure_desktop: s.repeat_secure_desktop, duplication_rebuild: s.repeat_duplication_rebuild, duplication_failed: s.repeat_duplication_failed };
    }

    /// 记入一条音轨的时间线计数，并重新汇总异常补静音时长与总丢弃采样数。
    /// `silence_filled_ms` 只统计**异常**的补静音（包间空档 + 流中断）：首包前的静音是打开设备的耗时，
    /// 空闲补齐是回环设备无声时的常态，两者都不该让“录制质量”看起来有问题，它们在 `audio` 里有各自的计数。
    pub fn absorb_audio(&mut self, track: &str, rate: u32, stats: &TimelineStats) {
        self.audio.insert(track.to_string(), stats.clone());
        let filled: u64 = self.audio.values().map(|s| s.gap_filled_frames + s.outage_filled_frames).sum();
        // 各轨采样率相同（48 kHz），总时长按同一采样率折算
        self.silence_filled_ms = filled * 1000 / u64::from(rate.max(1));
        self.dropped_audio_samples = self.audio.values().map(|s| s.dropped_frames).sum();
        self.large_gap_events = self.audio.values().map(|s| s.large_gaps as u32).sum();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub version: u32,
    pub id: String,
    pub app_version: String,
    pub state: SessionState,
    #[serde(default)]
    pub end_reason: Option<EndReason>,
    pub started_utc: String,
    #[serde(default)]
    pub ended_utc: Option<String>,
    pub qpc_frequency: i64,
    #[serde(default)]
    pub monitor: Option<MonitorMeta>,
    #[serde(default)]
    pub output: Option<OutputMeta>,
    #[serde(default)]
    pub tracks: Tracks,
    #[serde(default)]
    pub diagnostics: Diagnostics,
    /// 收到停止请求时的 QPC（100 ns）。`meeting.mp4` 的时长按它裁剪（回环音轨会比停止时刻晚结束约一个设备延迟）。
    /// 崩溃的会话没有这一项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_hns: Option<i64>,
    /// 不认识的字段原样保留（子项目 B、C 会往里加转写、纪要的状态）。
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl SessionMeta {
    pub fn new(id: &str, app_version: &str, started: DateTime<Utc>, qpc_frequency: i64) -> Self {
        Self {
            version: SESSION_VERSION,
            id: id.to_string(),
            app_version: app_version.to_string(),
            state: SessionState::Recording,
            end_reason: None,
            started_utc: rfc3339(started),
            ended_utc: None,
            qpc_frequency,
            monitor: None,
            output: None,
            tracks: Tracks::default(),
            diagnostics: Diagnostics::default(),
            stopped_hns: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn finalize(&mut self, reason: EndReason, ended: DateTime<Utc>) {
        self.state = if reason == EndReason::Recovered { SessionState::Recovered } else { SessionState::Finalized };
        self.end_reason = Some(reason);
        self.ended_utc = Some(rfc3339(ended));
    }
}

pub fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 会话目录名：本地时间 `YYYY-MM-DD_HHMMSS`。
pub fn session_id(local: NaiveDateTime) -> String {
    local.format("%Y-%m-%d_%H%M%S").to_string()
}

/// 在 `base` 下建立会话目录。同一秒内重名时加 `-2`、`-3` 后缀，不覆盖已有目录。
/// 返回 `(目录路径, 实际使用的 id)`。
pub fn create_session_dir(base: &Path, id: &str) -> io::Result<(PathBuf, String)> {
    fs::create_dir_all(base)?;
    for n in 1..=99u32 {
        let name = if n == 1 { id.to_string() } else { format!("{id}-{n}") };
        let dir = base.join(&name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok((dir, name)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("同一秒内已有太多个会话目录：{id}")))
}

pub fn save_meta(dir: &Path, meta: &SessionMeta) -> io::Result<()> {
    let text = serde_json::to_string_pretty(meta).map_err(io::Error::other)?;
    let tmp = dir.join(format!("{SESSION_FILE}.tmp"));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(SESSION_FILE))
}

pub fn load_meta(dir: &Path) -> io::Result<SessionMeta> {
    let text = fs::read_to_string(dir.join(SESSION_FILE))?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}：{e}", dir.join(SESSION_FILE).display())))
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib session::tests::
```

预期：通过，输出里有 `test result: ok. 19 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 会话目录与 session.json"
```

---

## 任务 13：record-core：controller

**目标：** 控制器：待命 → 启动中 → 录制中 → 收尾中 → 待命（设计文档 6.1）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/controller.rs`

**接口：**

- 依赖：`crate::session::EndReason`
- 提供：`pub const DEBOUNCE_MS: u64`；`pub enum State`；`pub enum Command`；`pub enum Input`；`pub enum Action`；`pub struct Controller`；`Controller::new`；`Controller::state`；`Controller::handle`
- 测试：17 个：`start_stop_cycle`、`commands_are_idempotent`、`repeated_commands_within_500ms_are_ignored`、`a_different_command_is_not_debounced`、`start_failure_returns_to_idle_with_a_message`、`quit_when_idle_exits_immediately`、`quit_while_recording_saves_first`、`stop_during_starting_is_deferred_until_recording_begins`、`quit_during_starting_waits_for_the_outcome`、`start_during_finalizing_begins_the_next_session_afterwards` 等

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod controller;
```

- [ ] **步骤 2：创建 `crates/record-core/src/controller.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;
    use Command::*;

    fn cmd(c: Command) -> Input {
        Input::Command(c)
    }

    fn recording() -> Controller {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        c.handle(Input::StartSucceeded, 100);
        assert_eq!(c.state(), State::Recording);
        c
    }

    #[test]
    fn start_stop_cycle() {
        let mut c = Controller::new();
        assert_eq!(c.handle(cmd(Start), 0), vec![BeginStart]);
        assert_eq!(c.state(), State::Starting);
        assert_eq!(c.handle(Input::StartSucceeded, 10), vec![]);
        assert_eq!(c.handle(cmd(Stop), 10_000), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.state(), State::Finalizing);
        assert_eq!(c.handle(Input::FinalizeDone, 11_000), vec![NotifySaved(EndReason::User)]);
        assert_eq!(c.state(), State::Idle);
    }

    #[test]
    fn commands_are_idempotent() {
        let mut c = Controller::new();
        assert_eq!(c.handle(cmd(Stop), 0), vec![], "待命时停止无效果");
        let mut c = recording();
        assert_eq!(c.handle(cmd(Start), 5_000), vec![], "录制中开始无效果");
        assert_eq!(c.state(), State::Recording);
    }

    #[test]
    fn repeated_commands_within_500ms_are_ignored() {
        let mut c = Controller::new();
        assert_eq!(c.handle(cmd(Start), 1_000), vec![BeginStart]);
        c.handle(Input::StartFailed("x".into()), 1_100); // 回到待命
        // 同一个命令 400 ms 后又来：去抖忽略，即使状态允许
        assert_eq!(c.handle(cmd(Start), 1_400), vec![]);
        assert_eq!(c.state(), State::Idle);
        // 满 500 ms 后接受
        assert_eq!(c.handle(cmd(Start), 1_500), vec![BeginStart]);
    }

    #[test]
    fn a_different_command_is_not_debounced() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        c.handle(Input::StartSucceeded, 50);
        assert_eq!(c.handle(cmd(Stop), 100), vec![BeginFinalize(EndReason::User)]);
    }

    #[test]
    fn start_failure_returns_to_idle_with_a_message() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(Input::StartFailed("磁盘不可写".into()), 10), vec![NotifyStartFailed("磁盘不可写".into())]);
        assert_eq!(c.state(), State::Idle);
        assert_eq!(c.handle(cmd(Start), 2_000), vec![BeginStart], "失败后可以重新开始");
    }

    #[test]
    fn quit_when_idle_exits_immediately() {
        assert_eq!(Controller::new().handle(cmd(Quit), 0), vec![QuitApp]);
    }

    #[test]
    fn quit_while_recording_saves_first() {
        let mut c = recording();
        assert_eq!(c.handle(cmd(Quit), 5_000), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.handle(Input::FinalizeDone, 6_000), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn stop_during_starting_is_deferred_until_recording_begins() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(cmd(Stop), 100), vec![]);
        assert_eq!(c.state(), State::Starting);
        assert_eq!(c.handle(Input::StartSucceeded, 200), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.state(), State::Finalizing);
    }

    #[test]
    fn quit_during_starting_waits_for_the_outcome() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(cmd(Quit), 100), vec![]);
        assert_eq!(c.handle(Input::StartFailed("设备打不开".into()), 200), vec![NotifyStartFailed("设备打不开".into()), QuitApp]);

        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        c.handle(cmd(Quit), 100);
        assert_eq!(c.handle(Input::StartSucceeded, 200), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.handle(Input::FinalizeDone, 300), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn start_during_finalizing_begins_the_next_session_afterwards() {
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        assert_eq!(c.handle(cmd(Start), 5_600), vec![]);
        assert_eq!(c.handle(Input::FinalizeDone, 6_000), vec![NotifySaved(EndReason::User), BeginStart]);
        assert_eq!(c.state(), State::Starting);
    }

    #[test]
    fn quit_cancels_a_queued_start() {
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        c.handle(cmd(Start), 5_600);
        c.handle(cmd(Quit), 6_200);
        assert_eq!(c.handle(Input::FinalizeDone, 7_000), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn auto_protections_finalize_with_their_reason() {
        for reason in [EndReason::MaxDuration, EndReason::DiskLow, EndReason::Sleep, EndReason::WriteError, EndReason::CaptureFailure] {
            let mut c = recording();
            assert_eq!(c.handle(Input::AutoStop(reason), 9_000), vec![BeginFinalize(reason)]);
            assert_eq!(c.handle(Input::FinalizeDone, 9_500), vec![NotifySaved(reason)]);
            assert_eq!(c.state(), State::Idle);
        }
    }

    #[test]
    fn auto_stop_is_ignored_when_not_recording() {
        let mut c = Controller::new();
        assert_eq!(c.handle(Input::AutoStop(EndReason::DiskLow), 0), vec![]);
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        assert_eq!(c.handle(Input::AutoStop(EndReason::DiskLow), 5_100), vec![], "已经在收尾");
    }

    #[test]
    fn auto_stop_during_starting_applies_once_recording_begins() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(Input::AutoStop(EndReason::Sleep), 50), vec![]);
        assert_eq!(c.handle(Input::StartSucceeded, 100), vec![BeginFinalize(EndReason::Sleep)]);
    }

    #[test]
    fn shutdown_saves_then_quits() {
        let mut c = recording();
        assert_eq!(c.handle(Input::EndSession(EndReason::Shutdown), 9_000), vec![BeginFinalize(EndReason::Shutdown)]);
        assert_eq!(c.handle(Input::FinalizeDone, 9_800), vec![NotifySaved(EndReason::Shutdown), QuitApp]);

        let mut idle = Controller::new();
        assert_eq!(idle.handle(Input::EndSession(EndReason::Logoff), 0), vec![QuitApp]);
    }

    #[test]
    fn shutdown_while_finalizing_quits_after_it_completes() {
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        assert_eq!(c.handle(Input::EndSession(EndReason::Shutdown), 5_100), vec![]);
        assert_eq!(c.handle(Input::FinalizeDone, 5_900), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn stray_completion_events_are_ignored() {
        let mut c = Controller::new();
        assert_eq!(c.handle(Input::StartSucceeded, 0), vec![]);
        assert_eq!(c.handle(Input::FinalizeDone, 0), vec![]);
        assert_eq!(c.handle(Input::StartFailed("x".into()), 0), vec![]);
        assert_eq!(c.state(), State::Idle);
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib controller::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/controller.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 控制器：待命 → 启动中 → 录制中 → 收尾中 → 待命（设计文档 6.1）。
//!
//! 纯状态机，不碰线程和设备：外壳把热键、菜单、自动保护、系统事件翻译成 [`Input`] 送进来，
//! 按返回的 [`Action`] 去执行。所有输入由同一个线程串行送入。

use crate::session::EndReason;

/// 500 ms 内重复的同一命令被忽略。
pub const DEBOUNCE_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Starting,
    Recording,
    Finalizing,
}

/// 用户命令，热键与托盘菜单共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    /// 保存并退出程序。
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Command(Command),
    /// 启动流程完成，已开始写入。
    StartSucceeded,
    StartFailed(String),
    /// 收尾完成。
    FinalizeDone,
    /// 自动保护、设备故障、睡眠等需要结束本场的原因。
    AutoStop(EndReason),
    /// 关机或注销：保存本场并退出程序。
    EndSession(EndReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 创建会话目录，启动采集与编码。
    BeginStart,
    /// 停止采集、冲刷队列、关闭编码器、写 `session.json`。
    BeginFinalize(EndReason),
    NotifyStartFailed(String),
    NotifySaved(EndReason),
    QuitApp,
}

#[derive(Debug)]
pub struct Controller {
    state: State,
    last_command: Option<(Command, u64)>,
    pending_stop: Option<EndReason>,
    pending_start: bool,
    quit_after: bool,
    finalizing_reason: EndReason,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl Controller {
    pub fn new() -> Self {
        Self { state: State::Idle, last_command: None, pending_stop: None, pending_start: false, quit_after: false, finalizing_reason: EndReason::User }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// `now_ms` 是单调递增的毫秒时钟，只用于去抖。
    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Action> {
        match input {
            Input::Command(cmd) => {
                if let Some((last, at)) = self.last_command
                    && last == cmd
                    && now_ms.saturating_sub(at) < DEBOUNCE_MS
                {
                    return vec![];
                }
                self.last_command = Some((cmd, now_ms));
                self.command(cmd)
            }
            Input::StartSucceeded => self.start_succeeded(),
            Input::StartFailed(msg) => self.start_failed(msg),
            Input::FinalizeDone => self.finalize_done(),
            Input::AutoStop(reason) => self.auto_stop(reason, false),
            Input::EndSession(reason) => self.auto_stop(reason, true),
        }
    }

    fn command(&mut self, cmd: Command) -> Vec<Action> {
        match (self.state, cmd) {
            (State::Idle, Command::Start) => {
                self.state = State::Starting;
                vec![Action::BeginStart]
            }
            (State::Idle, Command::Stop) => vec![],
            (State::Idle, Command::Quit) => vec![Action::QuitApp],

            (State::Starting, Command::Start) => vec![],
            (State::Starting, Command::Stop) => {
                self.pending_stop.get_or_insert(EndReason::User);
                vec![]
            }
            (State::Starting, Command::Quit) => {
                self.pending_stop.get_or_insert(EndReason::User);
                self.quit_after = true;
                vec![]
            }

            (State::Recording, Command::Start) => vec![],
            (State::Recording, Command::Stop) => self.begin_finalize(EndReason::User),
            (State::Recording, Command::Quit) => {
                self.quit_after = true;
                self.begin_finalize(EndReason::User)
            }

            (State::Finalizing, Command::Start) => {
                self.pending_start = true;
                vec![]
            }
            (State::Finalizing, Command::Stop) => vec![],
            (State::Finalizing, Command::Quit) => {
                self.quit_after = true;
                self.pending_start = false;
                vec![]
            }
        }
    }

    fn begin_finalize(&mut self, reason: EndReason) -> Vec<Action> {
        self.state = State::Finalizing;
        self.finalizing_reason = reason;
        vec![Action::BeginFinalize(reason)]
    }

    fn start_succeeded(&mut self) -> Vec<Action> {
        if self.state != State::Starting {
            return vec![];
        }
        self.state = State::Recording;
        match self.pending_stop.take() {
            Some(reason) => self.begin_finalize(reason),
            None => vec![],
        }
    }

    fn start_failed(&mut self, msg: String) -> Vec<Action> {
        if self.state != State::Starting {
            return vec![];
        }
        self.state = State::Idle;
        self.pending_stop = None;
        let mut actions = vec![Action::NotifyStartFailed(msg)];
        if std::mem::take(&mut self.quit_after) {
            actions.push(Action::QuitApp);
        }
        actions
    }

    fn finalize_done(&mut self) -> Vec<Action> {
        if self.state != State::Finalizing {
            return vec![];
        }
        self.state = State::Idle;
        let mut actions = vec![Action::NotifySaved(self.finalizing_reason)];
        if std::mem::take(&mut self.quit_after) {
            self.pending_start = false;
            actions.push(Action::QuitApp);
        } else if std::mem::take(&mut self.pending_start) {
            self.state = State::Starting;
            actions.push(Action::BeginStart);
        }
        actions
    }

    fn auto_stop(&mut self, reason: EndReason, then_quit: bool) -> Vec<Action> {
        if then_quit {
            self.quit_after = true;
            self.pending_start = false;
        }
        match self.state {
            State::Idle => {
                if then_quit {
                    self.quit_after = false;
                    vec![Action::QuitApp]
                } else {
                    vec![]
                }
            }
            State::Starting => {
                self.pending_stop = Some(reason);
                vec![]
            }
            State::Recording => self.begin_finalize(reason),
            State::Finalizing => vec![],
        }
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib controller::tests::
```

预期：通过，输出里有 `test result: ok. 17 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 控制器状态机（待命、启动中、录制中、收尾中）"
```

---

## 任务 14：record-core：ffmpeg_args

**目标：** ffmpeg 命令行的构造（设计文档 5.6、5.7、5.8）。只拼参数，不启动进程。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/ffmpeg_args.rs`

**接口：**

- 依赖：`crate::clock::HNS_PER_SEC`、`crate::config::Encoder`、`crate::session::{MEETING_FILE, SessionMeta, TrackKind, Tracks}`
- 提供：`pub enum ArgsError`；`pub struct VideoSpec`；`pub struct AudioSpec`；`pub fn video_writer_args(spec: &VideoSpec, out: &Path) -> Result<Vec<OsString>, ArgsError>`；`pub fn audio_writer_args(spec: &AudioSpec, out: &Path) -> Vec<OsString>`；`pub fn probe_args(enc: Encoder) -> Result<Vec<OsString>, ArgsError>`；`pub fn count_frames_args(file: &Path) -> Vec<OsString>`；`pub fn parse_progress_frames(text: &str) -> Option<u64>`；`pub fn repair_args(src: &Path, dst: &Path) -> Vec<OsString>`；`pub struct PlanSegment`；`pub const VIDEO_CONCAT_FILE: &str`；`pub struct FinalizePlan`；`FinalizePlan::from_tracks`；`FinalizePlan::from_meta`；`pub fn meeting_part_path(dir: &Path) -> PathBuf`；`pub fn finalize_args(dir: &Path, plan: &FinalizePlan, audio_rate: u32) -> Result<Vec<OsString>, ArgsError>`；`pub fn concat_list(segs: &[PlanSegment], fps: u32) -> String`；`pub fn display(args: &[OsString]) -> String`
- 测试：27 个：`nvenc_video_writer_reads_nv12_from_stdin_and_tags_bt709`、`every_encoder_has_its_own_codec_and_gop`、`auto_encoder_must_be_resolved_first`、`audio_writer_is_lossless_flac_in_matroska`、`counting_frames_copies_the_video_stream_to_a_null_output_and_reports_progress`、`the_last_progress_block_wins`、`repair_copies_streams_without_re_encoding`、`plan_uses_offsets_from_the_earliest_track_and_skips_missing_files`、`full_mix_aligns_with_adelay_and_mixes_with_a_limiter`、`video_offset_uses_seven_decimal_seconds` 等

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod ffmpeg_args;
```

- [ ] **步骤 2：创建 `crates/record-core/src/ffmpeg_args.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TrackMeta;

    fn s(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn has_seq(args: &[String], seq: &[&str]) -> bool {
        args.windows(seq.len()).any(|w| w.iter().zip(seq).all(|(a, b)| a == b))
    }

    fn vspec(enc: Encoder) -> VideoSpec {
        VideoSpec { width: 1920, height: 1080, fps: 15, encoder: enc }
    }

    #[test]
    fn nvenc_video_writer_reads_nv12_from_stdin_and_tags_bt709() {
        let a = s(&video_writer_args(&vspec(Encoder::Nvenc), Path::new("video.mkv")).unwrap());
        assert!(has_seq(&a, &["-f", "rawvideo", "-pix_fmt", "nv12", "-s", "1920x1080", "-framerate", "15", "-i", "pipe:0"]), "{a:?}");
        assert!(has_seq(&a, &["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "28"]));
        assert!(has_seq(&a, &["-bf", "0", "-g", "30"]));
        assert!(has_seq(&a, &["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]));
        assert!(has_seq(&a, &["-fps_mode", "passthrough"]));
        assert!(has_seq(&a, &["-cluster_time_limit", "1000", "-cluster_size_limit", "1048576", "-flush_packets", "1", "-f", "matroska", "video.mkv"]));
        assert!(a.contains(&"-an".to_string()));
    }

    #[test]
    fn every_encoder_has_its_own_codec_and_gop() {
        for (enc, codec) in [(Encoder::Nvenc, "h264_nvenc"), (Encoder::Qsv, "h264_qsv"), (Encoder::Amf, "h264_amf"), (Encoder::Software, "libopenh264")] {
            let a = s(&video_writer_args(&VideoSpec { fps: 30, ..vspec(enc) }, Path::new("v.mkv")).unwrap());
            assert!(has_seq(&a, &["-c:v", codec]), "{enc:?}");
            assert!(has_seq(&a, &["-g", "60"]), "{enc:?} 的关键帧间隔应为 2 秒");
        }
        let sw = s(&video_writer_args(&vspec(Encoder::Software), Path::new("v.mkv")).unwrap());
        assert!(has_seq(&sw, &["-pix_fmt", "yuv420p"]), "软件编码需要 yuv420p");
    }

    #[test]
    fn auto_encoder_must_be_resolved_first() {
        assert_eq!(video_writer_args(&vspec(Encoder::Auto), Path::new("v.mkv")).unwrap_err(), ArgsError::EncoderNotResolved);
    }

    #[test]
    fn audio_writer_is_lossless_flac_in_matroska() {
        let a = s(&audio_writer_args(&AudioSpec { rate: 48_000, channels: 2 }, Path::new("system.mka")));
        assert!(has_seq(&a, &["-f", "f32le", "-ar", "48000", "-ac", "2", "-i", "pipe:0"]));
        assert!(has_seq(&a, &["-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"]));
        assert!(has_seq(&a, &["-cluster_time_limit", "1000", "-cluster_size_limit", "1048576", "-flush_packets", "1", "-f", "matroska", "system.mka"]), "{a:?}");
    }

    #[test]
    fn counting_frames_copies_the_video_stream_to_a_null_output_and_reports_progress() {
        let a = s(&count_frames_args(Path::new("v.mkv")));
        assert!(has_seq(&a, &["-i", "v.mkv", "-map", "0:v:0", "-c", "copy", "-f", "null", "-", "-progress", "pipe:2"]), "{a:?}");
    }

    #[test]
    fn the_last_progress_block_wins() {
        let text = "frame=10\nfps=0.0\nprogress=continue\nframe=150\nfps=0.0\ntotal_size=N/A\nprogress=end\n";
        assert_eq!(parse_progress_frames(text), Some(150));
        assert_eq!(parse_progress_frames("frame=  7 \n"), Some(7));
        assert_eq!(parse_progress_frames("没有进度输出"), None);
        assert_eq!(parse_progress_frames("frame=N/A\n"), None);
    }

    #[test]
    fn repair_copies_streams_without_re_encoding() {
        let a = s(&repair_args(Path::new("a.mkv"), Path::new("a.mkv.repair")));
        assert!(has_seq(&a, &["-i", "a.mkv", "-c", "copy", "-f", "matroska", "a.mkv.repair"]));
    }

    fn meta(t0: i64) -> TrackMeta {
        TrackMeta::new("", Some(t0))
    }

    #[test]
    fn plan_uses_offsets_from_the_earliest_track_and_skips_missing_files() {
        let mut t = Tracks::default();
        t.set(TrackKind::Video, meta(10_000_500));
        t.set(TrackKind::Mic, meta(10_000_000));
        t.set(TrackKind::System, meta(10_020_000));
        let p = FinalizePlan::from_tracks(&t, |_| true);
        assert_eq!(p, FinalizePlan { video: Some(500), mic: Some(0), system: Some(20_000), ..Default::default() });
        let p = FinalizePlan::from_tracks(&t, |k| k != TrackKind::Mic);
        assert_eq!((p.video, p.mic, p.system), (Some(500), None, Some(20_000)));
    }

    #[test]
    fn full_mix_aligns_with_adelay_and_mixes_with_a_limiter() {
        let plan = FinalizePlan { video: Some(0), mic: Some(0), system: Some(25_000_000), ..Default::default() }; // 系统音晚 2.5 秒
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(graph, "[1:a]pan=stereo|c0=c0|c1=c0[a0];[2:a]adelay=120000S:all=1[a1];[a0][a1]amix=inputs=2:normalize=0,alimiter=limit=0.95[a]");
        assert!(
            has_seq(&a, &["-itsoffset", "0.0000000", "-i", "D\\video.mkv", "-i", "D\\mic.mka", "-i", "D\\system.mka"])
                || has_seq(&a, &["-itsoffset", "0.0000000", "-i", "D/video.mkv", "-i", "D/mic.mka", "-i", "D/system.mka"]),
            "{a:?}"
        );
        assert!(has_seq(&a, &["-map", "0:v", "-map", "[a]", "-c:v", "copy", "-c:a", "aac", "-b:a", "128k"]));
        assert!(has_seq(&a, &["-movflags", "+faststart", "-f", "mp4"]));
        assert!(a.last().unwrap().ends_with("meeting.mp4.part"));
    }

    #[test]
    fn video_offset_uses_seven_decimal_seconds() {
        let plan = FinalizePlan { video: Some(12_345_678), mic: Some(0), system: None, ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(has_seq(&a, &["-itsoffset", "1.2345678"]));
    }

    #[test]
    fn a_single_audio_track_is_not_mixed() {
        let plan = FinalizePlan { video: Some(0), mic: None, system: Some(0), ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(graph, "[1:a]anull[a0];[a0]anull[a]");
        assert!(!graph.contains("amix"));
    }

    #[test]
    fn video_only_has_no_audio_codec_or_filter() {
        let plan = FinalizePlan { video: Some(0), mic: None, system: None, ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(!a.contains(&"-filter_complex".to_string()) && !a.contains(&"-c:a".to_string()));
        assert!(has_seq(&a, &["-map", "0:v", "-c:v", "copy"]));
    }

    #[test]
    fn audio_only_still_produces_a_playable_file() {
        let plan = FinalizePlan { video: None, mic: Some(0), system: Some(0), ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(!a.contains(&"-c:v".to_string()));
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(graph.starts_with("[0:a]pan=stereo"), "{graph}");
    }

    #[test]
    fn the_duration_cap_goes_before_the_output_options() {
        let plan = FinalizePlan { video: Some(0), mic: Some(0), system: Some(0), duration_hns: Some(36_005_000_000), ..Default::default() };
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(has_seq(&a, &["-t", "3600.5000000", "-movflags", "+faststart"]), "{a:?}");
        let uncapped = s(&finalize_args(Path::new("D"), &FinalizePlan { video: Some(0), ..Default::default() }, 48_000).unwrap());
        assert!(!uncapped.contains(&"-t".to_string()));
    }

    fn session_with(stopped: Option<i64>, starts: [(TrackKind, Option<i64>); 3]) -> SessionMeta {
        let mut m = SessionMeta::new("x", "0", chrono::Utc::now(), 10_000_000);
        for (k, t0) in starts {
            m.tracks.set(k, TrackMeta::new(k.file_name(), t0));
        }
        m.stopped_hns = stopped;
        m
    }

    #[test]
    fn duration_is_the_stop_time_minus_the_earliest_start() {
        let m = session_with(Some(500_000_000), [(TrackKind::Video, Some(100_000_000)), (TrackKind::Mic, Some(100_300_000)), (TrackKind::System, Some(100_500_000))]);
        let p = FinalizePlan::from_meta(&m, |_| true);
        assert_eq!(p.duration_hns, Some(400_000_000));
        assert_eq!((p.video, p.mic, p.system), (Some(0), Some(300_000), Some(500_000)));
    }

    #[test]
    fn a_missing_track_file_does_not_move_the_origin() {
        // 视频文件不在：起点以仍然存在的最早一轨为准
        let m = session_with(Some(500_000_000), [(TrackKind::Video, Some(100_000_000)), (TrackKind::Mic, Some(120_000_000)), (TrackKind::System, Some(130_000_000))]);
        let p = FinalizePlan::from_meta(&m, |k| k != TrackKind::Video);
        assert_eq!(p.duration_hns, Some(380_000_000));
    }

    #[test]
    fn a_crashed_session_has_no_stop_time_and_is_not_trimmed() {
        let m = session_with(None, [(TrackKind::Video, Some(1)), (TrackKind::Mic, Some(2)), (TrackKind::System, None)]);
        assert_eq!(FinalizePlan::from_meta(&m, |_| true).duration_hns, None);
    }

    #[test]
    fn a_stop_time_before_the_start_never_goes_negative() {
        let m = session_with(Some(5), [(TrackKind::Video, Some(100)), (TrackKind::Mic, None), (TrackKind::System, None)]);
        assert_eq!(FinalizePlan::from_meta(&m, |_| true).duration_hns, Some(0));
    }

    fn seg(file: &str, offset_hns: i64, frames: Option<u64>) -> PlanSegment {
        PlanSegment { file: file.into(), offset_hns, frames }
    }

    #[test]
    fn segment_file_names_count_from_one() {
        assert_eq!(TrackKind::Video.segment_file_name(1), "video.mkv");
        assert_eq!(TrackKind::Video.segment_file_name(2), "video.2.mkv");
        assert_eq!(TrackKind::Mic.segment_file_name(3), "mic.3.mka");
        assert_eq!(TrackKind::System.segment_file_name(0), "system.mka");
    }

    #[test]
    fn a_restarted_track_carries_its_segments_into_the_plan_relative_to_the_earliest_start() {
        let mut m = session_with(Some(900_000_000), [(TrackKind::Video, Some(100_000_000)), (TrackKind::Mic, Some(100_200_000)), (TrackKind::System, None)]);
        let mic = m.tracks.mic.as_mut().unwrap();
        mic.segments = vec![
            crate::session::SegmentMeta { file: "mic.mka".into(), t0_hns: 100_200_000, frames: None },
            crate::session::SegmentMeta { file: "mic.2.mka".into(), t0_hns: 400_000_000, frames: None },
        ];
        let p = FinalizePlan::from_meta(&m, |k| k != TrackKind::System);
        assert_eq!(p.segments.get(&TrackKind::Mic), Some(&vec![seg("mic.mka", 200_000, None), seg("mic.2.mka", 300_000_000, None)]));
        assert!(!p.segments.contains_key(&TrackKind::Video), "没有重启过的轨道不带分段");
    }

    #[test]
    fn video_segments_are_joined_with_the_concat_demuxer_and_keep_the_track_offset() {
        let mut plan = FinalizePlan { video: Some(5_000_000), mic: Some(0), ..Default::default() };
        plan.segments.insert(TrackKind::Video, vec![seg("video.mkv", 5_000_000, Some(150)), seg("video.2.mkv", 105_000_000, None)]);
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(
            has_seq(&a, &["-itsoffset", "0.5000000", "-f", "concat", "-safe", "0", "-i", "D\\video.concat.txt"])
                || has_seq(&a, &["-itsoffset", "0.5000000", "-f", "concat", "-safe", "0", "-i", "D/video.concat.txt"]),
            "{a:?}"
        );
        assert!(has_seq(&a, &["-map", "0:v"]));
    }

    #[test]
    fn one_remaining_video_segment_is_read_directly_even_if_it_is_not_the_first_file() {
        let mut plan = FinalizePlan { video: Some(0), ..Default::default() };
        plan.segments.insert(TrackKind::Video, vec![seg("video.2.mkv", 0, None)]);
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        assert!(a.iter().any(|x| x.ends_with("video.2.mkv")) && !a.contains(&"concat".to_string()), "{a:?}");
    }

    #[test]
    fn audio_segments_are_delayed_individually_and_mixed_with_the_other_track() {
        let mut plan = FinalizePlan { mic: Some(0), system: Some(0), ..Default::default() };
        plan.segments.insert(TrackKind::Mic, vec![seg("mic.mka", 0, None), seg("mic.2.mka", 20_000_000, None)]); // 第二段晚 2 秒
        let a = s(&finalize_args(Path::new("D"), &plan, 48_000).unwrap());
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(graph, "[0:a]pan=stereo|c0=c0|c1=c0[a0];[1:a]pan=stereo|c0=c0|c1=c0,adelay=96000S:all=1[a1];[2:a]anull[a2];[a0][a1][a2]amix=inputs=3:normalize=0,alimiter=limit=0.95[a]");
    }

    #[test]
    fn the_concat_list_gives_exact_durations_for_all_but_the_last_segment() {
        let text = concat_list(&[seg("video.mkv", 0, Some(150)), seg("video.2.mkv", 0, Some(30)), seg("video.3.mkv", 0, None)], 15);
        assert_eq!(text, "ffconcat version 1.0\nfile 'video.mkv'\nduration 10.0000000\nfile 'video.2.mkv'\nduration 2.0000000\nfile 'video.3.mkv'\n");
    }
    #[test]
    fn nothing_to_mix_is_an_error() {
        assert_eq!(finalize_args(Path::new("D"), &FinalizePlan::default(), 48_000).unwrap_err(), ArgsError::NothingToMix);
    }

    #[test]
    fn delay_rounds_to_the_nearest_sample() {
        assert_eq!(delay_samples(0, 48_000), 0);
        assert_eq!(delay_samples(10_000_000, 48_000), 48_000);
        assert_eq!(delay_samples(104, 48_000), 0); // 约 0.5 个采样以内
        assert_eq!(delay_samples(105, 48_000), 1);
    }

    #[test]
    fn display_quotes_arguments_with_spaces_or_filter_syntax() {
        let d = display(&os(&["-i", "C:\\a b\\x.mkv", "-map", "[a]"]));
        assert_eq!(d, "-i \"C:\\a b\\x.mkv\" -map \"[a]\"");
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib ffmpeg_args::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/ffmpeg_args.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! ffmpeg 命令行的构造（设计文档 5.6、5.7、5.8）。只拼参数，不启动进程。
//!
//! - 写入：每轨一个进程，从 stdin 读原始数据，写 Matroska；`-flush_packets 1` 让崩溃时丢得最少。
//! - 收尾：视频直接拷贝，两条音轨按起点偏移对齐（`adelay`，精确到采样），等增益相加并限幅，编码为 AAC。
//!   不用 `-itsoffset` 对齐音频：`amix` 按到达顺序混合，不看时间戳，起点晚的音轨会被提前混进去。
//! - 修复：拷贝式重封装，重新生成索引。

use crate::clock::HNS_PER_SEC;
use crate::config::Encoder;
use crate::session::{MEETING_FILE, SessionMeta, TrackKind, Tracks};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArgsError {
    #[error("编码器还没有确定（\"auto\" 需要先探测）")]
    EncoderNotResolved,
    #[error("没有任何可用的轨道可以生成 {MEETING_FILE}")]
    NothingToMix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: Encoder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSpec {
    pub rate: u32,
    pub channels: u16,
}

const LOG: [&str; 5] = ["-hide_banner", "-nostats", "-loglevel", "warning", "-y"];

fn os(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn encoder_args(enc: Encoder, fps: u32) -> Result<Vec<OsString>, ArgsError> {
    let gop = (fps * 2).to_string();
    let mut a = match enc {
        Encoder::Auto => return Err(ArgsError::EncoderNotResolved),
        Encoder::Nvenc => os(&["-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "28", "-b:v", "0", "-bf", "0"]),
        Encoder::Qsv => os(&["-c:v", "h264_qsv", "-preset", "medium", "-global_quality", "28", "-look_ahead", "0", "-bf", "0"]),
        Encoder::Amf => os(&["-c:v", "h264_amf", "-quality", "balanced", "-rc", "cqp", "-qp_i", "28", "-qp_p", "28", "-bf", "0"]),
        Encoder::Software => os(&["-c:v", "libopenh264", "-b:v", "2M", "-pix_fmt", "yuv420p"]),
    };
    a.push("-g".into());
    a.push(gop.into());
    Ok(a)
}

/// 画面写入：stdin 收 NV12 原始帧，恒定帧率，输出标注为 BT.709 有限范围（与 GPU 转换的色彩空间一致）。
pub fn video_writer_args(spec: &VideoSpec, out: &Path) -> Result<Vec<OsString>, ArgsError> {
    let mut a = os(&LOG);
    a.extend(os(&["-f", "rawvideo", "-pix_fmt", "nv12", "-s"]));
    a.push(format!("{}x{}", spec.width, spec.height).into());
    a.extend(os(&["-framerate"]));
    a.push(spec.fps.to_string().into());
    a.extend(os(&["-i", "pipe:0", "-an"]));
    a.extend(encoder_args(spec.encoder, spec.fps)?);
    a.extend(os(&["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"]));
    a.extend(os(&["-fps_mode", "passthrough"]));
    a.extend(os(&MKV_CRASH_SAFE));
    a.push(out.as_os_str().to_owned());
    Ok(a)
}

/// Matroska 的 cluster 默认攒到 5 秒或 5 MB 才写盘，`-flush_packets` 冲不掉它；
/// 把 cluster 限制在 1 秒内，ffmpeg 被杀或断电时最多丢最后约 1 秒（实测见 record-mux 的集成测试）。
const MKV_CRASH_SAFE: [&str; 8] = ["-cluster_time_limit", "1000", "-cluster_size_limit", "1048576", "-flush_packets", "1", "-f", "matroska"];

/// 音轨写入：stdin 收交错 f32，FLAC 无损（24 位）。
pub fn audio_writer_args(spec: &AudioSpec, out: &Path) -> Vec<OsString> {
    let mut a = os(&LOG);
    a.extend(os(&["-f", "f32le", "-ar"]));
    a.push(spec.rate.to_string().into());
    a.push("-ac".into());
    a.push(spec.channels.to_string().into());
    a.extend(os(&["-i", "pipe:0", "-vn", "-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"]));
    a.extend(os(&MKV_CRASH_SAFE));
    a.push(out.as_os_str().to_owned());
    a
}

/// 探测编码器是否真能用：编一帧纯黑画面丢进空输出。
pub fn probe_args(enc: Encoder) -> Result<Vec<OsString>, ArgsError> {
    let mut a = os(&["-hide_banner", "-nostats", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=black:s=256x256:r=15", "-frames:v", "1"]);
    a.extend(encoder_args(enc, 15)?);
    a.extend(os(&["-f", "null", "-"]));
    Ok(a)
}

/// 数一个画面文件里有多少帧：拷贝到空输出，`-progress` 在结束时给出 `frame=N`。
/// 不用 ffprobe：安装包里只带 ffmpeg 一个可执行文件。
pub fn count_frames_args(file: &Path) -> Vec<OsString> {
    let mut a = os(&["-hide_banner", "-nostats", "-nostdin", "-loglevel", "error", "-i"]);
    a.push(file.as_os_str().to_owned());
    a.extend(os(&["-map", "0:v:0", "-c", "copy", "-f", "null", "-", "-progress", "pipe:2"]));
    a
}

/// 从 `-progress` 的输出里取最后一个 `frame=N`。
pub fn parse_progress_frames(text: &str) -> Option<u64> {
    text.lines().rev().find_map(|l| l.trim().strip_prefix("frame=")?.trim().parse().ok())
}

/// 崩溃后的拷贝式修复：读到哪算哪，重新写出完整的容器与索引。
pub fn repair_args(src: &Path, dst: &Path) -> Vec<OsString> {
    let mut a = os(&LOG);
    a.push("-i".into());
    a.push(src.as_os_str().to_owned());
    a.extend(os(&["-c", "copy", "-f", "matroska"]));
    a.push(dst.as_os_str().to_owned());
    a
}

/// 编码器重启过的轨道里的一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSegment {
    pub file: String,
    /// 相对最早起点的偏移（100 ns）。
    pub offset_hns: i64,
    /// 画面轨：本段的帧数，拼接时用它给出精确时长；最后一段为空。
    pub frames: Option<u64>,
}

/// 画面轨分段拼接时用的清单文件名（写在会话目录里）。
pub const VIDEO_CONCAT_FILE: &str = "video.concat.txt";

/// 收尾时每轨的起点偏移（100 ns），`None` 表示没有这一轨（或文件不存在）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FinalizePlan {
    pub video: Option<i64>,
    pub mic: Option<i64>,
    pub system: Option<i64>,
    /// 成品时长上限（100 ns），`None` 不裁剪。回环音轨在停止之后还会拖出约一个设备延迟的尾巴
    /// （S1 实测 10–560 ms，设备时间戳是“播放时刻”），按停止时刻裁掉，各轨才一样长。
    pub duration_hns: Option<i64>,
    /// 编码器重启过的轨道，各段按先后顺序。没有这一项的轨道只有默认文件名那一个文件。
    pub segments: BTreeMap<TrackKind, Vec<PlanSegment>>,
}

impl FinalizePlan {
    /// 按 `session.json` 里的起点计算偏移；`present` 判断该轨文件是否存在。不裁剪时长。
    pub fn from_tracks(tracks: &Tracks, present: impl Fn(TrackKind) -> bool) -> Self {
        let off = tracks.offsets_hns();
        let pick = |k: TrackKind| if present(k) { Some(off.get(&k).copied().unwrap_or(0)) } else { None };
        Self { video: pick(TrackKind::Video), mic: pick(TrackKind::Mic), system: pick(TrackKind::System), ..Default::default() }
    }

    /// 在 [`FinalizePlan::from_tracks`] 之上：记录了停止时刻的会话再按“停止时刻 − 最早起点”裁剪时长（崩溃的会话没有停止时刻，不裁剪）；
    /// 编码器重启过的轨道带上各段及其起点。调用方要先去掉空文件的段。
    pub fn from_meta(meta: &SessionMeta, present: impl Fn(TrackKind) -> bool) -> Self {
        let mut plan = Self::from_tracks(&meta.tracks, &present);
        let earliest = TrackKind::ALL.iter().filter(|&&k| present(k)).filter_map(|&k| meta.tracks.get(k).and_then(|t| t.t0_hns)).min();
        plan.duration_hns = meta.stopped_hns.zip(earliest).map(|(stop, t0)| (stop - t0).max(0));
        for kind in TrackKind::ALL {
            let Some(track) = meta.tracks.get(kind).filter(|t| present(kind) && !t.segments.is_empty()) else { continue };
            let base = earliest.unwrap_or(0);
            let segs = track.segments.iter().map(|s| PlanSegment { file: s.file.clone(), offset_hns: s.t0_hns - base, frames: s.frames }).collect();
            plan.segments.insert(kind, segs);
        }
        plan
    }
}

/// 临时输出名：生成完成后由调用方改名为 `meeting.mp4`，半成品不会被当成成品。
pub fn meeting_part_path(dir: &Path) -> PathBuf {
    dir.join(format!("{MEETING_FILE}.part"))
}

fn seconds(hns: i64) -> String {
    format!("{}.{:07}", hns / HNS_PER_SEC, hns % HNS_PER_SEC)
}

fn delay_samples(offset_hns: i64, rate: u32) -> i64 {
    ((i128::from(offset_hns) * i128::from(rate) + i128::from(HNS_PER_SEC) / 2) / i128::from(HNS_PER_SEC)) as i64
}

/// 生成 `meeting.mp4` 的命令。麦克风是单声道，复制到左右两路；系统音是立体声。
pub fn finalize_args(dir: &Path, plan: &FinalizePlan, audio_rate: u32) -> Result<Vec<OsString>, ArgsError> {
    if plan.video.is_none() && plan.mic.is_none() && plan.system.is_none() {
        return Err(ArgsError::NothingToMix);
    }
    let mut a = os(&LOG);
    let mut next_input = 0usize;
    let mut video_input = None;
    if let Some(off) = plan.video {
        a.push("-itsoffset".into());
        a.push(seconds(off).into());
        match plan.segments.get(&TrackKind::Video) {
            // 编码器重启过：各段用 concat 解复用器首尾相接（拷贝，不重新编码）
            Some(segs) if segs.len() > 1 => {
                a.extend(os(&["-f", "concat", "-safe", "0", "-i"]));
                a.push(dir.join(VIDEO_CONCAT_FILE).into_os_string());
            }
            // 只剩一段有内容（另一段是空的）：直接读那一个文件，它不一定叫 video.mkv
            Some(segs) if !segs.is_empty() => {
                a.push("-i".into());
                a.push(dir.join(&segs[0].file).into_os_string());
            }
            _ => {
                a.push("-i".into());
                a.push(dir.join(TrackKind::Video.file_name()).into_os_string());
            }
        }
        video_input = Some(next_input);
        next_input += 1;
    }
    // (输入序号, 是否单声道, 延迟采样数)。重启过的音轨每一段各算一路输入，各自按起点延迟，之后一起混：段与段之间不重叠，混完就是接起来。
    let mut audio: Vec<(usize, bool, i64)> = Vec::new();
    for (kind, off, mono) in [(TrackKind::Mic, plan.mic, true), (TrackKind::System, plan.system, false)] {
        let Some(off) = off else { continue };
        let inputs: Vec<(PathBuf, i64)> = match plan.segments.get(&kind) {
            Some(segs) if !segs.is_empty() => segs.iter().map(|s| (dir.join(&s.file), s.offset_hns)).collect(),
            _ => vec![(dir.join(kind.file_name()), off)],
        };
        for (path, offset) in inputs {
            a.push("-i".into());
            a.push(path.into_os_string());
            audio.push((next_input, mono, delay_samples(offset, audio_rate)));
            next_input += 1;
        }
    }

    let mut graph = String::new();
    for (n, &(input, mono, delay)) in audio.iter().enumerate() {
        let mut chain = format!("[{input}:a]");
        let mut filters: Vec<String> = Vec::new();
        if mono {
            filters.push("pan=stereo|c0=c0|c1=c0".into());
        }
        if delay > 0 {
            filters.push(format!("adelay={delay}S:all=1"));
        }
        if filters.is_empty() {
            filters.push("anull".into());
        }
        chain.push_str(&filters.join(","));
        chain.push_str(&format!("[a{n}];"));
        graph.push_str(&chain);
    }
    match audio.len() {
        0 => {}
        1 => graph.push_str("[a0]anull[a]"),
        n => {
            let labels: String = (0..n).map(|i| format!("[a{i}]")).collect();
            graph.push_str(&format!("{labels}amix=inputs={n}:normalize=0,alimiter=limit=0.95[a]"));
        }
    }

    if !audio.is_empty() {
        a.push("-filter_complex".into());
        a.push(graph.into());
    }
    if let Some(v) = video_input {
        a.push("-map".into());
        a.push(format!("{v}:v").into());
    }
    if !audio.is_empty() {
        a.push("-map".into());
        a.push("[a]".into());
    }
    if video_input.is_some() {
        a.extend(os(&["-c:v", "copy"]));
    }
    if !audio.is_empty() {
        a.extend(os(&["-c:a", "aac", "-b:a", "128k"]));
    }
    if let Some(d) = plan.duration_hns {
        a.push("-t".into());
        a.push(seconds(d).into());
    }
    a.extend(os(&["-movflags", "+faststart", "-f", "mp4"]));
    a.push(meeting_part_path(dir).into_os_string());
    Ok(a)
}

/// 画面轨分段拼接的 ffconcat 清单：除最后一段外都写明精确时长（帧数 ÷ 帧率），
/// 后一段才会正好接在前一段的第 N 帧之后，不依赖被强杀的那一段容器里估出来的时长。
pub fn concat_list(segs: &[PlanSegment], fps: u32) -> String {
    let mut text = String::from("ffconcat version 1.0\n");
    for (i, seg) in segs.iter().enumerate() {
        text.push_str(&format!("file '{}'\n", seg.file));
        if let Some(frames) = seg.frames.filter(|_| i + 1 < segs.len()) {
            text.push_str(&format!("duration {:.7}\n", frames as f64 / f64::from(fps)));
        }
    }
    text
}

/// 日志里打印命令用。
pub fn display(args: &[OsString]) -> String {
    args.iter()
        .map(|a| {
            let s = a.to_string_lossy();
            if s.contains(' ') || s.contains('[') || s.contains('|') { format!("\"{s}\"") } else { s.into_owned() }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib ffmpeg_args::tests::
```

预期：通过，输出里有 `test result: ok. 27 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): ffmpeg 命令行构造"
```

---

## 任务 15：record-core：limits

**目标：** 自动保护（设计文档 6.5）：单场时长上限、磁盘剩余空间下限。纯判定，采样由外壳每几秒做一次。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/limits.rs`

**接口：**

- 依赖：`crate::config::Limits`、`crate::session::EndReason`
- 提供：`pub fn check(elapsed_secs: u64, free_bytes: u64, limits: &Limits) -> Option<EndReason>`
- 测试：5 个：`healthy_sessions_are_left_alone`、`stops_exactly_at_the_time_limit`、`stops_below_the_free_space_floor`、`disk_beats_duration_when_both_apply`、`fractional_limits_work`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod limits;
```

- [ ] **步骤 2：创建 `crates/record-core/src/limits.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn healthy_sessions_are_left_alone() {
        assert_eq!(check(3_600, 100 * GB, &Limits::default()), None);
    }

    #[test]
    fn stops_exactly_at_the_time_limit() {
        let l = Limits::default();
        assert_eq!(check(6 * 3600 - 1, 100 * GB, &l), None);
        assert_eq!(check(6 * 3600, 100 * GB, &l), Some(EndReason::MaxDuration));
    }

    #[test]
    fn stops_below_the_free_space_floor() {
        let l = Limits::default();
        assert_eq!(check(10, 5 * GB, &l), None);
        assert_eq!(check(10, 5 * GB - 1, &l), Some(EndReason::DiskLow));
    }

    #[test]
    fn disk_beats_duration_when_both_apply() {
        assert_eq!(check(7 * 3600, GB, &Limits::default()), Some(EndReason::DiskLow));
    }

    #[test]
    fn fractional_limits_work() {
        let l = Limits { max_hours: 0.5, min_free_gb: 1.5 };
        assert_eq!(check(1_799, 2 * GB, &l), None);
        assert_eq!(check(1_800, 2 * GB, &l), Some(EndReason::MaxDuration));
        assert_eq!(check(0, GB, &l), Some(EndReason::DiskLow));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib limits::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/limits.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 自动保护（设计文档 6.5）：单场时长上限、磁盘剩余空间下限。纯判定，采样由外壳每几秒做一次。

use crate::config::Limits;
use crate::session::EndReason;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// 返回需要自动结束的原因。磁盘空间优先于时长：磁盘写满会损坏正在写的文件。
pub fn check(elapsed_secs: u64, free_bytes: u64, limits: &Limits) -> Option<EndReason> {
    if (free_bytes as f64) < limits.min_free_gb * GIB {
        return Some(EndReason::DiskLow);
    }
    if elapsed_secs as f64 >= limits.max_hours * 3600.0 {
        return Some(EndReason::MaxDuration);
    }
    None
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib limits::tests::
```

预期：通过，输出里有 `test result: ok. 5 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 自动保护判定（单场时长与磁盘剩余空间）"
```

---

## 任务 16：record-core：paths

**目标：** 配置、日志、模型的位置（设计文档 4.4）。保存目录默认在“视频”库下的 `Record`。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/paths.rs`

**接口：**

- 依赖：`crate::config::Config`
- 提供：`pub struct Paths`；`pub enum PathsError`；`Paths::from_dirs`；`Paths::from_env_values`；`Paths::from_env`；`pub fn resolve_output_dir(cfg: &Config, videos_dir: &Path) -> PathBuf`
- 测试：3 个：`locations_follow_the_spec`、`missing_environment_is_reported_by_name`、`output_dir_defaults_to_the_videos_library`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod paths;
```

- [ ] **步骤 2：创建 `crates/record-core/src/paths.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_follow_the_spec() {
        let p = Paths::from_dirs(Path::new(r"C:\Users\a\AppData\Roaming"), Path::new(r"C:\Users\a\AppData\Local"));
        assert_eq!(p.config_file, Path::new(r"C:\Users\a\AppData\Roaming\record\config.toml"));
        assert_eq!(p.log_dir, Path::new(r"C:\Users\a\AppData\Local\record\logs"));
        assert_eq!(p.models_dir, Path::new(r"C:\Users\a\AppData\Local\record\models"));
    }

    #[test]
    fn missing_environment_is_reported_by_name() {
        assert_eq!(Paths::from_env_values(None, Some("x".into())), Err(PathsError::MissingEnv("APPDATA")));
        assert_eq!(Paths::from_env_values(Some("x".into()), Some("".into())), Err(PathsError::MissingEnv("LOCALAPPDATA")));
        assert!(Paths::from_env_values(Some("a".into()), Some("b".into())).is_ok());
    }

    #[test]
    fn output_dir_defaults_to_the_videos_library() {
        let c = Config::default();
        assert_eq!(resolve_output_dir(&c, Path::new(r"D:\Videos")), Path::new(r"D:\Videos\Record"));
        let mut c = Config { output_dir: Some(r"E:\Records".into()), ..Config::default() };
        assert_eq!(resolve_output_dir(&c, Path::new(r"D:\Videos")), Path::new(r"E:\Records"));
        c.output_dir = Some("   ".into());
        assert_eq!(resolve_output_dir(&c, Path::new(r"D:\Videos")), Path::new(r"D:\Videos\Record"));
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib paths::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/paths.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 配置、日志、模型的位置（设计文档 4.4）。保存目录默认在“视频”库下的 `Record`。
//!
//! 视频库可能被重定向（OneDrive 等），所以由外壳用已知文件夹 API 解析后传进来，这里不猜。

use crate::config::Config;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_file: PathBuf,
    pub log_dir: PathBuf,
    pub models_dir: PathBuf,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathsError {
    #[error("环境变量 {0} 未设置，无法确定配置与日志位置")]
    MissingEnv(&'static str),
}

impl Paths {
    /// `roaming` 对应 `%APPDATA%`，`local` 对应 `%LOCALAPPDATA%`。
    pub fn from_dirs(roaming: &Path, local: &Path) -> Self {
        Self { config_file: roaming.join("record").join("config.toml"), log_dir: local.join("record").join("logs"), models_dir: local.join("record").join("models") }
    }

    pub fn from_env_values(appdata: Option<OsString>, localappdata: Option<OsString>) -> Result<Self, PathsError> {
        let roaming = appdata.filter(|v| !v.is_empty()).ok_or(PathsError::MissingEnv("APPDATA"))?;
        let local = localappdata.filter(|v| !v.is_empty()).ok_or(PathsError::MissingEnv("LOCALAPPDATA"))?;
        Ok(Self::from_dirs(Path::new(&roaming), Path::new(&local)))
    }

    pub fn from_env() -> Result<Self, PathsError> {
        Self::from_env_values(std::env::var_os("APPDATA"), std::env::var_os("LOCALAPPDATA"))
    }
}

/// 保存目录：配置里设置了就用配置的，否则是“视频”库下的 `Record`。
pub fn resolve_output_dir(cfg: &Config, videos_dir: &Path) -> PathBuf {
    match cfg.output_dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => videos_dir.join("Record"),
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib paths::tests::
```

预期：通过，输出里有 `test result: ok. 3 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 配置、日志、模型与保存目录的位置"
```

---

## 任务 17：record-core：recovery

**目标：** 崩溃恢复的计划（设计文档 5.8）。

**文件：**

- 修改：`crates/record-core/src/lib.rs` — 模块声明
- 创建：`crates/record-core/src/recovery.rs`

**接口：**

- 依赖：`crate::session::{MEETING_FILE, SessionMeta, SessionState, TrackKind}`
- 提供：`pub struct RecoveryPlan`；`RecoveryPlan::is_noop`；`pub fn plan(meta: &SessionMeta, exists: impl Fn(&str) -> bool) -> RecoveryPlan`
- 测试：9 个：`an_interrupted_session_repairs_every_track_that_exists_then_remuxes`、`missing_tracks_are_not_repaired`、`an_interrupted_session_with_no_data_is_only_marked`、`every_segment_of_a_restarted_track_is_repaired`、`a_segment_file_that_never_got_written_is_skipped`、`a_finalized_session_without_meeting_only_remuxes`、`a_recovered_session_without_meeting_remuxes_again`、`a_complete_session_needs_nothing`、`a_finalized_session_with_nothing_to_mix_is_left_alone`

- [ ] **步骤 1：在 `crates/record-core/src/lib.rs` 里声明模块**

加上这一行（`mod` 声明按模块名的字母顺序排）：

```rust
pub mod recovery;
```

- [ ] **步骤 2：创建 `crates/record-core/src/recovery.rs`**

先只写测试。这时模块里还没有实现，测试引用的函数和类型都不存在。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn meta(state: SessionState) -> SessionMeta {
        let mut m = SessionMeta::new("2026-10-13_203000", "0.1.0", Utc::now(), 10_000_000);
        m.state = state;
        m
    }

    fn has<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |n| names.contains(&n)
    }

    #[test]
    fn an_interrupted_session_repairs_every_track_that_exists_then_remuxes() {
        let p = plan(&meta(SessionState::Recording), has(&["video.mkv", "mic.mka", "system.mka", "session.json"]));
        assert_eq!(p, RecoveryPlan { repair: vec!["video.mkv".into(), "mic.mka".into(), "system.mka".into()], remux: true, mark_recovered: true });
    }

    #[test]
    fn missing_tracks_are_not_repaired() {
        let p = plan(&meta(SessionState::Recording), has(&["video.mkv", "system.mka"]));
        assert_eq!(p.repair, vec!["video.mkv", "system.mka"]);
        assert!(p.remux);
    }

    #[test]
    fn an_interrupted_session_with_no_data_is_only_marked() {
        let p = plan(&meta(SessionState::Recording), has(&["session.json"]));
        assert_eq!(p, RecoveryPlan { repair: vec![], remux: false, mark_recovered: true });
        assert!(!p.is_noop(), "状态要从 recording 改掉，否则每次启动都会再扫到它");
    }

    #[test]
    fn every_segment_of_a_restarted_track_is_repaired() {
        let mut m = meta(SessionState::Recording);
        let mut t = crate::session::TrackMeta::new("video.mkv", Some(1));
        t.segments = vec![crate::session::SegmentMeta { file: "video.mkv".into(), t0_hns: 1, frames: Some(40) }, crate::session::SegmentMeta { file: "video.2.mkv".into(), t0_hns: 9, frames: None }];
        m.tracks.set(TrackKind::Video, t);
        let p = plan(&m, has(&["video.mkv", "video.2.mkv", "mic.mka"]));
        assert_eq!(p.repair, vec!["video.mkv", "video.2.mkv", "mic.mka"]);
        assert!(p.remux);
    }

    #[test]
    fn a_segment_file_that_never_got_written_is_skipped() {
        let mut m = meta(SessionState::Recording);
        let mut t = crate::session::TrackMeta::new("mic.mka", Some(1));
        t.segments = vec![crate::session::SegmentMeta { file: "mic.mka".into(), t0_hns: 1, frames: None }, crate::session::SegmentMeta { file: "mic.2.mka".into(), t0_hns: 9, frames: None }];
        m.tracks.set(TrackKind::Mic, t);
        assert_eq!(plan(&m, has(&["mic.2.mka"])).repair, vec!["mic.2.mka"]);
    }

    #[test]
    fn a_finalized_session_without_meeting_only_remuxes() {
        let p = plan(&meta(SessionState::Finalized), has(&["video.mkv", "mic.mka"]));
        assert_eq!(p, RecoveryPlan { repair: vec![], remux: true, mark_recovered: false });
    }

    #[test]
    fn a_recovered_session_without_meeting_remuxes_again() {
        let p = plan(&meta(SessionState::Recovered), has(&["system.mka"]));
        assert!(p.remux && !p.mark_recovered && p.repair.is_empty());
    }

    #[test]
    fn a_complete_session_needs_nothing() {
        for state in [SessionState::Finalized, SessionState::Recovered] {
            assert!(plan(&meta(state), has(&["video.mkv", "mic.mka", "system.mka", "meeting.mp4"])).is_noop(), "{state:?}");
        }
    }

    #[test]
    fn a_finalized_session_with_nothing_to_mix_is_left_alone() {
        assert!(plan(&meta(SessionState::Finalized), has(&["session.json"])).is_noop());
    }
}
```

- [ ] **步骤 3：运行测试，确认它失败**

```powershell
cargo test -p record-core --lib recovery::tests::
```

预期：**失败**（编译错误：测试里用到的函数或类型还不存在）。

- [ ] **步骤 4：把实现补到 `crates/record-core/src/recovery.rs` 的最前面**

把下面这段放在文件最上面，和后面的 `#[cfg(test)]` 之间空一行：

```rust
//! 崩溃恢复的计划（设计文档 5.8）。
//!
//! 纯判定：根据 `session.json` 和目录里有哪些文件，决定要做什么。执行（调 ffmpeg、改文件）在 `record-engine`。
//! 启动时扫描保存目录，对每个会话目录算一份计划：
//! - `state=recording`：上次没有正常收尾。每个存在的轨道文件（编码器重启过的轨道含每一段）做拷贝式修复，标为 `recovered`，补做 `meeting.mp4`。
//! - `state=finalized` 或 `recovered` 但缺少 `meeting.mp4`：只补做 `meeting.mp4`。
//! - 没有 `session.json` 的目录不是我们的会话（或者元数据已损坏），不动它，由调用方报告。

use crate::session::{MEETING_FILE, SessionMeta, SessionState, TrackKind};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryPlan {
    /// 要做拷贝式修复的轨道文件名（存在的，含编码器重启后的各段）。
    pub repair: Vec<String>,
    /// 要（重新）生成 `meeting.mp4`。
    pub remux: bool,
    /// 完成后把会话标为 `recovered`。
    pub mark_recovered: bool,
}

impl RecoveryPlan {
    /// 什么都不用做。
    pub fn is_noop(&self) -> bool {
        self.repair.is_empty() && !self.remux && !self.mark_recovered
    }
}

/// `exists` 判断会话目录里是否有某个文件（按文件名）。元数据里没有登记的轨道，按默认文件名找。
pub fn plan(meta: &SessionMeta, exists: impl Fn(&str) -> bool) -> RecoveryPlan {
    let present: Vec<String> = TrackKind::ALL
        .iter()
        .flat_map(|&k| match meta.tracks.get(k) {
            Some(t) => t.files(),
            None => vec![k.file_name().to_string()],
        })
        .filter(|f| exists(f))
        .collect();
    let any = !present.is_empty();
    match meta.state {
        SessionState::Recording => RecoveryPlan { repair: present, remux: any, mark_recovered: true },
        SessionState::Finalized | SessionState::Recovered => RecoveryPlan { repair: Vec::new(), remux: any && !exists(MEETING_FILE), mark_recovered: false },
    }
}
```

- [ ] **步骤 5：运行并确认通过**

```powershell
cargo test -p record-core --lib recovery::tests::
```

预期：通过，输出里有 `test result: ok. 9 passed`。

- [ ] **步骤 6：提交**

```powershell
git add -A
git commit -m "feat(core): 崩溃恢复计划"
```

---

## 任务 18：质量门：格式、clippy、全部测试

**目标：** `record-core` 到此完整。在整个工作区上过一遍格式、clippy 和全部测试；之后每份计划结束时都要重复这个质量门。

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

预期：`record-core` 一行 `test result: ok. 195 passed; 0 failed`，没有 `FAILED`。三项都过的话工作区是干净的，没有东西要提交；`cargo fmt --check` 报差异时，先 `cargo fmt --all`，再 `git commit -am "style: cargo fmt"`。

---
