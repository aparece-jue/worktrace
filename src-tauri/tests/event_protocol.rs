//! P7 Task 0：事件信封与四条去重规则（00 §5）。
//!
//! 计划原文要求四条规则**各一条断言**。这些规则在 Task 2 会由前端再实现一遍
//! （`useSyncExternalStore` 那一侧），但**规则本身只在 Rust 侧断言**——
//! 前端测试只断言展示与转发（总纲 §5 第 8 条）。
//!
//! ⚠️ **本文件只放「信封 + 去重规则」的结构性事实。**
//! 「广播按提交顺序」与「广播失败不回滚已提交业务」在 **`tests/periodic_sampling.rs`
//! 的真实路径上**验（提交后可见 + 双写者单调非降 + 失败回滚），这里不再用
//! 「顺序 emit 到一个 `Vec`」冒充它们（fix round 1：那两条原本恒真）。
//!
//! **P6 Task 3 追加**：故障路径与边界也落在本文件——末次通知丢失后的 `get_revision`
//! 收敛（00 §5 规则 4 / F-020）、订阅者异常（`Err` 与 panic）、乱序通知被丢弃、
//! 迟到响应不覆盖新状态。后两条只在闸门上验；前两条要**真库 + 真命令路径**
//! （`launch` 夹具，走 `services::bootstrap::startup`），因为「通知丢了以后靠轮询收敛」
//! 不是闸门自己能证明的事：它要证明的是**已提交的业务**还在，而闸门还能从权威身份
//! 重新对齐。「广播按提交顺序」与「广播失败不回滚业务」仍以
//! `tests/periodic_sampling.rs` / `tests/ipc_commands.rs` 的真实路径用例为准，
//! 这里只在收敛用例里顺带核对（不重复造第二套断言）。

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use worktrace_lib::commands::{self, CreateTaskRequest};
use worktrace_lib::error::AppError;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{
    Broadcaster, EmitOutcome, EventEnvelope, EventSink, NotificationVerdict, QueryVerdict,
    RevisionGate, SnapshotEffect,
};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;

const EPOCH_A: &str = "epoch-a";
const EPOCH_B: &str = "epoch-b";

#[derive(Default)]
struct RecordingSink {
    seen: Mutex<Vec<EventEnvelope>>,
}

impl EventSink for RecordingSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

fn notification(epoch: &str, revision: i64) -> EventEnvelope {
    EventEnvelope::domain_changed(
        epoch,
        revision,
        1_700_000_000_000 + revision,
        serde_json::json!({}),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 四条规则
// ─────────────────────────────────────────────────────────────────────────────

/// 规则①：新 `data_epoch` 的权威快照使**全部**缓存失效；
/// **未知 epoch 的通知只触发重新握手**，绝不直接接纳。
#[test]
fn rule1_a_new_epoch_invalidates_everything_and_unknown_epochs_only_rehandshake() {
    let mut gate = RevisionGate::new();

    assert_eq!(
        gate.apply_snapshot(EPOCH_A, 10),
        SnapshotEffect::CacheInvalidated,
        "第一次拿到快照就是一次全量失效"
    );
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 11)),
        NotificationVerdict::Apply
    );

    // 换库（恢复/替换）：新 epoch 的快照再次让全部缓存失效。
    assert_eq!(
        gate.apply_snapshot(EPOCH_B, 1),
        SnapshotEffect::CacheInvalidated,
        "新 epoch 的快照使全部业务/计时缓存失效"
    );
    assert_eq!(gate.epoch(), Some(EPOCH_B));

    // 迟到的旧 epoch 通知：不许回来把缓存切回旧库，只重新握手。
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 12)),
        NotificationVerdict::Rehandshake
    );
    // 从没见过的 epoch 同理。
    assert_eq!(
        gate.on_notification(&notification("epoch-unknown", 1)),
        NotificationVerdict::Rehandshake
    );
    assert_eq!(
        gate.epoch(),
        Some(EPOCH_B),
        "重新握手之前，闸门仍停在当前 epoch"
    );
}

/// 监听早于首份快照：通知不能自行确立库身份或推进任何水位。
#[test]
fn a_notification_before_the_first_snapshot_requires_handshake_without_adopting_identity() {
    let mut gate = RevisionGate::new();
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 42)),
        NotificationVerdict::Rehandshake
    );
    assert_eq!(gate.epoch(), None);
    assert_eq!(gate.applied_revision(), 0);
    assert_eq!(gate.seen_revision(), 0);
    assert_eq!(
        gate.apply_snapshot(EPOCH_A, 41),
        SnapshotEffect::CacheInvalidated
    );
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 42)),
        NotificationVerdict::Apply
    );
}

