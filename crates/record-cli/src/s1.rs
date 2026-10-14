//! S1 音频 spike 的工具集：设备列表、测试音、采集（回环方案 A / A2 / C，麦克风，保活，时间源）、分析、自检。

use crate::args::Args;
use crate::packets;
use crate::wav::{self, WavWriter};
use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use record_capture::audio::{AudioEvent, AudioSource, Endpoint, Kind, PacketRecord, SourceConfig, SourceResult};
use record_capture::com::ComGuard;
use record_capture::devices::{self, Flow, RenderMeters, Role};
use record_capture::qpc::Qpc;
use record_capture::render::RenderPlayer;
use record_core::config::Config;
use record_core::dsp::{Burst, Spacing, detect_bursts, spacing};
use record_core::health::{SilenceWatchdog, WatchdogConfig, WatchdogEvent};
use record_core::timeline::{AudioFormat, TimeSource};
use std::f64::consts::PI;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const RATE: u32 = 48_000;

// ---------------------------------------------------------------------------
// 测试音
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct BeepSpec {
    pub interval_s: f64,
    pub burst_s: f64,
    pub freq: f64,
    pub amp: f32,
    /// 开始播放后多久出第一个脉冲。
    pub lead_s: f64,
}

impl Default for BeepSpec {
    fn default() -> Self {
        // 回环取的是数字信号，不受音箱音量影响，所以可以放得很轻。
        // 16 kHz：成年人基本听不见（不打扰人），而且音乐、人声、系统提示音在这个频段几乎没有能量（不容易被环境声污染）。
        Self { interval_s: 1.0, burst_s: 0.15, freq: 16_000.0, amp: 0.05, lead_s: 0.5 }
    }
}

/// 从命令行读测试音参数：`--interval`（秒）、`--freq`（Hz）。
pub fn beep_spec(a: &Args) -> Result<BeepSpec> {
    let d = BeepSpec::default();
    Ok(BeepSpec { interval_s: a.num("interval", d.interval_s)?, freq: a.num("freq", d.freq)?, ..d })
}

/// 测试音必须落在播放设备的带宽内，否则引擎重采样时会把它滤掉，表现为“全部漏录”。
pub fn check_beep_bandwidth(freq: f64) -> Result<()> {
    let _com = ComGuard::init()?;
    let en = devices::enumerator()?;
    let dev = devices::default_endpoint(&en, Flow::Render, Role::Console)?;
    let rate = devices::mix_format(&dev)?.rate;
    if freq >= f64::from(rate) * 0.45 {
        bail!("默认播放设备的混音采样率是 {rate} Hz，测试音 {freq} Hz 已接近或超过它的带宽，请用 --freq 指定更低的频率（例如 {:.0}）", f64::from(rate) * 0.3);
    }
    Ok(())
}

/// 周期性的短促正弦脉冲（两端 5 ms 淡入淡出）。每个脉冲播完时 `completed` 加一。
pub fn beep_generator(spec: BeepSpec, channels: u16, completed: Arc<AtomicU32>) -> impl FnMut(&mut [f32]) + Send + 'static {
    let mut n: u64 = 0;
    let mut in_burst = false;
    move |buf: &mut [f32]| {
        for frame in buf.chunks_exact_mut(usize::from(channels)) {
            let t = n as f64 / f64::from(RATE);
            n += 1;
            let mut v = 0.0f32;
            if t >= spec.lead_s {
                let p = (t - spec.lead_s) % spec.interval_s;
                if p < spec.burst_s {
                    let env = (p / 0.005).min(1.0).min(((spec.burst_s - p) / 0.005).min(1.0)) as f32;
                    v = spec.amp * env * (2.0 * PI * spec.freq * p).sin() as f32;
                    in_burst = true;
                } else if in_burst {
                    in_burst = false;
                    completed.fetch_add(1, Ordering::Relaxed);
                }
            }
            frame.fill(v);
        }
    }
}

// ---------------------------------------------------------------------------
// 设备列表
// ---------------------------------------------------------------------------

