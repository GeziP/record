//! 录制期间不让显示器和系统因空闲而休眠（设计文档 5.2）。显示器一休眠，桌面复制就拿不到画面，成品里只剩声音。
//!
//! 用电源请求（`PowerCreateRequest`）而不是 `SetThreadExecutionState`：请求绑在句柄上而不是线程上，在哪个线程创建、在哪个线程放手都行；
//! 进程退出或被杀时系统会关掉句柄，请求随之撤销，不会留下“永远不休眠”的残局。

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Power::{PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetRequest};
use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};
use windows::core::PWSTR;

/// 一份“保持唤醒”的电源请求：显示器常亮、系统不因空闲而睡眠。放手（`Drop`）时两项都撤销。
/// 它只拦空闲计时：用户自己点“睡眠”、合上笔记本盖子、电量耗尽这些睡眠它拦不住，也不该拦。
#[derive(Debug)]
pub struct Awake {
    handle: HANDLE,
}

// 电源请求的句柄不属于哪个线程。
unsafe impl Send for Awake {}

impl Awake {
    /// `reason` 会出现在系统的电源请求列表里（管理员运行 `powercfg /requests` 能看到）。
    pub fn new(reason: &str) -> windows::core::Result<Self> {
        let mut text: Vec<u16> = reason.encode_utf16().chain(Some(0)).collect();
        let context = REASON_CONTEXT { Version: 0, Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING, Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(text.as_mut_ptr()) } };
        let handle = unsafe { PowerCreateRequest(&context) }?;
        // 句柄一到手就交给 `Self`：后面任何一步失败，`Drop` 都会撤销已经设上的请求并关掉句柄
        let awake = Self { handle };
        unsafe {
            PowerSetRequest(handle, PowerRequestDisplayRequired)?;
            PowerSetRequest(handle, PowerRequestSystemRequired)?;
        }
        tracing::info!("已请求保持唤醒：显示器常亮、系统不因空闲休眠，直到本场保存或结束");
        Ok(awake)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        unsafe {
            let _ = PowerClearRequest(self.handle, PowerRequestDisplayRequired);
            let _ = PowerClearRequest(self.handle, PowerRequestSystemRequired);
            let _ = CloseHandle(self.handle);
        }
        tracing::info!("已撤销保持唤醒");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use windows::Win32::System::Power::{CallNtPowerInformation, SystemExecutionState};

    const SYSTEM_REQUIRED: u32 = 0x1;
    const DISPLAY_REQUIRED: u32 = 0x2;
    const BOTH: u32 = SYSTEM_REQUIRED | DISPLAY_REQUIRED;

    /// 整机汇总的“执行状态”：所有进程的电源请求叠加的结果。不需要管理员权限。
    fn execution_state() -> u32 {
        let mut state = 0u32;
        let status = unsafe { CallNtPowerInformation(SystemExecutionState, None, 0, Some((&mut state as *mut u32).cast()), 4) };
        assert_eq!(status.0, 0, "读整机执行状态失败：0x{:X}", status.0);
        state
    }

    /// 测试读的是整机状态，同一个进程里的几个测试会互相影响，所以排队跑。
    /// 别的程序（比如在放视频的浏览器）也可能占着请求：撤销只断言“放手前没有的位，放手后也没有”，不要求整个状态为零。
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn a_held_request_keeps_display_and_system_awake_until_it_is_dropped() {
        let _serial = SERIAL.lock();
        let before = execution_state();
        let awake = Awake::new("Record 单元测试（马上撤销）").expect("创建电源请求");
        let during = execution_state();
        assert_eq!(during & BOTH, BOTH, "显示器和系统两个请求都应该生效：0x{during:X}");
        drop(awake);
        let after = execution_state();
        assert_eq!(after & !before & BOTH, 0, "放手后自己造成的请求应该都撤销了：之前 0x{before:X}，之后 0x{after:X}");
    }

    #[test]
    fn a_request_can_be_dropped_on_another_thread() {
        let _serial = SERIAL.lock();
        let before = execution_state();
        let awake = Awake::new("Record 单元测试（马上撤销）").expect("创建电源请求");
        std::thread::spawn(move || drop(awake)).join().unwrap();
        let after = execution_state();
        assert_eq!(after & !before & BOTH, 0, "换个线程放手也应该撤销：之前 0x{before:X}，之后 0x{after:X}");
    }
}
