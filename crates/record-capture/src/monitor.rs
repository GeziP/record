//! 显示器枚举与选择（设计文档 5.2）。
//!
//! 每块屏对应一个 DXGI 输出，附带桌面坐标、旋转、HDR、刷新率与缩放：既用于选屏
//! （按下热键时前台窗口所在的那块），也写进每次录制的环境快照（设计文档 5.10）。
//! 进程必须是 Per-Monitor DPI 感知的，否则取到的是被虚拟化的逻辑坐标，见 [`ensure_dpi_aware`]。

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6};
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::{Interface, PCWSTR};

/// `MONITORINFO.dwFlags` 里表示主显示器的位。
const MONITORINFOF_PRIMARY_FLAG: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub adapter_index: u32,
    pub output_index: u32,
    pub adapter: String,
    /// 形如 `\\.\DISPLAY1`。
    pub device_name: String,
    pub hmonitor: isize,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// `DXGI_MODE_ROTATION`：1 = 不旋转，2 = 90°，3 = 180°，4 = 270°。
    pub rotation: u32,
    pub primary: bool,
    /// 输出当前处于 HDR10（BT.2020 + PQ）。
    pub hdr: bool,
    /// HDR 开启时系统的“SDR 内容亮度”（尼特）。SDR 内容在 HDR 桌面里按它放大，转回 SDR 要除掉这个比例。
    pub sdr_white_nits: Option<f32>,
    pub refresh_hz: u32,
    pub dpi_scale_percent: u32,
}

impl MonitorInfo {
    pub fn describe(&self) -> String {
        format!(
            "{} 在 {}：{}×{} @ {} Hz，位置 ({}, {})，缩放 {}%{}{}{}",
            self.device_name,
            self.adapter,
            self.width,
            self.height,
            self.refresh_hz,
            self.left,
            self.top,
            self.dpi_scale_percent,
            if self.primary { "，主显示器" } else { "" },
            if self.hdr { "，HDR 开" } else { "" },
            match (self.hdr, self.sdr_white_nits) {
                (true, Some(n)) => format!("（SDR 内容亮度 {n:.0} 尼特）"),
                _ => String::new(),
            } + &if self.rotation > 1 { format!("，旋转代码 {}", self.rotation) } else { String::new() },
        )
    }
}

/// 选屏方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// 前台窗口所在的显示器（没有前台窗口时取主显示器）。
    Foreground,
    Primary,
    /// 在 [`list_monitors`] 结果中的位置。
    Index(usize),
    Hmonitor(isize),
}

/// 让进程成为 Per-Monitor V2 DPI 感知。要在创建任何窗口之前调用；已经设置过时返回错误，可以忽略。
pub fn ensure_dpi_aware() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

fn refresh_rate(device_name: &[u16]) -> u32 {
    let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    let ok = unsafe { EnumDisplaySettingsW(PCWSTR(device_name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode) };
    if ok.as_bool() { mode.dmDisplayFrequency } else { 0 }
}

fn is_primary(hmon: HMONITOR) -> bool {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() && mi.dwFlags & MONITORINFOF_PRIMARY_FLAG != 0
}

/// 某块显示器（GDI 设备名，如 `\\.\DISPLAY1`）当前的 SDR 内容亮度，单位尼特。
/// 系统接口给的是以 1000 = 80 尼特为基准的整数。查询失败返回 `None`。
pub fn sdr_white_nits(device_name: &str) -> Option<f32> {
    let header = |kind, adapter, id, size: usize| DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id };
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm).ok().ok()?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None).ok().ok()?;
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, p.sourceInfo.adapterId, p.sourceInfo.id, std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>()),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 || wide_to_string(&src.viewGdiDeviceName) != device_name {
                continue;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, p.targetInfo.adapterId, p.targetInfo.id, std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>()),
                ..Default::default()
            };
            return (DisplayConfigGetDeviceInfo(&mut white.header) == 0 && white.SDRWhiteLevel > 0).then(|| white.SDRWhiteLevel as f32 * 80.0 / 1000.0);
        }
        None
    }
}

fn dpi_scale_percent(hmon: HMONITOR) -> u32 {
    let (mut x, mut y) = (0u32, 0u32);
    match unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x > 0 => (x * 100 + 48) / 96,
        _ => 100,
    }
}