/// 规则②：应用快照后，丢弃**同 epoch 且 `revision <=` 快照版本**的通知。
#[test]
fn rule2_notifications_at_or_below_the_applied_snapshot_are_dropped() {
    let mut gate = RevisionGate::new();
    gate.apply_snapshot(EPOCH_A, 7);

    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 7)),
        NotificationVerdict::Drop,
        "与快照同版本的通知没有新信息"
    );
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 3)),
        NotificationVerdict::Drop,
        "比快照旧的通知必须丢弃"
    );
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 8)),
        NotificationVerdict::Apply,
        "比快照新的通知照常接纳"
    );
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 8)),
        NotificationVerdict::Drop,
        "同一条通知重复到达也只是丢弃"
    );
}

/// 规则③：查询响应比已应用/所需版本旧时**不得覆盖**。
#[test]
fn rule3_a_stale_query_response_never_overwrites() {
    let mut gate = RevisionGate::new();
    gate.apply_snapshot(EPOCH_A, 10);

    assert_eq!(
        gate.on_query_response(EPOCH_A, 9, gate.applied_revision()),
        QueryVerdict::Drop,
        "旧响应不得覆盖已应用的版本"
    );
    assert_eq!(
        gate.on_query_response(EPOCH_A, 10, gate.applied_revision()),
        QueryVerdict::Accept
    );
    assert_eq!(
        gate.on_query_response(EPOCH_A, 11, 12),
        QueryVerdict::Drop,
        "比这次查询要求的版本旧，也不能用"
    );
    assert_eq!(
        gate.on_query_response("epoch-unknown", 12, 12),
        QueryVerdict::Rehandshake
    );
    // 迟到的**快照**同样不许把水位线拉回去（规则③对快照的同一条要求）。
    assert_eq!(
        gate.apply_snapshot(EPOCH_A, 4),
        SnapshotEffect::StaleIgnored
    );
    assert_eq!(gate.applied_revision(), 10);
}

/// 规则④：跳号/乱序**无法证明一致**时取新快照。
#[test]
fn rule4_a_revision_gap_requires_a_fresh_snapshot() {
    let mut gate = RevisionGate::new();
    gate.apply_snapshot(EPOCH_A, 10);

    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 11)),
        NotificationVerdict::Apply
    );
    // 13 到了、12 没到：中间那条可能已经丢了，不能假装一致。
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 13)),
        NotificationVerdict::Resync,
        "跳号必须触发重新取快照"
    );
    // 乱序迟到的那条（12）也不能让客户端「就地补课」：发现缺口之后必须整体重新同步，
    // 否则我们无法证明自己按提交顺序看过一遍。
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 12)),
        NotificationVerdict::Resync
    );
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 14)),
        NotificationVerdict::Resync,
        "新快照到手之前，缺口不会被后续通知掩盖"
    );

    // 新快照把它拉回连续。
    assert_eq!(gate.apply_snapshot(EPOCH_A, 14), SnapshotEffect::Applied);
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 15)),
        NotificationVerdict::Apply
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 广播：顺序与失败
// ─────────────────────────────────────────────────────────────────────────────

