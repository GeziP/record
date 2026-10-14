//! 集成测试的公用工具：定位 ffmpeg / ffprobe、合成信号、解码与分析。
#![allow(dead_code)]

use record_core::config::Encoder;
use record_mux::locate::{ffprobe_for, find_ffmpeg};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const RATE: u32 = 48_000;

pub fn ffmpeg() -> PathBuf {
    find_ffmpeg().unwrap_or_else(|e| panic!("集成测试需要 ffmpeg：{e}\n请先运行 scripts\\fetch-ffmpeg.ps1，或设置环境变量 RECORD_FFMPEG"))
}

/// 测试用编码器：默认 NVENC（开发机），其他机器用 `RECORD_TEST_ENCODER` 指定。
pub fn encoder() -> Encoder {
    match std::env::var("RECORD_TEST_ENCODER").as_deref() {
        Ok("h264_qsv") => Encoder::Qsv,
        Ok("h264_amf") => Encoder::Amf,
        Ok("libopenh264") => Encoder::Software,
        Ok("h264_nvenc") | Err(_) => Encoder::Nvenc,
        Ok(other) => panic!("RECORD_TEST_ENCODER 的值不认识：{other}"),
    }
}

/// 交错的正弦波，`freq` Hz，`seconds` 秒。
pub fn sine(freq: f64, channels: usize, seconds: f64, amplitude: f32) -> Vec<f32> {
    let frames = (seconds * f64::from(RATE)) as usize;
    let mut out = Vec::with_capacity(frames * channels);
    for n in 0..frames {
        let v = (2.0 * std::f64::consts::PI * freq * n as f64 / f64::from(RATE)).sin() as f32 * amplitude;
        for _ in 0..channels {
            out.push(v);
        }
    }
    out
}

/// 一帧 NV12：亮度是随帧号移动的斜纹，色度恒为 128。
pub fn nv12_frame(w: usize, h: usize, n: usize) -> Vec<u8> {
    let mut f = vec![128u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            f[y * w + x] = ((x + y + n * 7) % 220 + 16) as u8;
        }
    }
    f
}

fn probe(path: &Path, args: &[&str]) -> String {
    let out = Command::new(ffprobe_for(&ffmpeg())).args(["-v", "error"]).args(args).arg(path).output().expect("启动 ffprobe");
    assert!(out.status.success(), "ffprobe 失败：{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// 容器里记录的时长（秒）。
pub fn probe_duration(path: &Path) -> f64 {
    probe(path, &["-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]).parse().unwrap_or_else(|_| panic!("{} 没有有效的时长", path.display()))
}

/// 实际解出来的视频帧数。
pub fn probe_frames(path: &Path) -> u64 {
    probe(path, &["-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "default=nw=1:nk=1"]).parse().expect("帧数")
}

pub fn probe_start_time(path: &Path, stream: &str) -> f64 {
    probe(path, &["-select_streams", stream, "-show_entries", "stream=start_time", "-of", "default=nw=1:nk=1"]).parse().expect("start_time")
}

/// 解码音频为交错 f32（立体声，48 kHz）。
pub fn decode_stereo(path: &Path) -> Vec<f32> {
    let out = Command::new(ffmpeg()).args(["-v", "error", "-i"]).arg(path).args(["-map", "0:a:0", "-f", "f32le", "-ac", "2", "-ar", "48000", "-"]).output().expect("启动 ffmpeg");
    assert!(out.status.success(), "解码失败：{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "解码有告警：{}", String::from_utf8_lossy(&out.stderr));
    out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

/// 加汉宁窗的 Goertzel：返回 `freq` 处的幅度估计（纯正弦时约等于振幅）。
pub fn tone_level(window: &[f32], freq: f64) -> f64 {
    let n = window.len();
    let (mut re, mut im, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
    for (i, &s) in window.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
        let phase = 2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(RATE);
        re += f64::from(s) * w * phase.cos();
        im -= f64::from(s) * w * phase.sin();
        wsum += w;
    }
    2.0 * (re * re + im * im).sqrt() / wsum
}

/// 取左声道，逐 20 ms 窗口找 `freq` 的音量首次超过 `threshold` 的时刻（秒）。
pub fn tone_onset(stereo: &[f32], freq: f64, threshold: f64) -> Option<f64> {
    let left: Vec<f32> = stereo.chunks_exact(2).map(|f| f[0]).collect();
    let win = (RATE / 50) as usize;
    (0..left.len().saturating_sub(win)).step_by(win / 4).find(|&i| tone_level(&left[i..i + win], freq) > threshold).map(|i| i as f64 / f64::from(RATE))
}
