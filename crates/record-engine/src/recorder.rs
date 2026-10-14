//! 一场录制：启动、运行、保存并结束（设计文档 5.1–5.9、6.1、9）。
//!
//! 线程模型：每个采集源一个线程（音频 WASAPI 事件线程、画面循环线程），每轨一个 ffmpeg 子进程加一个写入线程，
//! 再加一个健康监测线程。采集线程只把数据放进写入器的队列，从不做磁盘 I/O。

use crate::awake::Awake;
use crate::disk;
use crate::events::{EngineEvent, StartError};
use crate::fault::{Fault, FaultSwitch};
use crate::health_loop::{self, AudioWatch, HealthInput, VideoWatch};
use crate::lock::SessionLock;
use crate::meeting::{self, AUDIO_RATE};
use crate::slot::Slot;
use chrono::{Local, Utc};
use crossbeam_channel::{Sender, unbounded};
use record_capture::audio::{AudioSink, AudioSource, Endpoint, Kind, SourceConfig, SourceResult};
use record_capture::com::ComGuard;
use record_capture::devices::{self, Flow, Role};
use record_capture::monitor::{MonitorInfo, Pick, list_monitors, pick_monitor};
use record_capture::qpc::Qpc;
use record_capture::video::{FrameSink, VideoConfig, VideoResult, VideoSource};
use record_core::clock::{HNS_PER_SEC, Hns};
use record_core::config::{Config, Encoder, MicRole, MonitorSel, SystemCapture};
use record_core::ffmpeg_args::{AudioSpec, VideoSpec};
use record_core::geometry::output_size;
use record_core::session::{EndReason, MonitorMeta, OutputMeta, SessionMeta, TrackKind, TrackMeta, TrackStatus, create_session_dir, save_meta, session_id};
use record_core::timeline::AudioFormat;
use record_mux::writer::{AudioWriter, VideoWriter, WriterReport};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
/// 音频写入队列的容量（秒）：ffmpeg 卡住这么久之后才开始丢数据。
pub(crate) const AUDIO_QUEUE_SECONDS: u32 = 10;
/// 收尾时等 ffmpeg 写完并退出的上限，超过就强杀。
const WRITER_FINISH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackSelect {
    pub video: bool,
    pub mic: bool,
    pub system: bool,
}

impl Default for TrackSelect {
    fn default() -> Self {
        Self { video: true, mic: true, system: true }
    }
}

impl TrackSelect {
    pub fn has(&self, kind: TrackKind) -> bool {
        match kind {
            TrackKind::Video => self.video,
            TrackKind::Mic => self.mic,
            TrackKind::System => self.system,
        }
    }
}

pub struct StartParams {
    pub config: Config,
    pub ffmpeg: PathBuf,
    /// 已经探测确定的编码器；录画面时不能是 `Auto`。
    pub encoder: Encoder,
    pub output_dir: PathBuf,
    pub app_version: String,
    pub tracks: TrackSelect,
    /// 覆盖配置里的显示器选择。热键触发时外壳传 `Pick::Foreground`。
    pub monitor: Option<Pick>,
}

/// 配置里的显示器选择换算成 [`Pick`]。
pub fn pick_from_config(sel: &MonitorSel) -> Result<Pick, String> {
    match sel {
        MonitorSel::Named(s) if s == "foreground" => Ok(Pick::Foreground),
        MonitorSel::Named(s) if s == "primary" => Ok(Pick::Primary),
        MonitorSel::Named(s) => Err(format!("monitor 必须是 \"foreground\"、\"primary\" 或从 1 开始的序号，不认识：{s}")),
        MonitorSel::Index(0) => Err("monitor 的序号从 1 开始".into()),
        MonitorSel::Index(n) => Ok(Pick::Index(*n as usize - 1)),
    }
}

struct VideoPlan {
    mon: MonitorInfo,
    out_w: u32,
    out_h: u32,
    all: Vec<String>,
}

