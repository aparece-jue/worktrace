//! 串行协调器（P2 Task 1）。
//!
//! **进程内唯一**持有计时内存状态的地方。所有改状态的方法取 `&mut self`，
//! 于是借用检查器保证同一时刻只有一条路径在动它；跨进程/跨命令的串行由 P7 的
//! 单实例与调度接线提供（单实例只保证「只有一个进程」，**不替代进程内串行**）。
//!
//! 对外只有两类入口：
//! - **查询**（[`Coordinator::snapshot`] / [`Coordinator::tick`]）：自己也取一次采样，
//!   不走 `&self` 绕过检测；
//! - **命令**（Task 3）：先校验请求，再由协调器采样——**调用方不得预先传入采样**，
//!   否则就能「先采样再校验请求」，违反总纲 §9。

use rusqlite::{Connection, OptionalExtension};

use crate::domain::interval::IntervalFacts;
use crate::domain::session::{SessionMode, SessionState, TimerBudget, TimerKind};
use crate::domain::task::{TaskStatus, TransitionCause};
use crate::error::AppError;
use crate::platform::clock::{Clock, ClockSample};
use crate::storage::checkpoint_repo::{self, Checkpoint};
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::{guard_epoch, guard_row_version};
use crate::storage::meta::{bump_revision, require_meta};
use crate::storage::session_repo::{self, SessionStateUpdate};
use crate::storage::task_repo;
use crate::storage::time_edit_repo::{self, TimeEdit};

use super::anchor::{AnchorState, SampleVerdict};
use super::primitives::{end_session_in_tx, EndSessionFacts};
use super::snapshot::TimerSnapshot;

/// 一次 `start` 请求。
///
/// **公开入口只收请求，不收采样**：先校验请求，再由协调器采样。若让调用方传采样，
/// 它就能「先采样再校验」，而总纲 §9 明确要求「失败直接拒绝用户命令，
/// 不借该无效请求采样或写异常事实」。
#[derive(Debug, Clone)]
pub struct StartRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub task_expected_version: i64,
    pub mode: SessionMode,
    pub timer_kind: TimerKind,
    pub target_duration_ms: Option<i64>,
    /// 时钟的预期采样间隔；用于识别挂起。
    pub expected_interval_ms: i64,
}

/// 一次 `pause` / `finish` 请求。
#[derive(Debug, Clone)]
pub struct SessionRequest {
    pub expected_data_epoch: String,
    pub session_id: String,
    pub session_expected_version: i64,
}

/// 一次 `resume` 请求。**两份版本**：任务与会话各自有自己的并发版本。
#[derive(Debug, Clone)]
pub struct ResumeRequest {
    pub expected_data_epoch: String,
    pub task_id: String,
    pub task_expected_version: i64,
    pub session_id: String,
    pub session_expected_version: i64,
}

/// 命令成功后的产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub snapshot: TimerSnapshot,
    /// 本次事务后的权威 `revision`。前端据此丢弃过期响应。
    pub revision: i64,
    /// 请求目标任务的提交后版本（快照可能指向另一条仍在运行的会话）。
    pub task_version: i64,
}

/// 协调器在内存里持有的会话事实。**每一条都能在库里找到对应行**——
/// 它不是第二份真相源，只是避免每次查询都重新聚合的缓存。
#[derive(Debug, Clone)]
pub struct LiveSession {
    pub id: String,
    /// 持久化会话归属的运行代次；旧 running 行必须先由启动扫描恢复。
    pub run_id: String,
    pub row_version: i64,
    pub state: SessionState,
    pub budget: TimerBudget,
    /// 已确认可信闭合区间的时长之和。
    pub closed_trusted_ms: i64,
    /// 当前开放区间（可信时才有）。`(interval_id, attributed_start)`。
    pub open_interval: Option<(String, i64)>,
}

/// 计时协调器。
pub struct Coordinator {
    clock: Box<dyn Clock + Send>,
    run_id: String,
    tick_seq: u64,
    /// 归属基线 + 采样检测状态。`None` 表示尚未建立（此时算不出暂计）。
    anchor_state: Option<AnchorState>,
    /// 最近一次判定的结果。命令/查询/系统事件都经同一条路径，所以这里是
    /// 「上一个人看到的事实可不可信」的唯一出口。
    last_verdict: SampleVerdict,
    /// **最后一次成功持久化的检查点**。异常分割的可信前缀由它决定——
    /// 内存里的「可信点」不能当恢复事实（计划原文）。
    last_checkpoint: Option<Checkpoint>,
    /// 上次写检查点时的单调读数，用于判断是否到了 30 秒。
    last_checkpoint_monotonic: Option<i64>,
    /// **故障态**：异常事务失败后置真。
    ///
    /// 为什么需要它：异常事务失败时事务已回滚、内存也没动，但检测器**已经消费掉了那一拍
    /// 采样**——下一拍的增量会从「异常那一拍」起算，于是再判就正常了。若不锁住，一次
    /// 失败的恢复会让坏事实在下一拍被当成好事实接受。置真后在成功重建之前所有入口一律
    /// 拒绝（计划原文：「明确进入故障处理」）。
    faulted: bool,
    /// 已检测但未接受的墙钟校正；不能因长期容差增长而自动消失。
    unaccepted_clock_correction: bool,
    live: Option<LiveSession>,
}

