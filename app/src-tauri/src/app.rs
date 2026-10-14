//! 外壳的装配（设计文档 6）：插件、托盘、热键、设置窗口，以及它们与执行者的连接。
//!
//! 这里只有胶水：每个决定都在 `tray_model`、`notice`、`hotkeys`、`runner` 里，那些模块有单元测试；
//! 这一层靠 `scripts/shell-check.ps1` 在真机上验证。

use crate::backend::{EngineBackend, SharedConfig, recover_on_startup, set_aside_broken};
use crate::hotkeys::{self, HotkeyEntry, HotkeyStatus};
use crate::notice::{Notice, should_show};
use crate::runner::{self, Handle, Progress, Ui};
use crate::tray_model::{self, Look};
use crate::{logging, power};
use record_capture::devices::{Flow, list_active};
use record_core::config::Config;
use record_core::controller::{Command, State as RunState};
use record_core::paths::{Paths, resolve_output_dir};
use record_core::session::EndReason;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{PCWSTR, w};

const TRAY_ID: &str = "record";
const SETTINGS_WINDOW: &str = "settings";
/// 睡眠通知后最多等多久让本场存完：系统给的时间本来就不多，来不及的由下次启动的崩溃恢复补上。
const SLEEP_WAIT: Duration = Duration::from_secs(2);
const END_SESSION_WAIT: Duration = Duration::from_secs(10);

struct TrayItems {
    start: MenuItem<Wry>,
    stop: MenuItem<Wry>,
    quit: MenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

#[derive(Default)]
struct Registered {
    entries: Vec<HotkeyEntry>,
    /// 插件给每个热键的编号 → 命令，热键触发时反查。
    ids: Vec<(u32, Command)>,
}

struct View {
    state: RunState,
    progress: Progress,
    warnings: Vec<String>,
    /// 托盘上现在显示的图标样子与提示，没变就不去动托盘。
    shown: Option<(Look, String)>,
}

struct Shared {
    paths: Paths,
    videos_dir: PathBuf,
    config: SharedConfig,
    config_error: Mutex<Option<String>>,
    handle: Handle,
    hotkeys: Mutex<Registered>,
    view: Mutex<View>,
    items: TrayItems,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn current_config(shared: &Shared) -> Config {
    shared.config.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

fn fatal(text: &str) -> ! {
    tracing::error!("{text}");
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { MessageBoxW(None, PCWSTR(wide.as_ptr()), w!("Record 无法启动"), MB_OK | MB_ICONERROR) };
    std::process::exit(1)
}

// ───────────────────────────── 托盘与通知 ─────────────────────────────

fn tray_icon(look: Look) -> tauri::image::Image<'static> {
    tauri::image::Image::new_owned(tray_model::icon_rgba(tray_model::rgb(look)), tray_model::ICON_SIZE, tray_model::ICON_SIZE)
}

/// 按当前状态刷新托盘图标、提示和菜单项；和上次显示的一样就不动。
fn refresh_tray(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let (state, look, tooltip) = {
        let mut view = lock(&shared.view);
        let look = tray_model::look(view.state, view.warnings.len());
        let tooltip = tray_model::tooltip(view.state, view.progress.elapsed, view.progress.bytes, &view.warnings);
        if view.shown.as_ref() == Some(&(look, tooltip.clone())) {
            return;
        }
        if view.shown.as_ref().map(|(shown_look, _)| *shown_look) == Some(look) {
            tracing::debug!(%tooltip, "托盘提示更新");
        } else {
            tracing::info!(?look, state = ?view.state, %tooltip, "托盘图标更新");
        }
        view.shown = Some((look, tooltip.clone()));
        (view.state, look, tooltip)
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(tray_icon(look)));
        let _ = tray.set_tooltip(Some(tooltip));
    }
    let enabled = tray_model::menu_enabled(state);
    let _ = shared.items.start.set_enabled(enabled.start);
    let _ = shared.items.stop.set_enabled(enabled.stop);
}

/// 菜单项上写热键；没注册成功的标出来。
fn refresh_menu_labels(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let registered = lock(&shared.hotkeys);
    let label = |command| hotkeys::menu_label(command, registered.entries.iter().find(|e| e.command == command));
    let (start, stop, quit) = (label(Command::Start), label(Command::Stop), label(Command::Quit));
    tracing::debug!(%start, %stop, %quit, "托盘菜单文字");
    let _ = shared.items.start.set_text(start);
    let _ = shared.items.stop.set_text(stop);
    let _ = shared.items.quit.set_text(quit);
}

fn notify(app: &AppHandle, notice: Notice) {
    let shared = app.state::<Shared>();
    let mode = current_config(&shared).notify.mode;
    let show = should_show(mode, &notice);
    let (title, body) = notice.text();
    tracing::info!(show, %title, %body, "通知");
    if show && let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("弹出系统通知失败：{e}");
    }
}

