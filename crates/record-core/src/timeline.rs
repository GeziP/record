//! 时间线修复：补静音与漂移修正（设计文档 5.5）。
//!
//! 每个音频数据包带一个 QPC 时间戳。已写入的采样数与“按 QPC 推算应写入的采样数”之差为 Δ：
//! - Δ 超过阈值：补 Δ 个静音采样（回环空闲、设备停摆、时钟偏慢）；
//! - Δ 低于负阈值：丢弃多余采样（时钟偏快）。
//!
//! 阈值带滞回：触发后不是修正到 0，而是留下 `settle` 的残差，避免时间戳抖动引起来回修正。
//! 没有数据包时，由调用方每 10 ms 调用一次 [`AudioTimeline::tick`]，让回环空闲期的时间线保持连续。
//!
//! 输出用回调按块交付（静音每块最多 1 秒），长时间卡顿后补静音也不会一次分配大量内存。

use crate::clock::{Hns, frames_to_hns, hns_to_frames, ms_to_hns};
use serde::{Deserialize, Serialize};

/// 交错 f32 采样的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub rate: u32,
    pub channels: u16,
}

/// 时间戳来源。每一路设备各选一种，由 S1 实测决定（设计文档 5.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeSource {
    /// 设备给出的 QPC 时间戳（`GetBuffer` 的 `qpcPosition`）。
    #[default]
    DeviceQpc,
    /// 包到达时间减去包时长。
    ArrivalMinusDuration,
}

