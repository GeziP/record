//! WASAPI 采集线程（设计文档 5.3、5.4、5.5、9）。
//!
//! 一个线程负责一路音频：打开设备、事件驱动取包、QPC 时间线修复、设备变化与丢失后的重连。
//! 输出是已经补过静音、修正过漂移的交错 f32，由调用方的回调接走（写入 ffmpeg，或测试时写 WAV）。

use crate::com::ComGuard;
use crate::devices::{self, EndpointInfo, Flow, MixFormat, Role};
use crate::qpc::Qpc;
use crate::render::{RenderPlayer, float_format};
use crossbeam_channel::Sender;
use record_core::clock::{Hns, ms_to_hns};
use record_core::timeline::{AudioPacket, AudioTimeline, PacketFlags, Report, TimelineConfig, TimelineStats};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient, IAudioClient, IMMDevice,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject};
use windows::core::w;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 回环：采集播放设备正在播放的声音。
    Loopback,
    Mic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// 跟随系统默认设备（指定角色），默认设备变化时自动重连。
    Default(Role),
    Id(String),
}

/// 采集到的每个原始包的记录，S1 用来比较各种时间源的抖动与漂移。
#[derive(Debug, Clone, Copy)]
pub struct PacketRecord {
    pub device_qpc: Hns,
    pub arrival_qpc: Hns,
    pub frames: u32,
    pub flags: PacketFlags,
}

#[derive(Clone)]
pub struct SourceConfig {
    pub label: String,
    pub kind: Kind,
    pub endpoint: Endpoint,
    pub timeline: TimelineConfig,
    /// 设备丢失后每隔多久重试（设计文档 9：3 秒）。
    pub reconnect_interval: Duration,
    /// 持续无法恢复多久后标记“中断”（设计文档 9：10 秒）。
    pub interrupted_after: Duration,
    /// 回环：另开一路静音渲染流保活（S1 对比后决定）。
    pub keepalive: bool,
    pub packet_tap: Option<Sender<PacketRecord>>,
}

impl SourceConfig {
    pub fn new(label: impl Into<String>, kind: Kind, endpoint: Endpoint, timeline: TimelineConfig) -> Self {
        Self { label: label.into(), kind, endpoint, timeline, reconnect_interval: Duration::from_secs(3), interrupted_after: Duration::from_secs(10), keepalive: false, packet_tap: None }
    }
}

#[derive(Debug, Clone)]
pub enum AudioEvent {
    Opened {
        label: String,
        device: EndpointInfo,
        mix: Option<MixFormat>,
    },
    OpenFailed {
        label: String,
        error: String,
    },
    DeviceChanged {
        label: String,
        from: String,
        to: String,
    },
    StreamLost {
        label: String,
        error: String,
    },
    /// 超过 `interrupted_after` 仍没有恢复。
    Interrupted {
        label: String,
    },
    Recovered {
        label: String,
    },
    LargeGap {
        label: String,
        filled_ms: u64,
    },
}

/// 运行中随时可读的状态。
#[derive(Debug, Default)]
pub struct Live {
    peak_bits: AtomicU32,
    frames_out: AtomicU64,
    last_packet_hns: AtomicI64,
    anchor_hns: AtomicI64,
}