fn on_ui(app: &AppHandle, event: Ui) {
    let shared = app.state::<Shared>();
    match event {
        Ui::State(state) => {
            let mut view = lock(&shared.view);
            view.state = state;
            if state == RunState::Recording {
                view.progress = Progress::default();
            }
            drop(view);
            refresh_tray(app);
        }
        Ui::Progress(progress) => {
            lock(&shared.view).progress = progress;
            refresh_tray(app);
        }
        Ui::Warnings(warnings) => {
            lock(&shared.view).warnings = warnings;
            refresh_tray(app);
        }
        Ui::Notice(notice) => notify(app, notice),
        Ui::Quit => {
            tracing::info!("退出");
            app.exit(0);
        }
    }
}

// ───────────────────────────── 热键、自启、文件夹 ─────────────────────────────

/// 按配置（重新）注册三个热键，返回每个的结果。常驻注册：不论当前是待命还是录制中，三个热键始终有效。
fn apply_hotkeys(app: &AppHandle, config: &Config) -> Vec<HotkeyEntry> {
    let shortcuts = app.global_shortcut();
    if let Err(e) = shortcuts.unregister_all() {
        tracing::warn!("清除旧热键失败：{e}");
    }
    let mut entries = Vec::new();
    let mut ids = Vec::new();
    match hotkeys::wanted(config) {
        Ok(wanted) => {
            for (command, text, plugin_text) in wanted {
                let status = match plugin_text.parse::<Shortcut>() {
                    Ok(shortcut) => match shortcuts.register(shortcut) {
                        Ok(()) => {
                            ids.push((shortcut.id(), command));
                            HotkeyStatus::Registered
                        }
                        Err(e) => HotkeyStatus::Failed(hotkeys::explain_failure(&e.to_string())),
                    },
                    Err(e) => HotkeyStatus::Failed(e.to_string()),
                };
                entries.push(HotkeyEntry { command, text, status });
            }
        }
        Err(why) => {
            let typed = [(Command::Start, &config.hotkeys.start), (Command::Stop, &config.hotkeys.stop), (Command::Quit, &config.hotkeys.quit)];
            entries.extend(typed.into_iter().map(|(command, text)| HotkeyEntry { command, text: text.clone(), status: HotkeyStatus::Failed(why.clone()) }));
        }
    }
    for entry in &entries {
        match &entry.status {
            HotkeyStatus::Registered => tracing::info!(command = ?entry.command, hotkey = %entry.text, "热键已注册"),
            HotkeyStatus::Failed(why) => tracing::warn!(command = ?entry.command, hotkey = %entry.text, "热键没注册成功：{why}"),
        }
    }
    *lock(&app.state::<Shared>().hotkeys) = Registered { entries: entries.clone(), ids };
    entries
}

/// 让系统的开机自启项与配置一致。
fn sync_autostart(app: &AppHandle, want: bool) {
    let launcher = app.autolaunch();
    match launcher.is_enabled() {
        Ok(now) if now == want => {}
        Ok(_) => {
            let result = if want { launcher.enable() } else { launcher.disable() };
            match result {
                Ok(()) => tracing::info!(want, "开机自启已同步"),
                Err(e) => tracing::warn!(want, "同步开机自启失败：{e}"),
            }
        }
        Err(e) => tracing::warn!("读取开机自启状态失败：{e}"),
    }
}

fn open_in_explorer(path: &Path) {
    if let Err(e) = std::fs::create_dir_all(path) {
        tracing::warn!("创建文件夹 {} 失败：{e}", path.display());
    }
    // explorer 打开文件夹时退出码常常是 1，不用理会
    if let Err(e) = std::process::Command::new("explorer.exe").arg(path).spawn() {
        tracing::warn!("打开文件夹 {} 失败：{e}", path.display());
    }
}

fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Err(e) = WebviewWindowBuilder::new(app, SETTINGS_WINDOW, WebviewUrl::App("index.html".into())).title("Record 设置").inner_size(760.0, 860.0).min_inner_size(560.0, 480.0).build() {
        tracing::error!("打开设置窗口失败：{e}");
    }
}

fn toggle_autostart(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let want = shared.items.autostart.is_checked().unwrap_or(false);
    let mut config = current_config(&shared);
    config.autostart = want;
    match persist(&shared, config) {
        Ok(()) => sync_autostart(app, want),
        Err(e) => {
            tracing::warn!("保存开机自启设置失败：{e}");
            let _ = shared.items.autostart.set_checked(!want);
        }
    }
}

/// 写入配置文件并让内存里的配置同步；读不懂的旧文件先留底。
fn persist(shared: &Shared, config: Config) -> Result<(), String> {
    if let Some(backup) = set_aside_broken(&shared.paths.config_file) {
        tracing::warn!("原配置文件读不懂，已改名保留为 {}", backup.display());
    }
    config.save(&shared.paths.config_file).map_err(|e| e.to_string())?;
    *shared.config.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
    *lock(&shared.config_error) = None;
    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    let shared = app.state::<Shared>();
    match id {
        "start" => shared.handle.command(Command::Start),
        "stop" => shared.handle.command(Command::Stop),
        "quit" => shared.handle.command(Command::Quit),
        "recordings" => open_in_explorer(&resolve_output_dir(&current_config(&shared), &shared.videos_dir)),
        "logs" => open_in_explorer(&shared.paths.log_dir),
        "settings" => show_settings(app),
        "autostart" => toggle_autostart(app),
        other => tracing::warn!("没有处理的菜单项：{other}"),
    }
}

// ───────────────────────────── 设置窗口用的命令 ─────────────────────────────

#[derive(Serialize)]
struct IssueDto {
    key: String,
    message: String,
}

