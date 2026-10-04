//! 任务状态编排与会话联动（P3 Task 6）。
//!
//! 这是 02 §3 的「任务完成/取消服务」：**一个用户事务**里结束该任务的
//! `running`/`paused` 会话、写任务状态与 `task_change`，`settle` 让 `revision` 恰好 +1。
//! `Blocked`/`Waiting` 走同一条路径，只是把结论换成「同事务暂停 `running` 会话」。
//!
//! # 三层同名（I5）：写代码必须带模块路径
//!
//! - [`crate::services::tasks::transition_task`]（本模块）：服务入口，**拥有事务**，
//!   校验请求、取样、编排会话联动、提交后收尾；
//! - [`crate::storage::task_repo::transition_task`]：P1 的事务原语，只改状态与质量、
//!   写 `task_change`；**签名与语义不改**；
//! - `AppState::transition_task`（`services::bootstrap`，S2）：命令入口的瘦包装。
//!
//! 既有三处对仓储原语的调用点（`catalog::clarify_ready`、`Coordinator::start`/`resume`）
//! **保持不动**：它们已经在自己的事务里，改走本入口只会得到事务套事务加二次取样。
//!
//! # 顺序（总纲 §9 与 §0.3 的 S3）
//!
//! ① 只读预检（epoch + 任务版本 + 跃迁表 + 幂等判定）→ ② [`Coordinator::boundary_facts`]
//! （**一次**样本、观察一次；判为异常时系统恢复事务已经提交，本命令返回
//! `RECOVERY_REQUIRED`、不执行原意图、不再加 `revision`）→ ③ 一个用户事务
//! （会话联动 + 任务跃迁 + 审计 + 恰好一次 `revision`）→ ④ **只有真有会话被结束/暂停**
//! 时才 [`Coordinator::rebuild_from_committed`] 重建内存镜像。
//!
//! 采样与归属终点**只**来自 ②：服务层不得自己取时间、不得自己算 `A(M)`
//! （`A(M)` 是协调器锚点的私有知识）。同理，⑷的返回值只用于重建镜像，
//! **不进 DTO 的版本字段**——`report.revision`/`report.data_epoch` 只来自 ③ 那次写事务
//! （R10：与 P4 的 `TaskChange` 逐字同一条路）。这条路径上**不得**调
//! `Coordinator::snapshot`：它自己另取一次样本，并在判为异常时再提交一笔独立系统事务，
//! 于是「写事务的版本」与「快照的版本」会差 1，DTO 里的版本再也说不清是哪一次写的。
//!
//! # 广播（顺带-2）
//!
//! 本模块**不**广播 `domain.changed`：那是命令层提交后、放锁前的事（`announce`）。
//! 已知例外照 P7 验收记录 §6.5 第 23 条登记：③ 提交成功但 ④ 重建失败时，那一笔
//! **不发** `domain.changed`（与 P2 计时族同一条例外，收敛靠 30 秒 `get_revision`）。

use rusqlite::{Connection, Transaction};

use crate::domain::error::DomainError;
use crate::domain::session::SessionState;
use crate::domain::task::{TaskStatus, TaskTransition, TransitionCause};
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::services::timer::coordinator::Coordinator;
use crate::services::timer::primitives::{end_session_in_tx, EndSessionFacts};
use crate::services::tx::{settle, settle_into, write_tx, SettledReport};
use crate::storage::db::{map_sqlite, Db};
use crate::storage::guards::{guard_epoch, guard_row_version};
use crate::storage::session_repo::{self, SessionRow};
use crate::storage::task_repo::{self, TaskRow};
use crate::storage::WriteOutcome;

/// 一次任务状态跃迁的请求。
///
/// `cause` 由请求带来（`Reopen` 是终结态回 `Ready` 的唯一合法原因）；
/// `TransitionCause` 的变体清单**不扩**（I5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionTaskRequest {
    pub task_id: String,
    pub target: TaskStatus,
    pub cause: TransitionCause,
}

