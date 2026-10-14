//! 可替换的共享句柄：采集线程每次交付数据时取当前写入器；编码器重启时换成新的，采集线程不用停。
//!
//! 交付与更换用读写锁隔开：采集线程在读锁里“交付并计数”，更换在写锁里“读计数并换上新的”，
//! 所以新写入器接手的位置与已交付的数量严格对得上，不会有数据同时交给新旧两个写入器，也不会漏。

use parking_lot::{RwLock, RwLockReadGuard};
use std::sync::Arc;

pub(crate) struct Slot<T>(RwLock<Arc<T>>);

impl<T> Slot<T> {
    pub fn new(value: T) -> Arc<Self> {
        Arc::new(Self(RwLock::new(Arc::new(value))))
    }

    pub fn get(&self) -> Arc<T> {
        Arc::clone(&self.0.read())
    }

    /// 持有读锁期间 [`Slot::replace_with`] 会等着。交付数据的采集线程在这里完成“交给写入器 + 计数”。
    pub fn read(&self) -> RwLockReadGuard<'_, Arc<T>> {
        self.0.read()
    }

    /// 换上新的，返回旧的。`at_swap` 在写锁内、替换之前执行：此时没有任何线程在交付数据，
    /// 用它读“已交付多少”，得到的就是旧写入器收到的全部、新写入器还没收到的起点。
    pub fn replace_with(&self, value: T, at_swap: impl FnOnce()) -> Arc<T> {
        let mut guard = self.0.write();
        at_swap();
        std::mem::replace(&mut *guard, Arc::new(value))
    }

    /// 所有持有者都放手之后取回里面的值；还有别人持有时返回 `None`。
    pub fn into_inner(self: Arc<Self>) -> Option<T> {
        let slot = Arc::try_unwrap(self).ok()?;
        Arc::try_unwrap(slot.0.into_inner()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn get_returns_the_current_value_and_replace_swaps_it() {
        let slot = Slot::new(1);
        assert_eq!(*slot.get(), 1);
        let old = slot.replace_with(2, || {});
        assert_eq!(*old, 1);
        assert_eq!(*slot.get(), 2);
    }

    #[test]
    fn the_swap_hook_runs_while_the_write_lock_is_held() {
        let slot = Slot::new(1);
        let mut locked = false;
        slot.replace_with(2, || locked = slot.0.try_read().is_none());
        assert!(locked, "钩子执行期间不能有任何读者在交付数据");
        assert!(slot.0.try_read().is_some(), "钩子结束后锁已释放");
    }

    #[test]
    fn into_inner_returns_the_value_when_nobody_else_holds_it() {
        assert_eq!(Slot::new(String::from("w")).into_inner(), Some(String::from("w")));
    }

    #[test]
    fn into_inner_refuses_while_the_slot_itself_is_shared() {
        let slot = Slot::new(1);
        let other = Arc::clone(&slot);
        assert_eq!(slot.into_inner(), None);
        assert_eq!(*other.get(), 1, "失败的 into_inner 不能弄坏另一个持有者");
    }

    #[test]
    fn into_inner_refuses_while_a_handle_to_the_value_is_out() {
        let slot = Slot::new(1);
        let held = slot.get();
        assert_eq!(slot.into_inner(), None);
        assert_eq!(*held, 1);
    }

    /// 并发场景：一个线程不停地“交付并计数”，另一个线程中途更换。
    /// 钩子里读到的计数必须正好等于旧写入器收到的数量，之后新写入器收到的就是其余的。
    #[test]
    fn the_count_read_at_swap_time_is_exactly_what_the_old_writer_received() {
        for _ in 0..20 {
            let slot = Slot::new(AtomicU64::new(0));
            let delivered = Arc::new(AtomicU64::new(0));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let feeder = {
                let (slot, delivered, stop) = (Arc::clone(&slot), Arc::clone(&delivered), Arc::clone(&stop));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let w = slot.read();
                        w.fetch_add(1, Ordering::Relaxed);
                        delivered.fetch_add(1, Ordering::Release);
                    }
                })
            };
            std::thread::sleep(std::time::Duration::from_millis(5));
            let mut base = 0;
            let old = slot.replace_with(AtomicU64::new(0), || base = delivered.load(Ordering::Acquire));
            std::thread::sleep(std::time::Duration::from_millis(5));
            stop.store(true, Ordering::Release);
            feeder.join().unwrap();
            assert_eq!(old.load(Ordering::Relaxed), base, "旧写入器收到的 = 更换时的计数");
            assert_eq!(slot.get().load(Ordering::Relaxed) + base, delivered.load(Ordering::Relaxed), "新写入器收到其余的，不多不少");
        }
    }
}
