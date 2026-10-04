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
//! （不能另开一条 `&mut` 通道，那等于把退出拆成两份实现）。停止位、`join` 句柄与完成位
//! 因此都用内部可变性：`AtomicBool` + `Mutex<Option<JoinHandle>>` + 完成位。`stop` 依旧幂等。
//!
//! **并发调用 `stop` 也保证「返回 ⇒ 线程已退出」**（fix round 1，评审 M1）：句柄只能被
//! 取走一次，第二个调用者拿不到句柄，于是它等的是**线程自己在退出前置的完成位**（panic
//! 展开也置位）。停一次的标志仍由停止位保证：置位之后不会再有新的一拍。

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
    /// 线程**已退出**（正常返回或 panic 展开都会置位）。
    ///
    /// 为什么需要它：`JoinHandle` 只能被取走一次，第二个 `stop()` 调用者拿不到句柄，
    /// 只凭「取不到句柄」就返回会让「返回 ⇒ 线程已退出」这句话不成立（M1）。
    done: Arc<AtomicBool>,
    /// 已完成的触发次数。只用于诊断与测试，不参与任何业务判断。
    ticks: Arc<AtomicU64>,
}

/// 线程退出前置完成位。**panic 也置位**：否则并发的第二个 `stop()` 会等一个
/// 永远不会到来的信号（`on_tick` panic 时线程直接结束，不走正常返回路径）。
struct DoneOnDrop(Arc<AtomicBool>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
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
        let done = Arc::new(AtomicBool::new(false));
        let ticks = Arc::new(AtomicU64::new(0));

        let handle = {
            let stop = Arc::clone(&stop);
            let ticks = Arc::clone(&ticks);
            let done = Arc::clone(&done);
            thread::Builder::new()
                .name("worktrace-sampler".to_string())
                .spawn(move || {
                    // 线程退出（含 panic 展开）时置完成位，供并发的第二个 `stop()` 等待。
                    let _done = DoneOnDrop(done);
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
            done,
            ticks,
        }
    }

    /// 已完成的触发次数。
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::SeqCst)
    }

    /// 停止并等待线程退出。可重复调用，且**只要共享引用就能调**（见模块头「停」）。
    ///
    /// **返回 ⇒ 线程已退出**，并发调用也成立（M1）：
    ///
    /// - 拿到句柄的那个调用者 `join` 线程；
    /// - 没拿到句柄的（句柄已被别人取走）等的是**完成位**——它由线程自己在退出前置位，
    ///   所以不会早于线程真正结束而返回。
    ///
    /// `join` 与等待完成位都在**放开那把 `Mutex` 之后**进行：不让别人为了一把已经
    /// 没用的锁排队。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();

        match handle {
            Some(handle) => {
                let _ = handle.join();
            }
            None => {
                while !self.done.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}

impl Drop for Scheduler {
    /// **注意（fix round 1 复评 N2）**：这里无条件 `stop()`，**不查**
    /// `holds_app_lock`。所以「同一个线程既持有串行边界的 guard、又丢弃自己拥有的
    /// `Scheduler`」能绕过 `RunningApp::shutdown` 的那道自死锁防线（`drop` 会 `join`，
    /// 而采样线程正堵在那把锁上）。**当前生产路径不可达**：`Scheduler` 只被 `RunningApp`
    /// 拥有，而 `RunningApp` 由 Tauri 托管、按进程生命周期析构（那时没有别的线程持锁）。
    /// P8 若新增「拥有并显式丢弃 `RunningApp`」的退出路径，必须先放掉那把锁——
    /// 与 `shutdown` 的姿势一致。
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

    /// **并发调用 `stop` 也保证「返回 ⇒ 线程已退出」**（fix round 1，评审 M1）。
    ///
    /// 做法是让线程**卡在 `on_tick` 里**（barrier）：这时句柄已经被第一个调用者取走，
    /// 第二个调用者只能等完成位。若第二个调用者「拿不到句柄就返回」，它会在闸门放开之前
    /// 就返回 —— 下面那条 `returned == 0` 会红。
    #[test]
    fn every_concurrent_stop_waits_until_the_thread_has_finished() {
        let gate = Arc::new(std::sync::Barrier::new(2));
        let started = Arc::new(AtomicBool::new(false));
        let returned = Arc::new(AtomicUsize::new(0));

        let scheduler = Arc::new({
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            Scheduler::spawn(5, move || {
                if !started.swap(true, Ordering::SeqCst) {
                    // 只在第一拍卡住：让线程停在 `on_tick` 内部，join 必须等它。
                    gate.wait();
                }
            })
        });

        // 等线程真的进了 `on_tick`（否则它可能还在睡，stop 会走另一条路）。
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !started.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "采样线程没有跑起来");
            thread::sleep(Duration::from_millis(5));
        }

        std::thread::scope(|scope| {
            for _ in 0..2 {
                let scheduler = Arc::clone(&scheduler);
                let returned = Arc::clone(&returned);
                scope.spawn(move || {
                    scheduler.stop();
                    returned.fetch_add(1, Ordering::SeqCst);
                });
            }

            // 两个 stop 都还没返回：一个在 join，另一个在等完成位。
            thread::sleep(Duration::from_millis(100));
            assert_eq!(
                returned.load(Ordering::SeqCst),
                0,
                "线程还卡在 on_tick 里，两个 stop 都不该已经返回"
            );

            // 放闸：线程走完这一拍 → 看到停止位 → 退出 → 完成位置位 → 两个 stop 返回。
            gate.wait();
        });

        assert_eq!(returned.load(Ordering::SeqCst), 2, "两个 stop 都该返回");
        let after = scheduler.ticks();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(scheduler.ticks(), after, "两个 stop 返回之后不得再有触发");
    }
}