/// 一次任务状态跃迁的结果。
///
/// 两个会话名单是**联动事实**（谁被结束了、谁被暂停了），不是「建议」：
/// 它们与 `task` 出自同一个写事务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTransitionReport {
    /// 任务行**提交后**的样子（含新 `row_version`）。
    pub task: TaskRow,
    /// 完成/取消联动结束的会话：`running` 的闭合开放区间，`paused` 的直接 `finished`。
    pub ended_sessions: Vec<String>,
    /// `Blocked`/`Waiting` 联动暂停的会话。
    pub paused_sessions: Vec<String>,
    /// 这次**写事务**读回的权威 `revision`（R10：不是提交后补读的）。
    pub revision: i64,
    /// 同一次写事务读回的库身份。
    pub data_epoch: String,
}

impl SettledReport for TaskTransitionReport {
    fn revision_mut(&mut self) -> &mut i64 {
        &mut self.revision
    }

    fn data_epoch_mut(&mut self) -> &mut String {
        &mut self.data_epoch
    }
}

/// 用户命令：把任务推到目标状态，并在**同一事务**里联动它的会话。
///
/// 前置与判据：
/// - `env.expected_row_version` 必填（任务版本），`env.expected_data_epoch` 在只读预检与
///   写事务里各校验一次；
/// - **幂等**：`task.status == target` 且该任务没有 `running`/`paused` 会话 ⇒ `Unchanged`
///   （不写 `task_change`、不加版本）；否则按 02 §5 的跃迁表判定；
/// - **完成/取消**（`Done`/`Cancelled`）：先检查该任务**所有**未结束会话——只要有一条
///   带未作废的待确认区间、或命中第 1 类不变量损坏、或属于别的 run，就**整体拒绝**
///   `RECOVERY_REQUIRED`（不部分执行）；通过之后同事务用
///   [`end_session_in_tx`] 结束全部 `running`/`paused`；
/// - **`Blocked`/`Waiting`**：同事务暂停 `running` 会话（`paused` 的不动）。
///   正常暂停不自动改变 `Doing`（02 §5：`Doing` 表示任务尚在处理，不等同 running session）；
/// - **reopen**：显式回 `Ready`、清当前质量、保留历史（`TaskTransition::clears_quality`
///   与仓储既有规则），**不**自动恢复旧会话。
pub fn transition_task(
    db: &mut Db,
    coordinator: &mut Coordinator,
    env: WriteEnvelope,
    req: TransitionTaskRequest,
) -> Result<WriteOutcome<TaskTransitionReport>, AppError> {
    let expected_version = env.expected_row_version.ok_or_else(|| AppError::Domain {
        detail: "缺少记录版本，无法安全地修改任务状态。".into(),
    })?;

    // ① 只读预检：请求校验（epoch + 任务版本 + 跃迁表）与幂等判定。
    //    先校验后取样（§0.3 S3 的顺序）——非法请求不该消耗掉一拍采样。
    {
        let pre = db
            .connection()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        guard_epoch(&pre, &env.expected_data_epoch)?;
        let task = task_repo::get_task(&pre, &req.task_id)?.ok_or(DomainError::UnknownTask)?;
        guard_row_version(task.row_version, expected_version)?;
        let unfinished = unfinished_sessions_of_task(&pre, &req.task_id)?;
        if !is_noop(task.status, req.target, &unfinished) {
            // `Scheduled` 在 V0.1 拒绝；终结态 → `Ready` 必须显式 `Reopen`。
            TaskTransition::new(task.status, req.target, req.cause)?;
        }
    }

    // ② 取样与检测（S3）：一次样本、观察一次。判为异常时**系统恢复事务已经提交**，
    //    本次命令返回 `RECOVERY_REQUIRED`：不执行原意图、不再加 revision（总纲 §9）。
    let facts = coordinator.boundary_facts(db)?;

    // ③ 用户事务。
    let tx = write_tx(db, &env)?;
    let before = task_repo::get_task(&tx, &req.task_id)?.ok_or(DomainError::UnknownTask)?;
    guard_row_version(before.row_version, expected_version)?;

    // 这条任务**全部**未结束会话（不限 run）：完成/取消的闸必须看得见别的 run 的残留，
    // 而真正会被结束/暂停的仍只有本次 run 的会话（`end_session_in_tx` 的判据不放宽）。
    let unfinished = unfinished_sessions_of_task(&tx, &req.task_id)?;

    if is_noop(before.status, req.target, &unfinished) {
        let report = TaskTransitionReport {
            task: before,
            ended_sessions: Vec::new(),
            paused_sessions: Vec::new(),
            revision: 0,
            data_epoch: String::new(),
        };
        // `Unchanged` 也走 `settle`：它负责在**同一个读事务**里读回权威版本，只是不加。
        let outcome = settle_into(settle(&tx, WriteOutcome::Unchanged(report))?);
        tx.commit().map_err(map_sqlite)?;
        // 没有会话被结束/暂停 ⇒ 不动镜像、不二次取样、不二次事务（R10）。
        return Ok(outcome);
    }

    // 权威判定：写之前把非法跃迁挡掉，「先结束会话、再发现跃迁不合法」不可能发生。
    TaskTransition::new(before.status, req.target, req.cause)?;

    // 本次命令会动到的会话：完成/取消 = `running` + `paused`；`Blocked`/`Waiting` =
    // 只有 `running`（已经暂停的不重复写）。
    let to_touch: Vec<&SessionRow> = unfinished
        .iter()
        .filter(|session| match req.target {
            TaskStatus::Done | TaskStatus::Cancelled => {
                matches!(session.state, SessionState::Running | SessionState::Paused)
            }
            TaskStatus::Blocked | TaskStatus::Waiting => session.state == SessionState::Running,
            _ => false,
        })
        .collect();

    // 跨 run 的会话只能先由启动扫描归一（02 §4）：在写之前整体拒绝，
    // 而不是靠放宽 `end_session_in_tx` 的 run 判据绕过。
    for session in &to_touch {
        if session.run_id != facts.run_id {
            return Err(AppError::RecoveryRequired);
        }
    }

    // 完成/取消的**整体闸**：一条会话不能被可信地结束，整条命令就拒绝。
    if matches!(req.target, TaskStatus::Done | TaskStatus::Cancelled) {
        require_completable(&tx, &req.task_id, &unfinished)?;
    }

    let session_target = match req.target {
        TaskStatus::Done | TaskStatus::Cancelled => SessionState::Finished,
        _ => SessionState::Paused,
    };

    let mut ended_sessions = Vec::new();
    let mut paused_sessions = Vec::new();
    for session in &to_touch {
        end_session_in_tx(
            &tx,
            &EndSessionFacts {
                session_id: session.id.clone(),
                // **当前 run**：上一个 run 留下的开放区间不能由本 run 以可信方式闭合。
                run_id: facts.run_id.clone(),
                expected_row_version: session.row_version,
                // 归属终点与挂钟样本都来自 ② 的那一次采样。
                attributed_end: facts.attributed_at,
                sampled_end_wall_at: facts.wall_ms,
                target_state: session_target,
            },
        )?;
        if session_target == SessionState::Finished {
            ended_sessions.push(session.id.clone());
        } else {
            paused_sessions.push(session.id.clone());
        }
    }

    let updated = task_repo::transition_task(
        &tx,
        &req.task_id,
        expected_version,
        req.target,
        req.cause,
        facts.wall_ms,
    )?;

    let report = TaskTransitionReport {
        task: updated,
        ended_sessions,
        paused_sessions,
        revision: 0,
        data_epoch: String::new(),
    };
    let outcome = settle_into(settle(&tx, WriteOutcome::Changed(report))?);
    tx.commit().map_err(map_sqlite)?;

    // ④ 提交后收尾：**只有真有会话被结束/暂停**时才重建镜像（与 P2 的 `finish` 完全
    //    同一条路径，含「库里还有别的 `running_foreground` 就改载它」的规则）。
    //    任务行变了不影响 `live`（`build` 每次都重读任务行），所以这一支既没有第二次取样，
    //    也没有第二笔事务。重建失败按提交后约定映射 `RECOVERY_REQUIRED`：事务已经落库，
    //    缺的是让内存与事实重新对上，不是「再试一次」。
    if !to_touch.is_empty() {
        let anchor = mirror_anchor(coordinator, &to_touch);
        coordinator
            .rebuild_from_committed(db.connection(), &anchor, facts.sample)
            .map_err(|_| AppError::RecoveryRequired)?;
    }
    Ok(outcome)
}

