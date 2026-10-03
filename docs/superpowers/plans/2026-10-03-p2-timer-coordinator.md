# P2 · 计时协调器与检查点 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 做出一个 `&mut self` 串行的计时协调器：它在内存里持有归属基线 `A(M)`，用同一份时钟采样生成区间事实与 tick 快照，约每 30 秒写一次 `interval_checkpoint`，并在墙钟与单调钟出现分歧时把区间切成「可信前缀 + 待确认余段」。

**Architecture:** `services/` 层第一次出现，位置在 `commands/` 与 `storage/` 之间。协调器是**唯一**碰计时内存状态的地方：所有改状态的方法都取 `&mut self`，因此进程内由借用检查器保证不会交错，跨命令由调用方（P7 的 Tauri state）用 `Mutex` 包一层。时钟一律经 `platform::clock::Clock` 注入，测试用 P1 建好的 `FakeClock`。协调器不认识 SQLite，只持有 `&Connection` 句柄转交仓储。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· 无新依赖

**Spec:**
- `docs/superpowers/specs/2026-10-02-worktrace-architecture/08-implementation-contracts.zh.md` §1（区间时长与时间归属）、§7（连续归属基线与阶段命令）、§8（`timer_kind` 字段矩阵）
- `.../00-architecture.zh.md` §5（数据代次、revision 与计时协议）
- `.../02-data-model.zh.md` §3（工作会话与计时）、§6（统计口径）
- `.../04-functional-spec.zh.md` F-006、F-007

**依赖的前置计划：** P1（`2026-10-03-worktrace-v01-foundation.md`）。本计划直接引用 P1 产出的
`platform::clock::{Clock, FakeClock}`、`storage::db::Db`、`storage::meta::{read_meta, bump_revision}`、
`storage::session_repo::{create_session, pause_session, interval_facts}`、`commands::envelope::AppError`、
`domain::session::{SessionState, SessionMode, TimerKind}`。**这些签名不得改动**；若实现时发现需要改，先更新计划总纲。

## Global Constraints

继承计划总纲 §5 的 7 条，另加本计划特有的：

- **两项阈值检查各自独立**（08 §1 与 §7）：相邻采样增量差、以及墙钟相对当前基线的**累计偏差**，**任一**绝对值 > 2000ms 即触发异常路径。只查相邻增量抓不住"每次 100ms 的缓慢漂移"。
- **心跳成功不重置基线**（08 §7 原话：「心跳成功不重置 anchor，防止小偏差累计而一直逃过检测」）。这是上一条能生效的前提。
- **阈值内的小幅校时不改基线、不重写历史**（08 §7）：因此相邻区间不会因 500ms 墙钟偏差重叠。
- **`duration_ms` 是校验值，不能独立编辑**（08 §1）：可信闭合区间必须满足 `duration_ms = ended_at - started_at`，且都是非负整数毫秒。原始墙钟关闭值存进 `sampled_end_wall_at` 只作诊断，**不拿它直接减开始时间**。
- **运行暂计必须来自同一协调器快照**（08 §1、02 §3）：`active_ms = SUM(可信闭合 interval.duration_ms) + 协调器当前单调增量`；**不得另取 `Date.now()`**。
- **提交失败不应用内存变更**（00 §5）：DB 事务失败时协调器的内存基线必须回退到调用前。
- **`tick_seq` 每个 run 内递增**（00 §5），tick 与查询返回同一展示序列基线。
- **心跳、tick、查询都不增加业务 revision**（00 §5）；只有改变区间或状态才增加。
- **本计划不做**：崩溃扫描与四类判定（P3）、`reconcile`/`correct`（P3）、IPC 命令与 `Mutex` 接线（P7）、番茄钟阶段（V0.2）。

---

## 文件结构

```
src-tauri/src/
  services/
    mod.rs                    建：服务层出口
    timer/
      mod.rs                  建：计时服务出口
      anchor.rs               建：归属基线 A(M) 与阈值分类（纯逻辑，无 IO）
      snapshot.rs             建：TimerSnapshot DTO 与字段口径
      coordinator.rs          建：串行协调器（唯一持有计时内存状态的地方）
  storage/
    checkpoint_repo.rs        建：interval_checkpoint 的读写
  lib.rs                      改：声明 services 模块
```

`anchor.rs` 是纯函数，不碰数据库也不碰时钟类型——它只收数字，因此可以穷举边界。`coordinator.rs` 是唯一把基线、仓储、时钟缝在一起的地方。

---

### Task 1: services 模块骨架与计时快照 DTO

**Files:**
- Create: `src-tauri/src/services/mod.rs`, `src-tauri/src/services/timer/mod.rs`, `src-tauri/src/services/timer/snapshot.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `domain::session::{SessionState, TimerKind}`（P1）
- Produces:
  - `services::timer::snapshot::TimerSnapshot`，字段与类型：`data_epoch: String`、`run_id: String`、`session_id: String`、`session_version: i64`、`tick_seq: i64`、`as_of_wall_ms: i64`、`active_ms: i64`、`state: SessionState`、`timer_kind: TimerKind`、`remaining_ms: Option<i64>`、`overtime_ms: Option<i64>`
  - `TimerSnapshot::is_countdown(&self) -> bool`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/services/timer/snapshot.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{SessionState, TimerKind};

    fn snap(kind: TimerKind) -> TimerSnapshot {
        TimerSnapshot {
            data_epoch: "epoch-a".into(),
            run_id: "run-1".into(),
            session_id: "s1".into(),
            session_version: 3,
            tick_seq: 7,
            as_of_wall_ms: 1_700_000_000_000,
            active_ms: 42_000,
            state: SessionState::Running,
            timer_kind: kind,
            remaining_ms: None,
            overtime_ms: None,
        }
    }

    #[test]
    fn countdown_carries_remaining_and_overtime() {
        let mut s = snap(TimerKind::Countdown);
        s.remaining_ms = Some(1_000);
        s.overtime_ms = Some(0);
        assert!(s.is_countdown());
        assert_eq!(s.remaining_ms, Some(1_000));
    }

    #[test]
    fn stopwatch_leaves_budget_fields_empty() {
        // 08 §8 的字段矩阵：stopwatch 的 remaining/overtime 为 null
        let s = snap(TimerKind::Stopwatch);
        assert!(!s.is_countdown());
        assert_eq!(s.remaining_ms, None);
        assert_eq!(s.overtime_ms, None);
    }

    #[test]
    fn snapshot_carries_the_full_envelope() {
        // 00 §5：tick 与查询都必须带 epoch/run/session/版本/序号
        let s = snap(TimerKind::Stopwatch);
        assert!(!s.data_epoch.is_empty());
        assert!(!s.run_id.is_empty());
        assert!(!s.session_id.is_empty());
        assert!(s.session_version >= 0);
        assert!(s.tick_seq >= 0);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib services`
Expected: 编译失败，`cannot find type TimerSnapshot`。

- [ ] **Step 3: 实现骨架与 DTO**

`src-tauri/src/services/timer/snapshot.rs`：

```rust
//! 计时快照 DTO。
//!
//! 字段集合来自 00 §5 的计时信封与 08 §8 的字段矩阵：**tick 与查询返回同一个
//! 形状**，前端只认这一个结构，避免"tick 少一个字段、查询多一个字段"的漂移。

use crate::domain::session::{SessionState, TimerKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerSnapshot {
    /// 库身份。前端据此丢弃恢复前的旧响应（00 §5）。
    pub data_epoch: String,
    pub run_id: String,
    pub session_id: String,
    /// = work_session.row_version，用来丢弃暂停/切换前生成的旧 tick。
    pub session_version: i64,
    /// 每个 run 内递增；前端只做展示序列比较，不用它推导状态跃迁。
    pub tick_seq: i64,
    /// 采样时刻的挂钟毫秒。仅用于展示与诊断，不参与时长计算。
    pub as_of_wall_ms: i64,
    /// = SUM(可信闭合 interval.duration_ms) + 协调器当前单调增量。
    pub active_ms: i64,
    pub state: SessionState,
    pub timer_kind: TimerKind,
    /// 仅 countdown 非空（08 §8）。
    pub remaining_ms: Option<i64>,
    /// 仅 countdown 非空。
    pub overtime_ms: Option<i64>,
}

impl TimerSnapshot {
    pub fn is_countdown(&self) -> bool {
        matches!(self.timer_kind, TimerKind::Countdown)
    }
}
```

`src-tauri/src/services/mod.rs`：

```rust
//! 服务层：编排领域与存储。不得绕过 storage 直连 SQL。

pub mod timer;
```

`src-tauri/src/services/timer/mod.rs`：

```rust
//! 计时服务：归属基线、检查点与串行协调器。

pub mod snapshot;
```

