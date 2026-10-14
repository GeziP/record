//! S4 评测用的回声消除小程序。
//!
//! 用法：`aec-harness <算法> <采样率> <远端.f32> <麦克风.f32> <输出.f32> [键=值 ...]`
//!
//! - 输入输出都是原始的、小端的、单声道 f32 文件（由评测脚本转换），不在这里解析 WAV，也不引入别的依赖。
//! - 算法：`none`（原样输出，用来核对读写）、`corti`（corti-aec）、`aec3`（WebRTC AEC3 的 Rust 移植）。
//! - 通用选项：`align=1` 先估计整体延迟并把远端向后平移对齐；`max_delay_ms=500` 是延迟搜索范围；`margin_ms=0` 是少平移的毫秒数（让远端比回声的直达声早到一点）。
//! - corti 选项：`filter_len mu eps power_smoothing double_talk_ratio passes suppress_residual`，缺省用库的缺省值。
//! - aec3 选项：`delay_ms` 初始延迟提示（缺省 0）。
//! - 标准输出一行 JSON：算法、采样率、样点数、估计的延迟与实际平移的毫秒数（没估计则为 null）、耗时（秒）。

use std::collections::HashMap;
use std::error::Error;
use std::time::Instant;

use rustfft::{FftPlanner, num_complex::Complex};

type Res<T> = Result<T, Box<dyn Error>>;

