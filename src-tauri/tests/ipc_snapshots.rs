//! IPC 响应 DTO 的**形状快照**（P7 Task 1，D1 裁决）。
//!
//! # 为什么是这条用例，而不是 DTO 生成器
//!
//! `ts-rs` / `specta` / `typeshare` 在本机两侧 cargo 缓存都是 **0 命中**，
//! 离线取不到，引入它们要联网并经用户确认。替代物就是这条用例：把每个响应 DTO
//! 序列化成 JSON，与仓库根 `src/types/__snapshots__/*.json` **逐字节比对**。
//! 改了 Rust 类型忘了改快照（以及 Task 1b 手写的 `src/types/ipc.ts`） ⇒ 这里红灯。
//!
//! # 快照在哪、怎么重新生成
//!
//! 路径是 `../src/types/__snapshots__/`（相对 `src-tauri/`，用 `CARGO_MANIFEST_DIR`
//! 定位，不依赖 `cargo test` 的当前目录）。**默认只比对**：文件缺失 = 红，
//! 不会静默创建。确实要改契约时显式重生成：
//!
//! ```text
//! WORKTRACE_UPDATE_IPC_SNAPSHOTS=1 cargo test --offline --test ipc_snapshots
//! ```
//!
//! 快照按 LF 存放在工作区（`.gitattributes` 对 `src/types/__snapshots__/*.json`
//! 钉了 `text eol=lf`）：本仓库 `core.autocrlf=true`，不钉的话换一台机器 checkout
//! 出来就是 CRLF，逐字节比对会在**没有改过任何类型**的情况下变红。
//!
//! # 覆盖范围
//!
//! 每个响应 DTO 一份快照 + 一条等式：`timer.tick` 事件的 `payload` **就是**
//! `TimerSnapshot` 的 JSON（不是第二份手写字段表）。请求类型不在快照里——它们是
//! 入参，命令层与 `src/types/ipc.ts` 按 `commands/mod.rs` 的字段名构造。

use std::path::{Path, PathBuf};

use worktrace_lib::domain::project::ProjectStatus;
use worktrace_lib::domain::session::{SessionMode, SessionState, TimerKind};
use worktrace_lib::domain::tag::TagKind;
use worktrace_lib::domain::task::TaskStatus;
use worktrace_lib::error::{AuthorityKind, ErrorAuthority, ErrorResponse, RecordVersion};
use worktrace_lib::services::catalog::{
    ProjectChange, ProjectList, TagChange, TagList, TaskChange, TaskProjectChange, TaskQueryResult,
    TaskTagsChange,
};
use worktrace_lib::services::daily_plan::{DailyPlanChange, DailyPlanView};
use worktrace_lib::services::events::timer_tick_payload;
use worktrace_lib::services::handshake::RevisionSnapshot;
use worktrace_lib::services::history::HistoryEditReport;
use worktrace_lib::services::recovery::ReconcileReport;
use worktrace_lib::services::stats::{
    CurrentTask, Measure, MeasureColumn, StatsClass, StatsRange, TodayView,
};
use worktrace_lib::services::tasks::TaskTransitionReport;
use worktrace_lib::services::timer::coordinator::CommandOutcome;
use worktrace_lib::services::timer::snapshot::TimerSnapshot;
use worktrace_lib::storage::project_repo::ProjectRow;
use worktrace_lib::storage::session_repo::{IntervalRow, SessionRow};
use worktrace_lib::storage::tag_repo::TagRow;
use worktrace_lib::storage::task_repo::TaskRow;

const EPOCH: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const TASK_ID: &str = "11111111-1111-4111-8111-111111111111";
const PROJECT_ID: &str = "22222222-2222-4222-8222-222222222222";
const TAG_ID: &str = "33333333-3333-4333-8333-333333333333";
const SESSION_ID: &str = "44444444-4444-4444-8444-444444444444";
const RUN_ID: &str = "55555555-5555-4555-8555-555555555555";
const INTERVAL_ID: &str = "66666666-6666-4666-8666-666666666666";
const AT: i64 = 1_700_000_000_000;
/// Today 样例的查询时区，以及 `AT` 在它里面的**真实**半开日界
/// （上海 2023-11-15 00:00 +08:00 → 次日零点）：`date` / `range` / `as_of` 三者自洽。
const TODAY_TZ: &str = "Asia/Shanghai";
const TODAY_FROM: i64 = 1_699_977_600_000;
const TODAY_TO: i64 = 1_700_064_000_000;

