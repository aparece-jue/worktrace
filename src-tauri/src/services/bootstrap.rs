//! **唯一**启动入口（P7 Task 0）。
//!
//! ## 固定顺序（01 §2 + 02 §4，不得调整）
//!
//! ① 单实例检查 → ② 打开库并迁移（含新库的库身份）→ ③ 新建 `application_run`
//! → ④ 恢复扫描（P3 的四类判定 + 落地事实，随后算门禁快照）→ ⑤ 启动协调器与周期采样驱动
//! → ⑥ 开窗口。
//!
//! 每一步都通过 [`StartupProbe`] 报出去，`tests/startup_order.rs` 断言的是**调用次序**
//! 本身，不是「没崩」。`lib.rs` 不得自己重排这段顺序：它只调用本入口
//! （Task 1 接线），窗口在⑥才开——后端就绪之前窗口不该开始拉数据。
//!
//! ## 单实例在前，且失败即退出
//!
//! 拿不到锁的进程**通知既有实例后退出**：不打开库、不迁移、不建 run、不启动计时
//! （F-016）。返回 [`Startup::AlreadyRunning`]，调用方必须直接退出。
//!
//! ## 串行执行边界（D6 裁决，写死在 `AppState` 上）
//!
//! **单一 `Mutex<AppState{db, coordinator}>`**（不是专用工作线程 + channel）。
//! 选型理由与 P1 的实测对照写在 `src-tauri/IMPLEMENTATION-NOTES.md` §4。
//! 与选型无关的三条硬约束，接线时必须遵守：
//!
//! 1. 命令一律 `async`；
//! 2. **不得在 UI 回调里跑长事务**——命令体在
//!    `tauri::async_runtime::spawn_blocking` 里取锁执行（Task 1）；
//! 3. **`Connection` 不得跨 `await` 持有**——锁与事务都活在那个阻塞闭包内。
//!
//! 周期采样驱动与用户命令走**同一把锁**：这就是「同一条串行边界」的具体含义，
//! 也是「广播按提交顺序」的依据（提交与广播在同一临界区内完成）。
//!
//! ## 本阶段不做
//!
//! 恢复确认与历史修正的用户命令、门禁重扫（P3 Task 2 起）；启动故障路径硬化与锁异常
//! 释放（P6 Task 1）；维护态隔离（P6 Task 2/4）；备份（P6 Task 4）；平台事件的实机
//! 验收（P8 复核）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;

use rusqlite::Connection;

use crate::domain::session::SessionState;
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::platform::clock::Clock;
use crate::platform::paths;
use crate::platform::scheduler::Scheduler;
use crate::platform::single_instance::{self, InstanceLock};
use crate::services::events::{Broadcaster, EventEnvelope, EventSink};
use crate::services::recovery::{ReconcileReport, ReconcileRequest};
use crate::services::timer::coordinator::{
    CommandOutcome, Coordinator, ResumeRequest, SessionRequest, StartRequest,
};
use crate::services::timer::primitives::{end_session_in_tx, EndSessionFacts};
use crate::services::timer::snapshot::TimerSnapshot;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::meta;
use crate::storage::migrations::migrate;
use crate::storage::run_repo;
use crate::storage::session_repo::{self, InvariantFault};
use crate::storage::WriteOutcome;

/// 周期采样一拍的默认间隔（毫秒）。
///
/// 1 秒对应展示刷新；它**不是**心跳间隔——检查点仍由协调器的
/// `HEARTBEAT_INTERVAL_MS`（30 秒）决定，采样每一拍都会问一次「到期没有」。
///
/// 是整数毫秒而不是 `Duration`：分层门禁禁止 `services` 出现 `std::time`
/// （那条规则的用意是「服务层不得自取时间」），而节拍本来就不该由服务层
/// 表达成一个时刻类型——`platform::scheduler` 收毫秒。
pub const DEFAULT_SAMPLING_INTERVAL_MS: u64 = 1_000;

// ─────────────────────────────────────────────────────────────────────────────
// 启动次序的探针
// ─────────────────────────────────────────────────────────────────────────────

/// 启动过程中的一个**副作用步骤**。
///
/// 计划里的六步在这里落成九条记录：①拆成「已检查单实例」与「已有实例：已发唤起请求」
/// 两条（后者是拿锁失败路径，之后必须退出），②拆成「打开库」与「迁移（含库身份）」，
/// ⑤拆成「协调器」与「采样驱动」，③④⑥ 各一条。拆开只是让断言能定位到具体一步，
/// 顺序本身与计划逐条一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupStep {
    /// ① 单实例检查（必须先于持久化初始化）。
    SingleInstanceChecked,
    /// 已有实例：已发出「唤起主窗」请求（拿锁失败路径，之后必须退出）。
    ExistingInstanceNotified,
    /// ② 打开库。
    DatabaseOpened,
    /// ② 迁移到当前 schema；新库在这里取得 `data_epoch`。
    Migrated,
    /// ③ 新建 `application_run`（每次成功启动一行）。
    RunCreated,
    /// ④ 恢复扫描。
    RecoveryScanned,
    /// ⑤ 协调器就绪。
    CoordinatorStarted,
    /// ⑤ 周期采样驱动已起。
    SamplingStarted,
    /// ⑥ 开窗口（最后一步）。
    WindowOpened,
}