/// 出口把每次调用原样转发给 sink，并如实累计诊断计数。
///
/// ⚠️ **只证明类型/接口形状**（转发 + 计数），**不证明「广播按提交顺序」，也不证明
/// 「广播失败不回滚业务」**：这里没有事务、没有并发、没有已提交业务，任何「不丢不重」
/// 的实现都会过。那两条的证据在 `tests/periodic_sampling.rs`：
/// `a_tick_is_broadcast_after_the_commit_inside_the_same_boundary`、
/// `two_writers_and_the_sampler_never_let_the_outlet_see_a_backwards_revision`、
/// `a_failed_tick_broadcast_never_rolls_back_the_committed_heartbeat`。
#[test]
fn the_outlet_forwards_every_call_and_counts_diagnostics() {
    let sink = Arc::new(RecordingSink::default());
    let broadcaster = Broadcaster::new(Arc::clone(&sink) as Arc<dyn EventSink>);

    for revision in 1..=3 {
        assert_eq!(
            broadcaster.emit(notification(EPOCH_A, revision)),
            EmitOutcome::Sent
        );
    }

    let seen: Vec<i64> = sink
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.revision)
        .collect();
    assert_eq!(
        seen,
        vec![1, 2, 3],
        "出口按调用顺序转发（本条只证明接口形状：不证明提交顺序，也不证明失败不回滚）"
    );
    assert_eq!(broadcaster.diagnostics().out_of_order, 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// P6 Task 3：故障路径与边界
//
// 三种「协议本身正确之外」的场面：通知**没到**（投递失败 / 通道坏了）、消息**到晚了**
// （乱序、迟到响应、旧 epoch）、订阅者**违约**（panic）。前两种先在纯闸门上验，
// 第三种与「丢了以后靠轮询收敛」要真库 + 真命令路径。
// ─────────────────────────────────────────────────────────────────────────────

/// 真库 + 真命令路径的夹具用的挂钟起点（`FakeClock`，不依赖真实时间）。
const WALL: i64 = 1_700_000_000_000;

/// **规则②（快照水位线）是性质，不是「四条各一例」里的那一例。**
///
/// `on_notification` 先查②（`revision <= applied_revision`）再查③
/// （`revision <= seen_revision`），而 `apply_snapshot` **恒保证** `seen >= applied`：
/// 新 epoch 的两条分支把两者设成同一个值，同 epoch 分支取 `max`。
/// ⇒ ②能挡下的通知，③一定也挡得下——**把②整条删掉，黑盒看不到任何差别**
/// （P7 已实测冻结：「只删闸门②六条全绿」，见 `docs/validation/p7-acceptance.md` §8.5）。
///
/// **这是它的性质，不是覆盖缺口**。所以这里不去构造一个「只有②能挡」的输入
/// （那要么造不出来，要么得先破坏 `apply_snapshot` 的不变量），而是驱动一串合法操作，
/// 逐步断言那条不变量本身，并在②命中的每一步顺手记下「③必然也命中」。
/// `src/types/__vectors__/revision-gate.json` 里那条「规则②」用例照旧保留：
/// 它是有效的**行为**回归（`<=` 的分寸：等于也丢），只是不充当②的判别力证明。
///
/// 判别力（P6 Task 3 的变异 C）：删掉同 epoch 分支的 `seen_revision.max(revision)`，
/// 本用例在第一次「快照把 applied 推高、seen 没跟上」时变红。
#[test]
fn rule2_applied_revision_never_exceeds_seen_revision() {
    #[derive(Debug)]
    enum Step {
        Snapshot(&'static str, i64),
        Notify(&'static str, i64),
    }

    let steps = [
        Step::Snapshot(EPOCH_A, 10),      // 首份快照：全部缓存失效
        Step::Notify(EPOCH_A, 10),        // 与快照同版本 ⇒ ②丢弃
        Step::Notify(EPOCH_A, 11),        // 接纳
        Step::Notify(EPOCH_A, 12),        // 接纳
        Step::Snapshot(EPOCH_A, 10),      // 旧于水位 ⇒ 不覆盖
        Step::Snapshot(EPOCH_A, 11),      // 同 epoch 新版本 ⇒ 整体替换
        Step::Notify(EPOCH_A, 11),        // 重复 ⇒ 丢弃
        Step::Notify(EPOCH_A, 20),        // 跳号 ⇒ 取新快照
        Step::Notify(EPOCH_A, 13),        // 缺口期内的迟到通知 ⇒ 仍要新快照
        Step::Snapshot(EPOCH_A, 20),      // 权威快照把缺口补上
        Step::Notify(EPOCH_A, 21),        // 恢复连续
        Step::Notify("epoch-unknown", 3), // 未知 epoch ⇒ 只握手
        Step::Snapshot(EPOCH_B, 1),       // 换库 ⇒ 全部缓存失效
        Step::Notify(EPOCH_A, 22),        // 迟到的旧 epoch 通知 ⇒ 只握手
        Step::Notify(EPOCH_B, 1),         // 与快照同版本 ⇒ 丢弃
        Step::Notify(EPOCH_B, 2),         // 接纳
        Step::Notify(EPOCH_B, 2),         // 重复 ⇒ 丢弃
    ];

    let mut gate = RevisionGate::new();
    let (mut snapshots, mut notifications, mut rule2_hits) = (0, 0, 0);

    for step in &steps {
        match step {
            Step::Snapshot(epoch, revision) => {
                gate.apply_snapshot(epoch, *revision);
                snapshots += 1;
            }
            Step::Notify(epoch, revision) => {
                // 判据必须取**调用之前**的水位：`Apply` 会把 `seen_revision` 抬到本条通知的
                // 版本（`events.rs` 的接纳分支），用事后值判「②命中 ⇒ ③必然也命中」会自证
                // ——一个错误地**接纳**了 `revision <= applied_revision` 的实现，事后 `seen`
                // 已经被抬到 ≥ revision，那条子断言照样通过（评审 Important 2 的修正）。
                let same_epoch = gate.epoch() == Some(*epoch);
                let (applied_before, seen_before) = (gate.applied_revision(), gate.seen_revision());

                let verdict = gate.on_notification(&notification(epoch, *revision));
                notifications += 1;

                if same_epoch && *revision <= applied_before {
                    // 这一步正是②会命中的地方：证明③也一定命中（两者处置都是丢弃）。
                    assert!(
                        *revision <= seen_before,
                        "②命中（{revision} <= applied={applied_before}）时③必然也命中\
                         （seen={seen_before}）：这就是②不可被黑盒杀掉的原因"
                    );
                    assert_eq!(
                        verdict,
                        NotificationVerdict::Drop,
                        "同 epoch 且不高于已应用水位的通知只可能被丢弃"
                    );
                    rule2_hits += 1;
                }
                // 通知只作缓存失效信号：它推 `seen`，但**不得**动已应用水位。
                assert_eq!(
                    gate.applied_revision(),
                    applied_before,
                    "通知路径改动了已应用水位（{step:?}）"
                );
            }
        }
        assert!(
            gate.applied_revision() <= gate.seen_revision(),
            "不变量破了：applied={} > seen={}（{step:?} 之后）",
            gate.applied_revision(),
            gate.seen_revision()
        );
    }

    assert_eq!(
        (snapshots, notifications),
        (5, 12),
        "步骤表被改过就重新核对覆盖面（首跑正是这条先红：计数与表对不上）"
    );
    assert!(
        rule2_hits >= 2,
        "至少要真的走到两次「②命中」的判定，否则这条性质是空过（实际 {rule2_hits} 次）"
    );
    // 收尾状态：换库后的 epoch，水位线 = 那一代的权威版本。
    assert_eq!(gate.epoch(), Some(EPOCH_B));
    assert_eq!((gate.applied_revision(), gate.seen_revision()), (1, 2));
}

/// **乱序（迟到）的通知被丢弃，而且不得把已见水位拉回去。**
///
/// 它与规则④的「跳号 ⇒ 取新快照」是两件事：跳号是往**未来**看不见（缺口），
/// 迟到是往**过去**补一条——后者不是「新通知」，否则展示会被一条旧消息拉回去。
///
/// 判别力（P6 Task 3 的变异 A）：删掉 `revision <= seen_revision` 那条判据，
/// 迟到的 11 会被接纳并把 `seen_revision` 从 12 拉回 11，本用例两条断言同时红。
#[test]
fn out_of_order_notifications_are_dropped_without_rewinding_the_seen_watermark() {
    let mut gate = RevisionGate::new();
    gate.apply_snapshot(EPOCH_A, 10);
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 11)),
        NotificationVerdict::Apply
    );
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 12)),
        NotificationVerdict::Apply
    );
    assert_eq!((gate.applied_revision(), gate.seen_revision()), (10, 12));

    // 迟到的 11：比 `applied` 新，但比 `seen` 旧 ⇒ 它不是新通知。
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 11)),
        NotificationVerdict::Drop
    );
    assert_eq!(gate.seen_revision(), 12, "已见水位不得被拉回去");
    assert_eq!(
        gate.applied_revision(),
        10,
        "通知只作失效信号，不动已应用水位"
    );

    // 紧邻 seen 的后继不是跳号：照常接纳。
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 13)),
        NotificationVerdict::Apply
    );
    assert_eq!(gate.seen_revision(), 13);
}

