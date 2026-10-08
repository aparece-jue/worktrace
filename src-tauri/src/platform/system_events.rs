//! 正式 OS 事件源（P6 Task 2c）。**平台叶子**：只做 OS 适配，不含业务规则。
//!
//! 它只回答一个问题：**系统刚刚说了什么**——「用户锁屏了」「要休眠了」「解锁了」
//! 「唤醒了」「系统时间被改了」，外加**事件时刻**的一次时钟采样。至于这些事实怎么
//! 影响工时，全在调用方（组合根注入的回调 → 协调器的平台边界入口）：本模块不认识
//! 协调器、不认识库、也不知道什么叫「暂停」。
//!
//! # 为什么是一个窗口
//!
//! Windows 上这三类通知**没有无窗口的订阅方式**：
//!
//! | 通知 | 消息 | 为什么必须是一个窗口 |
//! | --- | --- | --- |
//! | 锁屏 / 解锁 | `WM_WTSSESSION_CHANGE`（`WTSRegisterSessionNotification`） | API 要一个 HWND，且文档要求**顶层**窗口 |
//! | 休眠 / 唤醒 | `WM_POWERBROADCAST`（`PBT_APMSUSPEND` / `PBT_APMRESUMEAUTOMATIC`） | 系统把这条**广播**给顶层窗口，只有 HWND 收得到 |
//! | 系统改时 | `WM_TIMECHANGE` | 同上：广播给顶层窗口 |
//!
//! **消息专用窗口（`HWND_MESSAGE`）不行**：它不收广播消息，也不满足 WTS 的顶层要求。
//! 所以这里建的是一个**从不显示**的顶层窗口（不给 `WS_VISIBLE`），并且**自带线程与
//! 消息循环**——`GetMessage` 家族只把消息派发给**创建窗口的那个线程**。
//!
//! # 线程规则（与 `Scheduler` 同族的纪律）
//!
//! - 事件源**在事件线程上创建**（[`spawn_watched`] 收的是工厂而不是实例）：窗口句柄
//!   只在创建它的线程上有效，实例因此**从不跨线程移动**（也就没有 `unsafe impl Send`）。
//! - 停止条件是 [`AtomicBool`]，**不取任何业务锁**：置假之后消息循环最多再等
//!   [`MESSAGE_WAIT_MS`] 就返回，随后在**同一条线程**上注销通知、销毁窗口、注销窗口类。
//! - **没有任何路径 join 这条线程**。退出路径（`RunningApp::shutdown`）持锁 join 的是
//!   周期采样线程；事件线程只在**回调内部**取那把锁，所以「持锁 join 一个正堵在这把锁上
//!   的线程」那种自死锁在这里构造不出来。
//! - 所以回调**越快越好、且不得长时间持锁**：它就处在消息循环里。回调 panic 会穿过
//!   `extern "system"` 边界（平台未定义行为，dev/test 下线程直接结束），因此
//!   [`UnexpectedExitOnDrop`] 的看门狗与调用方的失败计数一并作为出口。
//!
//! # 失败路径
//!
//! 平台不支持或监听注册失败时：`start()` 返回 `Err`，[`spawn`] / [`spawn_watched`]
//! **同步**把它交回调用方（组合根只记一条诊断，不 panic、也不假装成功）；
//! 周期采样在另一条线程上，完全不受影响。注册成功之后线程再意外结束，由
//! `on_unexpected_exit` 报告一次（与 `Scheduler::spawn_watched` 同一手法）。
//!
//! # 本平台之外
//!
//! 非 Windows 走 [`UnsupportedSource`]：`start()` 明确报 `Unsupported`。**不做空实现**——
//! 静默的「成功」会让上层以为监听已经装好（正是本任务禁止的「假装成功」）。

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use crate::platform::clock::{ClockSample, SystemClock};

/// 事件线程名（诊断按它点名）。
pub const EVENT_THREAD_NAME: &str = "worktrace-system-events";

