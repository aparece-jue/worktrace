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

use std::sync::{Arc, Mutex};

use worktrace_lib::services::events::{
    Broadcaster, EmitOutcome, EventEnvelope, EventSink, NotificationVerdict, QueryVerdict,
    RevisionGate, SnapshotEffect,
};

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
    assert_eq!(seen, vec![1, 2, 3], "广播顺序就是提交顺序");
    assert_eq!(broadcaster.diagnostics().out_of_order, 0);
}