/// 幂等判据：已经是目标状态，且这条任务没有任何 `running`/`paused` 会话。
///
/// 只看这两种状态：`recovering` 不是「正在处理」，它的出口是 `reconcile` /
/// `discard_session`（Ruling 6），不该被本命令当成「有活在跑」。
fn is_noop(task_status: TaskStatus, target: TaskStatus, unfinished: &[SessionRow]) -> bool {
    task_status == target
        && !unfinished
            .iter()
            .any(|session| matches!(session.state, SessionState::Running | SessionState::Paused))
}

/// 这条任务的**全部未结束会话**（不限 run，按 `started_at, id` 稳定排序）。
///
/// 用 S9 的「不限 run」入口再看任务名：完成/取消的闸不能因为「只查本次 run」而漏掉
/// 旧 run 的残留——那些残留正是启动扫描要归一的材料。
fn unfinished_sessions_of_task(
    conn: &Connection,
    task_id: &str,
) -> Result<Vec<SessionRow>, AppError> {
    Ok(session_repo::unfinished_sessions(conn, None)?
        .into_iter()
        .filter(|session| session.task_id == task_id)
        .collect())
}

/// 完成/取消前的整体闸（02 §3：发现 recovering 先整体拒绝，不悄悄确认历史）。
///
/// 两条判据，逐条对应「这条会话不能被可信地结束」：
/// - **待确认事实**：会话是 `recovering`（它的未作废待确认段还不是工时）、会话级
///   `needs_review` 为真、或有未作废的 `needs_review` 区间（含 Ruling 6 的
///   `paused` + 待确认形态）——确认或作废是用户的决定，不是本命令能顺手做的事；
/// - **第 1 类不变量损坏**（`running` 没有开放区间、非 `running` 残留开放区间等）：
///   只诊断、不修，「结束一下」会把损坏洗成事实（`invariant_faults` 是唯一判据）。
fn require_completable(
    tx: &Transaction<'_>,
    task_id: &str,
    unfinished: &[SessionRow],
) -> Result<(), AppError> {
    for session in unfinished {
        let has_pending = session_repo::intervals_of_session(tx, &session.id)?
            .iter()
            .any(|interval| interval.needs_review && interval.voided_at.is_none());
        if session.state == SessionState::Recovering || session.needs_review || has_pending {
            return Err(AppError::RecoveryRequired);
        }
    }
    // 第 1 类损坏可能挂在**任何**状态的会话上（含终态），所以这里逐条回到会话行上
    // 确认归属，而不是只看未结束会话。
    for fault in session_repo::invariant_faults(tx, None)? {
        if session_repo::get_session(tx, &fault.session_id)?
            .is_some_and(|session| session.task_id == task_id)
        {
            return Err(AppError::RecoveryRequired);
        }
    }
    Ok(())
}

/// 提交后收尾要交给 [`Coordinator::rebuild_from_committed`] 的那条会话。
///
/// 优先取协调器**此刻镜像的那条**（它正是要按已提交事实刷新的对象）；镜像不在被改的
/// 集合里时取最后一条。给谁都安全：`rebuild_from_committed` 自己会在库里还有别的
/// `running_foreground` 时改载那一条——所以不存在「继续按旧状态出快照的 `live`」。
fn mirror_anchor(coordinator: &Coordinator, touched: &[&SessionRow]) -> String {
    let mirrored = coordinator.live().map(|live| live.id.as_str());
    touched
        .iter()
        .find(|session| Some(session.id.as_str()) == mirrored)
        .map(|session| session.id.clone())
        .unwrap_or_else(|| touched[touched.len() - 1].id.clone())
}