/// 枚举所有接在桌面上的显示器，按“显卡序号、输出序号”排列。
pub fn list_monitors() -> windows::core::Result<Vec<MonitorInfo>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut found = Vec::new();
    let mut adapter_index = 0u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e),
        };
        let adapter_name = wide_to_string(&unsafe { adapter.GetDesc1()? }.Description);
        let mut output_index = 0u32;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e),
            };
            let desc = unsafe { output.GetDesc()? };
            if desc.AttachedToDesktop.as_bool() {
                let r = desc.DesktopCoordinates;
                let hdr = output.cast::<IDXGIOutput6>().ok().and_then(|o| unsafe { o.GetDesc1() }.ok()).is_some_and(|d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
                let device_name = wide_to_string(&desc.DeviceName);
                found.push(MonitorInfo {
                    adapter_index,
                    output_index,
                    adapter: adapter_name.clone(),
                    sdr_white_nits: if hdr { sdr_white_nits(&device_name) } else { None },
                    device_name,
                    hmonitor: desc.Monitor.0 as isize,
                    left: r.left,
                    top: r.top,
                    width: (r.right - r.left).max(0) as u32,
                    height: (r.bottom - r.top).max(0) as u32,
                    rotation: desc.Rotation.0 as u32,
                    primary: is_primary(desc.Monitor),
                    hdr,
                    refresh_hz: refresh_rate(&desc.DeviceName),
                    dpi_scale_percent: dpi_scale_percent(desc.Monitor),
                });
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    Ok(found)
}

/// 某个窗口所在的显示器（句柄值）。
pub fn hmonitor_of(hwnd: HWND) -> isize {
    unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) }.0 as isize
}

/// 前台窗口所在的显示器；没有前台窗口时是主显示器。
pub fn foreground_hmonitor() -> isize {
    hmonitor_of(unsafe { GetForegroundWindow() })
}

/// 按 `pick` 从 `list` 里选一块屏；没有匹配时返回 `None`，由调用方报告，不替用户换一块屏。
pub fn pick_monitor(list: &[MonitorInfo], pick: Pick) -> Option<&MonitorInfo> {
    match pick {
        Pick::Foreground => {
            let h = foreground_hmonitor();
            list.iter().find(|m| m.hmonitor == h)
        }
        Pick::Primary => list.iter().find(|m| m.primary),
        Pick::Index(i) => list.get(i),
        Pick::Hmonitor(h) => list.iter().find(|m| m.hmonitor == h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(index: u32, hmonitor: isize, primary: bool) -> MonitorInfo {
        MonitorInfo {
            adapter_index: 0,
            output_index: index,
            adapter: "测试显卡".into(),
            device_name: format!(r"\\.\DISPLAY{}", index + 1),
            hmonitor,
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
            rotation: 1,
            primary,
            hdr: false,
            sdr_white_nits: None,
            refresh_hz: 60,
            dpi_scale_percent: 100,
        }
    }

    #[test]
    fn pick_by_index_primary_and_handle() {
        let list = vec![fake(0, 100, false), fake(1, 200, true)];
        assert_eq!(pick_monitor(&list, Pick::Index(0)).unwrap().hmonitor, 100);
        assert_eq!(pick_monitor(&list, Pick::Primary).unwrap().hmonitor, 200);
        assert_eq!(pick_monitor(&list, Pick::Hmonitor(200)).unwrap().output_index, 1);
        assert!(pick_monitor(&list, Pick::Index(5)).is_none());
        assert!(pick_monitor(&list, Pick::Hmonitor(999)).is_none());
    }

    #[test]
    fn the_machine_has_at_least_one_monitor_with_a_sane_size() {
        let list = list_monitors().expect("枚举显示器");
        assert!(!list.is_empty(), "没有显示器");
        for m in &list {
            assert!(m.width >= 640 && m.height >= 480, "{}", m.describe());
            assert!(m.refresh_hz >= 24, "{}", m.describe());
            assert!(matches!(m.dpi_scale_percent, 100..=500), "{}", m.describe());
        }
        assert!(list.iter().any(|m| m.primary), "没有主显示器");
    }

    #[test]
    fn describe_mentions_the_key_facts() {
        let mut m = fake(0, 1, true);
        m.hdr = true;
        m.sdr_white_nits = Some(240.0);
        let text = m.describe();
        assert!(text.contains("1920×1080") && text.contains("主显示器") && text.contains("HDR") && text.contains("240 尼特"), "{text}");
    }

    #[test]
    fn the_sdr_white_level_is_known_for_an_hdr_monitor() {
        let list = list_monitors().expect("枚举显示器");
        for m in list.iter().filter(|m| m.hdr) {
            let nits = m.sdr_white_nits.unwrap_or_else(|| panic!("HDR 显示器取不到 SDR 白电平：{}", m.describe()));
            assert!((80.0..=480.0).contains(&nits), "SDR 白电平 {nits} 尼特超出常见范围");
        }
    }
}
