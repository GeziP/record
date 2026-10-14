//! 每轨一个 ffmpeg 子进程加一个写入线程（设计文档 5.6、5.9）。
//!
//! - 音频：队列按“采样帧数预算”限界（约 10 秒）；放不下就丢弃并计数，**不阻塞采集线程**。
//! - 视频：队列按帧数限界（约 1 秒），满了丢最旧的；每帧带序号，写入线程在序号出现空缺时重复上一帧，
//!   保证输出时间线与 QPC 一致；序号倒退（迟到）的帧丢弃并计为重同步。
//! - ffmpeg 退出或写入出错：写入线程记下原因，之后收到的数据只计数、不再写，由引擎决定是否重启。

use crate::MuxError;
use crate::process::FfmpegProc;
use crate::queue::DropOldest;
use crossbeam_channel::{Receiver, Sender, unbounded};
use record_core::ffmpeg_args::{AudioSpec, VideoSpec, audio_writer_args, video_writer_args};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Debug, Default)]
struct Shared {
    failed: Mutex<Option<String>>,
    bytes_written: AtomicU64,
    dropped_failed: AtomicU64,
    gap_repeats: AtomicU64,
    late_discarded: AtomicU64,
}

impl Shared {
    fn fail(&self, why: String) {
        let mut f = self.failed.lock().unwrap();
        if f.is_none() {
            *f = Some(why);
        }
    }

