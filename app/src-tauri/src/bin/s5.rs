//! S5 外壳 spike：无窗口常驻、托盘图标、全局热键、单实例、开机自启与通知。
//!
//! 用法：`record-s5 [--log 文件] [--hotkeys "Ctrl+Alt+R,Ctrl+Alt+S,Ctrl+Alt+Q"] [--secs N] [--notify] [--autostart-roundtrip]`
//!
//! 每一件事的结果都带着时间写进日志（同时打印），由脚本去检查；程序本身不录任何东西。
//! 热键 `Ctrl+Alt+F9` 额外用来开关一个窗口，用来验证“关掉窗口后进程仍然常驻”（不用 `Ctrl+Alt+W`：它常被即时通讯软件占作“激活窗口”）。

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, ShortcutState};
use tauri_plugin_notification::NotificationExt;

struct Log {
    started: Instant,
    file: Option<Mutex<std::fs::File>>,
}

impl Log {
    fn line(&self, text: impl AsRef<str>) {
        let line = format!("[{:8.3}] {}", self.started.elapsed().as_secs_f64(), text.as_ref());
        println!("{line}");
        if let Some(Ok(mut f)) = self.file.as_ref().map(|f| f.lock()) {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

/// 32×32 的纯色圆点，托盘图标用（以后按状态换颜色）。
fn dot(rgb: [u8; 3]) -> tauri::image::Image<'static> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 + 0.5 - 16.0, y as f32 + 0.5 - 16.0);
            if dx * dx + dy * dy <= 12.0 * 12.0 {
                px[(y * N + x) * 4..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    tauri::image::Image::new_owned(px, N as u32, N as u32)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let hotkeys = opt("--hotkeys").unwrap_or_else(|| "Ctrl+Alt+R,Ctrl+Alt+S,Ctrl+Alt+Q".into());
    let secs: Option<u64> = opt("--secs").and_then(|s| s.parse().ok());
    let (notify, roundtrip) = (flag("--notify"), flag("--autostart-roundtrip"));
    let file = opt("--log").map(|p| Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(p).expect("打开日志文件")));
    let log = Arc::new(Log { started: Instant::now(), file });
    log.line(format!("启动，参数 {args:?}"));

    let app = tauri::Builder::default()
        // 单实例插件必须第一个注册：第二个实例会把参数交给第一个，然后自己退出
        .plugin(tauri_plugin_single_instance::init({
            let log = Arc::clone(&log);
            move |_app, argv, cwd| log.line(format!("第二个实例被拦下，参数 {argv:?}，目录 {cwd}"))
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler({
                    let log = Arc::clone(&log);
                    move |app, shortcut, event| {
                        log.line(format!("热键 {shortcut} {:?}", event.state()));
                        if event.state() == ShortcutState::Pressed && shortcut.matches(Modifiers::CONTROL | Modifiers::ALT, Code::F9) {
                            match app.get_webview_window("probe") {
                                Some(w) => log.line(format!("关闭窗口：{:?}", w.close())),
                                None => log.line(format!(
                                    "打开窗口：{:?}",
                                    WebviewWindowBuilder::new(app, "probe", WebviewUrl::App("index.html".into())).title("Record S5 窗口").inner_size(480.0, 320.0).build().map(|_| ())
                                )),
                            }
                        }
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_notification::init())
        .setup({
            let log = Arc::clone(&log);
            move |app| {
                let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&quit])?;
                let tray = TrayIconBuilder::with_id("s5").icon(dot([220, 20, 60])).tooltip("Record S5 探测").menu(&menu).on_menu_event(|app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                });
                app.manage(tray.build(app)?);
                log.line("托盘图标已创建");

                for text in hotkeys.split(',').map(str::trim).chain(["Ctrl+Alt+F9"]) {
                    match app.global_shortcut().register(text) {
                        Ok(()) => log.line(format!("热键 {text} 注册成功")),
                        Err(e) => log.line(format!("热键 {text} 注册失败：{e}")),
                    }
                }
                if notify {
                    log.line(format!("通知：{:?}", app.notification().builder().title("Record S5").body("通知测试").show()));
                }
                if roundtrip {
                    let m = app.autolaunch();
                    log.line(format!("开机自启 起始状态：{:?}", m.is_enabled()));
                    log.line(format!("开机自启 enable：{:?}，之后：{:?}", m.enable(), m.is_enabled()));
                    log.line(format!("开机自启 disable：{:?}，之后：{:?}", m.disable(), m.is_enabled()));
                }
                if let Some(secs) = secs {
                    let (handle, log) = (app.handle().clone(), Arc::clone(&log));
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(secs));
                        log.line("到时间，退出");
                        handle.exit(0);
                    });
                }
                log.line("就绪");
                Ok(())
            }
        })
        .build(tauri::generate_context!())
        .expect("创建 Tauri 应用");

    app.run({
        let log = Arc::clone(&log);
        move |_app, event| match event {
            // 没有窗口时进程也要常驻：只有明确的 exit(code) 才放行
            RunEvent::ExitRequested { code, api, .. } if code.is_none() => {
                log.line("收到退出请求（最后一个窗口关闭），已拦下，继续常驻");
                api.prevent_exit();
            }
            RunEvent::Exit => log.line("退出"),
            _ => {}
        }
    });
}
