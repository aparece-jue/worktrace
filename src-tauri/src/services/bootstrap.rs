//! **唯一**启动入口（P7 Task 0）。
//!
//! ## 固定顺序（01 §2 + 02 §4，不得调整）
//!
//! ① 单实例检查 → ② 打开库、（真的要迁移时）**迁移前一致备份**、迁移（含新库的库身份）
//! → ③ 新建 `application_run` → ④ 恢复扫描（P3 的四类判定 + 落地事实，随后算门禁快照）
//! → ⑤ 启动协调器与周期采样驱动 → ⑥ 开窗口。
//!
//! 第②步内部再固定成四拍：**拿到单实例锁 → `Db::open` → 读 `user_version`
//! → `user_version < SCHEMA_VERSION` 时 `VACUUM INTO` 备份（否则跳过并记诊断）→ `migrate`**。
//! 备份**失败即拒绝迁移**（不降级、不跳过、不先迁移后补），本次启动以可诊断的失败
//! 结束：不建 `application_run`、不启动协调器与采样驱动、不开窗口。详见
//! [`PreMigrationBackup`] 与 [`crate::services::backup::backup_before_migration`]——
//! **本入口只负责编排与顺序**，命名/保留策略/`VACUUM INTO` 都在 `services::backup`
//! （P6 Task 4a 从那时的本文件整体搬走，不留第二套拷贝）。
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
//! ## 维护态与退出意图（P6 Task 2a）
//!
//! **维护态标志就在 [`AppState`] 里**（与 `db`/`coordinator` 同一临界区，**不新增第二把锁**）：
//! 恢复流程要做的「停止受理写入」因此不需要给 [`Scheduler`] 加 `pause`/`resume`
//! （它没有，而且 `stop()` 不可逆），也不需要等"当前那一拍结束"（那会与正持锁的
//! 调用方互锁——采样每一拍要取的正是这把锁）。
//!
//! 两条判据各只有一个落点，别加第二处：
//!
//! - **采样**：[`AppState::sampling_allowed`] 在 `sampling_action` 取锁之后的**第一句**
//!   被问一次，维护态**整拍跳过**（含读：不 heartbeat、不 tick、不广播、不涨
//!   `sampling_errors`；`Scheduler::ticks` 照涨，它只是"触发了几次"）。
//! - **写入**：[`AppState::guard_writable`] 是唯一门禁，调用点是 `run_command`
//!   （取锁之后、命令体之前）与**绕过它**的两条托盘路径（`tray_pause_impl` 自己判；
//!   `RunningApp::shutdown` 走 [`AppState::begin_exit`] 的互斥）。四个统计/导出
//!   `&mut self` 入口与 `retry_recovery` 也在**取样本之前**先过这道门禁——它们**不是**
//!   只读查询（采样发现异常时可能提交 P2 的恢复事务）。
//!
//! **维护态不是错误**：[`AppState::sampling_allowed`] 的假不代表失败，采样一拍都不记；
//! 被拒的写入走 [`AppState::guard_writable`] 的错误——**第六个码
//! `DATA_RESTORE_IN_PROGRESS`**（[`AppError::DataRestoreInProgress`]，Task 4a 落地，
//! 四处联动见 `src/error.rs` 与前端 `src/types/ipc.ts` 的 `ERROR_CODES`）。
//!
//! ## 故障路径的两半（P6 Task 2b）
//!
//! **路 A：协调器故障态**（`Err`，线程还活着）。`Coordinator::faulted` 一旦置真，
//! `refuse_if_faulted` 会拒绝十个入口 ⇒ 计时没有出口；P2 实现清除、P3 给生产出口
//! （[`AppState::retry_recovery`]）、P8 提供触发，**P6 补的是「故障是怎么被发现的、
//! 被谁看见了」**：
//!
//! - **采样路径按「跃迁」记一条**（[`AppState::observe_timer_availability`]）：故障持续
//!   期间每一拍都拿 `Err(RecoveryRequired)`，逐拍记会把正式诊断刷爆。`sampling_errors`
//!   **口径不变**——它是「采样失败」，故障期间照涨，与「故障次数」不是一回事。
//! - **启动路径点名**：启动后**第一次**采样就已经不可用 ⇒ 记 `startup.timer_unavailable`
//!   （否则用户看到「刚打开就不能计时」却不知道原因）。
//! - **只读观察口** [`AppState::timer_faulted`] 照实投影 `Coordinator::is_faulted` 的
//!   **既有复合语义**（`faulted || pending_committed_reload.is_some()`）：两者对调用方
//!   是同一件事（十个入口一起被拒），诊断文案因此写成「计时不可用（故障态或提交后待
//!   刷新）」——**不得**把「提交后待刷新」误记成「进入故障态」。
//! - **归因边界**：进入行里的 `origin` 是**本拍能拿到的证据**，不是置真点的原始记录——
//!   这一拍自己置的真按判定精确归因（三组），**不是**这一拍置的真只写 `prior_fault`
//!   （命令路径可达的那几组在采样路径上本来就不可分）。口径与依据见
//!   [`FAULT_COMMITTED_REBUILD_FAILED`] 的「归因的证据边界」。
//! - **不自动重试、不自动清故障**：清除只走用户显式触发的 [`AppState::retry_recovery`]。
//!
//! **路 B：采样线程 panic**（`ticks` 停涨）。它不经过协调器，也不涨 `sampling_errors`；
//! 出口是 `platform::scheduler` 的看门狗——[`Scheduler::spawn_watched`] 的退出守卫把
//! `event=sampler.died_unexpectedly` 写进**同一个**诊断落点，只读投影是
//! [`RunningApp::sampling_died_unexpectedly`]。两条路的信号与处置都不同，别混成一条。
//!
//! **诊断落点是同一个文件**（[`StartupConfig::diagnostic_log`]）：启动探针、维护态跃迁、
//! 故障态跃迁都写它——release 的 Windows 子系统没有控制台，`println!` 写出去没人看得见。
//!
//! ## 本阶段不做
//!
//! 恢复确认与历史修正的用户命令、门禁重扫（P3 Task 2 起）；**装卸运行态**
//! （`take_runtime`/`install_runtime`/`runtime_present`、三个访问器改 `Result`）与恢复的
//! 三段流程（P6 Task 4b）；正式 OS 事件源与 `AppState::system_boundary`
//! （P6 Task 2c）；平台事件的实机验收（P8 复核）。**备份编排**在第②步里，原语已搬到
//! [`crate::services::backup`]（Task 4a），本文件不再持有任何拷贝逻辑。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;

use rusqlite::Connection;

use crate::domain::session::SessionState;
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::platform::clock::{Clock, ClockSample};
use crate::platform::diagnostics::Diagnostics;
use crate::platform::paths;
use crate::platform::scheduler::Scheduler;
use crate::platform::single_instance::{self, InstanceLock};
use crate::platform::system_events::{self, SystemEvent, SystemEventKind, SystemEventSource};
use crate::services::backup;
use crate::services::events::{Broadcaster, EventEnvelope, EventSink};
use crate::services::export::{ExportJson, ExportMarkdown, WeeklyQuery};
use crate::services::history::{BackfillRequest, CorrectRequest, HistoryEditReport};
use crate::services::recovery::{DiscardSessionRequest, ReconcileReport, ReconcileRequest};
use crate::services::stats::{StatsRangeQuery, StatsSnapshot, TodayQuery, TodayView};
use crate::services::tasks::{TaskTransitionReport, TransitionTaskRequest};
use crate::services::timer::anchor::SampleVerdict;
use crate::services::timer::coordinator::{
    AcceptClockCorrectionRequest, ClockCorrectionAccepted, CommandOutcome, Coordinator,
    ResumeRequest, SessionRequest, StartRequest,
};
use crate::services::timer::primitives::{end_session_in_tx, EndSessionFacts};
use crate::services::timer::snapshot::TimerSnapshot;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::guard_epoch;
use crate::storage::meta;
use crate::storage::migrations::{current_version, migrate, SCHEMA_VERSION};
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
// 故障路径的诊断词表（P6 Task 2b）
// ─────────────────────────────────────────────────────────────────────────────

/// 采样线程**意外结束**（`on_tick` panic ⇒ `ticks` 停涨）的诊断事件。
///
/// 由 `Scheduler::spawn_watched` 的退出守卫写；它**不是**广播事件（`services/events.rs`
/// 仍然只有 `domain.changed` 与 `timer.tick`——本任务不新增事件名那个口径说的是广播）。
pub const EVENT_SAMPLER_DIED: &str = "sampler.died_unexpectedly";

/// 「计时不可用」的**进入**诊断事件（故障态**或**提交后待刷新）。
pub const EVENT_TIMER_UNAVAILABLE_BEGIN: &str = "timer.unavailable.begin";

/// 启动后**第一次采样**就已经不可用的诊断事件（启动路径点名，见模块头）。
///
/// 它与上一条是**同一个跃迁的两种形态**：`sample_tick` 的第一次观察就不可用时记这一条
/// （「刚打开就不能计时」要在启动诊断里点名），之后才进故障态的记上一条。所以一次跃迁
/// **只会**留下其中一行；**事件名按观察点取**——别的入口（如显式重试）即便抢在第一次
/// 采样之前观察，也不会借用「启动」这个名字（那会名实矛盾）。
pub const EVENT_STARTUP_TIMER_UNAVAILABLE: &str = "startup.timer_unavailable";

/// 「计时不可用」的**清除**诊断事件。
pub const EVENT_TIMER_UNAVAILABLE_END: &str = "timer.unavailable.end";