impl StartupStep {
    pub const ALL: [StartupStep; 9] = [
        Self::SingleInstanceChecked,
        Self::ExistingInstanceNotified,
        Self::DatabaseOpened,
        Self::Migrated,
        Self::RunCreated,
        Self::RecoveryScanned,
        Self::CoordinatorStarted,
        Self::SamplingStarted,
        Self::WindowOpened,
    ];

    /// 诊断用的稳定名字。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingleInstanceChecked => "single_instance_checked",
            Self::ExistingInstanceNotified => "existing_instance_notified",
            Self::DatabaseOpened => "database_opened",
            Self::Migrated => "migrated",
            Self::RunCreated => "run_created",
            Self::RecoveryScanned => "recovery_scanned",
            Self::CoordinatorStarted => "coordinator_started",
            Self::SamplingStarted => "sampling_started",
            Self::WindowOpened => "window_opened",
        }
    }
}

/// 启动次序的观察者。生产侧接诊断日志，测试侧记录次序。
pub trait StartupProbe: Send + Sync {
    fn step(&self, step: StartupStep);
}

/// 什么都不记的探针。生产接线在 Task 1 换成诊断日志。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProbe;

impl StartupProbe for NoProbe {
    fn step(&self, _step: StartupStep) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// 配置与返回值
// ─────────────────────────────────────────────────────────────────────────────

/// 启动需要的路径与节拍。
#[derive(Debug, Clone)]
pub struct StartupConfig {
    pub db_path: PathBuf,
    pub lock_path: PathBuf,
    /// 周期采样节拍，毫秒。
    pub sampling_interval_ms: u64,
}

impl StartupConfig {
    /// 用平台路径构造。库与锁同目录（`platform::paths` 已保证）。
    pub fn from_app_paths() -> Result<Self, AppError> {
        Ok(Self {
            db_path: paths::database_file().map_err(|e| io_err("db path", e))?,
            lock_path: paths::instance_lock_file().map_err(|e| io_err("lock path", e))?,
            sampling_interval_ms: DEFAULT_SAMPLING_INTERVAL_MS,
        })
    }

    /// 显式路径（测试与打包路径实验用）。
    pub fn new(db_path: impl Into<PathBuf>, lock_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            lock_path: lock_path.into(),
            sampling_interval_ms: DEFAULT_SAMPLING_INTERVAL_MS,
        }
    }
}

/// 启动结果。
///
/// 不派生 `Debug`：`RunningApp` 持有采样线程与库连接，没有有意义的调试表示，
/// 为它实现 `Debug` 只会诱导出「把句柄打进日志」这种用法。
pub enum Startup {
    /// 本进程成为唯一实例，核心已在跑。
    Running(Box<RunningApp>),
    /// 已有实例持有锁：**调用方必须直接退出**（不得开窗口、不得碰库）。
    AlreadyRunning {
        /// 「唤起既有主窗」的请求是否发出。**失败也照样退出**。
        notified: bool,
    },
}

/// 一次成功启动之后核心持有的东西。
pub struct RunningApp {
    app: SharedApp,
    broadcaster: Arc<Broadcaster>,
    sampling: Scheduler,
    /// 持有到进程退出：**Drop 即释放**（被强杀时由内核释放）。
    _lock: InstanceLock,
    run_id: String,
    data_epoch: String,
    recovery: RecoveryScan,
    sampling_errors: Arc<AtomicU64>,
}

impl RunningApp {
    /// 共享的 `AppState`：命令（Task 1）与托盘（Task 4）都从这里取。
    pub fn app(&self) -> &SharedApp {
        &self.app
    }

