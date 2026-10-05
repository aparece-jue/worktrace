//! 事件信封与去重协议（00 §5，P7 Task 0）。
//!
//! 这是该协议在 Rust 侧的**唯一文本**：信封字段、广播口径与四条去重规则都写在这里，
//! 并由 `tests/event_protocol.rs` 逐条钉住。前端（Task 2）按同一份规则实现自己的
//! `domainState`，P6 只在这套协议上补故障路径（P6 Task 3）——**不新建第二套**。
//!
//! ## 信封（字段固定，不得增删）
//!
//! `data_epoch` / `event` / `revision` / `at`（Unix 毫秒）/ `payload`。
//! `event_seq` 之类**不在信封里**：跳号是靠同一 epoch 内 `revision` 的连续性判断的
//! （见规则④），多一个序号字段就多一处可以和 `revision` 打架的真相源。
//!
//! ## 广播
//!
//! - **按提交顺序**：广播发生在业务事务提交**之后**、且在同一串行边界内
//!   （[`crate::services::bootstrap::AppState`] 的那把锁）。广播出口另做一次
//!   单调性检查，倒序只记诊断——顺序本身不该由这个检查来保证。
//! - **失败只记诊断，不回滚已提交业务**：广播失败时事务早已提交，
//!   把它报成「事务失败」会让用户重做一次**已经成功**的操作（00 §4）。
//!   所以 [`Broadcaster::emit`] **不返回 `Result`**：调用方从类型上就没法把它
//!   当成业务失败往上抛。
//!
//! ## dev 注入开关（P7 Task 6a，**只在 debug 构建存在**）
//!
//! 真实双窗口实验（`tests/manual-sync.md`）要确定性地造出「末次通知丢失」，
//! 所以广播出口留了一个 [`Broadcaster::arm_drop_next`]：丢掉下一条**指定事件名**的
//! 通知。它**只影响投递**——业务写入、事务与 revision 都不经过它，丢一条通知不会
//! 让任何已提交的业务回滚（与「广播失败只记诊断」同一条口径）。
//!
//! 方法、状态字段与 `EmitOutcome::Dropped` 都带 `#[cfg(debug_assertions)]`：
//! 发布构建里它们整份不存在，不是「关掉」，是**没有**。
//!
//! ## 时间
//!
//! `at` 由调用方传入——服务层不得自取系统时间（分层门禁），
//! 计时路径传 `TimerSnapshot::as_of`（同一次采样的归属挂钟），
//! 业务写路径传协调器/时钟采样到的 `wall_ms`。

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::services::timer::snapshot::TimerSnapshot;

/// 同一 epoch 内一次成功业务写对应一条（00 §5）。
pub const EVENT_DOMAIN_CHANGED: &str = "domain.changed";
/// 周期采样驱动发出的计时快照；**不代表业务写**（不加 revision）。
pub const EVENT_TIMER_TICK: &str = "timer.tick";

/// 事件信封（00 §5）。字段就是这五个。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub data_epoch: String,
    pub event: String,
    pub revision: i64,
    /// Unix 毫秒。由调用方从 `platform::clock` 的采样传入。
    pub at: i64,
    pub payload: serde_json::Value,
}

impl EventEnvelope {
    pub fn new(
        data_epoch: impl Into<String>,
        event: impl Into<String>,
        revision: i64,
        at: i64,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            data_epoch: data_epoch.into(),
            event: event.into(),
            revision,
            at,
            payload,
        }
    }

    /// 业务变更通知。`payload` 是这次变更的载荷（形状由各服务定义）。
    pub fn domain_changed(
        data_epoch: impl Into<String>,
        revision: i64,
        at: i64,
        payload: serde_json::Value,
    ) -> Self {
        Self::new(data_epoch, EVENT_DOMAIN_CHANGED, revision, at, payload)
    }

    /// 计时 tick 通知。`revision` 取快照里的权威值（tick 本身不加 revision）。
    pub fn timer_tick(snapshot: &TimerSnapshot) -> Self {
        Self::new(
            snapshot.data_epoch.clone(),
            EVENT_TIMER_TICK,
            snapshot.revision,
            snapshot.as_of,
            timer_tick_payload(snapshot),
        )
    }
}