/// 诊断文案：照实投影 [`Coordinator::is_faulted`] 的**复合语义**（P6-3）。
///
/// **不得**写成「进入故障态」：`is_faulted()` 也可能只因「提交后待刷新」
/// （`pending_committed_reload`）为真，那时并没有进入故障态。
pub const TIMER_UNAVAILABLE_REASON: &str = "计时不可用（故障态或提交后待刷新）";

/// 置真点**分组名**（P6-4：`services/timer/coordinator.rs` 里 13 处置真按成因分 5 组）。
///
/// 这五个名字是**词表**，与置真点一一对应：
///
/// | 分组名 | 置真点 | 触发场景 |
/// | --- | --- | --- |
/// | [`FAULT_COMMITTED_REBUILD_FAILED`] | `rebuild_from_committed` 的 6 处 | 业务命令已提交、重建内存/响应时失败 |
/// | [`FAULT_NO_TRUSTED_BASELINE`] | `read_sample` 的无基线分支 | 采样失败且没有可信基线 |
/// | [`FAULT_ANOMALY_TRANSACTION_FAILED`] | `handle_anomaly` 的兜底 | 异常事务失败 |
/// | [`FAULT_MONOTONIC_BACKWARDS`] | 4 处单调钟硬故障分支 | 本 run 的单调读数已失去意义 |
/// | [`FAULT_CLOCK_CORRECTION_REJECTED`] | `accept_detected_clock_correction` | 接受校正入口撞上硬故障 |
///
/// **归因的证据边界（先读这一段，别把 `origin` 当成置真点的原始记录）**：协调器不暴露
/// 「是哪一处置的真」（P6-3：不新增 P2 访问器），采样路径能拿到的最强证据只有两个——
/// **这一拍之前是否已经在故障态**与**上一拍的判定**。于是 `origin` 只有两种来源
/// （规则与逐条依据见 [`fault_origin`]）：
///
/// - **这一拍自己置的真**（`faulted_before == false`）⇒ 按判定**精确**落到词表里的三组之一；
/// - **不是这一拍置的真**（`faulted_before == true`）⇒ 只能写 [`FAULT_PRIOR_FAULT`]：
///   命令路径的 6 处「提交后重建失败」、接受校正入口那 1 处、以及命令路径上同样可达的
///   异常/硬故障分支，在采样路径上**本来就不可分**。
///
/// **不要**拿上一拍的判定去猜后一种：那个判定可能停在故障**之前**（评审 Important 1 的
/// 可达路径：采样拍先判出 `Suspended`/`Jumped` 且异常事务成功 ⇒ 判定停在非 `Trusted`；
/// 随后一条命令提交成功但 `rebuild_from_committed` 失败置真；下一拍被 `refuse_if_faulted`
/// 拒绝、判定不再更新 ⇒ 猜会把「提交后重建失败」写成 `anomaly_transaction_failed`）。
///
/// 所以：词表 = 5 个分组名 + 1 个「非本拍」值（[`FAULT_PRIOR_FAULT`]），
/// 采样路径实际只会写出其中 4 个字符串；[`FAULT_CLOCK_CORRECTION_REJECTED`] 被观察到时
/// 协调器**已经**在故障态 ⇒ 落 `prior_fault`，不会再单独出现。
pub const FAULT_COMMITTED_REBUILD_FAILED: &str = "committed_rebuild_failed";
/// 见 [`FAULT_COMMITTED_REBUILD_FAILED`] 的表。
pub const FAULT_NO_TRUSTED_BASELINE: &str = "no_trusted_baseline";
/// 见 [`FAULT_COMMITTED_REBUILD_FAILED`] 的表。
pub const FAULT_ANOMALY_TRANSACTION_FAILED: &str = "anomaly_transaction_failed";
/// 见 [`FAULT_COMMITTED_REBUILD_FAILED`] 的表。
pub const FAULT_MONOTONIC_BACKWARDS: &str = "monotonic_backwards";
/// 见 [`FAULT_COMMITTED_REBUILD_FAILED`] 的表。
pub const FAULT_CLOCK_CORRECTION_REJECTED: &str = "clock_correction_rejected";
/// **不是这一拍置的真**——采样路径能诚实说出的上限（见上一条的「归因的证据边界」）。
pub const FAULT_PRIOR_FAULT: &str = "prior_fault";

/// 观察点名字：周期采样的一拍。
const OBSERVER_ENTRY_SAMPLING: &str = "sample_tick";
/// 观察点名字：P3 的显式重试入口（S12）。
const OBSERVER_ENTRY_RETRY: &str = "retry_recovery";

// ─────────────────────────────────────────────────────────────────────────────
// OS 事件路径的诊断词表（P6 Task 2c）
// ─────────────────────────────────────────────────────────────────────────────

/// 维护态期间到达的 OS 事件被**整条跳过**（与周期采样同一个判据）。
pub const EVENT_SYSTEM_EVENT_SKIPPED: &str = "system_events.skipped";

/// 平台边界调用失败（协调器拒绝 / 库忙 / 故障态）。
///
/// 事件是**稀疏**的（锁屏、休眠这种量级），不像采样那样一秒一拍，所以每次失败都记一条
/// 不会刷爆日志；累计次数另有 [`AppState::system_boundary_errors`] 这个只读出口。
pub const EVENT_SYSTEM_BOUNDARY_FAILED: &str = "system_events.boundary_failed";

/// 事件源**起不来**（平台不支持或监听注册失败）：组合根只记这一条，不 panic、不假装成功。
pub const EVENT_SYSTEM_EVENTS_UNAVAILABLE: &str = "system_events.unavailable";

/// 事件源线程**意外结束**（没有停止信号就退出）。
pub const EVENT_SYSTEM_EVENTS_DIED: &str = "system_events.died_unexpectedly";

/// 诊断行里挂钟取不到时的占位（照实记「不知道」，不编一个时刻）。
const WALL_MS_UNKNOWN: &str = "unknown";

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

/// 迁移前备份的**按需判据**走了哪条分支。
///
/// 三条分支都会表现为「这一次没有备份发生」（`NotNeeded` 与 `NothingToBackUp`），
/// 但**原因不同**，所以它们分开报：断言与诊断都按原因看，不合并成「都没备份」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreMigrationBackup {
    /// 库文件本来就不存在（首启）：**没有可备份的事实**。
    ///
    /// 注意新库的 `user_version == 0` 属于「需要迁移」——它在这里被挡下的原因是
    /// 库文件不存在，不是版本相等。
    NothingToBackUp,
    /// 库已经在 [`SCHEMA_VERSION`]：`migrate` 是幂等空操作，没有要保护的迁移动作。
    NotNeeded,
    /// 需要迁移：**已经在 `migrate` 之前**写出一份一致备份。
    Taken,
}

/// 启动次序的观察者。生产侧接诊断日志，测试侧记录次序。
pub trait StartupProbe: Send + Sync {
    fn step(&self, step: StartupStep);

    /// 迁移前备份的按需判据结果（**在 `migrate` 之前**报出）。
    ///
    /// 默认什么都不做：既有实现（含 [`NoProbe`]）不必为一个新信号改签名，
    /// 生产探针把它接进启动诊断。它**不是** `StartupStep`——备份不改变六步顺序，
    /// 而 `StartupStep::ALL` 的条数由另一条断言钉着。
    fn pre_migration_backup(&self, _outcome: PreMigrationBackup) {}
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
    /// 迁移前备份的落盘目录。
    ///
    /// `None` = 生产缺省：**只在确实需要迁移时**才解析 `app_data_dir()/backups`——
    /// 首启与「版本相等」两条路径本来就不该有产物，不该因为拿不到应用数据目录而失败。
    /// 测试（以及打包路径实验）注入临时目录，免得把产物写进开发机**真实**的数据目录、
    /// 让「需迁移 ⇒ 有产物 / 无需迁移 ⇒ 零产物」这对断言互相污染。
    pub backup_dir: Option<PathBuf>,
    /// 正式诊断日志的落盘路径（P6 Task 2a）。
    ///
    /// `None` = **关闭**（[`StartupConfig::new`] 的缺省）：显式路径构造多用于测试与打包
    /// 路径实验，缺省落盘只会在临时目录里多留文件——更糟的是，夹具一旦忘了注入就会写进
    /// 开发机**真实**的数据目录（Task 1 的变异实测过这种污染）。生产走
    /// [`StartupConfig::from_app_paths`]：`app_data_dir()/worktrace.log`。
    /// 测试要读回内容时用 [`StartupConfig::with_diagnostic_log`] 显式注入。
    pub diagnostic_log: Option<PathBuf>,
}

/// 生产缺省下诊断日志的文件名（与库、锁同在应用数据目录）。
///
/// 公开是有意的：2b 的故障态跃迁、P8 的排查都要知道去哪读。
pub const DIAGNOSTIC_LOG_FILE: &str = "worktrace.log";

impl StartupConfig {
    /// 用平台路径构造。库与锁同目录（`platform::paths` 已保证）。
    ///
    /// 备份目录**不在这里解析**：它是按需的，解析推迟到 [`startup`] 里
    /// 「确实需要迁移」那条分支（见 [`StartupConfig::backup_dir`]）。
    /// 诊断日志的路径**在这里就定下来**（它每一条记录都要用，且只是拼个路径、不建文件）。
    pub fn from_app_paths() -> Result<Self, AppError> {
        Ok(Self {
            db_path: paths::database_file().map_err(|e| io_err("db path", e))?,
            lock_path: paths::instance_lock_file().map_err(|e| io_err("lock path", e))?,
            sampling_interval_ms: DEFAULT_SAMPLING_INTERVAL_MS,
            backup_dir: None,
            diagnostic_log: Some(
                paths::app_data_dir()
                    .map_err(|e| io_err("diagnostic log path", e))?
                    .join(DIAGNOSTIC_LOG_FILE),
            ),
        })
    }

