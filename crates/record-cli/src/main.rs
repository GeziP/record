//! 开发与验证用的命令行工具：spike 的采集、分析、自检；不属于最终安装包。

mod args;
mod clocks;
mod frames;
mod packets;
mod rec;
mod response;
mod s1;
mod s2;
mod s2bench;
mod s3;
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
        (Some("s2"), Some("monitors")) => s2::monitors_cmd(),
        (Some("s2"), Some("selftest")) => s2::selftest_cmd(&args),
        (Some("s2"), Some("probe")) => s2::probe_cmd(&args),
        (Some("s2"), Some("window")) => s2bench::window_cmd(&args),
        (Some("s2"), Some("bench")) => s2bench::bench_cmd(&args),
        (Some("s3"), Some("stimulus")) => s3::stimulus_cmd(&args),
        (Some("s3"), Some("clocks")) => clocks::clocks_cmd(&args),
        (Some("s3"), Some("analyze")) => s3::analyze_cmd(&args),
        (Some("record"), _) => rec::record_cmd(&args),
        (Some("recover"), _) => rec::recover_cmd(&args),
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
                                      回环链路的频率响应（高频是否被砍掉）
  s2 monitors                         列出显示器（分辨率、刷新率、缩放、HDR 与 SDR 内容亮度）
  s2 selftest [--secs 8] [--fps 15] [--size 960x540] [--static] [--tolerance 6]
              [--monitor primary|foreground|N] [--out WxH]
                                      在目标显示器中央放测试窗口，验证 GPU 转换的颜色、黑边与画面陈旧程度
                                      （--size 取显示器整屏大小再加 --static，可验证桌面完全静止时的首帧）
  s2 probe [--fps 15] [--size 960x540] [--monitor ...] [--out DIR]
                                      排查用：对照 GDI 读到的颜色与桌面复制采到的颜色，导出只含测试窗口的裁剪图
  s2 window [--secs 30] [--size 960x540] [--monitor ...]
                                      只放一个动画测试窗口（bench 把它当子进程）
  record [--secs 30] [--out DIR] [--no-video] [--no-mic] [--no-system] [--encoder auto|nvenc|qsv|amf|software]
         [--fps 15] [--monitor primary|foreground|N] [--config 文件] [--inject stall:mic:10@5,kill:video@20]
                                      用录制引擎录一场（默认写到 %TEMP%\record-sessions），结束后打印诊断；
                                      --inject 在指定秒数注入故障（暂停采集线程或杀掉编码器）
  recover [--out DIR | --session DIR]
                                      扫描保存目录并修复上次崩溃留下的会话
  s2 bench [--secs 30] [--fps 15] [--encoder auto|nvenc|qsv|amf|software] [--report 10] [--limit 5]
           [--size 960x540] [--monitor ...] [--no-window]
                                      真实编码器下的 CPU 与吞吐（输出写到 NUL）；需要 ffmpeg（RECORD_FFMPEG）
  s3 stimulus [--secs 60] [--interval 2.0083] [--lead 3] [--freq 1000] [--naive] [--device 设备ID] [--monitor ...]
                                      屏幕中央闪白并在测试音出声的那一刻响起（间隔与帧周期错开），用来测量画面与声音的同步；
                                      --naive 改成写入即闪（测出来的就是播放设备的延迟）
  s3 clocks [--beeps 6] [--interval 2] [--freq 1000] [--limit-ms 80] [--device 设备ID]
                                      不录屏：在端点上放几声测试音，同时用录制器的系统音路径录下来，比较“播放流位置推算的出声时刻”
                                      与“录下来的声音被放在哪里”；差得超过限值，说明这个端点上 S3 的参照测不准
  s3 analyze <meeting.mp4> [--roi 0.35,0.35,0.3,0.3] [--freq 1000] [--limit-ms 80] [--search 1.0] [--verbose]
                                      从成品里找闪屏与测试音，配对后估计画面比声音晚多少毫秒（需要 ffmpeg 与 ffprobe）";