`src-tauri/src/lib.rs` 加一行 `pub mod services;`（放在 `pub mod platform;` 与 `pub mod storage;` 之间，保持声明的字母序与分层顺序一致）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib services`
Expected: `test result: ok. 3 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/ src-tauri/src/lib.rs
git commit -m "feat(m04): 服务层骨架与计时快照 DTO"
```

---

### Task 2: 归属基线与阈值检测（纯逻辑）

**Files:**
- Create: `src-tauri/src/services/timer/anchor.rs`
- Modify: `src-tauri/src/services/timer/mod.rs`

**Interfaces:**
- Consumes: 无（纯数字进、纯数字出）
- Produces:
  - `services::timer::anchor::THRESHOLD_MS: i64 = 2000`
  - `services::timer::anchor::Anchor { wall_at: i64, monotonic_at: i64 }`，方法 `attribution_at(&self, m: i64) -> i64`、`deviation(&self, sampled_wall_ms: i64, m: i64) -> i64`
  - `services::timer::anchor::Sample { wall_ms: i64, monotonic_ms: i64 }`
  - `services::timer::anchor::Anomaly`（`CumulativeDrift { deviation_ms }` / `AdjacentJump { delta_ms }` / `Backward` / `SystemEvent`）
  - `services::timer::anchor::classify(anchor: &Anchor, prev: &Sample, next: &Sample, system_event: bool) -> Option<Anomaly>`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/services/timer/anchor.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> Anchor {
        Anchor { wall_at: 1_700_000_000_000, monotonic_at: 1_000 }
    }

    #[test]
    fn attribution_follows_monotonic_not_wall() {
        // A(M) = anchor_wall + (M - anchor_monotonic)
        let a = anchor();
        assert_eq!(a.attribution_at(1_000), 1_700_000_000_000);
        assert_eq!(a.attribution_at(61_000), 1_700_000_060_000);
        // 墙钟改了也不影响：A 只看 M
        assert_eq!(a.attribution_at(61_000), a.attribution_at(61_000));
    }

    #[test]
    fn small_clock_correction_inside_threshold_is_accepted() {
        // 08 §7：阈值内小幅校时不改基线、不重写历史，所以 500ms 偏差放行
        let a = anchor();
        let prev = Sample { wall_ms: 1_700_000_030_000, monotonic_ms: 31_000 };
        let next = Sample { wall_ms: 1_700_000_060_500, monotonic_ms: 61_000 };
        assert_eq!(classify(&a, &prev, &next, false), None);
    }

    #[test]
    fn cumulative_drift_is_caught_even_when_each_step_is_small() {
        // 这条是核心：每次只漂 100ms，相邻增量差永远很小，
        // 但累计到基线已经偏了 3 秒，必须被抓到。
        let a = anchor();
        let mut prev = Sample { wall_ms: a.wall_at, monotonic_ms: a.monotonic_at };
        let mut caught = None;
        for step in 1..=30 {
            let next = Sample {
                wall_ms: a.wall_at + step * 30_000 + step * 100, // 每步多 100ms
                monotonic_ms: a.monotonic_at + step * 30_000,
            };
            if let Some(an) = classify(&a, &prev, &next, false) {
                caught = Some(an);
                break;
            }
            prev = next;
        }
        match caught {
            Some(Anomaly::CumulativeDrift { deviation_ms }) => assert!(deviation_ms.abs() > THRESHOLD_MS),
            other => panic!("应因累计偏差被抓到，实际 {other:?}"),
        }
    }

    #[test]
    fn adjacent_jump_is_caught_on_its_own() {
        // 相邻增量差自己也要能触发，不能只靠累计偏差
        let a = anchor();
        let prev = Sample { wall_ms: 1_700_000_030_000, monotonic_ms: 31_000 };
        let next = Sample { wall_ms: 1_700_000_093_000, monotonic_ms: 61_000 }; // 墙钟跳了 63s
        match classify(&a, &prev, &next, false) {
            Some(Anomaly::AdjacentJump { delta_ms }) => assert!(delta_ms.abs() > THRESHOLD_MS),
            other => panic!("应因相邻跳变被抓到，实际 {other:?}"),
        }
    }

    #[test]
    fn backward_wall_clock_is_always_anomalous() {
        let a = anchor();
        let prev = Sample { wall_ms: 1_700_000_030_000, monotonic_ms: 31_000 };
        let next = Sample { wall_ms: 1_700_000_020_000, monotonic_ms: 61_000 };
        assert_eq!(classify(&a, &prev, &next, false), Some(Anomaly::Backward));
    }

    #[test]
    fn trusted_system_event_is_anomalous_without_any_threshold() {
        let a = anchor();
        let prev = Sample { wall_ms: 1_700_000_030_000, monotonic_ms: 31_000 };
        let next = Sample { wall_ms: 1_700_000_060_000, monotonic_ms: 61_000 };
        assert_eq!(classify(&a, &prev, &next, true), Some(Anomaly::SystemEvent));
    }

    #[test]
    fn exactly_at_threshold_is_not_anomalous() {
        // 规格写的是"大于 2000ms"，等于不触发
        let a = anchor();
        let prev = Sample { wall_ms: a.wall_at, monotonic_ms: a.monotonic_at };
        let next = Sample {
            wall_ms: a.wall_at + 30_000 + THRESHOLD_MS,
            monotonic_ms: a.monotonic_at + 30_000,
        };
        // 累计偏差正好 2000ms、相邻差也正好 2000ms
        assert_eq!(classify(&a, &prev, &next, false), None);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib anchor`
Expected: 编译失败，`cannot find type Anchor`。

- [ ] **Step 3: 实现 anchor**

```rust
//! 归属基线与异常分类（08 §1、§7）。
//!
//! 纯逻辑：只收数字、只回数字，因此可以穷举边界，也便于在没有数据库与
//! 真实时钟的情况下测试"每分钟漂 100ms、半小时后才被发现"这类场景。

pub const THRESHOLD_MS: i64 = 2000;

/// 一个连续、可信 run 段的内存基线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub wall_at: i64,
    pub monotonic_at: i64,
}

impl Anchor {
    pub fn new(wall_ms: i64, monotonic_ms: i64) -> Self {
        Self { wall_at: wall_ms, monotonic_at: monotonic_ms }
    }

    /// 归属终点 A(M) = anchor_wall_at + (M - anchor_monotonic)。
    ///
    /// 关键在于它**只看单调时钟**：区间归属因此不会被墙钟的小幅校时影响，
    /// 相邻区间也就不会重叠（08 §7）。
    pub fn attribution_at(&self, monotonic_ms: i64) -> i64 {
        self.wall_at + (monotonic_ms - self.monotonic_at)
    }

    /// 墙钟相对基线的累计偏差：sampled_wall - A(M)。
    pub fn deviation(&self, sampled_wall_ms: i64, monotonic_ms: i64) -> i64 {
        sampled_wall_ms - self.attribution_at(monotonic_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub wall_ms: i64,
    pub monotonic_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anomaly {
    /// 墙钟相对基线的累计偏差超阈值。慢漂移只能靠这条发现。
    CumulativeDrift { deviation_ms: i64 },
    /// 相邻两次采样的墙钟增量与单调增量之差超阈值。
    AdjacentJump { delta_ms: i64 },
    /// 墙钟倒退。
    Backward,
    /// 收到可信的系统异常事件（休眠/时钟变更）。
    SystemEvent,
}

/// 08 §7：每次采样算 `abs(sampled_wall_at - A)` 并检查相邻增量，
/// **任一超过 2000ms** 或收到可信系统事件即进入异常路径。
pub fn classify(anchor: &Anchor, prev: &Sample, next: &Sample, system_event: bool) -> Option<Anomaly> {
    if system_event {
        return Some(Anomaly::SystemEvent);
    }
    if next.wall_ms < prev.wall_ms {
        return Some(Anomaly::Backward);
    }

    let cumulative = anchor.deviation(next.wall_ms, next.monotonic_ms);
    if cumulative.abs() > THRESHOLD_MS {
        return Some(Anomaly::CumulativeDrift { deviation_ms: cumulative });
    }

    let wall_delta = next.wall_ms - prev.wall_ms;
    let mono_delta = next.monotonic_ms - prev.monotonic_ms;
    let adjacent = wall_delta - mono_delta;
    if adjacent.abs() > THRESHOLD_MS {
        return Some(Anomaly::AdjacentJump { delta_ms: adjacent });
    }

    None
}
```

`services/timer/mod.rs` 加 `pub mod anchor;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib anchor`
Expected: `test result: ok. 7 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/timer/
git commit -m "feat(m04): 归属基线与两项阈值检测"
```

---

### Task 3: 区间检查点仓储

**Files:**
- Create: `src-tauri/src/storage/checkpoint_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`

