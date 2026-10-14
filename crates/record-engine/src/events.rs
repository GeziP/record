//! 引擎对外的事件、告警与启动错误。外壳把它们翻译成托盘状态、气泡通知和日志。

use record_capture::audio::AudioEvent;
use record_capture::video::VideoEvent;
use record_core::session::{EndReason, TrackKind};
use thiserror::Error;

/// 需要让用户知道的异常（设计文档 5.10、9）。
#[derive(Debug, Clone, PartialEq)]
pub enum Alert {
    /// 回环“通但全是静音”：播放设备有声，系统音轨连续静音。
    CaptureSilent {
        device: String,
    },
    CaptureSilentRecovered,
    /// 采集设备或线程挂起：连续若干秒没有任何包或帧。
    Hung {
        track: TrackKind,
        secs: u32,
    },
    HungRecovered {
        track: TrackKind,
    },
    /// 输出帧率持续低于设定值。
    LowFps {
        ratio: f32,
    },
    LowFpsRecovered,
    /// 单次补静音过长（采集线程被饿死或设备停摆）。
    LargeGap {
        track: TrackKind,
        ms: u64,
    },
    /// 设备丢失超过 10 秒仍未恢复，或画面管线持续不可用。
    TrackInterrupted {
        track: TrackKind,
    },
    TrackRecovered {
        track: TrackKind,
    },
    /// 编码器（ffmpeg）异常退出，已经自动重启一次并接着录；出错到恢复之间的一小段缺失（画面冻结、声音是静音）。
    EncoderRestarted {
        track: TrackKind,
        why: String,
    },
    /// 编码器（ffmpeg）异常退出，没能恢复：这一轨到此为止，其他轨继续。
    EncoderFailed {
        track: TrackKind,
        why: String,
    },
    /// 没有麦克风：画面和系统音照常录。
    NoMic,
    /// 画面输出是 HDR，已按系统的“SDR 内容亮度”转换成 SDR（HDR 的高亮细节不会保留）。
    HdrActive,
}

pub fn track_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "画面",
        TrackKind::Mic => "麦克风",
        TrackKind::System => "系统音",
    }
}

impl Alert {
    /// 让托盘变琥珀的告警。恢复通知与信息类提示不算。
    pub fn is_warning(&self) -> bool {
        !matches!(self, Alert::CaptureSilentRecovered | Alert::HungRecovered { .. } | Alert::LowFpsRecovered | Alert::TrackRecovered { .. } | Alert::HdrActive)
    }

    pub fn describe(&self) -> String {
        match self {
            Alert::CaptureSilent { device } => format!("系统音轨一直是静音，但播放设备有声音：{device}。对方的声音可能没有录进去，请在设置里切换系统音采集方案"),
            Alert::CaptureSilentRecovered => "系统音轨又采到声音了".into(),
            Alert::Hung { track, secs } => format!("{}已经 {secs} 秒没有数据，采集可能挂起", track_name(*track)),
            Alert::HungRecovered { track } => format!("{}恢复出数据", track_name(*track)),
            Alert::LowFps { ratio } => format!("画面帧率只有设定值的 {:.0}%，机器可能太忙", ratio * 100.0),
            Alert::LowFpsRecovered => "画面帧率恢复正常".into(),
            Alert::LargeGap { track, ms } => format!("{}一次补了 {:.1} 秒静音，采集线程可能被卡住或设备停摆", track_name(*track), *ms as f64 / 1000.0),
            Alert::TrackInterrupted { track } => format!("{}中断了，其他轨继续录制", track_name(*track)),
            Alert::TrackRecovered { track } => format!("{}已恢复", track_name(*track)),
            Alert::EncoderRestarted { track, why } => format!("{}的编码器异常退出（{why}），已自动重启并接着录，中间有一小段缺失", track_name(*track)),
            Alert::EncoderFailed { track, why } => format!("{}的编码器异常退出，没能恢复：{why}", track_name(*track)),
            Alert::NoMic => "没有找到麦克风，只录画面和系统音".into(),
            Alert::HdrActive => "显示器开着 HDR，画面按系统的“SDR 内容亮度”转成 SDR 录制，HDR 的高亮细节不会保留".into(),
        }
    }
}

/// 引擎发给外壳的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// 只写日志的信息。
    Info(String),
    Alert(Alert),
    /// 引擎要求外壳结束本场：自动保护触发、写入失败。外壳应当调用 `Recorder::stop`。
    RequestStop(EndReason),
}