pub fn devices_cmd() -> Result<()> {
    let _com = ComGuard::init()?;
    let en = devices::enumerator()?;
    println!("== 默认设备 ==");
    for (flow, fname) in [(Flow::Render, "播放"), (Flow::Capture, "录音")] {
        for (role, rname) in [(Role::Console, "默认"), (Role::Communications, "默认通信")] {
            match devices::default_endpoint(&en, flow, role) {
                Ok(d) => {
                    let i = devices::info(&d)?;
                    let mix = devices::mix_format(&d)
                        .map(|m| format!("{} Hz {} 声道 {} 位{}", m.rate, m.channels, m.bits, if m.float { " 浮点" } else { "" }))
                        .unwrap_or_else(|e| format!("取格式失败：{e}"));
                    println!("{fname} / {rname}：{}\n    id={}\n    混音格式：{mix}", i.name, i.id);
                }
                Err(e) => println!("{fname} / {rname}：无（{e}）"),
            }
        }
    }
    println!("\n== 全部活动设备 ==");
    for (flow, fname) in [(Flow::Render, "播放"), (Flow::Capture, "录音")] {
        for d in devices::active_endpoints(&en, flow)? {
            let i = devices::info(&d)?;
            let peak = devices::Meter::open(&d).map(|m| format!("{:.3}", m.peak())).unwrap_or_else(|_| "-".into());
            println!("[{fname}] {}  电平 {peak}\n    id={}", i.name, i.id);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 采集
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum SystemSel {
    None,
    Default(Role),
    /// 每个活动播放设备各开一路回环（方案 C 的采集部分，叠加由后续任务实现）。
    All,
    Id(String),
}

#[derive(Debug, Clone)]
pub enum MicSel {
    None,
    Default(Role),
    Id(String),
}

#[derive(Clone)]
pub struct CaptureOpts {
    pub secs: u64,
    pub out_dir: PathBuf,
    pub system: SystemSel,
    pub mic: MicSel,
    pub keepalive: bool,
    pub system_time: TimeSource,
    pub mic_time: TimeSource,
    pub play: Option<BeepSpec>,
    pub verbose: bool,
}

struct Running {
    label: String,
    file: PathBuf,
    /// 采集线程只往这里入队，写盘在独立线程里做（和生产实现一致，磁盘抖动不会进到采集结果里）。
    wav_tx: Sender<Vec<f32>>,
    wav_join: JoinHandle<std::io::Result<u64>>,
    src: AudioSource,
    /// 返回设备时间戳相对到达时间的中位数（毫秒）。
    csv: JoinHandle<Option<f64>>,
    is_system: bool,
}

pub struct TrackOutcome {
    pub label: String,
    pub file: PathBuf,
    pub frames: u64,
    pub result: SourceResult,
    /// 设备时间戳 − 到达时间 的中位数（毫秒）。回环是播放延迟（正值，设备缓冲越大越大），麦克风是采集缓冲（负值）。
    pub lag_ms: Option<f64>,
}

pub struct CaptureOutcome {
    pub tracks: Vec<TrackOutcome>,
    pub watchdog_alerts: u32,
    pub beeps_completed: u32,
    pub events: Vec<String>,
}

fn csv_writer(path: PathBuf, rx: Receiver<PacketRecord>) -> JoinHandle<Option<f64>> {
    thread::spawn(move || {
        let mut f = fs::File::create(&path).ok()?;
        let _ = writeln!(f, "device_ms,arrival_ms,frames,flags");
        let mut lags: Vec<i64> = Vec::new();
        for r in rx {
            lags.push(r.device_qpc - r.arrival_qpc);
            let flags = u8::from(r.flags.silent) | (u8::from(r.flags.timestamp_error) << 1) | (u8::from(r.flags.discontinuity) << 2);
            let _ = writeln!(f, "{:.3},{:.3},{},{}", r.device_qpc as f64 / 10_000.0, r.arrival_qpc as f64 / 10_000.0, r.frames, flags);
        }
        lags.sort_unstable();
        lags.get(lags.len() / 2).map(|&l| l as f64 / 10_000.0)
    })
}

fn describe(e: &AudioEvent) -> String {
    match e {
        AudioEvent::Opened { label, device, .. } => format!("[{label}] 已打开：{}", device.name),
        AudioEvent::OpenFailed { label, error } => format!("[{label}] 打开失败：{error}"),
        AudioEvent::DeviceChanged { label, from, to } => format!("[{label}] 默认设备变化：{from} → {to}"),
        AudioEvent::StreamLost { label, error } => format!("[{label}] 流中断：{error}"),
        AudioEvent::Interrupted { label } => format!("[{label}] 超过 10 秒未恢复，标记中断"),
        AudioEvent::Recovered { label } => format!("[{label}] 已恢复"),
        AudioEvent::LargeGap { label, filled_ms } => format!("[{label}] 单次补静音 {filled_ms} ms（large_gap）"),
    }
}

pub fn run_capture(o: &CaptureOpts) -> Result<CaptureOutcome> {
    fs::create_dir_all(&o.out_dir).with_context(|| format!("创建目录 {}", o.out_dir.display()))?;
    let _com = ComGuard::init()?;
    let en = devices::enumerator()?;
    let qpc = Qpc::new();
    let cfg = Config::default();
    let (ev_tx, ev_rx) = unbounded::<AudioEvent>();
    let mut running: Vec<Running> = Vec::new();

    let mut start = |label: String, kind: Kind, endpoint: Endpoint, channels: u16, ts: TimeSource, keepalive: bool, is_system: bool| -> Result<()> {
        let file = o.out_dir.join(format!("{label}.wav"));
        let writer = WavWriter::create(&file, channels, RATE)?;
        let (wav_tx, wav_rx) = unbounded::<Vec<f32>>();
        let wav_join = thread::spawn(move || {
            let mut w = writer;
            for chunk in wav_rx {
                w.write(&chunk)?;
            }
            w.finish()
        });
        let (ptx, prx) = unbounded();
        let csv = csv_writer(o.out_dir.join(format!("packets_{label}.csv")), prx);
        let mut sc = SourceConfig::new(label.clone(), kind, endpoint, cfg.timeline_config(AudioFormat { rate: RATE, channels }, ts));
        sc.keepalive = keepalive;
        sc.packet_tap = Some(ptx);
        let sink_tx = wav_tx.clone();
        let src = AudioSource::start(
            sc,
            qpc,
            ev_tx.clone(),
            Box::new(move |s| {
                let _ = sink_tx.send(s.to_vec());
            }),
        )?;
        running.push(Running { label, file, wav_tx, wav_join, src, csv, is_system });
        Ok(())
    };

    match &o.system {
        SystemSel::None => {}
        SystemSel::Default(role) => start("system".into(), Kind::Loopback, Endpoint::Default(*role), 2, o.system_time, o.keepalive, true)?,
        SystemSel::Id(id) => start("system".into(), Kind::Loopback, Endpoint::Id(id.clone()), 2, o.system_time, o.keepalive, true)?,
        SystemSel::All => {
            for (n, d) in devices::active_endpoints(&en, Flow::Render)?.iter().enumerate() {
                let i = devices::info(d)?;
                println!("回环 #{n}：{}", i.name);
                start(format!("system_{n}"), Kind::Loopback, Endpoint::Id(i.id), 2, o.system_time, o.keepalive, true)?;
            }
        }
    }
    match &o.mic {
        MicSel::None => {}
        MicSel::Default(role) => start("mic".into(), Kind::Mic, Endpoint::Default(*role), 1, o.mic_time, false, false)?,
        MicSel::Id(id) => start("mic".into(), Kind::Mic, Endpoint::Id(id.clone()), 1, o.mic_time, false, false)?,
    }
    if running.is_empty() {
        bail!("没有选择任何采集源");
    }

    let meters = RenderMeters::open_all(&en)?;
    let mut wd = SilenceWatchdog::new(WatchdogConfig::default());
    let mut events: Vec<String> = Vec::new();
    let mut alerts = 0u32;
    let completed = Arc::new(AtomicU32::new(0));
    let mut player: Option<RenderPlayer> = None;
    let drain = |events: &mut Vec<String>| {
        while let Ok(e) = ev_rx.try_recv() {
            let s = describe(&e);
            println!("  {s}");
            events.push(s);
        }
    };

    // 先采集 1 秒再开始播放，保证每个脉冲都落在采集窗口里
    thread::sleep(Duration::from_secs(1));
    if let Some(spec) = o.play {
        player = Some(RenderPlayer::start(None, Role::Console, RATE, 2, beep_generator(spec, 2, Arc::clone(&completed))).map_err(anyhow::Error::msg)?);
    }
    for sec in 1..=o.secs {
        thread::sleep(Duration::from_secs(1));
        let sys_peak = running.iter().filter(|r| r.is_system).map(|r| r.src.live().take_peak()).fold(0.0f32, f32::max);
        let mic_peak = running.iter().filter(|r| !r.is_system).map(|r| r.src.live().take_peak()).fold(0.0f32, f32::max);
        let meter = meters.max_peak();
        if let Some(ev) = wd.tick(meter, sys_peak, true) {
            let who = meters.loudest().map(|(n, p)| format!("{n}（电平 {p:.2}）")).unwrap_or_default();
            let line = match ev {
                WatchdogEvent::Alert => {
                    alerts += 1;
                    format!("！静默失效告警：播放设备有声而系统音轨连续静音 —— {who}")
                }
                WatchdogEvent::Recovered => "静默失效已恢复".to_string(),
            };
            println!("  [{sec:>3}s] {line}");
            events.push(line);
        }
        if o.verbose {
            println!("  [{sec:>3}s] 端点电平 {:.3}  系统音轨 {sys_peak:.3}  麦克风轨 {mic_peak:.3}", meter.unwrap_or(-1.0));
        }
        drain(&mut events);
    }
    if let Some(p) = player.take() {
        p.stop();
    }
    thread::sleep(Duration::from_secs(1));

    let mut tracks = Vec::new();
    for r in running {
        let result = r.src.stop(); // 采集线程结束后，它持有的发送端随之释放
        drop(r.wav_tx); // 关掉最后一个发送端，写盘线程收尾
        let frames = r.wav_join.join().map_err(|_| anyhow::anyhow!("WAV 写盘线程崩溃"))??;
        let lag_ms = r.csv.join().ok().flatten();
        tracks.push(TrackOutcome { label: r.label, file: r.file, frames, result, lag_ms });
    }
    drain(&mut events);
    Ok(CaptureOutcome { tracks, watchdog_alerts: alerts, beeps_completed: completed.load(Ordering::Relaxed), events })
}

pub fn outcome_json(o: &CaptureOutcome) -> serde_json::Value {
    serde_json::json!({
        "watchdog_alerts": o.watchdog_alerts,
        "beeps_completed": o.beeps_completed,
        "events": o.events,
        "tracks": o.tracks.iter().map(|t| serde_json::json!({
            "label": t.label,
            "file": t.file.display().to_string(),
            "frames": t.frames,
            "seconds": t.frames as f64 / f64::from(RATE),
            "anchor_ms": t.result.anchor as f64 / 10_000.0,
            "device_lag_ms": t.lag_ms,
            "device_changes": t.result.device_changes,
            "reconnects": t.result.reconnects,
            "timeline": t.result.stats,
        })).collect::<Vec<_>>(),
    })
}

pub fn default_out_dir() -> PathBuf {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    std::env::temp_dir().join("record-s1").join(secs.to_string())
}

fn role_of(s: &str) -> Option<Role> {
    match s {
        "console" => Some(Role::Console),
        "communications" => Some(Role::Communications),
        _ => None,
    }
}

fn time_of(s: Option<&str>, default: TimeSource) -> Result<TimeSource> {
    Ok(match s {
        None => default,
        Some("device") => TimeSource::DeviceQpc,
        Some("arrival") => TimeSource::ArrivalMinusDuration,
        Some(o) => bail!("时间源只能是 device 或 arrival，收到“{o}”"),
    })
}

pub fn opts_from_args(a: &Args) -> Result<CaptureOpts> {
    let system = match a.get("system").unwrap_or("console") {
        "none" => SystemSel::None,
        "all" => SystemSel::All,
        r if role_of(r).is_some() => SystemSel::Default(role_of(r).unwrap()),
        id => SystemSel::Id(id.to_string()),
    };
    let mic = match a.get("mic").unwrap_or("communications") {
        "none" => MicSel::None,
        r if role_of(r).is_some() => MicSel::Default(role_of(r).unwrap()),
        id => MicSel::Id(id.to_string()),
    };
    Ok(CaptureOpts {
        secs: a.num("secs", 20u64)?,
        out_dir: a.get("out").map(PathBuf::from).unwrap_or_else(default_out_dir),
        system,
        mic,
        keepalive: a.flag("keepalive"),
        system_time: time_of(a.get("system-time"), TimeSource::DeviceQpc)?,
        mic_time: time_of(a.get("mic-time"), TimeSource::ArrivalMinusDuration)?,
        play: if a.flag("play") { Some(beep_spec(a)?) } else { None },
        verbose: a.flag("verbose"),
    })
}

pub fn capture_cmd(a: &Args) -> Result<()> {
    let o = opts_from_args(a)?;
    println!("输出目录：{}", o.out_dir.display());
    let out = run_capture(&o)?;
    let json = serde_json::to_string_pretty(&outcome_json(&out))?;
    fs::write(o.out_dir.join("report.json"), &json)?;
    println!("{json}");
    Ok(())
}

// ---------------------------------------------------------------------------
// 测试音与分析
// ---------------------------------------------------------------------------

pub fn beeps_cmd(a: &Args) -> Result<()> {
    let _com = ComGuard::init()?;
    let secs = a.num("secs", 20u64)?;
    let spec = beep_spec(a)?;
    let role = role_of(a.get("role").unwrap_or("console")).context("--role 只能是 console 或 communications")?;
    let completed = Arc::new(AtomicU32::new(0));
    let p = RenderPlayer::start(a.get("device").map(String::from), role, RATE, 2, beep_generator(spec, 2, Arc::clone(&completed))).map_err(anyhow::Error::msg)?;
    println!("正在播放测试音 {secs} 秒（{} Hz，每 {} 秒一次）", spec.freq, spec.interval_s);
    thread::sleep(Duration::from_secs(secs));
    p.stop();
    println!("播完 {} 个脉冲", completed.load(Ordering::Relaxed));
    Ok(())
}

pub struct BeepAnalysis {
    pub bursts: Vec<Burst>,
    pub spacing: Option<Spacing>,
    pub seconds: f64,
    pub peak: f32,
    /// 测试音开始前，同一频率上已有的环境声音电平。接近检测阈值时，检测结果不可信。
    pub baseline: f64,
}

/// 采集比测试音早 1 秒开始，测试音的第一个脉冲在开始播放后 `lead_s` 秒：这之前的内容都是环境声音。
pub fn analyze_file(path: &Path, spec: &BeepSpec) -> Result<BeepAnalysis> {
    let w = wav::read(path)?;
    let mono = w.mono();
    let peak = mono.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let threshold = f64::from(spec.amp) * 0.5;
    let quiet_end = ((1.0 + spec.lead_s - 0.2) * f64::from(w.rate)) as usize;
    let win = (w.rate / 100) as usize;
    let baseline = (0..quiet_end.min(mono.len()).saturating_sub(win)).step_by(win).map(|i| record_core::dsp::tone_level(&mono[i..i + win], spec.freq, w.rate)).fold(0.0, f64::max);
    let bursts = detect_bursts(&mono, w.rate, spec.freq, threshold);
    Ok(BeepAnalysis { spacing: spacing(&bursts, spec.interval_s), bursts, seconds: w.frames() as f64 / f64::from(w.rate), peak, baseline })
}

/// 逐 100 ms 打印指定频率上的最大电平，用来直接看一条录音里测试音到底在不在。
pub fn levels_cmd(a: &Args) -> Result<()> {
    let file = a.pos.get(2).context("用法：s1 levels <wav 文件> [--freq 16000] [--from 0] [--to 8]")?;
    let freq: f64 = a.num("freq", BeepSpec::default().freq)?;
    let w = wav::read(Path::new(file))?;
    let mono = w.mono();
    let win = (w.rate / 100) as usize;
    let (from, to): (f64, f64) = (a.num("from", 0.0)?, a.num("to", mono.len() as f64 / f64::from(w.rate))?);
    let step = (w.rate / 10) as usize;
    let mut t = from;
    while t < to {
        let start = (t * f64::from(w.rate)) as usize;
        let end = (start + step).min(mono.len());
        let level = (start..end.saturating_sub(win)).step_by(win / 2).map(|i| record_core::dsp::tone_level(&mono[i..i + win], freq, w.rate)).fold(0.0, f64::max);
        let peak = mono[start.min(mono.len())..end].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        println!("{t:>6.1}s  {freq:.0} Hz 电平 {level:.4}  采样峰值 {peak:.3}");
        t += 0.1;
    }
    Ok(())
}

pub fn analyze_cmd(a: &Args) -> Result<()> {
    let file = a.pos.get(2).context("用法：s1 analyze <wav 文件> [--interval 1.0] [--freq 16000]")?;
    let spec = beep_spec(a)?;
    let r = analyze_file(Path::new(file), &spec)?;
    println!("时长 {:.2} 秒，峰值 {:.3}，检测到 {} 个脉冲", r.seconds, r.peak, r.bursts.len());
    if let Some(s) = r.spacing {
        println!("平均间隔 {:.4} 秒，最大偏差 {:.1} ms，漂移 {:+.0} ppm", s.mean_interval_s, s.max_dev_ms, s.drift_ppm);
    }
    Ok(())
}

pub fn packets_cmd(a: &Args) -> Result<()> {
    let file = a.pos.get(2).context("用法：s1 packets <packets_*.csv>")?;
    let recs = packets::parse_csv(&fs::read_to_string(file)?);
    match packets::analyze(&recs, RATE) {
        None => println!("包太少（{} 个），无法分析", recs.len()),
        Some(s) => println!(
            "{} 个包，{} 段\n设备时间戳：抖动 {:.3} ms，漂移 {:+.1} ppm\n到达−时长：抖动 {:.3} ms，漂移 {:+.1} ppm",
            s.packets, s.segments, s.device.jitter_ms, s.device.drift_ppm, s.arrival.jitter_ms, s.arrival.drift_ppm
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 480 帧一块调用生成器，和真实播放一致。
    fn render(spec: BeepSpec, seconds: f64) -> (Vec<f32>, u32) {
        let completed = Arc::new(AtomicU32::new(0));
        let mut g = beep_generator(spec, 2, Arc::clone(&completed));
        let mut buf = vec![0.0f32; (seconds * f64::from(RATE)) as usize * 2];
        for chunk in buf.chunks_mut(480 * 2) {
            g(chunk);
        }
        let mono = buf.chunks_exact(2).map(|f| f[0]).collect();
        (mono, completed.load(Ordering::Relaxed))
    }

    #[test]
    fn every_burst_has_the_requested_amplitude_at_every_frequency() {
        for freq in [1_000.0, 4_000.0, 8_000.0, 12_000.0, 16_000.0] {
            let spec = BeepSpec { freq, ..Default::default() };
            let (mono, completed) = render(spec, 3.0);
            let peak = mono.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > spec.amp * 0.8, "{freq} Hz：峰值只有 {peak}"); // 采样点未必落在波峰上，8 kHz 时是 0.866 倍
            let bursts = detect_bursts(&mono, RATE, freq, f64::from(spec.amp) * 0.5);
            assert_eq!((bursts.len(), completed), (3, 3), "{freq} Hz");
        }
    }
}