impl Live {
    /// 取走并清零自上次调用以来的峰值（0.0–1.0）。
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak_bits.swap(0, Ordering::AcqRel))
    }

    pub fn frames_out(&self) -> u64 {
        self.frames_out.load(Ordering::Relaxed)
    }

    /// 最近一次收到数据包的时刻，没收到过为 0。
    pub fn last_packet_hns(&self) -> Hns {
        self.last_packet_hns.load(Ordering::Relaxed)
    }

    /// 本轨时间线的起点（采集线程启动时的 QPC）。线程还没跑起来时为 0。
    /// 崩溃恢复要靠它给各轨对齐，所以录制中就要能读到，不能等到停止。
    pub fn anchor_hns(&self) -> Hns {
        self.anchor_hns.load(Ordering::Acquire)
    }

    fn publish_peak(&self, p: f32) {
        // 非负 f32 的位模式与数值同序，可以直接取最大值
        self.peak_bits.fetch_max(p.to_bits(), Ordering::AcqRel);
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceResult {
    pub stats: TimelineStats,
    pub anchor: Hns,
    pub device_changes: u32,
    pub reconnects: u32,
}

pub struct AudioSource {
    stop: Arc<AtomicBool>,
    /// 外部请求停止的时刻（QPC，100 ns）。0 表示还没请求过。
    /// 时间线要补齐到这一刻，而不是线程真正退出的那一刻：拆流可能要几十到几百毫秒，不该算进录音时长。
    stop_at: Arc<AtomicI64>,
    qpc: Qpc,
    live: Arc<Live>,
    thread: Option<JoinHandle<SourceResult>>,
}

/// 采集线程交付一块时间线上的音频（交错的 f32）时调用的回调。
pub type AudioSink = Box<dyn FnMut(&[f32]) + Send>;

impl AudioSource {
    /// 启动线程。`sink` 在采集线程上被调用，要尽快返回（只入队，不做 IO）。
    pub fn start(cfg: SourceConfig, qpc: Qpc, events: Sender<AudioEvent>, sink: AudioSink) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_at = Arc::new(AtomicI64::new(0));
        let live = Arc::new(Live::default());
        let (s, at, l) = (Arc::clone(&stop), Arc::clone(&stop_at), Arc::clone(&live));
        let thread = thread::Builder::new().name(format!("audio-{}", cfg.label)).spawn(move || run(cfg, qpc, events, sink, &s, &at, &l))?;
        Ok(Self { stop, stop_at, qpc, live, thread: Some(thread) })
    }

    /// 记下请求停止的时刻（只记第一次），再通知线程退出。不等线程结束；
    /// 多轨一起停时先对每一轨调用它，各轨的“停止时刻”才几乎相同，再逐个 `stop()` 收结果。
    pub fn request_stop(&self) {
        let _ = self.stop_at.compare_exchange(0, self.qpc.now_hns(), Ordering::AcqRel, Ordering::Acquire);
        self.stop.store(true, Ordering::Release);
    }

    pub fn live(&self) -> &Arc<Live> {
        &self.live
    }

    /// 停止采集：时间线补齐到停止时刻，返回统计。
    pub fn stop(mut self) -> SourceResult {
        self.request_stop();
        self.thread.take().map(|t| t.join().unwrap_or_default()).unwrap_or_default()
    }
}

impl Drop for AudioSource {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Mmcss(HANDLE);

impl Mmcss {
    fn enter() -> Option<Self> {
        let mut idx = 0u32;
        unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut idx).ok().map(Self) }
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.0);
        }
    }
}

enum StreamEnd {
    Stop,
    DeviceChanged { from: String, to: String },
    Lost(String),
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
    info: EndpointInfo,
    channels: usize,
    _keepalive: Option<RenderPlayer>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

fn resolve(cfg: &SourceConfig) -> windows::core::Result<IMMDevice> {
    let en = devices::enumerator()?;
    match (&cfg.endpoint, cfg.kind) {
        (Endpoint::Default(role), Kind::Loopback) => devices::default_endpoint(&en, Flow::Render, *role),
        (Endpoint::Default(role), Kind::Mic) => devices::default_endpoint(&en, Flow::Capture, *role),
        (Endpoint::Id(id), _) => devices::endpoint_by_id(&en, id),
    }
}

fn open(cfg: &SourceConfig) -> Result<Stream, String> {
    let err = |what: &'static str| move |e: windows::core::Error| format!("{what}：{e}");
    let dev = resolve(cfg).map_err(err("取设备"))?;
    let info = devices::info(&dev).map_err(err("取设备信息"))?;
    let fmt = cfg.timeline.format;
    unsafe {
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).map_err(err("激活 IAudioClient"))?;
        let wf = float_format(fmt.rate, fmt.channels);
        let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if cfg.kind == Kind::Loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        // 200 ms 缓冲：事件驱动下由引擎周期决定唤醒间隔，这里只是上限
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &wf, None).map_err(err("初始化音频流（自动格式转换）"))?;
        let event = CreateEventW(None, false, false, None).map_err(err("创建事件"))?;
        client.SetEventHandle(event).map_err(err("设置事件"))?;
        let capture: IAudioCaptureClient = client.GetService().map_err(err("取采集服务"))?;
        let keepalive = if cfg.kind == Kind::Loopback && cfg.keepalive {
            // 保活流必须与被采集的设备是同一个，否则无声时仍然不出包
            Some(RenderPlayer::start(Some(info.id.clone()), Role::Console, fmt.rate, 2, |_| {}).map_err(|e| format!("启动保活渲染流：{e}"))?)
        } else {
            None
        };
        client.Start().map_err(err("启动采集"))?;
        Ok(Stream { client, capture, event, info, channels: usize::from(fmt.channels), _keepalive: keepalive })
    }
}