/// 消息循环一次等待的上限：`alive` 置假之后最多这么久就退出。
///
/// 200ms 是「停止要快」与「不要空转」的折中：这段时间里线程在
/// `MsgWaitForMultipleObjectsEx` 上睡着，事件一到就立刻醒。
pub const MESSAGE_WAIT_MS: u32 = 200;

/// 一次 OS 通知的类别。**只描述平台事实**，不含业务判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEventKind {
    /// 会话被锁定（用户锁屏 / 切换用户 / 屏保接管）。
    Locked,
    /// 会话解锁。
    Unlocked,
    /// 系统准备休眠。
    Suspending,
    /// 系统从休眠/睡眠中回来。
    Resumed,
    /// 系统时间被改动。
    TimeChanged,
}

impl SystemEventKind {
    /// 诊断用的稳定名字（测试与事后排查按它读）。
    pub fn as_str(self) -> &'static str {
        match self {
            SystemEventKind::Locked => "locked",
            SystemEventKind::Unlocked => "unlocked",
            SystemEventKind::Suspending => "suspending",
            SystemEventKind::Resumed => "resumed",
            SystemEventKind::TimeChanged => "time_changed",
        }
    }
}

/// 一次 OS 通知：类别 + **事件时刻**的时钟样本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemEvent {
    pub kind: SystemEventKind,
    /// 事件到达那一刻的样本（与协调器**同源**的时钟）。
    ///
    /// `None` = 那一刻取不到时钟（[`Clock::sample`] 失败）⇒ 调用方必须按「边界未知」
    /// 处理。**不要**在这里补一个「差不多的时间」：平台边界的全部价值就是它精确。
    pub boundary: Option<ClockSample>,
}

/// 事件源适配接口（**可注入**：测试与将来别的平台都实现它）。
pub trait SystemEventSource {
    /// 诊断名。
    fn name(&self) -> &'static str;

    /// 注册监听。**在事件线程上调用**。
    ///
    /// `Err` = 本平台不支持 / 注册失败：调用方记诊断即可，**不得**返回 `Ok` 假装成功。
    fn start(&mut self) -> io::Result<()>;

    /// 已注册之后跑消息循环。`alive` 置假 ⇒ 尽快返回。
    ///
    /// 每个事件的边界样本**在这一层取**：只有这里知道「事件刚刚到达」。
    fn run(&mut self, alive: &AtomicBool, emit: &mut dyn FnMut(SystemEvent)) -> io::Result<()>;
}

/// 本平台没有事件源时的实现：明确报「不支持」。
pub struct UnsupportedSource {
    platform: &'static str,
}

impl SystemEventSource for UnsupportedSource {
    fn name(&self) -> &'static str {
        "unsupported"
    }

    fn start(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("no system event source for {}", self.platform),
        ))
    }

    fn run(&mut self, _alive: &AtomicBool, _emit: &mut dyn FnMut(SystemEvent)) -> io::Result<()> {
        Ok(())
    }
}

/// 生产事件源（本平台实现）。`clock` 必须与协调器**同源**（见 [`spawn`] 的说明）。
pub fn os_source(clock: SystemClock) -> Box<dyn SystemEventSource> {
    #[cfg(windows)]
    {
        Box::new(windows_source::WindowsSource::new(clock))
    }
    #[cfg(not(windows))]
    {
        let _ = clock;
        Box::new(UnsupportedSource {
            platform: std::env::consts::OS,
        })
    }
}

/// 起一条事件线程订阅本平台的系统通知（组合根的生产入口）。
///
/// `clock` **必须**克隆自交给协调器的那一个：边界样本的 `monotonic_ms` 与协调器同源，
/// `system_pause` 的边界校验才可能通过。拿另一个 `SystemClock::new()` 会得到一个不同的
/// 原点，现象是「每次锁屏都掉进 recovering」——**静默降级**，所以这条写在签名旁边。
///
/// 返回 `Err` = 注册失败（或线程起不来）：**不 panic、不假装成功**，调用方只记诊断。
pub fn spawn<F>(clock: SystemClock, alive: Arc<AtomicBool>, on_event: F) -> io::Result<()>
where
    F: FnMut(SystemEvent) + Send + 'static,
{
    spawn_watched(move || os_source(clock), alive, on_event, || {})
}