/// **换库之后，迟到的旧 epoch 通知与旧 epoch 查询响应都不得把缓存切回旧库**（规则①的外沿）。
///
/// 「只触发重新握手，不直接接纳」对**响应**同样成立：一条旧 epoch 的业务响应就是
/// 00 §5 说的「未应用建议」——接纳它等于拿旧库的数据覆盖新库的展示。
///
/// 判别力：删掉 `on_notification` / `on_query_response` 的 epoch 判据（或把它挪到
/// 水位判据之后），本用例第一条断言就会红。
#[test]
fn a_late_response_from_a_retired_epoch_never_switches_the_cache_back() {
    let mut gate = RevisionGate::new();
    gate.apply_snapshot(EPOCH_A, 10);
    assert_eq!(
        gate.apply_snapshot(EPOCH_B, 1),
        SnapshotEffect::CacheInvalidated,
        "新 epoch 的权威快照使全部业务/计时缓存失效"
    );
    let watermarks = (gate.applied_revision(), gate.seen_revision());

    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 12)),
        NotificationVerdict::Rehandshake,
        "迟到的旧 epoch 通知只触发重新握手"
    );
    assert_eq!(
        gate.on_query_response(EPOCH_A, 12, 0),
        QueryVerdict::Rehandshake,
        "旧 epoch 的响应是「未应用建议」：不得接纳"
    );
    assert_eq!(
        gate.on_query_response("epoch-unknown", 12, 0),
        QueryVerdict::Rehandshake
    );

    assert_eq!(gate.epoch(), Some(EPOCH_B), "闸门仍停在当前 epoch");
    assert_eq!(
        (gate.applied_revision(), gate.seen_revision()),
        watermarks,
        "被拒的通知与响应一格水位都不该动"
    );
}

