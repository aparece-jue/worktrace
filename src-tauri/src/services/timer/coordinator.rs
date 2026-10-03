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

use rusqlite::Connection;

use crate::domain::interval::IntervalFacts;
use crate::domain::session::{SessionState, TimerBudget, TimerKind};
use crate::error::AppError;
use crate::platform::clock::{Clock, ClockSample};
use crate::storage::meta::require_meta;
use crate::storage::session_repo;

use super::anchor::{AnchorState, SampleVerdict};
use super::snapshot::TimerSnapshot;

/// 协调器在内存里持有的会话事实。**每一条都能在库里找到对应行**——
/// 它不是第二份真相源，只是避免每次查询都重新聚合的缓存。
#[derive(Debug, Clone)]
pub struct LiveSession {
    pub id: String,
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

    /// 显式重建归属基线（新 run、休眠唤醒后、用户显式校正）。
    ///
    /// **调用方必须先关闭/隔离旧的开放事实**：重建会让 `A(M)` 跳变，旧区间若还开着，
    /// 它的起点就与新归属对不上了。
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

        self.live = Some(LiveSession {
            id: row.id,
            row_version: row.row_version,
            state: row.state,
            budget,
            closed_trusted_ms,
            open_interval: open,
        });
        Ok(())
    }

    /// 查询快照。**自己也取一次采样**，不走 `&self` 绕过检测。
    pub fn snapshot(&mut self, conn: &Connection) -> Result<TimerSnapshot, AppError> {
        // 采样失败**不得伪造**一个 ClockSample 继续。事实取不到就是取不到，
        // 走恢复路径让用户确认——用 RECOVERY_REQUIRED 而不是带技术细节的
        // DOMAIN_ERROR：后者的 message() 会把 detail 原样拼进用户文案。
        let sample = self
            .clock
            .sample()
            .map_err(|_| AppError::RecoveryRequired)?;
        self.build(conn, sample, false)
    }

    /// 推进一步。与 [`Coordinator::snapshot`] 的唯一区别是 `tick_seq` 前进一格。
    pub fn tick(&mut self, conn: &Connection) -> Result<TimerSnapshot, AppError> {
        // 采样失败**不得伪造**一个 ClockSample 继续。事实取不到就是取不到，
        // 走恢复路径让用户确认——用 RECOVERY_REQUIRED 而不是带技术细节的
        // DOMAIN_ERROR：后者的 message() 会把 detail 原样拼进用户文案。
        let sample = self
            .clock
            .sample()
            .map_err(|_| AppError::RecoveryRequired)?;
        self.build(conn, sample, true)
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

        // **先检测，再决定怎么用这个样本**。判定结果对外可见（`last_verdict`），
        // 由 Task 3/4 决定要不要提交恢复事务；本任务只负责「看见了什么」。
        self.last_verdict = match self.anchor_state.as_mut() {
            Some(st) => st.observe(sample),
            None => SampleVerdict::Trusted,
        };

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
            (_, Some((_, started_at)), SessionState::Running) => {
                let a = self.anchor_state.as_ref().expect("matched above");
                // 基线倒退时暂计会是负数——截到 0，异常由 Task 2 检测后分割，
                // 不在这里把负数当成工时。
                a.attribute(sample.monotonic_ms)
                    .saturating_sub(*started_at)
                    .max(0)
            }
            _ => 0,
        };

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
