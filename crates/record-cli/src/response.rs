//! 回环链路的频率响应检查：播放一个已知的测试音，用回环采回来，比较电平。
//!
//! 为什么需要它：录下来的系统声音如果在高频被悄悄砍掉，会议录音会发闷，而脉冲计数、时间线统计都发现不了。
//! 本检查还能把播放流和采集流各自是否经过重采样隔离开（`--play-rate` / `--cap-rate` 设成设备混音格式的采样率就不重采样），
//! 并能验证“同时打开麦克风是否改变播放端点”（`--with-mic`）：某些 USB/蓝牙耳麦一开麦克风就切到窄带通话模式。

use crate::args::Args;
use crate::s1::{BeepSpec, beep_generator};
use anyhow::{Context, Result, bail};
use crossbeam_channel::unbounded;
use record_capture::audio::{AudioEvent, AudioSource, Endpoint, Kind, SourceConfig};
use record_capture::com::ComGuard;
use record_capture::devices::{self, Flow, MixFormat, Role};
use record_capture::qpc::Qpc;
use record_capture::render::RenderPlayer;
use record_core::config::Config;
use record_core::dsp::tone_level;
use record_core::timeline::{AudioFormat, TimeSource};
use std::f64::consts::PI;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct Setup {
    pub freq: f64,
    pub play_rate: u32,
    pub cap_rate: u32,
    pub amp: f32,
    /// 用自检同款的脉冲，而不是连续音。
    pub burst: bool,
    /// 同时打开默认通信麦克风。
    pub with_mic: bool,
}

pub struct Measured {
    /// 测得的最大振幅（0..1）。回环取数字信号，与音箱音量无关。
    pub level: f64,
    /// 测量时默认播放端点的混音格式。
    pub render_mix: MixFormat,
}

/// 在默认播放设备上播放 `freq` Hz 的测试音，用回环流采回来。
pub fn measure(s: &Setup) -> Result<Measured> {
    let qpc = Qpc::new();
    let cfg = Config::default();
    let (ev_tx, ev_rx) = unbounded::<AudioEvent>();
    let samples = Arc::new(Mutex::new(Vec::<f32>::new()));

    let mic = if s.with_mic {
        let sc = SourceConfig::new("mic", Kind::Mic, Endpoint::Default(Role::Communications), cfg.timeline_config(AudioFormat { rate: 48_000, channels: 1 }, TimeSource::DeviceQpc));
        Some(AudioSource::start(sc, qpc, ev_tx.clone(), Box::new(|_| {}))?)
    } else {
        None
    };

    let mut sc = SourceConfig::new("response", Kind::Loopback, Endpoint::Default(Role::Console), cfg.timeline_config(AudioFormat { rate: s.cap_rate, channels: 2 }, TimeSource::DeviceQpc));
    sc.keepalive = true; // 保证没有声音的间隙里也持续出包
    let sink = Arc::clone(&samples);
    let src = AudioSource::start(sc, qpc, ev_tx, Box::new(move |d| sink.lock().unwrap().extend_from_slice(d)))?;

    // 等流打开（打开失败会作为事件报告）
    thread::sleep(Duration::from_millis(1_000));
    if let Some(AudioEvent::OpenFailed { label, error }) = ev_rx.try_iter().find(|e| matches!(e, AudioEvent::OpenFailed { .. })) {
        src.stop();
        bail!("[{label}] 打开失败：{error}");
    }
    let render_mix = {
        let en = devices::enumerator()?;
        devices::mix_format(&devices::default_endpoint(&en, Flow::Render, Role::Console)?)?
    };

    let (freq, amp, play_rate) = (s.freq, s.amp, s.play_rate);
    let player = if s.burst {
        let spec = BeepSpec { freq, amp, ..Default::default() };
        RenderPlayer::start(None, Role::Console, play_rate, 2, beep_generator(spec, 2, Arc::new(AtomicU32::new(0))))
    } else {
        let mut n = 0u64;
        RenderPlayer::start(None, Role::Console, play_rate, 2, move |buf: &mut [f32]| {
            for frame in buf.chunks_exact_mut(2) {
                frame.fill(amp * (2.0 * PI * freq * n as f64 / f64::from(play_rate)).sin() as f32);
                n += 1;
            }
        })
    }
    .map_err(anyhow::Error::msg)?;
    thread::sleep(Duration::from_millis(if s.burst { 3_000 } else { 1_500 }));
    player.stop();
    thread::sleep(Duration::from_millis(200));
    src.stop();
    if let Some(m) = mic {
        m.stop();
    }

    let all = samples.lock().unwrap();
    let mono: Vec<f32> = all.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect();
    let win = (s.cap_rate / 100) as usize;
    let level = mono.chunks_exact(win).map(|w| tone_level(w, freq, s.cap_rate)).fold(0.0, f64::max);
    Ok(Measured { level, render_mix })
}

pub fn response_cmd(a: &Args) -> Result<()> {
    let base = Setup { freq: 0.0, play_rate: a.num("play-rate", 48_000)?, cap_rate: a.num("cap-rate", 48_000)?, amp: a.num("amp", 0.1)?, burst: a.flag("burst"), with_mic: a.flag("with-mic") };
    let freqs: Vec<f64> = a.get("freqs").unwrap_or("1000,4000,8000,12000,16000").split(',').map(|s| s.trim().parse::<f64>().with_context(|| format!("频率“{s}”不是数字"))).collect::<Result<_>>()?;
    let _com = ComGuard::init()?;
    println!(
        "播放 {} Hz → 回环采集 {} Hz（振幅 {}，{}，{}麦克风）",
        base.play_rate,
        base.cap_rate,
        base.amp,
        if base.burst { "脉冲" } else { "连续音" },
        if base.with_mic { "同时开" } else { "不开" }
    );
    for f in freqs {
        let m = measure(&Setup { freq: f, ..base })?;
        let db = 20.0 * (m.level / f64::from(base.amp)).max(1e-6).log10();
        println!("  {f:>7.0} Hz：电平 {:.4}  相对播放振幅 {db:+.1} dB   （播放端点混音格式 {} Hz {} 声道）", m.level, m.render_mix.rate, m.render_mix.channels);
    }
    Ok(())
}
