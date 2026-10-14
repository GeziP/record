//! 视频帧队列：容量满时丢最旧的一帧（设计文档 5.9：音频绝不丢，视频队列满时丢最旧帧）。

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct DropOldest<T> {
    tx: Option<Sender<T>>,
    rx: Receiver<T>,
    dropped: AtomicU64,
}

impl<T> DropOldest<T> {
    pub fn new(capacity: usize) -> Self {
        let (tx, rx) = bounded(capacity.max(1));
        Self { tx: Some(tx), rx, dropped: AtomicU64::new(0) }
    }

    /// 入队；队列已满时先丢掉最旧的，返回本次丢弃的条数。关闭后入队的数据被丢弃，返回 0。
    pub fn push(&self, item: T) -> u64 {
        let Some(tx) = &self.tx else { return 0 };
        let mut item = item;
        let mut dropped = 0;
        loop {
            match tx.try_send(item) {
                Ok(()) => break,
                Err(TrySendError::Full(back)) => {
                    item = back;
                    if self.rx.try_recv().is_ok() {
                        dropped += 1;
                    }
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        self.dropped.fetch_add(dropped, Ordering::Relaxed);
        dropped
    }

    pub fn receiver(&self) -> Receiver<T> {
        self.rx.clone()
    }

    pub fn total_dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 关闭发送端：消费线程取完队列里剩余的数据后收到“断开”。
    pub fn close(&mut self) {
        self.tx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_items_when_full() {
        let q = DropOldest::new(3);
        let mut dropped = 0;
        for i in 0..10 {
            dropped += q.push(i);
        }
        assert_eq!(dropped, 7);
        assert_eq!(q.total_dropped(), 7);
        let rx = q.receiver();
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(got, vec![7, 8, 9]);
    }

    #[test]
    fn nothing_is_dropped_below_capacity() {
        let q = DropOldest::new(5);
        for i in 0..5 {
            assert_eq!(q.push(i), 0);
        }
        assert_eq!(q.total_dropped(), 0);
    }

    #[test]
    fn close_lets_the_consumer_drain_then_disconnect() {
        let mut q = DropOldest::new(4);
        q.push(1);
        q.push(2);
        let rx = q.receiver();
        q.close();
        assert_eq!(q.push(3), 0, "关闭后不再入队");
        assert_eq!(rx.recv().unwrap(), 1);
        assert_eq!(rx.recv().unwrap(), 2);
        assert!(rx.recv().is_err());
    }
}