**Interfaces:**
- Consumes: P1 的 `Db`/`StorageError`/`AppError`；P1 建好的 `interval_checkpoint` 表
- Produces:
  - `storage::checkpoint_repo::Checkpoint { interval_id: String, run_id: String, wall_at: i64, attribution_at: i64, elapsed_ms: i64 }`
  - `storage::checkpoint_repo::write(conn, &Checkpoint) -> Result<(), AppError>` —— 同事务 UPSERT，**不增加业务 revision**
  - `storage::checkpoint_repo::latest(conn, interval_id: &str) -> Result<Option<Checkpoint>, AppError>`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/storage/checkpoint_repo.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Db;
    use crate::storage::meta::{init_meta, read_meta};
    use crate::storage::migrations::migrate;

    fn seeded() -> Db {
        let db = Db::open_in_memory().unwrap();
        migrate(db.conn()).unwrap();
        init_meta(db.conn(), "epoch-a").unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO application_run(id, started_at) VALUES ('run-1', 0);
                 INSERT INTO task(id, title, status, row_version, created_at, updated_at)
                   VALUES ('t1', 'T', 'Doing', 0, 0, 0);
                 INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind,
                                          started_at, row_version)
                   VALUES ('s1', 't1', 'run-1', 'FOREGROUND', 'running', 'stopwatch', 0, 0);
                 INSERT INTO work_interval(id, session_id, started_at) VALUES ('i1', 's1', 0);",
            )
            .unwrap();
        db
    }

    fn cp(wall: i64, attrib: i64, elapsed: i64) -> Checkpoint {
        Checkpoint {
            interval_id: "i1".into(),
            run_id: "run-1".into(),
            wall_at: wall,
            attribution_at: attrib,
            elapsed_ms: elapsed,
        }
    }

    #[test]
    fn write_then_latest_round_trips() {
        let db = seeded();
        let c = cp(1_700_000_030_000, 1_700_000_030_000, 30_000);
        write(db.conn(), &c).unwrap();
        assert_eq!(latest(db.conn(), "i1").unwrap(), Some(c));
    }

    #[test]
    fn write_is_an_upsert_keyed_by_interval() {
        // 检查点是"最后一个可信映射"，同一区间只保留一条
        let db = seeded();
        write(db.conn(), &cp(1_700_000_030_000, 1_700_000_030_000, 30_000)).unwrap();
        write(db.conn(), &cp(1_700_000_060_000, 1_700_000_060_000, 60_000)).unwrap();

        let n: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM interval_checkpoint", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "同一 interval 只应有一条检查点");
        assert_eq!(latest(db.conn(), "i1").unwrap().unwrap().elapsed_ms, 60_000);
    }

    #[test]
    fn checkpoint_write_does_not_bump_business_revision() {
        // 00 §5：心跳与检查点不加业务 revision
        let db = seeded();
        let before = read_meta(db.conn()).unwrap().revision;
        write(db.conn(), &cp(1_700_000_030_000, 1_700_000_030_000, 30_000)).unwrap();
        write(db.conn(), &cp(1_700_000_060_000, 1_700_000_060_000, 60_000)).unwrap();
        assert_eq!(read_meta(db.conn()).unwrap().revision, before);
    }

    #[test]
    fn latest_is_none_for_an_unknown_interval() {
        let db = seeded();
        assert_eq!(latest(db.conn(), "nope").unwrap(), None);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib checkpoint_repo`
Expected: 编译失败，`cannot find type Checkpoint`。

- [ ] **Step 3: 实现 checkpoint_repo**

```rust
//! interval_checkpoint 的读写（08 §1）。
//!
//! 检查点是"最后一次成功持久化的可信时钟映射"，恢复时只认它。
//! 写入**不增加业务 revision**（00 §5）——心跳不是业务变化。

use crate::commands::envelope::AppError;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub interval_id: String,
    pub run_id: String,
    pub wall_at: i64,
    pub attribution_at: i64,
    pub elapsed_ms: i64,
}

pub fn write(conn: &Connection, c: &Checkpoint) -> Result<(), AppError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO interval_checkpoint(interval_id, run_id, wall_at, attribution_at, elapsed_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(interval_id) DO UPDATE SET
           run_id = excluded.run_id,
           wall_at = excluded.wall_at,
           attribution_at = excluded.attribution_at,
           elapsed_ms = excluded.elapsed_ms",
        rusqlite::params![c.interval_id, c.run_id, c.wall_at, c.attribution_at, c.elapsed_ms],
    )?;
    // 刻意不调用 bump_revision
    tx.commit()?;
    Ok(())
}

pub fn latest(conn: &Connection, interval_id: &str) -> Result<Option<Checkpoint>, AppError> {
    Ok(conn
        .query_row(
            "SELECT interval_id, run_id, wall_at, attribution_at, elapsed_ms
               FROM interval_checkpoint WHERE interval_id = ?1",
            [interval_id],
            |r| {
                Ok(Checkpoint {
                    interval_id: r.get(0)?,
                    run_id: r.get(1)?,
                    wall_at: r.get(2)?,
                    attribution_at: r.get(3)?,
                    elapsed_ms: r.get(4)?,
                })
            },
        )
        .optional()?)
}
```

`storage/mod.rs` 加 `pub mod checkpoint_repo;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib checkpoint_repo`
Expected: `test result: ok. 4 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/checkpoint_repo.rs src-tauri/src/storage/mod.rs
git commit -m "feat(m01): interval_checkpoint 仓储（不增加业务 revision）"
```

---

### Task 4: 协调器骨架与 start

**Files:**
- Create: `src-tauri/src/services/timer/coordinator.rs`
- Modify: `src-tauri/src/services/timer/mod.rs`

**Interfaces:**
- Consumes: Task 1 的 `TimerSnapshot`、Task 2 的 `Anchor`、Task 3 的 `checkpoint_repo`、P1 的 `session_repo::create_session`、`storage::meta::read_meta`、`platform::clock::Clock`
- Produces:
  - `services::timer::coordinator::Coordinator`，方法：
    - `new() -> Self`
    - `start(&mut self, conn: &Connection, clock: &dyn Clock, run_id: &str, task_id: &str, mode: SessionMode, timer_kind: TimerKind, target_duration_ms: Option<i64>) -> Result<TimerSnapshot, AppError>`
    - `snapshot(&self, conn: &Connection, clock: &dyn Clock) -> Result<TimerSnapshot, AppError>`（信封字段齐全；`active_ms` 与预算字段在 Task 9 补成完整口径）
    - `active_session(&self) -> Option<&str>`、`anchor(&self) -> Option<Anchor>`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/services/timer/coordinator.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{SessionMode, SessionState, TimerKind};
    use crate::platform::clock::FakeClock;
    use crate::storage::db::Db;
    use crate::storage::meta::init_meta;
    use crate::storage::migrations::migrate;

    pub(crate) fn seeded() -> Db {
        let db = Db::open_in_memory().unwrap();
        migrate(db.conn()).unwrap();
        init_meta(db.conn(), "epoch-a").unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO application_run(id, started_at) VALUES ('run-1', 1000);
                 INSERT INTO task(id, title, status, row_version, created_at, updated_at)
                   VALUES ('t1', 'T', 'Doing', 0, 0, 0);",
            )
            .unwrap();
        db
    }

    #[test]
    fn start_creates_a_running_session_and_establishes_the_anchor() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();

        let s = c
            .start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None)
            .unwrap();

        assert_eq!(s.state, SessionState::Running);
        assert_eq!(s.tick_seq, 0);
        assert_eq!(s.data_epoch, "epoch-a");
        assert_eq!(s.run_id, "run-1");

        let a = c.anchor().expect("start 必须建立基线");
        assert_eq!(a.wall_at, 1_700_000_000_000);
        assert_eq!(a.monotonic_at, 5_000);
        assert_eq!(c.active_session(), Some(s.session_id.as_str()));

        // 区间事实也建好了：一个开放区间
        let n: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM work_interval WHERE ended_at IS NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn start_is_rejected_while_another_session_is_active() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        let err = c
            .start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None)
            .unwrap_err();
        assert!(
            err.code() == "STORAGE_ERROR" || err.code() == "DOMAIN_ERROR",
            "第二条前台 running 必须被拒，实际 {}",
            err.code()
        );
    }

    #[test]
    fn countdown_start_requires_a_target() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        let err = c
            .start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Countdown, None)
            .unwrap_err();
        assert_eq!(err.code(), "DOMAIN_ERROR", "倒计时必须有 target_duration_ms");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib coordinator`
Expected: 编译失败，`cannot find type Coordinator`。

- [ ] **Step 3: 实现协调器骨架与 start**