/// 启动失败：不留半成品会话（设计文档 6.1）。
#[derive(Debug, Error)]
pub enum StartError {
    #[error("编码器还没有确定（不能是 auto）：请先探测可用的编码器")]
    EncoderNotResolved,
    #[error("磁盘剩余空间 {free_gb:.1} GB，低于下限 {min_gb:.1} GB，没有开始录制")]
    DiskLow { free_gb: f64, min_gb: f64 },
    #[error("没有选择任何要录制的轨道")]
    NothingSelected,
    #[error("系统音采集方案“所有播放设备”还没有实现（S1 判定需要时才做），请改用默认设备或默认通信设备")]
    AllEndpointsNotImplemented,
    #[error("找不到要录制的显示器：{0}")]
    Monitor(String),
    #[error("创建会话目录失败：{0}")]
    SessionDir(std::io::Error),
    #[error(transparent)]
    Mux(#[from] record_mux::MuxError),
    #[error("启动{what}采集失败：{why}")]
    Capture { what: &'static str, why: String },
}

pub fn describe_audio_event(e: &AudioEvent) -> String {
    match e {
        AudioEvent::Opened { label, device, mix } => match mix {
            Some(m) => format!("[{label}] 已打开：{}（设备混音格式 {} Hz，{} 声道）", device.name, m.rate, m.channels),
            None => format!("[{label}] 已打开：{}", device.name),
        },
        AudioEvent::OpenFailed { label, error } => format!("[{label}] 打开失败：{error}"),
        AudioEvent::DeviceChanged { label, from, to } => format!("[{label}] 默认设备变化：{from} → {to}"),
        AudioEvent::StreamLost { label, error } => format!("[{label}] 流中断：{error}"),
        AudioEvent::Interrupted { label } => format!("[{label}] 超过 10 秒未恢复，标记中断"),
        AudioEvent::Recovered { label } => format!("[{label}] 已恢复"),
        AudioEvent::LargeGap { label, filled_ms } => format!("[{label}] 单次补静音 {filled_ms} ms（large_gap）"),
    }
}

pub fn describe_video_event(e: &VideoEvent) -> String {
    match e {
        VideoEvent::Opened { monitor, src_w, src_h, out_w, out_h, hdr, took_ms, rebuild } => {
            format!("[画面 {monitor}] {}：{src_w}×{src_h} → {out_w}×{out_h}{}，打开用了 {took_ms} ms", if *rebuild { "重建成功" } else { "已打开" }, if *hdr { "（HDR，已转 SDR）" } else { "" },)
        }
        VideoEvent::FirstFrame { after_ms } => format!("[画面] 第一帧在开始后 {after_ms} ms"),
        VideoEvent::Lost { reason } => format!("[画面] 复制对象失效，开始重建：{reason}"),
        VideoEvent::SecureDesktop => "[画面] 进入安全桌面（UAC 或锁屏），系统不允许抓取，重复上一帧".into(),
        VideoEvent::Failed { error } => format!("[画面] 打开或重建失败：{error}"),
        VideoEvent::Recovered { down_ms } => format!("[画面] 已恢复，中断了 {down_ms} ms"),
        VideoEvent::SdrWhiteChanged { nits } => format!("[画面] 系统的 SDR 内容亮度改为 {nits:.0} 尼特，转换增益已更新"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoveries_and_notices_do_not_turn_the_tray_amber() {
        for a in [Alert::CaptureSilentRecovered, Alert::HungRecovered { track: TrackKind::Mic }, Alert::LowFpsRecovered, Alert::TrackRecovered { track: TrackKind::Video }, Alert::HdrActive] {
            assert!(!a.is_warning(), "{a:?}");
        }
    }

    #[test]
    fn real_problems_are_warnings() {
        for a in [
            Alert::CaptureSilent { device: "x".into() },
            Alert::Hung { track: TrackKind::Video, secs: 10 },
            Alert::LowFps { ratio: 0.5 },
            Alert::LargeGap { track: TrackKind::System, ms: 6000 },
            Alert::TrackInterrupted { track: TrackKind::Mic },
            Alert::EncoderFailed { track: TrackKind::Video, why: "x".into() },
            Alert::EncoderRestarted { track: TrackKind::Video, why: "x".into() },
            Alert::NoMic,
        ] {
            assert!(a.is_warning(), "{a:?}");
        }
    }

    #[test]
    fn the_silent_capture_alert_names_the_device_and_the_remedy() {
        let text = Alert::CaptureSilent { device: "扬声器 (Realtek)".into() }.describe();
        assert!(text.contains("扬声器 (Realtek)") && text.contains("设置"), "{text}");
    }

    #[test]
    fn every_alert_has_a_description() {
        for a in
            [Alert::NoMic, Alert::HdrActive, Alert::LowFps { ratio: 0.456 }, Alert::LargeGap { track: TrackKind::Mic, ms: 5500 }, Alert::EncoderRestarted { track: TrackKind::System, why: "x".into() }]
        {
            assert!(!a.describe().is_empty());
        }
        assert!(Alert::LowFps { ratio: 0.456 }.describe().contains("46%"));
        assert!(Alert::LargeGap { track: TrackKind::Mic, ms: 5500 }.describe().contains("5.5"));
    }
}
