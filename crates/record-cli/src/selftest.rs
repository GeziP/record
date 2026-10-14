//! 自检：在本机默认播放设备上播放测试音，用回环采回来，核对“零漏录、无空档、间隔准确”。
//!
//! 三个场景（`--only` 选其一）：
//! - `plain`：只录回环，不开保活流、不开麦克风。回环没有声音时不出包，考验空闲补齐；
//! - `keepalive`：只录回环，开保活流；
//! - `with-mic`：保活流 + 默认通信麦克风同时录——这才是真实的会议场景。
//!
//! 测试音：前两个场景用 16 kHz（几乎听不见，环境声音也很少在这个频段）。但有的耳麦一开麦克风就把播放带宽压到
//! 8 kHz 以下（本机实测：8 kHz −26 dB，16 kHz 消失，API 却报告端点格式没变），所以 `with-mic` 场景用 6 kHz（开麦克风时 5 kHz +0.1 dB、6 kHz −1.2 dB、7 kHz −3.8 dB；`--mic-freq` 调整）。

use crate::args::Args;
use crate::s1::{BeepSpec, CaptureOpts, MicSel, RATE, SystemSel, analyze_file, beep_spec, check_beep_bandwidth, default_out_dir, outcome_json, run_capture};
use crate::wav;
use anyhow::{Context, Result, bail};
use record_capture::devices::Role;
use record_core::timeline::{TimeSource, TimelineStats};
use std::fs;
use std::path::PathBuf;

/// 从开始采集到首次启动成功的耗时上限。设计目标是热键到开始写入不超过 1 秒；只对一开始就该出包的轨检查（保活流开着，或者是麦克风）。
const MAX_LEAD_IN_S: f64 = 1.0;
/// 异常补静音（包间空档、中断）在单条轨上累计超过这个时长，或次数超过 `MAX_ANOMALY_EVENTS`，就认为是系统性问题。
/// 在它们以内只警告：桌面机器上偶发的几十毫秒卡顿，时间线本来就是为它们准备的。
const MAX_ANOMALY_S: f64 = 2.0;
const MAX_ANOMALY_EVENTS: u64 = 5;

struct Scenario {
    name: &'static str,
    keepalive: bool,
    mic: bool,
    freq: f64,
}

fn secs_of(frames: u64) -> f64 {
    frames as f64 / f64::from(RATE)
}

fn describe_timeline(st: &TimelineStats) -> String {
    format!(
        "{} 个包；首包前 {:.2} 秒；延迟增大 {:.2} 秒；空闲 {:.2} 秒（{} 段）；包间空档 {} 次共 {:.2} 秒；中断 {:.2} 秒；收尾 {:.3} 秒；丢弃 {} 帧；large_gap {}；时间戳回退 {}；包间隙 {} / 重叠 {}；不连续标志 {}",
        st.packets,
        secs_of(st.lead_in_frames),
        secs_of(st.latency_shift_frames),
        secs_of(st.idle_filled_frames),
        st.idle_episodes,
        st.fill_events,
        secs_of(st.gap_filled_frames),
        secs_of(st.outage_filled_frames),
        secs_of(st.tail_frames),
        st.dropped_frames,
        st.large_gaps,
        st.timestamp_fallbacks,
        st.pts_gaps,
        st.pts_overlaps,
        st.discontinuities
    )
}

/// 一条轨上的异常补静音帧数：包间空档 + 中断；麦克风不该停止出包，所以它的空闲也算异常。
fn anomaly_frames(label: &str, st: &TimelineStats) -> u64 {
    st.gap_filled_frames + st.outage_filled_frames + if label == "mic" { st.idle_filled_frames } else { 0 }
}