```rust
//! 串行计时协调器（00 §5）。
//!
//! **唯一**持有计时内存状态的地方。所有改状态的方法都取 `&mut self`：
//! 进程内由借用检查器保证不会交错，跨命令由调用方用 `Mutex` 包（P7 接线）。
//!
//! 状态变更的顺序按 00 §5 固定：同一时钟采样 → 生成区间事实 → 提交 DB
//! 并增加 session_version → 应用内存基线。**提交失败绝不应用内存变更。**

use crate::commands::envelope::{guard_epoch, AppError};
use crate::domain::error::DomainError;
use crate::domain::session::{SessionMode, SessionState, TimerKind};
use crate::platform::clock::Clock;
use crate::services::timer::anchor::Anchor;
use crate::services::timer::snapshot::TimerSnapshot;
use crate::storage::meta::read_meta;
use crate::storage::session_repo;
use rusqlite::Connection;

pub struct Coordinator {
    active_session: Option<String>,
    active_interval: Option<String>,
    anchor: Option<Anchor>,
    /// 当前开放区间开始时的**单调**读数。只在内存里——单调时钟跨重启不复用，
    /// 持久化它没有意义（08 §1「单调 Instant 本身不持久化」）。
    interval_started_monotonic: Option<i64>,
    /// 上一次**被接受**的采样。异常采样不推进它（08 §7）。
    last_sample_wall: Option<i64>,
    last_sample_monotonic: Option<i64>,
    /// 本次会话的计时类型与预算，供 snapshot 决定 remaining/overtime 是否为 null。
    timer_kind: TimerKind,
    target_duration_ms: Option<i64>,
    tick_seq: i64,
    run_id: Option<String>,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl Coordinator {
    pub fn new() -> Self {
        Self {
            active_session: None,
            active_interval: None,
            anchor: None,
            interval_started_monotonic: None,
            last_sample_wall: None,
            last_sample_monotonic: None,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            tick_seq: 0,
            run_id: None,
        }
    }

    pub fn active_session(&self) -> Option<&str> {
        self.active_session.as_deref()
    }

    pub fn active_interval(&self) -> Option<String> {
        self.active_interval.clone()
    }

    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor
    }

    pub fn tick_seq(&self) -> i64 {
        self.tick_seq
    }

    pub fn last_sample_monotonic(&self) -> Option<i64> {
        self.last_sample_monotonic
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        conn: &Connection,
        clock: &dyn Clock,
        run_id: &str,
        task_id: &str,
        mode: SessionMode,
        timer_kind: TimerKind,
        target_duration_ms: Option<i64>,
    ) -> Result<TimerSnapshot, AppError> {
        if self.active_session.is_some() {
            return Err(AppError::Domain(DomainError::SessionAlreadyActive));
        }
        if matches!(timer_kind, TimerKind::Countdown) && target_duration_ms.is_none() {
            return Err(AppError::Domain(DomainError::CountdownRequiresTarget));
        }

        // 同一次时钟采样生成所有事实（00 §5：不允许采样之后再取一次时间）
        let wall = clock.wall_ms();
        let mono = clock.monotonic_ms();
        let epoch = read_meta(conn)?.data_epoch;
        guard_epoch(conn, &epoch)?;

        let sid = session_repo::create_session(conn, task_id, run_id, mode, timer_kind, wall)?;
        let interval_id: String = conn.query_row(
            "SELECT id FROM work_interval WHERE session_id = ?1 AND ended_at IS NULL",
            [&sid],
            |r| r.get(0),
        )?;
        let session_version: i64 =
            conn.query_row("SELECT row_version FROM work_session WHERE id = ?1", [&sid], |r| r.get(0))?;

        // 提交成功之后才应用内存状态
        self.active_session = Some(sid.clone());
        self.active_interval = Some(interval_id);
        self.anchor = Some(Anchor::new(wall, mono));
        self.interval_started_monotonic = Some(mono);
        self.last_sample_wall = Some(wall);
        self.last_sample_monotonic = Some(mono);
        self.timer_kind = timer_kind;
        self.target_duration_ms = target_duration_ms;
        self.run_id = Some(run_id.to_string());
        self.tick_seq = 0;

        Ok(TimerSnapshot {
            data_epoch: epoch,
            run_id: run_id.to_string(),
            session_id: sid,
            session_version,
            tick_seq: 0,
            as_of_wall_ms: wall,
            active_ms: 0,
            state: SessionState::Running,
            timer_kind,
            remaining_ms: target_duration_ms,
            overtime_ms: target_duration_ms.map(|_| 0),
        })
    }

    /// 最小快照。`active_ms` 与预算字段在 Task 9 补全成完整口径；
    /// 本任务只保证信封字段（epoch/run/session/版本/序号）齐全。
    pub fn snapshot(&self, conn: &Connection, clock: &dyn Clock) -> Result<TimerSnapshot, AppError> {
        let sid = self
            .active_session
            .clone()
            .ok_or(AppError::Domain(DomainError::SessionAlreadyActive))?;
        let meta = read_meta(conn)?;
        let (state_raw, session_version): (String, i64) = conn.query_row(
            "SELECT state, row_version FROM work_session WHERE id = ?1",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(TimerSnapshot {
            data_epoch: meta.data_epoch,
            run_id: self.run_id.clone().unwrap_or_default(),
            session_id: sid,
            session_version,
            tick_seq: self.tick_seq,
            as_of_wall_ms: clock.wall_ms(),
            active_ms: 0,
            state: SessionState::from_str(&state_raw)?,
            timer_kind: self.timer_kind,
            remaining_ms: None,
            overtime_ms: None,
        })
    }
}
```

`services/timer/mod.rs` 加 `pub mod coordinator;`。

**注意**：上面用到的 `DomainError::SessionAlreadyActive` 与 `DomainError::CountdownRequiresTarget` 是本计划新加的。Step 4 之前先在 `src-tauri/src/domain/error.rs` 补上：