/// **旧查询响应不覆盖新状态**（00 §5 规则 3 的查询半边）。
///
/// 现场：一次查询在 revision 10 发出（它要求的版本就是当时的 10），期间窗口收到通知
/// 并拉到了 12 的权威快照；那条**迟到的响应**回来时水位线已经是 12——它必须被丢弃，
/// 而不是把展示拉回 10。与 `rule3_a_stale_query_response_never_overwrites` 的区别：
/// 这一条盯的是「新状态**已经建立**之后」到达的响应，且事后核对水位线一格未动。
///
/// 判别力（P6 Task 3 的变异 A）：删掉 `on_query_response` 的水位判据，本用例会红。
#[test]
fn a_late_query_response_never_overwrites_the_newer_applied_state() {
    let mut gate = RevisionGate::new();
    gate.apply_snapshot(EPOCH_A, 10);
    let in_flight_required = gate.applied_revision();

    // 期间：通知 + 新快照把状态推到 12。
    assert_eq!(
        gate.on_notification(&notification(EPOCH_A, 11)),
        NotificationVerdict::Apply
    );
    assert_eq!(gate.apply_snapshot(EPOCH_A, 12), SnapshotEffect::Applied);
    assert_eq!(gate.applied_revision(), 12);

    // 迟到的那条响应（revision 11，仍高于它自己要求的 10）：不得覆盖 12。
    assert_eq!(
        gate.on_query_response(EPOCH_A, 11, in_flight_required),
        QueryVerdict::Drop
    );
    // 与当前水位齐平或更新的响应照常接纳。
    assert_eq!(
        gate.on_query_response(EPOCH_A, 12, 12),
        QueryVerdict::Accept
    );
    assert_eq!(
        gate.on_query_response(EPOCH_A, 13, 12),
        QueryVerdict::Accept
    );
    assert_eq!(
        (gate.applied_revision(), gate.seen_revision()),
        (12, 12),
        "被丢弃的响应只读：水位线一格都不动"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 真库夹具：通知丢失 / 订阅者违约（走 `services::bootstrap::startup` 与真命令路径）
// ─────────────────────────────────────────────────────────────────────────────

/// 会「丢一条」的出口：`lose_next()` 之后的第一条投递返回 `Err`，之后恢复正常。
///
/// 为什么用 `Err` 而不是 dev 注入开关（`Broadcaster::arm_drop_next`，只在 debug 构建存在）：
/// 对客户端来说「通道坏了」与「通知在路上丢了」是同一种可观察——那条通知没到，
/// 而业务已经提交。`Err` 这条路径**发布构建里也存在**，更接近生产；注入开关自己的
/// 语义（按事件名筛、一次一发）由 `tests/dev_injections.rs` 钉住，这里不重复。
#[derive(Default)]
struct LossySink {
    seen: Mutex<Vec<EventEnvelope>>,
    /// 「下一条投递失败」的开关（方法与字段不同名，读起来才不绕）。
    failing: AtomicBool,
}

impl LossySink {
    fn lose_next(&self) {
        self.failing.store(true, Ordering::SeqCst);
    }

    fn envelopes(&self) -> Vec<EventEnvelope> {
        self.seen.lock().unwrap().clone()
    }

    fn revisions(&self) -> Vec<i64> {
        self.envelopes().iter().map(|e| e.revision).collect()
    }
}

impl EventSink for LossySink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        if self.failing.swap(false, Ordering::SeqCst) {
            return Err("webview gone".to_string());
        }
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

/// 只 panic 一次的出口：用来观察「订阅者违约」之后边界还在不在。
#[derive(Default)]
struct PanicOnceSink {
    seen: Mutex<Vec<EventEnvelope>>,
    /// 「下一条投递 panic」的开关。
    exploding: AtomicBool,
    panics: AtomicUsize,
}

impl PanicOnceSink {
    fn panic_next(&self) {
        self.exploding.store(true, Ordering::SeqCst);
    }

    fn revisions(&self) -> Vec<i64> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.revision)
            .collect()
    }
}