// ─────────────────────────────────────────────────────────────────────────────
// 固定的样例值：全部是常量，没有 UUID/时钟/随机数，同一次编译的产出逐字节稳定
// ─────────────────────────────────────────────────────────────────────────────

fn project_row() -> ProjectRow {
    ProjectRow {
        id: PROJECT_ID.to_string(),
        name: "Worktrace V0.1".to_string(),
        description: None,
        row_version: 2,
        status: ProjectStatus::Archived,
        created_at: AT,
        updated_at: AT + 60_000,
    }
}

fn tag_row() -> TagRow {
    TagRow {
        id: TAG_ID.to_string(),
        kind: TagKind::Context,
        name: "在家".to_string(),
        parent_id: None,
        row_version: 0,
        created_at: AT,
    }
}

fn task_row() -> TaskRow {
    TaskRow {
        id: TASK_ID.to_string(),
        project_id: Some(PROJECT_ID.to_string()),
        title: "写 P7 Task 1 的报告".to_string(),
        status: TaskStatus::Ready,
        quality: None,
        row_version: 3,
        created_at: AT,
        updated_at: AT + 120_000,
    }
}

/// 活动会话的快照：正计时以外的分支（`pending_ms` / 倒计时字段）都要有值。
///
/// `task_id` / `task_row_version` / `task_title`（P7 Task 3 的契约补口）取 `task_row()`
/// 的常量：快照里的任务版本**不要求**等于同一份 JSON 里别的任务行的版本（这里
/// `task_version` 是 3，`CommandOutcome.task_version` 是 4），它只要求形状稳定。
fn timer_snapshot_active() -> TimerSnapshot {
    TimerSnapshot {
        data_epoch: EPOCH.to_string(),
        revision: 7,
        run_id: RUN_ID.to_string(),
        session_id: Some(SESSION_ID.to_string()),
        session_version: Some(4),
        task_id: Some(TASK_ID.to_string()),
        task_row_version: Some(3),
        // 与 `task_row().title` 同一句话：它是**界面唯一的标题来源**（24 条命令里没有
        // 「按 id 取任务」的读路径），所以样例值取任务行那一份，不另编一个。
        task_title: Some("写 P7 Task 1 的报告".to_string()),
        tick_seq: 42,
        as_of: AT + 300_000,
        active_ms: 300_000,
        pending_ms: Some(5_000),
        state: Some(SessionState::Running),
        timer_kind: Some(TimerKind::Countdown),
        remaining_ms: Some(1_500_000),
        overtime_ms: Some(0),
    }
}

/// 空闲快照：`null` 分支也要钉住——前端必须把它当 `T | null` 处理。
fn timer_snapshot_idle() -> TimerSnapshot {
    TimerSnapshot::idle(EPOCH.to_string(), 7, RUN_ID.to_string(), 43, AT + 301_000)
}

/// 一列统计结果的固定四项（顺序与 [`Measure::ALL`] 一致）。
///
/// `cells` 是 `(ms, intervals)`：待确认栏允许 `ms: None`（该 measure 一条已知端点的
/// 候选都没有）——`null` 分支也要钉住，前端必须把它当 `number | null` 处理。
fn measure_columns(class: StatsClass, cells: [(Option<i64>, usize); 4]) -> Vec<MeasureColumn> {
    Measure::ALL
        .iter()
        .copied()
        .zip(cells)
        .map(|(measure, (ms, intervals))| MeasureColumn {
            class,
            measure,
            timezone: TODAY_TZ.to_string(),
            range: StatsRange {
                from: TODAY_FROM,
                to: TODAY_TO,
            },
            as_of: AT,
            data_epoch: EPOCH.to_string(),
            revision: 14,
            ms,
            intervals,
        })
        .collect()
}