```rust
    #[error("a session is already active")]
    SessionAlreadyActive,

    #[error("a countdown session requires target_duration_ms")]
    CountdownRequiresTarget,
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib coordinator`
Expected: `test result: ok. 3 passed`。`countdown_start_requires_a_target` 断言的 `DOMAIN_ERROR` 来自 `AppError::Domain` 的 `code()` 映射（P1 已建）。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/timer/ src-tauri/src/domain/error.rs
git commit -m "feat(m04): 协调器骨架与 start"
```

---

### Task 5: 关闭区间的时间归属

**Files:**
- Modify: `src-tauri/src/services/timer/coordinator.rs`

**Interfaces:**
- Consumes: Task 4 的 `Coordinator`、Task 2 的 `Anchor`
- Produces:
  - `Coordinator::pause(&mut self, conn, clock) -> Result<TimerSnapshot, AppError>`

- [ ] **Step 1: 写失败的测试**

加进 `coordinator.rs` 的测试模块：

```rust
    #[test]
    fn close_computes_duration_from_monotonic_and_keeps_the_raw_wall_sample() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        // 墙钟与单调钟一起走 60 秒，再加 300ms 的阈值内校时
        clock.advance_monotonic_ms(60_000);
        clock.set_wall_ms(1_700_000_060_300);

        let s = c.pause(db.conn(), &clock).unwrap();
        assert_eq!(s.state, SessionState::Paused);

        let (started, ended, dur, sampled): (i64, i64, i64, i64) = db
            .conn()
            .query_row(
                "SELECT started_at, ended_at, duration_ms, sampled_end_wall_at
                   FROM work_interval WHERE ended_at IS NOT NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();

        // 08 §1：可信闭合满足 duration_ms = ended_at - started_at，且都是非负整数毫秒
        assert_eq!(dur, ended - started);
        assert_eq!(dur, 60_000, "时长取单调差，不受 300ms 墙钟校时影响");
        assert_eq!(sampled, 1_700_000_060_300, "原始墙钟关闭值原样保留作诊断");
        assert_ne!(sampled, ended, "不得拿原始墙钟直接当归属终点");
    }

    #[test]
    fn consecutive_intervals_do_not_overlap_under_small_clock_skew() {
        // 08 §7 的核心收益：阈值内的小幅校时不改基线，所以暂停再开不会重叠
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        clock.advance_monotonic_ms(60_000);
        clock.set_wall_ms(1_700_000_060_500); // 墙钟多走 500ms
        c.pause(db.conn(), &clock).unwrap();

        clock.advance_monotonic_ms(120_000);
        c.resume(db.conn(), &clock).unwrap();

        let rows: Vec<(i64, Option<i64>)> = db
            .conn()
            .prepare("SELECT started_at, ended_at FROM work_interval ORDER BY started_at")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(rows.len(), 2);
        let first_end = rows[0].1.expect("第一条应已闭合");
        let second_start = rows[1].0;
        assert!(
            second_start >= first_end,
            "相邻区间不得重叠：第一条结束 {first_end}，第二条开始 {second_start}"
        );
    }

    #[test]
    fn pause_without_an_active_session_is_rejected() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        assert!(c.pause(db.conn(), &clock).is_err());
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib coordinator`
Expected: 编译失败，`no method named pause`。

- [ ] **Step 3: 实现 pause 与 resume**

在 `coordinator.rs` 的 `impl Coordinator` 里加：

```rust
    /// 关闭当前区间。时长取**单调差**，归属终点取 `started_at + 本区间单调工作增量`。
    pub fn pause(
        &mut self,
        conn: &Connection,
        clock: &dyn Clock,
    ) -> Result<TimerSnapshot, AppError> {
        let sid = self.require_session()?;
        let interval_id = self.require_interval()?;
        // 区间的归属终点由 started_at + 本区间单调增量得出，
        // 基线本身在这一步不需要参与计算——它保留下来的意义在 heartbeat 与 resume。
        let _ = self.require_anchor()?;

        let wall = clock.wall_ms();
        let mono = clock.monotonic_ms();

        // 归属终点按基线推出，不取独立墙钟（08 §7）
        let started_at: i64 = conn.query_row(
            "SELECT started_at FROM work_interval WHERE id = ?1",
            [&interval_id],
            |r| r.get(0),
        )?;
        let mono_at_start = self
            .interval_started_monotonic
            .ok_or(AppError::Domain(DomainError::RunningSessionInvariant))?;
        let duration_ms = mono - mono_at_start;
        let ended_at = started_at + duration_ms;

        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE work_interval
                SET ended_at = ?1, duration_ms = ?2, sampled_end_wall_at = ?3, needs_review = 0
              WHERE id = ?4",
            rusqlite::params![ended_at, duration_ms, wall, interval_id],
        )?;
        tx.execute(
            "UPDATE work_session SET state = 'paused', row_version = row_version + 1 WHERE id = ?1",
            [&sid],
        )?;
        crate::storage::meta::bump_revision(&tx)?;
        tx.commit()?;

        // 提交成功后才动内存：清开放区间，但**保留基线**
        // （08 §7：正常暂停不清基线，段间暂停时间也包含在 A 的推进里）
        self.active_interval = None;
        self.interval_started_monotonic = None;

        self.snapshot(conn, clock)
    }

    /// 开一个新的工作区间，仍属同一 session（02 §3：暂停后恢复仍是同一 session）。
    pub fn resume(
        &mut self,
        conn: &Connection,
        clock: &dyn Clock,
    ) -> Result<TimerSnapshot, AppError> {
        let sid = self.require_session()?;
        if self.active_interval.is_some() {
            return Err(AppError::Domain(DomainError::RunningSessionInvariant));
        }
        let wall = clock.wall_ms();
        let mono = clock.monotonic_ms();

        let tx = conn.unchecked_transaction()?;
        let new_interval = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO work_interval(id, session_id, started_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![new_interval, sid, wall],
        )?;
        tx.execute(
            "UPDATE work_session SET state = 'running', row_version = row_version + 1 WHERE id = ?1",
            [&sid],
        )?;
        crate::storage::meta::bump_revision(&tx)?;
        tx.commit()?;

        self.active_interval = Some(new_interval);
        self.interval_started_monotonic = Some(mono);
        self.snapshot(conn, clock)
    }

    fn require_session(&self) -> Result<String, AppError> {
        self.active_session
            .clone()
            .ok_or(AppError::Domain(DomainError::SessionAlreadyActive))
    }

    fn require_interval(&self) -> Result<String, AppError> {
        self.active_interval
            .clone()
            .ok_or(AppError::Domain(DomainError::RunningSessionInvariant))
    }

    fn require_anchor(&self) -> Result<Anchor, AppError> {
        self.anchor
            .ok_or(AppError::Domain(DomainError::RunningSessionInvariant))
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib coordinator`
Expected: `test result: ok. 6 passed`。`consecutive_intervals_do_not_overlap_under_small_clock_skew` 必须绿——它是 04 的 F-007 验收项里「500ms 墙钟偏差下暂停/继续不重叠」的自动化版本。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/timer/coordinator.rs
git commit -m "feat(m04): 关闭区间按单调差归属，含 500ms 偏差不重叠验证"
```

---

### Task 6: 心跳、检查点与累计偏差检测

**Files:**
- Modify: `src-tauri/src/services/timer/coordinator.rs`

**Interfaces:**
- Consumes: Task 2 的 `anchor::classify`、Task 3 的 `checkpoint_repo`
- Produces:
  - `Coordinator::heartbeat(&mut self, conn, clock, system_event: bool) -> Result<HeartbeatOutcome, AppError>`
  - `services::timer::coordinator::HeartbeatOutcome`（`Idle` / `Checkpointed { elapsed_ms }` / `Anomalous(Anomaly)`）
  - `Coordinator::HEARTBEAT_INTERVAL_MS: i64 = 30_000`

- [ ] **Step 1: 写失败的测试**

```rust
    #[test]
    fn heartbeat_writes_a_checkpoint_with_the_attribution_endpoint() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        let s = c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        clock.advance_monotonic_ms(30_000);
        let out = c.heartbeat(db.conn(), &clock, false).unwrap();
        match out {
            HeartbeatOutcome::Checkpointed { elapsed_ms } => assert_eq!(elapsed_ms, 30_000),
            other => panic!("应写检查点，实际 {other:?}"),
        }

        let cp = crate::storage::checkpoint_repo::latest(db.conn(), &c.active_interval().unwrap())
            .unwrap()
            .expect("应有一条检查点");
        assert_eq!(cp.run_id, "run-1");
        assert_eq!(cp.attribution_at, 1_700_000_030_000, "归属终点由基线推出");
        assert_eq!(cp.wall_at, 1_700_000_030_000);
        assert_eq!(cp.elapsed_ms, 30_000);
        let _ = s;
    }

    #[test]
    fn heartbeat_before_the_interval_does_nothing() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        clock.advance_monotonic_ms(1_000); // 还没到 30 秒
        assert_eq!(c.heartbeat(db.conn(), &clock, false).unwrap(), HeartbeatOutcome::Idle);
    }

    #[test]
    fn heartbeat_does_not_reset_the_anchor() {
        // 08 §7：心跳成功不重置 anchor，否则小偏差累计永远逃过检测
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
        let before = c.anchor().unwrap();

        clock.advance_monotonic_ms(30_000);
        clock.set_wall_ms(1_700_000_030_000 + 1_500); // 单次偏差仍在阈值内
        c.heartbeat(db.conn(), &clock, false).unwrap();

        assert_eq!(c.anchor().unwrap(), before, "基线不得被心跳改动");
    }

    #[test]
    fn slow_drift_is_caught_after_enough_heartbeats() {
        // 每次心跳只漂 300ms，单次永远不超阈值；累计到 2100ms 必须被抓到
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        let mut caught = false;
        for step in 1..=10 {
            clock.advance_monotonic_ms(30_000);
            clock.set_wall_ms(1_700_000_000_000 + step * 30_000 + step * 300);
            if let HeartbeatOutcome::Anomalous(_) = c.heartbeat(db.conn(), &clock, false).unwrap() {
                caught = true;
                break;
            }
        }
        assert!(caught, "累计偏差必须最终被抓到（心跳不重置基线是前提）");
    }

    #[test]
    fn system_event_is_anomalous_immediately() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
        clock.advance_monotonic_ms(30_000);
        assert!(matches!(
            c.heartbeat(db.conn(), &clock, true).unwrap(),
            HeartbeatOutcome::Anomalous(_)
        ));
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib coordinator`
Expected: 编译失败，`no method named heartbeat`。

- [ ] **Step 3: 实现 heartbeat**

```rust
    pub const HEARTBEAT_INTERVAL_MS: i64 = 30_000;

    /// 约每 30 秒一次的采样。返回是否写了检查点、或是否发现异常。
    ///
    /// 关键顺序（08 §7）：**先检测、再写检查点**。异常之后不得先把可疑采样
    /// 写成可信检查点，否则恢复时会拿一个坏映射当锚。
    pub fn heartbeat(
        &mut self,
        conn: &Connection,
        clock: &dyn Clock,
        system_event: bool,
    ) -> Result<HeartbeatOutcome, AppError> {
        let interval_id = match self.active_interval.clone() {
            Some(i) => i,
            None => return Ok(HeartbeatOutcome::Idle), // 暂停中：没有开放区间，不采样
        };
        let anchor = self.require_anchor()?;

        let prev_mono = self.last_sample_monotonic.unwrap_or(anchor.monotonic_at);
        let prev_wall = self.last_sample_wall.unwrap_or(anchor.wall_at);
        let mono = clock.monotonic_ms();
        if mono - prev_mono < Self::HEARTBEAT_INTERVAL_MS {
            return Ok(HeartbeatOutcome::Idle);
        }

        let wall = clock.wall_ms();
        let prev = Sample { wall_ms: prev_wall, monotonic_ms: prev_mono };
        let next = Sample { wall_ms: wall, monotonic_ms: mono };

        if let Some(anomaly) = classify(&anchor, &prev, &next, system_event) {
            // 不更新 last_sample、不写检查点：把可疑采样挡在可信记录之外
            return Ok(HeartbeatOutcome::Anomalous(anomaly));
        }

        let elapsed_ms = (mono - self.interval_started_monotonic.unwrap_or(mono)).max(0);
        let cp = Checkpoint {
            interval_id: interval_id.clone(),
            run_id: self.run_id.clone().unwrap_or_default(),
            wall_at: wall,
            attribution_at: anchor.attribution_at(mono),
            elapsed_ms,
        };
        crate::storage::checkpoint_repo::write(conn, &cp)?;

        self.last_sample_wall = Some(wall);
        self.last_sample_monotonic = Some(mono);
        // 刻意不动 self.anchor —— 08 §7 明令

        Ok(HeartbeatOutcome::Checkpointed { elapsed_ms })
    }
```

字段与访问器（`last_sample_wall`、`last_sample_monotonic`、`active_interval()`）在 Task 4 已经定义好，本任务直接用。

模块顶部加：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    Idle,
    Checkpointed { elapsed_ms: i64 },
    Anomalous(Anomaly),
}
```

并把 `use` 补全：`anchor::{classify, Anchor, Anomaly, Sample}`、`storage::checkpoint_repo::Checkpoint`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib coordinator`
Expected: `test result: ok. 11 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/timer/coordinator.rs
git commit -m "feat(m04): 心跳写检查点，先检测后落盘且不重置基线"
```