impl Coordinator {
    pub fn new(clock: Box<dyn Clock + Send>, run_id: impl Into<String>) -> Self {
        Self {
            clock,
            run_id: run_id.into(),
            tick_seq: 0,
            anchor_state: None,
            last_verdict: SampleVerdict::Trusted,
            last_checkpoint: None,
            last_checkpoint_monotonic: None,
            faulted: false,
            unaccepted_clock_correction: false,
            live: None,
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn tick_seq(&self) -> u64 {
        self.tick_seq
    }

    pub fn live(&self) -> Option<&LiveSession> {
        self.live.as_ref()
    }

    /// 最近一次采样的判定。
    pub fn last_verdict(&self) -> SampleVerdict {
        self.last_verdict
    }

    /// 建立归属基线。只在**没有可信基线**时调用（Task 2）。
    pub fn establish_anchor(&mut self, sample: ClockSample) {
        self.anchor_state = Some(AnchorState::establish(sample, HEARTBEAT_INTERVAL_MS));
    }

    /// 一次**成功**心跳后前移累计偏差的参照点。
    ///
    /// **不碰归属基线**——心跳每 30 秒一次，若让它重置基线，`A(M)` 会随心跳跳变，
    /// 区间起点随之漂移。改成这样是实测逼出来的：挂钟与单调钟以 10–14 ms/分钟单向
    /// 分叉，累计偏差若永远对着 run 起点算，两三小时后健康会话就会被误判为异常。
    pub fn reanchor_drift_on_heartbeat(&mut self, sample: ClockSample) {
        if let Some(st) = self.anchor_state.as_mut() {
            st.reanchor_drift_on_heartbeat(sample);
        }
    }

    /// 接受已确认的时钟校正；调用方必须先成功提交校正审计。
    /// 清除未接受标记并更新长期参照，不等于确认可疑工时。
    pub fn accept_clock_correction(&mut self, sample: ClockSample) {
        self.unaccepted_clock_correction = false;
        if let Some(st) = self.anchor_state.as_mut() {
            st.accept_clock_correction(sample);
        }
    }

    /// 显式重建归属基线；必须先关闭或隔离旧开放事实。
    /// 重建会改变 A(M)，不更新长期参照或清除未接受校正标记。
    pub fn reestablish_anchor(&mut self, sample: ClockSample) {
        match self.anchor_state.as_mut() {
            Some(st) => st.reestablish(sample),
            None => self.establish_anchor(sample),
        }
    }

    /// 从持久化事实装载会话的内存镜像。
    ///
    /// 用 P1 已有的 `session_repo::intervals_of_session` 分类区间：只有
    /// **可信闭合**的进 `closed_trusted_ms`，只有 `running` 才认开放区间。
    /// 待确认、已作废、以及非 running 状态下残留的开放区间**都不计入**。
    pub fn load_session(&mut self, conn: &Connection, session_id: &str) -> Result<(), AppError> {
        let row = session_repo::get_session(conn, session_id)?.ok_or_else(|| AppError::Domain {
            detail: "no such session".into(),
        })?;

        let mut closed_trusted_ms: i64 = 0;
        let mut open: Option<(String, i64)> = None;
        for iv in session_repo::intervals_of_session(conn, session_id)? {
            let facts = IntervalFacts {
                range: crate::domain::interval::IntervalRange::new(
                    iv.started_at,
                    iv.ended_at.unwrap_or(iv.started_at),
                )?,
                duration_ms: iv.duration_ms,
                needs_review: iv.needs_review,
                voided: iv.voided_at.is_some(),
            };
            if facts.counts_as_confirmed() {
                closed_trusted_ms += iv.duration_ms.unwrap_or(0);
            }
            // 开放区间只在 running 下才算「正在计时」。
            if iv.ended_at.is_none() && iv.voided_at.is_none() && row.state.allows_open_interval() {
                open = Some((iv.id, iv.started_at));
            }
        }

        let budget = TimerBudget {
            kind: row.timer_kind,
            target_duration_ms: row.target_duration_ms,
        };

        self.last_checkpoint = match open.as_ref() {
            Some((id, _)) => checkpoint_repo::latest(conn, id)?,
            None => None,
        };
        self.last_checkpoint_monotonic = None;
        self.live = Some(LiveSession {
            id: row.id,
            run_id: row.run_id,
            row_version: row.row_version,
            state: row.state,
            budget,
            closed_trusted_ms,
            open_interval: open,
        });
        Ok(())
    }

    /// 故障态下拒绝一切入口。成功重建（[`Coordinator::rebuild_from_committed`]）才解锁。
    pub fn refuse_if_faulted(&self) -> Result<(), AppError> {
        if self.faulted {
            return Err(AppError::RecoveryRequired);
        }
        Ok(())
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    /// 观察一次采样并记录判定。**每个入口只调一次**——同一个采样看两次，
    /// 第二次的增量恒为 0，会把刚判出来的异常覆盖成 `Trusted`。
    fn observe(&mut self, sample: ClockSample) -> SampleVerdict {
        let v = match self.anchor_state.as_mut() {
            Some(st) => st.observe(sample),
            None => SampleVerdict::Trusted,
        };
        self.last_verdict = v;
        v
    }

    /// 查询快照。**自己也取一次采样**，不走 `&self` 绕过检测。
    /// 查询快照。**自己也取一次采样**，不走 `&self` 绕过检测。
    ///
    /// 检测到异常时**先提交独立系统恢复事务**，再返回该事务之后的权威快照——
    /// 所以这次查询确实写了库，调用方不能宣称「查询全程只读」（计划原文）。
    pub fn snapshot(&mut self, db: &mut Db) -> Result<TimerSnapshot, AppError> {
        self.refuse_if_faulted()?;
        let sample = self.read_sample(db)?;
        let verdict = self.observe(sample);
        if verdict.needs_recovery() {
            return self.handle_anomaly(db, sample, verdict);
        }
        self.build(db.connection(), sample, false)
    }

    /// 推进一步。与 [`Coordinator::snapshot`] 的唯一区别是 `tick_seq` 前进一格。
    /// 推进一步。与 [`Coordinator::snapshot`] 的唯一区别是 `tick_seq` 前进一格。
    pub fn tick(&mut self, db: &mut Db) -> Result<TimerSnapshot, AppError> {
        self.refuse_if_faulted()?;
        let sample = self.read_sample(db)?;
        let verdict = self.observe(sample);
        if verdict.needs_recovery() {
            return self.handle_anomaly(db, sample, verdict);
        }
        self.build(db.connection(), sample, true)
    }

    /// 客户端手上的会话版本是否已经过期。
    ///
    /// `00 §5`：旧状态生成的 tick **即使序号较新也不能覆盖**暂停/切换后的展示。
    /// 前端据此丢弃并改拉一次完整快照，而不是自行推导状态跃迁。
    pub fn is_stale_tick(&self, session_id: &str, session_version: i64) -> bool {
        match &self.live {
            None => true,
            Some(l) => l.id != session_id || l.row_version != session_version,
        }
    }

    /// 事实与展示值由**同一次采样**产出。
    fn build(
        &mut self,
        conn: &Connection,
        sample: ClockSample,
        advance_tick: bool,
    ) -> Result<TimerSnapshot, AppError> {
        let meta = require_meta(conn)?;

        // 检测**不在这里**做：入口已经 `observe` 过一次，结果在 `last_verdict`。
        let as_of = match self.anchor_state.as_ref() {
            Some(st) => st.attribute(sample.monotonic_ms),
            // 没有基线时只能退回采样本身的挂钟值。取值仍来自**同一次采样**，
            // 不会引入第二个瞬间。
            None => sample.wall_ms,
        };

        if advance_tick {
            // tick_seq 当前 run 内递增，**新会话不清零**；只有新 run 才重置。
            self.tick_seq += 1;
        }

        let Some(live) = self.live.clone() else {
            return Ok(TimerSnapshot::idle(
                meta.data_epoch,
                meta.revision,
                self.run_id.clone(),
                self.tick_seq,
                as_of,
            ));
        };

        // 暂计只加**当前可信开放区间**：非 running、没有基线、或没有开放区间都不加。
        let live_ms = match (&self.anchor_state, &live.open_interval, live.state) {
            (Some(a), Some((_, started_at)), SessionState::Running) => {
                // 基线倒退时暂计会是负数——截到 0，异常由 Task 2 检测后分割，
                // 不在这里把负数当成工时。
                a.attribute(sample.monotonic_ms)
                    .saturating_sub(*started_at)
                    .max(0)
            }
            _ => 0,
        };

        // 待确认段与已确认工时**分列**：它还不是工时，混在一起会让人以为算上了。
        let pending = self.pending_ms_of(conn, &live.id)?;
        let active_ms = live.closed_trusted_ms + live_ms;
        Ok(TimerSnapshot {
            data_epoch: meta.data_epoch,
            revision: meta.revision,
            run_id: self.run_id.clone(),
            session_id: Some(live.id),
            session_version: Some(live.row_version),
            tick_seq: self.tick_seq,
            as_of,
            active_ms,
            pending_ms: if pending > 0 { Some(pending) } else { None },
            state: Some(live.state),
            timer_kind: Some(live.budget.kind),
            remaining_ms: live.budget.remaining_ms(active_ms),
            overtime_ms: live.budget.overtime_ms(active_ms),
        })
    }
}

/// 心跳间隔（08 §1：约每 30 秒）。它**只控制检查点频率**，
/// 绝不能用来提前跳过异常检测——异常必须每一拍都看。
pub const HEARTBEAT_INTERVAL_MS: i64 = 30_000;

/// 只读地构造一个计时类型，供测试与 P7 的展示层复用。
pub fn timer_kind_of(state: &LiveSession) -> TimerKind {
    state.budget.kind
}

// ─────────────────────────────────────────────────────────────────────────────
// 命令（Task 3）
//
// 统一顺序（总纲 §9）：
//   ① 校验请求（epoch / 存在性 / 版本）——失败**直接拒绝，不采样、不写任何东西**
//   ② 取一次采样并检测
//   ③ 有异常 → 返回 RECOVERY_REQUIRED，不执行原意图（恢复事务由 Task 4 落）
//   ④ 正常路径 → 一个业务事务里再校验一次、调用仓储原语、恰好加一次 revision
//   ⑤ 提交成功后才应用内存
// ─────────────────────────────────────────────────────────────────────────────

impl Coordinator {
    /// 开始一次会话。
    ///
    /// 「理清」在这里是**两步**：02 §5 里 `Inbox → Doing` 不合法，必须
    /// `Inbox → Ready → Doing`。两次跃迁各自的 `expected_row_version` 按前一步的结果递进。
    pub fn start(&mut self, db: &mut Db, req: StartRequest) -> Result<CommandOutcome, AppError> {
        // 故障态优先于一切：连请求校验都不做，避免给出「版本冲突」这种会让人重试的码。
        self.refuse_if_faulted()?;

        if req.mode != SessionMode::Foreground {
            return Err(AppError::Domain {
                detail: "当前版本仅支持前台工作会话。".into(),
            });
        }
        // ① 校验请求——此阶段绝不采样
        {
            let conn = db.connection();
            guard_epoch_ro(conn, &req.expected_data_epoch)?;
            guard_row_version_of_ro(conn, "task", &req.task_id, req.task_expected_version)?;
        }

        // ② 采样 + 检测。命令入口**自己**采样，调用方不得预先传入。
        //    异常时先提交恢复事务再拒绝——不执行原意图（总纲 §9）。
        let sample = self.sample_and_detect(db)?;
        // **本 run 还没有过会话**时基线没有连续性要保：没有任何开放事实挂在旧归属上，
        // 长期参照的「进程生命期」语义也还没有可牵连的工时。用当前样本整体重定。
        //
        // 判据必须包含 `live.is_none()`，不能仅检查 `anchor_state.is_none()`：run 初始化就会
        // 建立基线（08 §1 对 lifetime_ref 的要求），此时若发生墙钟跳变，
        // `try_handle_anomaly` 在没有会话时什么都不做，而 `anchor_state.is_none()` 又为假
        // ——新会话的 `started_at` 会拿旧基线算，整整偏出跳变量（NTP 步进时是几分钟/
        // 几小时，之后按日期分桶的统计跟着错）。
        if self.anchor_state.is_none() || self.live.is_none() {
            self.establish_anchor(sample);
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let interval_id = uuid::Uuid::new_v4().to_string();
        let attributed_start = self.attribute(sample);
        let now_wall = sample.wall_ms;

        // ④ 一个业务事务
        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        guard_epoch(&tx, &req.expected_data_epoch)?;

        require_active_project(&tx, &req.task_id)?;
        // 前台槽位是**可预期的领域冲突**，在业务事务内先判；唯一索引仍是兜底。
        session_repo::require_no_running_foreground(&tx, None)?;
        session_repo::require_available_human_start(&tx, attributed_start)?;
        // 首次 start 冻结估时基准（判据是「还没有任何会话」，见 task_repo）
        let frozen = task_repo::freeze_baseline_estimate(
            &tx,
            &req.task_id,
            req.task_expected_version,
            now_wall,
        )?;
        let mut version = match frozen {
            task_repo::FreezeOutcome::Frozen { new_version, .. } => new_version,
            task_repo::FreezeOutcome::AlreadyFrozen { .. } => req.task_expected_version,
        };

        // 理清：Inbox/Clarifying → Ready → Doing；Ready → Doing；Doing 保持
        let current = task_repo::get_task(&tx, &req.task_id)?.ok_or_else(|| AppError::Domain {
            detail: "任务不存在。".into(),
        })?;
        if current.status == TaskStatus::Inbox || current.status == TaskStatus::Clarifying {
            let clarified = task_repo::transition_task(
                &tx,
                &req.task_id,
                version,
                TaskStatus::Ready,
                TransitionCause::User,
                now_wall,
            )?;
            version = clarified.row_version;
        }
        if task_repo::get_task(&tx, &req.task_id)?.map(|t| t.status) != Some(TaskStatus::Doing) {
            let started = task_repo::transition_task(
                &tx,
                &req.task_id,
                version,
                TaskStatus::Doing,
                TransitionCause::User,
                now_wall,
            )?;
            version = started.row_version;
        }
        let _ = version;

        session_repo::create_session(
            &tx,
            &session_id,
            &req.task_id,
            &self.run_id,
            req.mode,
            req.timer_kind,
            req.target_duration_ms,
            attributed_start,
            &interval_id,
        )?;

        checkpoint_repo::write(
            &tx,
            &Checkpoint {
                interval_id: interval_id.clone(),
                run_id: self.run_id.clone(),
                wall_at: now_wall,
                attribution_at: attributed_start,
                elapsed_ms: 0,
            },
        )?;

        let _revision = bump_revision(&tx)?;
        tx.commit().map_err(map_sqlite)?;
        // 提交之后的失败一律走恢复语义，不给可重试错误——事务已经落库了。
        self.rebuild_from_committed(db.connection(), &session_id, sample)
    }
}

impl Coordinator {
    /// **提交之后的收尾**：应用内存、生成响应。
    ///
    /// 关键约定（计划原文）：事务已经落库了，所以这里失败**不得返回普通可重试失败**
    /// ——客户端拿到可重试错误就会重发，而重发会**重复创建**（第二次 start 会开出
    /// 第二个会话）。一律映射为 [`AppError::RecoveryRequired`]：语义是
    /// 「状态已变，去按已提交事实对账」，不是「再试一次」。
    ///
    /// 映射前会尽量从**已提交事实**重建一次运行态；重建也失败就清掉 live，
    /// 不让旧内存冒充已提交状态，交给 P3 的对账流程接手。
    pub fn rebuild_from_committed(
        &mut self,
        conn: &Connection,
        session_id: &str,
        sample: ClockSample,
    ) -> Result<CommandOutcome, AppError> {
        if self.faulted
            && session_repo::get_session(conn, session_id)
                .map_err(|_| AppError::RecoveryRequired)?
                .is_some_and(|s| s.state == SessionState::Running)
        {
            return Err(AppError::RecoveryRequired);
        }
        if self.load_session(conn, session_id).is_err() {
            self.faulted = true;
            self.live = None;
            return Err(AppError::RecoveryRequired);
        }
        if let Some(active) = session_repo::running_foreground(conn).map_err(|_| {
            self.faulted = true;
            AppError::RecoveryRequired
        })? {
            if active.id != session_id && self.load_session(conn, &active.id).is_err() {
                self.faulted = true;
                return Err(AppError::RecoveryRequired);
            }
        }
        // 所有重建和响应校验成功之后才解除故障态。
        let snapshot = match self.build(conn, sample, false) {
            Ok(s) => s,
            Err(_) => {
                self.faulted = true;
                return Err(AppError::RecoveryRequired);
            }
        };
        let revision = match require_meta(conn) {
            Ok(m) => m.revision,
            Err(_) => {
                self.faulted = true;
                return Err(AppError::RecoveryRequired);
            }
        };
        let task_version = session_repo::get_session(conn, session_id)
            .and_then(|session| session.ok_or(AppError::RecoveryRequired))
            .and_then(|session| task_repo::get_task(conn, &session.task_id))
            .and_then(|task| task.ok_or(AppError::RecoveryRequired))
            .map_err(|_| {
                self.faulted = true;
                AppError::RecoveryRequired
            })?
            .row_version;
        self.faulted = false;
        Ok(CommandOutcome {
            snapshot,
            revision,
            task_version,
        })
    }
}

/// 只读的 epoch 校验：命令的**第一阶段**用，此时还没有事务。
fn guard_epoch_ro(conn: &Connection, expected: &str) -> Result<(), AppError> {
    let actual: String = conn
        .query_row(
            "SELECT data_epoch FROM app_meta WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .map_err(map_sqlite)?;
    if actual != expected {
        return Err(AppError::DataEpochMismatch);
    }
    Ok(())
}

/// 只读的版本校验：同上，用于第一阶段。
fn guard_row_version_of_ro(
    conn: &Connection,
    table: &'static str,
    id: &str,
    expected: i64,
) -> Result<(), AppError> {
    let sql = format!("SELECT row_version FROM {table} WHERE id = ?1");
    let actual: Option<i64> = conn
        .query_row(&sql, [id], |r| r.get(0))
        .optional()
        .map_err(map_sqlite)?;
    match actual {
        None => Err(AppError::Domain {
            detail: "记录不存在。".into(),
        }),
        Some(v) => crate::storage::guards::guard_row_version(v, expected),
    }
}

/// 归属：`A(M)`。没有基线时使用同次采样墙钟，不能把进程单调读数当时间戳。
impl Coordinator {
    fn attribute(&self, sample: ClockSample) -> i64 {
        match self.anchor_state.as_ref() {
            Some(st) => st.attribute(sample.monotonic_ms),
            None => sample.wall_ms,
        }
    }
}

impl Coordinator {
    /// 暂停。用已验证的单调差闭合当前区间。
    pub fn pause(&mut self, db: &mut Db, req: SessionRequest) -> Result<CommandOutcome, AppError> {
        self.refuse_if_faulted()?;
        self.validate_session_request(db, &req)?;
        let sample = self.sample_and_detect(db)?;
        let attributed_end = self.attribute(sample);

        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        guard_epoch(&tx, &req.expected_data_epoch)?;

        end_session_in_tx(
            &tx,
            &EndSessionFacts {
                run_id: self.run_id.clone(),
                session_id: req.session_id.clone(),
                expected_row_version: req.session_expected_version,
                attributed_end,
                sampled_end_wall_at: sample.wall_ms,
                target_state: SessionState::Paused,
            },
        )?;

        let _revision = bump_revision(&tx)?;
        tx.commit().map_err(map_sqlite)?;
        // 提交之后的失败一律走恢复语义，不给可重试错误——事务已经落库了。
        self.rebuild_from_committed(db.connection(), &req.session_id, sample)
    }

    /// 继续。**两份版本**：任务与会话各有自己的并发版本。
    ///
    /// 任务状态：`Doing` 保持、`Ready` 同事务转 `Doing`，其余一律拒绝——
    /// **不隐式解除等待、也不重开已结束的任务**。发现目标不行就报错让用户去改，
    /// 而不是替他做决定。
    pub fn resume(&mut self, db: &mut Db, req: ResumeRequest) -> Result<CommandOutcome, AppError> {
        self.refuse_if_faulted()?;
        {
            let conn = db.connection();
            guard_epoch_ro(conn, &req.expected_data_epoch)?;
            guard_row_version_of_ro(conn, "task", &req.task_id, req.task_expected_version)?;
            guard_row_version_of_ro(
                conn,
                "work_session",
                &req.session_id,
                req.session_expected_version,
            )?;
        }
        let session =
            session_repo::get_session(db.connection(), &req.session_id)?.ok_or_else(|| {
                AppError::Domain {
                    detail: "会话不存在。".into(),
                }
            })?;
        if session.task_id != req.task_id {
            return Err(AppError::Domain {
                detail: "任务与会话不匹配。".into(),
            });
        }
        let sample = self.sample_and_detect(db)?;
        // 同上（`start` 的注释）：本 run 还没有装载过会话时（典型是跨 run 恢复一个
        // 暂停会话），旧基线不属于这个 run，用当前样本整体重定。
        // 已装载会话但基线缺失也必须初始化，不能将单调读数误当归属时间。
        if self.anchor_state.is_none() || self.live.is_none() {
            self.establish_anchor(sample);
        }
        let attributed_start = self.attribute(sample);
        let interval_id = uuid::Uuid::new_v4().to_string();

        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        guard_epoch(&tx, &req.expected_data_epoch)?;

        let session =
            session_repo::get_session(&tx, &req.session_id)?.ok_or_else(|| AppError::Domain {
                detail: "会话不存在。".into(),
            })?;
        guard_row_version(session.row_version, req.session_expected_version)?;
        if session.state != SessionState::Paused {
            return Err(AppError::Domain {
                detail: "只有已暂停的会话可以继续。".into(),
            });
        }
        if session.task_id != req.task_id {
            return Err(AppError::Domain {
                detail: "任务与会话不匹配。".into(),
            });
        }
        if session.needs_review {
            return Err(AppError::RecoveryRequired);
        }
        require_active_project(&tx, &req.task_id)?;
        // 同上（`start` 的检查）：**排除目标自身**——要恢复的这个会话不算占用，
        // 占用它的是别人；唯一索引仍是兜底。
        session_repo::require_no_running_foreground(&tx, Some(&req.session_id))?;
        session_repo::require_available_human_start(&tx, attributed_start)?;
        // paused 且无待确认
        for iv in session_repo::intervals_of_session(&tx, &req.session_id)? {
            if iv.needs_review && iv.voided_at.is_none() {
                return Err(AppError::RecoveryRequired);
            }
        }

        let task = task_repo::get_task(&tx, &req.task_id)?.ok_or_else(|| AppError::Domain {
            detail: "任务不存在。".into(),
        })?;
        guard_row_version(task.row_version, req.task_expected_version)?;
        let mut version = task.row_version;
        if task.status == TaskStatus::Ready {
            version = task_repo::transition_task(
                &tx,
                &req.task_id,
                version,
                TaskStatus::Doing,
                TransitionCause::User,
                sample.wall_ms,
            )?
            .row_version;
        } else if task.status != TaskStatus::Doing {
            return Err(AppError::Domain {
                detail: "任务不在可继续的状态。".into(),
            });
        }
        let _ = version;

        // 先切 run_id 与状态，再开新区间——顺序与计划一致
        session_repo::update_session_state(
            &tx,
            &req.session_id,
            session.row_version,
            SessionState::Running,
            SessionStateUpdate {
                run_id: Some(&self.run_id),
                ..Default::default()
            },
        )?;
        session_repo::open_interval(&tx, &interval_id, &req.session_id, attributed_start)?;
        checkpoint_repo::write(
            &tx,
            &Checkpoint {
                interval_id: interval_id.clone(),
                run_id: self.run_id.clone(),
                wall_at: sample.wall_ms,
                attribution_at: attributed_start,
                elapsed_ms: 0,
            },
        )?;

        let _revision = bump_revision(&tx)?;
        tx.commit().map_err(map_sqlite)?;
        // 提交之后的失败一律走恢复语义，不给可重试错误——事务已经落库了。
        self.rebuild_from_committed(db.connection(), &req.session_id, sample)
    }

    /// 结束。**可以从 `paused` 直接结束**（02 §3）。
    pub fn finish(&mut self, db: &mut Db, req: SessionRequest) -> Result<CommandOutcome, AppError> {
        self.refuse_if_faulted()?;
        self.validate_session_request(db, &req)?;
        let sample = self.sample_and_detect(db)?;
        let attributed_end = self.attribute(sample);

        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        guard_epoch(&tx, &req.expected_data_epoch)?;

        end_session_in_tx(
            &tx,
            &EndSessionFacts {
                run_id: self.run_id.clone(),
                session_id: req.session_id.clone(),
                expected_row_version: req.session_expected_version,
                attributed_end,
                sampled_end_wall_at: sample.wall_ms,
                target_state: SessionState::Finished,
            },
        )?;

        let _revision = bump_revision(&tx)?;
        tx.commit().map_err(map_sqlite)?;
        // 提交之后的失败一律走恢复语义，不给可重试错误——事务已经落库了。
        self.rebuild_from_committed(db.connection(), &req.session_id, sample)
    }
}

impl Coordinator {
    /// 命令第一阶段：只校验请求，**不采样**。
    fn validate_session_request(&self, db: &Db, req: &SessionRequest) -> Result<(), AppError> {
        let conn = db.connection();
        guard_epoch_ro(conn, &req.expected_data_epoch)?;
        guard_row_version_of_ro(
            conn,
            "work_session",
            &req.session_id,
            req.session_expected_version,
        )
    }

    fn read_sample(&mut self, db: &mut Db) -> Result<ClockSample, AppError> {
        self.refuse_if_faulted()?;
        // 新 run 即使已有自己的基线，也不能解释旧 run 的开放工时。
        // 在采样/异常事务之前隔离，避免快照暂计或分割把停机时间变成事实。
        if self
            .live
            .as_ref()
            .is_some_and(|l| l.state == SessionState::Running && l.run_id != self.run_id)
        {
            return Err(AppError::RecoveryRequired);
        }
        match self.clock.sample() {
            Ok(sample) => Ok(sample),
            Err(_) => {
                if let Some(previous) = self.anchor_state.as_ref().and_then(|a| a.last()) {
                    self.last_verdict = SampleVerdict::Unavailable;
                    self.handle_anomaly(db, previous, SampleVerdict::Unavailable)?;
                } else {
                    self.faulted = true;
                }
                Err(AppError::RecoveryRequired)
            }
        }
    }

    /// 采样并检测。异常时**先提交独立系统恢复事务**，再返回 `RECOVERY_REQUIRED`
    /// ——原用户命令不执行，但恢复状态已经被落库（总纲 §9）。
    fn sample_and_detect(&mut self, db: &mut Db) -> Result<ClockSample, AppError> {
        let sample = self.read_sample(db)?;
        let verdict = self.observe(sample);
        if verdict.needs_recovery() {
            let was_running = self
                .live
                .as_ref()
                .is_some_and(|l| l.state == SessionState::Running);
            let _ = self.handle_anomaly(db, sample, verdict)?;
            // 本次检测也可能刚刚进入故障态，必须在原命令/统计继续前隔离。
            self.refuse_if_faulted()?;
            if was_running
                || (verdict.is_wall_clock_anomaly()
                    && self
                        .live
                        .as_ref()
                        .is_some_and(|l| l.state == SessionState::Recovering))
            {
                return Err(AppError::RecoveryRequired);
            }
        }
        if self.unaccepted_clock_correction {
            return Err(AppError::RecoveryRequired);
        }
        Ok(sample)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 心跳与异常原子跃迁（Task 4）
// ─────────────────────────────────────────────────────────────────────────────

impl Coordinator {
    /// 最后一次**成功持久化**的检查点。异常分割只看它，不看内存里的可信点。
    pub fn last_checkpoint(&self) -> Option<&Checkpoint> {
        self.last_checkpoint.as_ref()
    }

    /// 心跳：约每 30 秒写一次检查点。**不加 revision**（00 §5）。
    ///
    /// 返回是否真的写了。失败**不推进持久化标记**，所以下一次还会重试；
    /// 也**不会**把内存里的可信点当成已持久化的事实。
    pub fn heartbeat(&mut self, db: &mut Db) -> Result<bool, AppError> {
        self.refuse_if_faulted()?;
        let Some(live) = self.live.clone() else {
            return Ok(false);
        };
        if live.state != SessionState::Running {
            return Ok(false);
        }
        let Some((interval_id, started_at)) = live.open_interval.clone() else {
            return Ok(false);
        };
        let sample = self.sample_and_detect(db)?;
        let due = match self.last_checkpoint_monotonic {
            None => true,
            Some(prev) => sample.monotonic_ms - prev >= HEARTBEAT_INTERVAL_MS,
        };
        if !due {
            return Ok(false);
        }

        let attributed = self.attribute(sample);
        let cp = Checkpoint {
            interval_id: interval_id.clone(),
            run_id: self.run_id.clone(),
            wall_at: sample.wall_ms,
            attribution_at: attributed,
            elapsed_ms: (attributed - started_at).max(0),
        };

        // 心跳有自己的短事务，且**不加 revision**。
        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        checkpoint_repo::write(&tx, &cp)?;
        tx.commit().map_err(map_sqlite)?;
        // 提交之后的失败一律走恢复语义，不给可重试错误——事务已经落库了。
        self.last_checkpoint = Some(cp);
        self.last_checkpoint_monotonic = Some(sample.monotonic_ms);
        // 成功心跳前移累计偏差的**参照点**（不是归属基线）。
        self.reanchor_drift_on_heartbeat(sample);
        Ok(true)
    }

    /// 异常跃迁：**一次独立系统事务**保住可信前缀、把余段标成待确认、写审计、
    /// 会话置 `recovering`，`revision + 1`。随后返回提交后的**权威快照**。
    ///
    /// 这是总纲 §9 里那个「独立系统状态事务」：它**不是**用户命令的执行结果，
    /// 所以既不执行用户意图，也不因用户命令被拒而回滚。
    ///
    /// 幂等：已经 `recovering` 就返回现有结果，不重复分割、不重复写审计、不加版本。
    pub fn handle_anomaly(
        &mut self,
        db: &mut Db,
        sample: ClockSample,
        verdict: SampleVerdict,
    ) -> Result<TimerSnapshot, AppError> {
        match self.try_handle_anomaly(db, sample, verdict) {
            Ok(snapshot) => Ok(snapshot),
            Err(_) => {
                // 事务已在 drop 时回滚，内存也没应用任何未提交状态。
                // 但检测器已经吃掉了那一拍采样，所以必须**锁住**：
                // 在成功重建之前所有入口都回恢复语义，不让坏事实在下一拍被当成好事实。
                self.faulted = true;
                Err(AppError::RecoveryRequired)
            }
        }
    }

    /// 异常事务本体。失败由 [`Coordinator::handle_anomaly`] 统一兜底。
    /// 异常跃迁：**一次独立系统事务**保住可信前缀、把余段标成待确认、写审计、
    /// 会话置 `recovering`，`revision + 1`。随后返回提交后的**权威快照**。
    ///
    /// 这是总纲 §9 里那个「独立系统状态事务」：它**不是**用户命令的执行结果，
    /// 所以既不执行用户意图，也不因用户命令被拒而回滚。
    ///
    /// 幂等：已经 `recovering` 就返回现有结果，不重复分割、不重复写审计、不加版本。
    fn try_handle_anomaly(
        &mut self,
        db: &mut Db,
        sample: ClockSample,
        verdict: SampleVerdict,
    ) -> Result<TimerSnapshot, AppError> {
        let Some(live) = self.live.clone() else {
            // 没有会话也不能忽略进程级的单调钟硬故障。
            if matches!(verdict, SampleVerdict::MonotonicBackwards { .. }) {
                self.faulted = true;
            }
            return self.build(db.connection(), sample, false);
        };

        // 恢复记录幂等：不重复分割/写审计/增加版本，也不把重复通知当作新校正。
        if live.state == SessionState::Recovering {
            if verdict.is_wall_clock_anomaly() {
                self.unaccepted_clock_correction = true;
            }
            if verdict != SampleVerdict::Unavailable {
                self.reestablish_anchor(sample);
            }
            if matches!(verdict, SampleVerdict::MonotonicBackwards { .. }) {
                self.faulted = true;
            }
            return self.build(db.connection(), sample, false);
        }

        // 没有开放工时需要分割，但墙钟异常仍须先记录，不能允许 resume
        // 使用新归属却保留旧长期偏差，导致恢复成功后下一拍立即失败。
        if live.state != SessionState::Running {
            if verdict.is_wall_clock_anomaly() {
                let tx = db
                    .connection_mut()
                    .unchecked_transaction()
                    .map_err(map_sqlite)?;
                time_edit_repo::write(
                    &tx,
                    &TimeEdit {
                        id: uuid::Uuid::new_v4().to_string(),
                        session_id: live.id.clone(),
                        before_json: self.clock_references_json(),
                        after_json: serde_json::json!({
                            "sampled_wall_at": sample.wall_ms,
                            "sampled_monotonic_ms": sample.monotonic_ms,
                            "clock_correction_accepted": true,
                            "verdict": format!("{verdict:?}"),
                            "intervals_changed": false,
                        })
                        .to_string(),
                        reason: Some("wall clock anomaly without running work".into()),
                        created_at: sample.wall_ms,
                    },
                )?;
                // 暂停/恢复态的后续修改请求必须重新取得版本。终结态保持不变。
                if !live.state.is_terminal() {
                    session_repo::update_session_state(
                        &tx,
                        &live.id,
                        live.row_version,
                        live.state,
                        SessionStateUpdate::default(),
                    )?;
                }
                bump_revision(&tx)?;
                tx.commit().map_err(map_sqlite)?;
                self.load_session(db.connection(), &live.id)?;
                self.reestablish_anchor(sample);
                self.accept_clock_correction(sample);
            } else if verdict != SampleVerdict::Unavailable {
                self.reestablish_anchor(sample);
            }
            if matches!(verdict, SampleVerdict::MonotonicBackwards { .. }) {
                self.faulted = true;
            }
            return self.build(db.connection(), sample, false);
        }

        let candidate_end = self.attribute(sample);
        let trusted_until = self.last_checkpoint.as_ref().map(|c| c.attribution_at);
        let reason = match verdict {
            SampleVerdict::MonotonicBackwards { .. } => "monotonic clock went backwards",
            SampleVerdict::WallBackwards { .. } => "wall clock went backwards",
            SampleVerdict::Jumped { .. } => "clock jumped",
            SampleVerdict::Drifted { .. } => "cumulative clock drift",
            SampleVerdict::Suspended { .. } => "untrusted observation gap",
            SampleVerdict::Unavailable => "clock sample unavailable",
            SampleVerdict::Trusted => "not an anomaly",
        };

        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        let raw_wall = (verdict != SampleVerdict::Unavailable).then_some(sample.wall_ms);
        let before = session_repo::intervals_of_session(&tx, &live.id)?;
        let split =
            session_repo::split_for_anomaly(&tx, &live.id, trusted_until, candidate_end, raw_wall)?;

        time_edit_repo::write(
            &tx,
            &TimeEdit {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: live.id.clone(),
                before_json: serde_json::json!({
                    "open_interval": live.open_interval.as_ref().map(|(id, _)| id),
                    "trusted_until": trusted_until,
                    "clock_references": serde_json::from_str::<serde_json::Value>(&self.clock_references_json()).expect("serialized JSON"),
                    "intervals": before.iter().map(|i| serde_json::json!({
                        "id": i.id, "started_at": i.started_at, "ended_at": i.ended_at,
                        "duration_ms": i.duration_ms, "needs_review": i.needs_review,
                        "sampled_end_wall_at": i.sampled_end_wall_at,
                    })).collect::<Vec<_>>()
                })
                .to_string(),
                after_json: serde_json::json!({
                    "trusted_interval": split.trusted_interval_id,
                    "pending_interval": split.pending_interval_id,
                    "candidate_end": raw_wall.map(|_| split.candidate_end),
                    "sampled_wall_at": raw_wall,
                    "sampled_monotonic_ms": raw_wall.map(|_| sample.monotonic_ms),
                    "clock_correction_accepted": verdict.is_wall_clock_anomaly(),
                })
                .to_string(),
                reason: Some(reason.to_string()),
                created_at: sample.wall_ms,
            },
        )?;

        session_repo::update_session_state(
            &tx,
            &live.id,
            live.row_version,
            SessionState::Recovering,
            SessionStateUpdate {
                needs_review: Some(true),
                ..Default::default()
            },
        )?;

        let _revision = bump_revision(&tx)?;
        tx.commit().map_err(map_sqlite)?;

        // 提交后才重建内存：开放区间没了、状态是 recovering，于是 live 暂计自动停止、
        // 前台占用自动释放（`uq_running_foreground` 只约束 running）。
        // **不建立新的单调运行起点**。
        self.load_session(db.connection(), &live.id)?;
        self.last_checkpoint = None;
        self.last_checkpoint_monotonic = None;

        // **重建归属基线**。异常已经被承认并落库，旧基线的偏差（实测可达几十秒）会一直
        // 留在参照点里，导致此后每一次观察都判成累计漂移——那样恢复之后就再也开不了
        // 新会话了。旧开放事实已在上面分割完毕（前缀闭合、余段待确认），所以重建不会让
        // 任何未闭合区间与归属对不上。
        if verdict != SampleVerdict::Unavailable {
            self.reestablish_anchor(sample);
            // 仅墙钟异常的已提交审计允许接受校正；长间隔不得清除长期证据。
            if verdict.is_wall_clock_anomaly() {
                self.accept_clock_correction(sample);
            }
        }
        if matches!(verdict, SampleVerdict::MonotonicBackwards { .. }) {
            self.faulted = true;
        }
        self.build(db.connection(), sample, false)
    }

    fn clock_references_json(&self) -> String {
        let anchor = self.anchor_state.as_ref().map(|s| s.anchor());
        let short = self.anchor_state.as_ref().map(|s| s.drift_ref());
        let lifetime = self.anchor_state.as_ref().map(|s| s.lifetime_ref());
        let encode = |a: Option<super::anchor::Anchor>| {
            a.map(|a| {
                serde_json::json!({
                    "wall_at": a.wall_at, "monotonic_at": a.monotonic_at,
                })
            })
        };
        serde_json::json!({"attribution": encode(anchor), "short_term": encode(short),
            "lifetime": encode(lifetime)})
        .to_string()
    }

    /// 故障后重试原系统恢复事务，不能只装载 running 行就解除隔离。
    pub fn retry_recovery(&mut self, db: &mut Db) -> Result<TimerSnapshot, AppError> {
        if !self.faulted {
            return self.snapshot(db);
        }
        // 单调钟读数已失去同一 run 的意义，只能通过新 run 的安全启动恢复。
        if matches!(self.last_verdict, SampleVerdict::MonotonicBackwards { .. }) {
            return Err(AppError::RecoveryRequired);
        }
        if self.live.is_none() && session_repo::running_foreground(db.connection())?.is_some() {
            return Err(AppError::RecoveryRequired);
        }
        let sample = self
            .clock
            .sample()
            .map_err(|_| AppError::RecoveryRequired)?;
        let verdict = if self.last_verdict.needs_recovery() {
            self.last_verdict
        } else {
            SampleVerdict::Unavailable
        };
        let snapshot = self.handle_anomaly(db, sample, verdict)?;
        self.faulted = false;
        Ok(snapshot)
    }

    /// 平台已验证的离开边界；None 表示晚到或边界未知。唤醒后不自动继续。
    pub fn system_pause(
        &mut self,
        db: &mut Db,
        boundary: Option<ClockSample>,
    ) -> Result<TimerSnapshot, AppError> {
        self.refuse_if_faulted()?;
        let boundary_state = self.anchor_state;
        let previous = boundary_state.as_ref().and_then(|a| a.last());
        let sample = self.read_sample(db)?;
        // 只观察当前样本一次；历史 boundary 只作校验，不能推进 last。
        let verdict = self.observe(sample);
        let trusted = boundary.filter(|b| {
            previous.is_some_and(|p| {
                b.monotonic_ms >= p.monotonic_ms
                    && b.monotonic_ms <= sample.monotonic_ms
                    && b.wall_ms >= p.wall_ms
                    && b.wall_ms <= sample.wall_ms
                    // 用观察前的检测器副本复用三参照点规则，不推进真实 last。
                    // 平台可信边界可解释长间隔，但不能解释时钟跳变/漂移。
                    && boundary_state.is_some_and(|mut state| {
                        matches!(state.observe(*b), SampleVerdict::Trusted | SampleVerdict::Suspended { .. })
                    })
            })
        });
        let Some(live) = self
            .live
            .clone()
            .filter(|l| l.state == SessionState::Running)
        else {
            // 非运行状态也检测本次样本；只有记录后的墙钟异常可移动长期参照。
            if verdict.needs_recovery() {
                return self.handle_anomaly(db, sample, verdict);
            }
            return self.build(db.connection(), sample, false);
        };
        // 可信事件边界可以解释长间隔，不能覆盖硬故障或墙钟异常。
        if verdict.needs_recovery() && !matches!(verdict, SampleVerdict::Suspended { .. }) {
            return self.handle_anomaly(db, sample, verdict);
        }
        let Some(boundary) = trusted else {
            let verdict = SampleVerdict::Suspended {
                gap_ms: previous
                    .map(|p| sample.monotonic_ms - p.monotonic_ms)
                    .unwrap_or(0),
            };
            self.last_verdict = verdict;
            return self.handle_anomaly(db, sample, verdict);
        };
        let end = self.attribute(boundary);
        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        end_session_in_tx(
            &tx,
            &EndSessionFacts {
                run_id: self.run_id.clone(),
                session_id: live.id.clone(),
                expected_row_version: live.row_version,
                attributed_end: end,
                sampled_end_wall_at: boundary.wall_ms,
                target_state: SessionState::Paused,
            },
        )?;
        bump_revision(&tx)?;
        tx.commit().map_err(map_sqlite)?;
        // 旧开放事实已闭合，才允许建立唤醒后的映射。
        self.reestablish_anchor(sample);
        self.rebuild_from_committed(db.connection(), &live.id, sample)
            .map(|o| o.snapshot)
    }

    /// 待确认时长：所有 `needs_review=1` 且未作废的区间跨度之和。
    fn pending_ms_of(&self, conn: &Connection, session_id: &str) -> Result<i64, AppError> {
        let mut total = 0;
        for iv in session_repo::intervals_of_session(conn, session_id)? {
            if iv.needs_review && iv.voided_at.is_none() {
                if let Some(end) = iv.ended_at {
                    total += (end - iv.started_at).max(0);
                }
            }
        }
        Ok(total)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// P5 的统计采样接缝
// ─────────────────────────────────────────────────────────────────────────────

/// 一次统计采样。
///
/// **为什么需要专门的接缝**：统计要把「已确认闭合工时」和「当前开放区间的暂计」
/// 加起来，而这两者必须来自**同一个瞬间**。若先查闭合再查开放，中间夹了一次采样，
/// 同一段时间就会被算两遍（区间刚好在两次查询之间闭合时最明显）。
///
/// 这里保证三件事：
/// 1. 样本、区间分类、归属终点都出自**同一次调用**，也就是同一个串行边界；
/// 2. `closed_trusted_ms` 只累加**已闭合且可信**的区间，`live_ms` 只算**当前开放**那一段，
///    一遍回放分出来，**互斥**；
/// 3. `attributed_end` 是 `A(M)`——开放区间暂计到这里为止，调用方不要自己再取挂钟。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSample {
    pub run_id: String,
    pub session_id: Option<String>,
    pub session_version: Option<i64>,
    /// 当前开放区间 id。没有在计时的区间时为 `None`。
    pub open_interval_id: Option<String>,
    /// 归属终点 `A(M)`。
    pub attributed_end: i64,
    /// 已确认可信闭合工时。**不含** `live_ms`。
    pub closed_trusted_ms: i64,
    /// 当前开放区间的暂计。与 `closed_trusted_ms` **互斥**，不会重复计入。
    pub live_ms: i64,
    pub state: Option<SessionState>,
}

impl StatsSample {
    /// 两段之和。调用方要用总工时就用它，别自己加——加错了就是重复计入。
    pub fn total_ms(&self) -> i64 {
        self.closed_trusted_ms + self.live_ms
    }
}

impl Coordinator {
    /// 取一次统计采样。检测到异常时**先提交恢复事务**，再以恢复语义返回 Err——
    /// 统计不能基于不可信的事实。
    pub fn stats_sample(&mut self, db: &mut Db) -> Result<StatsSample, AppError> {
        // 与命令路径共用同一套「采样 → 检测 → 异常则先提交恢复事务再隔离」逻辑
        // （`sample_and_detect` 内部已含故障态检查与恢复事务）。
        let sample = self.sample_and_detect(db)?;
        // 跨 run 的旧 running 行尚未恢复，不能用墙钟补出开放区间工时。
        if self.anchor_state.is_none()
            && self
                .live
                .as_ref()
                .is_some_and(|l| l.state == SessionState::Running)
        {
            return Err(AppError::RecoveryRequired);
        }
        let attributed_end = self.attribute(sample);

        let Some(live) = self.live.clone() else {
            return Ok(StatsSample {
                run_id: self.run_id.clone(),
                session_id: None,
                session_version: None,
                open_interval_id: None,
                attributed_end,
                closed_trusted_ms: 0,
                live_ms: 0,
                state: None,
            });
        };

        // **一遍回放**分出两段：闭合可信的进 closed，当前开放的进 live。
        // 用 `if/else` 而不是两次 `filter`——那样一个区间就可能同时落进两边。
        let mut closed_trusted_ms = 0;
        let mut open_interval_id = None;
        let mut live_ms = 0;
        for iv in session_repo::intervals_of_session(db.connection(), &live.id)? {
            let voided = iv.voided_at.is_some();
            match iv.ended_at {
                Some(_) if !voided && !iv.needs_review => {
                    closed_trusted_ms += iv.duration_ms.unwrap_or(0);
                }
                None if !voided && live.state == SessionState::Running => {
                    open_interval_id = Some(iv.id.clone());
                    live_ms = (attributed_end - iv.started_at).max(0);
                }
                _ => {}
            }
        }

        Ok(StatsSample {
            run_id: self.run_id.clone(),
            session_id: Some(live.id),
            session_version: Some(live.row_version),
            open_interval_id,
            attributed_end,
            closed_trusted_ms,
            live_ms,
            state: Some(live.state),
        })
    }
}

fn require_active_project(conn: &Connection, task_id: &str) -> Result<(), AppError> {
    task_repo::require_active_project(conn, task_id)
}