/// Today 的五项 + 口径字段，全部是常量：（① `tasks` 与 ② `current` 都非空，
/// 三组工时各四项且 `class` 与所在组一致）。
fn today_view() -> TodayView {
    TodayView {
        tasks: vec![task_row()],
        current: Some(CurrentTask {
            session_id: SESSION_ID.to_string(),
            task_id: TASK_ID.to_string(),
            task_title: "写 P7 Task 1 的报告".to_string(),
            state: SessionState::Running,
        }),
        confirmed: measure_columns(
            StatsClass::Confirmed,
            [
                (Some(3_600_000), 3),
                (Some(1_200_000), 2),
                (Some(300_000), 1),
                (Some(600_000), 1),
            ],
        ),
        live: measure_columns(
            StatsClass::Live,
            [(Some(900_000), 1), (Some(0), 0), (Some(0), 0), (Some(0), 0)],
        ),
        pending: measure_columns(
            StatsClass::Pending,
            [
                // 零长度候选：有已知端点、跨度 0 ⇒ 计数但给 0。
                (Some(0), 1),
                // 终点未知的候选：不推算 ⇒ 这一 measure 整列不给毫秒。
                (None, 1),
                (Some(120_000), 1),
                (None, 0),
            ],
        ),
        date: "2023-11-15".to_string(),
        timezone: TODAY_TZ.to_string(),
        range: StatsRange {
            from: TODAY_FROM,
            to: TODAY_TO,
        },
        as_of: AT,
        data_epoch: EPOCH.to_string(),
        revision: 14,
    }
}

/// `work_session` 的一行（P8 Task 2a：恢复与历史的报告里直接装它交给 IPC）。
///
/// 样例是**一条已经结束的前台会话**：`state` / `mode` / `timer_kind` 三个枚举字段
/// 在这里首次进入快照，前端必须按落库字符串取值。
fn session_row() -> SessionRow {
    SessionRow {
        id: SESSION_ID.to_string(),
        task_id: TASK_ID.to_string(),
        run_id: RUN_ID.to_string(),
        mode: SessionMode::Foreground,
        state: SessionState::Finished,
        timer_kind: TimerKind::Stopwatch,
        // 正计时没有预算；倒计时才有（`ck_timer_budget`）。
        target_duration_ms: None,
        started_at: AT - 3_600_000,
        ended_at: Some(AT),
        needs_review: false,
        row_version: 5,
    }
}

/// `work_interval` 的一行（确认过、未作废）。
///
/// `sampled_end_wall_at` 为 `None`：**手工补录与用户确认都没有采样点**
/// （只有机器采样闭合的段才有），所以「`None`」是正常分支而不是缺失。
fn interval_row() -> IntervalRow {
    IntervalRow {
        id: INTERVAL_ID.to_string(),
        session_id: SESSION_ID.to_string(),
        started_at: AT - 3_600_000,
        ended_at: Some(AT),
        voided_at: None,
        duration_ms: Some(3_600_000),
        sampled_end_wall_at: None,
        needs_review: false,
    }
}

/// 被作废的一段：`voided_at` 非空是前端必须处理的**另一条分支**
/// （「整次作废」之后区间行仍在，只是不再计入任何工时）。
fn voided_interval_row() -> IntervalRow {
    IntervalRow {
        voided_at: Some(AT + 60_000),
        ..interval_row()
    }
}