---

### Task 7: 异常分割——可信前缀与待确认余段

**Files:**
- Modify: `src-tauri/src/services/timer/coordinator.rs`

**Interfaces:**
- Consumes: Task 6 的 `heartbeat`
- Produces:
  - `Coordinator::split_on_anomaly(&mut self, conn, clock) -> Result<SplitOutcome, AppError>`
  - `services::timer::coordinator::SplitOutcome { trusted_interval_id: Option<String>, pending_interval_id: String, pending_from: i64 }`

- [ ] **Step 1: 写失败的测试**

```rust
    #[test]
    fn anomaly_keeps_the_trusted_prefix_and_marks_the_remainder() {
        // 有检查点：闭合前缀按检查点归属，余段标 needs_review
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        clock.advance_monotonic_ms(30_000);
        c.heartbeat(db.conn(), &clock, false).unwrap();   // 可信检查点在 +30s

        clock.advance_monotonic_ms(30_000);
        clock.set_wall_ms(1_700_000_000_000 + 90_000);    // 累积偏差 30s，触发异常
        assert!(matches!(c.heartbeat(db.conn(), &clock, false).unwrap(), HeartbeatOutcome::Anomalous(_)));

        let out = c.split_on_anomaly(db.conn(), &clock).unwrap();
        assert!(out.trusted_interval_id.is_some(), "应保留可信前缀");

        let rows: Vec<(String, i64, Option<i64>, i64)> = db
            .conn()
            .prepare("SELECT id, started_at, ended_at, needs_review FROM work_interval ORDER BY started_at")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(rows.len(), 2, "应切成前缀与余段两条");
        assert!(rows[0].2.is_some(), "前缀必须闭合");
        assert_eq!(rows[0].3, 0, "前缀是可信的");
        assert!(rows[1].2.is_none(), "余段保持开放");
        assert_eq!(rows[1].3, 1, "余段必须标 needs_review");
        assert_eq!(rows[1].1, out.pending_from);
    }

    #[test]
    fn anomaly_without_any_checkpoint_marks_the_whole_interval_pending() {
        // 08 §1：没有可信检查点则整个当前区间待确认
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        clock.advance_monotonic_ms(30_000);
        clock.set_wall_ms(1_700_000_000_000 + 90_000);
        assert!(matches!(c.heartbeat(db.conn(), &clock, false).unwrap(), HeartbeatOutcome::Anomalous(_)));

        let out = c.split_on_anomaly(db.conn(), &clock).unwrap();
        assert!(out.trusted_interval_id.is_none(), "没有可信检查点就没有前缀");

        let (open, review): (i64, i64) = db
            .conn()
            .query_row(
                "SELECT count(*), COALESCE(SUM(needs_review), 0) FROM work_interval WHERE ended_at IS NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(open, 1);
        assert_eq!(review, 1, "整个当前区间标 needs_review");
    }

    #[test]
    fn the_trusted_prefix_is_never_double_counted() {
        // 08 §1：可信前缀不因异常被重复计入——分裂后前缀的时长是确定的
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        clock.advance_monotonic_ms(30_000);
        c.heartbeat(db.conn(), &clock, false).unwrap();
        clock.advance_monotonic_ms(30_000);
        clock.set_wall_ms(1_700_000_000_000 + 90_000);
        c.heartbeat(db.conn(), &clock, false).unwrap();
        c.split_on_anomaly(db.conn(), &clock).unwrap();

        // 再调一次不应再切一刀
        c.split_on_anomaly(db.conn(), &clock).unwrap();
        let n: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM work_interval", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "重复调用不得重复分割");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib coordinator`
Expected: 编译失败，`no method named split_on_anomaly`。

- [ ] **Step 3: 实现 split_on_anomaly**

```rust
    /// 把当前区间切成「可信前缀（闭合、needs_review=0）」与「余段（开放、needs_review=1）」。
    ///
    /// 切点取**最后一个成功持久化的检查点**的归属终点；没有检查点则整个区间
    /// 标待确认、不产生前缀（08 §1）。两次调用是幂等的：已经切过就不再切。
    pub fn split_on_anomaly(
        &mut self,
        conn: &Connection,
        clock: &dyn Clock,
    ) -> Result<SplitOutcome, AppError> {
        let interval_id = self.require_interval()?;
        let checkpoint = crate::storage::checkpoint_repo::latest(conn, &interval_id)?;

        let started_at: i64 = conn.query_row(
            "SELECT started_at FROM work_interval WHERE id = ?1",
            [&interval_id],
            |r| r.get(0),
        )?;

        match checkpoint {
            Some(cp) => {
                let prefix_ms = (cp.attribution_at - started_at).max(0);
                let tx = conn.unchecked_transaction()?;
                // 前缀：闭合、可信
                tx.execute(
                    "UPDATE work_interval
                        SET ended_at = ?1, duration_ms = ?2, needs_review = 0
                      WHERE id = ?3",
                    rusqlite::params![cp.attribution_at, prefix_ms, interval_id],
                )?;
                // 余段：开放、待确认
                let pending_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO work_interval(id, session_id, started_at, needs_review)
                     VALUES (?1, ?2, ?3, 1)",
                    rusqlite::params![pending_id, self.require_session()?, cp.attribution_at],
                )?;
                crate::storage::meta::bump_revision(&tx)?;
                tx.commit()?;

                let mono = clock.monotonic_ms();
                self.active_interval = Some(pending_id.clone());
                self.interval_started_monotonic = Some(mono);
                self.last_sample_wall = Some(clock.wall_ms());
                self.last_sample_monotonic = Some(mono);

                Ok(SplitOutcome {
                    trusted_interval_id: Some(interval_id),
                    pending_interval_id: pending_id,
                    pending_from: cp.attribution_at,
                })
            }
            None => {
                // 没有可信检查点：整段待确认，不产生前缀
                let tx = conn.unchecked_transaction()?;
                tx.execute(
                    "UPDATE work_interval SET needs_review = 1 WHERE id = ?1",
                    [&interval_id],
                )?;
                crate::storage::meta::bump_revision(&tx)?;
                tx.commit()?;

                let mono = clock.monotonic_ms();
                self.last_sample_wall = Some(clock.wall_ms());
                self.last_sample_monotonic = Some(mono);

                Ok(SplitOutcome {
                    trusted_interval_id: None,
                    pending_interval_id: interval_id,
                    pending_from: started_at,
                })
            }
        }
    }
```