    pub fn broadcaster(&self) -> &Arc<Broadcaster> {
        &self.broadcaster
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn data_epoch(&self) -> &str {
        &self.data_epoch
    }

    /// 启动扫描之后的恢复门禁快照（`requires_recovery()` 就是 `start`/`resume` 的判据）。
    pub fn recovery(&self) -> &RecoveryScan {
        &self.recovery
    }

    /// 采样线程已完成的触发次数（诊断与测试）。
    pub fn sampling_ticks(&self) -> u64 {
        self.sampling.ticks()
    }

    /// 采样出错的累计次数。**只记诊断**：采样失败不该把进程打死。
    pub fn sampling_errors(&self) -> u64 {
        self.sampling_errors.load(Ordering::SeqCst)
    }

    /// **显式退出**（Task 4 的托盘「退出」与 P8 复用这一条入口）。
    ///
    /// 顺序：**先停定时器**（此后不会再有新的一拍进事务），再在**一个事务**里
    /// 结束 `running`/`paused` 会话、写 `clean_exit_at`、按需推进 revision。
    /// `recovering` 记录原样保留（02 §4）。
    ///
    /// 退出是**终态**：调用方随后应当退出进程，不得再对本实例下命令。
    ///
    /// 取 `&self` 而不是 `&mut self`（2026-10-04 Task 4）：托盘的「退出」在组合根里
    /// 只拿得到共享引用（`AppHandle::state::<RunningApp>()` 给的正是 `&RunningApp`），
    /// 而要它走**这一条**入口就不能另开一条 `&mut` 通道——那等于把退出拆成两份实现。
    /// 内部可变性收在 `Scheduler`（停止位 + `Mutex<Option<JoinHandle>>`）与 `AppState`
    /// 那把锁里，本类型自身仍然没有可被外部摆布的状态。
    ///
    /// **持锁调用会被拒**（fix round 1，评审 I1）：`&self` 让「先取锁再调退出」也能编译，
    /// 而那条路必然与采样线程互锁。防线是 [`holds_app_lock`]，失败是
    /// `STORAGE_ERROR`（接线缺陷，不是用户操作错误）：**拒绝时不碰任何东西**——
    /// 采样没停、没有事务、没有半退出状态。
    pub fn shutdown(&self) -> Result<ExitReport, AppError> {
        // **自死锁防线**（P7 Task 4 fix round 1，评审 I1）：退出要先 `join` 采样线程，
        // 而采样线程每一拍都要取这把锁。调用者自己正持着锁时，那次 join 永远等不到
        // 头——进程会静默卡死（放宽到 `&self` 之后，这个环从「编译错误」变成了
        // 「可以写出来的代码」，所以必须在这里拦住）。这是**接线缺陷**，不是用户错误：
        // 明确失败，并说清正确姿势。
        if holds_app_lock(&self.app) {
            return Err(AppError::Storage {
                detail: "显式退出不能在持有串行边界的线程上调用（会与采样线程互锁）：请在锁外调用"
                    .to_string(),
            });
        }

        self.sampling.stop();

        let at = {
            let mut state = lock_app(&self.app);
            // 退出时刻必须来自协调器的时钟（生产是 SystemClock，测试是 FakeClock）：
            // 这不仅是为了可测，也是为了让「退出」与「归属」用同一个时间来源。
            let AppState {
                db, coordinator, ..
            } = &mut *state;
            coordinator.snapshot(db)?.as_of
        };

        lock_app(&self.app).explicit_exit(at)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 恢复门禁（启动第 ④ 步之后的门禁快照）
// ─────────────────────────────────────────────────────────────────────────────

/// 启动扫描之后的门禁快照：**不是本次 run** 的会话里还有没有「没结束 / 待确认 /
/// 事实损坏」。
///
/// 三件事任一命中 ⇒ 拒绝业务计时（`start`/`resume` 拿 `RECOVERY_REQUIRED`），
/// **不自动修复、不忽略历史**。四类判定与能安全归一的动作已经由
/// [`crate::services::recovery::scan_at_startup`] 在第 ④ 步做完；快照是在那之后
/// 重新查一遍事实得到的门禁依据（第 4 类重绑过的会话不再算「别的 run 的残留」）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryScan {
    /// 未结束（`running`/`paused`/`recovering`）的会话 id。
    pub unfinished_sessions: Vec<String>,
    /// 未作废的待确认区间 id。
    pub pending_intervals: Vec<String>,
    /// 事实已不满足不变量的会话。
    pub invariant_faults: Vec<InvariantFault>,
}

impl RecoveryScan {
    /// 是否要求先完成恢复。
    pub fn requires_recovery(&self) -> bool {
        !self.unfinished_sessions.is_empty()
            || !self.pending_intervals.is_empty()
            || !self.invariant_faults.is_empty()
    }
}

/// 按 `run_id <> 当前 run` 查那三件事。
pub fn scan_recovery(conn: &Connection, current_run_id: &str) -> Result<RecoveryScan, AppError> {
    Ok(RecoveryScan {
        unfinished_sessions: session_repo::unfinished_sessions_of_other_runs(conn, current_run_id)?
            .into_iter()
            .map(|s| s.id)
            .collect(),
        pending_intervals: session_repo::pending_intervals_of_other_runs(conn, current_run_id)?
            .into_iter()
            .map(|i| i.id)
            .collect(),
        invariant_faults: session_repo::invariant_faults_of_other_runs(conn, current_run_id)?,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// 串行边界：AppState
// ─────────────────────────────────────────────────────────────────────────────

/// 进程内**唯一**的数据库句柄与计时协调器。
///
/// 一次只允许一条路径进入（外部那层 `Mutex`），所以「用户命令」与「周期采样」
/// 天然串行——这就是 D6 选定的边界。命令体必须在阻塞线程上取锁执行，见模块头。
///
/// **字段私有**：拿到锁不等于拿到「可以随便动的协调器」。可变访问一律走本类型的
/// 方法（`start`/`resume`/`snapshot`/`tick`/`sample_tick`/`explicit_exit`）——
/// 其中 `start`/`resume` 挂着恢复门禁（第 8 条）。若把 `&mut Coordinator` 递出去，
/// 「不得忽略历史」就只剩纪律；Task 1 的命令层就在下一轮，这道口子不能留。
pub struct AppState {
    db: Db,
    coordinator: Coordinator,
    recovery: RecoveryScan,
    /// 上一次恢复扫描**失败**的标记（P3 S1）。启动成功时一定是 `false`。
    ///
    /// 为什么必须是独立的一位：[`AppState::recovery`] 是「上一次**成功**扫描的结论」。
    /// 扫描查询失败时那份结论不再代表今天的事实，直接拿它放行就是把「不知道有没有
    /// 未处理的恢复事实」当成「没有」。所以失败时置真、**原样保留**旧快照，
    /// 由 [`AppState::guard_business_timing`] 先看这一位再走原有门禁。
    recovery_scan_failed: bool,
}

/// 命令、托盘与采样共用的句柄。
///
/// 里面除了那份 `AppState`，还记着**当前持锁线程**——见 [`AppBoundary`] 与
/// [`AppGuard`] 的说明。
pub type SharedApp = Arc<AppBoundary>;

/// 串行边界：唯一那份 [`AppState`] + **当前持锁线程**。
///
/// 为什么要多记一个线程 id（P7 Task 4 fix round 1，评审 I1）：显式退出会先 `join`
/// 采样线程，而采样线程**每一拍都要取这把锁**。「调用者自己正持着锁」时那次 `join`
/// 必然互锁——采样线程等锁、调用者等采样线程，进程静默卡死。
/// 记下持锁线程，「持锁调用退出」就能在进入死锁之前被明确拒绝
/// （[`RunningApp::shutdown`]）。这个 id 只用来回答一个问题：
/// **本线程是不是正持着这把锁**——它不是第二把业务锁，也不参与任何串行决策。
pub struct AppBoundary {
    state: Mutex<AppState>,
    holder: Mutex<Option<ThreadId>>,
}

/// 串行边界的 guard：持锁期间登记持锁线程，`Drop` 清位。
///
/// `Deref`/`DerefMut` 到 [`AppState`]，所以既有调用点（`lock_app(&app).db()`、
/// `body(&mut guard)` 等）不需要改写法。
pub struct AppGuard<'a> {
    state: MutexGuard<'a, AppState>,
    boundary: &'a AppBoundary,
}

impl std::ops::Deref for AppGuard<'_> {
    type Target = AppState;

    fn deref(&self) -> &AppState {
        &self.state
    }
}

impl std::ops::DerefMut for AppGuard<'_> {
    fn deref_mut(&mut self) -> &mut AppState {
        &mut self.state
    }
}

impl Drop for AppGuard<'_> {
    fn drop(&mut self) {
        *lock_holder(self.boundary) = None;
    }
}

/// 取锁。**中毒不 panic**：持锁线程 panic 时事务已经回滚，继续用剩下的状态
/// 比让整个进程崩掉更合理（P6 的诊断接管之前）。
///
/// 拿到的 [`AppGuard`] 会登记「本线程正持锁」，直到它被丢弃。
pub fn lock_app(shared: &SharedApp) -> AppGuard<'_> {
    let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    *lock_holder(shared) = Some(std::thread::current().id());
    AppGuard {
        state,
        boundary: shared,
    }
}

