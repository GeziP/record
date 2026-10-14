//! S3 的参照诊断：不录屏，只比较“播放流位置”与录制器给声音定的时刻是否一致。
//!
//! `s3 stimulus` 让闪屏等到测试音“出声”的那一刻才亮，这个时刻由播放流的设备位置推算；录制器却按回环采集包的设备时间戳给声音定位。
//! 两个来源对同一个端点“什么时候出声”的说法一致，S3 测出来的偏移才是录制器自己的偏移；说法不一致（本机的蓝牙端点就是这样），
//! S3 测出来的主要是两个来源的分歧，没法当通过或不通过来读。
//!
//! 这个命令在端点上放几声测试音，同时用录制器的系统音路径（回环，按设备时间戳放置）录下来，对每一声比较
//! “播放流位置认为它什么时候出声”和“录下来的声音被放在哪里”。差值的符号与 S3 相同：负数表示参照比录下来的声音早。
//! 不需要 ffmpeg，也不碰屏幕。

use crate::args::Args;
use anyhow::{Context, Result, bail};
use crossbeam_channel::unbounded;
use record_capture::audio::{AudioEvent, AudioSource, Endpoint, Kind, SourceConfig};
use record_capture::devices::Role;
use record_capture::qpc::Qpc;
use record_capture::render::{PlayClock, RenderPlayer};
use record_core::config::Config;
use record_core::syncprobe::ToneLevels;
use record_core::timeline::{AudioFormat, TimeSource};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;
/// 测试音的长度（秒），与 `s3 stimulus` 相同。
const BEEP_SECS: f64 = 0.15;
const NOT_YET: u64 = u64::MAX;
/// 单位换算：QPC 的 100 ns 与毫秒。
const HNS_PER_MS: f64 = 10_000.0;

/// 每一声的三个时刻（QPC，100 ns）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Row {
    /// 要求渲染线程播放这一声的时刻。
    asked: i64,
    /// 播放流位置推算的出声时刻（`s3 stimulus` 用它决定闪屏的时刻）。
    heard: i64,
    /// 录制器把这一声放在哪里：录下来的声音里它的起点。
    recorded: i64,
}

impl Row {
    fn reference_ms(&self) -> f64 {
        (self.heard - self.asked) as f64 / HNS_PER_MS
    }

    fn recorded_ms(&self) -> f64 {
        (self.recorded - self.asked) as f64 / HNS_PER_MS
    }

    /// 参照比录下来的声音晚多少毫秒；负数表示参照先到（与 S3 的“画面 − 声音”同号）。
    fn delta_ms(&self) -> f64 {
        (self.heard - self.recorded) as f64 / HNS_PER_MS
    }
}

