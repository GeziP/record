//! 线程级 COM 初始化：每个用到 WASAPI 的线程先建一个 [`ComGuard`]，线程结束时自动反初始化；进程里另外留着一个永远不退出的多线程单元成员线程。

use std::sync::Once;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

static KEEPER: Once = Once::new();

/// 启动一个永远不退出、留在多线程单元（MTA）里的线程（只做一次）；[`ComGuard::init`] 会自动调用它，外壳启动时也要先调用一次。
///
/// 为什么需要：最后一个 MTA 成员线程反初始化时，COM 会拆掉整个多线程单元，并卸载没有人在用的组件 DLL；
/// 而 `windows` 库把 WinRT 的激活工厂缓存在静态变量里（外壳弹系统通知要用 `XmlDocument`），
/// DLL 被卸载之后下一次用缓存的工厂就是访问违例。每场录制结束时采集线程全部退出，正好触发这件事：
/// 标准模式下第二场开始时弹“开始录制”通知，外壳进程直接崩溃（退出码 0xC0000005，崩溃位置在 `XmlDocument` 的激活函数里）。
pub fn keep_mta_alive() {
    KEEPER.call_once(|| {
        let (ready, wait) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("com-mta".into())
            .spawn(move || {
                let ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
                let _ = ready.send(ok);
                loop {
                    std::thread::park();
                }
            })
            .expect("启动 COM 单元守护线程");
        let _ = wait.recv();
    });
}

pub struct ComGuard(());

impl ComGuard {
    pub fn init() -> windows::core::Result<Self> {
        keep_mta_alive();
        // 重复初始化返回 S_FALSE，同样需要配对反初始化，所以只要不是失败码就算成功
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self(()))
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{APTTYPE, APTTYPE_MTA, APTTYPEQUALIFIER, CoGetApartmentType};

    /// 一个没有初始化过 COM 的新线程眼下是不是落在 MTA 里（隐式 MTA）；没有 MTA 时系统返回 CO_E_NOTINITIALIZED。
    fn fresh_thread_is_in_mta() -> bool {
        std::thread::spawn(|| unsafe {
            let (mut kind, mut qualifier) = (APTTYPE(0), APTTYPEQUALIFIER(0));
            CoGetApartmentType(&mut kind, &mut qualifier).is_ok() && kind == APTTYPE_MTA
        })
        .join()
        .unwrap()
    }

    #[test]
    fn the_mta_outlives_the_last_guard() {
        // 一个线程建了守卫又放掉；没有守护线程的话，它就是最后一个 MTA 成员，单元随之被拆掉
        std::thread::spawn(|| drop(ComGuard::init().expect("COM 初始化"))).join().unwrap();
        assert!(fresh_thread_is_in_mta(), "最后一个守卫放掉之后，多线程单元被拆掉了");
    }
}