    fn failure(&self) -> Option<String> {
        self.failed.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Running,
    /// 写入线程记下的原因（含 ffmpeg 最后的错误输出）。
    Failed(String),
}

/// 一次 `push` / `submit` 的去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    Queued,
    /// 队列放不下：音频是新数据被丢弃；视频是丢掉了最旧的 n 帧。
    Overflow(u64),
    /// 写入器已失败，数据未写入。
    WriterFailed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriterReport {
    pub exit_code: Option<i32>,
    /// 超时后被强杀。
    pub killed: bool,
    pub failed: Option<String>,
    pub bytes_written: u64,
    /// 因队列满而丢弃（音频：采样帧数；视频：帧数）。
    pub dropped_queue: u64,
    /// 写入器失败后收到并丢弃的数据（单位同上）。
    pub dropped_failed: u64,
    /// 视频：序号出现空缺时重复上一帧的次数。
    pub gap_repeats: u64,
    /// 视频：序号倒退而被丢弃的迟到帧。
    pub late_discarded: u64,
    pub stderr_tail: String,
}

fn finish_process(mut proc: FfmpegProc, timeout: Duration) -> (Option<i32>, bool, String) {
    let (code, killed) = match proc.wait_timeout(timeout) {
        Ok(Some(status)) => (status.code(), false),
        _ => {
            proc.kill();
            (None, true)
        }
    };
    (code, killed, proc.stderr_tail())
}

// ---------------------------------------------------------------------------
// 音频
// ---------------------------------------------------------------------------

pub struct AudioWriter {
    tx: Option<Sender<Vec<f32>>>,
    shared: Arc<Shared>,
    queued_frames: Arc<AtomicUsize>,
    cap_frames: usize,
    channels: usize,
    dropped_queue: AtomicU64,
    proc: Mutex<Option<FfmpegProc>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioWriter {
    pub fn spawn(ffmpeg: &Path, spec: &AudioSpec, out: &Path, queue_seconds: u32) -> Result<Self, MuxError> {
        let args = audio_writer_args(spec, out);
        let mut proc = FfmpegProc::spawn(ffmpeg, &args, true, false).map_err(MuxError::Spawn)?;
        let mut stdin = proc.take_stdin().expect("stdin 已设置为管道");
        let (tx, rx): (Sender<Vec<f32>>, Receiver<Vec<f32>>) = unbounded();
        let shared = Arc::new(Shared::default());
        let queued = Arc::new(AtomicUsize::new(0));
        let channels = usize::from(spec.channels);
        let (sh, q) = (Arc::clone(&shared), Arc::clone(&queued));
        let thread = thread::Builder::new().name("audio-writer".into()).spawn(move || {
            let mut bytes: Vec<u8> = Vec::new();
            for chunk in rx {
                q.fetch_sub(chunk.len() / channels, Ordering::Relaxed);
                if sh.failure().is_some() {
                    sh.dropped_failed.fetch_add((chunk.len() / channels) as u64, Ordering::Relaxed);
                    continue;
                }
                bytes.clear();
                bytes.reserve(chunk.len() * 4);
                for s in &chunk {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                match stdin.write_all(&bytes) {
                    Ok(()) => {
                        sh.bytes_written.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    }
                    Err(e) => sh.fail(format!("写入 ffmpeg 失败：{e}")),
                }
            }
            drop(stdin); // 关闭标准输入，ffmpeg 读到结尾后写完文件并退出
        })?;
        Ok(Self {
            tx: Some(tx),
            shared,
            queued_frames: queued,
            cap_frames: spec.rate as usize * queue_seconds as usize,
            channels,
            dropped_queue: AtomicU64::new(0),
            proc: Mutex::new(Some(proc)),
            thread: Some(thread),
        })
    }

    /// 交付一块交错 f32 采样。放不下时丢弃这块并计数，不阻塞调用线程。
    pub fn push(&self, samples: &[f32]) -> Pushed {
        if self.shared.failure().is_some() {
            self.shared.dropped_failed.fetch_add((samples.len() / self.channels) as u64, Ordering::Relaxed);
            return Pushed::WriterFailed;
        }
        let frames = samples.len() / self.channels;
        if self.queued_frames.load(Ordering::Relaxed) + frames > self.cap_frames {
            self.dropped_queue.fetch_add(frames as u64, Ordering::Relaxed);
            return Pushed::Overflow(frames as u64);
        }
        self.queued_frames.fetch_add(frames, Ordering::Relaxed);
        if let Some(tx) = &self.tx {
            let _ = tx.send(samples.to_vec());
        }
        Pushed::Queued
    }

    pub fn health(&self) -> Health {
        match self.shared.failure() {
            Some(why) => Health::Failed(why),
            None => Health::Running,
        }
    }

    pub fn bytes_written(&self) -> u64 {
        self.shared.bytes_written.load(Ordering::Relaxed)
    }

    pub fn queued_frames(&self) -> usize {
        self.queued_frames.load(Ordering::Relaxed)
    }

    /// 故障注入与测试用：强杀 ffmpeg 进程。
    pub fn kill_process(&self) {
        if let Some(p) = self.proc.lock().unwrap().as_mut() {
            p.kill();
        }
    }

    /// ffmpeg 子进程的 pid（用于计量它的 CPU）。`finish` 之后没有。
    pub fn process_id(&self) -> Option<u32> {
        self.proc.lock().unwrap().as_ref().map(FfmpegProc::id)
    }

    /// 冲刷队列、关闭标准输入、等待 ffmpeg 退出；超过 `timeout` 强杀。
    pub fn finish(mut self, timeout: Duration) -> WriterReport {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            // 写入线程可能卡在写一个不再读取的管道上：先等进程，超时强杀后线程自然出错退出
            let proc = self.proc.get_mut().unwrap().take().expect("进程仍在");
            let (code, killed, tail) = finish_process(proc, timeout);
            let _ = t.join();
            return self.report(code, killed, tail);
        }
        self.report(None, false, String::new())
    }

    fn report(&self, exit_code: Option<i32>, killed: bool, stderr_tail: String) -> WriterReport {
        WriterReport {
            exit_code,
            killed,
            failed: self.shared.failure(),
            bytes_written: self.shared.bytes_written.load(Ordering::Relaxed),
            dropped_queue: self.dropped_queue.load(Ordering::Relaxed),
            dropped_failed: self.shared.dropped_failed.load(Ordering::Relaxed),
            stderr_tail,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// 视频
// ---------------------------------------------------------------------------

struct Frame {
    index: u64,
    data: Arc<[u8]>,
}

pub struct VideoWriter {
    queue: DropOldest<Frame>,
    shared: Arc<Shared>,
    frame_bytes: usize,
    proc: Mutex<Option<FfmpegProc>>,
    thread: Option<JoinHandle<()>>,
}

impl VideoWriter {
    /// `queue_frames` 约为 1 秒的帧数。
    pub fn spawn(ffmpeg: &Path, spec: &VideoSpec, out: &Path, queue_frames: usize) -> Result<Self, MuxError> {
        Self::spawn_from(ffmpeg, spec, out, queue_frames, 0)
    }

    /// 从全局帧序号 `first_index` 开始写：编码器重启后的新一段接着上一段的最后一帧。
    /// 收到的第一帧序号大于 `first_index` 时，中间空出来的序号用这第一帧补齐（画面冻结到恢复为止），
    /// 前后各段首尾相接，时间线不错位。
    pub fn spawn_from(ffmpeg: &Path, spec: &VideoSpec, out: &Path, queue_frames: usize, first_index: u64) -> Result<Self, MuxError> {
        let args = video_writer_args(spec, out)?;
        let mut proc = FfmpegProc::spawn(ffmpeg, &args, true, false).map_err(MuxError::Spawn)?;
        let mut stdin = proc.take_stdin().expect("stdin 已设置为管道");
        let queue: DropOldest<Frame> = DropOldest::new(queue_frames);
        let rx = queue.receiver();
        let shared = Arc::new(Shared::default());
        let sh = Arc::clone(&shared);
        let frame_bytes = (spec.width as usize * spec.height as usize * 3) / 2;
        let thread = thread::Builder::new().name("video-writer".into()).spawn(move || {
            let mut next: u64 = first_index;
            let mut last: Option<Arc<[u8]>> = None;
            for frame in rx {
                if sh.failure().is_some() {
                    sh.dropped_failed.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if frame.index < next {
                    sh.late_discarded.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let filler = last.clone().unwrap_or_else(|| Arc::clone(&frame.data));
                let mut write = |data: &Arc<[u8]>| -> bool {
                    match stdin.write_all(data) {
                        Ok(()) => {
                            sh.bytes_written.fetch_add(data.len() as u64, Ordering::Relaxed);
                            true
                        }
                        Err(e) => {
                            sh.fail(format!("写入 ffmpeg 失败：{e}"));
                            false
                        }
                    }
                };
                let mut ok = true;
                while ok && next < frame.index {
                    ok = write(&filler);
                    if ok {
                        sh.gap_repeats.fetch_add(1, Ordering::Relaxed);
                        next += 1;
                    }
                }
                if ok && write(&frame.data) {
                    next = frame.index + 1;
                    last = Some(frame.data);
                }
            }
            drop(stdin);
        })?;
        Ok(Self { queue, shared, frame_bytes, proc: Mutex::new(Some(proc)), thread: Some(thread) })
    }

    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    /// ffmpeg 子进程的 pid（用于计量它的 CPU）。`finish` 之前才有。
    pub fn process_id(&self) -> Option<u32> {
        self.proc.lock().unwrap().as_ref().map(FfmpegProc::id)
    }

    /// 交付第 `index` 帧（NV12）。重复帧共享同一份 `Arc`，不复制数据。
    pub fn submit(&self, index: u64, data: Arc<[u8]>) -> Pushed {
        assert_eq!(data.len(), self.frame_bytes, "帧大小与编码器设置不符");
        if self.shared.failure().is_some() {
            self.shared.dropped_failed.fetch_add(1, Ordering::Relaxed);
            return Pushed::WriterFailed;
        }
        match self.queue.push(Frame { index, data }) {
            0 => Pushed::Queued,
            n => Pushed::Overflow(n),
        }
    }

    pub fn health(&self) -> Health {
        match self.shared.failure() {
            Some(why) => Health::Failed(why),
            None => Health::Running,
        }
    }

    pub fn bytes_written(&self) -> u64 {
        self.shared.bytes_written.load(Ordering::Relaxed)
    }

    /// 故障注入与测试用：强杀 ffmpeg 进程。
    pub fn kill_process(&self) {
        if let Some(p) = self.proc.lock().unwrap().as_mut() {
            p.kill();
        }
    }

    pub fn finish(mut self, timeout: Duration) -> WriterReport {
        self.queue.close();
        let proc = self.proc.get_mut().unwrap().take().expect("进程仍在");
        let (code, killed, tail) = finish_process(proc, timeout);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        WriterReport {
            exit_code: code,
            killed,
            failed: self.shared.failure(),
            bytes_written: self.shared.bytes_written.load(Ordering::Relaxed),
            dropped_queue: self.queue.total_dropped(),
            dropped_failed: self.shared.dropped_failed.load(Ordering::Relaxed),
            gap_repeats: self.shared.gap_repeats.load(Ordering::Relaxed),
            late_discarded: self.shared.late_discarded.load(Ordering::Relaxed),
            stderr_tail: tail,
        }
    }
}