    /// 显式路径（测试与打包路径实验用）。**诊断落盘关闭**（见
    /// [`StartupConfig::diagnostic_log`]）。
    pub fn new(db_path: impl Into<PathBuf>, lock_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            lock_path: lock_path.into(),
            sampling_interval_ms: DEFAULT_SAMPLING_INTERVAL_MS,
            backup_dir: None,
            diagnostic_log: None,
        }
    }

    /// 指定迁移前备份的目录（不调则由生产缺省按需解析）。
    pub fn with_backup_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.backup_dir = Some(dir.into());
        self
    }

    /// 指定诊断日志的落盘路径（测试读回内容用；不调则由 [`StartupConfig::new`] 关闭）。
    pub fn with_diagnostic_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.diagnostic_log = Some(path.into());
        self
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

    /// 采样线程是否**意外结束**（P6 Task 2b 的看门狗出口）。
    ///
    /// 真 ⇒ 线程已经不在了、`sampling_ticks` 停涨，而**故障态检测那一半也再没有采样拍
    /// 可看**：所以这条信号与 [`AppState::timer_faulted`] 是**互补**的，不是同一件事的
    /// 两个名字（见模块头「故障路径的两半」）。对应的诊断行是
    /// `event=sampler.died_unexpectedly`，由 `Scheduler::spawn_watched` 的退出守卫写。
    pub fn sampling_died_unexpectedly(&self) -> bool {
        self.sampling.died_unexpectedly()
    }

    /// **显式退出**（Task 4 的托盘「退出」与 P8 复用这一条入口）。
    ///
    /// 顺序：`holds_app_lock` 检查 → **`begin_exit`（退出意图）** → **停定时器**
    /// （此后不会再有新的一拍进事务）→ 在**一个事务**里
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
    ///
    /// **维护态同样拒绝**（P6 Task 2a）：置位的是 [`AppState::begin_exit`]，位置在
    /// `sampling.stop()` **之前**（见函数体的顺序说明）。拒绝返回维护态错误，
    /// **采样线程仍在跑**——用户不能在恢复中途退出进程（恢复很短，强杀会把库停在中间态）。
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

        // **退出意图先置位**（P6 Task 2a，fix round 4 的 C-2）：`sampling.stop()` 不可逆，
        // 维护态下先停采样再发现"访问器返 Err"⇒ 采样永久停、退出事务没跑、进程只能强杀。
        // 所以顺序固定为 holds_app_lock 检查 → `begin_exit`（锁内、短）→ `sampling.stop()`
        // → `explicit_exit`；维护态在 `begin_exit` 就被拒，**这一条路径上 `stop()` 不会被调到**。
        //
        // `begin_exit` 与退出时刻的采样共用**同一次**取锁（都是锁内短操作）：置位与读时刻
        // 之间没有窗口，别的路径不可能插进来把维护态置上。
        let at = {
            let mut state = lock_app(&self.app);
            state.begin_exit()?;
            // 退出时刻必须来自协调器的时钟（生产是 SystemClock，测试是 FakeClock）：
            // 这不仅是为了可测，也是为了让「退出」与「归属」用同一个时间来源。
            let AppState {
                db, coordinator, ..
            } = &mut *state;
            coordinator.snapshot(db)?.as_of
        };

        self.sampling.stop();

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

/// 维护态阶段（V0.1 只有恢复需要维护态；备份走在线备份，不停写入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenancePhase {
    /// 恢复/替换库：换库窗口里没有可用的运行态。
    Restore,
}

impl MaintenancePhase {
    /// 稳定名字：诊断日志与测试按它读，不依赖 `Debug` 的拼法。
    pub fn as_str(self) -> &'static str {
        match self {
            MaintenancePhase::Restore => "Restore",
        }
    }
}

/// 进程内唯一的维护态记录。**字段私有**，读走 [`AppState::maintenance`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceState {
    phase: MaintenancePhase,
    entered_at_ms: i64,
}

impl MaintenanceState {
    /// 阶段（只读：置位/清位只能走 [`AppState`] 的方法）。
    pub fn phase(&self) -> MaintenancePhase {
        self.phase
    }

    /// 进入维护态的**挂钟毫秒**（由调用方从时钟接缝取，不由本类型自取）。
    pub fn entered_at_ms(&self) -> i64 {
        self.entered_at_ms
    }
}

/// 这条错误是不是「维护态拒绝」。
///
/// 用途只有一个：托盘的「退出」在维护态下要**拒绝退出**（不调 `handle.exit`），
/// 而它与别的失败（持有串行边界、退出事务失败）必须分开处理。
///
/// 按**码**判定（Task 4a 落地第六个码之前，这里靠 `STORAGE_ERROR` + 一个 detail 前缀
/// 认出来——那只是临时的形状）。调用方仍可用 `error.code()` 直接比对，这个函数存在的
/// 意义是让"维护态拒绝"这个判断只有一个落点。
pub fn is_maintenance_refusal(error: &AppError) -> bool {
    matches!(error, AppError::DataRestoreInProgress)
}