/// 在从 `anchor`（QPC）起的单声道录音里找每一声：`beeps` 是（要求播放的时刻，播放流位置推算的出声时刻），
/// 在推算时刻前后 `radius` 秒里找 `freq` 的测试音；附近找不到的那一声略过。
fn locate(mono: &[f32], rate: u32, freq: f64, anchor: i64, beeps: &[(i64, i64)], radius: f64) -> Vec<Row> {
    let tone = ToneLevels::new(mono, rate, freq, 0.0);
    beeps
        .iter()
        .filter_map(|&(asked, heard)| {
            let center = (heard - anchor) as f64 / 1e7;
            let onset = tone.onset_near(center, radius)?;
            Some(Row { asked, heard, recorded: anchor + (onset * 1e7).round() as i64 })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Judgement {
    n: usize,
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
    /// 每一声的差值都在限值之内。
    usable: bool,
}

/// 判定参照能不能用：每一声的 |差值| 都不超过 `limit_ms`（与 S3 的验收限值相同）。
/// 参照自己就差到超过整个限值，S3 测出来的偏移就分不清是录制器的还是参照的，不能当通过或不通过来读。
fn judge(rows: &[Row], limit_ms: f64) -> Option<Judgement> {
    if rows.is_empty() {
        return None;
    }
    let mut d: Vec<f64> = rows.iter().map(Row::delta_ms).collect();
    d.sort_by(f64::total_cmp);
    let (min_ms, max_ms) = (d[0], d[d.len() - 1]);
    Some(Judgement { n: d.len(), median_ms: d[d.len() / 2], min_ms, max_ms, usable: min_ms.abs().max(max_ms.abs()) <= limit_ms })
}

/// 等渲染线程把测试音写进缓冲区，返回测试音第一帧的序号。
fn wait_for_beep(frame: &AtomicU64) -> Option<u64> {
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        let f = frame.load(Ordering::Acquire);
        if f != NOT_YET {
            return Some(f);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    None
}

pub fn clocks_cmd(a: &Args) -> Result<()> {
    let count: usize = a.num("beeps", 6)?;
    let interval: f64 = a.num("interval", 2.0)?;
    let freq: f64 = a.num("freq", 1000.0)?;
    let limit_ms: f64 = a.num("limit-ms", 80.0)?;
    let device = a.get("device").map(String::from);
    if count < 3 {
        bail!("--beeps 至少 3 声，少了估计不出稳定程度");
    }
    // 搜索半径取间隔的一半减 0.1 秒，免得配到邻居；出声延迟可达 0.6 秒以上，半径要容得下它
    let radius = interval / 2.0 - 0.1;
    if radius < 0.7 {
        bail!("--interval 太短：至少 1.6 秒（搜索半径取间隔的一半减 0.1 秒，要容得下 0.6 秒以上的出声延迟）");
    }

    let qpc = Qpc::new();
    // 测试音：主线程要求播放时置位，渲染线程在下一次填充缓冲区时从头播一声，并记下它第一帧的序号（与 `s3 stimulus` 一样）
    let request = Arc::new(AtomicU32::new(0));
    let first_frame = Arc::new(AtomicU64::new(NOT_YET));
    let clock = Arc::new(PlayClock::new(RATE));
    let beep_frames = (BEEP_SECS * f64::from(RATE)) as u32;
    let fade = (0.002 * f64::from(RATE)) as u32;
    let (req, first, gen_clock) = (Arc::clone(&request), Arc::clone(&first_frame), Arc::clone(&clock));
    let (mut left, mut done) = (0u32, 0u32);
    let player = RenderPlayer::start_clocked(device.clone(), Role::Console, RATE, 2, Arc::clone(&clock), move |buf| {
        let r = req.swap(0, Ordering::AcqRel);
        if r > 0 {
            (left, done) = (r, 0);
            first.store(gen_clock.written(), Ordering::Release);
        }
        for frame in buf.chunks_exact_mut(2) {
            if left == 0 {
                break;
            }
            let ramp = (done as f32 / fade as f32).min(1.0);
            frame.fill(ramp * 0.15 * (2.0 * std::f64::consts::PI * freq * f64::from(done) / f64::from(RATE)).sin() as f32);
            left -= 1;
            done += 1;
        }
    })
    .map_err(anyhow::Error::msg)
    .context("在播放设备上播放测试音")?;

    // 录制器的系统音路径：回环、按设备时间戳放置（`system_time_source` 的默认值）
    let pcm = Arc::new(Mutex::new(Vec::<f32>::new()));
    let sink = Arc::clone(&pcm);
    let endpoint = device.map_or(Endpoint::Default(Role::Console), Endpoint::Id);
    let source_cfg = SourceConfig::new("clocks", Kind::Loopback, endpoint, Config::default().timeline_config(AudioFormat { rate: RATE, channels: 2 }, TimeSource::DeviceQpc));
    let (events_tx, events) = unbounded::<AudioEvent>();
    let source = AudioSource::start(source_cfg, qpc, events_tx, Box::new(move |d| sink.lock().unwrap().extend_from_slice(d))).context("启动回环采集")?;
    let up = Instant::now() + Duration::from_secs(5);
    while source.live().last_packet_hns() == 0 {
        if Instant::now() > up {
            bail!("回环 5 秒内没有出过包：{:?}", events.try_iter().collect::<Vec<_>>());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // 让播放流的设备位置读数稳定下来
    std::thread::sleep(Duration::from_secs(1));

    println!("在端点上放 {count} 声 {freq} Hz 的短音（每 {interval} 秒一声，音量很低）……");
    let mut beeps = Vec::with_capacity(count);
    for i in 0..count {
        first_frame.store(NOT_YET, Ordering::Release);
        let asked = qpc.now_hns();
        request.store(beep_frames, Ordering::Release);
        let frame = wait_for_beep(&first_frame).context("渲染线程没有取走测试音")?;
        let heard = clock.heard_at(frame).context("还没有读到播放设备的位置")?;
        beeps.push((asked, heard));
        if i + 1 < count {
            std::thread::sleep(Duration::from_secs_f64(interval));
        }
    }
    // 最后一声要等它走完设备延迟、进到回环里
    std::thread::sleep(Duration::from_millis(1_500));
    player.stop();
    let anchor = source.stop().anchor;

    let name = events.try_iter().find_map(|e| if let AudioEvent::Opened { device, .. } = e { Some(device.name) } else { None });
    let mono: Vec<f32> = pcm.lock().unwrap().chunks_exact(2).map(|c| (c[0] + c[1]) * 0.5).collect();
    let rows = locate(&mono, RATE, freq, anchor, &beeps, radius);
    println!("端点：{}；录下 {:.1} 秒", name.as_deref().unwrap_or("（未知）"), mono.len() as f64 / f64::from(RATE));
    println!("参照 = 播放流位置推算的出声时刻；录制 = 录制器放置这一声的时刻；都从写入测试音起算，单位 ms。");
    println!("  序号  参照(ms)  录制(ms)  参照−录制(ms)");
    for (i, r) in rows.iter().enumerate() {
        println!("  {:>3}  {:>+8.1}  {:>+8.1}  {:>+12.1}", i + 1, r.reference_ms(), r.recorded_ms(), r.delta_ms());
    }
    let Some(j) = judge(&rows, limit_ms).filter(|j| j.n >= 3) else {
        bail!("只在录下的声音里找到 {} 声（至少需要 3 声）。检查系统音是否真的录到了测试音、有没有别的 {freq} Hz 声音", rows.len());
    };
    println!("配上 {}/{count} 声；参照−录制：中位数 {:+.1} ms，范围 {:+.1}～{:+.1} ms", j.n, j.median_ms, j.min_ms, j.max_ms);
    println!("说明：这个差值（负数表示参照比录下来的声音早，与 S3 同号）会进到同一次 s3 stimulus 加 s3 analyze 测出的偏移里；同一次之内几乎不变，每次重新打开播放流可能不同。");
    if j.usable {
        println!(
            "结论：这一次参照可用（每一声的 |差值| 都不超过 {limit_ms} ms）。蓝牙等端点每次打开播放流报告的延迟可能不一样：S3 前后各跑一次，两次都可用，S3 的偏移才可以当录制器的偏移来读（偏移里仍含有上面这个差值）。"
        );
        Ok(())
    } else {
        bail!(
            "结论：这一次合成信号在这个端点上测不准（有的差值超过 {limit_ms} ms）：播放流位置与回环时间戳对“出声时刻”的说法差得太多，S3 在这里测出的偏移主要是这两个来源的分歧，不能当通过或不通过来读。\
             录制器的真实对位取决于哪一个说法对：回环时间戳对，偏移只剩画面一侧的延迟；播放流位置对，还要再加上这个差值。要确定，需要外部测量（比如手机同时拍屏幕与录声音）"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 任意的 QPC 起点（100 ns）。
    const ANCHOR: i64 = 5_000_000_000;
    /// 起点检测自己的偏差：2 毫秒淡入加 10 毫秒窗口取中心，合成信号上约晚 2 毫秒（S3 的分析程序用同一个检测，偏差相同）。
    const ONSET_MS: f64 = 3.0;

    fn hns(secs: f64) -> i64 {
        ANCHOR + (secs * 1e7).round() as i64
    }

    /// 合成单声道录音：`beeps` 里每个时刻（相对录音起点的秒数）起一声 0.15 秒的 1 kHz 音，2 毫秒淡入。
    fn recording(beeps: &[f64], secs: f64) -> Vec<f32> {
        (0..(secs * f64::from(RATE)) as usize)
            .map(|n| {
                let t = n as f64 / f64::from(RATE);
                beeps.iter().find(|&&b| t >= b && t < b + BEEP_SECS).map_or(0.0, |&b| ((t - b) / 0.002).min(1.0) as f32 * 0.15 * (2.0 * std::f64::consts::PI * 1000.0 * t).sin() as f32)
            })
            .collect()
    }

    fn row(heard_minus_recorded_ms: f64) -> Row {
        Row { asked: 0, heard: (heard_minus_recorded_ms * HNS_PER_MS).round() as i64, recorded: 0 }
    }

    #[test]
    fn each_beep_is_compared_with_where_the_reference_said_it_would_sound() {
        let heard = |t: f64, off: f64| (hns(t), hns(t + off));
        // 录音里的测试音在 1、3、5 秒；参照说它们比录下来的晚 25 毫秒出声
        let rows = locate(&recording(&[1.0, 3.0, 5.0], 7.0), RATE, 1000.0, ANCHOR, &[heard(1.0, 0.025), heard(3.0, 0.025), heard(5.0, 0.025)], 0.9);
        assert_eq!(rows.len(), 3);
        for r in &rows {
            assert!((r.delta_ms() - 25.0).abs() < ONSET_MS, "{r:?}");
        }
    }

    #[test]
    fn a_reference_that_is_early_by_a_quarter_second_shows_up_as_negative() {
        let rows = locate(&recording(&[1.0, 3.0, 5.0], 7.0), RATE, 1000.0, ANCHOR, &[(hns(0.7), hns(0.725)), (hns(2.7), hns(2.725)), (hns(4.7), hns(4.725))], 0.9);
        assert_eq!(rows.len(), 3);
        for r in &rows {
            assert!((r.delta_ms() + 275.0).abs() < ONSET_MS, "{r:?}");
            // 写入后录制说 0.3 秒出声，参照说 0.025 秒
            assert!((r.recorded_ms() - 300.0).abs() < ONSET_MS && (r.reference_ms() - 25.0).abs() < 0.01, "{r:?}");
        }
    }

    #[test]
    fn a_delay_of_over_half_a_second_is_still_found_inside_the_search_radius() {
        // 参照比录下来的晚 0.6 秒（有的蓝牙耳机的出声延迟就是这个量级）
        let rows = locate(&recording(&[1.0, 3.0, 5.0], 7.0), RATE, 1000.0, ANCHOR, &[(hns(1.6), hns(1.6)), (hns(3.6), hns(3.6)), (hns(5.6), hns(5.6))], 0.9);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| (r.delta_ms() - 600.0).abs() < ONSET_MS), "{rows:?}");
    }

    #[test]
    fn a_beep_that_was_never_recorded_is_skipped() {
        let rows = locate(&recording(&[1.0, 5.0], 7.0), RATE, 1000.0, ANCHOR, &[(hns(1.0), hns(1.0)), (hns(3.0), hns(3.0)), (hns(5.0), hns(5.0))], 0.9);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn agreeing_clocks_are_usable() {
        let j = judge(&[row(18.0), row(20.5), row(19.1), row(17.9)], 80.0).unwrap();
        assert!(j.usable);
        assert_eq!((j.n, j.min_ms, j.max_ms), (4, 17.9, 20.5));
    }

    #[test]
    fn clocks_a_quarter_second_apart_are_not_usable() {
        let j = judge(&[row(-275.0), row(-274.8), row(-276.1)], 80.0).unwrap();
        assert!(!j.usable);
        assert_eq!(j.median_ms, -275.0);
    }

    #[test]
    fn one_wild_beep_is_enough_to_spoil_the_verdict() {
        assert!(!judge(&[row(10.0), row(12.0), row(11.0), row(-150.0)], 80.0).unwrap().usable);
    }

    #[test]
    fn the_limit_itself_is_still_usable() {
        assert!(judge(&[row(80.0), row(-80.0), row(0.0)], 80.0).unwrap().usable);
        assert!(!judge(&[row(80.1), row(0.0), row(0.0)], 80.0).unwrap().usable);
    }

    #[test]
    fn no_beeps_no_verdict() {
        assert_eq!(judge(&[], 80.0), None);
    }
}