/// tick 事件的载荷：00 §5 的计时字段，一个不多一个不少。
///
/// **就是 [`TimerSnapshot`] 的 serde 形状**（P7 Task 1 订正）：原先这里手写一份
/// `json!`，当时的理由是本类型还没有 `Serialize`（「属 Task 1 的 DTO 工作」）。
/// 现在它有了，两份形状必须合成一份——留着两份，就是给「改了 Rust 类型忘了改前端类型」
/// 留一道手写的缝。`tests/ipc_snapshots.rs` 同时钉住 `TimerSnapshot` 的快照与
/// 「载荷 = 快照的 JSON」这条等式。
pub fn timer_tick_payload(snapshot: &TimerSnapshot) -> serde_json::Value {
    serde_json::to_value(snapshot).expect("timer snapshot is plain JSON data")
}

/// 广播出口。**实现必须是投递式（非阻塞）的**：它在串行边界内被调用，
/// 长阻塞会让用户命令排队等它。
pub trait EventSink: Send + Sync {
    /// 投递一条通知。`Err` 只被当作**诊断**。
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String>;
}

/// 广播出口的累计诊断。只增不减，不参与业务判断。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BroadcastDiagnostics {
    /// 成功投递条数。
    pub sent: u64,
    /// 投递失败条数（**不影响已提交业务**）。
    pub failed: u64,
    /// 同一 epoch 内 revision 倒退的投递次数。
    ///
    /// 这是**编程错误**的计数器，不是用户可见状态：顺序由「同一串行边界内、
    /// 提交之后广播」保证。真的出现时仍然投递（见 [`Broadcaster::emit`]）。
    pub out_of_order: u64,
    /// 被 **dev 注入开关**（`arm_drop_next`，只在 debug 构建存在）丢掉的通知条数。
    ///
    /// 发布构建里它恒为 0：那时开关根本不存在。当前只有测试消费；P8 的开发诊断
    /// 出口接入后，实机实验（`tests/manual-sync.md` §2.1/§2.3.1）才可读取该值确认注入命中。
    pub dropped: u64,
    /// 最近一次失败的诊断文本。
    pub last_failure: Option<String>,
}

/// 一次投递的结果。**刻意不是 `Result`**：广播失败不是业务失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitOutcome {
    Sent,
    Failed,
    /// 被 dev 注入开关拦下（P7 Task 6a 的「丢一次通知」，只在 debug 构建存在）。
    ///
    /// 与 [`EmitOutcome::Failed`] 分开，是因为两者要分辨的事不同：失败是**通道**
    /// 出问题（要记诊断），丢弃是**实验器材故意**做的（业务照常）。
    #[cfg(debug_assertions)]
    Dropped,
}

impl EmitOutcome {
    pub fn is_sent(self) -> bool {
        matches!(self, Self::Sent)
    }
}

#[derive(Debug, Default)]
struct BroadcastState {
    epoch: Option<String>,
    last_revision: i64,
    diagnostics: BroadcastDiagnostics,
}

/// 广播器：持有出口，记录诊断，检查同一 epoch 内的 revision 单调性。
///
/// 不派生 `Debug`：出口是 `dyn EventSink`，诊断请走 [`Broadcaster::diagnostics`]。
pub struct Broadcaster {
    sink: Arc<dyn EventSink>,
    state: Mutex<BroadcastState>,
    /// dev 注入开关的状态（P7 Task 6a）：**只在 debug 构建存在**。
    #[cfg(debug_assertions)]
    dev: Mutex<DevInjections>,
}

/// dev 注入开关的状态。只在 debug 构建编译（`arm_drop_next` 是它唯一的写入方）。
#[cfg(debug_assertions)]
#[derive(Debug, Default)]
struct DevInjections {
    /// 「丢掉下一条 `kind` 事件」：`None` = 没装开关。
    drop_next: Option<String>,
}

impl Broadcaster {
    pub fn new(sink: Arc<dyn EventSink>) -> Self {
        Self {
            sink,
            state: Mutex::new(BroadcastState::default()),
            #[cfg(debug_assertions)]
            dev: Mutex::new(DevInjections::default()),
        }
    }