impl EventSink for PanicOnceSink {
    fn broadcast(&self, envelope: &EventEnvelope) -> Result<(), String> {
        if self.exploding.swap(false, Ordering::SeqCst) {
            self.panics.fetch_add(1, Ordering::SeqCst);
            panic!("注入的订阅者 panic（`EventSink` 的契约是返回 Err，不是 panic）");
        }
        self.seen.lock().unwrap().push(envelope.clone());
        Ok(())
    }
}

/// 真应用夹具（形状照 `tests/dev_injections.rs::launch`）：临时库 + 真命令路径。
/// 采样节拍设成 1 小时——本文件不测采样，别让心跳插进来写检查点。
struct Rig {
    _dir: tempfile::TempDir,
    running: Box<RunningApp>,
    epoch: String,
}

fn launch(sink: Arc<dyn EventSink>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("worktrace.db");
    let lock_path = dir.path().join("instance.lock");

    {
        let mut db = Db::open(&db_path).unwrap();
        migrate(db.connection()).unwrap();
        let tx = db.connection_mut().unchecked_transaction().unwrap();
        init_meta(&tx).unwrap();
        tx.commit().unwrap();
    }

    let mut config = StartupConfig::new(&db_path, &lock_path);
    config.sampling_interval_ms = 3_600_000;
    let running = match startup(
        config,
        Box::new(FakeClock::new(WALL, 0)),
        sink,
        &NoProbe,
        &|| -> Result<(), AppError> { Ok(()) },
    )
    .expect("启动应当成功")
    {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    };

    Rig {
        _dir: dir,
        epoch: running.data_epoch().to_string(),
        running,
    }
}

/// 某个标题的任务有几行：用来断言「业务真的提交了」（广播路径不碰这些行）。
fn task_rows(state: &AppState, title: &str) -> i64 {
    state
        .db()
        .unwrap()
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM task WHERE title = ?1",
            [title],
            |row| row.get(0),
        )
        .unwrap()
}

/// 一个窗口的握手：拿权威身份 → 应用快照 ⇒ 水位线从权威 `revision` 起算。
/// 返回闸门与基线版本；顺带核对握手读回的库身份与夹具一致。
fn connect(state: &mut AppState, epoch: &str) -> (RevisionGate, i64) {
    let identity = commands::get_revision_impl(state).unwrap();
    assert_eq!(identity.data_epoch, epoch, "握手读回的库身份");
    let mut gate = RevisionGate::new();
    assert_eq!(
        gate.apply_snapshot(&identity.data_epoch, identity.revision),
        SnapshotEffect::CacheInvalidated,
        "首次快照就是一次全量失效"
    );
    (gate, identity.revision)
}