pub fn selftest_cmd(a: &Args) -> Result<()> {
    let secs = a.num("secs", 20u64)?;
    let spec = beep_spec(a)?;
    let mic_freq: f64 = a.num("mic-freq", 6_000.0)?;
    let mic_time = match a.get("mic-time").unwrap_or("device") {
        "device" => TimeSource::DeviceQpc,
        "arrival" => TimeSource::ArrivalMinusDuration,
        o => bail!("--mic-time 只能是 device 或 arrival，收到“{o}”"),
    };
    let all = [
        Scenario { name: "plain", keepalive: false, mic: false, freq: spec.freq },
        Scenario { name: "keepalive", keepalive: true, mic: false, freq: spec.freq },
        Scenario { name: "with-mic", keepalive: true, mic: true, freq: mic_freq },
    ];
    let chosen: Vec<&Scenario> = match a.get("only") {
        None => all.iter().collect(),
        Some(n) => vec![all.iter().find(|s| s.name == n).with_context(|| format!("--only 只能是 plain、keepalive 或 with-mic，收到“{n}”"))?],
    };
    for s in &chosen {
        check_beep_bandwidth(s.freq)?;
    }
    let base = a.get("out").map(PathBuf::from).unwrap_or_else(default_out_dir);
    let mut failed = Vec::new();

    for sc in chosen {
        let name = sc.name;
        let spec = BeepSpec { freq: sc.freq, ..spec };
        println!("\n=== 回环自检：{name}（保活流{}，麦克风{}，测试音 {:.0} Hz，{secs} 秒）===", if sc.keepalive { "开" } else { "关" }, if sc.mic { "开" } else { "关" }, sc.freq);
        let o = CaptureOpts {
            secs,
            out_dir: base.join(name),
            system: SystemSel::Default(Role::Console),
            mic: if sc.mic { MicSel::Default(Role::Communications) } else { MicSel::None },
            keepalive: sc.keepalive,
            system_time: TimeSource::DeviceQpc,
            mic_time,
            play: Some(spec),
            verbose: a.flag("verbose"),
        };
        let out = run_capture(&o)?;
        let sys = out.tracks.iter().find(|t| t.label == "system").context("没有系统音轨")?;
        let r = analyze_file(&sys.file, &spec)?;
        println!("系统音轨 {:.2} 秒（期望约 {} 秒），播放了 {} 个脉冲，检测到 {} 个，测试音频率上的环境电平 {:.4}", r.seconds, secs + 2, out.beeps_completed, r.bursts.len(), r.baseline);
        if r.baseline > f64::from(spec.amp) * 0.25 {
            failed.push(format!("{name}：环境声音在测试音频率上的电平 {:.4} 过高，检测结果不可信，请暂停其他播放后重试", r.baseline));
        }
        if let Some(s) = r.spacing {
            println!("脉冲间隔 {:.4} 秒，最大偏差 {:.1} ms，漂移 {:+.0} ppm", s.mean_interval_s, s.max_dev_ms, s.drift_ppm);
        }

        for t in &out.tracks {
            let st = &t.result.stats;
            println!("时间线[{}]：{}", t.label, describe_timeline(st));
            if let Some(lag) = t.lag_ms {
                println!("  设备时间戳 − 到达时间 = {lag:+.1} ms（回环是播放延迟，麦克风是采集缓冲）");
            }
            let anomaly = anomaly_frames(&t.label, st);
            let events = st.fill_events + st.outage_filled_frames.min(1);
            if secs_of(anomaly) > MAX_ANOMALY_S || events > MAX_ANOMALY_EVENTS {
                failed.push(format!("{name}：{} 轨异常补静音共 {:.2} 秒（{} 次），是系统性问题", t.label, secs_of(anomaly), events));
            } else if anomaly > 0 {
                println!(
                    "警告：{} 轨有 {:.2} 秒异常补静音（包间空档 {} 次，中断 {:.2} 秒），时间线已补齐，但说明设备或系统出现过卡顿",
                    t.label,
                    secs_of(anomaly),
                    st.fill_events,
                    secs_of(st.outage_filled_frames)
                );
            }
            // 没开保活流时，回环要等到有声音才出第一个包：“首包前”量到的是测试音自己的开始时间加上播放延迟，不是启动耗时，不在这里卡
            let packets_expected_at_once = sc.keepalive || t.label == "mic";
            if packets_expected_at_once && secs_of(st.lead_in_frames) > MAX_LEAD_IN_S {
                failed.push(format!("{name}：{} 轨从开始采集到首次启动成功用了 {:.2} 秒，超过 {MAX_LEAD_IN_S} 秒", t.label, secs_of(st.lead_in_frames)));
            }
        }
        if let Some(m) = out.tracks.iter().find(|t| t.label == "mic") {
            let mr = wav::read(&m.file)?;
            let peak = mr.samples.iter().fold(0.0f32, |a, s| a.max(s.abs()));
            println!("麦克风轨 {:.2} 秒，峰值 {:.4}（只验证能打开并持续出数据）", secs_of(m.frames), peak);
            if m.frames < (secs + 1) * u64::from(RATE) {
                failed.push(format!("{name}：麦克风轨过短"));
            }
        }

        match (r.bursts.len() as u32).cmp(&out.beeps_completed) {
            std::cmp::Ordering::Less => failed.push(format!("{name}：播放 {} 个脉冲，只检测到 {} 个（漏录）", out.beeps_completed, r.bursts.len())),
            std::cmp::Ordering::Greater => {
                failed.push(format!("{name}：播放 {} 个脉冲，却检测到 {} 个：测试音频率上混入了环境声音，结果不可信，请暂停其他播放后重试", out.beeps_completed, r.bursts.len()))
            }
            std::cmp::Ordering::Equal => {}
        }
        match r.spacing {
            Some(s) if s.max_dev_ms > 25.0 => failed.push(format!("{name}：脉冲间隔最大偏差 {:.1} ms 超过 25 ms（时间线有空档或压缩）", s.max_dev_ms)),
            None => failed.push(format!("{name}：脉冲太少，无法评估间隔")),
            _ => {}
        }
        // 回环包的设备时间戳是播放时刻，比读到它的时刻晚一个播放延迟，所以轨比采集时长长出这么多是正常的
        let latency_s = sys.lag_ms.unwrap_or(0.0).max(0.0) / 1000.0;
        let expected = (secs + 2) as f64;
        if r.seconds < expected - 0.3 || r.seconds > expected + latency_s + 0.3 {
            failed.push(format!("{name}：系统音轨时长 {:.2} 秒，应在 {:.2}–{:.2} 秒之间（采集 {expected} 秒，播放延迟 {latency_s:.2} 秒）", r.seconds, expected - 0.3, expected + latency_s + 0.3));
        }
        if out.watchdog_alerts > 0 {
            failed.push(format!("{name}：静默失效监测误报 {} 次", out.watchdog_alerts));
        }
        fs::write(o.out_dir.join("report.json"), serde_json::to_string_pretty(&outcome_json(&out))?)?;
    }

    if failed.is_empty() {
        println!("\n自检通过。");
        Ok(())
    } else {
        for f in &failed {
            println!("未通过：{f}");
        }
        bail!("自检未通过（{} 项）", failed.len())
    }
}