    /// 投递一条通知。**永不返回错误**：失败只记诊断。
    ///
    /// 倒序（同 epoch 且 `revision` 小于上一条）会加一次 `out_of_order` 诊断，
    /// 但**仍然投递**——丢掉它可能让某个落后的客户端永远停在旧状态；
    /// 而客户端的规则②本来就会丢弃过期通知，重复投递是无害的。
    pub fn emit(&self, envelope: EventEnvelope) -> EmitOutcome {
        {
            let mut state = lock(&self.state);
            if state.epoch.as_deref() != Some(envelope.data_epoch.as_str()) {
                // 新 epoch：上一代次的 revision 不可比（00 §5），重新起算。
                state.epoch = Some(envelope.data_epoch.clone());
                state.last_revision = envelope.revision;
            } else if envelope.revision < state.last_revision {
                state.diagnostics.out_of_order += 1;
            } else {
                state.last_revision = envelope.revision;
            }
        }

        // dev 注入开关（P7 Task 6a，只在 debug 构建存在）：**按事件名**丢掉这一条。
        // 顺序水位线在上面已经推进过——这条通知是「产生了但在路上丢了」，
        // 不是「没产生」，客户端的跳号判据看到的正是这个缺口。
        #[cfg(debug_assertions)]
        if self.take_drop(&envelope.event) {
            println!(
                "[worktrace] dev: {}",
                serde_json::json!({
                    "phase": "event_dropped", "event": envelope.event,
                    "revision": envelope.revision,
                })
            );
            lock(&self.state).diagnostics.dropped += 1;
            return EmitOutcome::Dropped;
        }

        match self.sink.broadcast(&envelope) {
            Ok(()) => {
                lock(&self.state).diagnostics.sent += 1;
                EmitOutcome::Sent
            }
            Err(detail) => {
                let mut state = lock(&self.state);
                state.diagnostics.failed += 1;
                state.diagnostics.last_failure = Some(detail);
                EmitOutcome::Failed
            }
        }
    }

    /// 诊断快照。
    pub fn diagnostics(&self) -> BroadcastDiagnostics {
        lock(&self.state).diagnostics.clone()
    }

    /// **dev 注入 (a)**（P7 Task 6a）：丢掉**下一条事件名为 `kind` 的通知**。
    ///
    /// ⚠️ 必须按事件名筛（`manual-sync.md` §1 的订正）：有活动会话时每秒一条
    /// `timer.tick`，不筛的话「丢掉下一条」会被一条无害 tick 吃掉，实机看起来
    /// 「什么都没发生」。所以开关只在**事件名相同**时被消费（见 [`Self::take_drop`]）。
    ///
    /// 重复装开关 = 后一次覆盖前一次（语义始终是「下一条」）。开关是**进程内全局**的：
    /// 丢的是那次广播本身，不是某个窗口的收件箱——`manual-sync.md` §2.1 正是这么用的
    /// （在 B 上装开关、在 A 上触发写，A 靠自己的命令响应更新，B 等 30 秒校验收敛）。
    ///
    /// 只影响**事件投递**：业务写入、事务、revision 都已经在调用它之前完成。
    #[cfg(debug_assertions)]
    pub fn arm_drop_next(&self, kind: &str) {
        lock(&self.dev).drop_next = Some(kind.to_string());
    }

    /// 消费式判定：事件名**相同**才命中，命中即清空（一次一发）。
    #[cfg(debug_assertions)]
    fn take_drop(&self, kind: &str) -> bool {
        let mut dev = lock(&self.dev);
        if dev.drop_next.as_deref() == Some(kind) {
            dev.drop_next = None;
            true
        } else {
            false
        }
    }
}

/// 互斥锁中毒不该让广播诊断变成 panic：诊断是尽力而为的。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ─────────────────────────────────────────────────────────────────────────────
// 去重协议（客户端侧规则的参考实现）
// ─────────────────────────────────────────────────────────────────────────────

/// 应用一份权威快照之后，客户端缓存发生了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotEffect {
    /// **新 epoch**：全部业务/计时缓存失效（规则①）。旧 epoch 的通知与响应此后一律不接纳。
    CacheInvalidated,
    /// 同 epoch 的新版本：按快照整体替换（它本身就是权威状态）。
    Applied,
    /// 同 epoch 但比已应用版本旧：**不覆盖**（规则③对快照的同一条要求）。
    StaleIgnored,
}

