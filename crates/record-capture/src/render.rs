//! 渲染流：用于“空闲期保活”（持续写入全零，让回环在无声时也持续出包，参考 OBS 的静音修复）
//! 和 S1 的合成测试音。事件驱动，共享模式。

use crate::com::ComGuard;
use crate::devices::{self, Flow, Role};
use crossbeam_channel::bounded;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient, IAudioClock, IAudioRenderClient,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

pub fn float_format(rate: u32, channels: u16) -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * u32::from(channels) * 4,
        nBlockAlign: channels * 4,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

/// 第 `frame` 帧（从 0 数起）出声的 QPC 时刻（100 ns）：设备位置 `position` 帧在 `position_qpc` 时刻正在出声，其余按采样率推算。
pub fn heard_hns(position: u64, position_qpc: i64, frame: u64, rate: u32) -> i64 {
    position_qpc + ((frame as f64 - position as f64) * 1e7 / f64::from(rate)).round() as i64
}

/// 渲染流的“出声时钟”：渲染线程每次醒来读一次设备位置，外部据此推算某个已交给引擎的帧什么时候从扬声器里出声。
/// 播放设备的缓冲与驱动延迟可以很大（本机实测约 0.6 秒），写进去的声音要这么久之后才听得到；
/// 回环采集给声音打的是出声时刻，所以做同步测试时，画面必须按这个时刻去闪，而不是按写入时刻。
pub struct PlayClock {
    rate: u32,
    written: AtomicU64,
    /// （设备位置对应的帧序号，读取该位置时的 QPC，单位 100 ns）
    position: Mutex<Option<(u64, i64)>>,
}

impl PlayClock {
    pub fn new(rate: u32) -> Self {
        Self { rate, written: AtomicU64::new(0), position: Mutex::new(None) }
    }

    /// 到目前为止交给引擎的帧数；在生成器里读到的就是这次要填的缓冲区第一帧的序号。
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// 第 `frame` 帧出声的 QPC 时刻；还没读到过设备位置时返回 `None`。
    pub fn heard_at(&self, frame: u64) -> Option<i64> {
        let (position, qpc) = (*self.position.lock().ok()?)?;
        Some(heard_hns(position, qpc, frame, self.rate))
    }
}

/// 在指定的播放设备上持续播放。`generator` 每次要填满给定的交错 f32 缓冲区。
pub struct RenderPlayer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RenderPlayer {
    /// 流成功启动后才返回；失败时返回原因。`role` 为 `None` 时用 `device_id` 指定的设备。
    pub fn start(device_id: Option<String>, role: Role, rate: u32, channels: u16, generator: impl FnMut(&mut [f32]) + Send + 'static) -> Result<Self, String> {
        Self::start_clocked(device_id, role, rate, channels, Arc::new(PlayClock::new(rate)), generator)
    }

    /// 同 [`start`](Self::start)，并把出声时钟交给调用方：生成器和主线程都可以读它。`clock` 必须用同一个 `rate` 创建。
    pub fn start_clocked(device_id: Option<String>, role: Role, rate: u32, channels: u16, clock: Arc<PlayClock>, generator: impl FnMut(&mut [f32]) + Send + 'static) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = bounded::<Result<(), String>>(1);
        let stop2 = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("render-player".into())
            .spawn(move || {
                let r = run(Target { device_id, role, rate, channels }, &clock, generator, &stop2, &ready_tx);
                if let Err(e) = r {
                    let _ = ready_tx.try_send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("渲染线程在启动前退出".into()),
        }
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for RenderPlayer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 渲染到哪个设备、什么格式。
struct Target {
    device_id: Option<String>,
    role: Role,
    rate: u32,
    channels: u16,
}

fn run(target: Target, clock: &PlayClock, mut generator: impl FnMut(&mut [f32]), stop: &AtomicBool, ready: &crossbeam_channel::Sender<Result<(), String>>) -> Result<(), String> {
    let Target { device_id, role, rate, channels } = target;
    let e = |what: &str| {
        let what = what.to_string();
        move |err: windows::core::Error| format!("{what}：{err}")
    };
    let _com = ComGuard::init().map_err(e("COM 初始化"))?;
    let en = devices::enumerator().map_err(e("创建设备枚举器"))?;
    let dev = match &device_id {
        Some(id) => devices::endpoint_by_id(&en, id),
        None => devices::default_endpoint(&en, Flow::Render, role),
    }
    .map_err(e("取播放设备"))?;
    unsafe {
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).map_err(e("激活 IAudioClient"))?;
        let fmt = float_format(rate, channels);
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, 0, 0, &fmt, None)
            .map_err(e("初始化渲染流"))?;
        let event: HANDLE = CreateEventW(None, false, false, None).map_err(e("创建事件"))?;
        client.SetEventHandle(event).map_err(e("设置事件"))?;
        let buffer_frames = client.GetBufferSize().map_err(e("取缓冲大小"))?;
        let render: IAudioRenderClient = client.GetService().map_err(e("取渲染服务"))?;
        let ch = usize::from(channels);

        let mut fill = |frames: u32| -> Result<(), String> {
            let p = render.GetBuffer(frames).map_err(e("GetBuffer"))?;
            let slice = std::slice::from_raw_parts_mut(p as *mut f32, frames as usize * ch);
            slice.fill(0.0);
            generator(slice);
            render.ReleaseBuffer(frames, 0).map_err(e("ReleaseBuffer"))?;
            clock.written.fetch_add(u64::from(frames), Ordering::AcqRel);
            Ok(())
        };
        // 设备位置：按流的采样率换算成帧；每次醒来读一次，供 `PlayClock::heard_at` 外推
        let device_clock: IAudioClock = client.GetService().map_err(e("取时钟服务"))?;
        let device_freq = device_clock.GetFrequency().map_err(e("取时钟频率"))?.max(1);
        let read_position = || {
            let (mut position, mut qpc) = (0u64, 0u64);
            if device_clock.GetPosition(&mut position, Some(&mut qpc)).is_ok()
                && let Ok(mut slot) = clock.position.lock()
            {
                *slot = Some(((u128::from(position) * u128::from(rate) / u128::from(device_freq)) as u64, qpc as i64));
            }
        };
        fill(buffer_frames)?;
        client.Start().map_err(e("启动渲染流"))?;
        let _ = ready.try_send(Ok(()));
        while !stop.load(Ordering::Acquire) {
            WaitForSingleObject(event, 100);
            read_position();
            let padding = client.GetCurrentPadding().map_err(e("GetCurrentPadding"))?;
            let avail = buffer_frames.saturating_sub(padding);
            if avail > 0 {
                fill(avail)?;
            }
        }
        let _ = client.Stop();
        let _ = CloseHandle(event);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_under_the_speaker_is_heard_at_the_reading_time() {
        assert_eq!(heard_hns(48_000, 123_456_789, 48_000, 48_000), 123_456_789);
    }

    #[test]
    fn frames_further_ahead_are_heard_later_at_the_sample_rate() {
        // 比正在出声的帧晚 24000 帧，也就是半秒
        assert_eq!(heard_hns(1000, 0, 25_000, 48_000), 5_000_000);
        // 已经出过声的帧在过去
        assert_eq!(heard_hns(48_000, 0, 0, 48_000), -10_000_000);
    }

    #[test]
    fn nothing_is_known_before_the_first_position_reading() {
        let c = PlayClock::new(48_000);
        assert_eq!(c.written(), 0);
        assert_eq!(c.heard_at(0), None);
    }
}
