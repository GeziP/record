//! 闪屏窗口：整块纯色在黑与白之间切换，用来测量画面与声音的同步（设计文档 10 的 S3）。
//!
//! 窗口置顶、不抢焦点、鼠标穿透。切换用事件唤醒窗口线程，不靠定时轮询，从调用 [`FlashWindow::set`] 到开始重绘不到 1 毫秒。
//! 这是验证用的辅助代码，不会进入正式的录制路径。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{COLORREF, CloseHandle, HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, UpdateWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, LWA_ALPHA, MSG, MsgWaitForMultipleObjects, PM_REMOVE,
    PeekMessageW, PostQuitMessage, QS_ALLINPUT, RegisterClassExW, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_DESTROY, WM_ERASEBKGND,
    WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_QUIT, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

struct State {
    on: Arc<AtomicBool>,
}

pub struct FlashWindow {
    on: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// 唤醒窗口线程的事件（句柄值，线程间传递用）。
    signal: isize,
    thread: Option<JoinHandle<()>>,
}

unsafe fn paint(hwnd: HWND, st: &State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
        let level = if st.on.load(Ordering::Acquire) { 255 } else { 0 };
        let brush = CreateSolidBrush(COLORREF(level | level << 8 | level << 16));
        FillRect(hdc, &rc, brush);
        let _ = DeleteObject(brush.into());
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = lparam.0 as *const CREATESTRUCTW;
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize) };
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let st = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    match msg {
        WM_PAINT if !st.is_null() => {
            unsafe { paint(hwnd, &*st) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // HTTRANSPARENT 是 -1：鼠标点击穿过窗口，不挡住用户操作
        WM_NCHITTEST => LRESULT(-1),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn window_thread(rect: (i32, i32, u32, u32), on: Arc<AtomicBool>, stop: Arc<AtomicBool>, signal: isize, ready: mpsc::Sender<Result<(), String>>) {
    let hinstance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(format!("GetModuleHandle：{e}")));
            return;
        }
    };
    let class = w!("RecordFlashWindow");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinstance.into(), lpszClassName: class, ..Default::default() };
    // 同一进程里第二次注册会失败（类已存在），不影响创建窗口
    unsafe { RegisterClassExW(&wc) };

    let state = Box::new(State { on });
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT;
    let (left, top, width, height) = rect;
    let hwnd =
        match unsafe { CreateWindowExW(ex, class, PCWSTR::null(), WS_POPUP, left, top, width as i32, height as i32, None, None, Some(hinstance.into()), Some(&*state as *const State as *const _)) } {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("CreateWindowEx：{e}")));
                return;
            }
        };
    unsafe {
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = UpdateWindow(hwnd);
    }
    let _ = ready.send(Ok(()));

    let handles = [HANDLE(signal as *mut _)];
    let mut msg = MSG::default();
    'outer: loop {
        // 有事件（要切换颜色）或消息就醒；50 毫秒的上限只是为了定期看一眼停止标志
        let woke = unsafe { MsgWaitForMultipleObjects(Some(&handles), false, 50, QS_ALLINPUT) };
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                break 'outer;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if stop.load(Ordering::Acquire) {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            continue;
        }
        if woke == WAIT_OBJECT_0 {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
                let _ = UpdateWindow(hwnd);
            }
        }
    }
    drop(state);
}

impl FlashWindow {
    /// 在桌面坐标 `(left, top)` 处开一个 `width`×`height`（物理像素；进程需是 Per-Monitor DPI 感知）的窗口，初始为黑色。
    pub fn open(left: i32, top: i32, width: u32, height: u32) -> Result<Self, String> {
        let on = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let signal = unsafe { CreateEventW(None, false, false, None) }.map_err(|e| format!("创建事件：{e}"))?;
        let signal = signal.0 as isize;
        let (tx, rx) = mpsc::channel();
        let (on2, stop2) = (Arc::clone(&on), Arc::clone(&stop));
        let thread = thread::Builder::new().name("flash-window".into()).spawn(move || window_thread((left, top, width, height), on2, stop2, signal, tx)).map_err(|e| e.to_string())?;
        rx.recv().map_err(|_| "窗口线程提前退出".to_string())??;
        Ok(Self { on, stop, signal, thread: Some(thread) })
    }

    /// 切换成白（`true`）或黑（`false`）。立即返回，重绘由窗口线程完成。
    pub fn set(&self, on: bool) {
        self.on.store(on, Ordering::Release);
        unsafe {
            let _ = SetEvent(HANDLE(self.signal as *mut _));
        }
    }

    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        unsafe {
            let _ = SetEvent(HANDLE(self.signal as *mut _));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            unsafe {
                let _ = CloseHandle(HANDLE(self.signal as *mut _));
            }
        }
    }
}

impl Drop for FlashWindow {
    fn drop(&mut self) {
        self.shutdown();
    }
}