/// **末次通知丢失之后，靠「至多 30 秒校验 `get_revision`」收敛**（00 §5 规则 4；F-020）。
///
/// 现场（真库 + 真命令路径）：窗口 B 先握手，另一个窗口写一笔业务，而那条
/// `domain.changed` 投递失败 ⇒ B 什么都没收到。逐条断言：
/// ① 已提交业务不回滚、命令也不返回失败（否则用户会重做一次**已经成功**的操作）；
/// ② B 的水位停在原处——通知是真的丢了，不是「假装的丢」；
/// ③ 丢掉的那条在下一次通知上表现为**跳号** ⇒ 规则④要求取新快照，不许猜；
/// ④ `get_revision`（可见窗口每 30 秒做的那次校验）读回权威 `epoch + revision`，
///    应用之后闸门回到连续：下一条通知照常接纳，不再要求重新同步；
/// ⑤ 迟到的窗口（重连）从权威身份起算 ⇒ 它收到的旧通知被丢弃，而不是被当成跳号。
///
/// 「至多 30 秒」这个节拍是**窗口侧**的契约（00 §5 规则 4；实机判据见
/// `manual-sync.md` §4），Rust 侧能钉的是**收敛输入是否充分**：`get_revision` 在同一个
/// 读事务里给出 epoch+revision，而且它自己不写库——轮询不得变成一次写入
/// （下面用 `total_changes()` 钉住）。
#[test]
fn a_lost_notification_converges_through_get_revision_without_rolling_back_the_write() {
    let sink = Arc::new(LossySink::default());
    let rig = launch(Arc::clone(&sink) as Arc<dyn EventSink>);
    let broadcaster = rig.running.broadcaster();
    let mut state = lock_app(rig.running.app());

    let (mut gate, baseline) = connect(&mut state, &rig.epoch);
    assert_eq!(baseline, 0, "新库的 revision 从 0 起");

    // ①②一笔写：广播失败 ⇒ B 收不到通知，业务照常提交、命令照常成功。
    sink.lose_next();
    let first = commands::create_task_impl(
        &mut state,
        broadcaster,
        CreateTaskRequest {
            expected_data_epoch: rig.epoch.clone(),
            title: "窗口 B 没收到通知".to_string(),
            project_id: None,
        },
    )
    .expect("广播失败不是业务失败：命令必须照常成功");
    assert_eq!(first.revision, baseline + 1, "业务已经提交");
    assert_eq!(
        task_rows(&state, "窗口 B 没收到通知"),
        1,
        "已提交的业务不会被广播失败回滚"
    );
    assert!(sink.revisions().is_empty(), "B 一条通知都没收到");
    assert!(broadcaster.diagnostics().failed >= 1, "失败被记成诊断");
    assert_eq!(broadcaster.diagnostics().sent, 0, "这条通道一次都没成功过");
    assert_eq!(
        (gate.applied_revision(), gate.seen_revision()),
        (baseline, baseline),
        "B 还停在旧版本：通知确实丢了"
    );

    // ③通道恢复后的下一笔 ⇒ B 直接看到 baseline+2 的通知：中间那条丢了 = 跳号。
    let second = commands::create_task_impl(
        &mut state,
        broadcaster,
        CreateTaskRequest {
            expected_data_epoch: rig.epoch.clone(),
            title: "第二笔".to_string(),
            project_id: None,
        },
    )
    .unwrap();
    assert_eq!(second.revision, baseline + 2);
    assert_eq!(sink.revisions(), vec![baseline + 2]);
    let arrived = sink.envelopes().pop().unwrap();
    assert_eq!(arrived.data_epoch, rig.epoch);
    assert_eq!(arrived.event, "domain.changed");
    assert_eq!(arrived.at, WALL, "时刻走时钟接缝（FakeClock）");
    assert_eq!(
        gate.on_notification(&arrived),
        NotificationVerdict::Resync,
        "丢了一条以后不能就地补课：必须取新快照"
    );

    // ④收敛：可见窗口那次「至多每 30 秒」的校验。
    let changes_before = state.db().unwrap().connection().total_changes();
    let polled = commands::get_revision_impl(&mut state).unwrap();
    assert_eq!(polled.data_epoch, rig.epoch);
    assert_eq!(polled.revision, second.revision, "轮询读回权威版本");
    assert_eq!(
        state.db().unwrap().connection().total_changes(),
        changes_before,
        "轮询自己不写库（每 30 秒一次，不能变成一次写入）"
    );
    assert_eq!(
        gate.apply_snapshot(&polled.data_epoch, polled.revision),
        SnapshotEffect::Applied
    );
    assert_eq!(gate.applied_revision(), polled.revision);
    assert!(
        gate.applied_revision() <= gate.seen_revision(),
        "②的不变量在收敛路径上同样成立"
    );

    // 收敛之后紧接着的通知照常接纳（缺口真的补上了，不是压着不报）。
    let third = commands::create_task_impl(
        &mut state,
        broadcaster,
        CreateTaskRequest {
            expected_data_epoch: rig.epoch.clone(),
            title: "第三笔".to_string(),
            project_id: None,
        },
    )
    .unwrap();
    assert_eq!(third.revision, baseline + 3);
    let arrived = sink.envelopes().pop().unwrap();
    assert_eq!(gate.on_notification(&arrived), NotificationVerdict::Apply);

    // ⑤迟到的窗口（重连）：从权威身份起算，握手之前产生的通知一律丢弃。
    let (mut late, late_baseline) = connect(&mut state, &rig.epoch);
    assert_eq!(late_baseline, third.revision);
    let stale =
        EventEnvelope::domain_changed(rig.epoch.clone(), baseline + 2, WALL, serde_json::json!({}));
    assert_eq!(late.on_notification(&stale), NotificationVerdict::Drop);
    assert_eq!(
        late.seen_revision(),
        late_baseline,
        "迟到通知不得把已见水位拉回去"
    );

    assert_eq!(broadcaster.diagnostics().out_of_order, 0, "广播按提交顺序");
}