/// 与 [`spawn`] 相同，外加两件可注入的东西（生产与用例都走这一条）：
///
/// - `make_source`：事件源的**工厂**。它在事件线程上被调用——平台对象（窗口句柄之类）
///   只在创建它们的线程上有效，实例因此从不跨线程移动；用例注入自己的脚本源也走这里。
/// - `on_unexpected_exit`：事件线程**在没有停止信号的情况下**结束时的报告出口
///   （`run` 返回 `Err`，或回调 panic 展开；与 `Scheduler::spawn_watched` 同一手法）。
///   正常停止**不会**调用它，否则每次退出都会留一条假故障。
pub fn spawn_watched<S, F, D>(
    make_source: S,
    alive: Arc<AtomicBool>,
    mut on_event: F,
    on_unexpected_exit: D,
) -> io::Result<()>
where
    S: FnOnce() -> Box<dyn SystemEventSource> + Send + 'static,
    F: FnMut(SystemEvent) + Send + 'static,
    D: FnOnce() + Send + 'static,
{
    // 注册结果**同步**交回调用方：`Err` 原样透出（含 `ErrorKind` 与原因），
    // 让上层既能记诊断、也能按种类分辨「平台不支持」与「注册被拒」。
    let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();

    let spawned = thread::Builder::new()
        .name(EVENT_THREAD_NAME.to_string())
        .spawn(move || {
            let mut source = make_source();
            let started = source.start();
            let failed = started.is_err();
            let _ = ready_tx.send(started);
            if failed {
                // 注册失败：干净退出。**不**触发看门狗——这不是「意外结束」，
                // 是「一开始就没起来」，由调用方按注册失败记诊断。
                return;
            }

            // 看门狗：走到析构时 `alive` 仍为真 ⇒ 线程不是被叫停的。
            let guard = UnexpectedExitOnDrop {
                alive: Arc::clone(&alive),
                report: Some(on_unexpected_exit),
            };
            // 返回值只用于「有没有正常跑完」：失败由看门狗报告，这里不再重复记一次。
            let _ = source.run(&alive, &mut on_event);
            drop(guard);
            // 平台对象在**创建它们的线程**上释放（`Drop` 里注销通知、销毁窗口）。
            drop(source);
        });

    if let Err(error) = spawned {
        return Err(io::Error::new(
            error.kind(),
            format!("spawn system event thread: {error}"),
        ));
    }

    match ready_rx.recv() {
        Ok(result) => result,
        // 线程在报告注册结果之前就没了（例如工厂 panic）：不假装成功。
        Err(_) => Err(io::Error::other(
            "system event thread ended before reporting its registration",
        )),
    }
}

/// 事件线程**意外结束**的报告守卫。
///
/// 判据是 `alive`：为真说明「本该还在监听」却退出了。回调**不得 panic**——
/// 它在展开过程中执行（再 panic 一次就是 abort）。
struct UnexpectedExitOnDrop<D: FnOnce()> {
    alive: Arc<AtomicBool>,
    report: Option<D>,
}