fn run(cfg: SourceConfig, qpc: Qpc, events: Sender<AudioEvent>, mut sink: AudioSink, stop: &AtomicBool, stop_at: &AtomicI64, live: &Live) -> SourceResult {
    let _com = ComGuard::init().expect("COM 初始化");
    let _mmcss = Mmcss::enter();
    let label = cfg.label.clone();
    let channels = usize::from(cfg.timeline.format.channels);
    let anchor = qpc.now_hns();
    live.anchor_hns.store(anchor, Ordering::Release);
    let mut tl = AudioTimeline::new(cfg.timeline, anchor);
    let mut result = SourceResult { anchor, ..Default::default() };
    let mut emit = |s: &[f32]| {
        live.frames_out.fetch_add((s.len() / channels) as u64, Ordering::Relaxed);
        sink(s);
    };
    let mut down_since: Option<Hns> = None;
    let mut interrupted = false;
    let mut connected_once = false;

    while !stop.load(Ordering::Acquire) {
        let mut wait = cfg.reconnect_interval;
        match open(&cfg) {
            Ok(mut stream) => {
                if connected_once {
                    result.reconnects += 1;
                }
                connected_once = true;
                down_since = None;
                // 打开设备花的时间（或重连的空档）在这一刻补齐，之后的空闲/空档才有意义
                let started = tl.stream_started(qpc.now_hns(), &mut emit);
                notify_large_gap(&events, &cfg, &started);
                let _ = events.send(AudioEvent::Opened { label: label.clone(), device: stream.info.clone(), mix: None });
                if interrupted {
                    interrupted = false;
                    let _ = events.send(AudioEvent::Recovered { label: label.clone() });
                }
                match pump(&mut stream, &cfg, &qpc, &mut tl, &mut emit, stop, live, &events) {
                    StreamEnd::Stop => break,
                    StreamEnd::DeviceChanged { from, to } => {
                        result.device_changes += 1;
                        let _ = events.send(AudioEvent::DeviceChanged { label: label.clone(), from, to });
                        wait = Duration::ZERO; // 默认设备变了，立刻连新设备
                    }
                    StreamEnd::Lost(error) => {
                        let _ = events.send(AudioEvent::StreamLost { label: label.clone(), error });
                        down_since = Some(qpc.now_hns());
                    }
                }
            }
            Err(error) => {
                let _ = events.send(AudioEvent::OpenFailed { label: label.clone(), error });
                down_since.get_or_insert_with(|| qpc.now_hns());
            }
        }
        // 重连等待：期间照常由 QPC 推算补静音，时间线保持连续
        let until = qpc.now_hns() + wait.as_millis() as i64 * 10_000;
        while !stop.load(Ordering::Acquire) {
            let now = qpc.now_hns();
            if let Some(since) = down_since
                && !interrupted
                && now - since >= cfg.interrupted_after.as_millis() as i64 * 10_000
            {
                interrupted = true;
                let _ = events.send(AudioEvent::Interrupted { label: label.clone() });
            }
            let r = if down_since.is_some() { tl.tick_outage(now, &mut emit) } else { tl.tick(now, &mut emit) };
            notify_large_gap(&events, &cfg, &r);
            if now >= until {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    // 补齐到“请求停止”的时刻；没有记录（线程自己结束）才用当前时刻
    let requested = stop_at.load(Ordering::Acquire);
    tl.finish(if requested > 0 { requested } else { qpc.now_hns() }, &mut emit);
    result.stats = tl.stats().clone();
    result
}

/// 单次补静音达到 `large_gap_ms` 时通知上层（用于告警，不影响录制本身）。
fn notify_large_gap(events: &Sender<AudioEvent>, cfg: &SourceConfig, r: &Report) {
    if r.large_gap {
        let _ = events.send(AudioEvent::LargeGap { label: cfg.label.clone(), filled_ms: r.filled_frames * 1000 / u64::from(cfg.timeline.format.rate) });
    }
}

#[allow(clippy::too_many_arguments)]
fn pump(s: &mut Stream, cfg: &SourceConfig, qpc: &Qpc, tl: &mut AudioTimeline, emit: &mut dyn FnMut(&[f32]), stop: &AtomicBool, live: &Live, events: &Sender<AudioEvent>) -> StreamEnd {
    let mut last_default_check = qpc.now_hns();
    let mut scratch: Vec<f32> = Vec::new();
    loop {
        if stop.load(Ordering::Acquire) {
            return StreamEnd::Stop;
        }
        unsafe { WaitForSingleObject(s.event, 10) };
        let now = qpc.now_hns();

        // 取走所有已就绪的包
        loop {
            let n = match unsafe { s.capture.GetNextPacketSize() } {
                Ok(n) => n,
                Err(e) => return StreamEnd::Lost(format!("GetNextPacketSize：{e}")),
            };
            if n == 0 {
                break;
            }
            let (mut data, mut frames, mut flags, mut dev_pos, mut qpc_pos) = (std::ptr::null_mut(), 0u32, 0u32, 0u64, 0u64);
            if let Err(e) = unsafe { s.capture.GetBuffer(&mut data, &mut frames, &mut flags, Some(&mut dev_pos), Some(&mut qpc_pos)) } {
                return StreamEnd::Lost(format!("GetBuffer：{e}"));
            }
            let arrival = qpc.now_hns();
            let pf = PacketFlags {
                silent: flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0,
                timestamp_error: flags & (AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32) != 0,
                discontinuity: flags & (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32) != 0,
            };
            let samples: &[f32] = if pf.silent || data.is_null() {
                &[]
            } else {
                let len = frames as usize * s.channels;
                let p = data as *const f32;
                if p.is_aligned() {
                    unsafe { std::slice::from_raw_parts(p, len) }
                } else {
                    scratch.clear();
                    scratch.extend((0..len).map(|i| unsafe { p.add(i).read_unaligned() }));
                    &scratch
                }
            };
            // qpcPosition 已经是 100 ns 单位
            let device_qpc = qpc_pos as Hns;
            let pkt = AudioPacket { device_qpc: Some(device_qpc), arrival_qpc: arrival, frames: frames as usize, samples, flags: pf };
            if let Some(tap) = &cfg.packet_tap {
                let _ = tap.send(PacketRecord { device_qpc, arrival_qpc: arrival, frames, flags: pf });
            }
            live.last_packet_hns.store(arrival, Ordering::Relaxed);
            let r = tl.push_packet(&pkt, emit);
            notify_large_gap(events, cfg, &r);
            if let Err(e) = unsafe { s.capture.ReleaseBuffer(frames) } {
                return StreamEnd::Lost(format!("ReleaseBuffer：{e}"));
            }
        }

        // 空闲：没有包也要让时间线继续走
        let r = tl.tick(now, emit);
        notify_large_gap(events, cfg, &r);
        live.publish_peak(tl.take_peak());

        // 默认设备变化：每 500 ms 比较一次（比 IMMNotificationClient 回调简单，且没有跨线程回调的死锁风险）
        if let Endpoint::Default(_) = cfg.endpoint
            && now - last_default_check >= ms_to_hns(500)
        {
            last_default_check = now;
            if let Ok(dev) = resolve(cfg)
                && let Ok(cur) = devices::info(&dev)
                && cur.id != s.info.id
            {
                return StreamEnd::DeviceChanged { from: s.info.name.clone(), to: cur.name };
            }
        }
    }
}

/// 把共享状态与结果放在一起，工具程序方便使用。
pub type SharedResult = Arc<Mutex<Option<SourceResult>>>;