fn cases() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "revision_snapshot",
            json(&RevisionSnapshot {
                data_epoch: EPOCH.to_string(),
                revision: 7,
            }),
        ),
        (
            "project_list",
            json(&ProjectList {
                items: vec![project_row()],
                data_epoch: EPOCH.to_string(),
                revision: 7,
            }),
        ),
        (
            "tag_list",
            json(&TagList {
                items: vec![tag_row()],
                data_epoch: EPOCH.to_string(),
                revision: 7,
            }),
        ),
        (
            "task_query_result",
            json(&TaskQueryResult {
                tasks: vec![task_row()],
                total: 1,
                data_epoch: EPOCH.to_string(),
                revision: 7,
            }),
        ),
        (
            "daily_plan_view",
            json(&DailyPlanView {
                tasks: vec![task_row()],
                data_epoch: EPOCH.to_string(),
                revision: 7,
            }),
        ),
        ("timer_snapshot", json(&timer_snapshot_active())),
        ("timer_snapshot_idle", json(&timer_snapshot_idle())),
        ("today_view", json(&today_view())),
        (
            "command_outcome",
            json(&CommandOutcome {
                snapshot: timer_snapshot_active(),
                revision: 8,
                task_version: 4,
            }),
        ),
        (
            "project_change",
            json(&ProjectChange {
                project: project_row(),
                revision: 8,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "task_project_change",
            json(&TaskProjectChange {
                task: task_row(),
                revision: 9,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "task_change",
            json(&TaskChange {
                task: task_row(),
                revision: 10,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "tag_change",
            json(&TagChange {
                tag: tag_row(),
                revision: 11,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "task_tags_change",
            json(&TaskTagsChange {
                tags: vec![tag_row()],
                revision: 12,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "daily_plan_change",
            json(&DailyPlanChange {
                tasks: vec![task_row()],
                revision: 13,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "error_response",
            json(&ErrorResponse {
                code: "VERSION_CONFLICT".to_string(),
                message: "这条记录已被修改，请刷新后重试。".to_string(),
                authority: Some(ErrorAuthority {
                    data_epoch: EPOCH.to_string(),
                    revision: 13,
                    records: vec![
                        RecordVersion {
                            kind: AuthorityKind::Task,
                            id: TASK_ID.to_string(),
                            row_version: Some(4),
                        },
                        // 已显式确认不存在：字段必须留着（`null`），不能省略。
                        RecordVersion {
                            kind: AuthorityKind::Project,
                            id: PROJECT_ID.to_string(),
                            row_version: None,
                        },
                    ],
                }),
                requires_handshake: false,
            }),
        ),
        // ── 恢复与历史（P8 Task 2a 的五条写命令） ──────────────────────────────
        //
        // 五条命令只有**三个**响应类型：`correct` / `backfill` / `discard_session`
        // 共用 `HistoryEditReport`。三条各出一份快照，是因为它们钉的是**不同分支**
        // ——照 `timer_snapshot` / `timer_snapshot_idle` 同一先例：
        //   `reconcile_report`        确认后的会话（`sampled_end_wall_at` 有值 / 零长度候选）；
        //   `correct_report`          重定时后的区间（时长跟着起止一起变）；
        //   `backfill_report`         新建的会话与区间（版本从 0 开始、没有采样点）；
        //   `discard_session_report`  作废整次（会话 `discarded`、区间 `voided_at` 非空）。
        (
            "reconcile_report",
            json(&ReconcileReport {
                session: SessionRow {
                    // 对账之后 `run_id` 已经切到本次 run、会话级待确认标记清假。
                    run_id: RUN_ID.to_string(),
                    needs_review: false,
                    ..session_row()
                },
                intervals: vec![
                    IntervalRow {
                        id: "77777777-7777-4777-8777-777777777777".to_string(),
                        // 机器采样闭合的可信前缀：这一条才有采样点。
                        sampled_end_wall_at: Some(AT - 3_500_000),
                        ..interval_row()
                    },
                    // 零长度候选被确认成零长度事实：`duration_ms = 0`，不是 `null`。
                    IntervalRow {
                        id: "88888888-8888-4888-8888-888888888888".to_string(),
                        started_at: AT,
                        ended_at: Some(AT),
                        duration_ms: Some(0),
                        ..interval_row()
                    },
                ],
                revision: 15,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "correct_report",
            json(&HistoryEditReport {
                // 修正区间事实会把所属会话的版本 +1（区间没有独立版本列）。
                session: SessionRow {
                    row_version: 6,
                    ..session_row()
                },
                interval: IntervalRow {
                    started_at: AT - 3_600_000,
                    ended_at: Some(AT - 60_000),
                    duration_ms: Some(3_540_000),
                    ..interval_row()
                },
                revision: 16,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "backfill_report",
            json(&HistoryEditReport {
                // 新建：会话版本从 0 开始，`run_id` 是补录时的 run。
                session: SessionRow {
                    needs_review: false,
                    row_version: 0,
                    ..session_row()
                },
                interval: interval_row(),
                revision: 17,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "discard_session_report",
            json(&HistoryEditReport {
                session: SessionRow {
                    // 整次作废：终态 `discarded`，终点的口径与 `finish` 的 `finished` 同级。
                    state: SessionState::Discarded,
                    ended_at: Some(AT + 60_000),
                    row_version: 7,
                    ..session_row()
                },
                interval: voided_interval_row(),
                revision: 18,
                data_epoch: EPOCH.to_string(),
            }),
        ),
        (
            "task_transition_report",
            json(&TaskTransitionReport {
                // 完成：任务行是提交后的样子，两个名单是同事务的联动事实。
                task: TaskRow {
                    status: TaskStatus::Done,
                    row_version: 6,
                    ..task_row()
                },
                ended_sessions: vec![SESSION_ID.to_string()],
                paused_sessions: vec!["99999999-9999-4999-8999-999999999999".to_string()],
                revision: 19,
                data_epoch: EPOCH.to_string(),
            }),
        ),
    ]
}

fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("响应 DTO 必须可序列化")
}

fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 一定有父目录（仓库根）")
        .join("src")
        .join("types")
        .join("__snapshots__")
}

fn updating() -> bool {
    std::env::var("WORKTRACE_UPDATE_IPC_SNAPSHOTS").is_ok()
}

/// 逐字节比对（或按显式开关重写）。返回 `Err` 是给调用方汇总用的诊断文本。
fn check(name: &str, value: &serde_json::Value) -> Result<(), String> {
    let dir = snapshot_dir();
    let path = dir.join(format!("{name}.json"));
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(value).expect("响应 DTO 必须可序列化")
    );

    if updating() {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("建不了快照目录 {}: {e}", dir.display()))?;
        std::fs::write(&path, &actual)
            .map_err(|e| format!("写不了快照 {}: {e}", path.display()))?;
        return Ok(());
    }

    let expected = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}：快照缺失或不可读（{e}）。这是前端要消费的契约，必须提交；\
             确认契约变更后用 WORKTRACE_UPDATE_IPC_SNAPSHOTS=1 重新生成",
            path.display()
        )
    })?;

    if expected == actual {
        return Ok(());
    }
    let (expected_hint, actual_hint) = first_difference(&expected, &actual);
    Err(format!(
        "{} 与 Rust 归一化 JSON 快照不一致（内容或格式发生变化）。\n  提交的快照: {expected_hint}\n  当前归一化结果: {actual_hint}\n\
         请先检查响应契约、JSON 键序及格式；确认变更后用 WORKTRACE_UPDATE_IPC_SNAPSHOTS=1 重新生成，契约变化时同步 src/types/ipc.ts",
        path.display(),
    ))
}

/// 第一处不同的行：`(期望, 实际)`，带上行号，输出不至于只有一句「不相等」。
fn first_difference(expected: &str, actual: &str) -> (String, String) {
    for (index, (left, right)) in expected.lines().zip(actual.lines()).enumerate() {
        if left != right {
            return (
                format!("第 {} 行 {left}", index + 1),
                format!("第 {} 行 {right}", index + 1),
            );
        }
    }
    (
        format!("多出 {} 行", expected.lines().count()),
        format!("多出 {} 行", actual.lines().count()),
    )
}

/// 每个响应 DTO 的 JSON 都必须与提交的快照逐字节一致。
///
/// 一次跑完所有 DTO 再汇总失败（改一个字段常常连带改好几个快照，逐条报红会来回好几轮）。
#[test]
fn every_response_dto_matches_its_committed_snapshot() {
    let mut failures = Vec::new();
    let mut names = Vec::new();
    for (name, value) in cases() {
        names.push(name);
        if let Err(detail) = check(name, &value) {
            failures.push(detail);
        }
    }

    assert!(
        names.len() >= 21,
        "快照用例至少要覆盖 21 个响应 DTO，实际 {}：{names:?}",
        names.len()
    );
    assert!(
        failures.is_empty(),
        "有 {} 份 IPC 快照与 Rust 类型不一致：\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// `timer.tick` 的 `payload` **就是** [`TimerSnapshot`] 的 JSON，不是第二份字段表。
///
/// ⚠️ **这是回归绊线，不是当前行为的证据**（P7 Task 1 fix round 1，评审 M2）：两边现在
/// 都走 `serde_json::to_value`，所以这条**恒真**。它的用处是将来——谁把
/// `timer_tick_payload` 改回手写 `json!`（或漏掉一个字段），这里立刻红。
/// 「载荷真的被发出去、字段真的到了客户端」这类外部证据在
/// `tests/event_protocol.rs` 与 `tests/periodic_sampling.rs`（逐字段读 `payload["…"]`）。
#[test]
fn the_tick_payload_is_exactly_the_timer_snapshot_json() {
    let snapshot = timer_snapshot_active();
    assert_eq!(
        timer_tick_payload(&snapshot),
        json(&snapshot),
        "tick 载荷必须由 TimerSnapshot 自己的 serde 形状产出"
    );
}