impl<D: FnOnce()> Drop for UnexpectedExitOnDrop<D> {
    fn drop(&mut self) {
        if !self.alive.load(Ordering::SeqCst) {
            return;
        }
        if let Some(report) = self.report.take() {
            report();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Windows 实现：一个不可见的顶层消息窗 + 三类系统通知
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod windows_source {
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        MsgWaitForMultipleObjectsEx, PeekMessageW, RegisterClassW, TranslateMessage,
        UnregisterClassW, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
        PM_REMOVE, QS_ALLINPUT, WM_POWERBROADCAST, WM_TIMECHANGE, WM_WTSSESSION_CHANGE, WNDCLASSW,
        WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    use super::{SystemEvent, SystemEventKind, SystemEventSource, MESSAGE_WAIT_MS};
    use crate::platform::clock::{Clock, SystemClock};

    /// 窗口类名的序号：同一进程内多次注册（用例 + 生产）不能撞名。
    static CLASS_SEQ: AtomicU32 = AtomicU32::new(0);

    thread_local! {
        /// 窗口过程只**入队**：它可能在我们等消息时被系统直接调用，那里不取锁、不碰业务。
        static PENDING: RefCell<Vec<SystemEvent>> = const { RefCell::new(Vec::new()) };
        /// 事件时刻的时钟（与协调器同源的那一份）。窗口过程用它取边界样本。
        static CLOCK: RefCell<Option<SystemClock>> = const { RefCell::new(None) };
    }

    /// 把 `&str` 编成 Windows 要的 UTF-16、NUL 结尾。
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 消息 → 事件类别。**纯函数**（用例直接断言它）。
    fn classify(msg: u32, wparam: WPARAM) -> Option<SystemEventKind> {
        match msg {
            // 系统把这条**发送**给每个顶层窗口（不是投递）。
            WM_POWERBROADCAST => match wparam as u32 {
                PBT_APMSUSPEND => Some(SystemEventKind::Suspending),
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => Some(SystemEventKind::Resumed),
                _ => None,
            },
            // 由 `WTSRegisterSessionNotification` 送来。
            WM_WTSSESSION_CHANGE => match wparam as u32 {
                WTS_SESSION_LOCK => Some(SystemEventKind::Locked),
                WTS_SESSION_UNLOCK => Some(SystemEventKind::Unlocked),
                _ => None,
            },
            // 广播给所有顶层窗口。
            WM_TIMECHANGE => Some(SystemEventKind::TimeChanged),
            _ => None,
        }
    }

    /// 电源通知要回 `TRUE`：告诉电源管理器「本程序已经准备好挂起 / 已经处理了唤醒」。
    fn power_ack(msg: u32, wparam: WPARAM) -> bool {
        msg == WM_POWERBROADCAST
            && matches!(
                wparam as u32,
                PBT_APMSUSPEND | PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND
            )
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if let Some(kind) = classify(msg, wparam) {
            // 边界样本在**入队时**取：越贴近事件时刻越好（循环下一轮就要把它交出去）。
            let boundary = CLOCK.with(|clock| match clock.borrow().as_ref() {
                Some(clock) => clock.sample().ok(),
                None => None,
            });
            PENDING.with(|queue| queue.borrow_mut().push(SystemEvent { kind, boundary }));
            if power_ack(msg, wparam) {
                return 1;
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    /// 本平台的实现：一个从不显示的顶层窗口 + 它自己的线程。
    pub struct WindowsSource {
        clock: SystemClock,
        /// 窗口句柄：`start` 成功之后才有。
        hwnd: Option<HWND>,
        /// 窗口类名（注销时要用同一份内存）。
        class: Option<Vec<u16>>,
    }

    impl WindowsSource {
        pub fn new(clock: SystemClock) -> Self {
            Self {
                clock,
                hwnd: None,
                class: None,
            }
        }

        /// 建窗并注册会话通知。**必须在将跑消息循环的那条线程上调用**。
        pub fn start(&mut self) -> std::io::Result<()> {
            let class = wide(&format!(
                "worktrace.system_events.{}.{}",
                std::process::id(),
                CLASS_SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            let title = wide("worktrace system events (invisible)");

            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: Some(window_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: instance,
                    hIcon: std::ptr::null_mut(),
                    hCursor: std::ptr::null_mut(),
                    hbrBackground: std::ptr::null_mut(),
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class.as_ptr(),
                };
                if RegisterClassW(&wc) == 0 {
                    return Err(std::io::Error::last_os_error());
                }

                // 顶层、但**从不显示**（不给 WS_VISIBLE）：消息专用窗口收不到广播，
                // 所以这里必须是一个真的顶层窗口。
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    title.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    let error = std::io::Error::last_os_error();
                    UnregisterClassW(class.as_ptr(), instance);
                    return Err(error);
                }

                if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) == 0 {
                    let error = std::io::Error::last_os_error();
                    DestroyWindow(hwnd);
                    UnregisterClassW(class.as_ptr(), instance);
                    return Err(error);
                }

                self.hwnd = Some(hwnd);
                self.class = Some(class);
                // 时钟在这里就装进本线程：窗口过程从**消息一到**就要能取边界样本，
                // 而第一条消息可能早于 `run` 的循环（系统是主动发的，不排队等我们）。
                CLOCK.with(|clock| *clock.borrow_mut() = Some(self.clock.clone()));
                Ok(())
            }
        }

        /// 把所有已入队的事件交给 `emit`。
        fn drain(emit: &mut dyn FnMut(SystemEvent)) {
            let mut events = PENDING.with(|queue| std::mem::take(&mut *queue.borrow_mut()));
            for event in events.drain(..) {
                emit(event);
            }
        }

        /// 窗口句柄（用例投消息用；`start` 之前是 `None`）。
        #[cfg(test)]
        fn window(&self) -> Option<HWND> {
            self.hwnd
        }
    }

    impl SystemEventSource for WindowsSource {
        fn name(&self) -> &'static str {
            "windows"
        }

        fn start(&mut self) -> std::io::Result<()> {
            WindowsSource::start(self)
        }

        fn run(
            &mut self,
            alive: &AtomicBool,
            emit: &mut dyn FnMut(SystemEvent),
        ) -> std::io::Result<()> {
            // `start` 必须先成功：没有窗口就没有消息循环可跑。
            self.hwnd.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "system event window was never created",
                )
            })?;

            while alive.load(Ordering::SeqCst) {
                unsafe {
                    let mut message: MSG = std::mem::zeroed();
                    // 先把队列里的投递消息清空（`WM_TIMECHANGE` 这类走队列）。
                    while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    // 然后睡到「有消息」或超时：**发送**过来的 `WM_POWERBROADCAST` /
                    // `WM_WTSSESSION_CHANGE` 就是在这类等待里被派发到窗口过程的。
                    MsgWaitForMultipleObjectsEx(
                        0,
                        std::ptr::null(),
                        MESSAGE_WAIT_MS,
                        QS_ALLINPUT,
                        0,
                    );
                }
                Self::drain(emit);
            }

            // 停止信号之后还有最后一批已经入队的事件：交出去，别丢。
            Self::drain(emit);
            CLOCK.with(|clock| *clock.borrow_mut() = None);
            PENDING.with(|queue| queue.borrow_mut().clear());
            Ok(())
        }
    }

    impl Drop for WindowsSource {
        fn drop(&mut self) {
            CLOCK.with(|clock| *clock.borrow_mut() = None);
            let (Some(hwnd), Some(class)) = (self.hwnd, self.class.as_ref()) else {
                return;
            };
            unsafe {
                WTSUnRegisterSessionNotification(hwnd);
                DestroyWindow(hwnd);
                UnregisterClassW(class.as_ptr(), GetModuleHandleW(std::ptr::null()));
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::mpsc;
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, SendMessageW};

        /// 消息 → 类别：三类通知各一条，外加「无关消息不入队」。
        #[test]
        fn classify_maps_the_three_notification_families() {
            assert_eq!(
                classify(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as WPARAM),
                Some(SystemEventKind::Locked)
            );
            assert_eq!(
                classify(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as WPARAM),
                Some(SystemEventKind::Unlocked)
            );
            assert_eq!(
                classify(WM_POWERBROADCAST, PBT_APMSUSPEND as WPARAM),
                Some(SystemEventKind::Suspending)
            );
            assert_eq!(
                classify(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as WPARAM),
                Some(SystemEventKind::Resumed)
            );
            assert_eq!(
                classify(WM_TIMECHANGE, 0),
                Some(SystemEventKind::TimeChanged)
            );
            assert_eq!(classify(WM_WTSSESSION_CHANGE, 0xFFFF), None);
            assert_eq!(classify(0x0123, 0), None);
        }

        /// **真窗口往返**：往自己的隐藏窗投一条 `WM_TIMECHANGE`（走队列）与发送
        /// `WM_POWERBROADCAST`/`WM_WTSSESSION_CHANGE`（走窗口过程），断言它们经
        /// 「消息循环 → 分类 → 取边界样本 → emit」出来。
        ///
        /// 它**不依赖系统真的锁屏或休眠**（那是 P8 的实机项），但确实经过真窗口、
        /// 真消息队列与真的 `WTSRegisterSessionNotification` 注册。
        #[test]
        fn the_hidden_window_turns_system_messages_into_events() {
            let (tx, rx) = mpsc::channel();
            let alive = Arc::new(AtomicBool::new(true));
            let stop = Arc::clone(&alive);

            let runner = std::thread::spawn(move || {
                let mut source = WindowsSource::new(SystemClock::new());
                source.start().expect("注册消息窗与会话通知");
                let hwnd = source.window().expect("窗口已建");
                unsafe {
                    PostMessageW(hwnd, WM_TIMECHANGE, 0, 0);
                    SendMessageW(hwnd, WM_POWERBROADCAST, PBT_APMSUSPEND as WPARAM, 0);
                    SendMessageW(hwnd, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as WPARAM, 0);
                    SendMessageW(hwnd, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as WPARAM, 0);
                }
                source
                    .run(&stop, &mut |event| {
                        let _ = tx.send(event);
                    })
                    .expect("消息循环");
            });

            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            while seen.len() < 4 {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(event) => seen.push(event),
                    Err(error) => assert!(
                        Instant::now() < deadline,
                        "只收到 {} 条事件：{error}",
                        seen.len()
                    ),
                }
            }

            alive.store(false, Ordering::SeqCst);
            runner.join().expect("事件线程退出");

            let kinds: Vec<SystemEventKind> = seen.iter().map(|event| event.kind).collect();
            for kind in [
                SystemEventKind::TimeChanged,
                SystemEventKind::Suspending,
                SystemEventKind::Locked,
                SystemEventKind::Unlocked,
            ] {
                assert!(kinds.contains(&kind), "缺 {kind:?}：{kinds:?}");
            }
            for event in &seen {
                let boundary = event.boundary.expect("事件必须自带边界样本");
                assert!(boundary.wall_ms > 0, "样本来自真时钟：{boundary:?}");
            }
        }
    }
}

/// [`spawn`] 的对外行为：**本平台能注册就注册成功，不能注册就同步报出原因**。
///
/// 为什么单列一条：它是 C-C 点名的生产入口签名（`spawn(clock, alive, on_event)`），
/// 生产接线走 [`spawn_watched`]（多一个「意外结束」的报告出口），所以这条签名只有这里
/// 真的调用它——不钉住它就成了「零调用者」的公开 API。断言只覆盖「注册这半边」；
/// **锁屏/休眠/改时的到达延迟与行为**是 P8 的实机项（`pre-p6-closure.md` 第 10 条），
/// 本用例不声称那半边。
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_production_entry_registers_or_reports_why_not() {
        let alive = Arc::new(AtomicBool::new(true));
        let spawned = spawn(SystemClock::new(), Arc::clone(&alive), |_event| {});

        match spawned {
            Ok(()) => {}
            Err(error) => assert_eq!(
                error.kind(),
                io::ErrorKind::Unsupported,
                "本平台要么注册成功，要么明确报「不支持」——不得假装成功：{error:?}"
            ),
        }

        // 停止信号之后线程自己收尾（没有任何路径 join 它）。
        alive.store(false, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(MESSAGE_WAIT_MS as u64 + 50));
    }
}