/// **本线程**是否正持着这把锁。
///
/// 唯一的生产用途是 [`RunningApp::shutdown`] 的自死锁防线；用例也用它钉住那条防线
/// （`tests/shell_lifecycle.rs`）。**不要**拿它当业务分支：它描述的是调用姿势，不是状态。
pub fn holds_app_lock(shared: &SharedApp) -> bool {
    *lock_holder(shared) == Some(std::thread::current().id())
}

/// 取持锁线程登记（中毒同样不 panic，理由与 [`lock_app`] 相同）。
fn lock_holder(boundary: &AppBoundary) -> MutexGuard<'_, Option<ThreadId>> {
    boundary
        .holder
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

impl AppState {
    /// 只读借用数据库句柄：诊断与只读校验（例如在 App **自己那条连接**上取
    /// `total_changes()`）。写入一律走服务方法。
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// 只读借用协调器：展示运行态（`run_id`/`tick_seq`/`live`/故障标记）。
    /// 需要可变入口时用本类型的命令方法，别在这里开第二条路。
    pub fn coordinator(&self) -> &Coordinator {
        &self.coordinator
    }

    /// 取一次墙钟毫秒：写命令的 `created_at` / `updated_at` 与审计行用它。
    ///
    /// 为什么要有这个入口：服务层**不得自取时间**（分层门禁禁 `std::time`），
    /// 而时间只该从协调器这一条时钟接缝出来（生产 `SystemClock`、测试 `FakeClock`）。
    /// 命令层若自己 `SystemClock::new()`，同一个进程里就有两个时间源。
    ///
    /// 采样**用完即弃**：它不喂给锚点/检测器，也不推进任何采样状态。
    pub fn now_ms(&self) -> Result<i64, AppError> {
        self.coordinator.wall_ms()
    }

