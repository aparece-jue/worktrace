//! 去重协议的**两侧共读向量**（P7 Task 2，评审 M2 的另一半）。
//!
//! `src/types/__vectors__/revision-gate.json` 是规范判决的共享文本：
//!
//! - 本文件把它 replay 到 Rust 的 [`RevisionGate`]（**规范实现**）；
//! - `src/state/__tests__/revision-protocol.test.ts` 把同一串步骤 replay 到前端的
//!   `createFreshnessGate`（`src/ipc.ts`）。
//!
//! 改一条规则（换个比较符、合并一条分支、调换两条判据的顺序）⇒ 这一侧先红；
//! 照着改完向量之后，前端那一侧再红，直到两侧都改。**「必须同时改两侧」是机械的，
//! 不是靠人记得。** 反向也成立：前端改了规则而 Rust 没改，本文件会拦住。
//!
//! 向量刻意**不含**「还没应用过任何快照时的通知」：`RevisionGate::on_notification`
//! 在那格判 `Rehandshake`（`epoch == None` 分支），而前端的 `isUnknownEpoch` 把
//! 「还没应用过任何快照」定义为**不未知**（Task 1b 的既定语义，启动顺序让它不可达）。
//! 这是两侧唯一的一处字面差异，由前端的 `ipc.test.ts` / `domainState.test.ts` 钉住。
//!
//! 逐步断言之外还有两条防空过：场景数/步骤数下限、八个判决取值一个不少。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use worktrace_lib::services::events::{
    EventEnvelope, NotificationVerdict, QueryVerdict, RevisionGate, SnapshotEffect,
};

const AT: i64 = 1_700_000_000_000;

fn vectors_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 一定有父目录（仓库根）")
        .join("src")
        .join("types")
        .join("__vectors__")
        .join("revision-gate.json")
}

/// 判决 → 向量里的字符串（与 TS 侧同名）。
///
/// 故意写成**穷尽 match**：给枚举加一个变体，这里就编译不过——变体必须先有名字，
/// 才谈得上被向量覆盖。
fn effect_name(effect: SnapshotEffect) -> &'static str {
    match effect {
        SnapshotEffect::CacheInvalidated => "cache_invalidated",
        SnapshotEffect::Applied => "applied",
        SnapshotEffect::StaleIgnored => "stale_ignored",
    }
}

fn notification_name(verdict: NotificationVerdict) -> &'static str {
    match verdict {
        NotificationVerdict::Apply => "apply",
        NotificationVerdict::Drop => "drop",
        NotificationVerdict::Rehandshake => "rehandshake",
        NotificationVerdict::Resync => "resync",
    }
}

fn query_name(verdict: QueryVerdict) -> &'static str {
    match verdict {
        QueryVerdict::Accept => "accept",
        QueryVerdict::Drop => "drop",
        QueryVerdict::Rehandshake => "rehandshake",
    }
}

fn text<'a>(step: &'a serde_json::Value, field: &str, case: &str) -> &'a str {
    step[field]
        .as_str()
        .unwrap_or_else(|| panic!("{case}: 步骤缺少字符串字段 `{field}`"))
}

fn number(step: &serde_json::Value, field: &str, case: &str) -> i64 {
    step[field]
        .as_i64()
        .unwrap_or_else(|| panic!("{case}: 步骤缺少数字字段 `{field}`"))
}

fn expected<'a>(step: &'a serde_json::Value, case: &str) -> &'a str {
    text(step, "expect", case)
}

#[test]
fn the_revision_gate_replays_the_shared_vectors() {
    let path = vectors_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不了协议向量 {}：{e}", path.display()));
    let document: serde_json::Value = serde_json::from_str(&raw).expect("协议向量必须是合法 JSON");
    assert_eq!(
        document["protocol"].as_str(),
        Some("revision-gate"),
        "向量文件不是给 RevisionGate 用的那一份"
    );

    let cases = document["cases"].as_array().expect("向量必须有 cases 数组");
    assert!(
        cases.len() >= 6,
        "向量至少要覆盖四条规则的六个场景，现在只有 {}",
        cases.len()
    );

    let mut steps_run = 0usize;
    let mut outcomes: BTreeSet<&'static str> = BTreeSet::new();

    for case in cases {
        let name = case["name"].as_str().expect("每个场景都要有名字");
        let steps = case["steps"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: steps 必须是数组"));
        let mut gate = RevisionGate::new();

        for step in steps {
            match text(step, "op", name) {
                "apply_snapshot" => {
                    let effect = gate
                        .apply_snapshot(text(step, "epoch", name), number(step, "revision", name));
                    let actual = effect_name(effect);
                    outcomes.insert(actual);
                    assert_eq!(actual, expected(step, name), "{name}: 快照的处置对不上");
                }
                "notification" => {
                    let envelope = EventEnvelope::domain_changed(
                        text(step, "epoch", name),
                        number(step, "revision", name),
                        AT,
                        serde_json::json!({}),
                    );
                    let actual = notification_name(gate.on_notification(&envelope));
                    outcomes.insert(actual);
                    assert_eq!(actual, expected(step, name), "{name}: 通知的处置对不上");
                }
                "query_response" => {
                    let actual = query_name(gate.on_query_response(
                        text(step, "epoch", name),
                        number(step, "revision", name),
                        number(step, "required", name),
                    ));
                    outcomes.insert(actual);
                    assert_eq!(actual, expected(step, name), "{name}: 查询响应的处置对不上");
                }
                "state" => {
                    assert_eq!(gate.epoch(), step["epoch"].as_str(), "{name}: epoch 对不上");
                    assert_eq!(
                        gate.applied_revision(),
                        number(step, "applied", name),
                        "{name}: 已应用水位对不上"
                    );
                    assert_eq!(
                        gate.seen_revision(),
                        number(step, "seen", name),
                        "{name}: 已见版本对不上"
                    );
                }
                other => panic!("{name}: 未知的 op `{other}`——向量文件与这份 replay 对不上"),
            }
            steps_run += 1;
        }
    }

    assert!(steps_run >= 40, "向量步骤太少（{steps_run}），防空过");
    let expected_outcomes: BTreeSet<&str> = [
        "accept",
        "applied",
        "apply",
        "cache_invalidated",
        "drop",
        "rehandshake",
        "resync",
        "stale_ignored",
    ]
    .into_iter()
    .collect();
    let actual_outcomes: BTreeSet<&str> = outcomes.into_iter().collect();
    assert_eq!(
        actual_outcomes, expected_outcomes,
        "八个判决取值必须被向量全部走到（少一个就说明有分支没被覆盖）"
    );
}
