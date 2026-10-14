//! 健康监测线程（设计文档 5.10、6.5、9）：每秒一次的探测器、事件汇总、自动保护、`session.json` 增量落盘。
//!
//! 采集线程从不做 I/O；它们只更新原子计数。这个线程读那些计数，把异常变成 `EngineEvent`。
//! 它独占工作中的 `SessionMeta`，收尾时把它交还给 `Recorder::stop`。

use crate::disk;
use crate::events::{Alert, EngineEvent, describe_audio_event, describe_video_event};
use crate::fault::track_index;
use crate::meeting::AUDIO_RATE;
use crate::recorder::AUDIO_QUEUE_SECONDS;
use crate::slot::Slot;
use crossbeam_channel::{Receiver, Sender};
use record_capture::audio::{AudioEvent, Live};
use record_capture::com::ComGuard;
use record_capture::devices::{self, RenderMeters};
use record_capture::qpc::Qpc;
use record_capture::video::{VideoEvent, VideoLive};
use record_core::clock::{HNS_PER_SEC, Hns};
use record_core::config::{Config, Encoder};
use record_core::ffmpeg_args::{AudioSpec, VideoSpec};
use record_core::health::{HungDetector, HungEvent, LowRateDetector, LowRateEvent, SilenceWatchdog, WatchdogConfig, WatchdogEvent};
use record_core::limits;
use record_core::session::{EndReason, RestrictedInterval, SessionMeta, TrackKind, TrackStatus, save_meta};
use record_mux::probe::count_video_frames;
use record_mux::writer::{AudioWriter, Health, VideoWriter};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(100);
/// 写入失败时剩余空间低于它，就当作磁盘写满：保存并结束，而不是重启编码器。
const DISK_FULL_BYTES: u64 = 64 * 1024 * 1024;
/// 画面管线持续不可用（不含安全桌面）超过这么久，标记画面轨中断。
const VIDEO_INTERRUPTED_AFTER: Duration = Duration::from_secs(10);

/// 编码器出错后自动重启的次数上限（设计文档 9：重启一次；再失败，这一轨到此为止）。
const MAX_RESTARTS: u32 = 1;
/// 失败的旧写入器收尾最多等这么久：它的进程已被杀，只是回收线程。
const OLD_WRITER_FINISH: Duration = Duration::from_secs(5);
/// 数旧画面段里有多少帧的上限：长录制的文件要整个读一遍。
const COUNT_FRAMES_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct AudioWatch {
    pub live: Arc<Live>,
    pub slot: Arc<Slot<AudioWriter>>,
    pub channels: u16,
    /// 已交付给写入器的帧数；采集线程在槽的读锁里累加，重启时在写锁里读。
    pub delivered: Arc<AtomicU64>,
}

pub(crate) struct VideoWatch {
    pub live: Arc<VideoLive>,
    pub slot: Arc<Slot<VideoWriter>>,
    pub spec: VideoSpec,
}

pub(crate) struct HealthInput {
    pub cfg: Config,
    pub encoder: Encoder,
    pub ffmpeg: PathBuf,
    pub monitors: Vec<String>,
    pub dir: PathBuf,
    pub qpc: Qpc,
    pub t0: Hns,
    pub meta: SessionMeta,
    pub video: Option<VideoWatch>,
    pub mic: Option<AudioWatch>,
    pub system: Option<AudioWatch>,
    pub audio_rx: Receiver<AudioEvent>,
    pub video_rx: Receiver<VideoEvent>,
    pub events: Sender<EngineEvent>,
    pub stop: Arc<AtomicBool>,
    pub active: Arc<AtomicBool>,
}

/// “受限区间”：这段时间里画面是重复的上一帧（安全桌面、画面管线重建）。
#[derive(Debug, Default)]
pub(crate) struct IntervalTracker {
    open: Option<(u64, &'static str)>,
}

pub(crate) const SECURE_DESKTOP: &str = "secure_desktop";
pub(crate) const CAPTURE_LOST: &str = "capture_lost";
/// 画面管线重建一般只要 160 毫秒，不值得记；安全桌面不管多短都记，用户要知道那段是空的。
const MIN_LOST_MS: u64 = 1_000;

impl IntervalTracker {
    pub fn begin(&mut self, at_ms: u64, reason: &'static str) {
        match &mut self.open {
            None => self.open = Some((at_ms, reason)),
            // 重建过程中进入安全桌面：同一段，原因升级成安全桌面
            Some((_, r)) if reason == SECURE_DESKTOP => *r = SECURE_DESKTOP,
            Some(_) => {}
        }
    }