模块顶部加：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOutcome {
    pub trusted_interval_id: Option<String>,
    pub pending_interval_id: String,
    pub pending_from: i64,
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib coordinator`
Expected: `test result: ok. 14 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/timer/coordinator.rs
git commit -m "feat(m04): 异常时分割可信前缀与待确认余段"
```

---

### Task 8: 新 run / 休眠后重建基线

**Files:**
- Modify: `src-tauri/src/services/timer/coordinator.rs`

**Interfaces:**
- Consumes: Task 2 的 `Anchor`
- Produces:
  - `Coordinator::rebuild_anchor(&mut self, clock: &dyn Clock, run_id: &str)`
  - `Coordinator::attribution_conflicts(&self, conn, candidate_start_ms: i64) -> Result<Vec<(String, i64, i64)>, AppError>` —— 返回与候选起点重叠的可信区间

- [ ] **Step 1: 写失败的测试**

```rust
    #[test]
    fn a_new_run_does_not_inherit_the_previous_monotonic_clock() {
        // 08 §7：新 run、休眠后或显式校正后不能继承 Instant
        let db = seeded();
        let c1 = FakeClock::new(1_700_000_000_000, 5_000);
        let mut coord = Coordinator::new();
        coord.start(db.conn(), &c1, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
        let old = coord.anchor().unwrap();

        // 新的进程 run：挂钟继续，但单调时钟从 0 重新开始
        let c2 = FakeClock::new(1_700_000_600_000, 0);
        coord.rebuild_anchor(&c2, "run-2");

        let new = coord.anchor().unwrap();
        assert_eq!(new.monotonic_at, 0);
        assert_eq!(new.wall_at, 1_700_000_600_000);
        assert_ne!(new, old, "不得继承旧基线");
        // 新基线立刻可用，且它的 A(M) 与当前挂钟一致
        assert_eq!(new.attribution_at(0), 1_700_000_600_000);
    }

    #[test]
    fn rebuild_resets_the_sample_window() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut coord = Coordinator::new();
        coord.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
        coord.rebuild_anchor(&clock, "run-2");
        // 重建后相邻增量比较的基准是重建点，不是上一段
        assert_eq!(coord.last_sample_monotonic(), Some(5_000));
    }

    #[test]
    fn attribution_conflicts_lists_overlapping_trusted_intervals() {
        // 08 §7：重建基线后要"验证与历史及待确认记录的归属冲突"
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
        clock.advance_monotonic_ms(60_000);
        c.pause(db.conn(), &clock).unwrap();

        // 候选起点落在已闭合区间 [W0, W0+60000) 之内 → 冲突
        let conflicts = c.attribution_conflicts(db.conn(), 1_700_000_030_000).unwrap();
        assert_eq!(conflicts.len(), 1, "应报出一处冲突");

        // 恰好接在端点之后 → 半开区间不算冲突
        let none = c.attribution_conflicts(db.conn(), 1_700_000_060_000).unwrap();
        assert!(none.is_empty(), "半开区间端点相接不算重叠");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib coordinator`
Expected: 编译失败，`no method named rebuild_anchor`。

- [ ] **Step 3: 实现重建与冲突检查**

```rust
    /// 重建归属基线。调用时机：新 run、休眠唤醒、显式校正之后（08 §7）。
    pub fn rebuild_anchor(&mut self, clock: &dyn Clock, run_id: &str) {
        let wall = clock.wall_ms();
        let mono = clock.monotonic_ms();
        self.anchor = Some(Anchor::new(wall, mono));
        self.last_sample_wall = Some(wall);
        self.last_sample_monotonic = Some(mono);
        self.run_id = Some(run_id.to_string());
    }

    pub fn last_sample_monotonic(&self) -> Option<i64> {
        self.last_sample_monotonic
    }

    /// 候选归属起点与既有**可信闭合**区间是否重叠。
    ///
    /// 半开区间 [start, end)：端点相接不算冲突（02 §6）。
    /// 待确认区间不参与——它们只是候选范围，不是既成事实（08 §7）。
    pub fn attribution_conflicts(
        &self,
        conn: &Connection,
        candidate_start_ms: i64,
    ) -> Result<Vec<(String, i64, i64)>, AppError> {
        let mut stmt = conn.prepare(
            "SELECT id, started_at, ended_at FROM work_interval
              WHERE ended_at IS NOT NULL AND needs_review = 0 AND voided_at IS NULL
                AND started_at < ?1 AND ?1 < ended_at
              ORDER BY started_at",
        )?;
        let rows = stmt
            .query_map([candidate_start_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib coordinator`
Expected: `test result: ok. 17 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/timer/coordinator.rs
git commit -m "feat(m04): 新 run/休眠后重建基线，附归属冲突检查"
```

---

### Task 9: tick 与 active_ms 快照

**Files:**
- Modify: `src-tauri/src/services/timer/coordinator.rs`

**Interfaces:**
- Consumes: Task 4 的 `snapshot`、P1 的 `session_repo::interval_facts`
- Produces:
  - `Coordinator::tick(&mut self, conn, clock) -> Result<TimerSnapshot, AppError>` —— `tick_seq` 自增
  - `Coordinator::snapshot(...)` 补全 `active_ms`/`remaining_ms`/`overtime_ms`
  - `Coordinator::remember_timer_kind(&mut self, kind: TimerKind, target: Option<i64>)`

- [ ] **Step 1: 写失败的测试**

```rust
    #[test]
    fn active_ms_sums_closed_durations_plus_the_live_monotonic_delta() {
        // 02 §3：active_ms = SUM(可信闭合 duration_ms) + 协调器当前单调增量
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        clock.advance_monotonic_ms(60_000);
        c.pause(db.conn(), &clock).unwrap();
        clock.advance_monotonic_ms(120_000);   // 暂停期间不累计
        c.resume(db.conn(), &clock).unwrap();
        clock.advance_monotonic_ms(15_000);

        let s = c.snapshot(db.conn(), &clock).unwrap();
        assert_eq!(s.active_ms, 75_000, "60s 已闭合 + 15s 运行中");
    }

    #[test]
    fn active_ms_freezes_while_paused() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
        clock.advance_monotonic_ms(30_000);
        c.pause(db.conn(), &clock).unwrap();
        let a = c.snapshot(db.conn(), &clock).unwrap().active_ms;

        clock.advance_monotonic_ms(600_000);
        let b = c.snapshot(db.conn(), &clock).unwrap().active_ms;
        assert_eq!(a, b, "暂停期间值冻结（02 §3）");
    }

    #[test]
    fn countdown_reports_remaining_and_overtime_without_going_negative() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(
            db.conn(), &clock, "run-1", "t1",
            SessionMode::Foreground, TimerKind::Countdown, Some(60_000),
        )
        .unwrap();

        clock.advance_monotonic_ms(20_000);
        let s = c.snapshot(db.conn(), &clock).unwrap();
        assert_eq!(s.remaining_ms, Some(40_000));
        assert_eq!(s.overtime_ms, Some(0));

        clock.advance_monotonic_ms(60_000); // 超时 20 秒
        let s = c.snapshot(db.conn(), &clock).unwrap();
        assert_eq!(s.remaining_ms, Some(0), "不得为负");
        assert_eq!(s.overtime_ms, Some(20_000));
    }

    #[test]
    fn tick_advances_the_sequence_and_shares_the_snapshot_path() {
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();

        let t1 = c.tick(db.conn(), &clock).unwrap();
        let t2 = c.tick(db.conn(), &clock).unwrap();
        assert_eq!(t1.tick_seq, 1);
        assert_eq!(t2.tick_seq, 2);
        // tick 与查询走同一条采样路径，字段口径一致
        let q = c.snapshot(db.conn(), &clock).unwrap();
        assert_eq!(q.session_id, t2.session_id);
        assert_eq!(q.state, t2.state);
        assert_eq!(q.active_ms, t2.active_ms);
    }

    #[test]
    fn tick_does_not_touch_business_revision() {
        // 00 §5：timer.tick 不加业务 revision
        let db = seeded();
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        let mut c = Coordinator::new();
        c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
        let before = crate::storage::meta::read_meta(db.conn()).unwrap().revision;
        for _ in 0..5 { c.tick(db.conn(), &clock).unwrap(); }
        assert_eq!(crate::storage::meta::read_meta(db.conn()).unwrap().revision, before);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib coordinator`
Expected: 编译失败，`no method named tick`。

- [ ] **Step 3: 实现 tick 与补全 snapshot**

`timer_kind` 与 `target_duration_ms` 两个字段在 Task 4 已定义、并在 `start` 里写入，本任务直接用。

新增：

```rust
    /// 每 run 内递增。**只用于展示序列比较**，前端不得据它推导状态跃迁（00 §5）。
    pub fn tick(&mut self, conn: &Connection, clock: &dyn Clock) -> Result<TimerSnapshot, AppError> {
        self.tick_seq += 1;
        self.snapshot(conn, clock)
    }
```

把 `snapshot` 替换为完整实现：

```rust
    pub fn snapshot(&self, conn: &Connection, clock: &dyn Clock) -> Result<TimerSnapshot, AppError> {
        let sid = self.require_session()?;
        let meta = read_meta(conn)?;
        let (state_raw, session_version): (String, i64) = conn.query_row(
            "SELECT state, row_version FROM work_session WHERE id = ?1",
            [&sid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let state = SessionState::from_str(&state_raw)?;

        // 已闭合的可信区间之和
        let closed_ms: i64 = conn.query_row(
            "SELECT COALESCE(SUM(duration_ms), 0) FROM work_interval
              WHERE session_id = ?1 AND ended_at IS NOT NULL
                AND needs_review = 0 AND voided_at IS NULL",
            [&sid],
            |r| r.get(0),
        )?;
        // 运行中才叠加当前单调增量；暂停时没有开放区间，值自然冻结
        let live_ms = match (state, self.interval_started_monotonic) {
            (SessionState::Running, Some(start)) => (clock.monotonic_ms() - start).max(0),
            _ => 0,
        };
        let active_ms = closed_ms + live_ms;

        let (remaining_ms, overtime_ms) = match (self.timer_kind, self.target_duration_ms) {
            (TimerKind::Countdown, Some(target)) => {
                (Some((target - active_ms).max(0)), Some((active_ms - target).max(0)))
            }
            _ => (None, None),
        };

        Ok(TimerSnapshot {
            data_epoch: meta.data_epoch,
            run_id: self.run_id.clone().unwrap_or_default(),
            session_id: sid,
            session_version,
            tick_seq: self.tick_seq,
            as_of_wall_ms: clock.wall_ms(),
            active_ms,
            state,
            timer_kind: self.timer_kind,
            remaining_ms,
            overtime_ms,
        })
    }
```

`Coordinator::new()` 里给新字段默认值：`timer_kind: TimerKind::Stopwatch`、`target_duration_ms: None`；`start` 里写入实参。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib coordinator`
Expected: `test result: ok. 22 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/services/timer/coordinator.rs
git commit -m "feat(m04): tick 与 active_ms 快照（同一采样路径）"
```

---

### Task 10: 提交失败不污染内存

**Files:**
- Modify: `src-tauri/src/services/timer/coordinator.rs`
- Create: `src-tauri/tests/timer_coordinator.rs`

**Interfaces:**
- Consumes: 前九个任务的全部 API
- Produces: 无新 API；把 00 §5 的「提交失败不应用内存变更」变成可回归的断言

- [ ] **Step 1: 写测试**

创建 `src-tauri/tests/timer_coordinator.rs`：

```rust
//! 协调器的跨层验证：重点是**失败时内存不得前进**（00 §5）。

use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::timer::coordinator::{Coordinator, HeartbeatOutcome};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, read_meta};
use worktrace_lib::storage::migrations::migrate;

fn seeded() -> Db {
    let db = Db::open_in_memory().unwrap();
    migrate(db.conn()).unwrap();
    init_meta(db.conn(), "epoch-a").unwrap();
    db.conn()
        .execute_batch(
            "INSERT INTO application_run(id, started_at) VALUES ('run-1', 0);
             INSERT INTO task(id, title, status, row_version, created_at, updated_at)
               VALUES ('t1', 'T', 'Doing', 0, 0, 0);",
        )
        .unwrap();
    db
}

#[test]
fn a_rejected_start_leaves_no_trace() {
    let db = seeded();
    let clock = FakeClock::new(1_700_000_000_000, 0);
    let mut c = Coordinator::new();

    // 倒计时缺 target：领域层就该拒，绝不能留下 session
    assert!(c
        .start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Countdown, None)
        .is_err());

    let n: i64 = db.conn().query_row("SELECT count(*) FROM work_session", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "被拒的 start 不得留下半个会话");
    assert!(c.anchor().is_none(), "被拒的 start 不得建立基线");
    assert!(c.active_session().is_none());
    assert_eq!(read_meta(db.conn()).unwrap().revision, 0, "被拒的 start 不得增加 revision");
}

#[test]
fn a_failed_heartbeat_does_not_advance_the_sample_window() {
    // 异常心跳不更新 last_sample：否则下一轮就从新的坏点起算，
    // 累计偏差被抹掉，异常永远收敛不了。
    let db = seeded();
    let clock = FakeClock::new(1_700_000_000_000, 0);
    let mut c = Coordinator::new();
    c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
    let sample_before = c.last_sample_monotonic();

    clock.advance_monotonic_ms(30_000);
    clock.set_wall_ms(1_700_000_000_000 + 90_000);
    assert!(matches!(
        c.heartbeat(db.conn(), &clock, false).unwrap(),
        HeartbeatOutcome::Anomalous(_)
    ));

    assert_eq!(c.last_sample_monotonic(), sample_before, "异常不得推进采样窗口");

    // 再采一次仍然异常——说明坏点没有被吸收成新的可信基准
    clock.advance_monotonic_ms(30_000);
    clock.set_wall_ms(1_700_000_000_000 + 120_000);
    assert!(matches!(
        c.heartbeat(db.conn(), &clock, false).unwrap(),
        HeartbeatOutcome::Anomalous(_)
    ));
}

#[test]
fn full_timing_path_is_consistent_end_to_end() {
    let db = seeded();
    let clock = FakeClock::new(1_700_000_000_000, 0);
    let mut c = Coordinator::new();

    let s = c.start(db.conn(), &clock, "run-1", "t1", SessionMode::Foreground, TimerKind::Stopwatch, None).unwrap();
    assert_eq!(s.tick_seq, 0);

    clock.advance_monotonic_ms(30_000);
    assert!(matches!(
        c.heartbeat(db.conn(), &clock, false).unwrap(),
        HeartbeatOutcome::Checkpointed { elapsed_ms: 30_000 }
    ));

    clock.advance_monotonic_ms(30_000);
    c.pause(db.conn(), &clock).unwrap();
    assert_eq!(c.snapshot(db.conn(), &clock).unwrap().active_ms, 60_000);

    // 时长事实与库里的 CHECK 约束一致
    let (started, ended, dur): (i64, i64, i64) = db
        .conn()
        .query_row(
            "SELECT started_at, ended_at, duration_ms FROM work_interval WHERE ended_at IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(dur, ended - started);
    assert_eq!(dur, 60_000);
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test --test timer_coordinator`
Expected: `test result: ok. 3 passed`。

若 `a_failed_heartbeat_does_not_advance_the_sample_window` 红，检查 `heartbeat` 是否在 `classify` 返回 `Some` 之后才更新 `last_sample_*`——异常分支必须提前 `return`，且不得调用 `checkpoint_repo::write`。

- [ ] **Step 3: 跑全量并确认无回归**

Run: `cargo test`
Expected: P1 的 `schema`、`foundation` 与本计划的 `timer_coordinator` 加上 `--lib` 全部绿。

- [ ] **Step 4: 跑分层自查**

Run:

```bash
cd src-tauri \
  && (grep -rn "rusqlite\|std::fs\|std::time" src/domain/ && echo "违反：domain 不得有 IO" && exit 1 || echo "domain 无 IO ✓") \
  && (grep -rn "platform::" src/storage/ && echo "违反：storage 不得调用 platform" && exit 1 || echo "storage 未越层 ✓") \
  && (grep -rn "std::time" src/services/ && echo "违反：services 不得直接取时间，必须经 Clock" && exit 1 || echo "services 只用注入时钟 ✓")
```

Expected: 三行 ✓。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/tests/timer_coordinator.rs src-tauri/src/services/timer/coordinator.rs
git commit -m "test(m04): 失败不前进内存，端到端计时链一致"
```

---

## 验收（跑完本计划后必须成立）

1. `cargo test` 全绿，P1 的测试无回归。
2. **两项阈值各自生效**：`slow_drift_is_caught_after_enough_heartbeats`（累计偏差）与 `adjacent_jump_is_caught_on_its_own`（相邻增量）分别独立通过——只实现其中一项时另一项必红。
3. **心跳不重置基线**：`heartbeat_does_not_reset_the_anchor` 通过。
4. **500ms 墙钟偏差下相邻区间不重叠**：`consecutive_intervals_do_not_overlap_under_small_clock_skew` 通过（对应 04 的 F-007 验收项）。
5. **异常分割正确**：有检查点保留前缀、无检查点整段待确认、重复调用不重复分割。
6. **失败不前进内存**：被拒的 `start` 不留会话/不留基线/不加 revision；异常心跳不推进采样窗口。
7. **`active_ms` 口径**：闭合区间之和 + 当前单调增量；暂停时冻结；倒计时的 `remaining_ms` 不为负、`overtime_ms` 正确。
8. **`services/` 不直接取时间**：第三条 grep 自查通过。

## 不在本计划范围内（交给 P3 / P7）

- **崩溃扫描与四类判定**（02 §4）：启动时扫描旧 run、把含开放区间的会话送进 `recovering`、不变量损坏隔离。本计划只产出异常分割原语。
- **`reconcile` / `correct` / `discard_session` / `backfill`**：P3。
- **人工修正后重算 `duration_ms` 并写 `time_edit`**：P3。
- **IPC 命令与 `Mutex` 接线**：P7。本计划所有方法都取 `&mut self`，进程内不可交错；跨命令的串行由 P7 的 Tauri state 提供。
- **`switch`/打断**：P3。
- **番茄钟阶段**（`pomodoro_cycle`/`phase_checkpoint`/`phase_state`）：V0.2。
