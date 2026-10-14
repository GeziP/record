//! 睡眠、关机、注销时保存本场（设计文档 6.5）。
//!
//! 一个隐藏的顶层窗口接收 `WM_POWERBROADCAST` 与 `WM_ENDSESSION`：仅用消息窗口（`HWND_MESSAGE`）收不到
//! `WM_QUERYENDSESSION`/`WM_ENDSESSION`（S5 实测），所以建的是普通顶层窗口，只是从不显示。
//! 窗口过程在“保存完成”之前不返回，系统因此会等本场存完再睡眠或关机。

use record_core::session::EndReason;
use std::sync::OnceLock;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::RegisterSuspendResumeNotification;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DispatchMessageW, ENDSESSION_LOGOFF, GetMessageW, MSG, PBT_APMSUSPEND, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
    WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

pub const CLASS_NAME: PCWSTR = w!("RecordSessionEvents");

/// 需要保存本场的系统事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Suspend,
    EndSession(EndReason),
}

/// 把窗口消息翻译成事件；与保存无关的消息返回 `None`。
/// `WM_ENDSESSION` 的 `wparam` 为 0 表示系统结束会话的决定被取消，那就什么都不做。
pub fn classify(msg: u32, wparam: usize, lparam: isize) -> Option<Event> {
    match msg {
        WM_POWERBROADCAST if wparam as u32 == PBT_APMSUSPEND => Some(Event::Suspend),
        WM_ENDSESSION if wparam != 0 => Some(Event::EndSession(if (lparam as u32) & ENDSESSION_LOGOFF != 0 { EndReason::Logoff } else { EndReason::Shutdown })),
        _ => None,
    }
}

type Callback = Box<dyn Fn(Event) + Send + Sync>;
static CALLBACK: OnceLock<Callback> = OnceLock::new();

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(event) = classify(msg, wparam.0, lparam.0)
        && let Some(callback) = CALLBACK.get()
    {
        callback(event);
    }
    match msg {
        // 同意关机：真正的保存在 WM_ENDSESSION 里做，这里不做耗时的事
        WM_QUERYENDSESSION | WM_POWERBROADCAST => LRESULT(1),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_window() -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance.into(), lpszClassName: CLASS_NAME, ..Default::default() };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS_NAME, w!("Record"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(instance.into()), None)?;
        // 新式待机（Modern Standby）的机器上，睡眠通知要登记才有；旧式广播也会来，重复的睡眠事件由执行者幂等地忽略
        if let Err(e) = RegisterSuspendResumeNotification(HANDLE(hwnd.0), DEVICE_NOTIFY_WINDOW_HANDLE) {
            tracing::warn!("登记睡眠通知失败，只能依赖系统广播：{e}");
        }
        Ok(hwnd)
    }
}

/// 在新线程里建窗口并跑消息循环；窗口建好后才返回。整个进程只能调用一次。
pub fn spawn(on_event: impl Fn(Event) + Send + Sync + 'static) -> std::io::Result<()> {
    if CALLBACK.set(Box::new(on_event)).is_err() {
        return Err(std::io::Error::other("系统事件窗口已经建过了"));
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("session-events".into()).spawn(move || {
        let created = create_window();
        let _ = ready_tx.send(created.map(|_| ()).map_err(|e| e.to_string()));
        let mut msg = MSG::default();
        // 窗口永不关闭：循环随进程结束
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    })?;
    ready_rx.recv().map_err(|_| std::io::Error::other("系统事件线程提前结束"))?.map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageW};

    #[test]
    fn only_suspend_and_a_confirmed_end_of_session_are_events() {
        assert_eq!(classify(WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0), Some(Event::Suspend));
        assert_eq!(classify(WM_POWERBROADCAST, 18, 0), None, "唤醒不是要保存的事件");
        assert_eq!(classify(WM_ENDSESSION, 1, 0), Some(Event::EndSession(EndReason::Shutdown)));
        assert_eq!(classify(WM_ENDSESSION, 1, ENDSESSION_LOGOFF as isize), Some(Event::EndSession(EndReason::Logoff)));
        assert_eq!(classify(WM_ENDSESSION, 1, (ENDSESSION_LOGOFF | 1) as isize), Some(Event::EndSession(EndReason::Logoff)), "带上 CLOSEAPP 等其他标志也一样");
        assert_eq!(classify(WM_ENDSESSION, 0, 0), None, "关机被取消");
        assert_eq!(classify(WM_QUERYENDSESSION, 1, 0), None);
    }

    #[test]
    fn the_hidden_window_turns_real_system_messages_into_events() {
        let (tx, rx) = unbounded();
        spawn(move |event| {
            let _ = tx.send(event);
        })
        .expect("建系统事件窗口");
        assert!(spawn(|_| {}).is_err(), "只允许建一次");
        let hwnd = unsafe { FindWindowW(CLASS_NAME, None) }.expect("按类名找到隐藏窗口");
        let send = |msg: u32, wparam: usize, lparam: isize| unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))) };

        assert_eq!(send(WM_QUERYENDSESSION, 0, 0).0, 1, "同意关机");
        assert!(rx.try_recv().is_err());
        send(WM_POWERBROADCAST, PBT_APMSUSPEND as usize, 0);
        assert_eq!(rx.try_recv(), Ok(Event::Suspend));
        send(WM_ENDSESSION, 1, ENDSESSION_LOGOFF as isize);
        assert_eq!(rx.try_recv(), Ok(Event::EndSession(EndReason::Logoff)));
        send(WM_ENDSESSION, 1, 0);
        assert_eq!(rx.try_recv(), Ok(Event::EndSession(EndReason::Shutdown)));
        send(WM_ENDSESSION, 0, 0);
        assert!(rx.try_recv().is_err());
    }
}
