//! 画面采集线程（设计文档 5.2、5.9）。
//!
//! 按恒定帧率节拍取帧：每到一个节拍以 0 超时向桌面复制要一帧，有更新就转成 NV12，没有就沿用上一帧。
//! 每个到期的帧恰好交付一次（序号 = floor((now − t0) × fps)）。采集循环落后时，较早到期的帧用上一帧补齐，
//! 最新到期的一帧用刚取到的画面，保证输出文件的时间线与 QPC 一致。
//!
//! 失败不做静默兜底：复制失效就按节流重建，期间重复上一帧，按原因计数并发事件告警。

use crate::com::ComGuard;
use crate::monitor::{MonitorInfo, sdr_white_nits};
use crate::pipeline::{ErrorKind, Pipeline, PipelineError};
use crate::qpc::Qpc;
use crossbeam_channel::Sender;
use record_core::clock::Hns;
use record_core::pacer::{FramePacer, FrameStats, RepeatReason};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub monitor: MonitorInfo,
    pub fps: u32,
    pub out_w: u32,
    pub out_h: u32,
    /// 第 0 帧的时刻（QPC，100 ns），即整场录制的 t0。
    pub t0: Hns,
    /// 重建尝试的最短间隔（设计文档 5.2：OBS 是 3 秒，本设计初值 1 秒，由 S2 调定）。
    pub rebuild_interval: Duration,
}

/// 交付给写入器的这一帧是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// 这一节拍刚取到的新画面。
    Fresh,
    /// 沿用上一帧，附原因。
    Repeat(RepeatReason),
    /// 采集循环落后，较早到期的帧用上一帧补出来。
    CatchUp,
}

#[derive(Debug, Clone)]
pub enum VideoEvent {
    Opened {
        monitor: String,
        src_w: u32,
        src_h: u32,
        out_w: u32,
        out_h: u32,
        hdr: bool,
        took_ms: u64,
        rebuild: bool,
    },
    /// 第一帧新画面。`after_ms` 自 t0 起算。
    FirstFrame {
        after_ms: u64,
    },
    /// 复制对象失效或显卡设备丢失，开始重建。
    Lost {
        reason: String,
    },
    /// 安全桌面（UAC、锁屏）：系统不允许抓取。每次进入只报一次。
    SecureDesktop,
    /// 打开或重建失败（不是安全桌面）。同一种错误只报一次。
    Failed {
        error: String,
    },
    /// 重建成功，`down_ms` 是这次断开持续的时间。
    Recovered {
        down_ms: u64,
    },
    /// HDR 下用户调了系统的“SDR 内容亮度”，转换增益已跟着更新（画面亮度不会因此变化）。
    SdrWhiteChanged {
        nits: f32,
    },
}

/// 运行中随时可读的状态。
#[derive(Debug, Default)]
pub struct VideoLive {
    fresh: AtomicU64,
    emitted: AtomicU64,
    on_time: AtomicU64,
    last_fresh_hns: AtomicI64,
    down: AtomicBool,
}

impl VideoLive {
    pub fn fresh_frames(&self) -> u64 {
        self.fresh.load(Ordering::Relaxed)
    }

    pub fn emitted_frames(&self) -> u64 {
        self.emitted.load(Ordering::Relaxed)
    }

    /// 按时交付的帧数（新画面与“没变化/管线不可用”的重复帧），不含采集循环落后时追帧补出来的。
    /// 帧率监测用它：静止的屏幕也应当稳定地达到设定帧率。
    pub fn on_time_frames(&self) -> u64 {
        self.on_time.load(Ordering::Relaxed)
    }

    /// 最近一次新画面的时刻，没有过为 0。
    pub fn last_fresh_hns(&self) -> Hns {
        self.last_fresh_hns.load(Ordering::Relaxed)
    }

    /// 采集管线当前是否不可用（正在重建、安全桌面或失败）。
    pub fn is_down(&self) -> bool {
        self.down.load(Ordering::Relaxed)
    }
}

/// 取一帧（含 GPU 转换与回读）耗时的分布。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GrabTimes {
    pub count: u64,
    pub p50_us: u32,
    pub p95_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
}