#[derive(Serialize)]
struct HotkeyDto {
    command: &'static str,
    text: String,
    ok: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct SettingsDto {
    config: Config,
    /// 还没有配置文件：设置窗口按首次使用向导显示。
    first_run: bool,
    /// 配置文件读不懂时的原因；此时 `config` 是默认值。
    config_error: Option<String>,
    default_output_dir: String,
    hotkeys: Vec<HotkeyDto>,
    config_path: String,
    version: &'static str,
}

#[derive(Serialize)]
struct SaveResultDto {
    issues: Vec<IssueDto>,
    hotkeys: Vec<HotkeyDto>,
}

#[derive(Serialize)]
struct DeviceDto {
    id: String,
    name: String,
    /// `render`（播放，系统音从这里回环）或 `capture`（录音，麦克风）。
    flow: &'static str,
    is_default: bool,
}

fn hotkey_dtos(entries: &[HotkeyEntry]) -> Vec<HotkeyDto> {
    entries
        .iter()
        .map(|e| HotkeyDto {
            command: match e.command {
                Command::Start => "start",
                Command::Stop => "stop",
                Command::Quit => "quit",
            },
            text: e.text.clone(),
            ok: e.status == HotkeyStatus::Registered,
            error: if let HotkeyStatus::Failed(why) = &e.status { Some(why.clone()) } else { None },
        })
        .collect()
}

/// 配置本身的问题，加上保存目录能不能建出来（建得出来就顺手建好，开始录制时不必再等）。
fn save_issues(config: &Config, videos_dir: &Path) -> Vec<IssueDto> {
    let mut issues: Vec<IssueDto> = config.validate().into_iter().map(|i| IssueDto { key: i.key.to_string(), message: i.message }).collect();
    let dir = resolve_output_dir(config, videos_dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        issues.push(IssueDto { key: "output_dir".into(), message: format!("保存目录不能使用（{}）：{e}", dir.display()) });
    }
    issues
}

#[tauri::command]
fn load_settings(shared: State<'_, Shared>) -> SettingsDto {
    SettingsDto {
        config: current_config(&shared),
        first_run: !shared.paths.config_file.exists(),
        config_error: lock(&shared.config_error).clone(),
        default_output_dir: shared.videos_dir.join("Record").display().to_string(),
        hotkeys: hotkey_dtos(&lock(&shared.hotkeys).entries),
        config_path: shared.paths.config_file.display().to_string(),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<'_, Shared>, config: Config) -> Result<SaveResultDto, String> {
    let issues = save_issues(&config, &shared.videos_dir);
    if !issues.is_empty() {
        return Ok(SaveResultDto { issues, hotkeys: hotkey_dtos(&lock(&shared.hotkeys).entries) });
    }
    persist(&shared, config.clone())?;
    let entries = apply_hotkeys(&app, &config);
    let conflicts = hotkeys::conflict_lines(&entries);
    refresh_menu_labels(&app);
    let _ = shared.items.autostart.set_checked(config.autostart);
    sync_autostart(&app, config.autostart);
    tracing::info!("设置已保存");
    if !conflicts.is_empty() {
        notify(&app, Notice::HotkeyConflict(conflicts));
    }
    Ok(SaveResultDto { issues: Vec::new(), hotkeys: hotkey_dtos(&entries) })
}

#[tauri::command]
fn list_devices() -> Result<Vec<DeviceDto>, String> {
    list_active().map(|devices| devices.into_iter().map(|d| DeviceDto { id: d.id, name: d.name, flow: if d.flow == Flow::Render { "render" } else { "capture" }, is_default: d.is_default }).collect())
}

#[tauri::command]
fn open_folder(shared: State<'_, Shared>, which: String) -> Result<(), String> {
    match which.as_str() {
        "recordings" => open_in_explorer(&resolve_output_dir(&current_config(&shared), &shared.videos_dir)),
        "logs" => open_in_explorer(&shared.paths.log_dir),
        other => return Err(format!("不认识的文件夹：{other}")),
    }
    Ok(())
}

/// 设置窗口里录入热键期间暂停全局热键：否则按下已经注册的组合，会真的开始或结束一场录制。
#[tauri::command]
fn pause_hotkeys(app: AppHandle) {
    match app.global_shortcut().unregister_all() {
        Ok(()) => tracing::info!("热键已暂停：设置窗口正在录入热键"),
        Err(e) => tracing::warn!("暂停热键失败：{e}"),
    }
}

#[tauri::command]
fn resume_hotkeys(app: AppHandle) {
    resume(&app);
}

/// 按已保存的配置重新注册热键。设置窗口关闭时也会走一遍，免得窗口在录入中途被关掉后热键一直失效。
fn resume(app: &AppHandle) {
    let shared = app.state::<Shared>();
    apply_hotkeys(app, &current_config(&shared));
    refresh_menu_labels(app);
}

#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.close();
    }
}

// ───────────────────────────── 启动 ─────────────────────────────