/// **订阅者 panic**（违约，不是返回 `Err`）。
///
/// `EventSink::broadcast` 的契约是 `Result<(), String>`，「失败只记诊断」那条口径管的是
/// `Err`；panic 会展开穿过广播层。这一层**刻意不包 `catch_unwind`**：
/// `[profile.release]` 是 `panic = "abort"`（P6-2 的裁决），发布构建里捕不到，
/// 包一层只会给出虚假的保证。⇒ 本用例钉的不是「panic 怎么传播」，而是 00 §4 的两条
/// **后果**：
/// ① 已提交的业务不回滚（panic 发生在提交之后）；
/// ② 串行边界**被真的毒过之后**仍然可用——`AppBoundary` 那把锁对中毒是容忍的
///    （`lock_app` 取 `into_inner`），而不是「一次订阅者崩溃 = 应用废掉」。
///
/// ⚠️ ②要成立，**锁必须在 `catch_unwind` 的闭包内部取**（本用例就是这么写的）：
/// 那正是生产路径的形状——`run_command` 也是在 `spawn_blocking` 的闭包里面才
/// `lock_app`（`commands/mod.rs`），订阅者 panic 会真的毒掉那把锁。评审 Important 1
/// 指出过上一版的写法把 guard 留在**测试帧**里：panic 在闭包边界就被接住、guard 从未
/// 在展开路径上 drop ⇒ 锁根本没被毒，「中毒的锁不该让应用废掉」是一句**恒真断言**
/// （把 `lock_app` 的容忍改回 `unwrap()` 它也照样绿）。现在 guard 在闭包内，
/// 展开时 drop ⇒ 中毒是**结构性成立**的，而下面重新取锁走的正是那条容忍分支
/// （变异 D3 实测：把容忍改回 `unwrap()`，本用例红）。
///
/// ⚠️ **残留边界（登记，本任务不改）**：dev/test 档里这次 panic 经命令包装的
/// `spawn_blocking` 会变成一次内部错误（`STORAGE_ERROR` + `requires_handshake`），
/// 而业务其实已经提交——用户看到失败、事实是成功。断言那一层要 Tauri 运行时
/// （同 P6-13 登记的缺口）；release 档则是进程直接终止（P6-2 已裁决不动 profile）。
#[test]
fn a_panicking_subscriber_never_rolls_back_the_committed_business() {
    let sink = Arc::new(PanicOnceSink::default());
    let rig = launch(Arc::clone(&sink) as Arc<dyn EventSink>);
    let broadcaster = rig.running.broadcaster();

    sink.panic_next();
    // 注射的 panic 不必刷进测试日志；钩子是进程级的，所以窗口开得尽可能小。
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // 取锁在闭包**内部**：guard 会在展开路径上 drop ⇒ 这把锁真的被中毒
        // （与 `run_command` 的 `spawn_blocking` 闭包同一个形状）。
        let mut state = lock_app(rig.running.app());
        commands::create_task_impl(
            &mut state,
            broadcaster,
            CreateTaskRequest {
                expected_data_epoch: rig.epoch.clone(),
                title: "订阅者 panic 之前的那笔写".to_string(),
                project_id: None,
            },
        )
    }));
    std::panic::set_hook(hook);

    assert!(
        outcome.is_err(),
        "panic 越过广播层（这一层只把 Err 记成诊断）"
    );
    assert_eq!(sink.panics.load(Ordering::SeqCst), 1);

    // ②被毒过的那把锁还能取：这一行就是容忍分支（`unwrap_or_else(into_inner)`），
    // 改回 `unwrap()` 会在这里 panic ⇒ 本用例红。
    let mut state = lock_app(rig.running.app());
    assert_eq!(
        task_rows(&state, "订阅者 panic 之前的那笔写"),
        1,
        "panic 发生在提交之后：已提交的业务不得回滚"
    );
    let committed = commands::get_revision_impl(&mut state).unwrap().revision;
    assert_eq!(committed, 1, "那次写已经提交（revision 恰好 +1）");

    let next = commands::create_task_impl(
        &mut state,
        broadcaster,
        CreateTaskRequest {
            expected_data_epoch: rig.epoch.clone(),
            title: "订阅者 panic 之后的那笔写".to_string(),
            project_id: None,
        },
    )
    .expect("中毒的锁不该让应用废掉");
    assert_eq!(next.revision, committed + 1);
    assert_eq!(sink.revisions(), vec![next.revision], "恢复投递后的通知");
}
