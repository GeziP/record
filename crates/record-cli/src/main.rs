//! 开发与验证用的命令行工具：spike 的采集、分析、自检；不属于最终安装包。

mod args;
mod frames;
mod packets;
mod response;
mod s1;
mod s2;
mod selftest;
mod wav;

use anyhow::{Result, bail};
use args::Args;

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let args = Args::parse(std::env::args().skip(1));
    let cmd = (args.pos.first().map(String::as_str), args.pos.get(1).map(String::as_str));
    match cmd {
        (Some("s1"), Some("devices")) => s1::devices_cmd(),
        (Some("s1"), Some("beeps")) => s1::beeps_cmd(&args),
        (Some("s1"), Some("capture")) => s1::capture_cmd(&args),
        (Some("s1"), Some("analyze")) => s1::analyze_cmd(&args),
        (Some("s1"), Some("levels")) => s1::levels_cmd(&args),
        (Some("s1"), Some("packets")) => s1::packets_cmd(&args),
        (Some("s1"), Some("selftest")) => selftest::selftest_cmd(&args),
        (Some("s1"), Some("response")) => response::response_cmd(&args),
        _ => bail!("{USAGE}"),
    }
}

const USAGE: &str = "用法：record-cli <命令>
  s1 devices                          列出默认设备与全部活动设备
  s1 beeps [--secs 20] [--interval 1] [--freq 16000]
                                      在默认播放设备上播放测试音（默认 16 kHz，几乎听不见）
  s1 capture [--secs 20] [--system console|communications|all|none|<设备ID>] [--mic communications|console|none|<设备ID>]
             [--keepalive] [--system-time device|arrival] [--mic-time device|arrival] [--play] [--out DIR] [--verbose]
  s1 analyze <wav> [--interval 1] [--freq 16000]
                                      检测测试音脉冲并评估间隔
  s1 levels <wav> [--freq 16000] [--from 0] [--to 8]
                                      逐 100 ms 打印指定频率上的电平
  s1 packets <packets_*.csv>          比较两种时间源的抖动与漂移
  s1 selftest [--secs 20] [--only plain|keepalive|with-mic] [--freq 16000] [--mic-freq 6000] [--mic-time device|arrival] [--out DIR]
                                      默认播放设备上的回环自检
  s1 response [--play-rate 48000] [--cap-rate 48000] [--freqs 1000,4000,8000,12000,16000]
             [--amp 0.1] [--burst] [--with-mic]
                                      回环链路的频率响应（高频是否被砍掉）";