pub fn run() {
    let paths = Paths::from_env().unwrap_or_else(|e| fatal(&e.to_string()));
    let log_guard = logging::init(&paths.log_dir);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %paths.config_file.display(), "启动");
    // COM 的多线程单元要在整个进程生命周期里都存在：每场录制结束、采集线程全部退出时，它不能跟着被拆掉，
    // 否则下一条系统通知会撞上已经被卸载的组件而崩溃（原因见 record_capture::com）。
    record_capture::com::keep_mta_alive();

    let (config, config_error) = match Config::load(&paths.config_file) {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(e.to_string())),
    };
    let first_run = config_error.is_none() && !paths.config_file.exists();
    let shared_config: SharedConfig = Arc::new(RwLock::new(config.clone()));

    let app = tauri::Builder::default()
        // 单实例插件必须第一个注册：再次启动只会唤起已运行实例的设置窗口
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            tracing::info!(?argv, "又启动了一个实例，已拦下，打开设置窗口");
            show_settings(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let shared = app.state::<Shared>();
                    let command = lock(&shared.hotkeys).ids.iter().find(|(id, _)| *id == shortcut.id()).map(|(_, command)| *command);
                    if let Some(command) = command {
                        tracing::info!(?command, %shortcut, "热键");
                        shared.handle.command(command);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![load_settings, save_settings, list_devices, open_folder, pause_hotkeys, resume_hotkeys, close_settings])
        .on_window_event(|window, event| {
            if window.label() == SETTINGS_WINDOW && matches!(event, tauri::WindowEvent::Destroyed) {
                resume(window.app_handle());
            }
        })
        .setup(move |app| {
            let videos_dir = app.path().video_dir().unwrap_or_else(|_| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Videos"));
            let ui_app = app.handle().clone();
            let handle = runner::spawn(EngineBackend::new(shared_config.clone(), videos_dir.clone()), move |event| on_ui(&ui_app, event));

            let start = MenuItem::with_id(app, "start", hotkeys::command_name(Command::Start), true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", hotkeys::command_name(Command::Stop), false, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", hotkeys::command_name(Command::Quit), true, None::<&str>)?;
            let autostart = CheckMenuItem::with_id(app, "autostart", "开机自启", true, config.autostart, None::<&str>)?;
            let recordings = MenuItem::with_id(app, "recordings", "打开录音文件夹", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
            let logs = MenuItem::with_id(app, "logs", "打开日志文件夹", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&start, &stop, &PredefinedMenuItem::separator(app)?, &recordings, &settings, &logs, &autostart, &PredefinedMenuItem::separator(app)?, &quit])?;
            let tray = TrayIconBuilder::with_id(TRAY_ID)
                .icon(tray_icon(Look::Standby))
                .tooltip(tray_model::tooltip(RunState::Idle, Duration::ZERO, 0, &[]))
                .menu(&menu)
                .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
                .build(app)?;
            app.manage(tray);
            tracing::info!("托盘图标已创建");

            app.manage(Shared {
                paths: paths.clone(),
                videos_dir: videos_dir.clone(),
                config: shared_config.clone(),
                config_error: Mutex::new(config_error.clone()),
                handle: handle.clone(),
                hotkeys: Mutex::new(Registered::default()),
                view: Mutex::new(View {
                    state: RunState::Idle,
                    progress: Progress::default(),
                    warnings: Vec::new(),
                    shown: Some((Look::Standby, tray_model::tooltip(RunState::Idle, Duration::ZERO, 0, &[]))),
                }),
                items: TrayItems { start, stop, quit, autostart },
            });

            let app_handle = app.handle().clone();
            let entries = apply_hotkeys(&app_handle, &config);
            refresh_menu_labels(&app_handle);
            let conflicts = hotkeys::conflict_lines(&entries);
            if !conflicts.is_empty() {
                notify(&app_handle, Notice::HotkeyConflict(conflicts));
            }
            if let Some(why) = &config_error {
                notify(&app_handle, Notice::ConfigInvalid(why.clone()));
            }
            // 首次使用：把自启项交给向导决定，之后才与配置同步；已经有配置的，每次启动都对一遍（重装、换路径后自启项会过期）
            if !first_run {
                sync_autostart(&app_handle, config.autostart);
            }

            let power_handle = handle.clone();
            power::spawn(move |event| match event {
                power::Event::Suspend => {
                    tracing::info!("系统即将睡眠，保存本场");
                    let _ = power_handle.auto(EndReason::Sleep).recv_timeout(SLEEP_WAIT);
                }
                power::Event::EndSession(reason) => {
                    tracing::info!(?reason, "系统结束会话，保存本场并退出");
                    let _ = power_handle.end_session(reason).recv_timeout(END_SESSION_WAIT);
                }
            })?;

            // 崩溃恢复放在后台：不拖慢托盘和热键就绪
            let recovery_app = app_handle.clone();
            let recovery_dir = resolve_output_dir(&config, &videos_dir);
            std::thread::spawn(move || match record_mux::locate::find_ffmpeg() {
                Ok(ffmpeg) => {
                    let (repaired, failed) = recover_on_startup(&ffmpeg, &recovery_dir);
                    if repaired + failed > 0 {
                        notify(&recovery_app, Notice::Recovered { count: repaired, failed });
                    }
                }
                Err(e) => tracing::warn!("找不到 ffmpeg，跳过崩溃恢复检查：{e}"),
            });

            if first_run {
                show_settings(&app_handle);
            }
            tracing::info!("就绪");
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| fatal(&format!("创建应用失败：{e}")));

    let mut log_guard = log_guard;
    app.run(move |_app, event| match event {
        // 没有窗口时进程也要常驻：只有明确的 exit(code) 才放行
        RunEvent::ExitRequested { code, api, .. } if code.is_none() => api.prevent_exit(),
        RunEvent::Exit => drop(log_guard.take()),
        _ => {}
    });
}