/// 一条通知的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationVerdict {
    /// 接纳：作缓存失效处理（事件只作失效，不携带权威状态）。
    Apply,
    /// 丢弃：同 epoch 且版本不高于已应用/已见版本（规则②）。
    Drop,
    /// **未知 epoch**：只触发重新握手，绝不直接接纳（规则①）。
    Rehandshake,
    /// 跳号/乱序，无法证明一致：取新快照（规则④）。
    Resync,
}

/// 一个查询响应的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryVerdict {
    Accept,
    /// 比已应用/所需版本旧：**不得覆盖**（规则③）。
    Drop,
    /// epoch 已变：先重新握手。
    Rehandshake,
}

/// 客户端侧版本闸门：规则①–④的参考实现。
///
/// 每个 JS 上下文一个（窗口各自持有一个）。它**不缓存业务数据**，只维护
/// 「我已经应用到哪一版」这三个数——业务规则留在 Rust，这里只是协议。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RevisionGate {
    epoch: Option<String>,
    /// 已应用快照的版本。规则②与规则③的水位线。
    applied_revision: i64,
    /// 已接纳通知的最大版本。规则④的跳号判据。
    seen_revision: i64,
    /// 已经发现过跳号：在拿到新快照之前不再逐条接纳。
    ///
    /// 为什么需要它：发现缺了一条之后，迟到的那条通知虽然能把序号补上，
    /// 但客户端已经无法证明「自己看到的顺序就是提交顺序」。取一份新快照是唯一的
    /// 收敛方式（规则④），所以这个标记**只由快照清除**。
    resync_required: bool,
}