fn plan_video(cfg: &Config, pick: Option<Pick>) -> Result<VideoPlan, StartError> {
    let list = list_monitors().map_err(|e| StartError::Monitor(format!("枚举显示器失败：{e}")))?;
    let pick = match pick {
        Some(p) => p,
        None => pick_from_config(&cfg.monitor).map_err(StartError::Monitor)?,
    };
    let all: Vec<String> = list.iter().map(MonitorInfo::describe).collect();
    let mon = pick_monitor(&list, pick).cloned().ok_or_else(|| StartError::Monitor(format!("没有匹配的显示器（{pick:?}）。当前有：{}", all.join("；"))))?;
    if mon.rotation != 1 {
        return Err(StartError::Monitor(format!("{} 是旋转的显示器（旋转代码 {}），画面采集暂不支持旋转", mon.device_name, mon.rotation)));
    }
    let (out_w, out_h) = output_size(mon.width, mon.height, cfg.video.max_long_edge, cfg.video.max_short_edge);
    Ok(VideoPlan { mon, out_w, out_h, all })
}

fn encoder_name(enc: Encoder) -> String {
    serde_json::to_value(enc).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

fn role_of(r: MicRole) -> Role {
    match r {
        MicRole::Communications => Role::Communications,
        MicRole::Console => Role::Console,
    }
}

/// 在独立线程里查有没有默认麦克风：调用方线程的 COM 状态不受影响（Tauri 主线程是单线程单元）。
fn has_default_mic(role: Role) -> bool {
    std::thread::spawn(move || {
        let Ok(_com) = ComGuard::init() else { return true };
        match devices::enumerator() {
            Ok(en) => devices::default_endpoint(&en, Flow::Capture, role).is_ok(),
            Err(_) => true,
        }
    })
    .join()
    .unwrap_or(true)
}

/// 音频交付：在槽的读锁里“交给写入器 + 给已交付帧数计数”，编码器重启时换写入器正好卡在两次交付之间，
/// 新一段的起点 = 锚点 + 已交付帧数，不会错位。
fn audio_sink(slot: Arc<Slot<AudioWriter>>, kind: TrackKind, fault: Arc<FaultSwitch>, qpc: Qpc, channels: u16, delivered: Arc<AtomicU64>) -> AudioSink {
    Box::new(move |samples| {
        if let Some(d) = fault.remaining_stall(kind, qpc.now_hns()) {
            std::thread::sleep(d);
        }
        let writer = slot.read();
        let _ = writer.push(samples);
        delivered.fetch_add((samples.len() / usize::from(channels)) as u64, Ordering::Release);
    })
}

fn video_sink(slot: Arc<Slot<VideoWriter>>, fault: Arc<FaultSwitch>, qpc: Qpc) -> FrameSink {
    Box::new(move |index, data, _kind| {
        if let Some(d) = fault.remaining_stall(TrackKind::Video, qpc.now_hns()) {
            std::thread::sleep(d);
        }
        let _ = slot.get().submit(index, data);
    })
}

struct AudioPart {
    kind: TrackKind,
    slot: Arc<Slot<AudioWriter>>,
    source: AudioSource,
    channels: u16,
    /// 已交付给写入器的帧数（时间线上的帧，含补的静音）。
    delivered: Arc<AtomicU64>,
}

impl AudioPart {
    fn watch(&self) -> AudioWatch {
        AudioWatch { live: Arc::clone(self.source.live()), slot: Arc::clone(&self.slot), channels: self.channels, delivered: Arc::clone(&self.delivered) }
    }
}

struct VideoPart {
    slot: Arc<Slot<VideoWriter>>,
    source: VideoSource,
}

pub struct Recorder {
    id: String,
    dir: PathBuf,
    ffmpeg: PathBuf,
    qpc: Qpc,
    t0: Hns,
    started: Instant,
    video: Option<VideoPart>,
    mic: Option<AudioPart>,
    system: Option<AudioPart>,
    health: Option<JoinHandle<SessionMeta>>,
    health_stop: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    fault: Arc<FaultSwitch>,
    lock: Option<SessionLock>,
    awake: Option<Awake>,
    initial_meta: SessionMeta,
}

/// 录制中随时可读的状态。
#[derive(Debug, Clone)]
pub struct Status {
    pub id: String,
    pub dir: PathBuf,
    pub elapsed: Duration,
    /// 各轨文件目前的总大小。
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StopTimings {
    /// 停止请求到各采集源退出。
    pub sources_ms: u64,
    /// 各 ffmpeg 写完并退出。
    pub writers_ms: u64,
    /// 生成 `meeting.mp4`。
    pub finalize_ms: u64,
}

pub struct StopOutcome {
    pub id: String,
    pub dir: PathBuf,
    pub reason: EndReason,
    /// 生成的成品；没有任何可用轨道、或生成失败时为 `None`（原因在 `warnings`）。
    pub meeting: Option<PathBuf>,
    pub duration_secs: f64,
    pub warnings: Vec<String>,
    pub meta: SessionMeta,
    pub timings: StopTimings,
}

fn writer_json(r: &WriterReport) -> serde_json::Value {
    json!({
        "exit_code": r.exit_code,
        "killed": r.killed,
        "failed": r.failed,
        "bytes_written": r.bytes_written,
        "dropped_queue": r.dropped_queue,
        "dropped_failed": r.dropped_failed,
        "gap_repeats": r.gap_repeats,
        "late_discarded": r.late_discarded,
    })
}

fn finish_audio(slot: Arc<Slot<AudioWriter>>) -> WriterReport {
    match slot.into_inner() {
        Some(w) => w.finish(WRITER_FINISH_TIMEOUT),
        None => WriterReport { failed: Some("写入器仍被占用，没有正常收尾".into()), ..Default::default() },
    }
}

fn finish_video(slot: Arc<Slot<VideoWriter>>) -> WriterReport {
    match slot.into_inner() {
        Some(w) => w.finish(WRITER_FINISH_TIMEOUT),
        None => WriterReport { failed: Some("写入器仍被占用，没有正常收尾".into()), ..Default::default() },
    }
}

fn track_meta(kind: TrackKind) -> TrackMeta {
    TrackMeta::new(kind.file_name(), None)
}

pub fn start(p: StartParams, events: Sender<EngineEvent>) -> Result<Recorder, StartError> {
    let cfg = &p.config;
    if !(p.tracks.video || p.tracks.mic || p.tracks.system) {
        return Err(StartError::NothingSelected);
    }
    if p.tracks.video && p.encoder == Encoder::Auto {
        return Err(StartError::EncoderNotResolved);
    }
    if p.tracks.system && cfg.audio.system_capture == SystemCapture::AllEndpoints {
        return Err(StartError::AllEndpointsNotImplemented);
    }
    std::fs::create_dir_all(&p.output_dir).map_err(StartError::SessionDir)?;
    if let Some(free) = disk::free_bytes(&p.output_dir)
        && (free as f64) < cfg.limits.min_free_gb * GIB
    {
        return Err(StartError::DiskLow { free_gb: free as f64 / GIB, min_gb: cfg.limits.min_free_gb });
    }
    // 先选显示器再建目录：失败不留任何痕迹
    let video_plan = if p.tracks.video { Some(plan_video(cfg, p.monitor)?) } else { None };

    let qpc = Qpc::new();
    let (dir, id) = create_session_dir(&p.output_dir, &session_id(Local::now().naive_local())).map_err(StartError::SessionDir)?;
    let lock = match SessionLock::acquire(&dir) {
        Ok(l) => l,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(StartError::SessionDir(e));
        }
    };

    let mut meta = SessionMeta::new(&id, &p.app_version, Utc::now(), qpc.frequency());
    for kind in TrackKind::ALL {
        if p.tracks.has(kind) {
            meta.tracks.set(kind, track_meta(kind));
        }
    }
    if let Some(v) = &video_plan {
        meta.monitor = Some(MonitorMeta { name: v.mon.device_name.clone(), width: v.mon.width, height: v.mon.height });
        meta.output = Some(OutputMeta { width: v.out_w, height: v.out_h, fps: cfg.video.fps, encoder: encoder_name(p.encoder) });
    }

    let (audio_tx, audio_rx) = unbounded();
    let (video_tx, video_rx) = unbounded();
    let fault = Arc::new(FaultSwitch::default());
    let active = Arc::new(AtomicBool::new(true));
    let health_stop = Arc::new(AtomicBool::new(false));
    let mut rec = Recorder {
        id: id.clone(),
        dir: dir.clone(),
        ffmpeg: p.ffmpeg.clone(),
        qpc,
        t0: 0,
        started: Instant::now(),
        video: None,
        mic: None,
        system: None,
        health: None,
        health_stop: Arc::clone(&health_stop),
        active: Arc::clone(&active),
        fault: Arc::clone(&fault),
        lock: Some(lock),
        awake: None,
        initial_meta: meta.clone(),
    };

    let launched = (|| -> Result<(), StartError> {
        save_meta(&dir, &meta).map_err(StartError::SessionDir)?;

        // 先把各轨的 ffmpeg 起来：它们初始化编码器要几百毫秒，期间采集的数据在队列里等着
        let video_slot = match &video_plan {
            Some(v) => {
                let spec = VideoSpec { width: v.out_w, height: v.out_h, fps: cfg.video.fps, encoder: p.encoder };
                let w = VideoWriter::spawn(&p.ffmpeg, &spec, &dir.join(TrackKind::Video.file_name()), cfg.video.fps as usize)?;
                Some(Slot::new(w))
            }
            None => None,
        };
        let mut audio_slots: Vec<(TrackKind, Arc<Slot<AudioWriter>>)> = Vec::new();
        for (kind, channels) in [(TrackKind::System, 2u16), (TrackKind::Mic, 1u16)] {
            if p.tracks.has(kind) {
                let w = AudioWriter::spawn(&p.ffmpeg, &AudioSpec { rate: AUDIO_RATE, channels }, &dir.join(kind.file_name()), AUDIO_QUEUE_SECONDS)?;
                audio_slots.push((kind, Slot::new(w)));
            }
        }

        // 采集源：回环、麦克风、画面。所有 ffmpeg 就绪之后才取 t0
        let t0 = qpc.now_hns();
        rec.t0 = t0;
        for (kind, slot) in audio_slots {
            let (label, k, endpoint, channels, time_source) = match kind {
                TrackKind::System => {
                    let endpoint = match cfg.audio.system_device.as_str() {
                        "default" => Endpoint::Default(match cfg.audio.system_capture {
                            SystemCapture::CommunicationsEndpoint => Role::Communications,
                            _ => Role::Console,
                        }),
                        id => Endpoint::Id(id.to_string()),
                    };
                    ("system", Kind::Loopback, endpoint, 2u16, cfg.audio.system_time_source)
                }
                _ => {
                    let endpoint = match cfg.audio.mic_device.as_str() {
                        "default" => Endpoint::Default(role_of(cfg.audio.mic_role)),
                        id => Endpoint::Id(id.to_string()),
                    };
                    ("mic", Kind::Mic, endpoint, 1u16, cfg.audio.mic_time_source)
                }
            };
            let mut sc = SourceConfig::new(label, k, endpoint, cfg.timeline_config(AudioFormat { rate: AUDIO_RATE, channels }, time_source));
            if kind == TrackKind::System {
                sc.keepalive = cfg.audio.system_keepalive;
            }
            let delivered = Arc::new(AtomicU64::new(0));
            let sink = audio_sink(Arc::clone(&slot), kind, Arc::clone(&fault), qpc, channels, Arc::clone(&delivered));
            let source =
                AudioSource::start(sc, qpc, audio_tx.clone(), sink).map_err(|e| StartError::Capture { what: if kind == TrackKind::Mic { "麦克风" } else { "系统音" }, why: e.to_string() })?;
            let part = AudioPart { kind, slot, source, channels, delivered };
            if kind == TrackKind::System {
                rec.system = Some(part);
            } else {
                rec.mic = Some(part);
            }
        }
        if let (Some(v), Some(slot)) = (&video_plan, video_slot) {
            let vc = VideoConfig { monitor: v.mon.clone(), fps: cfg.video.fps, out_w: v.out_w, out_h: v.out_h, t0, rebuild_interval: Duration::from_secs(1) };
            let source =
                VideoSource::start(vc, qpc, video_tx.clone(), video_sink(Arc::clone(&slot), Arc::clone(&fault), qpc)).map_err(|e| StartError::Capture { what: "画面", why: e.to_string() })?;
            rec.video = Some(VideoPart { slot, source });
        }

        // 视频起点马上落盘；音频起点由健康线程在采集线程发布后补上
        if let Some(t) = meta.tracks.video.as_mut() {
            t.t0_hns = Some(t0);
        }
        save_meta(&dir, &meta).map_err(StartError::SessionDir)?;
        rec.initial_meta = meta.clone();

        let input = HealthInput {
            cfg: cfg.clone(),
            encoder: p.encoder,
            ffmpeg: p.ffmpeg.clone(),
            monitors: video_plan.as_ref().map(|v| v.all.clone()).unwrap_or_default(),
            dir: dir.clone(),
            qpc,
            t0,
            meta: meta.clone(),
            video: match (&rec.video, &video_plan) {
                (Some(v), Some(plan)) => {
                    Some(VideoWatch { live: Arc::clone(v.source.live()), slot: Arc::clone(&v.slot), spec: VideoSpec { width: plan.out_w, height: plan.out_h, fps: cfg.video.fps, encoder: p.encoder } })
                }
                _ => None,
            },
            mic: rec.mic.as_ref().map(AudioPart::watch),
            system: rec.system.as_ref().map(AudioPart::watch),
            audio_rx,
            video_rx,
            events: events.clone(),
            stop: Arc::clone(&health_stop),
            active: Arc::clone(&active),
        };
        let health = std::thread::Builder::new().name("health".into()).spawn(move || health_loop::run(input)).map_err(StartError::SessionDir)?;
        rec.health = Some(health);
        Ok(())
    })();

    if let Err(e) = launched {
        rec.abort();
        return Err(e);
    }
    if p.tracks.mic && !has_default_mic(role_of(cfg.audio.mic_role)) {
        let _ = events.send(EngineEvent::Alert(crate::events::Alert::NoMic));
    }
    rec.started = Instant::now();
    // 画面靠桌面复制，显示器一休眠就没有画面：录制期间不让显示器和系统因空闲而休眠。放在所有采集都起来之后，启动失败的路径碰不到电源状态；
    // 请求不到也照常录，只留一条警告。
    rec.awake = Awake::new("Record 正在录制会议（保存或结束后自动恢复）").inspect_err(|e| tracing::warn!("没能请求保持唤醒（{e}）：久不动鼠标时显示器可能休眠，休眠期间录不到画面")).ok();
    Ok(rec)
}

impl Recorder {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn status(&self) -> Status {
        // 轨道文件：mkv 是画面，mka 是音频；编码器重启过的轨道还有后续各段
        let bytes = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| matches!(e.path().extension().and_then(|x| x.to_str()), Some("mkv" | "mka")))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        Status { id: self.id.clone(), dir: self.dir.clone(), elapsed: self.started.elapsed(), bytes }
    }

    /// 睡眠、锁屏期间传 `false`：基于超时的探测器暂停，恢复后重新布防。
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    /// 故障注入（设计文档 10.1 S3）。
    pub fn inject(&self, fault: Fault) -> Result<(), String> {
        match fault {
            Fault::StallCapture { track, secs } => {
                if !self.has(track) {
                    return Err(format!("这场录制没有{}", crate::events::track_name(track)));
                }
                self.fault.stall_until(track, self.qpc.now_hns() + secs as i64 * HNS_PER_SEC);
                Ok(())
            }
            Fault::KillEncoder { track } => match track {
                TrackKind::Video => self.video.as_ref().map(|v| v.slot.get().kill_process()),
                TrackKind::Mic => self.mic.as_ref().map(|a| a.slot.get().kill_process()),
                TrackKind::System => self.system.as_ref().map(|a| a.slot.get().kill_process()),
            }
            .ok_or_else(|| format!("这场录制没有{}", crate::events::track_name(track))),
        }
    }

    fn has(&self, kind: TrackKind) -> bool {
        match kind {
            TrackKind::Video => self.video.is_some(),
            TrackKind::Mic => self.mic.is_some(),
            TrackKind::System => self.system.is_some(),
        }
    }

    /// 启动中途失败时的清理：停掉已启动的一切，删掉会话目录，不留半成品。
    fn abort(mut self) {
        self.health_stop.store(true, Ordering::Release);
        if let Some(h) = self.health.take() {
            let _ = h.join();
        }
        let parts = [self.system.take(), self.mic.take()];
        for a in parts.into_iter().flatten() {
            a.slot.get().kill_process();
            drop(a.source);
        }
        if let Some(v) = self.video.take() {
            v.slot.get().kill_process();
            drop(v.source);
        }
        if let Some(l) = self.lock.take() {
            l.release();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// 保存并结束：补齐各轨到停止时刻、等 ffmpeg 写完、生成 `meeting.mp4`、写好 `session.json`。
    /// 会阻塞几秒（视频编码器冲刷、成品封装），外壳应在工作线程里调用。
    pub fn stop(mut self, reason: EndReason) -> StopOutcome {
        let begin = Instant::now();
        let stopped_hns = self.qpc.now_hns();
        let mut warnings: Vec<String> = Vec::new();

        // 各轨先一起请求停止，停止时刻才几乎相同
        for a in [&self.system, &self.mic].into_iter().flatten() {
            a.source.request_stop();
        }
        if let Some(v) = &self.video {
            v.source.request_stop();
        }
        let video_res: Option<(Arc<Slot<VideoWriter>>, VideoResult)> = self.video.take().map(|v| {
            let res = v.source.stop();
            (v.slot, res)
        });
        let audio_res: Vec<(TrackKind, Arc<Slot<AudioWriter>>, SourceResult)> = [self.system.take(), self.mic.take()]
            .into_iter()
            .flatten()
            .map(|a| {
                let res = a.source.stop();
                (a.kind, a.slot, res)
            })
            .collect();

        self.health_stop.store(true, Ordering::Release);
        let mut meta = match self.health.take().map(JoinHandle::join) {
            Some(Ok(m)) => m,
            _ => {
                warnings.push("健康监测线程异常退出，诊断信息不完整".into());
                self.initial_meta.clone()
            }
        };
        let sources_ms = begin.elapsed().as_millis() as u64;

        let writers_begin = Instant::now();
        let mut writer_reports = serde_json::Map::new();
        meta.stopped_hns = Some(stopped_hns);

        if let Some((slot, res)) = video_res {
            let report = finish_video(slot);
            meta.diagnostics.absorb_video(&res.stats);
            // 加上去：编码器重启过的话，之前几段的计数已经由健康线程记进来了
            meta.diagnostics.dropped_video_frames.queue += report.dropped_queue;
            meta.diagnostics.dropped_video_frames.error += report.dropped_failed;
            meta.diagnostics.dropped_video_frames.resync += report.late_discarded;
            if let Some(t) = meta.tracks.video.as_mut()
                && res.first_frame_ms.is_none()
            {
                t.t0_hns = None;
                t.status = TrackStatus::Missing;
                t.note = Some("没有采到任何画面".into());
                warnings.push("没有采到任何画面，成品里不含画面".into());
            }
            meta.extra.insert(
                "video_capture".into(),
                json!({
                    "src": [res.src_w, res.src_h], "out": [res.out_w, res.out_h], "hdr": res.hdr,
                    "rebuilds": res.rebuilds, "lost": res.lost, "secure_desktop_episodes": res.secure_desktop_episodes,
                    "first_frame_ms": res.first_frame_ms, "down_ms": res.down_ms,
                    "frames": { "fresh": res.stats.fresh, "no_change": res.stats.repeat_no_change, "catch_up": res.stats.catch_up },
                    "grab_us": { "count": res.grab.count, "p50": res.grab.p50_us, "p95": res.grab.p95_us, "p99": res.grab.p99_us, "max": res.grab.max_us },
                }),
            );
            if let Some(why) = &report.failed {
                warnings.push(format!("画面编码器出错：{why}"));
            }
            writer_reports.insert("video".into(), writer_json(&report));
        }
        for (kind, slot, res) in audio_res {
            let report = finish_audio(slot);
            meta.diagnostics.absorb_audio(kind.key(), AUDIO_RATE, &res.stats);
            meta.diagnostics.device_changes += res.device_changes;
            meta.diagnostics.dropped_audio_samples += report.dropped_queue + report.dropped_failed;
            let has_data = res.anchor != 0;
            let track = match kind {
                TrackKind::Mic => meta.tracks.mic.as_mut(),
                _ => meta.tracks.system.as_mut(),
            };
            if let Some(t) = track {
                if has_data {
                    t.t0_hns = Some(res.anchor);
                } else {
                    t.t0_hns = None;
                    t.status = TrackStatus::Missing;
                    t.note = Some("设备始终没有打开".into());
                    warnings.push(format!("{}始终没有打开，成品里不含这一轨", crate::events::track_name(kind)));
                }
            }
            if let Some(why) = &report.failed {
                warnings.push(format!("{}编码器出错：{why}", crate::events::track_name(kind)));
            }
            writer_reports.insert(kind.key().into(), writer_json(&report));
        }
        meta.extra.insert("writers".into(), serde_json::Value::Object(writer_reports));
        for kind in TrackKind::ALL {
            if meta.tracks.get(kind).is_some_and(|t| t.segments.len() > 1) {
                warnings.push(format!("{}的编码器在录制中异常退出过，已自动重启，成品里有一小段缺失", crate::events::track_name(kind)));
            }
        }
        let writers_ms = writers_begin.elapsed().as_millis() as u64;

        // 先落一次盘（仍是 recording）：收尾中途崩溃时，恢复流程能拿到各轨起点与停止时刻
        if let Err(e) = save_meta(&self.dir, &meta) {
            warnings.push(format!("写 session.json 失败：{e}"));
        }
        let finalize_begin = Instant::now();
        let meeting = match meeting::make_meeting(&self.ffmpeg, &self.dir, &meta) {
            Ok(Some(path)) => Some(path),
            Ok(None) => {
                warnings.push("没有任何可用的轨道，没有生成 meeting.mp4".into());
                None
            }
            Err(e) => {
                warnings.push(format!("生成 meeting.mp4 失败（原始轨道文件都在，下次启动会重试）：{e}"));
                None
            }
        };
        let finalize_ms = finalize_begin.elapsed().as_millis() as u64;

        meta.finalize(reason, Utc::now());
        if let Err(e) = save_meta(&self.dir, &meta) {
            warnings.push(format!("写 session.json 失败：{e}"));
        }
        // 都存好了：撤销保持唤醒，系统恢复自己的休眠计时
        self.awake = None;
        if let Some(l) = self.lock.take() {
            l.release();
        }
        let t0 = self.t0;
        StopOutcome {
            id: std::mem::take(&mut self.id),
            dir: std::mem::take(&mut self.dir),
            reason,
            meeting,
            duration_secs: (stopped_hns - t0).max(0) as f64 / HNS_PER_SEC as f64,
            warnings,
            meta,
            timings: StopTimings { sources_ms, writers_ms, finalize_ms },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_selection_in_the_config_maps_to_a_pick() {
        assert_eq!(pick_from_config(&MonitorSel::Named("foreground".into())), Ok(Pick::Foreground));
        assert_eq!(pick_from_config(&MonitorSel::Named("primary".into())), Ok(Pick::Primary));
        assert_eq!(pick_from_config(&MonitorSel::Index(1)), Ok(Pick::Index(0)));
        assert_eq!(pick_from_config(&MonitorSel::Index(3)), Ok(Pick::Index(2)));
        assert!(pick_from_config(&MonitorSel::Index(0)).is_err());
        assert!(pick_from_config(&MonitorSel::Named("左边那块".into())).is_err());
    }

    #[test]
    fn starting_without_any_track_or_without_an_encoder_is_refused_before_touching_the_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let params = |tracks, encoder| StartParams {
            config: Config::default(),
            ffmpeg: PathBuf::from("ffmpeg-不存在.exe"),
            encoder,
            output_dir: tmp.path().join("out"),
            app_version: "0".into(),
            tracks,
            monitor: None,
        };
        let (tx, _rx) = unbounded();
        let none = TrackSelect { video: false, mic: false, system: false };
        assert!(matches!(start(params(none, Encoder::Nvenc), tx.clone()), Err(StartError::NothingSelected)));
        assert!(matches!(start(params(TrackSelect::default(), Encoder::Auto), tx.clone()), Err(StartError::EncoderNotResolved)));
        let mut p = params(TrackSelect { video: false, mic: false, system: true }, Encoder::Nvenc);
        p.config.audio.system_capture = SystemCapture::AllEndpoints;
        assert!(matches!(start(p, tx), Err(StartError::AllEndpointsNotImplemented)));
        assert!(!tmp.path().join("out").exists(), "拒绝启动时不能建目录");
    }

    #[test]
    fn a_missing_ffmpeg_leaves_no_half_finished_session_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, _rx) = unbounded();
        let p = StartParams {
            config: Config::default(),
            ffmpeg: PathBuf::from("ffmpeg-不存在.exe"),
            encoder: Encoder::Nvenc,
            output_dir: tmp.path().to_path_buf(),
            app_version: "0".into(),
            tracks: TrackSelect { video: false, mic: true, system: false },
            monitor: None,
        };
        assert!(matches!(start(p, tx), Err(StartError::Mux(_))));
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "启动失败后目录必须是空的");
    }
}