    pub fn end(&mut self, at_ms: u64) -> Option<RestrictedInterval> {
        let (start_ms, reason) = self.open.take()?;
        (reason == SECURE_DESKTOP || at_ms.saturating_sub(start_ms) >= MIN_LOST_MS).then(|| RestrictedInterval { start_ms, end_ms: at_ms, reason: reason.to_string() })
    }
}

/// 取回旧写入器的所有权以便收尾。画面采集线程可能刚好还捏着一个克隆，等一小会儿；
/// 一直等不到就放手：最后一个持有者放下时进程会被一并收掉。
fn reclaim<T>(mut shared: Arc<T>) -> Option<T> {
    for _ in 0..100 {
        match Arc::try_unwrap(shared) {
            Ok(v) => return Some(v),
            Err(back) => {
                shared = back;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    None
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s).trim().chars().take(200).collect()
}

struct Loop {
    h: HealthInput,
    dirty: bool,
    tick_n: u64,
    dog: SilenceWatchdog,
    mic_hung: HungDetector,
    video_hung: HungDetector,
    low: LowRateDetector,
    seen_mic_packet: Hns,
    seen_emitted: u64,
    seen_on_time: u64,
    interrupted: [bool; 3],
    /// 编码器已经彻底失败（重启过仍失败，或不该重启）：这一轨不再检查。
    writer_failed: [bool; 3],
    /// 各轨已经自动重启的次数。
    restarts: [u32; 3],
    /// 画面当前这一段的第一帧的全局序号（没重启过是 0）。
    video_base: u64,
    stop_requested: bool,
    video_failed_since: Option<Hns>,
    intervals: IntervalTracker,
    hdr_noted: bool,
    meters: Option<RenderMeters>,
    enumerator: Option<windows::Win32::Media::Audio::IMMDeviceEnumerator>,
}

fn label_kind(label: &str) -> TrackKind {
    if label == "mic" { TrackKind::Mic } else { TrackKind::System }
}

impl Loop {
    fn elapsed_ms(&self, now: Hns) -> u64 {
        ((now - self.h.t0).max(0) / 10_000) as u64
    }

    fn info(&self, text: String) {
        tracing::info!("{text}");
        let _ = self.h.events.send(EngineEvent::Info(text));
    }

    fn alert(&mut self, a: Alert) {
        let d = &mut self.h.meta.diagnostics;
        match &a {
            Alert::CaptureSilent { .. } => d.capture_silent_alerts += 1,
            Alert::Hung { .. } => d.hung_alerts += 1,
            Alert::LowFps { .. } => d.low_fps_alerts += 1,
            _ => {}
        }
        self.dirty = true;
        if a.is_warning() {
            tracing::warn!("{}", a.describe());
        } else {
            tracing::info!("{}", a.describe());
        }
        let _ = self.h.events.send(EngineEvent::Alert(a));
    }

    fn request_stop(&mut self, reason: EndReason) {
        if !self.stop_requested {
            self.stop_requested = true;
            let _ = self.h.events.send(EngineEvent::RequestStop(reason));
        }
    }

    fn mark_interrupted(&mut self, kind: TrackKind, note: String) {
        self.interrupted[track_index(kind)] = true;
        if let Some(t) = self.h.meta.tracks.get_mut(kind) {
            t.status = TrackStatus::Interrupted;
            t.note = Some(note);
        }
        self.dirty = true;
    }

    fn set_t0(&mut self, kind: TrackKind, t0: Hns) {
        if let Some(t) = self.h.meta.tracks.get_mut(kind).filter(|t| t.t0_hns.is_none()) {
            t.t0_hns = Some(t0);
            self.dirty = true;
        }
    }

    // ----- 事件 -----

    fn drain_events(&mut self) {
        let audio: Vec<AudioEvent> = self.h.audio_rx.try_iter().collect();
        for e in audio {
            self.info(describe_audio_event(&e));
            let now = self.h.qpc.now_hns();
            match &e {
                AudioEvent::LargeGap { label, filled_ms } => self.alert(Alert::LargeGap { track: label_kind(label), ms: *filled_ms }),
                AudioEvent::Interrupted { label } => {
                    let kind = label_kind(label);
                    self.mark_interrupted(kind, "设备丢失超过 10 秒仍未恢复".into());
                    self.alert(Alert::TrackInterrupted { track: kind });
                }
                AudioEvent::Recovered { label } => {
                    let kind = label_kind(label);
                    if self.interrupted[track_index(kind)] {
                        self.interrupted[track_index(kind)] = false;
                        self.alert(Alert::TrackRecovered { track: kind });
                    }
                    // 丢失期间时间线补了静音，这段时间不能算“挂起”
                    if kind == TrackKind::Mic {
                        self.mic_hung.rearm(now);
                    }
                }
                _ => {}
            }
        }
        let video: Vec<VideoEvent> = self.h.video_rx.try_iter().collect();
        for e in video {
            self.info(describe_video_event(&e));
            let now = self.h.qpc.now_hns();
            let at = self.elapsed_ms(now);
            match &e {
                VideoEvent::Opened { hdr: true, rebuild: false, .. } if !self.hdr_noted => {
                    self.hdr_noted = true;
                    self.alert(Alert::HdrActive);
                }
                VideoEvent::Lost { .. } => self.intervals.begin(at, CAPTURE_LOST),
                VideoEvent::SecureDesktop => self.intervals.begin(at, SECURE_DESKTOP),
                VideoEvent::Failed { .. } => {
                    self.video_failed_since.get_or_insert(now);
                    self.intervals.begin(at, CAPTURE_LOST);
                }
                VideoEvent::Recovered { .. } => {
                    self.video_failed_since = None;
                    if let Some(i) = self.intervals.end(at) {
                        self.h.meta.diagnostics.restricted_intervals.push(i);
                        self.dirty = true;
                    }
                    if self.interrupted[track_index(TrackKind::Video)] {
                        self.interrupted[track_index(TrackKind::Video)] = false;
                        self.alert(Alert::TrackRecovered { track: TrackKind::Video });
                    }
                }
                _ => {}
            }
        }
    }

    // ----- 每秒一次 -----

    fn tick_anchors(&mut self) {
        let t0 = self.h.t0;
        self.set_t0(TrackKind::Video, t0);
        for kind in [TrackKind::Mic, TrackKind::System] {
            let anchor = match kind {
                TrackKind::Mic => self.h.mic.as_ref(),
                _ => self.h.system.as_ref(),
            }
            .map(|w| w.live.anchor_hns())
            .unwrap_or(0);
            if anchor != 0 {
                self.set_t0(kind, anchor);
            }
        }
    }

    fn tick_mic(&mut self, now: Hns, active: bool) {
        let Some(mic) = &self.h.mic else { return };
        let pkt = mic.live.last_packet_hns();
        if pkt != self.seen_mic_packet {
            self.seen_mic_packet = pkt;
            if self.mic_hung.progress(now) == Some(HungEvent::Recovered) {
                self.alert(Alert::HungRecovered { track: TrackKind::Mic });
            }
        }
        // 设备丢失已经按“中断”告警过，不再重复报“挂起”
        if let Some(HungEvent::Hung { idle_secs }) = self.mic_hung.tick(now, active)
            && !self.interrupted[track_index(TrackKind::Mic)]
        {
            self.alert(Alert::Hung { track: TrackKind::Mic, secs: idle_secs });
        }
    }

    fn tick_video(&mut self, now: Hns, active: bool, dt: f64) {
        let Some(v) = &self.h.video else { return };
        let (emitted, on_time, fps, down) = (v.live.emitted_frames(), v.live.on_time_frames(), v.spec.fps, v.live.is_down());
        if emitted != self.seen_emitted {
            self.seen_emitted = emitted;
            if self.video_hung.progress(now) == Some(HungEvent::Recovered) {
                self.alert(Alert::HungRecovered { track: TrackKind::Video });
            }
        }
        if let Some(HungEvent::Hung { idle_secs }) = self.video_hung.tick(now, active) {
            self.alert(Alert::Hung { track: TrackKind::Video, secs: idle_secs });
        }
        let achieved = ((on_time - self.seen_on_time) as f64 / dt.max(0.1)).round() as u32;
        self.seen_on_time = on_time;
        match self.low.tick(achieved, fps, active) {
            Some(LowRateEvent::Low { ratio }) => self.alert(Alert::LowFps { ratio }),
            Some(LowRateEvent::Recovered) => self.alert(Alert::LowFpsRecovered),
            None => {}
        }
        // 画面管线持续打不开（不是安全桌面）
        let limit = VIDEO_INTERRUPTED_AFTER.as_secs() as i64 * HNS_PER_SEC;
        if down
            && let Some(since) = self.video_failed_since
            && now - since >= limit
            && !self.interrupted[track_index(TrackKind::Video)]
        {
            self.mark_interrupted(TrackKind::Video, "画面采集持续不可用".into());
            self.alert(Alert::TrackInterrupted { track: TrackKind::Video });
        }
    }

    fn tick_silence(&mut self, active: bool) {
        let Some(sys) = &self.h.system else { return };
        let captured = sys.live.take_peak();
        let meter = self.meters.as_ref().and_then(RenderMeters::max_peak);
        match self.dog.tick(meter, captured, active) {
            Some(WatchdogEvent::Alert) => {
                let device = self.meters.as_ref().and_then(RenderMeters::loudest).map(|(n, _)| n).unwrap_or_else(|| "（未知）".into());
                self.alert(Alert::CaptureSilent { device });
            }
            Some(WatchdogEvent::Recovered) => self.alert(Alert::CaptureSilentRecovered),
            None => {}
        }
    }

    fn writer_health(&self, kind: TrackKind) -> Option<Health> {
        match kind {
            TrackKind::Video => self.h.video.as_ref().map(|v| v.slot.get().health()),
            TrackKind::Mic => self.h.mic.as_ref().map(|a| a.slot.get().health()),
            TrackKind::System => self.h.system.as_ref().map(|a| a.slot.get().health()),
        }
    }

    /// 每个轮询周期看一眼各轨的编码器。出错时自动重启一次：新开一段文件接着写，旧的一段保留，成品里拼起来；
    /// 再出错，或者磁盘已满，这一轨就到此为止，其他轨继续（磁盘满则整场保存并结束）。
    fn check_writers(&mut self) {
        for kind in TrackKind::ALL {
            let i = track_index(kind);
            if self.writer_failed[i] {
                continue;
            }
            let Some(Health::Failed(why)) = self.writer_health(kind) else { continue };
            let short = first_line(&why);
            let disk_full = disk::free_bytes(&self.h.dir).is_some_and(|f| f < DISK_FULL_BYTES);
            let attempt = if disk_full {
                Err("磁盘空间已满，重启也写不进去".to_string())
            } else if self.restarts[i] >= MAX_RESTARTS {
                Err("已经自动重启过一次，不再重试".to_string())
            } else {
                self.restart(kind).map_err(|e| format!("自动重启失败：{e}"))
            };
            match attempt {
                Ok(()) => {
                    self.restarts[i] += 1;
                    self.h.meta.diagnostics.encoder_restarts += 1;
                    if let Some(t) = self.h.meta.tracks.get_mut(kind) {
                        t.note = Some(format!("编码器异常退出（{short}），已自动重启，中间有一小段缺失"));
                    }
                    self.alert(Alert::EncoderRestarted { track: kind, why: short });
                    self.persist();
                }
                Err(why_not) => {
                    self.writer_failed[i] = true;
                    let full = format!("{short}（{why_not}）");
                    self.mark_interrupted(kind, format!("编码器异常退出：{full}"));
                    self.alert(Alert::EncoderFailed { track: kind, why: full });
                    if disk_full {
                        self.request_stop(EndReason::WriteError);
                    }
                }
            }
        }
    }

    /// 换上写进新一段文件的写入器，并把这一段登记进 `session.json`（设计文档 9：编码器自动重启一次）。
    fn restart(&mut self, kind: TrackKind) -> Result<(), String> {
        let track = self.h.meta.tracks.get(kind).ok_or("元数据里没有这一轨")?;
        let file = track.next_segment_file(kind);
        let current = track.files().pop().unwrap_or_else(|| kind.file_name().to_string());
        match kind {
            TrackKind::Video => self.restart_video(&current, file),
            _ => self.restart_audio(kind, file),
        }
    }

    fn restart_video(&mut self, current: &str, file: String) -> Result<(), String> {
        let v = self.h.video.as_ref().ok_or("没有画面轨")?;
        let (spec, slot) = (v.spec, Arc::clone(&v.slot));
        // 旧编码器可能只是管道断了而进程还在：先让它死透，文件才不会再变，也才数得准
        slot.get().kill_process();
        let in_old = count_video_frames(&self.h.ffmpeg, &self.h.dir.join(current), COUNT_FRAMES_TIMEOUT).map_err(|e| format!("数不出上一段有多少帧：{e}"))?;
        // 新的一段从上一段的最后一帧之后接着写；中间丢失的一截，由新段收到的第一帧补齐（画面冻结）
        let next_base = self.video_base + in_old;
        let writer = VideoWriter::spawn_from(&self.h.ffmpeg, &spec, &self.h.dir.join(&file), spec.fps as usize, next_base).map_err(|e| e.to_string())?;
        let old = slot.replace_with(writer, || {});
        if let Some(old) = reclaim(old) {
            let r = old.finish(OLD_WRITER_FINISH);
            let d = &mut self.h.meta.diagnostics.dropped_video_frames;
            d.queue += r.dropped_queue;
            d.error += r.dropped_failed;
            d.resync += r.late_discarded;
        }
        let t0 = self.h.t0 + next_base as i64 * HNS_PER_SEC / i64::from(spec.fps);
        if let Some(t) = self.h.meta.tracks.get_mut(TrackKind::Video) {
            t.push_segment(file, t0, Some(in_old));
        }
        self.video_base = next_base;
        self.dirty = true;
        Ok(())
    }

    fn restart_audio(&mut self, kind: TrackKind, file: String) -> Result<(), String> {
        let w = match kind {
            TrackKind::Mic => self.h.mic.as_ref(),
            _ => self.h.system.as_ref(),
        }
        .ok_or("没有这一轨")?;
        let (slot, delivered, channels) = (Arc::clone(&w.slot), Arc::clone(&w.delivered), w.channels);
        let anchor = w.live.anchor_hns();
        if anchor == 0 {
            return Err("设备还没有开始录音，没有接着录的位置".into());
        }
        slot.get().kill_process();
        let writer = AudioWriter::spawn(&self.h.ffmpeg, &AudioSpec { rate: AUDIO_RATE, channels }, &self.h.dir.join(&file), AUDIO_QUEUE_SECONDS).map_err(|e| e.to_string())?;
        // 在写锁里读已交付的帧数：旧写入器收到的就是这么多，新写入器从下一帧开始；中间没写出来的部分在成品里是静音
        let mut already = 0;
        let old = slot.replace_with(writer, || already = delivered.load(Ordering::Acquire));
        if let Some(old) = reclaim(old) {
            let r = old.finish(OLD_WRITER_FINISH);
            self.h.meta.diagnostics.dropped_audio_samples += r.dropped_queue + r.dropped_failed;
        }
        let t0 = anchor + already as i64 * HNS_PER_SEC / i64::from(AUDIO_RATE);
        if let Some(t) = self.h.meta.tracks.get_mut(kind) {
            t.push_segment(file, t0, None);
        }
        self.dirty = true;
        Ok(())
    }

    fn tick_limits(&mut self, now: Hns) {
        if let Some(free) = disk::free_bytes(&self.h.dir)
            && let Some(reason) = limits::check(self.elapsed_ms(now) / 1000, free, &self.h.cfg.limits)
        {
            self.request_stop(reason);
        }
    }

    fn reopen_meters(&mut self) {
        if let Some(en) = &self.enumerator {
            self.meters = RenderMeters::open_all(en).ok();
        }
    }

    fn tick(&mut self, dt: f64) {
        let now = self.h.qpc.now_hns();
        let active = self.h.active.load(Ordering::Acquire);
        self.tick_n += 1;
        self.tick_anchors();
        self.tick_mic(now, active);
        self.tick_video(now, active, dt);
        self.tick_silence(active);
        if self.tick_n.is_multiple_of(5) {
            self.tick_limits(now);
        }
        // 插拔播放设备后，电平表要跟着重开，否则看门狗盯着已经不存在的设备
        if self.tick_n.is_multiple_of(15) {
            self.reopen_meters();
        }
        self.persist();
    }

    fn persist(&mut self) {
        if self.dirty {
            match save_meta(&self.h.dir, &self.h.meta) {
                Ok(()) => self.dirty = false,
                Err(e) => tracing::warn!("写 session.json 失败：{e}"),
            }
        }
    }
}

/// 线程主体，返回收尾时的 `SessionMeta`。
pub(crate) fn run(h: HealthInput) -> SessionMeta {
    // 这个线程自己初始化 COM（多线程单元），电平表和环境快照都在这里取
    let com = ComGuard::init();
    let enumerator = if com.is_ok() { devices::enumerator().ok() } else { None };
    let meters = enumerator.as_ref().and_then(|e| RenderMeters::open_all(e).ok());
    let now = h.qpc.now_hns();
    let (hung_s, low_ratio, low_s, confirm) = (h.cfg.health.hung_s, h.cfg.health.low_fps_ratio, h.cfg.health.low_fps_s, h.cfg.health.silent_confirm_s);
    let mut l = Loop {
        dog: SilenceWatchdog::new(WatchdogConfig { confirm_ticks: confirm, ..WatchdogConfig::default() }),
        mic_hung: HungDetector::new(hung_s, now),
        video_hung: HungDetector::new(hung_s, now),
        low: LowRateDetector::new(low_ratio, low_s),
        h,
        dirty: false,
        tick_n: 0,
        seen_mic_packet: 0,
        seen_emitted: 0,
        seen_on_time: 0,
        interrupted: [false; 3],
        writer_failed: [false; 3],
        restarts: [0; 3],
        video_base: 0,
        stop_requested: false,
        video_failed_since: None,
        intervals: IntervalTracker::default(),
        hdr_noted: false,
        meters,
        enumerator,
    };
    match &l.enumerator {
        Some(en) => {
            let env = crate::snapshot::environment(en, &l.h.cfg, l.h.encoder, &l.h.ffmpeg, &l.h.monitors);
            l.h.meta.extra.insert("environment".into(), env);
            l.dirty = true;
        }
        None => l.info("无法初始化设备枚举：环境快照与系统音静默检测不可用".into()),
    }
    if l.meters.is_none() && l.h.system.is_some() {
        l.info("无法打开播放设备的电平表：系统音“通但全是静音”的检测不可用".into());
    }

    let mut last = Instant::now();
    let mut next_tick = last + Duration::from_secs(1);
    while !l.h.stop.load(Ordering::Acquire) {
        std::thread::sleep(POLL);
        l.drain_events();
        l.check_writers();
        let t = Instant::now();
        if t >= next_tick {
            let dt = (t - last).as_secs_f64();
            last = t;
            next_tick += Duration::from_secs(1);
            if next_tick < t {
                next_tick = t + Duration::from_secs(1);
            }
            l.tick(dt);
        }
        l.persist();
    }
    l.drain_events();
    // 收尾时仍处于受限的区间：算到停止时刻
    let at = l.elapsed_ms(l.h.qpc.now_hns());
    if let Some(i) = l.intervals.end(at) {
        l.h.meta.diagnostics.restricted_intervals.push(i);
    }
    l.h.meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_rebuild_is_not_worth_recording_but_a_secure_desktop_always_is() {
        let mut t = IntervalTracker::default();
        t.begin(10_000, CAPTURE_LOST);
        assert_eq!(t.end(10_160), None);
        t.begin(20_000, SECURE_DESKTOP);
        let i = t.end(20_300).expect("安全桌面必须记");
        assert_eq!((i.start_ms, i.end_ms, i.reason.as_str()), (20_000, 20_300, "secure_desktop"));
    }

    #[test]
    fn a_long_capture_loss_is_recorded() {
        let mut t = IntervalTracker::default();
        t.begin(5_000, CAPTURE_LOST);
        let i = t.end(8_000).expect("3 秒的画面丢失要记");
        assert_eq!((i.start_ms, i.end_ms, i.reason.as_str()), (5_000, 8_000, "capture_lost"));
    }

    #[test]
    fn entering_the_secure_desktop_during_a_rebuild_extends_the_same_interval() {
        let mut t = IntervalTracker::default();
        t.begin(1_000, CAPTURE_LOST);
        t.begin(1_100, SECURE_DESKTOP);
        t.begin(1_200, CAPTURE_LOST);
        let i = t.end(1_400).unwrap();
        assert_eq!((i.start_ms, i.end_ms, i.reason.as_str()), (1_000, 1_400, "secure_desktop"));
    }

    #[test]
    fn ending_without_beginning_gives_nothing() {
        assert_eq!(IntervalTracker::default().end(5), None);
    }

    #[test]
    fn the_first_line_of_an_error_is_what_gets_shown() {
        assert_eq!(first_line("\n  写入 ffmpeg 失败：管道已结束。 (os error 109)\n后面还有很多"), "写入 ffmpeg 失败：管道已结束。 (os error 109)");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn reclaiming_waits_for_a_borrowed_handle_and_gives_up_if_it_never_comes_back() {
        let a = Arc::new(5);
        let b = Arc::clone(&a);
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(b);
        });
        assert_eq!(reclaim(a), Some(5));
        h.join().unwrap();

        let a = Arc::new(6);
        let _held = Arc::clone(&a);
        let t = Instant::now();
        assert_eq!(reclaim(a), None);
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn labels_map_to_tracks() {
        assert_eq!(label_kind("mic"), TrackKind::Mic);
        assert_eq!(label_kind("system"), TrackKind::System);
    }
}
