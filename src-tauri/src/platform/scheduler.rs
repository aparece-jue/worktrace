//! 周期采样驱动（P7 Task 0、F-009）。**平台叶子**：只提供「按时触发」，不懂业务。
//!
//! ## 为什么是一个**不依赖任何窗口**的线程
//!
//! F-009 要求「关闭或隐藏全部窗口时核心继续运行」：定时器**不得**挂在任何窗口
//! 对象上（挂在窗口上的定时器会随窗口一起消失，计时就在用户关窗时静默停摆）。
//! 所以这里是一个普通后台线程，生命周期只跟进程走。
//!
//! ## 触发动作必须走**与用户命令同一条串行边界**
//!
//! 本模块**不知道**那条边界是什么——它只接受一个闭包。接线方
//! （[`crate::services::bootstrap`]）传入的闭包在 `AppState` 的同一把锁下执行，
//! 于是「周期触发」与「用户命令」不可能并发进入协调器或数据库。
//! 把这条留成调用方的责任而不是在这里加锁：调度器加锁只会得到**第二把**锁。
//!
//! ## 停
//!
//! [`Scheduler::stop`]（以及 `Drop`）置停止位并 `join`，所以退出路径上不会再有一拍
//! 落在「事务已经结束」之后。等待被切成 ≤ 50ms 的小片，停止不需要等满一个周期。
//!
//! `stop` 取 **`&self`**：显式退出的调用方（`RunningApp::shutdown`）在组合根里只拿得到
//! 共享引用——Tauri 托管状态给出的就是 `&RunningApp`，而托盘的「退出」必须走那一条入口
//! （不能另开一条 `&mut` 通道，那等于把退出拆成两份实现）。停止位与 `join` 句柄因此
//! 都用内部可变性：`AtomicBool` + `Mutex<Option<JoinHandle>>`。`stop` 依旧幂等。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// 一次 `sleep` 最多睡这么久，让 `stop` 能在半个周期内生效。
const MAX_SLEEP_SLICE: Duration = Duration::from_millis(50);

/// 后台周期驱动。**Drop 会停止并 join**。
#[derive(Debug)]
pub struct Scheduler {
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
    /// 已完成的触发次数。只用于诊断与测试，不参与任何业务判断。
    ticks: Arc<AtomicU64>,
}

impl Scheduler {
    /// 起一个后台线程，每 `interval_ms` 毫秒调一次 `on_tick`。
    ///
    /// 用毫秒整数而不是 `Duration`：全仓的时间量都是 `_ms` 整数
    /// （`HEARTBEAT_INTERVAL_MS`、`expected_interval_ms`、`active_ms`…），
    /// 而且 `services` 层被门禁禁止出现 `std::time`——让平台层收毫秒，
    /// 服务层就不必为了传一个节拍去 import `Duration`。
    ///
    /// `interval_ms` 会被夹到至少 1ms：0 会让线程空转。
    pub fn spawn<F>(interval_ms: u64, mut on_tick: F) -> Self
    where
        F: FnMut() + Send + 'static,
    {
        let interval = Duration::from_millis(interval_ms.max(1));
        let stop = Arc::new(AtomicBool::new(false));
        let ticks = Arc::new(AtomicU64::new(0));

        let handle = {
            let stop = Arc::clone(&stop);
            let ticks = Arc::clone(&ticks);
            thread::Builder::new()
                .name("worktrace-sampler".to_string())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        if !sleep_in_slices(interval, &stop) {
                            break;
                        }
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        on_tick();
                        ticks.fetch_add(1, Ordering::SeqCst);
                    }
                })
                .expect("sampler thread")
        };

        Self {
            stop,
            handle: Mutex::new(Some(handle)),
            ticks,
        }
    }

    /// 已完成的触发次数。
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::SeqCst)
    }

    /// 停止并等待线程退出。可重复调用，且**只要共享引用就能调**（见模块头「停」）。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // 先取出句柄、**放开那把 `Mutex`**，再 `join`：join 期间不让别人为了一把已经
        // 没用的锁排队（`stop` 是幂等的，第二个调用者拿到 `None` 直接返回）。
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 分片睡眠。返回 `false` 表示期间收到了停止信号。
fn sleep_in_slices(total: Duration, stop: &AtomicBool) -> bool {
    let mut left = total;
    while left > Duration::ZERO {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let slice = left.min(MAX_SLEEP_SLICE);
        thread::sleep(slice);
        left -= slice;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// 窗口全关也照跑：调度器只认进程，没有任何窗口引用可传进来。
    #[test]
    fn ticks_until_stopped_without_any_window_reference() {
        let seen = Arc::new(AtomicUsize::new(0));
        let scheduler = {
            let seen = Arc::clone(&seen);
            Scheduler::spawn(5, move || {
                seen.fetch_add(1, Ordering::SeqCst);
            })
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while seen.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let before = seen.load(Ordering::SeqCst);
        assert!(before >= 3, "应至少触发 3 次，实际 {before}");

        scheduler.stop();
        let after_stop = seen.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            after_stop,
            "stop 之后不得再有触发"
        );
        assert_eq!(scheduler.ticks(), after_stop as u64);
    }

    /// Drop 也必须停：否则运行时会留下一个仍在写库的线程。
    #[test]
    fn dropping_the_scheduler_stops_the_thread() {
        let seen = Arc::new(AtomicUsize::new(0));
        {
            let seen = Arc::clone(&seen);
            let _scheduler = Scheduler::spawn(5, move || {
                seen.fetch_add(1, Ordering::SeqCst);
            });
            thread::sleep(Duration::from_millis(50));
        }
        let after_drop = seen.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            after_drop,
            "Drop 之后不得再触发"
        );
    }

    /// 停止不需要等满一个周期（分片睡眠）。
    #[test]
    fn stop_is_prompt_even_with_a_long_interval() {
        let scheduler = Scheduler::spawn(30_000, || {});
        let started = std::time::Instant::now();
        scheduler.stop();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "长周期下 stop 也应在 1 秒内返回，实际 {:?}",
            started.elapsed()
        );
    }
}