    /// 写服务要的可变库句柄（`services::catalog` / `services::daily_plan` 的写入口）。
    ///
    /// **不绕过任何门禁**：恢复门禁挂在 [`AppState::start`] / [`AppState::resume`] 上，
    /// 而协调器仍然只有只读访问器 [`AppState::coordinator`]——拿到 `&mut Db` 也拿不到
    /// 可以随便动的计时状态（Task 0 收口时私有化字段，堵的是 `&mut Coordinator`，
    /// 不是业务写本身）。
    pub fn db_mut(&mut self) -> &mut Db {
        &mut self.db
    }

    pub fn recovery(&self) -> &RecoveryScan {
        &self.recovery
    }

    /// 业务计时（`start`/`resume`）的门禁。
    ///
    /// **只挡「开始新计时」**：查询（`snapshot`/`tick`）照常可用——用户要能看到
    /// 「有什么在等恢复」，把他挡在界面外面不如告诉他发生了什么。恢复入口
    /// （`reconcile` / `discard_session` / `retry_recovery`）也不受它限制，
    /// 否则门禁自己就成了死锁。
    ///
    /// **先看「上次扫描是否失败」再看快照**（P3 S1）：失败的扫描没有结论，
    /// 旧快照只是历史快照，不能拿它放行。
    pub fn guard_business_timing(&self) -> Result<(), AppError> {
        if self.recovery_scan_failed {
            return Err(AppError::RecoveryRequired);
        }
        if self.recovery.requires_recovery() {
            return Err(AppError::RecoveryRequired);
        }
        Ok(())
    }

    /// 重扫恢复事实并替换门禁快照（P3 S1）。`start`/`resume` 读的就是这个字段。
    ///
    /// 服务层事务**提交之后**、仍持同一把锁时调用；返回重扫结论——
    /// `requires_recovery()` 仍为真时 `start`/`resume` 继续拒绝（判据不变），
    /// 那**不是**失败：调用方照常拿到快照。
    ///
    /// **失败也闭环**：三条只读查询失败时置 [`AppState::recovery_scan_failed`]、
    /// **不替换**旧快照、返回 [`AppError::RecoveryRequired`]——旧快照是上一次成功扫描的
    /// 结论，不能当这次的结果。扫描查询本身是幂等的，再次扫描成功就清标记；
    /// 清标记**不要求重做**任何已提交的用户命令（已提交事实保留，不重复审计、不重复加版本）。
    pub fn rescan_recovery(&mut self) -> Result<RecoveryScan, AppError> {
        let AppState {
            db,
            coordinator,
            recovery,
            recovery_scan_failed,
        } = self;
        let run_id = coordinator.run_id().to_string();
        match scan_recovery(db.connection(), &run_id) {
            Ok(scan) => {
                // 成功才替换快照并清标记：两者同一步完成，不留「新快照 + 旧标记」的中间态。
                *recovery = scan.clone();
                *recovery_scan_failed = false;
                Ok(scan)
            }
            Err(_) => {
                // 底层诊断（SQLite 原文）在这里没地方落：契约只要求按码分支，
                // 用户看到的是 RECOVERY_REQUIRED 那句话。失败的**结论**留在标记里。
                *recovery_scan_failed = true;
                Err(AppError::RecoveryRequired)
            }
        }
    }

    /// 开始计时（先过恢复门禁）。
    pub fn start(&mut self, req: StartRequest) -> Result<CommandOutcome, AppError> {
        self.guard_business_timing()?;
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.start(db, req)
    }

