//! 时间源比较（设计文档 5.5）：同一路设备的包，用“设备给的 QPC 时间戳”和“到达时间 − 包时长”
//! 各算一遍相对理想等间隔的残差，比较抖动与漂移，决定每一路用哪一种。

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rec {
    pub device_ms: f64,
    pub arrival_ms: f64,
    pub frames: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceStats {
    /// 相邻包残差差分的标准差（毫秒），越小越稳。
    pub jitter_ms: f64,
    /// 残差随时间的斜率（百万分率）：相对 QPC 的时钟漂移。
    pub drift_ppm: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PacketStats {
    pub packets: usize,
    pub segments: usize,
    pub device: SourceStats,
    pub arrival: SourceStats,
}

/// 残差跳变超过这个值（毫秒）认为是断点（空闲、设备切换），在这里切段，每段单独算。
const BREAK_MS: f64 = 50.0;

pub fn analyze(recs: &[Rec], rate: u32) -> Option<PacketStats> {
    if recs.len() < 20 {
        return None;
    }
    let mut ideal = 0.0f64;
    let mut dev_res = Vec::with_capacity(recs.len());
    let mut arr_res = Vec::with_capacity(recs.len());
    for r in recs {
        let dur_ms = f64::from(r.frames) * 1000.0 / f64::from(rate);
        dev_res.push((r.device_ms - ideal, r.device_ms));
        arr_res.push((r.arrival_ms - dur_ms - ideal, r.device_ms));
        ideal += dur_ms;
    }
    let device = segmented(&dev_res);
    let arrival = segmented(&arr_res);
    Some(PacketStats { packets: recs.len(), segments: device.1.max(arrival.1), device: device.0, arrival: arrival.0 })
}

/// (时间残差, 横轴时间)：按跳变切段，取最长的一段拟合斜率，所有段的差分合起来算抖动。
fn segmented(res: &[(f64, f64)]) -> (SourceStats, usize) {
    let mut segs: Vec<&[(f64, f64)]> = Vec::new();
    let mut start = 0;
    for i in 1..res.len() {
        if (res[i].0 - res[i - 1].0).abs() > BREAK_MS {
            segs.push(&res[start..i]);
            start = i;
        }
    }
    segs.push(&res[start..]);
    let diffs: Vec<f64> = segs.iter().flat_map(|s| s.windows(2).map(|w| w[1].0 - w[0].0)).collect();
    let mean = diffs.iter().sum::<f64>() / diffs.len().max(1) as f64;
    let jitter = (diffs.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / diffs.len().max(1) as f64).sqrt();
    let longest = segs.iter().max_by_key(|s| s.len()).unwrap();
    // 斜率的单位是 毫秒/毫秒，乘 1e6 即百万分率
    (SourceStats { jitter_ms: jitter, drift_ppm: slope(longest) * 1e6 }, segs.len())
}

/// 最小二乘斜率（残差毫秒 / 时间毫秒）。
fn slope(seg: &[(f64, f64)]) -> f64 {
    let n = seg.len() as f64;
    let (mx, my) = (seg.iter().map(|p| p.1).sum::<f64>() / n, seg.iter().map(|p| p.0).sum::<f64>() / n);
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for &(y, x) in seg {
        sxx += (x - mx) * (x - mx);
        sxy += (x - mx) * (y - my);
    }
    if sxx == 0.0 { 0.0 } else { sxy / sxx }
}

pub fn parse_csv(text: &str) -> Vec<Rec> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            Some(Rec { device_ms: f.first()?.parse().ok()?, arrival_ms: f.get(1)?.parse().ok()?, frames: f.get(2)?.parse().ok()? })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 ms 一个包，设备时钟比 QPC 快 `ppm`；设备时间戳无抖动，到达时间带 `jitter_ms` 的伪随机抖动。
    fn synth(n: usize, ppm: f64, jitter_ms: f64, gap_at: Option<usize>) -> Vec<Rec> {
        let mut seed = 12345u64;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0
        };
        let mut t = 1000.0;
        (0..n)
            .map(|k| {
                if Some(k) == gap_at {
                    t += 5000.0; // 空闲 5 秒，没有包
                }
                let dev = t;
                t += 10.0 * (1.0 + ppm * 1e-6);
                // 到达时间：该包结束时刻附近，加抖动
                Rec { device_ms: dev, arrival_ms: dev + 10.0 + rnd() * jitter_ms, frames: 480 }
            })
            .collect()
    }

    #[test]
    fn device_timestamps_show_the_clock_drift() {
        let s = analyze(&synth(6_000, 100.0, 0.0, None), 48_000).unwrap();
        assert!((s.device.drift_ppm - 100.0).abs() < 5.0, "{s:?}");
        assert!(s.device.jitter_ms < 0.01, "{s:?}");
    }

    #[test]
    fn arrival_time_shows_scheduling_jitter() {
        let s = analyze(&synth(6_000, 0.0, 4.0, None), 48_000).unwrap();
        assert!(s.arrival.jitter_ms > 1.0, "到达时间应当抖动：{s:?}");
        assert!(s.device.jitter_ms < 0.01, "设备时间戳应当平稳：{s:?}");
    }

    #[test]
    fn idle_gaps_split_segments_instead_of_polluting_the_drift() {
        let s = analyze(&synth(6_000, 0.0, 0.0, Some(3_000)), 48_000).unwrap();
        assert_eq!(s.segments, 2);
        assert!(s.device.drift_ppm.abs() < 5.0, "{s:?}");
    }

    #[test]
    fn too_few_packets_give_no_answer() {
        assert!(analyze(&synth(10, 0.0, 0.0, None), 48_000).is_none());
    }

    #[test]
    fn csv_round_trip() {
        let recs = parse_csv("device_ms,arrival_ms,frames,flags\n10.5,20.5,480,0\n30.25,40.0,441,1\nbad line\n");
        assert_eq!(recs, vec![Rec { device_ms: 10.5, arrival_ms: 20.5, frames: 480 }, Rec { device_ms: 30.25, arrival_ms: 40.0, frames: 441 }]);
    }
}