fn read_f32(path: &str) -> Res<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() % 4 != 0 {
        return Err(format!("{path}：长度不是 4 的倍数，不像原始 f32 文件").into());
    }
    Ok(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

fn write_f32(path: &str, samples: &[f32]) -> Res<()> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

/// 把信号整体向后平移 d 个样点（前面补 0，长度不变）。
fn shift_right(x: &[f32], d: usize) -> Vec<f32> {
    let mut out = vec![0.0; x.len()];
    if d < x.len() {
        out[d..].copy_from_slice(&x[..x.len() - d]);
    }
    out
}

const SEG_LEN: usize = 32768;
const MAX_SEGS: usize = 64;

/// 估计 mic 相对 far 的整体延迟（样点数，不小于 0，不超过 max_lag）：mic[n] ≈ far[n - d]。
/// 做法：取最多 MAX_SEGS 段远端有声的片段，每段做 PHAT 加权的互相关（互功率谱除以自己的模），把各段累加后反变换，取峰值位置。
/// 远端全程无声或太短时返回 None。
fn estimate_delay(far: &[f32], mic: &[f32], max_lag: usize) -> Option<usize> {
    let n = far.len().min(mic.len());
    if n < SEG_LEN + max_lag {
        return None;
    }
    let fft_len = (SEG_LEN + max_lag).next_power_of_two();
    let starts: Vec<usize> = (0..MAX_SEGS).map(|i| i * (n - SEG_LEN - max_lag) / (MAX_SEGS - 1).max(1)).collect();
    let energy = |s: usize| far[s..s + SEG_LEN].iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>();
    let top = starts.iter().map(|&s| energy(s)).fold(0.0, f64::max);
    if top <= 0.0 {
        return None;
    }
    let mut planner = FftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(fft_len);
    let inv = planner.plan_fft_inverse(fft_len);
    let mut acc = vec![Complex::new(0.0f32, 0.0); fft_len];
    let mut used = 0;
    let mut last = usize::MAX;
    for &s in &starts {
        if s == last || energy(s) < 0.25 * top {
            continue;
        }
        last = s;
        let mut x = vec![Complex::new(0.0f32, 0.0); fft_len];
        let mut y = vec![Complex::new(0.0f32, 0.0); fft_len];
        for i in 0..SEG_LEN {
            x[i].re = far[s + i];
        }
        for i in 0..SEG_LEN + max_lag {
            y[i].re = mic[s + i];
        }
        fwd.process(&mut x);
        fwd.process(&mut y);
        for k in 0..fft_len {
            let c = x[k].conj() * y[k];
            let m = c.norm();
            if m > 1e-12 {
                acc[k] += c / m;
            }
        }
        used += 1;
    }
    if used == 0 {
        return None;
    }
    inv.process(&mut acc);
    (0..=max_lag).max_by(|&a, &b| acc[a].re.partial_cmp(&acc[b].re).unwrap())
}

fn opt<T: std::str::FromStr>(o: &HashMap<String, String>, key: &str) -> Res<Option<T>> {
    match o.get(key) {
        None => Ok(None),
        Some(v) => v.parse::<T>().map(Some).map_err(|_| format!("选项 {key}={v} 的值不对").into()),
    }
}

fn run_corti(far: &[f32], mic: &[f32], rate: u32, o: &HashMap<String, String>) -> Res<Vec<f32>> {
    let mut cfg = corti_aec::AecConfig::default();
    if let Some(v) = opt(o, "filter_len")? {
        cfg.filter_len = v;
    }
    if let Some(v) = opt(o, "mu")? {
        cfg.mu = v;
    }
    if let Some(v) = opt(o, "eps")? {
        cfg.eps = v;
    }
    if let Some(v) = opt(o, "power_smoothing")? {
        cfg.power_smoothing = v;
    }
    if let Some(v) = opt(o, "double_talk_ratio")? {
        cfg.double_talk_ratio = v;
    }
    if let Some(v) = opt(o, "passes")? {
        cfg.passes = v;
    }
    if let Some(v) = opt(o, "suppress_residual")? {
        cfg.suppress_residual = v;
    }
    Ok(corti_aec::cancel(mic, far, rate, &cfg))
}

fn run_aec3(far: &[f32], mic: &[f32], rate: u32, o: &HashMap<String, String>) -> Res<Vec<f32>> {
    use aec3::nodes::audio::AudioFormat;
    use aec3::pipelines::linear;
    let delay_ms: i32 = opt(o, "delay_ms")?.unwrap_or(0);
    let format = AudioFormat::ten_ms(rate, 1);
    let mut pipeline = linear::builder(format, format).initial_delay_ms(delay_ms).build()?;
    let frame = format.sample_count();
    let mut out = Vec::with_capacity(mic.len());
    let padded = |x: &[f32], at: usize| {
        let mut f = vec![0.0f32; frame];
        if at < x.len() {
            let n = (x.len() - at).min(frame);
            f[..n].copy_from_slice(&x[at..at + n]);
        }
        f
    };
    let mut at = 0;
    while at < mic.len() {
        pipeline.handle_render_frame(&padded(far, at))?;
        let valid = (mic.len() - at).min(frame);
        let mut processed = vec![0.0f32; frame];
        let produced = pipeline.process_capture_frame(&padded(mic, at), &mut processed)?;
        if produced {
            out.extend_from_slice(&processed[..valid]);
        } else {
            out.resize(out.len() + valid, 0.0);
        }
        at += valid;
    }
    Ok(out)
}

/// `align=1` 时先估计整体延迟并把远端向后平移对齐；返回（用于消除的远端，估计的延迟毫秒，实际平移的毫秒）。没开 `align` 或估计不出来时原样返回。
/// `margin_ms`（缺省 0）：少平移这么多，让远端比回声的直达声早到 margin_ms。零余量时直达声恰好落在滤波器的第 0 点，估计差一个样点它就跑到参考信号前面去，因果滤波器消不掉。
fn prepare_far(far: &[f32], mic: &[f32], rate: u32, o: &HashMap<String, String>) -> Res<(Vec<f32>, Option<f64>, Option<f64>)> {
    if opt::<u8>(o, "align")?.unwrap_or(0) == 0 {
        return Ok((far.to_vec(), None, None));
    }
    let max_ms: f64 = opt(o, "max_delay_ms")?.unwrap_or(500.0);
    let margin_ms: f64 = opt(o, "margin_ms")?.unwrap_or(0.0);
    if margin_ms.is_nan() || margin_ms < 0.0 {
        return Err(format!("选项 margin_ms={margin_ms} 要是不小于 0 的数").into());
    }
    let max_lag = (max_ms * rate as f64 / 1000.0) as usize;
    let margin = (margin_ms * rate as f64 / 1000.0).round() as usize;
    Ok(match estimate_delay(far, mic, max_lag) {
        Some(d) => {
            let shift = d.saturating_sub(margin);
            (shift_right(far, shift), Some(d as f64 * 1000.0 / rate as f64), Some(shift as f64 * 1000.0 / rate as f64))
        }
        None => (far.to_vec(), None, None),
    })
}

/// 按算法名处理：none 原样返回麦克风信号，corti、aec3 调各自的库。
fn run(algo: &str, far: &[f32], mic: &[f32], rate: u32, o: &HashMap<String, String>) -> Res<Vec<f32>> {
    match algo {
        "none" => Ok(mic.to_vec()),
        "corti" => run_corti(far, mic, rate, o),
        "aec3" => run_aec3(far, mic, rate, o),
        other => Err(format!("不认识的算法 {other}").into()),
    }
}

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        return Err("用法：aec-harness <none|corti|aec3> <采样率> <远端.f32> <麦克风.f32> <输出.f32> [键=值 ...]".into());
    }
    let algo = args[0].as_str();
    let rate: u32 = args[1].parse().map_err(|_| format!("采样率 {} 不对", args[1]))?;
    let mut options = HashMap::new();
    for kv in &args[5..] {
        let (k, v) = kv.split_once('=').ok_or_else(|| format!("选项 {kv} 不是 键=值 的形式"))?;
        options.insert(k.to_string(), v.to_string());
    }
    let far = read_f32(&args[2])?;
    let mic = read_f32(&args[3])?;
    if far.len() != mic.len() {
        return Err(format!("远端与麦克风长度不同：{} 与 {} 个样点", far.len(), mic.len()).into());
    }

    let started = Instant::now();
    let (far_used, delay_ms, shift_ms) = prepare_far(&far, &mic, rate, &options)?;
    let out = run(algo, &far_used, &mic, rate, &options)?;
    let wall = started.elapsed().as_secs_f64();
    write_f32(&args[4], &out)?;
    let ms = |v: Option<f64>| v.map_or("null".to_string(), |d| format!("{d:.2}"));
    println!("{{\"algo\":\"{algo}\",\"rate\":{rate},\"samples\":{},\"delay_ms\":{},\"shift_ms\":{},\"wall_s\":{wall:.3}}}", out.len(), ms(delay_ms), ms(shift_ms));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 可复现的白噪声，范围 [-amp, amp]
    fn noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                ((s >> 8) as f32 / 8_388_608.0 - 1.0) * amp
            })
            .collect()
    }

    fn energy(x: &[f32]) -> f64 {
        x.iter().map(|v| (*v as f64) * (*v as f64)).sum()
    }

    #[test]
    fn shift_right_pads_front_and_keeps_length() {
        assert_eq!(shift_right(&[1.0, 2.0, 3.0, 4.0], 2), vec![0.0, 0.0, 1.0, 2.0]);
        assert_eq!(shift_right(&[1.0, 2.0], 0), vec![1.0, 2.0]);
        assert_eq!(shift_right(&[1.0, 2.0], 5), vec![0.0, 0.0]);
    }

    #[test]
    fn f32_files_round_trip_and_reject_bad_length() {
        let dir = std::env::temp_dir().join(format!("aec-harness-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.f32");
        let x = vec![0.0, -1.5, 3.25, f32::MIN_POSITIVE];
        write_f32(p.to_str().unwrap(), &x).unwrap();
        assert_eq!(read_f32(p.to_str().unwrap()).unwrap(), x);
        std::fs::write(&p, [1u8, 2, 3]).unwrap();
        assert!(read_f32(p.to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delay_estimate_finds_the_true_delay_even_with_near_end_interference() {
        let rate = 16000usize;
        let far = noise(rate * 20, 1, 0.5);
        for d in [0usize, 320, 1600, 4800] {
            let mut mic = shift_right(&far, d).iter().map(|v| v * 0.4).collect::<Vec<_>>();
            let near = noise(mic.len(), 2, 0.2);
            for (m, n) in mic.iter_mut().zip(&near) {
                *m += n;
            }
            assert_eq!(estimate_delay(&far, &mic, 8000), Some(d), "延迟 {d} 个样点");
        }
    }

    #[test]
    fn delay_estimate_gives_none_for_silent_far_end_or_short_input() {
        let rate = 16000usize;
        let mic = noise(rate * 20, 3, 0.5);
        assert_eq!(estimate_delay(&vec![0.0; rate * 20], &mic, 8000), None);
        assert_eq!(estimate_delay(&noise(1000, 4, 0.5), &noise(1000, 5, 0.5), 800), None);
    }

    #[test]
    fn none_keeps_the_microphone_signal_and_unknown_algorithms_are_rejected() {
        let far = noise(100, 6, 0.5);
        let mic = noise(100, 7, 0.5);
        assert_eq!(run("none", &far, &mic, 16000, &HashMap::new()).unwrap(), mic);
        assert!(run("bogus", &far, &mic, 16000, &HashMap::new()).unwrap_err().to_string().contains("不认识的算法"));
    }

    #[test]
    fn prepare_far_shifts_by_the_estimated_delay_only_when_asked() {
        let rate = 16000u32;
        let far = noise(rate as usize * 20, 8, 0.5);
        let mic: Vec<f32> = shift_right(&far, 1600).iter().map(|v| v * 0.4).collect();
        let (same, none, no_shift) = prepare_far(&far, &mic, rate, &HashMap::new()).unwrap();
        assert_eq!(same, far);
        assert_eq!(none, None);
        assert_eq!(no_shift, None);
        let align = HashMap::from([("align".to_string(), "1".to_string())]);
        let (shifted, ms, shift_ms) = prepare_far(&far, &mic, rate, &align).unwrap();
        assert_eq!(shifted, shift_right(&far, 1600));
        assert!((ms.unwrap() - 100.0).abs() < 1e-9, "估计的延迟 {ms:?} 毫秒");
        assert!((shift_ms.unwrap() - 100.0).abs() < 1e-9, "平移 {shift_ms:?} 毫秒");
        // 搜索范围不够大时，不会估出一个错的数当作对的
        let (_, small, _) = prepare_far(&far, &mic, rate, &HashMap::from([("align".to_string(), "1".to_string()), ("max_delay_ms".to_string(), "50".to_string())])).unwrap();
        assert_ne!(small, Some(100.0));
    }

    #[test]
    fn margin_shifts_less_than_the_estimated_delay_and_never_below_zero() {
        let rate = 16000u32;
        let far = noise(rate as usize * 20, 8, 0.5);
        let mic: Vec<f32> = shift_right(&far, 1600).iter().map(|v| v * 0.4).collect();
        let with = |margin: &str| HashMap::from([("align".to_string(), "1".to_string()), ("margin_ms".to_string(), margin.to_string())]);
        // 估计的延迟仍是 100 ms，实际只平移 100 - 10 = 90 ms（1440 个样点）
        let (shifted, ms, shift_ms) = prepare_far(&far, &mic, rate, &with("10")).unwrap();
        assert_eq!(shifted, shift_right(&far, 1440));
        assert!((ms.unwrap() - 100.0).abs() < 1e-9);
        assert!((shift_ms.unwrap() - 90.0).abs() < 1e-9);
        // 余量比延迟还大：不平移（不会减成负数绕回去）
        let (zero, _, shift_ms) = prepare_far(&far, &mic, rate, &with("250")).unwrap();
        assert_eq!(zero, far);
        assert_eq!(shift_ms, Some(0.0));
        // 负数、NaN（它和任何数比大小都是假的）与写错的值都报错
        assert!(prepare_far(&far, &mic, rate, &with("-1")).is_err());
        assert!(prepare_far(&far, &mic, rate, &with("NaN")).is_err());
        assert!(prepare_far(&far, &mic, rate, &with("abc")).is_err());
    }

    #[test]
    fn bad_option_values_are_reported_not_ignored() {
        let far = noise(100, 6, 0.5);
        let o = HashMap::from([("filter_len".to_string(), "abc".to_string())]);
        assert!(run_corti(&far, &far, 16000, &o).unwrap_err().to_string().contains("filter_len=abc"));
        let o = HashMap::from([("align".to_string(), "yes".to_string())]);
        assert!(prepare_far(&far, &far, 16000, &o).is_err());
    }

    /// 纯回声（麦克风里只有远端延迟 + 衰减）：两个算法在后半段都应把能量压下去至少 10 dB。
    #[test]
    fn both_algorithms_reduce_a_pure_delayed_echo() {
        let rate = 16000u32;
        let far = noise(rate as usize * 30, 7, 0.5);
        let mic: Vec<f32> = shift_right(&far, 1600).iter().map(|v| v * 0.4).collect();
        let half = mic.len() / 2;
        let before = energy(&mic[half..]);

        let mut opts = HashMap::new();
        opts.insert("filter_len".to_string(), "16384".to_string());
        let corti = run_corti(&far, &mic, rate, &opts).unwrap();
        let reduction = 10.0 * (before / energy(&corti[half..]).max(1e-30)).log10();
        assert!(reduction >= 10.0, "corti 对齐前只压了 {reduction:.1} dB");

        let aligned = shift_right(&far, estimate_delay(&far, &mic, 8000).unwrap());
        let aec3 = run_aec3(&aligned, &mic, rate, &HashMap::new()).unwrap();
        let reduction = 10.0 * (before / energy(&aec3[half..]).max(1e-30)).log10();
        assert!(reduction >= 10.0, "aec3 对齐后只压了 {reduction:.1} dB");
    }
}