    /// 继续计时（先过恢复门禁）。
    pub fn resume(&mut self, req: ResumeRequest) -> Result<CommandOutcome, AppError> {
        self.guard_business_timing()?;
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.resume(db, req)
    }

    /// 暂停（**不过**恢复门禁：它不是「开始新计时」，而是把一个正在跑的会话停下来）。
    pub fn pause(&mut self, req: SessionRequest) -> Result<CommandOutcome, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.pause(db, req)
    }

    /// 结束计时（同为「停止」类，不过恢复门禁）。
    pub fn finish(&mut self, req: SessionRequest) -> Result<CommandOutcome, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.finish(db, req)
    }

    /// 查询快照（无新计时，不受恢复门禁限制）。
    pub fn snapshot(&mut self) -> Result<TimerSnapshot, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.snapshot(db)
    }

    /// 推进一步（同上）。
    pub fn tick(&mut self) -> Result<TimerSnapshot, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.tick(db)
    }

    /// **周期采样的一拍**：先问心跳（约 30 秒一次检查点），再取一次计时快照。
    ///
    /// 空闲（没有活动会话）时两步都**不写库**：心跳直接返回 `Ok(false)`，
    /// tick 走 `TimerSnapshot::idle` 分支——只有读。这是「不空转制造 revision」
    /// 的落点（00 §5：心跳、tick 不加业务 revision）。
    pub fn sample_tick(&mut self) -> Result<TimerSnapshot, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.heartbeat(db)?;
        coordinator.tick(db)
    }

    /// **显式退出**的入口（不由 `RunningApp::shutdown` 独享：Task 4 的托盘
    /// 与 P8 都复用这一条）。
    ///
    /// 一个事务里：结束 `running`/`paused` 会话（结束 running 会闭合它的开放区间）、
    /// 写 `clean_exit_at`、有业务变化时推进一次 revision。
    /// **`recovering` 记录保留**——退出不是恢复，不得顺手确认历史（02 §4）。
    /// 只处理**当前 run** 的会话：别的 run 留下的记录是恢复材料，
    /// 自动结束它们就是「自动修复」，正是门禁禁止的事。
    ///
    /// 不是杀进程；长事务不得跑在 UI 回调里——调用方在阻塞线程上执行它。
    pub fn explicit_exit(&mut self, at: i64) -> Result<ExitReport, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        let run_id = coordinator.run_id().to_string();

        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;

        let mut sessions_ended = Vec::new();
        let mut recovering_kept = Vec::new();
        for session in session_repo::unfinished_sessions_of_run(&tx, &run_id)? {
            match session.state {
                SessionState::Running | SessionState::Paused => {
                    end_session_in_tx(
                        &tx,
                        &EndSessionFacts {
                            run_id: run_id.clone(),
                            session_id: session.id.clone(),
                            expected_row_version: session.row_version,
                            attributed_end: at,
                            sampled_end_wall_at: at,
                            target_state: SessionState::Finished,
                        },
                    )?;
                    sessions_ended.push(session.id);
                }
                // 02 §4 原文：「recovering 记录保留」。
                SessionState::Recovering => recovering_kept.push(session.id),
                _ => {}
            }
        }

        let revision_changed = !sessions_ended.is_empty();
        let revision = if revision_changed {
            meta::bump_revision(&tx)?
        } else {
            meta::require_meta(&tx)?.revision
        };
        let clean_exit_recorded = run_repo::mark_clean_exit(&tx, &run_id, at)?;
        // 报告里的退出时刻以**库里那一行**为准（不是本次传入的 `at`）：
        // 重复退出不覆盖第一次的值，回一个没落库的时刻会让 Task 4 把它写进
        // 日志/UI 时与库里的记录互相矛盾。
        let clean_exit_at = run_repo::get_run(&tx, &run_id)?
            .and_then(|run| run.clean_exit_at)
            .ok_or_else(|| AppError::Storage {
                detail: "clean_exit_at missing right after a clean exit".into(),
            })?;
        tx.commit().map_err(map_sqlite)?;

        Ok(ExitReport {
            run_id,
            clean_exit_at,
            sessions_ended,
            recovering_kept,
            revision,
            revision_changed,
            clean_exit_recorded,
        })
    }

    /// 对账（确认 / 丢弃不确定区间）的命令入口（P3 S2）。`env` 的版本位是**会话**版本。
    ///
    /// 服务在自己的事务里做完校验、写入、审计与恰好一次 `revision`；**提交之后**由这里
    /// 做两件内存收尾：
    /// 1. [`AppState::rescan_recovery`]（S1）重算门禁——事实刚变，快照必须跟着变；
    /// 2. **当且仅当被改动的会话正是协调器此刻镜像的那条**时 `Coordinator::load_session`
    ///    刷新它，**不留一个继续按旧状态出快照的 `live`**（采样线程每一拍都出快照）。
    ///
    /// 第 2 步为什么带条件（计划「新增-2」的原文是「**改到协调器正镜像的会话时**」）：
    /// 无条件刷新会把 `live` 换成另一条会话。可达位移——本 run 的 A 变成 `recovering`
    /// （门禁只数**别的** run，所以它不挡计时），用户合法地 `start` 了 B；此时对账 A 会让
    /// 镜像停在 A（`paused`），而 B 的 `live_ms` 与心跳检查点从此失去内存镜像，
    /// 直到 B 自己的下一条命令把它重新载入——正在计时的会话会静默跟着错的会话。
    ///
    /// 两处失败都映射 [`AppError::RecoveryRequired`]（P2 的提交后约定）：事务已经落库，
    /// 缺的是「让内存与事实重新对上」，不是「再试一次」——重发一条对账命令会撞上
    /// 「会话已经不是 `recovering`」的前置。
    pub fn reconcile(
        &mut self,
        env: WriteEnvelope,
        req: ReconcileRequest,
    ) -> Result<WriteOutcome<ReconcileReport>, AppError> {
        let (now, run_id) = {
            let coordinator = &self.coordinator;
            (coordinator.wall_ms()?, coordinator.run_id().to_string())
        };
        let session_id = req.session_id.clone();
        let outcome = {
            let AppState { db, .. } = self;
            crate::services::recovery::reconcile(db, env, req, now, &run_id)?
        };

        // 提交之后的第一步：门禁重算（失败 ⇒ 标记挡住计时，旧快照不动）。
        self.rescan_recovery()?;

        // `live()` 是只读访问器；先问清楚「镜像的是不是这条」，再决定要不要刷新。
        let mirrored = {
            let AppState { coordinator, .. } = self;
            coordinator
                .live()
                .map(|live| live.id == session_id)
                .unwrap_or(false)
        };
        if mirrored {
            let AppState {
                db, coordinator, ..
            } = self;
            coordinator
                .load_session(db.connection(), &session_id)
                .map_err(|_| AppError::RecoveryRequired)?;
        }
        Ok(outcome)
    }
}

