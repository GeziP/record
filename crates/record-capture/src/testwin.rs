//! 测试窗口：按 [`record_core::testpattern`] 绘制已知颜色分区和一条随时间移动的亮条（设计文档 10 的 S2、S3 自检）。
//!
//! 窗口置顶、不抢焦点、鼠标穿透（不会挡住用户操作）。这是验证用的辅助代码，不会进入正式的录制路径。

use crate::qpc::Qpc;
use record_core::clock::Hns;
use record_core::testpattern::{BAR_RGB, QUADRANT_RGB, STRIP_RGB, bar_left, bar_width, phase_at, quadrants, strip};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, GetDC, GetPixel, HBRUSH, InvalidateRect, PAINTSTRUCT, ReleaseDC,
    SRCCOPY, SelectObject, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetWindowLongPtrW, GetWindowRect, LWA_ALPHA, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage,
    RegisterClassExW, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_DESTROY, WM_ERASEBKGND, WM_NCCREATE, WM_NCHITTEST, WM_PAINT, WM_QUIT,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

#[derive(Debug, Clone)]
pub struct TestWindowSpec {
    /// 窗口左上角，桌面坐标（物理像素，进程需是 Per-Monitor DPI 感知）。
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// 亮条是否持续移动；关闭时只画一次（用于“静止首帧”测试）。
    pub animate: bool,
}

struct State {
    spec: TestWindowSpec,
    qpc: Qpc,
    t_ref: Hns,
}

pub struct TestWindow {
    pub hwnd: isize,
    pub spec: TestWindowSpec,
    /// 亮条相位为 0 的时刻（窗口创建时）。
    pub t_ref: Hns,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn rgb(c: [u8; 3]) -> COLORREF {
    COLORREF(u32::from(c[0]) | u32::from(c[1]) << 8 | u32::from(c[2]) << 16)
}

fn fill(hdc: windows::Win32::Graphics::Gdi::HDC, r: record_core::geometry::Rect, color: [u8; 3]) {
    let rc = RECT { left: r.x as i32, top: r.y as i32, right: r.right() as i32, bottom: r.bottom() as i32 };
    unsafe {
        let brush: HBRUSH = CreateSolidBrush(rgb(color));
        FillRect(hdc, &rc, brush);
        let _ = DeleteObject(brush.into());
    }
}

unsafe fn paint(hwnd: HWND, st: &State) {
    let (w, h) = (st.spec.width, st.spec.height);
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    // 先画到内存位图再整块拷贝，避免桌面复制抓到画了一半的画面
    let mem = unsafe { CreateCompatibleDC(Some(hdc)) };
    let bmp = unsafe { CreateCompatibleBitmap(hdc, w as i32, h as i32) };
    let old = unsafe { SelectObject(mem, bmp.into()) };
    for (r, c) in quadrants(w, h).into_iter().zip(QUADRANT_RGB) {
        fill(mem, r, c);
    }
    let s = strip(w, h);
    fill(mem, s, STRIP_RGB);
    if st.spec.animate {
        let left = bar_left(phase_at(st.qpc.now_hns(), st.t_ref), w);
        fill(mem, record_core::geometry::Rect { x: left, y: s.y, w: bar_width(w), h: s.h }, BAR_RGB);
    }
    unsafe {
        let _ = BitBlt(hdc, 0, 0, w as i32, h as i32, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
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
        // HTTRANSPARENT 是 -1；常量在不同绑定里的类型不同，直接写数值更稳妥
        WM_NCHITTEST => LRESULT(-1),
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn window_thread(spec: TestWindowSpec, qpc: Qpc, stop: Arc<AtomicBool>, ready: mpsc::Sender<Result<(isize, Hns), String>>) {
    let hinstance = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(format!("GetModuleHandle：{e}")));
            return;
        }
    };
    let class = w!("RecordS2TestWindow");
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinstance.into(), lpszClassName: class, ..Default::default() };
    // 同一进程里第二次注册会失败（类已存在），不影响创建窗口
    unsafe { RegisterClassExW(&wc) };

    let t_ref = qpc.now_hns();
    let state = Box::new(State { spec: spec.clone(), qpc, t_ref });
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT;
    let hwnd = match unsafe {
        CreateWindowExW(ex, class, PCWSTR::null(), WS_POPUP, spec.left, spec.top, spec.width as i32, spec.height as i32, None, None, Some(hinstance.into()), Some(&*state as *const State as *const _))
    } {
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
    let _ = ready.send(Ok((hwnd.0 as isize, t_ref)));

    let mut msg = MSG::default();
    'outer: loop {
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
        if state.spec.animate {
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
                let _ = UpdateWindow(hwnd);
            }
        }
        thread::sleep(Duration::from_millis(4));
    }
    drop(state);
}

/// 屏幕上某点的实际颜色（GDI 读取的是 DWM 合成后的结果），用作独立于桌面复制的参照。
pub fn screen_pixel(x: i32, y: i32) -> [u8; 3] {
    unsafe {
        let hdc = GetDC(None);
        let c = GetPixel(hdc, x, y).0;
        ReleaseDC(None, hdc);
        [(c & 0xFF) as u8, (c >> 8 & 0xFF) as u8, (c >> 16 & 0xFF) as u8]
    }
}

impl TestWindow {
    /// 窗口当前在桌面上的实际位置：`(left, top, width, height)`，以及它所在显示器的 DPI。
    pub fn actual_rect(&self) -> ((i32, i32, i32, i32), u32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd(), &mut r);
            ((r.left, r.top, r.right - r.left, r.bottom - r.top), GetDpiForWindow(self.hwnd()))
        }
    }

    pub fn open(spec: TestWindowSpec, qpc: Qpc) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (s, sp) = (Arc::clone(&stop), spec.clone());
        let thread = thread::Builder::new().name("test-window".into()).spawn(move || window_thread(sp, qpc, s, tx)).map_err(|e| e.to_string())?;
        let (hwnd, t_ref) = rx.recv().map_err(|_| "窗口线程提前退出".to_string())??;
        Ok(Self { hwnd, spec, t_ref, stop, thread: Some(thread) })
    }

    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        self.shutdown();
    }
}