/// 按**本拍能拿到的证据**归因一次「刚观察到的不可用」（P6-4 的分组名词表 + `prior_fault`）。
///
/// 规则只有两条（边界与依据见 [`FAULT_COMMITTED_REBUILD_FAILED`] 的说明）：
///
/// 1. **`faulted_before == true`（不是这一拍置的真）⇒ [`FAULT_PRIOR_FAULT`]。**
///    这一支**不看判定**：判定可能停在故障**之前**，拿它归因会把命令路径的「提交后重建
///    失败」写成 `anomaly_transaction_failed`（评审 Important 1 的可达路径）。
/// 2. **`faulted_before == false`（这一拍自己置的真）⇒ 按判定精确归因**：
///    - `MonotonicBackwards` ⇒ [`FAULT_MONOTONIC_BACKWARDS`]：它是 `d_mono < 0` 的唯一来源，
///      `try_handle_anomaly` 的 4 处硬故障分支都以它为判据；
///    - 其余 `needs_recovery()` ⇒ [`FAULT_ANOMALY_TRANSACTION_FAILED`]：采样拍上唯一能
///      带着异常判决置真的地方是 `handle_anomaly` 的兜底（`Err(_)` 分支）；
///    - `Trusted` ⇒ [`FAULT_NO_TRUSTED_BASELINE`]：采样拍自己置真且没有任何异常判决，
///      只剩 `read_sample` 的「没有基线可采样」那条（启动第⑤步就建立了基线，所以它在
///      生产上几乎只有时钟彻底坏掉时才可达）。
///
/// 第 5 组 [`FAULT_CLOCK_CORRECTION_REJECTED`] 被采样路径观察到时协调器**已经**在故障态
/// ⇒ 必然落第 1 条（`prior_fault`），不会再单独出现。
fn fault_origin(verdict: SampleVerdict, faulted_before: bool) -> &'static str {
    if faulted_before {
        return FAULT_PRIOR_FAULT;
    }
    if matches!(verdict, SampleVerdict::MonotonicBackwards { .. }) {
        return FAULT_MONOTONIC_BACKWARDS;
    }
    if verdict.needs_recovery() {
        return FAULT_ANOMALY_TRANSACTION_FAILED;
    }
    FAULT_NO_TRUSTED_BASELINE
}

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
    /// 维护态：`Some` 期间**不采样、不受理写入**（P6 Task 2a）。
    ///
    /// 为什么放在这里而不是 `Scheduler` 上：判定必须与 `db` 在**同一临界区**里
    /// （D6 的"同一串行边界"），而且只有"取锁之后"这个位置才能覆盖**已经在途**的那一拍。
    /// `Scheduler` 保持只有 `stop()`（不可逆）这一条停机语义，**不加 pause/resume**。
    maintenance: Option<MaintenanceState>,
    /// **退出意图**（P6 Task 2a，fix round 4 的 C-2）：`shutdown` 已经决定退出。
    ///
    /// 为什么不能只看一眼再 `stop()`：`Scheduler::stop()` 不可逆，而「看一眼前」到
    /// `stop()` 之间有一个窗口——维护态若恰好在那时置位，采样线程照样被永久停掉、
    /// 退出事务照样跑不成，进程只能强杀。置位与维护态**互斥**，窗口因此关闭。
    shutting_down: bool,
    /// 采样/启动路径**上一次观察到的**「计时是否可用」（P6 Task 2b）。
    ///
    /// `None` = **还没有观察过**（启动后第一次采样还没来）；`Some(false)` = 上次可用；
    /// `Some(true)` = 上次不可用。诊断只在**跃迁**上记一条，所以这一位必须与 `db`/`coordinator`
    /// 同处一个临界区（不新增第二把锁）：比较与记录必须是一次原子动作，否则两拍并发
    /// （理论上只有一拍，但 `retry_recovery` 也会观察）会各记一条。
    timer_unavailable: Option<bool>,
    /// 平台边界（OS 事件）路径上 `system_boundary` **失败**的累计次数（P6 Task 2c）。
    ///
    /// 与 `RunningApp::sampling_errors` 同档的「连续失败可被上层观察到」出口：事件源起不来
    /// 是**一次性**的（由组合根记诊断），而事件处理连续失败（例如协调器已经在故障态、
    /// 或者库忙）需要有一个能读到的计数，否则现象是「锁屏不再暂停，但没有任何一处会红」。
    system_boundary_errors: u64,
    /// 正式诊断日志的落点（release 的 Windows 子系统没有控制台，见模块头）。
    diagnostics: Diagnostics,
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

    /// 诊断落点（只读借用）。服务与命令层在**同一次状态跃迁**里顺手记一条。
    ///
    /// 本任务用它记维护态的进入/退出；Task 2b 的故障态进入/清除用**同一个**落点。
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    /// 只读投影：**计时现在可不可用**（P6 Task 2b 的观察口）。
    ///
    /// 照实投影 [`Coordinator::is_faulted`] 的**既有复合语义**
    /// （`faulted || pending_committed_reload.is_some()`）：故障态与「提交后待刷新」都会让
    /// 十个入口一起返回 `RECOVERY_REQUIRED`，对观察者是同一件事。**不新增 P2 访问器、
    /// 不在服务层拆字段**（P6-3）——拆开只会诱导调用方按错误的粒度分支。
    ///
    /// **它只是观察口，不是出口**：清除仍然只能走 [`AppState::retry_recovery`]（P3 S12），
    /// P6 不做自动重试、也不自动清故障。
    ///
    /// **Task 4 的口径（先写在这里，免得装卸运行态时漏掉）**：运行态不在手（维护态的换库
    /// 窗口）时返回 `false`——观察口不制造「故障」这种结论。
    pub fn timer_faulted(&self) -> bool {
        self.coordinator.is_faulted()
    }

    /// 观察一次「计时是否可用」，在**跃迁**上记一条诊断，返回本次观察值（P6 Task 2b）。
    ///
    /// 为什么不每拍记一条：故障态下每一拍都会拿到 `Err(RecoveryRequired)`，
    /// 1 秒一条会把正式诊断日志刷爆——诊断要的是**跃迁**（进入/清除各一条）。
    ///
    /// 口径（别与 `sampling_errors` 混）：这里「只记一条」**不改变** `sampling_errors`，
    /// 后者是「采样失败」的既有计数，故障期间照涨（每一拍都算一次失败）。
    ///
    /// - `entry`：观察点（[`OBSERVER_ENTRY_SAMPLING`] / [`OBSERVER_ENTRY_RETRY`]）。
    /// - `faulted_before`：**调用协调器之前**读到的值——归因要用它区分「这一拍自己置的真」
    ///   与「已经在故障态（别处置的真）」。观察点必须自己先读一次，这里不替它猜。
    /// - `snapshot_wall_ms`：**同一次调用**里已经取到的挂钟（快照的 `as_of`）；没有就给
    ///   `None`，由 [`AppState::diagnostic_wall_ms`] 决定是补一次采样还是照实记 `unknown`。
    fn observe_timer_availability(
        &mut self,
        entry: &str,
        faulted_before: bool,
        snapshot_wall_ms: Option<i64>,
    ) -> bool {
        let unavailable = self.timer_faulted();
        let previous = self.timer_unavailable.replace(unavailable);
        let transition = match previous {
            // 第一次观察就是「可用」：没有跃迁可记（健康的启动不该在日志里留一行）。
            None => unavailable,
            Some(previous) => previous != unavailable,
        };
        if !transition {
            return unavailable;
        }

        let run_id = self.coordinator.run_id().to_string();
        let wall_ms = self.diagnostic_wall_ms(entry, snapshot_wall_ms);
        if unavailable {
            // **启动路径点名只属于采样拍**（"启动后第一次采样"）：事件名按**观察点**取，
            // 不按"是不是第一次观察"取——否则「重试对账早于第一次采样且仍不可用」会写出
            // `event=startup.timer_unavailable … entry=retry_recovery` 这种名实矛盾的行。
            let event = if previous.is_none() && entry == OBSERVER_ENTRY_SAMPLING {
                EVENT_STARTUP_TIMER_UNAVAILABLE
            } else {
                EVENT_TIMER_UNAVAILABLE_BEGIN
            };
            let origin = fault_origin(self.coordinator.last_verdict(), faulted_before);
            self.diagnostics.record(
                event,
                &format!(
                    "run_id={run_id} wall_ms={wall_ms} origin={origin} entry={entry} \
                     reason={TIMER_UNAVAILABLE_REASON}"
                ),
            );
        } else {
            self.diagnostics.record(
                EVENT_TIMER_UNAVAILABLE_END,
                &format!("run_id={run_id} wall_ms={wall_ms} entry={entry}"),
            );
        }
        unavailable
    }

    /// 诊断行里的 `wall_ms`：优先用**同一次调用**已经取到的那个（快照的 `as_of`，零成本）。
    ///
    /// 没有快照时（失败拍不带 `as_of`）按观察点分两种：
    ///
    /// - **采样拍可以补一次** `Coordinator::wall_ms()`：它的既有口径就是「用完即弃、不喂给
    ///   锚点或检测器」，所以不会影响任何判定，只多一次读钟。
    /// - **显式重试入口不可以**：P3 的用例钉着「S12 全路径只取一次样本」
    ///   （`tests/exception_closure.rs::retry_recovery_clears_the_fault_only_after_a_successful_commit`
    ///   数的是 `clock.sample()` 的**调用次数**），多一次就红。而重试成功时 `Ok(snapshot)`
    ///   一定带着 `as_of`，所以「清除」记录永远有真实挂钟，不会退化成 `unknown`。
    fn diagnostic_wall_ms(&self, entry: &str, snapshot_wall_ms: Option<i64>) -> String {
        if let Some(wall_ms) = snapshot_wall_ms {
            return wall_ms.to_string();
        }
        if entry == OBSERVER_ENTRY_SAMPLING {
            if let Ok(wall_ms) = self.coordinator.wall_ms() {
                return wall_ms.to_string();
            }
        }
        WALL_MS_UNKNOWN.to_string()
    }

    /// 只读投影：当前维护态（诊断与 P8 的状态展示）。
    pub fn maintenance(&self) -> Option<&MaintenanceState> {
        self.maintenance.as_ref()
    }

    /// 进入维护态。**已在维护态 ⇒ 拒绝**（不覆盖进入时刻、不重入）。
    ///
    /// 退出意图已置位时同样拒绝（两者互斥，见 [`AppState::begin_exit`]）。
    /// 进入/退出各记一条诊断（含 `phase`、`entered_at_ms`、时长）。
    pub fn begin_maintenance(
        &mut self,
        phase: MaintenancePhase,
        entered_at_ms: i64,
    ) -> Result<(), AppError> {
        if self.shutting_down {
            return Err(AppError::DataRestoreInProgress);
        }
        if self.maintenance.is_some() {
            return Err(AppError::DataRestoreInProgress);
        }
        self.maintenance = Some(MaintenanceState {
            phase,
            entered_at_ms,
        });
        self.diagnostics.record(
            "maintenance.begin",
            &format!("phase={} entered_at_ms={entered_at_ms}", phase.as_str()),
        );
        Ok(())
    }

    /// 退出维护态，交回被清掉的那份记录（**幂等**：已清则 `None`，也不再记一条）。
    ///
    /// 时长取**同一条时钟接缝**（[`AppState::now_ms`]）的当前墙钟减去 `entered_at_ms`；
    /// 时钟取不到时照实记 `unknown`，不因为诊断失败而拒绝退出。
    ///
    /// **调用时机（写给 Task 4）**：恢复流程的 ③-a/③-b 在 `install_runtime` **之后**才调它，
    /// 所以这条时钟接缝那时又在手了；顺序若反过来（先清维护态、后装运行态），时长会退化成
    /// `unknown`——不是错误，但会丢掉维护窗口的长度。
    pub fn end_maintenance(&mut self) -> Option<MaintenanceState> {
        let state = self.maintenance.take()?;
        let left_at_ms = self.now_ms().ok();
        let left = match left_at_ms {
            Some(left_at_ms) => format!(
                "left_at_ms={left_at_ms} duration_ms={}",
                left_at_ms - state.entered_at_ms
            ),
            None => "left_at_ms=unknown duration_ms=unknown".to_string(),
        };
        self.diagnostics.record(
            "maintenance.end",
            &format!(
                "phase={} entered_at_ms={} {left}",
                state.phase.as_str(),
                state.entered_at_ms
            ),
        );
        Some(state)
    }

    /// 采样判据：无维护态才允许采样。**采样拍在 `lock_app` 之后第一句就问它。**
    ///
    /// 维护态**不调** `Scheduler::stop()`（不可逆，调了就再也回不来）：
    /// 这里返回 `false` 只是让那一拍**整拍跳过**（含读），线程与 `ticks` 都照旧。
    pub fn sampling_allowed(&self) -> bool {
        self.maintenance.is_none()
    }

    /// 写入门禁：维护态 ⇒ [`AppError::DataRestoreInProgress`]（第六个码
    /// `DATA_RESTORE_IN_PROGRESS`，Task 4a；前端按 `code` 显示「正在恢复」）。
    ///
    /// 调用点**唯一**是 `commands` 那层的 `run_command`（取锁之后、命令体之前）；
    /// 绕过它的两条托盘路径与四个统计/导出入口各自显式调用它（见模块头）。
    ///
    /// **2c 的 `AppState::system_boundary`（平台可信边界）落地时也必须先过这一道**：
    /// 否则维护态期间一次锁屏/唤醒通知会绕开隔离直接写库。
    pub fn guard_writable(&self) -> Result<(), AppError> {
        match &self.maintenance {
            Some(_) => Err(AppError::DataRestoreInProgress),
            None => Ok(()),
        }
    }

    /// 置位**退出意图**（P6 Task 2a，fix round 4 的 C-2）。判据：
    ///
    /// - **维护态 ⇒ 拒绝**（不碰采样线程）：此刻运行态正要被换掉，退出事务既跑不成，
    ///   强杀还会把库停在中间态；
    /// - 否则置位并返回成功。重复调用**幂等**（退出是终态，第二次调不改语义）。
    ///
    /// 与 [`AppState::begin_maintenance`] **互斥**：置位之后维护态进不来。
    pub fn begin_exit(&mut self) -> Result<(), AppError> {
        if self.maintenance.is_some() {
            return Err(AppError::DataRestoreInProgress);
        }
        self.shutting_down = true;
        Ok(())
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
            // 维护态/退出意图/诊断落点与这次重扫无关（Task 2a 新增；`..` 同时让以后
            // 新增字段不再需要改这一处）。
            ..
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

    /// **统计快照**（P5 Task 1）：在**同一条串行边界**内取一次协调器样本，再用一次
    /// 一致读把事实冻成快照；纯聚合与序列化可在边界外基于它进行。
    /// 正常采样只读；采样发现异常时按 P2 的异常路径处理——**可能**提交恢复事务及其审计
    /// （幂等分支与硬故障回滚分支**零写入**），随后返回恢复错误。
    /// （[`StatsSnapshot::report`]）。
    ///
    /// 为什么必须是这一个方法，而不是让服务层自己够协调器：`AppState.coordinator` 是
    /// 私有字段（把 `&mut Coordinator` 递出去，「不得忽略历史」就只剩纪律），而运行区间
    /// 的终点只能来自 [`Coordinator::stats_sample`]。所以这里与 `start`/`pause` 同一姿势：
    /// 解构 `AppState`、取一次样本、交给服务——IPC 命令归 P8，本层只开这一条接缝。
    ///
    /// **不过恢复门禁**：门禁只挡「开始新计时」（`start`/`resume`），统计是只读查询；
    /// 真正不可信的那些事实由 `stats_sample` 自己按恢复语义拒绝（`RECOVERY_REQUIRED`），
    /// 不在这里重复判一遍。
    ///
    /// **过维护态门禁，且在取样本之前**（2026-10-08 收口）。它与上面那条恢复门禁的**口径不同**：
    /// 恢复门禁挡的是「开始新计时」，维护态挡的是「此刻没有可用的运行态」（Task 4 的
    /// `take_runtime` 之后连库都不在手）。而且这条入口**不是**纯粹的只读查询——采样发现
    /// 异常时它会提交 P2 的恢复事务（见上段），所以不能按"只读"绕过维护隔离。
    pub fn stats_snapshot(&mut self, query: &StatsRangeQuery) -> Result<StatsSnapshot, AppError> {
        self.guard_writable()?;
        let AppState {
            db, coordinator, ..
        } = self;
        let sample = coordinator.stats_sample(db)?;
        crate::services::stats::snapshot(db, sample, query)
    }

    /// **Today 聚合**（P5 Task 2，F-010）：与 [`AppState::stats_snapshot`] 同一姿势——
    /// 同一条串行边界内取一次样本，再由服务在**一个读事务**里取事实、今日选择列表与
    /// 当前会话，一次返回五项（分别显示、不预先相加）。
    ///
    /// 为什么今天也走这条接缝：五项必须出自**同一次**样本与同一个版本（同一
    /// `as_of`/`revision`），而 `AppState.coordinator` 是私有字段——样本只能从这里取。
    /// 同样**不过恢复门禁**（统计是只读查询，判据在 `stats_sample` 自己那里）；
    /// **过维护态门禁，且在取样本之前**（与 [`AppState::stats_snapshot`] 同一条理由：
    /// 采样发现异常时它会提交 P2 的恢复事务）；
    /// **正常采样下只读**；采样发现异常时按 P2 的异常路径处理——**可能**提交恢复事务及其审计
    /// （幂等分支与硬故障回滚分支**零写入**），随后返回恢复错误。IPC 命令归 P8。
    pub fn stats_today(&mut self, query: &TodayQuery) -> Result<TodayView, AppError> {
        self.guard_writable()?;
        let AppState {
            db, coordinator, ..
        } = self;
        let sample = coordinator.stats_sample(db)?;
        crate::services::stats::today(db, sample, query)
    }

    /// **JSON 明细导出**（P5 Task 3，F-018）：与 [`AppState::stats_snapshot`] 同一姿势——
    /// 同一条串行边界内取一次样本，交给 `services::export` 用**同一份** `services::stats`
    /// 取数路径产出明细导出。导出**不依赖 AI、也不需要网络**；**正常采样下只读**；采样异常时按 P2 的
    /// 异常路径处理——**可能**提交恢复事务及其审计（幂等分支与硬故障回滚分支**零写入**），随后返回恢复错误；
    /// **落盘归 P8**，这一层只返回内容（[`ExportJson::text`]）。**维护态在取样本之前就挡住**
    /// （与 [`AppState::stats_snapshot`] 同一条理由）。
    ///
    /// **生成时间由这一层给**（`generated_at` 是服务层的显式参数）：服务层不许读时钟
    /// （分层门禁机器强制），所以这里从**平台时钟接缝**取一次**生成本刻的墙钟**
    /// （[`AppState::now_ms`] → `Coordinator::wall_ms` → `platform::clock::Clock`），
    /// 与样本同处一条串行边界。它与数据水位是两件事：数字仍**全部**来自那一次样本
    /// （`as_of` 是它的归属终点 `A(M)`），`generated_at` 只说明这份文件何时产出
    /// （Ruling P5-19）——两者不相等时，导出不代表数据更新到了那一刻。
    pub fn export_json(&mut self, query: &StatsRangeQuery) -> Result<ExportJson, AppError> {
        self.guard_writable()?;
        let AppState {
            db, coordinator, ..
        } = self;
        let sample = coordinator.stats_sample(db)?;
        let generated_at = coordinator.wall_ms()?;
        crate::services::export::json(db, sample, query, generated_at)
    }

    /// **Markdown 周回顾**（P5 Task 4，F-018）：与 [`AppState::export_json`] 同一姿势——
    /// 同一条串行边界内取一次样本，交给 `services::export` 用**同一份** `services::stats`
    /// 取数路径产出周回顾（人工投入 / 完成任务 / 待确认记录三节）。**不依赖 AI、也不需要
    /// 网络**；**正常采样下只读**；采样异常时按 P2 的异常路径处理——**可能**提交恢复事务及其审计
    /// （幂等分支与硬故障回滚分支**零写入**），随后返回恢复错误；**落盘归 P8**，这一层只返回内容
    /// （[`ExportMarkdown::text`]）。**维护态在取样本之前就挡住**（与
    /// [`AppState::stats_snapshot`] 同一条理由）。
    ///
    /// **生成时间由这一层给**（`generated_at` 是服务层的显式参数，服务层不许读时钟）：
    /// 取自**平台时钟接缝**的**生成本刻的墙钟**（[`AppState::now_ms`] →
    /// `Coordinator::wall_ms`），与样本同处一条串行边界；它只说明这份文件何时产出，
    /// 数字仍**全部**来自那一次样本（`as_of` 是它的归属终点 `A(M)`，Ruling P5-19 对
    /// Markdown 同一口径）。
    ///
    /// **周界由服务定**：请求里的 `anchor` 省略时用**同一次样本**的归属终点算「本周」，
    /// 与 Today 的「今天」同一口径——调用方不必、也不该为此另采一次墙钟。
    pub fn export_weekly_markdown(
        &mut self,
        query: &WeeklyQuery,
    ) -> Result<ExportMarkdown, AppError> {
        self.guard_writable()?;
        let AppState {
            db, coordinator, ..
        } = self;
        let sample = coordinator.stats_sample(db)?;
        let generated_at = coordinator.wall_ms()?;
        crate::services::export::weekly(db, sample, query, generated_at)
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

    /// **平台可信边界**（P6 Task 2c）：通往 `Coordinator::system_pause` 的**唯一**生产入口。
    ///
    /// 与周期采样是**并列的第二条**路径，不是同一条：采样走 [`AppState::sample_tick`]
    /// （心跳 + tick，**永不调 `system_pause`**），本入口走平台边界。两者共用的是
    /// **同一把锁与同一份 `AppState`**，所以它们不可能并发进入协调器。
    ///
    /// 三条纪律：
    ///
    /// 1. **取不到锁是正常的**（说明有别的操作在临界区里）——调用方在 `lock_app` 上**排队**，
    ///    拿到之后按**当时**的状态重新判定。本方法不用 `try_lock`、不丢弃、不 panic。
    /// 2. **维护态与周期采样同一判据**：调用方（[`system_events_action`]）取锁之后已经问过
    ///    [`AppState::sampling_allowed`]（跳过并记一条诊断）；这里**再判一次**是防线，
    ///    让任何未来的调用方都绕不过去——换库窗口里 `db` 这个句柄正被关闭/替换，
    ///    读它要么报错要么读到已经作废的那个世界。
    /// 3. **边界合法性一律交 P2**：本方法不写任何时钟判断（`None` = 边界未知/晚到 ⇒
    ///    `recovering`；唤醒不自动继续）。
    pub fn system_boundary(
        &mut self,
        boundary: Option<ClockSample>,
    ) -> Result<TimerSnapshot, AppError> {
        if !self.sampling_allowed() {
            return Err(AppError::DataRestoreInProgress);
        }

        let outcome = {
            let AppState {
                db, coordinator, ..
            } = self;
            coordinator.system_pause(db, boundary)
        };

        if let Err(error) = &outcome {
            // 「连续失败要能被上层观察到」：计数 + 一条诊断（见 `system_boundary_errors`）。
            self.system_boundary_errors = self.system_boundary_errors.saturating_add(1);
            self.diagnostics.record(
                EVENT_SYSTEM_BOUNDARY_FAILED,
                &format!(
                    "errors={} code={} detail={}",
                    self.system_boundary_errors,
                    error.code(),
                    error.detail().unwrap_or_default()
                ),
            );
        }

        outcome
    }

    /// 平台边界路径的失败累计次数（与 `RunningApp::sampling_errors` 同档的只读出口）。
    pub fn system_boundary_errors(&self) -> u64 {
        self.system_boundary_errors
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
    /// 2. **当且仅当被改动的会话正是协调器此刻镜像的那条**时 `Coordinator::refresh_committed_session`
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
    ///
    /// 两步**互相独立**（P3 终审 I1）：重算失败只说明门禁快照没跟着走，
    /// 不是「内存可以停在旧状态」——所以先取重扫的结果，照样做镜像刷新，
    /// 最后再把重扫的错误抛出去。
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
        let rescan = self.rescan_recovery();
        // 第二步：条件化镜像刷新——**不能在重扫失败时被跳过**。
        self.refresh_mirror_if_live(&session_id)?;
        rescan?;
        Ok(outcome)
    }

    /// 提交之后的**条件化镜像刷新**（Ruling 13）：当且仅当被改动的会话正是协调器
    /// 此刻镜像的那条（`live`）时，按**已提交**的事实把它重新载入。
    ///
    /// 为什么必须有这一步：`Coordinator::build` 从内存镜像渲染快照。被改的会话若正是
    /// 镜像那条，不刷新就等于继续按旧状态出快照——已作废的会话仍被报成 `running`、
    /// `active_ms` 继续涨，直到某条别的命令把它重新载入。计划的新增-2 正因为这条性质
    /// 才写明「**不得**留一个继续按旧状态出快照的 `live`」。
    ///
    /// 为什么只在被改动的那条命中时才刷新：无条件刷新会把 `live` 换成另一条会话
    /// （可达位移见 [`AppState::reconcile`] 的文档）。
    ///
    /// 为什么抽成一处（P3 终审 I2a）：`reconcile` / `correct` / `discard_session` 原先
    /// 各有**逐字相同**的一份；「只在被改动的是 `live` 那条时刷新」这条纪律不能靠
    /// 三份拷贝维持。
    ///
    /// 失败映射 [`AppError::RecoveryRequired`]（P2 的提交后约定：事实已经落库，
    /// 缺的是内存与事实重新对上）。
    fn refresh_mirror_if_live(&mut self, session_id: &str) -> Result<(), AppError> {
        // `live()` 是只读访问器；先问清楚「镜像的是不是这条」，再决定要不要刷新。
        let mirrored = {
            let AppState { coordinator, .. } = self;
            coordinator
                .live()
                .map(|live| live.id == session_id)
                .unwrap_or(false)
        };
        if !mirrored {
            return Ok(());
        }
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator
            .refresh_committed_session(db.connection(), session_id)
            .map_err(|_| AppError::RecoveryRequired)
    }

    /// 历史修正（重定时 / 软删除）的命令入口（P3 S2）。`env` 的版本位是**会话**版本。
    ///
    /// 与 [`AppState::reconcile`] 的两点不同（Task 3 的计划原文）：
    /// 1. **不重扫门禁**——`correct` 不改恢复性，门禁快照照旧；
    /// 2. 提交后仍然**当且仅当被改动的会话正是协调器此刻镜像的那条**时
    ///    `Coordinator::refresh_committed_session` 刷新它（Ruling 13）：`finish` 之后 `live` 还停在
    ///    那条 `finished` 会话上，所以这条真的会命中——不刷新的话，快照的
    ///    `closed_trusted_ms` 会继续按修正前的时长算。刷新失败同样映射
    ///    [`AppError::RecoveryRequired`]（提交后约定：已提交事实保留，缺的是内存与事实
    ///    重新对上）。
    pub fn correct(
        &mut self,
        env: WriteEnvelope,
        req: CorrectRequest,
    ) -> Result<WriteOutcome<HistoryEditReport>, AppError> {
        let now = self.coordinator.wall_ms()?;
        let session_id = req.session_id.clone();
        let outcome = {
            let AppState { db, .. } = self;
            crate::services::history::correct(db, env, req, now)?
        };

        // 唯一的提交后收尾：条件化镜像刷新（不重扫门禁，见上）。
        self.refresh_mirror_if_live(&session_id)?;
        Ok(outcome)
    }

    /// 手工补录的命令入口（P3 S2）。`env` 是**新建**信封（只带 epoch）。
    ///
    /// **提交后什么都不做**——这是它与 [`AppState::reconcile`] 的两点不同之一
    /// （另一点是它不重扫门禁）：补录新建的是一条 `finished` 行，它不改变协调器
    /// 正镜像的那条会话（`live` 仍指向正在计时的那条，或本来就是 `None`），
    /// 也不产生任何待处理事实，所以既不需要 `rescan_recovery`（S1 的调用方只有
    /// `reconcile`/`discard_session`），也不需要 `load_session`。
    pub fn backfill(
        &mut self,
        env: WriteEnvelope,
        req: BackfillRequest,
    ) -> Result<WriteOutcome<HistoryEditReport>, AppError> {
        let (now, run_id) = {
            let coordinator = &self.coordinator;
            (coordinator.wall_ms()?, coordinator.run_id().to_string())
        };
        let AppState { db, .. } = self;
        crate::services::history::backfill(db, env, req, now, &run_id)
    }

    /// 作废整次的命令入口（P3 S2）。`env` 的版本位是**会话**版本。
    ///
    /// **无状态前置**：`recovering`/`running`/`paused`/`finished` 都能作废——包括
    /// 「`paused` 却仍有待确认区间」这一类四类判定盖不住的形态（Ruling 6 的出口）。
    ///
    /// 提交之后两步（与 [`AppState::reconcile`] 同一条收尾口径）：
    /// 1. [`AppState::rescan_recovery`]（S1）**无条件**重扫门禁——事实刚变，
    ///    被判成终态/已作废的记录不再挡计时，快照必须跟着走；
    /// 2. **当且仅当被作废的会话正是协调器此刻镜像的那条**时 `Coordinator::refresh_committed_session`
    ///    刷新它（Ruling 13）。这里通常**就是**镜像那条（用户正在计时时作废它），
    ///    刷新后 `live.state` 停在 `discarded`——与 `finish` 之后停在 `finished`
    ///    **完全同一口径**：`live` 只表示「本 run 最后装载过哪条会话」，
    ///    不是「正在计时」，所以**不要**把 `live` 清成 `None`。
    ///
    /// 两处失败都映射 [`AppError::RecoveryRequired`]（P2 的提交后约定）：事务已经落库，
    /// 缺的是「让内存与事实重新对上」。
    ///
    /// 与 [`AppState::reconcile`] 同样：两步**互相独立**（P3 终审 I1），重扫失败不得
    /// 把镜像刷新一起跳过——这里被作废的通常正是镜像那条。
    pub fn discard_session(
        &mut self,
        env: WriteEnvelope,
        req: DiscardSessionRequest,
    ) -> Result<WriteOutcome<HistoryEditReport>, AppError> {
        let now = self.coordinator.wall_ms()?;
        let session_id = req.session_id.clone();
        let outcome = {
            let AppState { db, .. } = self;
            crate::services::recovery::discard_session(db, env, req, now)?
        };

        // 提交之后的第一步：门禁重算（失败 ⇒ 标记挡住计时，旧快照不动）。
        let rescan = self.rescan_recovery();
        // 第二步：条件化镜像刷新——**不能在重扫失败时被跳过**。
        self.refresh_mirror_if_live(&session_id)?;
        rescan?;
        Ok(outcome)
    }

    /// 任务状态编排的命令入口（P3 S2）。`env` 的版本位是**任务**版本。
    ///
    /// 整条链路都在**同一把锁**里跑（命令层在 `AppGuard` 内调用它，这就是「同一串行
    /// 边界」的保证）：服务自己完成「只读预检 → S3 取样与检测 → 一个用户事务
    /// （会话联动 + 任务跃迁 + 审计 + 恰好一次 `revision`）→ 提交后按需重建镜像」。
    ///
    /// 包装只把三个字段解构出来，**不额外取时间、不额外取样**：提交后收尾要用样本与
    /// 归属终点，两者都来自 S3 的那一次采样（R10：`report.revision`/`data_epoch`
    /// 只来自那次写事务，`rebuild_from_committed` 的返回值不进 DTO）。
    pub fn transition_task(
        &mut self,
        env: WriteEnvelope,
        req: TransitionTaskRequest,
    ) -> Result<WriteOutcome<TaskTransitionReport>, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        crate::services::tasks::transition_task(db, coordinator, env, req)
    }

    /// 时钟校正的显式接受的命令入口（P3 S4，§0.3）。第六个瘦包装。
    ///
    /// 只把两个字段解构出来递给协调器：这条命令**不改会话事实**（写的是审计与版本），
    /// 所以既不需要取 `now`（`created_at` 取本次采样的挂钟），也不做提交后收尾
    /// ——不重扫门禁、**不刷新镜像**（R13：刷新以「被改动的会话正是 `live` 镜像的那条」
    /// 为条件，这里没有任何会话行被改动）。
    ///
    /// 失败保留标记：协调器只在审计提交之后清内存里的未接受标记，
    /// 所以这里原样把错误交出去，不做任何「顺手清一下」的补偿。
    pub fn accept_detected_clock_correction(
        &mut self,
        expected_data_epoch: &str,
    ) -> Result<ClockCorrectionAccepted, AppError> {
        let AppState {
            db, coordinator, ..
        } = self;
        coordinator.accept_detected_clock_correction(
            db,
            AcceptClockCorrectionRequest {
                expected_data_epoch: expected_data_epoch.to_string(),
            },
        )
    }

    /// 重试协调器的**故障态恢复事务**（P3 S12，§0.3）。`faulted` 的唯一生产出口。
    ///
    /// 为什么必须有这个包装：[`Coordinator::retry_recovery`] 是 `pub`，但
    /// `AppState.coordinator` 私有（同 [`AppState::db`] 的理由），所以服务层与命令层
    /// 都够不着它；而 `refuse_if_faulted` 会拒绝 `snapshot`/`tick`/`start`/`pause`/
    /// `resume`/`finish`/`heartbeat`/`system_pause` 等十个入口。
    ///
    /// 顺序是契约的一部分：
    /// 1. **先只读预检**（读事务 + `guard_epoch`，模式抄 `catalog::list_projects`）：
    ///    epoch 不一致直接 `DATA_EPOCH_MISMATCH`、**不进协调器**；
    /// 2. 再 [`Coordinator::retry_recovery`]——它成功提交那笔恢复事务**之后**才清
    ///    `faulted`（失败保留故障：坏事实不许在下一拍被当成好事实）；
    /// 3. **随后无条件** [`AppState::rescan_recovery`]（S1）——那笔系统事务可能刚把某个
    ///    会话推成 `recovering`，门禁字段必须跟着事实走。只调一个都闭环不了：
    ///    只调 S12，新出现的 `recovering` 不会立刻挡计时；只重扫，`refuse_if_faulted`
    ///    继续拒绝一切。返回的是**提交后**的权威快照。
    ///
    /// 第 3 步失败时本方法返回 [`AppError::RecoveryRequired`]（S1 的失败标记挡住计时，
    /// 旧快照原样保留）——那**不是**「协调器没恢复」：第 2 步已经提交、故障态已经清了，
    /// 不许对外宣称整体回滚。恢复出口是显式的再次重扫/重试成功，或安全的新 run。
    ///
    /// **epoch 口径（如实记下，不要假装它是事务内校验）**：第 1 步的预检与第 2 步内部
    /// 那笔恢复事务之间存在理论窗口——[`Coordinator::retry_recovery`] 的签名里没有
    /// `env`（P2 既有），与 P2 的异常事务同一口径。进程内由 `AppBoundary`/`lock_app`
    /// （D6 单锁）串行、跨进程由单实例锁，所以实际只可能来自 P6 的恢复/替换库
    /// （那会换 `data_epoch` 并要求重新握手）。
    ///
    /// **硬故障一律解不开**：单调钟倒退置真的 `faulted` 由协调器直接返回
    /// `RECOVERY_REQUIRED`（单调读数已失去本 run 的意义，只能新 run 安全重建）。
    /// **不做定时自动重试**：08 §1 的立场是「故障不能自己把证据擦掉」，
    /// 这条入口只由用户显式触发（P8 的 IPC 命令）。
    pub fn retry_recovery(&mut self, expected_data_epoch: &str) -> Result<TimerSnapshot, AppError> {
        // 维护态在**任何读取与事务之前**就挡住（2026-10-08 收口）：它是要重试一笔恢复事务的
        // 写入口，不能因为"用户显式触发"就绕过维护隔离。
        self.guard_writable()?;

        // 1. 只读预检：epoch 是**请求带来的**期望值，不与「读出来的当前值」自比。
        {
            let tx = self
                .db
                .connection()
                .unchecked_transaction()
                .map_err(map_sqlite)?;
            guard_epoch(&tx, expected_data_epoch)?;
            drop(tx);
        }

        // 2. 重试那笔恢复事务；成功提交后才清故障态。
        let faulted_before = self.timer_faulted();
        let retried = {
            let AppState {
                db, coordinator, ..
            } = self;
            coordinator.retry_recovery(db)
        };

        // **无论成败**都观察一次跃迁：成功 ⇒ 记一条「清除」；失败 ⇒ 值没变，零记录。
        // 为什么放在这里而不是只靠采样拍：S12 是文档写明的**清除入口**，用户点完「重试
        // 对账」日志里应当立刻有结论，不该等下一拍（生产是 1 秒）才发现。
        //
        // 挂钟**只从这次调用的结果里取**（`snapshot.as_of`），不额外采样：P3 的用例钉着
        // 「S12 全路径只取一次样本」，多一次 `clock.sample()` 就红；成功时一定有 `as_of`。
        let snapshot_wall_ms = retried.as_ref().ok().map(|snapshot| snapshot.as_of);
        self.observe_timer_availability(OBSERVER_ENTRY_RETRY, faulted_before, snapshot_wall_ms);
        let snapshot = retried?;

        // 3. 无条件重扫门禁（S1），事实刚变，快照必须跟着变。
        self.rescan_recovery()?;
        Ok(snapshot)
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

    // ② 打开库、（真的要迁移时）迁移前一致备份、迁移。新库在这里取得库身份
    //    （`migrate` 不写 `app_meta`）。
    //
    //    「库文件本来在不在」必须在 `Db::open` **之前**问：`open` 会把文件建出来。
    let db_existed = config.db_path.exists();
    let mut db = Db::open(&config.db_path)?;
    probe.step(StartupStep::DatabaseOpened);

    // 判据读 `PRAGMA user_version`（与 `migrate` 自己那条检查同源），**不是**
    // `meta::read_meta`：新库在 `migrate` 之前根本没有 `app_meta` 表，混用会把新库
    // 误判成「无需迁移」而静默跳过迁移。
    //
    // 首启先判：`user_version == 0` 的新库属于「需要迁移」，但它被挡下的原因与
    // 「版本相等」**不同**（没有可备份的事实），所以两条分支分开报。
    // 未来版本（`from_version > SCHEMA_VERSION`）落到最后一条：`migrate` 会拒绝它
    // （不尝试降级），而拿一份读不懂的库做备份没有意义。
    let from_version = current_version(db.connection())?;
    let backup = if !db_existed {
        PreMigrationBackup::NothingToBackUp
    } else if from_version < SCHEMA_VERSION {
        // 原语在 `services::backup`（Task 4a 整体搬走）：本入口只负责**编排与顺序**。
        // 备份目录在这里才解析——下面的参数是 `Option`，`None` 的缺省路径由原语自己算，
        // 所以首启与「版本相等」两条路径都不碰应用数据目录。
        backup::backup_before_migration(
            config.backup_dir.as_deref(),
            db.connection(),
            from_version,
            &*clock,
        )?;
        PreMigrationBackup::Taken
    } else {
        PreMigrationBackup::NotNeeded
    };
    probe.pre_migration_backup(backup);

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

    // 正式诊断日志的落点**只解析一次**（P6 Task 2b）：一份给状态跃迁（`AppState`），
    // 一份给采样线程的失活看门狗。同一条 `config.diagnostic_log` ⇒ 同一个文件；
    // 组合根那条启动探针也按同一个字段构造（`Diagnostics::from_optional_path`）。
    let diagnostics = Diagnostics::from_optional_path(config.diagnostic_log.clone());

    let app: SharedApp = Arc::new(AppBoundary {
        state: Mutex::new(AppState {
            db,
            coordinator,
            recovery: recovery.clone(),
            // 启动能走到这里就说明第 ④ 步的扫描成功了：没有「扫描失败」的遗留。
            recovery_scan_failed: false,
            // 维护态与退出意图都是**进程内**状态，启动时一定是「没有」。
            maintenance: None,
            shutting_down: false,
            // 计时可用性还没被观察过：启动后第一次采样说了算（它要是立刻不可用，
            // 就记 `startup.timer_unavailable` 点名）。
            timer_unavailable: None,
            // OS 事件路径还没失败过：事件源在启动之后才由组合根挂上（见 `start_system_events`）。
            system_boundary_errors: 0,
            // `None` = 关闭（显式路径构造的缺省），生产解析到应用数据目录。
            // 路径只在第一次真的记录时才创建文件（见 `platform::diagnostics`）。
            diagnostics: diagnostics.clone(),
        }),
        holder: Mutex::new(None),
    });
    let broadcaster = Arc::new(Broadcaster::new(sink));
    let sampling_errors = Arc::new(AtomicU64::new(0));
    let sampling = Scheduler::spawn_watched(
        config.sampling_interval_ms,
        {
            let app = Arc::clone(&app);
            let broadcaster = Arc::clone(&broadcaster);
            let errors = Arc::clone(&sampling_errors);
            move || sampling_action(&app, &broadcaster, &errors)
        },
        // **路 B 的出口**（P6 Task 2b 的看门狗）：线程在没有停止信号的情况下退出
        // ⇒ `on_tick` panic 展开（dev/test profile；release 是整进程 abort，见
        // `platform::scheduler` 的模块头）。这里只写一条诊断，不重启、不假装恢复。
        sampler_died_report(&diagnostics),
    );
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

/// 看门狗报告的**内容**（P6 Task 2b）：写成一处，用例可以直接核对那一行。
///
/// 与 `Scheduler::spawn_watched` 的分工：平台层只提供「线程在没有停止信号的情况下结束了」
/// 这个事实与**一次回调**，写什么、写到哪由服务层决定（`platform` 不认识业务）。
///
/// 回调**不得 panic**：它在 panic 展开过程中执行（见 `platform::scheduler` 的模块头）。
fn sampler_died_report(diagnostics: &Diagnostics) -> impl FnOnce() + Send + 'static {
    let diagnostics = diagnostics.clone();
    move || {
        diagnostics.record(
            EVENT_SAMPLER_DIED,
            &format!(
                "thread={} effect=ticks_stop_growing",
                crate::platform::scheduler::SAMPLER_THREAD_NAME
            ),
        );
    }
}

/// 采样驱动的一拍：**在串行边界内**取快照，有活动会话才广播。
///
/// 空闲（没有活动会话）时：不广播、不写库（`sample_tick` 的读不算写）。
/// 广播留在临界区内完成，所以「广播顺序 = 提交顺序」。
fn sampling_action(app: &SharedApp, broadcaster: &Broadcaster, errors: &AtomicU64) {
    let mut state = lock_app(app);
    // **维护态的唯一落点**（P6 Task 2a）：取锁之后第一句就问，维护态**整拍跳过**——
    // 含读。理由：① 换库窗口里 `db` 这个句柄正被关闭/替换，读它要么报错（平白污染
    // `sampling_errors`）要么读到已经作废的那个世界；② 采样的快照会广播给所有窗口，
    // 在"停止受理写入"的同时广播一份旧世界的 tick 自相矛盾；③ 只有真的停止取样，
    // 维护结束后的第一拍才会是"跨了维护窗口的长间隔"，由 P2 既有的长间隔规则处理。
    //
    // 维护态**不是错误**：这里直接返回，不涨 `errors`；`Scheduler::ticks` 照涨
    // （它只是"触发了几次"的诊断计数）。
    if !state.sampling_allowed() {
        return;
    }

    // **故障态的检测半边**（P6 Task 2b）：`faulted_before` 必须在调用**之前**读——
    // 归因分组名要用它区分「这一拍自己置的真」与「已经在故障态（别处置的真）」。
    let faulted_before = state.timer_faulted();
    let outcome = state.sample_tick();
    // 按**跃迁**记账：进入/清除各一条，持续期间零记录（故障态下每一拍都是 Err，
    // 逐拍记会 1 秒一条刷爆诊断）。注意它**不改变** `sampling_errors` 的口径。
    //
    // 挂钟优先用这一拍的快照（零成本）；失败拍没有快照，由观察点补一次读钟
    // （`Coordinator::wall_ms` 用完即弃、不喂检测器）。
    let snapshot_wall_ms = outcome.as_ref().ok().map(|snapshot| snapshot.as_of);
    state.observe_timer_availability(OBSERVER_ENTRY_SAMPLING, faulted_before, snapshot_wall_ms);

    match outcome {
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

// ─────────────────────────────────────────────────────────────────────────────
// OS 事件路径（P6 Task 2c）：与周期采样**并列的第二条**入口，不是同一条
// ─────────────────────────────────────────────────────────────────────────────

/// 平台事件到达之后走的路：**取锁 → 维护态判据 → 平台边界 → 有会话才广播**。
///
/// 顺序与 [`sampling_action`] 逐字对齐，只有两点不同（这正是「并列的第二条路径」的含义）：
///
/// - 采样走 `sample_tick`（心跳 + tick），本路径走 [`AppState::system_boundary`]
///   （→ `Coordinator::system_pause`）；
/// - 取锁用 `lock_app` **排队等**，不是 `try_lock`：事件到达时锁被别人握着是**正常**的，
///   丢弃一次锁屏通知会让「锁屏即暂停」（R-02）静默落空。拿到锁之后按**当时**的状态
///   重新判定——所以维护态是「取锁之后」才判的，不是事件到达时判的。
///
/// 失败只记账（[`AppState::system_boundary_errors`] + 一条诊断），不 panic：
/// 事件线程死了就没有下一次通知，而周期采样在另一条线程上照常。
pub fn system_events_action(app: &SharedApp, broadcaster: &Broadcaster, event: SystemEvent) {
    let mut state = lock_app(app);

    // **维护态的唯一判据**（与周期采样同一处，P6 Task 2a）：维护态**整条事件跳过**。
    // 理由与采样那一拍相同——换库窗口里 `db` 正被关闭/替换，而且此时放行一次写入
    // 就把「封锁写入」戳出一个洞。
    if !state.sampling_allowed() {
        state.diagnostics().record(
            EVENT_SYSTEM_EVENT_SKIPPED,
            &format!("kind={}", event.kind.as_str()),
        );
        return;
    }

    let boundary = event_boundary(event);
    // 失败已经在 `system_boundary` 里计数并记了诊断（含协调器故障态那条恢复语义），
    // 这里不重复记、也不 panic：事件线程死了就没有下一次通知，采样在另一条线程上照常。
    if let Ok(snapshot) = state.system_boundary(boundary) {
        if snapshot.session_id.is_some() {
            // 与采样同一条出口：广播在临界区内完成，顺序 = 提交顺序。
            broadcaster.emit(EventEnvelope::timer_tick(&snapshot));
        }
    }
}

/// 事件 → 交给协调器的边界样本（**唯一一张表**，别在别处再判一次）。
///
/// - `Locked` / `Suspending` 是**离开边界**：平台事实就是「工作到此为止」，把事件时刻的
///   样本交给 P2；样本拿不到（`None`）就按「边界未知」处理。
/// - `Unlocked` / `Resumed` / `TimeChanged` **不是**离开边界 ⇒ 交出 `None`。P2 的
///   `system_pause(None)` 语义是「边界未知/晚到」：有 running 会话时走 `recovering` +
///   待确认（**不会**把锁屏/改时的跨度静默算成工时），空闲时只是一次只读观察；
///   **唤醒不自动继续**（本路径永不调 `start`/`resume`）。
///   把返回边界当成离开边界是错的：那会把锁屏那段时间算成工时。
///
/// 本函数**不判断时钟**（不新增任何阈值/规则），只是把「哪个通知是哪类边界」写在这里一处。
fn event_boundary(event: SystemEvent) -> Option<ClockSample> {
    match event.kind {
        SystemEventKind::Locked | SystemEventKind::Suspending => event.boundary,
        SystemEventKind::Unlocked | SystemEventKind::Resumed | SystemEventKind::TimeChanged => None,
    }
}

/// 组合根的事件源接线：起事件线程，注册**失败只记诊断**（不 panic、不假装成功）。
///
/// 为什么在这里而不是在组合根里写这几行：①「注册失败 ⇒ 记诊断」这条口径要与
/// 「意外结束 ⇒ 记诊断」写在**同一处**，否则两处文案迟早漂移；②这几行因此可以用
/// 注入的事件源直接测（真 `lib.rs` 的接线要 Tauri 运行时，本仓没有启用那个 feature）。
/// `make_source` 仍由组合根给——事件源要的是与协调器**同源**的时钟，而那份时钟在组合根手里。
///
/// 收的是**工厂**而不是现成的事件源：平台对象（窗口句柄之类）只在创建它们的线程上有效，
/// 而这里正是「哪条线程」的唯一定义处（事件线程由 [`system_events::spawn_watched`] 起）。
/// 工厂闭包只捕获时钟这类可跨线程的值，所以它自己是 `Send`；成品事件源从不跨线程移动。
///
/// 返回 `Err` 只是把原因交回上层（组合根另外打一行 `eprintln!`，与托盘/唤醒接收同一姿势）；
/// **周期采样不受影响**：它在另一条线程上，本函数不碰它。
pub fn start_system_events<S>(
    make_source: S,
    alive: Arc<AtomicBool>,
    app: SharedApp,
    broadcaster: Arc<Broadcaster>,
    diagnostics: &Diagnostics,
) -> std::io::Result<()>
where
    S: FnOnce() -> Box<dyn SystemEventSource> + Send + 'static,
{
    let died = diagnostics.clone();
    let result = system_events::spawn_watched(
        make_source,
        alive,
        move |event| system_events_action(&app, &broadcaster, event),
        move || {
            died.record(
                EVENT_SYSTEM_EVENTS_DIED,
                &format!("thread={}", system_events::EVENT_THREAD_NAME),
            );
        },
    );

    if let Err(error) = &result {
        diagnostics.record(EVENT_SYSTEM_EVENTS_UNAVAILABLE, &format!("detail={error}"));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生产侧看门狗报告的那一行必须**逐字**可核对。
    ///
    /// 为什么要有这条：[`Scheduler::spawn_watched`] 的契约只是「回调被调用一次」，
    /// 写什么完全在服务层（见 [`sampler_died_report`]）；`tests/periodic_sampling.rs`
    /// 注入的是**它自己**的闭包，核不到生产这一份。
    #[test]
    fn the_production_sampler_died_report_writes_one_named_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.log");
        let sink = Diagnostics::to_file(&path);

        sampler_died_report(&sink)();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "event=sampler.died_unexpectedly thread=worktrace-sampler effect=ticks_stop_growing\n"
        );
    }

    /// 关闭的落点上报告是**空操作**（不 panic、不建文件）——回调在展开过程中执行，
    /// 它绝不能出问题。
    #[test]
    fn the_production_sampler_died_report_is_a_no_op_when_the_sink_is_disabled() {
        sampler_died_report(&Diagnostics::disabled())();
    }
}