impl GrabTimes {
    pub fn from_samples(samples: &mut [u32]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_unstable();
        let at = |p: f64| samples[(((samples.len() - 1) as f64) * p).round() as usize];
        Self { count: samples.len() as u64, p50_us: at(0.50), p95_us: at(0.95), p99_us: at(0.99), max_us: *samples.last().unwrap() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct VideoResult {
    pub stats: FrameStats,
    pub src_w: u32,
    pub src_h: u32,
    pub out_w: u32,
    pub out_h: u32,
    pub hdr: bool,
    /// 成功的重建次数（不含第一次打开）。
    pub rebuilds: u32,
    /// 复制对象失效或设备丢失的次数。
    pub lost: u32,
    pub secure_desktop_episodes: u32,
    /// 桌面复制给出的纹理格式（`DXGI_FORMAT` 数值）。
    pub src_format: Option<i32>,
    pub first_frame_ms: Option<u64>,
    /// 管线不可用的累计毫秒数（第一次打开之前的等待不算）。
    pub down_ms: u64,
    pub grab: GrabTimes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DownKind {
    Rebuilding,
    SecureDesktop,
    Failed(String),
}

struct Down {
    since: Hns,
    last_attempt: Option<Hns>,
    kind: DownKind,
}

fn repeat_reason(up: bool, kind: &DownKind) -> RepeatReason {
    match (up, kind) {
        (true, _) => RepeatReason::NoChange,
        (false, DownKind::Rebuilding) => RepeatReason::DuplicationRebuild,
        (false, DownKind::SecureDesktop) => RepeatReason::SecureDesktop,
        (false, DownKind::Failed(_)) => RepeatReason::DuplicationFailed,
    }
}

/// 把一次打开或取帧的失败记到断开状态里，状态变化时才发事件。
fn note_failure(e: &PipelineError, down: &mut Down, result: &mut VideoResult, events: &Sender<VideoEvent>) {
    let kind = if e.kind == ErrorKind::AccessDenied { DownKind::SecureDesktop } else { DownKind::Failed(e.message.clone()) };
    if kind != down.kind {
        match &kind {
            DownKind::SecureDesktop => {
                result.secure_desktop_episodes += 1;
                let _ = events.send(VideoEvent::SecureDesktop);
            }
            DownKind::Failed(m) => {
                let _ = events.send(VideoEvent::Failed { error: m.clone() });
            }
            DownKind::Rebuilding => {}
        }
        down.kind = kind;
    }
}

/// 交付一帧：序号、NV12 数据（重复帧共享同一份）、种类。在采集线程上调用，要尽快返回（只入队）。
pub type FrameSink = Box<dyn FnMut(u64, Arc<[u8]>, FrameKind) + Send>;

pub struct VideoSource {
    stop: Arc<AtomicBool>,
    live: Arc<VideoLive>,
    thread: Option<JoinHandle<VideoResult>>,
}

impl VideoSource {
    pub fn start(cfg: VideoConfig, qpc: Qpc, events: Sender<VideoEvent>, sink: FrameSink) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let live = Arc::new(VideoLive::default());
        let (s, l) = (Arc::clone(&stop), Arc::clone(&live));
        let thread = thread::Builder::new().name("video-capture".into()).spawn(move || run(cfg, qpc, events, sink, &s, &l))?;
        Ok(Self { stop, live, thread: Some(thread) })
    }

    pub fn live(&self) -> &Arc<VideoLive> {
        &self.live
    }

    /// 通知线程退出，不等它结束（多轨一起停时先对每一轨调用，再逐个 `stop()` 收结果）。
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn stop(mut self) -> VideoResult {
        self.stop.store(true, Ordering::Release);
        self.thread.take().map(|t| t.join().unwrap_or_default()).unwrap_or_default()
    }
}

impl Drop for VideoSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(cfg: VideoConfig, qpc: Qpc, events: Sender<VideoEvent>, mut sink: FrameSink, stop: &AtomicBool, live: &VideoLive) -> VideoResult {
    let _com = ComGuard::init().expect("COM 初始化");
    let interval = cfg.rebuild_interval.as_millis() as i64 * 10_000;
    let mut pacer = FramePacer::new(cfg.fps, cfg.t0);
    let mut result = VideoResult { out_w: cfg.out_w, out_h: cfg.out_h, hdr: cfg.monitor.hdr, ..Default::default() };
    let mut pipeline: Option<Pipeline> = None;
    let mut down = Down { since: qpc.now_hns(), last_attempt: None, kind: DownKind::Rebuilding };
    let mut ever_up = false;
    let mut last_frame: Option<Arc<[u8]>> = None;
    let mut grab_us: Vec<u32> = Vec::new();
    let mut white_nits: Option<f32> = None;
    let mut last_white_check: Hns = 0;
    live.down.store(true, Ordering::Relaxed);

    while !stop.load(Ordering::Acquire) {
        let mut now = qpc.now_hns();
        let mut due = pacer.due(now);
        if due.is_empty() {
            let wait = pacer.time_of(pacer.next_index()) - now;
            thread::sleep(Duration::from_micros((wait / 10).clamp(500, 20_000) as u64));
            continue;
        }

        // 1. 管线断开时按节流尝试（第一次不等）
        if pipeline.is_none() && down.last_attempt.is_none_or(|t| now - t >= interval) {
            down.last_attempt = Some(now);
            match Pipeline::open(&cfg.monitor, cfg.out_w, cfg.out_h) {
                Ok(p) => {
                    let _ = events.send(VideoEvent::Opened {
                        monitor: cfg.monitor.device_name.clone(),
                        src_w: p.src_w,
                        src_h: p.src_h,
                        out_w: p.out_w,
                        out_h: p.out_h,
                        hdr: p.is_hdr(),
                        took_ms: ((qpc.now_hns() - now) / 10_000).max(0) as u64,
                        rebuild: ever_up,
                    });
                    (result.src_w, result.src_h) = (p.src_w, p.src_h);
                    result.hdr = p.is_hdr();
                    white_nits = sdr_white_nits(&cfg.monitor.device_name);
                    last_white_check = now;
                    if ever_up {
                        result.rebuilds += 1;
                        let down_ms = ((qpc.now_hns() - down.since) / 10_000).max(0) as u64;
                        result.down_ms += down_ms;
                        let _ = events.send(VideoEvent::Recovered { down_ms });
                    }
                    ever_up = true;
                    pipeline = Some(p);
                    live.down.store(false, Ordering::Relaxed);
                    // 打开管线本身要 100~200 ms。到期的帧要按“现在”重新算，否则第一张新画面会被标上早了这么久的时刻
                    now = qpc.now_hns();
                    let more = pacer.due(now);
                    if !more.is_empty() {
                        due.end = more.end;
                    }
                }
                Err(e) => note_failure(&e, &mut down, &mut result, &events),
            }
        }

        // 1.5 HDR 下每秒看一次“SDR 内容亮度”，用户调了滑块就更新增益
        if let Some(p) = pipeline.as_ref().filter(|p| p.is_hdr())
            && now - last_white_check >= 10_000_000
        {
            last_white_check = now;
            let current = sdr_white_nits(&cfg.monitor.device_name);
            if let (Some(n), true) = (current, current != white_nits) {
                p.set_sdr_white_nits(n);
                white_nits = current;
                let _ = events.send(VideoEvent::SdrWhiteChanged { nits: n });
            }
        }

        // 2. 向桌面复制要一帧
        let mut fresh: Option<Arc<[u8]>> = None;
        if let Some(p) = pipeline.as_mut() {
            let t = qpc.now_hns();
            match p.grab() {
                Ok(Some(f)) => {
                    grab_us.push(((qpc.now_hns() - t) / 10).clamp(0, i64::from(u32::MAX)) as u32);
                    result.src_format = p.src_format;
                    fresh = Some(f);
                }
                Ok(None) => {}
                Err(e) => {
                    pipeline = None;
                    live.down.store(true, Ordering::Relaxed);
                    down = Down { since: qpc.now_hns(), last_attempt: None, kind: DownKind::Rebuilding };
                    if matches!(e.kind, ErrorKind::AccessLost | ErrorKind::DeviceLost) {
                        result.lost += 1;
                        let _ = events.send(VideoEvent::Lost { reason: e.message });
                    } else {
                        note_failure(&e, &mut down, &mut result, &events);
                    }
                }
            }
        }

        // 3. 每个到期的帧恰好交付一次：最新的一帧用新画面，其余用上一帧补齐
        let last = due.end - 1;
        for idx in due {
            let (data, kind) = if idx == last && fresh.is_some() {
                let f = fresh.take().expect("刚判断过");
                result.stats.fresh += 1;
                live.fresh.fetch_add(1, Ordering::Relaxed);
                live.last_fresh_hns.store(now, Ordering::Relaxed);
                if result.first_frame_ms.is_none() {
                    let after_ms = ((now - cfg.t0) / 10_000).max(0) as u64;
                    result.first_frame_ms = Some(after_ms);
                    let _ = events.send(VideoEvent::FirstFrame { after_ms });
                }
                last_frame = Some(Arc::clone(&f));
                (f, FrameKind::Fresh)
            } else if let Some(prev) = &last_frame {
                if idx == last {
                    let reason = repeat_reason(pipeline.is_some(), &down.kind);
                    result.stats.record_repeat(reason);
                    (Arc::clone(prev), FrameKind::Repeat(reason))
                } else {
                    result.stats.catch_up += 1;
                    (Arc::clone(prev), FrameKind::CatchUp)
                }
            } else {
                // 还没有任何画面：不交付，写入器会用第一帧补齐开头
                continue;
            };
            live.emitted.fetch_add(1, Ordering::Relaxed);
            if kind != FrameKind::CatchUp {
                live.on_time.fetch_add(1, Ordering::Relaxed);
            }
            sink(idx, data, kind);
        }
    }

    if pipeline.is_none() {
        result.down_ms += ((qpc.now_hns() - down.since) / 10_000).max(0) as u64;
    }
    result.grab = GrabTimes::from_samples(&mut grab_us);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grab_times_report_percentiles_and_the_maximum() {
        let mut v: Vec<u32> = (1..=100).collect();
        let g = GrabTimes::from_samples(&mut v);
        assert_eq!(g.count, 100);
        assert_eq!(g.p50_us, 51); // 下标 round(99 × 0.5) = 50
        assert_eq!(g.p95_us, 95);
        assert_eq!(g.p99_us, 99);
        assert_eq!(g.max_us, 100);
    }

    #[test]
    fn grab_times_of_nothing_are_zero() {
        assert_eq!(GrabTimes::from_samples(&mut []), GrabTimes::default());
        let mut one = [7];
        let g = GrabTimes::from_samples(&mut one);
        assert_eq!((g.p50_us, g.p99_us, g.max_us), (7, 7, 7));
    }

    #[test]
    fn repeat_reasons_follow_the_link_state() {
        assert_eq!(repeat_reason(true, &DownKind::Rebuilding), RepeatReason::NoChange);
        assert_eq!(repeat_reason(false, &DownKind::Rebuilding), RepeatReason::DuplicationRebuild);
        assert_eq!(repeat_reason(false, &DownKind::SecureDesktop), RepeatReason::SecureDesktop);
        assert_eq!(repeat_reason(false, &DownKind::Failed("x".into())), RepeatReason::DuplicationFailed);
    }

    fn denied() -> PipelineError {
        PipelineError { kind: ErrorKind::AccessDenied, message: "E_ACCESSDENIED".into() }
    }

    #[test]
    fn a_failure_is_reported_once_per_state_change() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut down = Down { since: 0, last_attempt: None, kind: DownKind::Rebuilding };
        let mut result = VideoResult::default();
        for _ in 0..5 {
            note_failure(&denied(), &mut down, &mut result, &tx);
        }
        assert_eq!(result.secure_desktop_episodes, 1);
        assert_eq!(rx.try_iter().filter(|e| matches!(e, VideoEvent::SecureDesktop)).count(), 1);

        let other = PipelineError { kind: ErrorKind::Unsupported, message: "不支持".into() };
        note_failure(&other, &mut down, &mut result, &tx);
        note_failure(&other, &mut down, &mut result, &tx);
        assert_eq!(rx.try_iter().filter(|e| matches!(e, VideoEvent::Failed { .. })).count(), 1);
        assert_eq!(down.kind, DownKind::Failed("不支持".into()));

        // 回到安全桌面算新的一次
        note_failure(&denied(), &mut down, &mut result, &tx);
        assert_eq!(result.secure_desktop_episodes, 2);
    }
}