impl RevisionGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn epoch(&self) -> Option<&str> {
        self.epoch.as_deref()
    }

    pub fn applied_revision(&self) -> i64 {
        self.applied_revision
    }

    pub fn seen_revision(&self) -> i64 {
        self.seen_revision
    }

    /// 应用一份权威快照。
    pub fn apply_snapshot(&mut self, data_epoch: &str, revision: i64) -> SnapshotEffect {
        match self.epoch.as_deref() {
            Some(current) if current == data_epoch => {
                if revision < self.applied_revision {
                    return SnapshotEffect::StaleIgnored;
                }
                self.applied_revision = revision;
                self.seen_revision = self.seen_revision.max(revision);
                // 一份权威快照就是「重新同步」本身：缺口到此为止。
                self.resync_required = false;
                SnapshotEffect::Applied
            }
            _ => {
                self.epoch = Some(data_epoch.to_string());
                self.applied_revision = revision;
                self.seen_revision = revision;
                self.resync_required = false;
                SnapshotEffect::CacheInvalidated
            }
        }
    }

    /// 处置一条通知。
    ///
    /// **只用于「使缓存失效」的通知（`domain.changed`）**。计时 tick 不走这里：
    /// tick 只更新展示值，它的新鲜度按 00 §5 的另一条判据（先 epoch / run_id /
    /// session_id / session_version，再比 `tick_seq`），由前端 `orderTimer` 实现；
    /// Rust 的 `Coordinator::is_stale_tick` 只是局部探针，不是镜像。把 tick 塞进这套版本水位线，会让同一
    /// `revision` 下的第二拍 tick 被当成「过期通知」丢掉，计时展示就停住了。
    ///
    /// 判定顺序（就是规则的顺序，不要重排）：
    /// ① 未知 epoch → `Rehandshake`；
    /// ② 同 epoch 且 `revision <= applied_revision` → `Drop`；
    /// ③ 同 epoch 且 `revision <= seen_revision`（重复/乱序）→ `Drop`；
    /// ④ 同 epoch 但跳号（`revision > seen_revision + 1`）→ `Resync`；
    /// 否则接纳。
    ///
    /// ④ 一旦发现跳号就**持续要求重新同步**，直到一份新快照把缺口补上——
    /// 这样「中间漏了一条」不会被迟到的通知掩盖过去（迟到的那条也许能补上序号，
    /// 但补不回「我们已经按错误顺序看过一遍」这件事）。
    pub fn on_notification(&mut self, envelope: &EventEnvelope) -> NotificationVerdict {
        match self.epoch.as_deref() {
            Some(current) if current == envelope.data_epoch => {}
            _ => return NotificationVerdict::Rehandshake,
        }

        if envelope.revision <= self.applied_revision {
            return NotificationVerdict::Drop;
        }
        if envelope.revision <= self.seen_revision {
            return NotificationVerdict::Drop;
        }
        if self.resync_required {
            return NotificationVerdict::Resync;
        }
        if envelope.revision > self.seen_revision + 1 {
            self.resync_required = true;
            return NotificationVerdict::Resync;
        }

        self.seen_revision = envelope.revision;
        NotificationVerdict::Apply
    }

    /// 处置一个查询响应。
    ///
    /// `required_revision` 是这次查询**必须达到**的版本（通常是
    /// `applied_revision`；某次变更之后等待中的视图会传更高的值）。
    /// 比它旧就丢弃，而不是拿旧数据覆盖新展示。
    pub fn on_query_response(
        &self,
        data_epoch: &str,
        revision: i64,
        required_revision: i64,
    ) -> QueryVerdict {
        match self.epoch.as_deref() {
            Some(current) if current == data_epoch => {}
            _ => return QueryVerdict::Rehandshake,
        }
        if revision < required_revision || revision < self.applied_revision {
            return QueryVerdict::Drop;
        }
        QueryVerdict::Accept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingSink {
        seen: Mutex<Vec<EventEnvelope>>,
        fail: bool,
    }

    impl EventSink for RecordingSink {
        fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
            if self.fail {
                return Err("sink down".to_string());
            }
            lock(&self.seen).push(envelope.clone());
            Ok(())
        }
    }

    #[test]
    fn a_failing_sink_only_produces_diagnostics() {
        let sink = Arc::new(RecordingSink {
            seen: Mutex::new(Vec::new()),
            fail: true,
        });
        let broadcaster = Broadcaster::new(Arc::clone(&sink) as Arc<dyn EventSink>);

        let outcome = broadcaster.emit(EventEnvelope::domain_changed(
            "e1",
            1,
            10,
            serde_json::json!({}),
        ));
        assert_eq!(
            outcome,
            EmitOutcome::Failed,
            "失败不是 panic 也不是业务错误"
        );
        let d = broadcaster.diagnostics();
        assert_eq!((d.sent, d.failed), (0, 1));
        assert_eq!(d.last_failure.as_deref(), Some("sink down"));
    }

    #[test]
    fn a_backwards_revision_is_recorded_as_out_of_order() {
        let sink = Arc::new(RecordingSink {
            seen: Mutex::new(Vec::new()),
            fail: false,
        });
        let broadcaster = Broadcaster::new(Arc::clone(&sink) as Arc<dyn EventSink>);

        for rev in [1, 2, 3, 2] {
            broadcaster.emit(EventEnvelope::domain_changed(
                "e1",
                rev,
                10,
                serde_json::json!({}),
            ));
        }
        assert_eq!(broadcaster.diagnostics().out_of_order, 1);
        assert_eq!(
            broadcaster.diagnostics().sent,
            4,
            "倒序仍然投递（客户端按规则②丢弃）"
        );
    }

    #[test]
    fn a_new_epoch_restarts_the_ordering_watermark() {
        let sink = Arc::new(RecordingSink {
            seen: Mutex::new(Vec::new()),
            fail: false,
        });
        let broadcaster = Broadcaster::new(Arc::clone(&sink) as Arc<dyn EventSink>);

        broadcaster.emit(EventEnvelope::domain_changed(
            "e1",
            9,
            10,
            serde_json::json!({}),
        ));
        broadcaster.emit(EventEnvelope::domain_changed(
            "e2",
            1,
            11,
            serde_json::json!({}),
        ));
        assert_eq!(
            broadcaster.diagnostics().out_of_order,
            0,
            "新 epoch 的 revision 与旧 epoch 不可比"
        );
    }

    #[test]
    fn the_envelope_has_exactly_the_five_contract_fields() {
        let envelope =
            EventEnvelope::domain_changed("e1", 3, 1_700_000_000_000, serde_json::json!({"a": 1}));
        let json = serde_json::to_value(&envelope).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["at", "data_epoch", "event", "payload", "revision"]);
        assert_eq!(json["event"], "domain.changed");
    }
}