/// WASAPI 包标志中与时间线有关的三个。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacketFlags {
    /// `AUDCLNT_BUFFERFLAGS_SILENT`：按全零处理，不读缓冲内容。
    pub silent: bool,
    /// `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`：设备时间戳不可信。
    pub timestamp_error: bool,
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`：只计数，由 Δ 补齐。
    pub discontinuity: bool,
}

/// 采集线程交给时间线的一个数据包。
#[derive(Debug, Clone, Copy)]
pub struct AudioPacket<'a> {
    /// 设备给出的该包首帧的 QPC 时间（100 ns）。设备没给时为 `None`。
    pub device_qpc: Option<Hns>,
    /// 采集线程取到该包时的 QPC 时间（100 ns）。
    pub arrival_qpc: Hns,
    pub frames: usize,
    /// 交错 f32，长度应为 `frames × channels`。`silent` 包可以传空切片。
    pub samples: &'a [f32],
    pub flags: PacketFlags,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineConfig {
    pub format: AudioFormat,
    pub time_source: TimeSource,
    /// 数据包到达时触发修正的偏差阈值（毫秒）。设计初值 40，由 S1 调定。
    pub tolerance_ms: u32,
    /// 没有数据包时（`tick`）触发补静音的阈值（毫秒）。比 `tolerance_ms` 宽，
    /// 否则设备数据只要晚到 40 ms，就会先被补上静音、迟到的真实数据再被当作重叠丢掉。初值 100。
    pub idle_tolerance_ms: u32,
    /// 修正后保留的残差（毫秒），即滞回宽度。初值 10。
    pub settle_ms: u32,
    /// 单次补静音达到该时长就记 `large_gap`（毫秒）。设计值 5000。
    pub large_gap_ms: u32,
}

impl TimelineConfig {
    pub fn new(format: AudioFormat) -> Self {
        Self { format, time_source: TimeSource::DeviceQpc, tolerance_ms: 40, idle_tolerance_ms: 100, settle_ms: 10, large_gap_ms: 5000 }
    }
}

/// 补静音的原因。分开统计，因为它们的含义完全不同：回环设备没有声音时没有包到达，空闲补齐是常态；
/// 麦克风出现空闲、或者包与包之间出现空档，才说明采集出了问题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// 第一个数据包之前：打开设备的耗时，加上设备时间戳相对采集时刻的延迟
    /// （回环包的时间戳是“将在扬声器播放的时刻”，比读到它的时刻晚一个播放延迟；本机不开麦克风时约 0.56 秒）。
    LeadIn,
    /// 收到包时发现与上一个包之间有空档：包到得晚，说明设备或系统卡过。
    Gap,
    /// 包准时到达，但设备时间戳跑到了前面：播放延迟在增大（本机开保活流时开头 0.56 秒内每包 +10 ms）。
    /// 时间线仍然按设备时间对齐（补静音），但这不是异常。
    LatencyShift,
    /// 流正常、但没有包到达。
    Idle,
    /// 流已中断（设备被拔、切换默认设备、重连等待期间）。
    Outage,
    /// 收尾时补到停止时刻（最后一个包到停止之间，正常情况下不超过一个包的时长）。
    Tail,
}

/// 写入 `session.json` 诊断的计数。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineStats {
    pub packets: u64,
    pub frames_written: u64,
    /// 合成静音总帧数 = `lead_in_frames` + `gap_filled_frames` + `latency_shift_frames` + `idle_filled_frames` + `outage_filled_frames` + `tail_frames`。
    pub silence_filled_frames: u64,
    pub dropped_frames: u64,
    /// 从开始采集到第一个数据包之间的静音：打开设备的耗时 + 设备时间戳相对采集时刻的延迟
    /// （回环是播放延迟）+ 回环无声时等待第一个包的时间。**不是异常**，但对热键到开始录音的延迟有参考价值。
    pub lead_in_frames: u64,
    /// 收到包时发现的包间空档，且包到得晚。**异常**：设备或系统卡顿。
    pub gap_filled_frames: u64,
    /// 包准时到达、但设备时间戳跑到了前面（播放延迟增大）而补的静音。**不是异常**。
    pub latency_shift_frames: u64,
    /// 没有包到达期间补的静音。回环设备无声时是**常态**，麦克风出现则不正常。
    pub idle_filled_frames: u64,
    /// 流中断期间补的静音。**异常**。
    pub outage_filled_frames: u64,
    /// 收尾时补到停止时刻的静音：最后一个包到停止之间，正常情况下不超过一个包的时长。
    pub tail_frames: u64,
    /// 包间空档的次数（对应 `gap_filled_frames`）。
    pub fill_events: u64,
    /// 空闲段数：连续的空闲补齐算一段，不是每次 tick 算一次。
    pub idle_episodes: u64,
    pub drop_events: u64,
    pub large_gaps: u64,
    /// 相邻两个包的时间戳之间出现的间隙 / 重叠次数（超过阈值才算）。
    pub pts_gaps: u64,
    pub pts_overlaps: u64,
    /// 设备时间戳不可用，改用“到达时间 − 包时长”的次数。
    pub timestamp_fallbacks: u64,
    pub discontinuities: u64,
    pub silent_packets: u64,
    pub max_abs_delta_frames: u64,
}

/// 一次 `push_packet` 或 `tick` 的结果，供调用方决定要不要告警。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    pub filled_frames: u64,
    pub dropped_frames: u64,
    /// 本次一次性补的静音达到了 `large_gap_ms`。
    pub large_gap: bool,
}

pub struct AudioTimeline {
    cfg: TimelineConfig,
    /// 该轨的时间起点：开始采集的 QPC。首包到来前的空档按补静音处理。
    anchor: Hns,
    written: u64,
    prev_end: Option<Hns>,
    /// 上一个包的到达时间，用来判断本包是否准时。
    prev_arrival: Option<Hns>,
    /// 正处在一段空闲补齐中（还没有新包到达）。
    idle_open: bool,
    /// 是否已经成功启动过流（区分“打开设备耗时”与“重连中断”）。
    started_once: bool,
    stats: TimelineStats,
    peak: f32,
    tol_frames: i64,
    idle_tol_frames: i64,
    settle_frames: i64,
    large_gap_frames: i64,
    zeros: Vec<f32>,
}

impl AudioTimeline {
    pub fn new(cfg: TimelineConfig, anchor: Hns) -> Self {
        let rate = cfg.format.rate;
        let ms = |v: u32| hns_to_frames(ms_to_hns(i64::from(v)), rate);
        Self {
            cfg,
            anchor,
            written: 0,
            prev_end: None,
            prev_arrival: None,
            idle_open: false,
            started_once: false,
            stats: TimelineStats::default(),
            peak: 0.0,
            tol_frames: ms(cfg.tolerance_ms),
            idle_tol_frames: ms(cfg.idle_tolerance_ms),
            settle_frames: ms(cfg.settle_ms),
            large_gap_frames: ms(cfg.large_gap_ms),
            zeros: Vec::new(),
        }
    }

    pub fn anchor(&self) -> Hns {
        self.anchor
    }

    pub fn written_frames(&self) -> u64 {
        self.written
    }

    pub fn stats(&self) -> &TimelineStats {
        &self.stats
    }

    /// 取走自上次调用以来输出采样的最大绝对值（健康监测每秒调用一次）。
    pub fn take_peak(&mut self) -> f32 {
        std::mem::take(&mut self.peak)
    }

    /// 处理一个数据包。需要写出的采样（可能先有补的静音，再有包内数据）通过 `emit` 按块交付。
    pub fn push_packet(&mut self, pkt: &AudioPacket<'_>, emit: &mut dyn FnMut(&[f32])) -> Report {
        let rate = self.cfg.format.rate;
        let ch = usize::from(self.cfg.format.channels);
        let dur = frames_to_hns(pkt.frames as i64, rate);

        self.stats.packets += 1;
        self.idle_open = false;
        if pkt.flags.discontinuity {
            self.stats.discontinuities += 1;
        }
        if pkt.flags.silent {
            self.stats.silent_packets += 1;
        }

        let start = self.packet_start(pkt, dur);

        let first = self.prev_end.is_none();
        let tol_hns = ms_to_hns(i64::from(self.cfg.tolerance_ms));
        // 到达间隔不超过“包时长 + 容差”算准时。包是否准时是区分“卡顿造成的空档”和“延迟增大”的依据
        let on_time = self.prev_arrival.is_none_or(|pa| pkt.arrival_qpc - pa <= dur + tol_hns);
        self.prev_arrival = Some(pkt.arrival_qpc);
        if let Some(prev_end) = self.prev_end {
            let gap = start - prev_end;
            if gap > tol_hns {
                self.stats.pts_gaps += 1;
            } else if gap < -tol_hns {
                self.stats.pts_overlaps += 1;
            }
        }
        self.prev_end = Some(start + dur);

        let delta = hns_to_frames(start - self.anchor, rate) - self.written as i64;
        self.stats.max_abs_delta_frames = self.stats.max_abs_delta_frames.max(delta.unsigned_abs());

        let mut report = Report::default();
        let mut skip = 0usize;
        if delta > self.tol_frames {
            let kind = if first {
                Fill::LeadIn
            } else if on_time {
                Fill::LatencyShift
            } else {
                Fill::Gap
            };
            report = self.fill_silence((delta - self.settle_frames) as u64, kind, emit);
        } else if delta < -self.tol_frames {
            let drop = ((-delta) - self.settle_frames).max(0) as usize;
            skip = drop.min(pkt.frames);
            report.dropped_frames = skip as u64;
            self.stats.dropped_frames += skip as u64;
            self.stats.drop_events += 1;
        }

        let keep = pkt.frames - skip;
        if keep > 0 {
            if pkt.flags.silent || pkt.samples.is_empty() {
                self.emit_zeros(keep, emit);
            } else {
                let data = &pkt.samples[skip * ch..pkt.frames * ch];
                for s in data {
                    let a = s.abs();
                    if a > self.peak {
                        self.peak = a;
                    }
                }
                emit(data);
            }
            self.written += keep as u64;
            self.stats.frames_written += keep as u64;
        }
        report
    }

    /// 每次成功启动采集流后立刻调用：把此前还没补的时间线精确补到 `now`。
    /// 第一次是打开设备的耗时，记为 `lead_in_frames`；之后（重连、切换默认设备）记为中断。
    pub fn stream_started(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        let kind = if self.started_once { Fill::Outage } else { Fill::LeadIn };
        self.started_once = true;
        let delta = hns_to_frames(now - self.anchor, self.cfg.format.rate) - self.written as i64;
        if delta > 0 {
            self.stats.max_abs_delta_frames = self.stats.max_abs_delta_frames.max(delta.unsigned_abs());
            self.fill_silence(delta as u64, kind, emit)
        } else {
            Report::default()
        }
    }

    /// 流正常、没有数据包期间由调用方每 10 ms 调用一次：按 QPC 推算，落后超过 `idle_tolerance_ms` 才补静音。
    pub fn tick(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        self.tick_as(now, Fill::Idle, emit)
    }

    /// 流已中断（重连等待期间）时的 `tick`：补的静音记为中断而不是空闲。
    pub fn tick_outage(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        self.tick_as(now, Fill::Outage, emit)
    }

    fn tick_as(&mut self, now: Hns, kind: Fill, emit: &mut dyn FnMut(&[f32])) -> Report {
        // 第一个包之前的等待都算首包前，不论是打开设备慢、设备延迟大，还是回环一直没有声音
        let kind = if kind == Fill::Idle && self.prev_end.is_none() { Fill::LeadIn } else { kind };
        let delta = hns_to_frames(now - self.anchor, self.cfg.format.rate) - self.written as i64;
        if delta > self.idle_tol_frames {
            self.stats.max_abs_delta_frames = self.stats.max_abs_delta_frames.max(delta.unsigned_abs());
            self.fill_silence((delta - self.settle_frames) as u64, kind, emit)
        } else {
            Report::default()
        }
    }

    /// 收尾时调用：把时间线精确补到 `now`（没有阈值，也不留残差），让这一轨的长度等于录制时长。
    /// 已经超过 `now` 时什么也不做。
    pub fn finish(&mut self, now: Hns, emit: &mut dyn FnMut(&[f32])) -> Report {
        let delta = hns_to_frames(now - self.anchor, self.cfg.format.rate) - self.written as i64;
        if delta > 0 { self.fill_silence(delta as u64, Fill::Tail, emit) } else { Report::default() }
    }

    fn packet_start(&mut self, pkt: &AudioPacket<'_>, dur: Hns) -> Hns {
        let from_arrival = pkt.arrival_qpc - dur;
        match self.cfg.time_source {
            TimeSource::ArrivalMinusDuration => from_arrival,
            TimeSource::DeviceQpc => match pkt.device_qpc {
                Some(t) if !pkt.flags.timestamp_error => t,
                _ => {
                    self.stats.timestamp_fallbacks += 1;
                    from_arrival
                }
            },
        }
    }

    fn fill_silence(&mut self, frames: u64, kind: Fill, emit: &mut dyn FnMut(&[f32])) -> Report {
        self.emit_zeros(frames as usize, emit);
        self.written += frames;
        self.stats.frames_written += frames;
        self.stats.silence_filled_frames += frames;
        match kind {
            Fill::LeadIn => self.stats.lead_in_frames += frames,
            Fill::Gap => {
                self.stats.gap_filled_frames += frames;
                self.stats.fill_events += 1;
            }
            Fill::Idle => {
                self.stats.idle_filled_frames += frames;
                if !self.idle_open {
                    self.idle_open = true;
                    self.stats.idle_episodes += 1;
                }
            }
            Fill::LatencyShift => self.stats.latency_shift_frames += frames,
            Fill::Outage => self.stats.outage_filled_frames += frames,
            Fill::Tail => self.stats.tail_frames += frames,
        }
        let large = frames as i64 >= self.large_gap_frames;
        if large {
            self.stats.large_gaps += 1;
        }
        Report { filled_frames: frames, dropped_frames: 0, large_gap: large }
    }

    fn emit_zeros(&mut self, mut frames: usize, emit: &mut dyn FnMut(&[f32])) {
        let ch = usize::from(self.cfg.format.channels);
        let chunk_frames = self.cfg.format.rate as usize; // 每块最多 1 秒
        if self.zeros.len() < chunk_frames.min(frames) * ch {
            self.zeros.resize(chunk_frames.min(frames) * ch, 0.0);
        }
        while frames > 0 {
            let n = frames.min(chunk_frames);
            emit(&self.zeros[..n * ch]);
            frames -= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::HNS_PER_SEC;

    const RATE: u32 = 48_000;
    const CH: u16 = 2;

    fn cfg() -> TimelineConfig {
        TimelineConfig::new(AudioFormat { rate: RATE, channels: CH })
    }

    fn dur(frames: usize) -> Hns {
        frames_to_hns(frames as i64, RATE)
    }

    /// 构造一个设备时间戳为 `t` 的包，数据全部为 `value`；到达时间为包结束时刻。
    fn packet(t: Hns, frames: usize, data: &[f32]) -> AudioPacket<'_> {
        AudioPacket { device_qpc: Some(t), arrival_qpc: t + dur(frames), frames, samples: data, flags: PacketFlags::default() }
    }

    fn run_push(tl: &mut AudioTimeline, pkt: &AudioPacket<'_>, out: &mut Vec<f32>) -> Report {
        tl.push_packet(pkt, &mut |s| out.extend_from_slice(s))
    }

    #[test]
    fn contiguous_packets_pass_through_unchanged() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.25f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..100 {
            let r = run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
            assert_eq!(r, Report::default());
        }
        assert_eq!(out.len(), 100 * 480 * 2);
        assert!(out.iter().all(|&s| s == 0.25));
        let st = tl.stats();
        assert_eq!(st.silence_filled_frames, 0);
        assert_eq!(st.dropped_frames, 0);
        assert_eq!(st.frames_written, 48_000);
    }

    #[test]
    fn leading_gap_is_filled_and_leaves_settle_residual() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        // 首包在起点之后 100 ms 才到：应补 100 ms − 10 ms 残差 = 90 ms 静音
        let r = run_push(&mut tl, &packet(ms_to_hns(100), 480, &data), &mut out);
        assert_eq!(r.filled_frames, 4_320);
        assert!(!r.large_gap);
        assert_eq!(out.len(), (4_320 + 480) * 2);
        assert!(out[..4_320 * 2].iter().all(|&s| s == 0.0));
        assert!(out[4_320 * 2..].iter().all(|&s| s == 0.5));
        assert_eq!(tl.written_frames(), 4_800);
    }

    #[test]
    fn timestamp_jitter_inside_tolerance_is_ignored() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..200i64 {
            // 时间戳在 ±30 ms 内抖动，但帧数是连续的
            let jitter = if k % 2 == 0 { ms_to_hns(30) } else { -ms_to_hns(30) };
            run_push(&mut tl, &packet(dur(480) * k + jitter, 480, &data), &mut out);
        }
        let st = tl.stats();
        assert_eq!(st.silence_filled_frames, 0);
        assert_eq!(st.dropped_frames, 0);
        assert_eq!(out.len(), 200 * 480 * 2);
    }

    #[test]
    fn idle_ticks_keep_the_timeline_continuous() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut total = 0usize;
        for step in 1..=1_000i64 {
            let now = ms_to_hns(10) * step; // 10 秒，每 10 ms 一次
            tl.tick(now, &mut |s| total += s.len());
            let exp = hns_to_frames(now, RATE);
            let lag = exp - tl.written_frames() as i64;
            assert!((0..=4_800).contains(&lag), "第 {step} 次：落后 {lag} 帧"); // 空闲阈值 100 ms
        }
        assert_eq!(total, tl.written_frames() as usize * 2);
        assert!(tl.stats().silence_filled_frames > 470_000);
    }

    #[test]
    fn packet_after_idle_continues_without_big_correction() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut out = Vec::new();
        for step in 1..=300i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s)); // 3 秒空闲
        }
        let before = tl.written_frames();
        let data = vec![0.5f32; 480 * 2];
        let r = run_push(&mut tl, &packet(ms_to_hns(3_000), 480, &data), &mut out);
        assert_eq!(r.dropped_frames, 0, "{r:?}");
        assert!(r.filled_frames <= 4_800, "{r:?}");
        assert_eq!(tl.written_frames() - before, 480 + r.filled_frames);
    }

    #[test]
    fn short_delivery_stall_does_not_lose_real_audio() {
        // 设备在 100 ms 内没出包，随后一次性交付这 100 ms 的数据：tick 不应抢先补静音
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        for step in 2..=11i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        assert_eq!(tl.stats().silence_filled_frames, 0);
        for k in 1..=10i64 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
        }
        let st = tl.stats();
        assert_eq!((st.silence_filled_frames, st.dropped_frames), (0, 0));
        assert_eq!(out.len(), 11 * 480 * 2);
        assert!(out.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn long_delivery_stall_trades_late_audio_for_silence_and_stays_aligned() {
        // 卡顿 300 ms：tick 先补了静音，随后交付的迟到数据里有一部分与已补的静音重叠，被丢弃。
        // 时间线始终与 QPC 一致，补的静音量与丢的数据量相抵。
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        for step in 2..=31i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        assert!(tl.stats().silence_filled_frames > 0);
        for k in 1..=99i64 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out); // 迟到的积压 + 后续数据
        }
        let st = tl.stats().clone();
        assert!(st.dropped_frames > 0, "{st:?}");
        let exp = hns_to_frames(dur(480) * 100, RATE);
        let diff = (tl.written_frames() as i64 - exp).abs();
        assert!(diff <= 1_920 + 480, "写入 {} 期望 {}", tl.written_frames(), exp);
    }

    #[test]
    fn finish_pads_exactly_to_the_end_time() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut total = 0usize;
        tl.tick(ms_to_hns(60), &mut |s| total += s.len());
        let r = tl.finish(ms_to_hns(75), &mut |s| total += s.len());
        assert_eq!(tl.written_frames() as i64, hns_to_frames(ms_to_hns(75), RATE));
        assert_eq!(total, tl.written_frames() as usize * 2);
        assert!(r.filled_frames > 0);
        // 已经超过结束时间则什么也不做
        assert_eq!(tl.finish(ms_to_hns(10), &mut |_| panic!("不应输出")), Report::default());
    }

    fn simulate_drift(ppm: f64, seconds: i64) -> (AudioTimeline, usize) {
        // 设备采样时钟比 QPC 快 `ppm`：每个 480 帧的包在 QPC 上只占 10 ms / (1 + ppm·1e-6)
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.3f32; 480 * 2];
        let mut total = 0usize;
        let step_real = 10.0 * 10_000.0 / (1.0 + ppm * 1e-6); // 100ns 单位
        let n = (seconds as f64 * 1_000.0 / (10.0 / (1.0 + ppm * 1e-6))) as i64;
        for k in 0..n {
            let t = (k as f64 * step_real).round() as Hns;
            let p = packet(t, 480, &data);
            tl.push_packet(&p, &mut |s| total += s.len());
        }
        (tl, total)
    }

    #[test]
    fn fast_device_clock_is_corrected_by_dropping() {
        let (tl, total) = simulate_drift(200.0, 600);
        let st = tl.stats();
        // 10 分钟 × 200 ppm = 120 ms 漂移，应该被修正掉，且是靠丢弃
        assert!(st.dropped_frames > 0 && st.silence_filled_frames == 0, "{st:?}");
        let exp = hns_to_frames(600 * HNS_PER_SEC, RATE);
        assert!((tl.written_frames() as i64 - exp).abs() <= 1_920 + 480, "写入 {} 期望 {}", tl.written_frames(), exp);
        assert_eq!(total, tl.written_frames() as usize * 2);
    }

    #[test]
    fn slow_device_clock_is_corrected_by_filling() {
        let (tl, _) = simulate_drift(-200.0, 600);
        let st = tl.stats();
        assert!(st.silence_filled_frames > 0 && st.dropped_frames == 0, "{st:?}");
        let exp = hns_to_frames(600 * HNS_PER_SEC, RATE);
        assert!((tl.written_frames() as i64 - exp).abs() <= 1_920 + 480);
    }

    #[test]
    fn long_stall_is_filled_in_one_second_chunks_and_flagged() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut max_chunk = 0usize;
        let mut total = 0usize;
        let r = tl.tick(6 * HNS_PER_SEC, &mut |s| {
            max_chunk = max_chunk.max(s.len());
            total += s.len();
        });
        assert!(r.large_gap);
        assert_eq!(tl.stats().large_gaps, 1);
        assert!(max_chunk <= RATE as usize * 2, "单块 {max_chunk}");
        assert_eq!(total, r.filled_frames as usize * 2);
        assert!(r.filled_frames > 5 * RATE as u64);
    }

    #[test]
    fn silent_flag_forces_zeros_even_if_buffer_has_data() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let garbage = vec![0.9f32; 480 * 2];
        let mut p = packet(0, 480, &garbage);
        p.flags.silent = true;
        let mut out = Vec::new();
        run_push(&mut tl, &p, &mut out);
        assert_eq!(out.len(), 480 * 2);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(tl.stats().silent_packets, 1);
        assert_eq!(tl.take_peak(), 0.0);
    }

    #[test]
    fn timestamp_error_falls_back_to_arrival_minus_duration() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.2f32; 480 * 2];
        let mut p = packet(ms_to_hns(5_000), 480, &data); // 设备时间戳明显有误
        p.arrival_qpc = dur(480); // 真实到达：第一个包
        p.flags.timestamp_error = true;
        let mut out = Vec::new();
        let r = run_push(&mut tl, &p, &mut out);
        assert_eq!(r, Report::default());
        assert_eq!(tl.stats().timestamp_fallbacks, 1);
        assert_eq!(out.len(), 480 * 2);
    }

    #[test]
    fn arrival_time_source_ignores_device_timestamps() {
        let mut c = cfg();
        c.time_source = TimeSource::ArrivalMinusDuration;
        let mut tl = AudioTimeline::new(c, 0);
        let data = vec![0.2f32; 480 * 2];
        let mut p = packet(ms_to_hns(5_000), 480, &data);
        p.arrival_qpc = dur(480);
        let mut out = Vec::new();
        let r = run_push(&mut tl, &p, &mut out);
        assert_eq!(r, Report::default());
        assert_eq!(tl.stats().timestamp_fallbacks, 0);
    }

    #[test]
    fn missing_device_timestamp_counts_as_fallback() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.2f32; 480 * 2];
        let mut p = packet(0, 480, &data);
        p.device_qpc = None;
        p.arrival_qpc = dur(480);
        run_push(&mut tl, &p, &mut Vec::new());
        assert_eq!(tl.stats().timestamp_fallbacks, 1);
    }

    #[test]
    fn pts_gap_and_overlap_are_counted() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        run_push(&mut tl, &packet(ms_to_hns(10) + ms_to_hns(100), 480, &data), &mut out); // 间隙 100 ms
        run_push(&mut tl, &packet(ms_to_hns(10) + ms_to_hns(100) + ms_to_hns(10) - ms_to_hns(90), 480, &data), &mut out); // 重叠 90 ms
        let st = tl.stats();
        assert_eq!(st.pts_gaps, 1);
        assert_eq!(st.pts_overlaps, 1);
    }

    #[test]
    fn peak_is_tracked_and_resets_when_taken() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut data = vec![0.1f32; 480 * 2];
        data[100] = -0.7;
        run_push(&mut tl, &packet(0, 480, &data), &mut Vec::new());
        assert!((tl.take_peak() - 0.7).abs() < 1e-6);
        assert_eq!(tl.take_peak(), 0.0);
    }

    #[test]
    fn discontinuity_flag_is_counted() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut p = packet(0, 480, &data);
        p.flags.discontinuity = true;
        run_push(&mut tl, &p, &mut Vec::new());
        assert_eq!(tl.stats().discontinuities, 1);
    }

    /// 四类补静音加起来必须等于总数，任何一次补静音都不能漏记。
    fn assert_categories_sum_up(st: &TimelineStats) {
        assert_eq!(st.silence_filled_frames, st.lead_in_frames + st.gap_filled_frames + st.latency_shift_frames + st.idle_filled_frames + st.outage_filled_frames + st.tail_frames, "{st:?}");
    }

    #[test]
    fn stream_start_fill_is_lead_in_the_first_time_and_outage_afterwards() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut out = Vec::new();
        // 打开设备花了 600 ms：精确补到启动时刻，记为首包前
        let r = tl.stream_started(ms_to_hns(600), &mut |s| out.extend_from_slice(s));
        assert_eq!(r.filled_frames, 600 * 48);
        let st = tl.stats();
        assert_eq!(st.lead_in_frames, 600 * 48);
        assert_eq!((st.fill_events, st.gap_filled_frames, st.idle_filled_frames, st.idle_episodes, st.outage_filled_frames), (0, 0, 0, 0, 0));
        // 紧接着的第一个包不再被当成空档
        let data = vec![0.5f32; 480 * 2];
        run_push(&mut tl, &packet(ms_to_hns(605), 480, &data), &mut out);
        assert_eq!(tl.stats().fill_events, 0);
        // 之后连接断了、重连成功：这段时间记为中断，而不是首包前
        let mut tl2 = AudioTimeline::new(cfg(), 0);
        tl2.stream_started(ms_to_hns(100), &mut |_| {});
        tl2.stream_started(ms_to_hns(1_100), &mut |_| {});
        let st2 = tl2.stats();
        assert_eq!((st2.lead_in_frames, st2.outage_filled_frames), (100 * 48, 1_000 * 48));
        assert_categories_sum_up(st2);
    }

    #[test]
    fn stream_started_does_nothing_when_the_timeline_is_already_ahead() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        tl.tick_outage(ms_to_hns(500), &mut |_| {});
        let before = tl.written_frames();
        let r = tl.stream_started(ms_to_hns(450), &mut |_| {});
        assert_eq!((r, tl.written_frames()), (Report::default(), before));
    }

    #[test]
    fn gap_between_packets_is_counted_as_a_gap() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        run_push(&mut tl, &packet(ms_to_hns(10) + ms_to_hns(200), 480, &data), &mut out); // 空档 200 ms
        let st = tl.stats();
        assert_eq!((st.fill_events, st.lead_in_frames, st.idle_filled_frames), (1, 0, 0));
        assert_eq!(st.gap_filled_frames, (200 - 10) * 48);
        assert_categories_sum_up(st);
    }

    #[test]
    fn consecutive_idle_ticks_are_one_episode_and_a_new_packet_starts_a_new_one() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        run_push(&mut tl, &packet(0, 480, &data), &mut out);
        // 第一段空闲：2 秒内每 10 ms 一次 tick，会补很多次，但只算一段
        for step in 2..=200i64 {
            tl.tick(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        assert_eq!(tl.stats().idle_episodes, 1);
        assert!(tl.stats().idle_filled_frames > 90_000);
        // 一个新包到达，随后再空闲：第二段
        let next = frames_to_hns(tl.written_frames() as i64, RATE);
        run_push(&mut tl, &packet(next, 480, &data), &mut out);
        for step in 1..=100i64 {
            tl.tick(next + dur(480) + ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        let st = tl.stats();
        assert_eq!(st.idle_episodes, 2, "{st:?}");
        assert_eq!((st.fill_events, st.gap_filled_frames, st.outage_filled_frames), (0, 0, 0));
        assert_categories_sum_up(st);
    }

    #[test]
    fn ticks_while_the_stream_is_down_are_outage_and_do_not_touch_idle() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let mut out = Vec::new();
        for step in 1..=300i64 {
            tl.tick_outage(ms_to_hns(10) * step, &mut |s| out.extend_from_slice(s));
        }
        let st = tl.stats();
        assert!(st.outage_filled_frames > 130_000, "{st:?}");
        assert_eq!((st.idle_filled_frames, st.idle_episodes, st.fill_events), (0, 0, 0));
        assert_categories_sum_up(st);
    }

    #[test]
    fn finish_tail_is_its_own_category_and_totals_still_add_up() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        tl.stream_started(ms_to_hns(300), &mut |s| out.extend_from_slice(s)); // 首包前 300 ms
        run_push(&mut tl, &packet(ms_to_hns(300), 480, &data), &mut out);
        tl.tick_outage(ms_to_hns(1_000), &mut |s| out.extend_from_slice(s)); // 中断
        tl.finish(ms_to_hns(1_500), &mut |s| out.extend_from_slice(s)); // 收尾
        let st = tl.stats();
        assert!(st.lead_in_frames > 0 && st.outage_filled_frames > 0 && st.tail_frames > 0, "{st:?}");
        assert_eq!((st.idle_filled_frames, st.idle_episodes), (0, 0), "收尾补齐不是空闲：{st:?}");
        assert_categories_sum_up(st);
        assert_eq!(st.frames_written, (out.len() / 2) as u64);
        assert_eq!(tl.written_frames(), 72_000); // 精确补到 1.5 s
    }

    #[test]
    fn waiting_for_the_first_packet_is_lead_in_whether_by_tick_or_by_late_timestamp() {
        // 回环一直没有声音：第一个包之前的 tick 补齐算首包前，不是空闲
        let mut tl = AudioTimeline::new(cfg(), 0);
        for step in 1..=300i64 {
            tl.tick(ms_to_hns(10) * step, &mut |_| {});
        }
        let st = tl.stats();
        assert!(st.lead_in_frames > 130_000, "{st:?}");
        assert_eq!((st.idle_filled_frames, st.idle_episodes, st.fill_events), (0, 0, 0));
        assert_categories_sum_up(st);
        // 第一个包的设备时间戳比采集开始晚 560 ms：同样是首包前，不是包间空档
        let mut late = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        run_push(&mut late, &packet(ms_to_hns(560), 480, &data), &mut Vec::new());
        let st = late.stats();
        assert_eq!((st.lead_in_frames, st.fill_events, st.gap_filled_frames), ((560 - 10) * 48, 0, 0));
    }

    #[test]
    fn a_constant_playback_latency_costs_one_lead_in_and_no_gaps() {
        // 回环：包在真实时间 t 读到，但设备时间戳（播放时刻）是 t + 560 ms
        let latency = ms_to_hns(560);
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.1f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..300i64 {
            run_push(&mut tl, &packet(dur(480) * k + latency, 480, &data), &mut out);
        }
        let st = tl.stats();
        assert_eq!((st.fill_events, st.pts_gaps, st.dropped_frames), (0, 0, 0), "{st:?}");
        assert_eq!(st.lead_in_frames, (560 - 10) * 48);
        // 在真实时间 3.0 s 停止：时间线已经写到 3.55 s，比停止时刻长一个播放延迟，收尾不应再补
        let r = tl.finish(ms_to_hns(3_000), &mut |s| out.extend_from_slice(s));
        assert_eq!(r, Report::default());
        assert!(tl.written_frames() > 3_000 * 48);
    }

    #[test]
    fn on_time_packets_with_a_growing_device_lead_are_latency_shift_not_gaps() {
        // 本机开保活流时的实测：包每 10 ms 准时到达，设备时间戳每包前进 20 ms（播放延迟每包 +10 ms），持续 56 个包
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.0f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..56i64 {
            let p = AudioPacket { device_qpc: Some(ms_to_hns(20) * (k + 1)), arrival_qpc: ms_to_hns(10) * (k + 1), frames: 480, samples: &data, flags: PacketFlags::default() };
            run_push(&mut tl, &p, &mut out);
        }
        let st = tl.stats();
        assert_eq!((st.fill_events, st.gap_filled_frames), (0, 0), "{st:?}");
        // 设备时间最终比到达时间领先 560 ms，时间线补上了这段（扣掉每次留下的 10 ms 残差和容差内的部分）
        assert!(st.latency_shift_frames > 18_000 && st.latency_shift_frames <= 560 * 48, "{st:?}");
        assert_categories_sum_up(st);
    }

    #[test]
    fn a_late_packet_after_a_stall_is_still_a_gap() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..10i64 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
        }
        // 设备停了 300 ms：下一个包到得晚，设备时间戳也跟着晚
        run_push(&mut tl, &packet(dur(480) * 10 + ms_to_hns(300), 480, &data), &mut out);
        let st = tl.stats();
        assert_eq!((st.fill_events, st.latency_shift_frames), (1, 0), "{st:?}");
        assert_eq!(st.gap_filled_frames, (300 - 10) * 48);
        assert_categories_sum_up(st);
    }

    #[test]
    fn stopping_a_healthy_stream_adds_no_idle_episode() {
        let mut tl = AudioTimeline::new(cfg(), 0);
        let data = vec![0.5f32; 480 * 2];
        let mut out = Vec::new();
        for k in 0..100 {
            run_push(&mut tl, &packet(dur(480) * k, 480, &data), &mut out);
        }
        // 最后一个包结束于 1.0 s，停止发生在 1.015 s
        tl.finish(ms_to_hns(1_015), &mut |s| out.extend_from_slice(s));
        let st = tl.stats();
        assert_eq!((st.tail_frames, st.idle_episodes, st.idle_filled_frames, st.fill_events), (15 * 48, 0, 0, 0));
        assert_categories_sum_up(st);
    }

    #[test]
    fn stats_from_an_older_session_json_without_the_new_fields_still_load() {
        let st: TimelineStats = serde_json::from_str(r#"{"packets": 5, "silence_filled_frames": 7}"#).unwrap();
        assert_eq!((st.packets, st.silence_filled_frames, st.lead_in_frames, st.idle_episodes), (5, 7, 0, 0));
    }
}