/// 一次显式退出的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitReport {
    pub run_id: String,
    /// **库里那一行**的 `clean_exit_at`（重复退出时是第一次写入的时刻，
    /// 不是本次传入的 `at`）。
    pub clean_exit_at: i64,
    /// 被本次退出结束的会话（原状态 `running`/`paused`）。
    pub sessions_ended: Vec<String>,
    /// **原样保留**的 `recovering` 会话。
    pub recovering_kept: Vec<String>,
    /// 退出后的权威 revision。
    pub revision: i64,
    /// 是否因为结束了会话而推进了 revision。
    pub revision_changed: bool,
    /// `clean_exit_at` 本次是否真的写入（重复退出时为 `false`）。
    pub clean_exit_recorded: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// 启动
// ─────────────────────────────────────────────────────────────────────────────

/// 唯一的启动入口。步骤与顺序见模块头。
///
/// - `clock`：协调器的时间来源（生产 `SystemClock`，测试 `FakeClock`）。
///   **采样在③之前取一次**：`application_run.started_at` 与归属基线用同一个样本。
/// - `sink`：事件广播出口（生产接 Tauri 的 emit，测试接记录器）。
/// - `probe`：启动次序观察者。
/// - `open_window`：第⑥步。真实接线里它创建/显示窗口；**失败即启动失败**
///   （采样驱动会被停掉，不留一个跑着的后台线程）。
pub fn startup(
    config: StartupConfig,
    clock: Box<dyn Clock + Send>,
    sink: Arc<dyn EventSink>,
    probe: &dyn StartupProbe,
    open_window: &dyn Fn() -> Result<(), AppError>,
) -> Result<Startup, AppError> {
    // ① 单实例检查——必须先于任何持久化初始化。
    let lock = match InstanceLock::acquire(&config.lock_path).map_err(|e| io_err("lock", e))? {
        Some(lock) => {
            probe.step(StartupStep::SingleInstanceChecked);
            lock
        }
        None => {
            probe.step(StartupStep::SingleInstanceChecked);
            // 「唤起既有主窗」是通知，与锁分离：失败也照样退出。
            let notified = single_instance::request_activation(&config.lock_path).is_ok();
            probe.step(StartupStep::ExistingInstanceNotified);
            return Ok(Startup::AlreadyRunning { notified });
        }
    };

    // ② 打开库并迁移。新库在这里取得库身份（`migrate` 不写 `app_meta`）。
    let mut db = Db::open(&config.db_path)?;
    probe.step(StartupStep::DatabaseOpened);

    migrate(db.connection())?;
    let data_epoch = match meta::read_meta(db.connection())? {
        Some(existing) => existing.data_epoch,
        None => {
            let tx = db
                .connection_mut()
                .unchecked_transaction()
                .map_err(map_sqlite)?;
            let meta = meta::init_meta(&tx)?;
            tx.commit().map_err(map_sqlite)?;
            meta.data_epoch
        }
    };
    probe.step(StartupStep::Migrated);

    // ③ 新建 application_run。时间来自可注入的时钟；采样失败即启动失败——
    //    没有可信墙钟就没法给这次 run 定起点，硬编一个值只会污染归属。
    let sample = clock.sample().map_err(|_| AppError::Storage {
        detail: "clock sample unavailable at startup".to_string(),
    })?;
    let run_id = uuid::Uuid::new_v4().to_string();
    {
        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        run_repo::start_run(&tx, &run_id, sample.wall_ms)?;
        tx.commit().map_err(map_sqlite)?;
    }
    probe.step(StartupStep::RunCreated);

    // ④ 恢复扫描。P3 的扫描服务先做四类判定并**落地事实**（第 2 类归一出可信前缀
    //    与终点未知的待确认段、第 4 类把干净的暂停会话重绑到本次 run），随后仍由
    //    [`scan_recovery`] 算门禁快照——它的语义与签名不动（`tests/startup_order.rs`
    //    直接调它），命中也仍然只挡新计时、不自动修复。
    //
    //    扫描结论（`StartupScanReport`）在这里用完即弃：启动路径只需要事实落地，
    //    恢复确认入口与全局概览由 P8 的命令去读（`attention_overview`），门禁字段
    //    则由上面的快照负责。第 2 类归一后**不重建协调器镜像**——扫描发生在第 ⑤ 步之前，
    //    `live` 还是 `None`。
    let _scan_report =
        crate::services::recovery::scan_at_startup(&mut db, &run_id, sample.wall_ms)?;
    let recovery = scan_recovery(db.connection(), &run_id)?;
    probe.step(StartupStep::RecoveryScanned);

    // ⑤ 协调器 + 周期采样驱动。基线在这里建立（08 §1 的 lifetime_ref：
    //    run 初始化就要有参照点，否则「run 开始到第一次 start 之间」的改时漏检）。
    let mut coordinator = Coordinator::new(clock, run_id.clone());
    coordinator.establish_anchor(sample);
    probe.step(StartupStep::CoordinatorStarted);

    let app: SharedApp = Arc::new(AppBoundary {
        state: Mutex::new(AppState {
            db,
            coordinator,
            recovery: recovery.clone(),
            // 启动能走到这里就说明第 ④ 步的扫描成功了：没有「扫描失败」的遗留。
            recovery_scan_failed: false,
        }),
        holder: Mutex::new(None),
    });
    let broadcaster = Arc::new(Broadcaster::new(sink));
    let sampling_errors = Arc::new(AtomicU64::new(0));
    let sampling = Scheduler::spawn(config.sampling_interval_ms, {
        let app = Arc::clone(&app);
        let broadcaster = Arc::clone(&broadcaster);
        let errors = Arc::clone(&sampling_errors);
        move || sampling_action(&app, &broadcaster, &errors)
    });
    probe.step(StartupStep::SamplingStarted);

    // ⑥ 开窗口——最后一步：后端就绪之前窗口不该开始拉数据。
    if let Err(e) = open_window() {
        sampling.stop();
        return Err(e);
    }
    probe.step(StartupStep::WindowOpened);

    Ok(Startup::Running(Box::new(RunningApp {
        app,
        broadcaster,
        sampling,
        _lock: lock,
        run_id,
        data_epoch,
        recovery,
        sampling_errors,
    })))
}

/// 采样驱动的一拍：**在串行边界内**取快照，有活动会话才广播。
///
/// 空闲（没有活动会话）时：不广播、不写库（`sample_tick` 的读不算写）。
/// 广播留在临界区内完成，所以「广播顺序 = 提交顺序」。
fn sampling_action(app: &SharedApp, broadcaster: &Broadcaster, errors: &AtomicU64) {
    let mut state = lock_app(app);
    match state.sample_tick() {
        Ok(snapshot) => {
            if snapshot.session_id.is_some() {
                // 失败只记诊断（`Broadcaster` 内部计数），不回滚任何已提交业务。
                broadcaster.emit(EventEnvelope::timer_tick(&snapshot));
            }
        }
        Err(_) => {
            // 采样失败（含恢复语义）只记诊断：下一拍还会再试，
            // 把用户命令或进程拖死都不是它的职责。
            errors.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn io_err(what: &str, e: std::io::Error) -> AppError {
    AppError::Storage {
        detail: format!("{what}: {e}"),
    }
}
