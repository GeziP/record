//! 录制环境快照：写进 `session.json` 的 `environment`。
//!
//! 事后排查“为什么对方的声音没录进去”时最需要的是当时系统里有哪些设备、默认设备是谁、用的哪种采集方案。

use record_capture::devices::{self, Flow, Role};
use record_core::config::{Config, Encoder};
use serde_json::{Value, json};
use std::path::Path;
use windows::Win32::Media::Audio::IMMDeviceEnumerator;

fn default_name(en: &IMMDeviceEnumerator, flow: Flow, role: Role) -> Value {
    match devices::default_endpoint(en, flow, role).and_then(|d| devices::info(&d)) {
        Ok(i) => json!(i.name),
        Err(e) => json!(format!("（没有：{e}）")),
    }
}

fn endpoints(en: &IMMDeviceEnumerator, flow: Flow) -> Value {
    let Ok(list) = devices::active_endpoints(en, flow) else {
        return json!([]);
    };
    let items: Vec<Value> = list
        .iter()
        .filter_map(|d| {
            let name = devices::info(d).ok()?.name;
            let mix = devices::mix_format(d).map(|m| format!("{} Hz，{} 声道，{} 位{}", m.rate, m.channels, m.bits, if m.float { "浮点" } else { "" })).ok();
            Some(json!({ "name": name, "mix": mix }))
        })
        .collect();
    json!(items)
}

pub(crate) fn environment(en: &IMMDeviceEnumerator, cfg: &Config, encoder: Encoder, ffmpeg: &Path, monitors: &[String]) -> Value {
    json!({
        "os": "Windows",
        "ffmpeg": ffmpeg.display().to_string(),
        "encoder": format!("{encoder:?}"),
        "monitors": monitors,
        "render_default_console": default_name(en, Flow::Render, Role::Console),
        "render_default_communications": default_name(en, Flow::Render, Role::Communications),
        "capture_default_console": default_name(en, Flow::Capture, Role::Console),
        "capture_default_communications": default_name(en, Flow::Capture, Role::Communications),
        "render_devices": endpoints(en, Flow::Render),
        "capture_devices": endpoints(en, Flow::Capture),
        "system_capture": format!("{:?}", cfg.audio.system_capture),
        "mic_role": format!("{:?}", cfg.audio.mic_role),
        "mic_time_source": format!("{:?}", cfg.audio.mic_time_source),
        "system_time_source": format!("{:?}", cfg.audio.system_time_source),
        "system_keepalive": cfg.audio.system_keepalive,
    })
}
