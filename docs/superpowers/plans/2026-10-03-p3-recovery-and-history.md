# P3 · 恢复确认、历史修正与补录实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把"计时已经跑完之后"的四件事做扎实：崩溃重启后的四类判定与恢复确认（`reconcile`）、对已完成历史的修正（`correct`）、手工补录（`backfill`）、以及作废整次会话（`discard_session`）——全部带 `time_edit` 审计，且不产生重叠或负时长。**并且必须闭环**：用户把待确认处理完之后，`start`/`resume` 要真的能再用（见 §2 闭环链；原计划缺这一环，见 C1）。

**Architecture:** 新增 `services/recovery.rs`（启动扫描、恢复确认、待确认概览）、`services/history.rs`（修正、补录、作废）、`services/tasks.rs`（任务状态编排与会话联动）。恢复与历史服务**只消费** P2 已提交的事实与 P1 的仓储原语：区间分割用 P2 已经实现的那一套，本计划不另写一份时钟逻辑。服务拥有事务，仓储接受 `&Transaction`；一次用户命令恰好加一次 `revision`。**日界换算复用 P4 的时区入口**（`services/daily_plan.rs`，那里才有 jiff），半开相交复用 `domain::interval`，都不新写第二份。（2026-10-04 修订：因 C1/C2/C3 —— 原架构没有门禁解除、没有组合服务取样接缝、并错误地声称启动扫描能复用 `split_for_anomaly`。）

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· 无新依赖

**Spec:**
- `../specs/2026-10-02-worktrace-architecture/02-data-model.zh.md` §3（命令表）、§4（崩溃恢复四类判定）、§6（统计排除口径）、§9（明细历史与恢复实现）、§10（服务契约补充）、§11 与文末「启动扫描的版本与审计补充」
- `.../08-implementation-contracts.zh.md` §1（异常分割与跨日日界；「异常时保留至最后可信检查点的闭合前缀，剩余部分标 needs_review…没有可信检查点则整个当前区间待确认」；「已检测但未接受的墙钟校正…只有校正审计提交后的接受路径可清除」）、§7（`reconcile` 语义）
- `.../00-architecture.zh.md` §4/§5（错误契约、写事务信封）
- `.../04-functional-spec.zh.md` F-006、F-008、F-009、F-015、F-017

**依赖的前置计划：**
- **P1**（`2026-10-03-worktrace-v01-foundation.md`）：`storage::{meta, guards, task_repo, session_repo, checkpoint_repo, time_edit_repo}`、`error::AppError`、`domain::{session, interval}`
- **P4**：`domain::localdate::LocalDate` 与 Rust 时区校验能力（`services/daily_plan.rs`）；本计划的日界裁剪使用相同的时区库与别名策略，不再选第二套。
- **P2**（`2026-10-03-p2-timer-coordinator.md`）：协调器已提交的 `recovering` 事实与区间分割原语（可信前缀 + 待确认余段）、`TimerSnapshot`、`start/pause/resume/finish`、事务内结束原语（`services/timer/primitives.rs`）
- **P7（已交付，2026-10-04）**：唯一启动入口与串行边界（`services/bootstrap.rs` 的 `startup` / `lock_app` / `AppState`）、命令层 `run_command`、事件广播；**P3 接进 P7 已建立的入口，不自建第二套启动或第二把锁**（2026-10-04 修订：原依赖清单漏了 P7，而 P3 的全部接缝都挂在 P7 的既有结构上）。

**边界（不要越界）：**
- `start/pause/resume/finish` **归 P2**，本计划只读它们提交的事实，不重复实现。
- `switch` 与打断记录属 **V0.2**（F-106，在 `F-101…F-113` 内），本计划不实现。
- 统计口径（范围裁剪、人工/机器分离）归 **P5**；本计划只保证被它读取的事实不重叠、非负、半开，并把「真实日界」与「隔离判定」的唯一入口交给它。
- UI 与 IPC 命令接线归 **P8**（2026-10-04 修订：因 顺带-1 —— 原文写"归 P7"，但 P7 已交付且明确不做恢复确认界面：P7 验收记录 §6.1 第 3 条把「恢复确认页与待确认区间展示」判给 P8/P5，§6.1 第 1 条把托盘「完成」项留给 P8 启用（今天只是禁用占位）；P7 计划的「评审补充：恢复提示与时钟校正」第 1 条也写着「未达成：无入口、无 P3 查询；归 P8」。（2026-10-04 fix round 3：跨计划引用一律改为**小节/条目定位**，下同；原行号见 §1 末尾的 fix round 3 对照表。）**P3 交付服务层 + `AppState` 命令入口 + 测试，不新增 `#[tauri::command]`、不改 `commands/mod.rs`。**

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条（尤其第 ④ 项：被拒命令要逐字段比对，不只比行数）。
**异常事务时序：** 见总纲 §9「异常事务、用户命令拒绝与查询的边界」。

---

## 0. 开工前必读：今天代码里的既成事实（2026-10-04 核对）

> 这一节是"引用的符号必须真实存在"的落点。行号取自本地镜像 `worktrace-src/`（= 仓库 `src-tauri/` 的工作副本，仓库根 `D:\ProJect\worktrace`），对齐时点是 **2026-10-04 21:1x**；**写代码前必须现场 `grep` 符号，不要照抄行号**（每行都给了符号名，行号只是「当时核对」的快照）。
> **已知漂移**：`services/bootstrap.rs` 在 21:02、`tests/startup_order.rs` 在 21:09 被**本计划之外**的改动各动过一次（前一版行号整体 +1：`scan_recovery` 由 `313` 变 `314`、`startup` 由 `640` 变 `641`、`AppState` 由 `340` 变 `341`；`startup_order.rs` 里 `scan_recovery(...)` 的调用点由 `340` 变 `466`）。本节与全文已按**漂移后**的行号对齐。

### 0.1 直接复用（不要新造）

| 符号 | 位置 | P3 怎么用 |
| --- | --- | --- |
| `WriteEnvelope::{for_create, for_update}` | `src/envelope.rs:60/69/77` | 单对象命令用它：`reconcile`/`correct`/`discard_session`/`transition_task` 的版本位是**会话/任务**的版本 |
| `write_tx` / `settle` / `Settled` | `src/services/tx.rs:26/40/60` | 写事务骨架；`settle` 负责「`Changed` 才 `bump_revision` 一次」+ 同事务读回 `revision`/`data_epoch`。**`pub(super)`**：`services::recovery`/`history`/`tasks` 都在 `services` 子树里，能用 |
| 仓储写原语（接受 `&Transaction`，不提交不加版本） | `session_repo.rs:125/180/203/262`、`task_repo.rs:292`、`time_edit_repo.rs:27` | 服务在同一事务里调它们 |
| 事务内结束原语 `end_session_in_tx` + `EndSessionFacts` + `open_interval_of` | `services/timer/primitives.rs:64/27/45` | Task 6 结束/暂停会话；它含 `run_id` 判据（`:75`）与 `Recovering`/`needs_review` 判据（`:85`-`:92`） |
| `session_repo::{get_session, intervals_of_session, update_session_state, SessionStateUpdate}` | `session_repo.rs:91/105/262/248` | 会话读取与状态/`run_id`/`needs_review` 更新（`update_session_state` 同时 `row_version + 1`） |
| `checkpoint_repo::latest(conn, interval_id)` | `checkpoint_repo.rs:129` | Task 1 第 2 类的**闭合点**（最后成功持久化的 `attribution_at`） |
| `session_repo::invariant_faults_of_other_runs` / `InvariantFault` / `fault_reason` | `session_repo.rs:552/535/594` | 四类判定第 1 类的**唯一判据**；P3 把它扩成「不限 run」的读入口 |
| `session_repo::{unfinished_sessions_of_other_runs, pending_intervals_of_other_runs}` | `session_repo.rs:482/515` | 门禁查询；P3 加「不限 run」的兄弟入口，用于全局待确认概览 |
| `bootstrap::{RecoveryScan, scan_recovery, AppState::recovery, AppState::guard_business_timing}` | `bootstrap.rs:295/314/458/466` | 门禁的现状；**不改语义**，C1 只补「重扫」 |
| `bootstrap::{lock_app, AppGuard, AppBoundary}` | `bootstrap.rs:399/370/361` | 唯一串行边界（D6）：P3 全部命令入口都在 `AppGuard` 内跑 |
| `Coordinator::{run_id, wall_ms, live, load_session, snapshot, rebuild_from_committed, is_faulted, refuse_if_faulted}` | `coordinator.rs:192/209/218/266/342/615/322/315` | 时间、镜像刷新、提交后重建 |
| `domain::interval::{IntervalRange::overlap_ms/overlaps/clipped_ms, IntervalFacts, IntervalSet}` | `domain/interval.rs:35/41/46/56/109` | **半开相交已经存在**，Task 5 只复用（因 I3） |
| `daily_plan::{normalize_timezone, local_date_at}` | `services/daily_plan.rs:52/80` | 时区规范化与「时刻 → 本地日」；**反向**（本地日 → 真实半开界）才是 P3 要补的 |
| `LocalDate::{parse, new, year/month/day}`、`LocalDate::from_jiff`（`pub(crate)`） | `domain/localdate.rs:36/55/65-75/82` | 日界函数的输入/构造 |
| `session_repo::{create_session, split_for_anomaly}` | `session_repo.rs:125/330` | **参考语义**，但 Task 1 第 2 类与 Task 4 补录都**不能**直接用（见 0.3 的 S5/S6） |
| `SessionAttention` | `domain/session.rs:179` | 四类判定的类型（见 I2） |
| `TaskTransition::new`、`TransitionCause::{User, Reopen}`、`task_repo::transition_task` | `domain/task.rs:115/95`、`task_repo.rs:292` | Task 6 的任务跃迁；`TransitionCause` **不扩**（因 I5） |

### 0.2 已私有、P3 够不着（不要试图直接调）

- `Coordinator::{observe:328, attribute:713, validate_session_request:913, refuse_stale_run_session:930, read_sample:941, sample_and_detect:962}` 全部是**私有方法**（无 `pub`，也没有 `pub(crate)`）。
- `Coordinator::guard_epoch_ro:677` / `guard_row_version_of_ro:692` 是 `coordinator.rs` 里的**私有自由函数**。
- `AppState` 的 `db` / `coordinator` / `recovery` 三个字段**私有**（`bootstrap.rs:342-344`，结构体本身在 `:341`；P7 Task 0 评审 I3 专门私有化，理由见该处文档：把 `&mut Coordinator` 递出去，"不得忽略历史"就只剩纪律）。
- ⇒ 因此 `services/{recovery,history,tasks}.rs` **不可能**自己取样、自己算 `A(M)`、自己清门禁。P3 必须显式加接缝（0.3），不许绕。

### 0.3 P3 必须新增的接缝（名字 / 签名 / 可见性 / 归属文件 / 谁调用）

> 这是本计划的**接口契约**。实现时签名若调整，必须同步本节、下游接口一节与 P5/P6/P8 的引用。

**S1（C1，门禁解除）** `services/bootstrap.rs`，`impl AppState`：
```rust
/// 重扫恢复事实并替换门禁快照（`start`/`resume` 读的就是这个字段）。
/// 服务层事务**提交之后**、仍持同一把锁时调用；返回重扫结论——
/// `requires_recovery()` 仍为真时 `start`/`resume` 继续拒绝（判据不变）。
pub fn rescan_recovery(&mut self) -> Result<RecoveryScan, AppError>;
```
**提交后失败也必须闭环**：S1 新增私有 `AppState::recovery_scan_failed: bool`（启动成功时 false）；扫描成功才原子替换 `recovery` 并清标记，失败置 true、返回 `RECOVERY_REQUIRED`，不把旧快照当成功结果。`guard_business_timing` 先检查该标记，再走原有门禁；查询/恢复入口仍可用。S12 必须允许在协调器已恢复但该标记仍为 true 时再次执行 S1，清标记不要求重做已提交的用户命令。P6 安装新 Runtime 前仍必须完成同一扫描并按结果设置标记。Task 7 注入“用户事务已提交、三条扫描查询失败”，断言已提交事实保留、不重复审计/revision、start/resume 拒绝、再次扫描成功才解除；新标记不改变 TimerSnapshot 或既有 IPC DTO。
调用方：`AppState::reconcile`、`AppState::discard_session`（成功提交后**无条件**重扫：三条只读查询，幂等，与 `Changed` 与否无关）；P6 的恢复/替换库路径复用同一入口（**P6 计划的 Task 1「单实例与启动顺序的硬化」**启动顺序第 ④ 步写着"调用 **P3** 的恢复扫描"；**Task 4「WAL 一致备份与恢复」**的「恢复的执行骨架与顺序」条明确要与 S1 共用同一函数——P6 那边同样按小节/符号定位，不锁行号）。**为什么必须是本模块的方法**：`recovery` 是私有字段（`bootstrap.rs:344`），只有 `services::bootstrap` 能写（0.2）。

**S2（全部命令入口）** `services/bootstrap.rs`，`impl AppState`：`reconcile` / `correct` / `backfill` / `discard_session` / `transition_task` / `accept_detected_clock_correction` 六个瘦包装（各自解构 `AppState { db, coordinator, .. }`、取 `now`、调服务、做提交后收尾）。命令层（P8）在 `run_command` 的 `lock_app` 守卫内调它们（`commands/mod.rs:127/140`）——**同一串行边界由此保证，P3 不得自行 `lock_app` 第二把锁**。

**S3（C2，同一边界取样 + 检测）** `services/timer/coordinator.rs`，`impl Coordinator`：
```rust
pub struct BoundaryFacts { pub sample: ClockSample, pub wall_ms: i64, pub attributed_at: i64, pub run_id: String }

/// 组合服务（Task 6 的任务状态编排）的取样接缝：取一次样本、观察一次；
/// 判为异常时**先提交独立系统恢复事务**，随后返回 `RECOVERY_REQUIRED`
/// （原用户命令不执行，总纲 §9）。
pub fn boundary_facts(&mut self, db: &mut Db) -> Result<BoundaryFacts, AppError>;
```
实现＝`let s = self.sample_and_detect(db)?; Ok(BoundaryFacts { sample: s, wall_ms: s.wall_ms, attributed_at: self.attribute(s), run_id: self.run_id.clone() })`。**为什么不是把两个私有方法放宽成 `pub(crate)`**：`observe` 每个样本只能观察一次（`coordinator.rs:326-327` 的注释写明"同一个采样看两次，第二次的增量恒为 0，会把刚判出来的异常覆盖成 `Trusted`"），把 `sample_and_detect` 与 `attribute` 拆到两个模块调用，正是这个 bug 的入口；`read_sample`（`:941`）又是唯一带"故障态 + 跨 run"判据的采样入口。

**S4（时钟校正的显式接受）** `services/timer/coordinator.rs`，`impl Coordinator`：
```rust
pub struct AcceptClockCorrectionRequest { pub expected_data_epoch: String }
pub struct ClockCorrectionAccepted { pub accepted: bool, pub data_epoch: String, pub revision: i64 }

/// 用户命令：显式接受一次**已检测但未接受**的墙钟校正（08 §1）。
/// 一个用户事务：`guard_epoch` → 写 `time_edit`（before = 三参照点，
/// after = 本次样本 + `clock_correction_accepted: true` + `intervals_changed: false`）
/// → `revision` 恰好 +1；**提交后**才调 [`Coordinator::accept_clock_correction`]
/// （`:245`，既有的内存半部）清未接受标记并前移长期参照。不确认任何可疑工时。
pub fn accept_detected_clock_correction(
    &mut self, db: &mut Db, req: AcceptClockCorrectionRequest,
) -> Result<ClockCorrectionAccepted, AppError>;
```
**与既有的分工**：`Coordinator::accept_clock_correction(sample)`（`coordinator.rs:245`）是 **P2 已交付的内存原语**（清 `unaccepted_clock_correction`、前移 `lifetime_ref`，**不写审计、不加 revision**）；S4 是它的**事务外壳**，不重写它。实现顺序：`guard_epoch`（只读预检 + 事务内各一次，模式抄 `catalog::list_projects`——只读事务 + `guard_epoch`，`catalog.rs:290-294`）→ `read_sample`（`:941`，含故障态与跨 run 判据）→ `observe` 一次（**必须观察**：不推进检测器的 `last`，下一拍会把"距上次观察很久"误判成长间隔 `Suspended`）→ 写审计 → `bump_revision` → 提交 → `accept_clock_correction(sample)`。判据：`unaccepted_clock_correction == false` ⇒ `Unchanged`（无审计、零变化）；`live` 为 `None` 同样按 `Unchanged` 处理（该标记只在 `live.state == Recovering` 分支里被置真，`:1096-1099`）；检测到 `MonotonicBackwards` ⇒ `faulted = true` 并返回 `RECOVERY_REQUIRED`（它不是墙钟校正，`SampleVerdict::is_wall_clock_anomaly`，`anchor.rs:119`）。`time_edit.session_id` 取当前 `live.id`。
**（2026-10-05 实施期订正，见文末「P3 实施记录」第 8 条）**：上面的 `Unchanged` 只适用于**未观察到异常**的情形。`observe` 每个样本只能观察一次且会推进检测器的 `last`，所以在这条"本来会返回 `Unchanged`"的路上判出非 `Trusted` 判决后若直接丢弃，睡眠/休眠（`Suspended`）就**再也无法被重新检测**，停机时间会被静默计成工时。⇒ 实现为：该路上的非 `Trusted` 判决交给**既有异常处理**（系统事务）后返回 `RECOVERY_REQUIRED`；`flag == true` 的正常接受路径不变。**P8 的界面必须容忍这一点**（收到 `RECOVERY_REQUIRED` 后刷新并显示恢复提示，不要当失败重试）。

**S5（C3，第 2 类的归一原语）** `storage/session_repo.rs`：
```rust
/// 旧 run 的「running + 开放区间」→「可信前缀（可信闭合）+ 终点未知的待确认段」。
/// 返回值复用 P2 的 [`AnomalySplit`]（`:309`）：`pending_interval_id` 在 P3 里**总是** `Some`。
/// **绝不留下** `voided_at IS NULL AND ended_at IS NULL` 的行（那会让下一次启动命中
/// 自造的 `open_interval_outside_running`，见 C3）。
pub fn normalize_crashed_open_interval(
    tx: &Transaction<'_>, session_id: &str,
) -> Result<AnomalySplit, AppError>;
```
**为什么不能复用 `split_for_anomaly`（`:330`）**：它用 `sampled_wall_at: Option<i64>` 决定 `ended_at`——`None` 时 `ended_at` 也写成 `NULL`（`:379` 与 `:405`），而启动扫描**没有**这一拍的墙钟样本（进程刚起来，旧 run 的样本不存在）；这正是 C3 的故障源。

**两个分支的字段填法（`AnomalySplit:309` 的三个 `Option` 必须逐字段明确，不许"看着办"）：**

| 分支 | `trusted_interval_id` | `trusted_until` | `pending_interval_id` | `candidate_end` |
| --- | --- | --- | --- | --- |
| **有检查点**（`t > started_at`） | `Some(原区间 id)` | `Some(t)` | `Some(新余段 id)` | `t` |
| **无检查点**（或 `t <= started_at`） | `None` | `None` | `Some(原区间 id)` | `started_at` |

**没有开放区间**怎么办（第 2 类的前提就是"有开放区间"，走到这里说明判据之间不一致）：返回 `DomainError::NoOpenInterval`（`domain/error.rs:26`），由扫描层按**第 1 类**降级——记进 `faults`（reason 用 `running_without_open_interval`）后**继续处理下一行**；**不许** abort 整批扫描，也**不许**静默返回一个全 `None` 的 split（P2 的 `split_for_anomaly` 在「没有开放区间」分支里就是那种写法，`:347-355`；在 P3 的语义下它把"损坏"伪装成"没事"）。（2026-10-04 fix round 2：因 R4。）

**S6（I4，补录原语）** `storage/session_repo.rs`：
```rust
pub struct NewFinishedSession<'a> {
    pub id: &'a str, pub task_id: &'a str, pub run_id: &'a str,
    pub mode: SessionMode, pub timer_kind: TimerKind, pub target_duration_ms: Option<i64>,
    pub interval_id: &'a str,
    pub started_at: i64, pub ended_at: i64, pub duration_ms: i64,   // 服务算好；仓储只写
}
/// 建立一个**终态**会话与它的可信闭合区间；不占前台槽位、不写检查点。
pub fn create_finished_session(
    tx: &Transaction<'_>, facts: &NewFinishedSession<'_>,
) -> Result<SessionRow, AppError>;
```
**为什么不能复用 `create_session`（`:125`）**：它把 state 硬编 `'running'`（`:159`）并紧跟 `open_interval`（`:172`）——会短暂占用 `uq_running_foreground`（`schema_v1.rs:183`），而补录既不计时也不占前台（02 §3）。

**S7（重叠校验）** `storage/session_repo.rs`：
```rust
/// `[start, end)` 与**全部**未作废、已确认的 FOREGROUND 区间是否相交（半开，端点相接不算）。
/// 命中即返回 `DomainError::OverlappingInterval { existing_start, existing_end }`（`domain/error.rs:38`）。
/// `exclude_interval` 供 `correct` 排除被修正的那一段自身。
pub fn require_no_human_overlap(
    conn: &Connection, start: i64, end: i64, exclude_interval: Option<&str>,
) -> Result<(), AppError>;
```
口径：跨会话（02 §3 的 `correct`/`reconcile` 都要求"与既有人工时间不重叠"）；机器模式（`BACKGROUND`/`PASSIVE`/`WAITING`）按独立口径，不参与互斥（02 §6）。

**S8（区间事实写入口，三条）** `storage/session_repo.rs`（服务不写 SQL）：
```rust
pub fn confirm_interval(tx, interval_id: &str, started_at: i64, ended_at: i64, duration_ms: i64) -> Result<IntervalRow, AppError>; // needs_review 1→0；保留 voided_at=NULL 与 sampled_end_wall_at 原值
pub fn void_interval(tx, interval_id: &str, voided_at: i64) -> Result<IntervalRow, AppError>;                                     // voided_at=…、needs_review=0、有 duration_ms 的已知区间保留 ended_at；无 duration_ms 的候选区间清 ended_at 为 NULL
pub fn retime_interval(tx, interval_id: &str, started_at: i64, ended_at: i64, duration_ms: i64) -> Result<IntervalRow, AppError>;  // correct 用；仅允许 needs_review=0 且未作废
```
`void_interval` 按时长是否已知处理：`duration_ms IS NOT NULL` 时保留原始起止与时长；`duration_ms IS NULL` 时将 `ended_at` 清为 `NULL`，同时设 `voided_at`、清 `needs_review`，保留 started_at/sample 原值。不能给未确认的候选区间补 0 时长冒充事实。S5 会造出 `ended_at=started_at, duration_ms=NULL, needs_review=1` 的零长度候选；只清 needs_review 会违反既有 ck_interval_duration（该 CHECK 不豁免 voided），因此不能承诺所有 ended_at 原样。修改前的候选终点完整记录在 time_edit.before_json，after_json 记录清空后的事实；不改已发布 schema。已作废且 ended_at=NULL 不占开放有效区间槽位，因为约束/查询都过滤 voided_at IS NULL。

**S9（扫描/概览用的只读查询）** `storage/session_repo.rs`：把三条 run 过滤查询泛化成 `Option<&str>`（`None` = 不限 run），保留既有签名的兄弟入口（`tests/startup_order.rs` 的 `the_recovery_scan_only_counts_sessions_of_other_runs` 直接调 `scan_recovery`，别动它的语义——**以符号/用例名为准**）：
```rust
pub fn unfinished_sessions(conn, run: Option<&str>) -> Result<Vec<SessionRow>, AppError>;
pub fn pending_intervals(conn, run: Option<&str>) -> Result<Vec<IntervalRow>, AppError>;
pub fn invariant_faults(conn, run: Option<&str>) -> Result<Vec<InvariantFault>, AppError>;
```
SQL 里用 `(?1 IS NULL OR s.run_id <> ?1)` 一个参数承载两种语义（**一份谓词，两个入口**，不许复制 SQL）。同时按 C3 的衍生结论收紧 `open_interval_outside_running` 的判据（见 Task 1 第 1 类）。

**S10（真实日界）** `services/daily_plan.rs`（那里才有 jiff 与私有的 `timezone_of:94`，避免第二份时区管道）：
```rust
/// 本地日期在给定时区里的**真实**半开界 `[start, end)`：`end` 是「次日零点」的换算结果，
/// **不是** `start + 86_400_000`（夏令时切换日 23/25 小时，08 §1）。
pub fn local_day_bounds(timezone: &str, date: LocalDate) -> Result<IntervalRange, AppError>;

/// 一个半开范围覆盖到的每一个本地日：(本地日期, 该日的真实半开界)，升序。
/// 调用方用 `IntervalRange::clipped_ms`（`domain/interval.rs:46`）取交集，**不要**自己算。
pub fn local_days_covering(timezone: &str, from: i64, to: i64) -> Result<Vec<(LocalDate, IntervalRange)>, AppError>;
```
实现方向：`daily_plan::normalize_timezone`（`:52`）→ `jiff::tz::TimeZone::get` → `jiff::civil::Date::new(y,m,d)` → `.at(0,0,0,0).to_zoned(zone)` → `.timestamp().as_millisecond()`（`Date::tomorrow` 推进一天；`Zoned::timestamp` / `Timestamp::as_millisecond` 已核对存在于 jiff 0.2.37）。`LocalDate::from_jiff`（`domain/localdate.rs:82`）用于返回本地日期。

**语义（钉死，2026-10-04 fix round 2：因 R5）：**
- `local_days_covering` 返回与 `[from, to)` **正相交**（`overlap_ms > 0`，`interval.rs:35`）的那些本地日，按日升序，元素是该日的**真实半开界**；
- `from == to`（零长度范围）⇒ **空 `Vec`**（零长度范围不覆盖任何一天）；
- `from > to` ⇒ `DomainError::NegativeInterval { started_at: from, ended_at: to }`（与 `IntervalRange::new:14` 同一判据，**不静默交换端点**——交换会把"调用方算错了"变成"悄悄换了一天"）；
- 端点正好落在日界上时**不含次日**（半开语义：`[d0, d1)` 只覆盖 `d0` 那一天）；
- `local_day_bounds` 的 `end` 由 `Date::tomorrow` 推进，**不用** `start + 86_400_000`，所以夏令时切换日是 23/25 小时。

**S11（模块注册）** `services/mod.rs:6-20` 增加 `pub mod recovery; pub mod history; pub mod tasks;`；同步更新 `services/tx.rs:9-10` 那句过期的使用者清单（因 顺带-4）。

**S12（协调器故障态的唯一生产出口，2026-10-04 fix round 2：因 R3）** `services/bootstrap.rs`，`impl AppState`：
```rust
/// 重试协调器的**故障态恢复事务**（`Coordinator::faulted`）。
/// 一个用户命令：`coordinator.retry_recovery(db)` 成功提交后才清故障态；
/// 随后**无条件重扫门禁**（S1）——那笔系统事务可能刚把某个会话推成 `recovering`，
/// 门禁字段必须跟着事实走。返回提交后的权威快照。
pub fn retry_recovery(&mut self, expected_data_epoch: &str) -> Result<TimerSnapshot, AppError>;
```
- 为什么必须有这个包装：`Coordinator::retry_recovery`（`:1265`）是 `pub`，但 `AppState.coordinator` 私有（`bootstrap.rs:344`）⇒ 服务层与命令层都够不着；而**它是 `faulted` 的唯一生产出口**（置真点 12 处：`:624/636/641/645/653/663` 提交后重建失败、`:953` 无基线采样失败、`:1067` 异常事务失败、`:1090/1104/1154/1243` 单调钟硬故障；置真后 `refuse_if_faulted`（`:315`）会拒绝 `snapshot/tick/start/pause/resume/finish/heartbeat/system_pause` 等 10 个入口）。
- 与 S1 的分工：**S12 清协调器故障态（内存 + 提交那笔系统事务），S1 重算门禁字段（三条只读查询）**；只调 S12 不重扫 ⇒ 新出现的 `recovering` 不会立刻挡计时（会滞后到下次重扫）；只重扫不调 S12 ⇒ `refuse_if_faulted` 继续拒绝一切，闭环不了。所以包装里**两个都调**，且 S12 先。
- 不能解除的故障：`MonotonicBackwards` 置真的硬故障一律 `RECOVERY_REQUIRED`（`:1270-1272`，单调读数已失去本 run 的意义，只能新 run 安全重建；P2 计划的「非运行态硬故障补全」末条同口径：硬故障不能通过 `retry_recovery` 解除）。
- 触发时机（**用户显式重试**，不是定时自动重试）：P8 新增一条 IPC 命令（用户点"重试对账"；P8 计划已登记，见其「P8 新增的 IPC 命令」第 7 条）；P6 负责采样/启动路径的看门狗与重启（P7 验收 §6.7 第 37 条登记的"采样线程 panic 静默死亡"属 P6）。**不做定时自动重试**：08 §1 的立场是"故障不能自己把证据擦掉"，自动重试会把"用户没处理"变成"悄悄恢复"。
- **epoch 口径（写清，不要假装它是事务内校验）**：IPC 请求带 `expected_data_epoch`（P8 计划同条已登记）；`AppState::retry_recovery` 先做**只读预检**（读事务 + `guard_epoch`，模式同 `catalog::list_projects`（只读事务 + `guard_epoch`，`catalog.rs:290-294`）），不一致直接 `DATA_EPOCH_MISMATCH`、不进协调器。**协调器内部那笔恢复事务不带 epoch**——`Coordinator::retry_recovery` 的签名没有 env（P2 既有），与 P2 的异常事务同一口径，所以"预检 → 提交"之间存在理论窗口；进程内由 `AppBoundary`/`lock_app`（D6 单锁）串行、跨进程由单实例锁，实际只可能来自 P6 的恢复/替换库（那会换 epoch 并要求重新握手）。这一条要如实写进实现注释。

### 0.4 不许碰（已冻结的契约）

- `WriteEnvelope`（crate 根）、`WriteOutcome::{Changed,Unchanged}` + `into_parts()`（`storage/mod.rs:50/78`）、`bump_revision` 只在业务事务里被调一次（`meta.rs:64`）；
- **每笔业务写恰好一次 `bump_revision`**：P3 的扫描事务是"一批一次"（02 文末补充），用户命令是"一条一次"，**不得出现第二处**；
- `domain.changed` 只在 `Changed` 时广播、在命令层、提交后、放锁前（`commands/mod.rs:186` 的 `announce`）——P3 不改广播路径；
- 24 条 IPC 命令都经 `commands/mod.rs::run_command`（`:127`）；P3 不新增命令；
- DTO 形状由 `worktrace-web/src/types/__snapshots__/*.json`（15 份）逐字节钉住：**P3 不新增/不改任何快照 fixture**，也不改 `TimerSnapshot`/`TaskChange` 等既有 DTO 的字段；
- `AppBoundary`/`lock_app` 单一串行边界（D6）：P3 不得新增锁、不得在持锁时再取锁；
- `AppError` 只有 5 个码（`error.rs:39-47`）：P3 **不新增错误码**，也**尽量不新增 `DomainError` 变体**（`domain/error.rs:8` 的变体清单有穷尽 match 证人，见 `tests/error_contract.rs:173`）；新规则文案用手写 `AppError::Domain { detail }`（**中文、面向用户**，`tests/error_contract.rs` 的文案门禁会查）。

### 0.5 P3 新增的 DTO 形状（钉死；P8 照抄，别自己发明）

> 这些类型**是 P3 产出接口的一部分**：P3 自己的测试要按字段钉住它们（含边界值：`None` 与零长度），P8 的 IPC 包装与快照 fixture 直接照抄。改字段 = 改下游接口，必须同步本节、Task 里的语义与 P8。
（2026-10-04 fix round 2：因 R1/R2/R7/R10——终审指出 `PendingIntervalItem` 只有名字没有定义。）

```rust
// services/recovery.rs（S9/S10 的消费面）
pub struct StartupScanReport {
    pub normalized_sessions: Vec<String>,   // 第 2 类：S5 归一过的会话
    pub rebound_sessions: Vec<String>,      // 第 4 类：run_id 重绑过的会话
    pub recovering_kept: Vec<String>,       // 第 3 类：原样保持
    pub faults: Vec<InvariantFault>,        // 第 1 类：只诊断（session_repo.rs:535）
    pub attention: Vec<SessionAttentionItem>,
    pub revision_changed: bool,
}

// 全局待确认概览（P8 的「恢复确认」入口与 P5 的排除口径都读它）
pub struct AttentionOverview {
    pub items: Vec<SessionAttentionItem>,
    pub pending_intervals: usize,   // 待确认区间总数（含终点未知的）
    pub pending_sessions: usize,    // 有待确认区间的会话数
    pub fault_sessions: usize,      // 第 1 类（隔离）会话数
    pub data_epoch: String,
    pub revision: i64,
}

pub struct SessionAttentionItem {
    pub session_id: String,
    pub task_id: String,
    pub state: SessionState,          // domain/session.rs:9
    pub run_id: String,
    pub is_current_run: bool,         // 与本次 application_run 比（P8 据此分组）
    pub attention: SessionAttention,  // domain/session.rs:179（I2）
    pub intervals: Vec<PendingIntervalItem>,
    pub fault_reason: Option<String>, // 第 1 类的 InvariantFault.reason（诊断文本，不进用户文案）
    pub session_row_version: i64,     // 供 reconcile/discard 的 expected_row_version
    pub session_needs_review: bool,   // 会话级标记（新增-1 的清理对象）
}

/// 一条待确认区间。**字段与 `IntervalRow`（`session_repo.rs:34`）同源、同一次读事务**，
/// 但按展示口径裁过：不带 `session_id`（在父项里已有）与 `voided_at`
/// （待确认集合按定义 `voided_at IS NULL`；已作废的只在历史/审计里看，02 §6）。
pub struct PendingIntervalItem {
    pub id: String,
    pub started_at: i64,
    /// **候选端点**（不是已确认事实）：S5 归一时等于可信前缀的界、P2 分割时等于候选结束。
    pub ended_at: Option<i64>,
    /// `None` = 未确认（终点未知或还没被用户确认）⇒ **不得**当已确认时长用。
    pub duration_ms: Option<i64>,
    pub sampled_end_wall_at: Option<i64>,
    pub needs_review: bool,           // 恒为 true（这个列表就是待确认集合）
}
```

其余四个命令 DTO 的字段在各自 Task 里给（`ReconcileReport`/`HistoryEditReport`/`TaskTransitionReport`/`ClockCorrectionAccepted`），形状同样按本节口径钉死。
（R1/R2 后续：`attention_overview` 的 **IPC 命令由 P8 新增**——见「下游接口」第 2 条；P8 计划已登记这一条与其 DTO 引用（其「P8 新增的 IPC 命令」清单，P3 侧共 8 条 + 导出 1 条）。）

---

## 1. 开工前修订记录（2026-10-04）

> 逐条：**原措辞 → 新措辞 → 依据（file:line 或命令）→ 为什么**。C = Critical，I = Important，"顺带" = 同一份文件里的过时表述。

**C1｜门禁永远不会被解除**
- 原：「扫描时机…本任务只提供扫描入口」「`reconcile`…更新 `run_id` 到当前 run」（全篇没有一处说门禁怎么解除）。
- 新：新增 S1 `AppState::rescan_recovery`，由 `AppState::reconcile` / `AppState::discard_session` 在提交后调用；`requires_recovery()` 仍为真就继续拒绝（详见 Task 1「门禁解除」与 §2）。
- 依据：`bootstrap.rs:344`（`recovery` 是启动时算一次的私有字段）、`:458`（只读 getter）、`:466-471`（`guard_business_timing` 只看该字段）、`:474/:483`（`start`/`resume` 是唯一两个受它约束的入口）、`:699`（`scan_recovery` 全仓唯一调用点）。
- 为什么：不改就是"P3 做完也闭环不了"——用户处理完待确认后 `start`/`resume` 仍被永久拒绝；重启若库里还有旧 running/recovering 会话会再次拒绝。

**C2｜Task 6 需要的"同一边界取样 + 检测"接缝是私有的**
- 原：「在同一串行协调器边界采样；按总纲 §9 先验证请求，再检测异常。」
- 新：新增 S3 `Coordinator::boundary_facts`（`pub fn`，返回 `sample` / `wall_ms` / `attributed_at` / `run_id`），由 `services::tasks::transition_task` 调用；同一串行边界由 `AppGuard` 保证（S2）。
- 依据：`coordinator.rs:962/941/328/713/913`（相关方法全私有）、`bootstrap.rs:424-608`（`AppState` 只有 `snapshot`/`tick`/`sample_tick` 之类包装，没有取样入口）、`bootstrap.rs:399`（`lock_app`）、`commands/mod.rs:140`（命令体在锁内跑）。
- 为什么：`services/tasks.rs` 与 `Coordinator` 是兄弟模块，够不着私有方法；放宽 `observe`/`attribute` 会引入"同一个采样观察两次"的静默 bug（`coordinator.rs:326-327`）。

**C3｜Task 1 第 2 类的语义与既有原语冲突（会自造不变量故障）**
- 原：「`running` 且有开放区间 → session 设 `recovering`，**只把该开放区间标 `needs_review=1`**」+「复用 P2 的区间分割原语；本计划**不另写**"找一个可信前缀"的逻辑。」
- 新：新增 S5 `session_repo::normalize_crashed_open_interval`，动作逐值写死（Task 1 第 2 类）：闭合点 = `interval_checkpoint.attribution_at`（无检查点则用区间自身的 `started_at`）；可信前缀闭合为 `needs_review=0`、`duration_ms = t - started_at`、`sampled_end_wall_at = checkpoint.wall_at`；待确认段是**新行**且 `ended_at = t`（候选端点）、`duration_ms = NULL`、`sampled_end_wall_at = NULL`、`needs_review = 1`；**不得留下 `voided_at IS NULL AND ended_at IS NULL` 的行**。「复用 P2 分割原语」这句**删除**。
- 依据：`session_repo.rs:330`（`split_for_anomaly` 签名带 `sampled_wall_at`）、`:379`/`:405`（`sampled_wall_at=None` ⇒ `ended_at` 写 `NULL`）、`:370-383`（余段插入分支）、`:569`（`open_interval_outside_running` 判据）、`schema_v1.rs:116-124`（`ck_interval_duration` 允许「`ended_at` 非空 + `duration_ms IS NULL` + `needs_review=1`」）、`:185-186`（`uq_open_interval`）、`08 §1`（异常分割的正当形态）。
- 为什么：启动扫描**没有**这一拍的墙钟样本；照原措辞实现，会话被置 `recovering` 后仍留一条 `ended_at=NULL` 的段，下次启动即被自己的判据判成"非 running 残留开放区间"，从此既进不了 `reconcile` 正常路，又被门禁永久拦住。

**I1｜"隔离/不变量故障"没有持久化落脚点**
- 原：「两类事实要分开记录：**不变量损坏**…与**普通待确认**…」——没说记在哪，也没说 P5 怎么读到。
- 新：**不新增落点**。损坏是既有事实上的**可判定谓词**，唯一判据是 `session_repo::invariant_faults`（由 `:552` 泛化而来，S9）；P3 在 `services::recovery::attention_overview` 里把它作为只读字段交给 P5/P8，`reconcile` 对命中者一律拒绝并要求诊断。**写入时机：不写；事务口径：不涉及**（因此也不可能引入第二处 `bump_revision`）。
- 依据：`schema_v1.rs:22-26`（`app_meta` 只有 `singleton`/`data_epoch`/`revision` 三列，且 `:3-4` 明写已发布 DDL 不得原地改写 ⇒ 加列/加键都要 v2 迁移）、`schema_v1.rs:28-32`（`application_run` 是"每 run 一行"，表达不了"哪一个会话损坏"）、`coordinator.rs:170/322`（`faulted` 只在内存）、`session_repo.rs:552-573`（三条谓词全部可 SQL 判定）。
- 为什么：新表/新列都要动已发布 DDL 走 v2 迁移，而它们存的是一个**可随时从事实重算**的谓词——判据与标记会漂移（尤其 P6 恢复/替换库之后）。可判定的事实存两份，正是本仓库反复拒绝的做法。

**I2｜`SessionAttention` 零调用**
- 原：计划通篇未提（P7 验收 §6.6 第 31 条把它归 P3）。
- 新：**用起来**。四类判定的逐会话视图、`attention_overview` 的条目类型都用 `SessionAttention`（第 1 类 `InvariantBroken`；第 2/3 类 `NeedsReview`；第 4 类 `None`），不新造枚举。
- 依据：`domain/session.rs:179`（类型定义，文档自己写着"恢复扫描与 UI 都据此分流"）、P7 验收记录 §6.6 第 31 条（"`SessionAttention` 零调用…P3（四类判定接入时决定去留）"；原记 `p7-acceptance.md:667`）。
- 为什么：它已经区分"普通待确认"与"不变量损坏"，正好是计划要求的"两类事实分开记录"；删掉再新造一个同形枚举是纯浪费。

**I3｜半开相交已存在；"真实日界裁剪"确实不存在**
- 原：「提供**一处**区间规则实现供 P3 内部与 P5 消费：半开区间 `[start, end)` 相交判定、按查询时区的**真实日界**裁剪」。
- 新：相交判定**复用**`domain::interval`（`IntervalRange::overlap_ms:35` / `overlaps:41` / `clipped_ms:46`、`IntervalFacts:56`、`IntervalSet:109`），P3 **不新写**；只有"本地日 → 真实半开界"是新的（S10，落 `services/daily_plan.rs`）。
- 依据：`domain/interval.rs:35/41/46/109/56`（全在，且 `IntervalSet::insert:119` 已经做重叠拒绝）、`localdate.rs`（全文件只有日期解析/构造，没有任何时刻换算）、`services/daily_plan.rs:80`（`local_date_at` 只有单向"时刻 → 日期"）。
- 为什么：原措辞会让实现者复制一份相交判定，而 P5 也共用同一份——两份必然漂移。

**I4｜`backfill` 没有原语**
- 原：「创建 `finished` session 与**可信闭合**区间」。
- 新：新增 S6 `session_repo::create_finished_session`（`&Transaction`、显式收 `started_at`/`ended_at`/`duration_ms`、直插区间行、`sampled_end_wall_at = NULL`）；服务层固定 `SessionMode::Foreground` + `TimerKind::Stopwatch`（`target_duration_ms = NULL`）。
- 依据：`session_repo.rs:125`（`create_session`）、`:159`（硬编 `'running'`）、`:172`（紧跟 `open_interval`）、`:187`（`open_interval` 要求状态允许开放区间）、`schema_v1.rs:183`（`uq_running_foreground`）、`:94-97`（`ck_timer_budget`）、02 §6（人工仅 `FOREGROUND`）。
- 为什么：走 `create_session` 会短暂占用前台槽位（与"补录不占前台"直接冲突），而且它必然开一个开放区间；V0.1 的补录是"人工历史录入"，没有执行过程，因此不重建倒计时预算。

**I5｜`transition_task` 两层同名**
- 原：「提供 transition_task(request) 服务；…P1 task_repo 仅为事务原语，P7/托盘只调用此服务，不拆成任务和会话多个命令。」
- 新：写清三层命名与归属——**`services::tasks::transition_task`（P3 新增的服务层入口，对外名字就是 02 §3 的 `transition_task`，`AppState::transition_task` 是它的命令入口）**｜**`storage::task_repo::transition_task`（P1 既有事务原语，`task_repo.rs:292`，签名与语义都不改）**；写代码时必须带模块路径。既有三处调用点**不改成走新入口**：`services::catalog::clarify_ready`（`catalog.rs:608`）已在服务层且只做无会话的 `Inbox/Clarifying → Ready`；`Coordinator::start`/`resume`（`coordinator.rs:552/563/834`）在协调器自己的事务里，改走新入口＝事务套事务 + 二次取样。**`TransitionCause` 不扩**（只有 `User`/`Reopen`，`domain/task.rs:95`）。
- 依据：`task_repo.rs:292`、`domain/task.rs:95-101`、`catalog.rs:577-613`、`coordinator.rs:552/563/834`、`platform/tray.rs:104`、P7 验收记录 §6.1 第 1 条（托盘「完成」项；原记 `:609`）、P8 计划的「轻量 GTD 与联动入口的最终整合」第 1 条（原记 `:177`）。
- 为什么：同名两层是"照计划写会写错层"的经典来源；而扩 `TransitionCause` 对既有快照/向量**无影响也不必要**——`cause` 从不落库（`task_repo.rs:335-338` 的 `task_change` 只写 `status`/`quality`），跃迁表只用它判"终结态回 Ready 必须显式"（`domain/task.rs:132`）。

**I6｜下游接口清单缺三样**
- 原（§下游接口）：只列了 `reconcile`/`correct`/`backfill`/`discard_session`、`transition_task`、区间规则、恢复扫描入口。
- 新：补三条真实情况（详见「下游接口」一节）：
  ① `accept_clock_correction`：**P2 已交付内存半部**（`coordinator.rs:245`，不写审计不加 revision），P3 补的是**事务外壳 S4**（审计 + revision + 提交后调它）——不再把 `accept_clock_correction` 整体算作 P3 产出；
  ② **全局待确认列表/数量**：P3 补（`services::recovery::attention_overview`），P8 正等着（P7 验收记录 §6.1 第 3 条、P7 计划的「评审补充：恢复提示与时钟校正」第 1 条；原记 `:611`/`:245`）；
  ③ P7 计划的「遗留与边界（2026-10-04 Task 1b 登记）」把 `retry_recovery` 写成"P3 的对账入口"（原记 `:826`）——**实现是 P2 的**（`coordinator.rs:1265`），且**生产零调用**；P3 不重写实现，只补 **S12 `AppState::retry_recovery`** 这个包装（`faulted` 的唯一生产出口），触发归 P8、看门狗归 P6（详见「下游接口」第 3 条与 **R3**）。
- 依据：`coordinator.rs:245/1265`、`grep -rn "retry_recovery" src/`（只有定义与注释）、P7 计划的「遗留与边界（Task 1b 登记）」与「评审补充：恢复提示与时钟校正」第 1 条、P7 验收记录 §6.1 第 3 条（原记 `p7-shell-and-ui.md:826`/`:245`、`p7-acceptance.md:611`）。
- 为什么：把已实现的东西再列成"待产出"，会让 P8 去接一个不存在的入口；而 `retry_recovery` 被写进 P7 记录却没有任何生产调用点，是同一类漂移。

**顺带-1｜"UI 与 IPC 归 P7"已过期** → 见「边界」第 4 条：归 **P8**（依据 P7 验收记录 §6.1 第 3 条与第 1 条、P7 计划的「评审补充：恢复提示与时钟校正」第 1 条；原记 `p7-acceptance.md:611`/`:609`、`p7-shell-and-ui.md:245`）。

**顺带-2｜P7 验收 §6.5 第 23 条要登记进 P3** → 「完成门槛」与 Task 6 里登记：计时族命令"提交后重建失败"那一笔**不发 `domain.changed`**，P3 的会话联动沿用同一例外（不新增第二条广播路径），收敛路径与归属见 P7 验收记录 §6.5 第 23 条，以及 P7 计划的「遗留与边界（2026-10-04 Task 1b 登记）」里那条同名条目（含它的"收敛路径"段；原记 `:651`、`p7-shell-and-ui.md:815-828`）。

**顺带-3｜`OverlappingInterval` 的注释写着"同会话"** → 计划里点明正确口径：P3 的 `correct`/`reconcile` 是**跨会话**检测（02 §3 原文"检查版本、区间与重叠"）；实现时**顺手把 `domain/error.rs:37` 那句注释改成"与另一段有效人工区间（可跨会话）"**（只改注释，不改变体与文案形状——`tests/error_contract.rs` 钉着变体清单与中文文案）。**本计划不现在改代码**。

**顺带-4｜`services/tx.rs:9-10` 的使用者清单过期** → P3 实现时同步更新为"`catalog` / `daily_plan` / `recovery` / `history` / `tasks`"（依据：`services/tx.rs:9-10` 现文只列前两个，而 S2 的五个服务都要用它）。

**新增-1｜`reconcile`/`discard_session` 必须清 `session.needs_review`**（原计划只在区间上写"清 `needs_review`"）
- 依据：`coordinator.rs:813`（`resume` 遇 `session.needs_review` 直接 `RecoveryRequired`）、`primitives.rs:85-92`（`end_session_in_tx` 同样拒绝）、`session_repo.rs:254`（`SessionStateUpdate::needs_review` 的注释自己写着"异常分割置真、确认/作废置假"）、`grep -rn "needs_review: Some(false)" src/`（**生产代码零处**——今天没有任何路径会清它）。
- 为什么：不清就等于"确认完仍不能 resume / 不能 finish"，是 C1 之外的第二个闭环断点。

**新增-2｜历史命令提交后必须刷新协调器内存镜像**
- 依据：`coordinator.rs:376`（`build` 用 `live` 出快照）、`:266`（`load_session` 是 `pub` 的镜像重建入口）、`:615`（`rebuild_from_committed` 是 P2 的提交后收尾）、`bootstrap.rs:749`（采样线程每拍都出快照）。
- 为什么：`reconcile`/`discard_session`/`transition_task` 会改到协调器**正镜像着**的会话（当前 run 的 recovering、正在计时的 running）；不刷新，`live` 会继续冒充已提交事实（例如作废后仍按 running 计暂计）。

**新增-3｜"更新任务状态/质量"里的质量半边在 V0.1 不存在**
- 原：「同事务复用 P2 结束原语结束全部 running/paused、更新任务状态/质量、写 task_change」。
- 新：只做**状态**与联动；`task_repo::transition_task` 的既有质量规则原样复用（终结态保留原值、其余清空、Reopen 清空，`:306-315`），**不扩签名、不新增质量写入口**。F-404 的 `quality` 消费属 V0.5。
- 依据：`task_repo.rs:292-346`（签名没有 quality 参数）、`schema_v1.rs:68-74`（质量组合 CHECK）、04 的 F-404 条（返工识别："快但返工"与"快且干净"依据 `quality` 区分，M08/§24 —— 属 V0.5；原记 `04-functional-spec.zh.md:132`）。
- 为什么：加一个 quality 参数要改 5 处生产调用点 + 6 处测试调用点，而 V0.1 没有任何入口会传它（P8 的任务动作只做状态）。

### fix round 2（2026-10-04，终审判定 + 补正）

> 终审结论：三处 Critical 的修法语义成立、S1–S11 零幻影、第 1 类放宽 `recovering + needs_review=1` 是**必需**的（不放宽会与 P2 的 Unavailable 合法输出死锁）。本节把终审提的补正逐条落地。编号沿用终审清单（**注：终审写"6 条小补"，实际列出 7 条，即 R4–R10**）。

**R1｜`PendingIntervalItem` 只有名字没有定义**
- 原：`attention_overview` 的 DTO 只给了一句字段清单，`PendingIntervalItem` **从未定义**（Task 1 的 `StartupScanReport.attention` 也引用了它）。
- 新：新增 **§0.5「P3 新增的 DTO 形状（钉死；P8 照抄）」**，把 `StartupScanReport` / `AttentionOverview` / `SessionAttentionItem` / `PendingIntervalItem` 的字段逐条写死（`PendingIntervalItem`：`id`/`started_at`/`ended_at`/`duration_ms`/`sampled_end_wall_at`/`needs_review`；父项另有 `task_id`/`run_id`/`is_current_run`/`fault_reason`/`session_row_version`/`session_needs_review`），并写明"**这些类型是 P3 产出接口的一部分**：P3 的测试按字段钉住、P8 照抄、改字段＝改下游接口"。**故意不带** `session_id`（父项已有）与 `voided_at`（待确认集合按定义 `voided_at IS NULL`；已作废的只在历史/审计里看，02 §6）。
- 依据：本计划原 `p3:492` 与 `p3:322`；字段同源处 `session_repo.rs:34`（`IntervalRow`）。
- 为什么：P8 是消费方，"只有名字"的 DTO 到 IPC 层必然被重新发明一次——那是漂移的起点。

**R2｜全局待确认概览没有 IPC 归属**
- 原：P3 声明"不新增 `#[tauri::command]`"（`p3:27`），下游接口又写"P8 用它"，但 P8 侧没有点名这条命令。
- 新：「下游接口」第 2 条写死："**IPC 命令由 P8 新增**（控制器已通知 P8 计划登记 8 条命令，含本条；P3 只交付服务入口与 DTO 形状）"；§0.5 末尾复述同一句。
- 依据：`p3:27`、`p3:492`；`grep -n "attention_overview" docs/superpowers/plans/2026-10-03-p8-stats-recovery-export-ui.md` → **0 命中**（确实没人点名）。
- 为什么：一条"两边都以为对方接"的服务入口，正是 V0.1 恢复确认界面最终没人做的典型原因。

**R3｜`retry_recovery` 的归属与调用时机（`faulted` 没有生产出口）**
- 原：「P3 不新造同名入口：故障态重试属协调器/P6 的故障路径」（把归属推给 P6，而 P6/P8/总纲对它 **0 命中**）。
- 新：四件事分开写死——**实现 P2**（`coordinator.rs:1265`）／**生产出口 P3**（新增 **S12 `AppState::retry_recovery`**：先 `coordinator.retry_recovery(db)`、再 `rescan_recovery()`，返回提交后快照）／**触发 P8**（新增一条 IPC 命令＝用户显式重试；**不做定时自动重试**，理由 08 §1"故障不能自己把证据擦掉"）／**采集与看门狗 P6**（含把本条登记进 P6 自己的入口清单；该文件改动由控制器统一落盘）。并写清与 `rescan_recovery` 的分工、以及硬故障不能解除（`:1270-1272`）。
- 依据：`coordinator.rs:170/322`（`faulted` 只在内存）、12 处置真点 `:624/636/641/645/653/663/953/1067/1090/1104/1154/1243`、`refuse_if_faulted:315` 守 10 个入口、`retry_recovery:1265`；`grep -rn "retry_recovery" docs/superpowers/plans/ | grep -v p3` → 只有 P2 计划与 P7 计划的「遗留与边界（2026-10-04 Task 1b 登记）」条（原记 `:826`）。
- 为什么：`faulted` 一旦置真（异常事务失败、磁盘满/busy、提交后重建失败），**除这一条路没有任何出口** ⇒ "计时可用"在故障路径上不闭环。

**R4｜S5 的两个分支字段填法未写**
- 新：§0.3 S5 补**分支填法表**（有检查点：`Some(原 id)`/`Some(t)`/`Some(新 id)`/`t`；无检查点：`None`/`None`/`Some(原 id)`/`started_at`），并写死"没有开放区间 ⇒ `DomainError::NoOpenInterval`（`domain/error.rs:26`），由扫描层按第 1 类降级（进 `faults`，reason `running_without_open_interval`）后继续下一行；不许 abort 整批、不许静默返回全 `None` 的 split"。
- 依据：`session_repo.rs:309`（`AnomalySplit` 三个 `Option`）、`:347-355`（P2 的全 `None` 分支——在 P3 语义下把损坏伪装成没事）。
- 为什么：三个 `Option` 不写清，实现者会照 P2 抄；抄到"全 `None`"就等于把第 2 类静默变成"什么都没发生"。

**R5｜`local_days_covering` 的空范围/反向范围语义**
- 新：§0.3 S10 增语义条：`from == to` ⇒ 空 `Vec`；`from > to` ⇒ `DomainError::NegativeInterval`（与 `IntervalRange::new:14` 同判据，**不静默交换端点**）；端点正好落在日界上**不含次日**。
- 依据：`domain/interval.rs:14`、`:35`（半开相交，端点相接不算）。
- 为什么：这三种输入在 P5 的跨日分桶里都会出现（空范围、调用方算反、范围正好卡零点），不写清就各写各的兜底。

**R6｜`reconcile` 收尾取 `MAX(ended_at)` 的口径**
- 新：写死 `COALESCE((SELECT MAX(ended_at) FROM work_interval WHERE session_id = ? AND voided_at IS NULL AND ended_at IS NOT NULL), session.started_at)`；并补**收尾后形态不变量**：该会话不得再有 `voided_at IS NULL AND ended_at IS NULL` 的区间（测试按**全表**断言）。
- 依据：`schema_v1.rs:93`（`ck_session_range`）、`:116-124`（`ck_interval_duration`）、`session_repo.rs:569`（残留开放区间的判据）。
- 为什么：少 `voided_at IS NULL` 会把刚作废的段算进会话结束时刻；少 `ended_at IS NOT NULL` 会被 P2 遗留的"终点未知"段带成 `NULL` 参与比较；少 `COALESCE` 会让"全部作废"的会话拿到 `NULL` 结束时刻。

**R7｜`DiscardUncertain` 时 `ranges` 的合法性**
- 新：`DiscardUncertain` 的 `ranges` **必须为空**（作废目标集合由服务取 `needs_review=1 AND voided_at IS NULL` 的全部），非空 ⇒ 整条命令拒绝。
- 依据：02 §4「丢弃不确定区间…保留此前有效闭合区间」+ Task 2 的"一次处理全部待确认"口径。
- 为什么：允许客户端指定子集，"一次一条"的歧义就从后门回来了（正是 Task 2 要禁止的形态）。

**R8｜`discard_session` 之后 `live` 停在 `discarded`**
- 新：Task 4 补一句"与 `finish` 停在 `finished` **完全同一口径**（`tests/timer_commands.rs:778`）；`live` 只表示本 run 最后装载过哪条会话；**P8 登记界面表现**（作废后显示无活动会话/已作废，不得按 running 计暂计）"。
- 依据：`tests/timer_commands.rs:777-780`、`coordinator.rs:376-460`（`build` 只按 `live` 出快照）。
- 为什么：这是"看起来像缺陷"的既有口径；不写一句，实施者会去"修"它（例如把 `live` 清成 `None`），从而与 P2 的 `finish` 分叉。

**R9｜02 §4 表与本计划的分割形态冲突**
- 新：Task 1 第 2 类补 **spec 冲突登记**：「02 §4 的恢复表第 2 行（"running 且有开放区间…只将该区间 `needs_review=1`"）是单行形态，与本节的**两行分割**不一致；同文件 §3（"在最后可信检查点停止可疑区间并进入 recovering"）、§9（检查点与 `duration_ms` 口径）与 08 §1（"异常时保留至最后可信检查点的闭合前缀…没有可信检查点则整个当前区间待确认"）都支持两行分割 ⇒ **实现以本节为准；该表行待订正**（spec 由控制器统一落盘）。原记 `02-data-model.zh.md:138`/`:127`/`:248`、`08-implementation-contracts.zh.md:14`」。
- 依据：02 §4 的恢复表第 2 行 vs 同文件 §3 与 §9、08 §1（原记 `02-data-model.zh.md:138`/`:127`/`:248`、`08-implementation-contracts.zh.md:14`）。
- 为什么：不登记，实施者照 02 §4 表写就会**复现 C3**。

**R10｜`TaskTransitionReport` 缺 `revision`/`data_epoch`，且禁止二次取样**
- 原：DTO 带 `snapshot: TimerSnapshot`、没有 `revision`/`data_epoch`；且"没有会话被结束时调 `coordinator.snapshot(db)`"会在命令路径里**再提交一笔系统恢复事务**。
- 新：DTO 改为 `{ task, ended_sessions, paused_sessions, revision, data_epoch }`，两个版本字段**只来自这次写事务**（`settle` → `require_meta`，与 P4 的 `TaskChange` 同一条路，`catalog.rs:223`）；**禁止**该路径调 `Coordinator::snapshot(db)`；收尾只在"确实结束/暂停了会话"时做、只用 `rebuild_from_committed` 重建镜像、其返回值不进版本字段；没有会话被结束时**不动镜像、不二次取样、不二次事务**。
- 依据：`coordinator.rs:342-348`（`snapshot` 自己取样、异常时提交系统事务）、`:615`（`rebuild_from_committed`）、`:420-424`（`build` 每次重读任务行 ⇒ 任务行变化不必动镜像）。
- 为什么：两次取样会让"写事务的 revision"与"快照的 revision"相差 1，P8 的 `announce` 与前端水位就说不清广播的是哪一次写。


### fix round 3（2026-10-04：跨文档引用一律改为定位式）

> 触发：本计划登记的「P3→P6 行号已过期」获授权修（P6 计划在并行修订中变到 411 行、P8 计划变到 244 行，行号全漂）。**只改本计划这一份文件**；代码（`src-tauri/**`）的 `file:line` 引用**保留**。

**改了什么**：本计划里**所有**指向其它文档（P1–P8 计划、总纲、P7 验收记录、spec）的行号引用，一律改成**小节名 / 任务编号 / 条目名**的定位式写法。每一条都现场 `grep -n` 核过目标文档，**没有用替换式改写**（并行修订会让行号继续漂）。**证据不丢**：被替换掉的行号都在原地以「原记 `:NNN`」保留。

**新约定（此后写 P3 相关文档一律照此）**：
- 指别的计划 → 写「P6 计划的 Task 1 第 ④ 步」「P6 计划的 Task 4『恢复的执行骨架与顺序』」「P7 计划的『评审补充：恢复提示与时钟校正』第 1 条」「P8 计划的『P8 新增的 IPC 命令（9 条）』第 7 条」，**不写行号**；
- 指 P7 验收记录 → 写章节 + 条目号（§6.1 第 3 条 / §6.5 第 23 条 / §6.6 第 31 条 / §6.7 第 37 条）；
- 指 spec → 写 §号 + **引文原句**（spec 也会被订正，例：02 §4 的恢复表第 2 行正要改）；
- 指代码 → **保留 `file:line`**，但必须同时给**符号名**；行号漂了就按符号 `grep`（§0 顶部那句硬要求）。

**对照表（旧行号 → 新定位式；旧行号＝当时核对的证据快照）**

| 旧引用 | 出现在 | 现在的定位式 |
| --- | --- | --- |
| P6 计划 `:71`/`:178` | §0.3 S1、Task 1、下游接口 7 | P6 计划 **Task 1「单实例与启动顺序的硬化」启动顺序第 ④ 步**（"调用 **P3** 的恢复扫描"）+ **Task 4「WAL 一致备份与恢复」的「恢复的执行骨架与顺序」条**（明写与 P3 的 S1 共用同一函数） |
| P6 计划 `:65` | 下游接口 7 | P6 计划 **Task 1「P7 已建立这套顺序并有测试」条**（"把 P7 的「开发验证库」门禁升级为正式恢复扫描接入"）+ 文末「与提前外壳的责任衔接」 |
| P2 计划 `:140` | §0.3 S12 | P2 计划的 **「非运行态硬故障补全」末条**（硬故障不能通过 `retry_recovery` 解除） |
| P8 计划 `:177` | §1 I5 依据 | P8 计划的 **「轻量 GTD 与联动入口的最终整合」第 1 条** |
| P7 计划 `:245` | §1 I6②/顺带-1、下游接口 2 | P7 计划的 **「评审补充：恢复提示与时钟校正」第 1 条** |
| P7 计划 `:826`、`:815-828` | §1 I6③/依据/顺带-2、下游接口 3、§1 R3 依据 | P7 计划的 **「遗留与边界（2026-10-04 Task 1b 登记）」**里那条「计时族命令「提交后重建失败」那一笔不发 `domain.changed`」（含它的"收敛路径"段） |
| P7 验收 `:609`/`:611` | 边界、§1 I5/I6/顺带-1 | P7 验收记录 **§6.1 第 1 条 / 第 3 条** |
| P7 验收 `:651` | §1 顺带-2、Task 6、完成门槛 | P7 验收记录 **§6.5 第 23 条** |
| P7 验收 `:667` | §1 I2 依据 | P7 验收记录 **§6.6 第 31 条** |
| P4 计划 `:159-160` | Task 2 版本条 | P4 计划的 **「本轮开工审查补全」里「interval 没有独立 `row_version`」条** |
| P5 计划 `:19`/`:37` | 下游接口 6 | P5 计划的 **「依赖的前置计划」里的 P3 条** + **Task 1「统计口径——范围裁剪、排除与人工/机器分离」第一条** |
| `02-data-model.zh.md:138`/`:127`/`:248`、`08-implementation-contracts.zh.md:14` | §1 R9、Task 1 的 spec 冲突登记 | 02 的 **§4 恢复表第 2 行 / §3 / §9** 与 **08 §1**（并保留引文原句） |
| `04-functional-spec.zh.md:132` | §1 新增-3 依据 | 04 的 **F-404 条**（返工识别，M08/§24 → V0.5） |

**核对命令**：对 P6/P2/P8/P7/P4/P5 六份计划分别 `grep -n` 目标小节名（"调用 \*\*P3\*\* 的恢复扫描"、"恢复的执行骨架"、"非运行态硬故障补全"、"轻量 GTD 与联动入口的最终整合"、"评审补充：恢复提示与时钟校正"、"遗留与边界（2026-10-04 Task 1b 登记）"、"本轮开工审查补全"、"统计口径——范围裁剪"），对 P7 验收记录 `grep -n "### 6.1\|### 6.5\|### 6.6"`。

**未动的**：§1 里已登记的「原 → 新」结论**一字未改**（只把其中的行号补成定位式并保留「原记」）；代码引用全部保留，复核后**零漂移**（135 处 `file.rs:N` + 58 处「符号:行号」）。

### fix round 4（2026-10-04：4 处**带符号名**的过期行号）

> 触发：复评实测出 4 处"带符号名但行号过期"的代码引用（并指出我上一轮"代码引用零漂移"这句不实）。**只改本计划**；改法按复评要求：**优先只留符号名**，必须给行号的地方用现场 `grep -n` 的值并就地标注「**以符号名为准**」。

**逐条（原 → 新）**

| # | 位置 | 原 | 新 | 现场核对 |
| --- | --- | --- | --- | --- |
| 1 | §0.3 **S9** | 原写 `tests/startup_order.rs` **第 340 行**（当时错的行号） | `tests/startup_order.rs` 的 **`the_recovery_scan_only_counts_sessions_of_other_runs`**（**只留用例名/符号，不给行号**） | `grep -n "scan_recovery(db.connection()" tests/startup_order.rs` → `:466`；那个用例 `fn the_recovery_scan_only_counts_sessions_of_other_runs()` 在 `:399` |
| 2 | §2 闭环链·第 ④ 步那一行 | 原写 `bootstrap.rs` **第 698 行**（当时错的行号） | （替换 `bootstrap::startup` 里那次 `scan_recovery` 调用；`bootstrap.rs:699`，**以符号名为准**） | `grep -n "let recovery = scan_recovery" src/services/bootstrap.rs` → `:699` |
| 3 | §2 闭环链·`scan_recovery`/`AppState.recovery` 那一行 | 原写 `scan_recovery` 的 **第 313 行**（当时错的行号） | `scan_recovery` +（`bootstrap.rs:314`，**以符号名为准**） | 同上 → `pub fn scan_recovery` 在 `:314` |
| 4 | 同上那一行 | 原写 `AppState.recovery` 的 **第 707-714 行**（当时错的行号） | `AppState.recovery`（构造点写成 `Arc::new(AppBoundary { state: Mutex::new(AppState { … }) })`；`bootstrap.rs:708-715`，**以符号名为准**） | `sed -n '705,720p'` → `:708` `let app: SharedApp = Arc::new(AppBoundary {`，`:715` `});` |

**自核（按复评要求，现场重核了计划里**全部** `bootstrap.rs:N` 与 `tests/startup_order.rs:N`）**：

| 引用 | 现场行内容 | 结论 |
| --- | --- | --- |
| `bootstrap.rs:295` | `pub struct RecoveryScan {` | ✓ |
| `bootstrap.rs:314` | `pub fn scan_recovery(conn: &Connection, …)` | ✓（本轮订正） |
| `bootstrap.rs:341` / `:342-344` | `pub struct AppState {` / 三个字段 `db`/`coordinator`/`recovery` | ✓ |
| `bootstrap.rs:361` / `:370` / `:399` | `AppBoundary` / `AppGuard` / `lock_app` | ✓ |
| `bootstrap.rs:424-608` | `impl AppState {` … `}` | ✓ |
| `bootstrap.rs:458` / `:466` / `:466-471` | `pub fn recovery` / `pub fn guard_business_timing`（函数体到 471） | ✓ |
| `bootstrap.rs:474` / `:483` | `pub fn start` / `pub fn resume` | ✓ |
| `bootstrap.rs:546` / `:552-555` | `pub fn explicit_exit` / 开事务那段（`let tx = db` → `.map_err(map_sqlite)?;`） | ✓ |
| `bootstrap.rs:641` | `pub fn startup(` | ✓ |
| `bootstrap.rs:684` | `let sample = clock.sample()…` | ✓ |
| `bootstrap.rs:699` | `let recovery = scan_recovery(db.connection(), &run_id)?;` | ✓（本轮订正） |
| `bootstrap.rs:704` | `let mut coordinator = Coordinator::new(clock, run_id.clone());` | ✓ |
| `bootstrap.rs:708-715` | `let app: SharedApp = Arc::new(AppBoundary {` … `});` | ✓（本轮订正） |
| `bootstrap.rs:749` | `fn sampling_action(…)` | ✓ |
| `tests/startup_order.rs`（S9，本轮改为用例名） | 该用例在 `:399`，内部 `scan_recovery` 调用在 `:466` | ✓（原写 `:340` 是过期的） |
| `tests/startup_order.rs:466` | `let scan = scan_recovery(db.connection(), "run-now").unwrap();` | ✓ |
| `tests/startup_order.rs:69` | `fn fixture() -> Fixture {` | ✓ |

**订正一句**：fix round 3 记录里"代码引用…复核后零漂移"的措辞**不准确**——实际有上面 4 处过期（全部集中在 §0.3 S9 与 §2 闭环链，都是带符号名因而可定位、但行号没跟上镜像 21:02 的 +1 漂移）。本轮已订正，并把「以符号名为准」写在每一处保留行号的地方。（不改 fix round 3 的历史结论，仅在此登记订正。）

---

## 2. 功能闭环链（必须完整；缺一环就不算做完）

```
崩溃（旧 run 留下 running/paused/recovering 会话与待确认区间）
  └─ 启动：单实例 → 开库/迁移 → 新建 application_run（P7 的 bootstrap::startup:641）
       └─ 第 ④ 步 调 P3 扫描服务 services::recovery::scan_at_startup（替换 `bootstrap::startup` 里那次 `scan_recovery` 调用；`bootstrap.rs:699`，**以符号名为准**）
            ├─ 第 1 类 不变量损坏  → 只诊断（S9 谓词），不写事实；reconcile 拒绝它
            ├─ 第 2 类 running+开放 → S5 归一：可信前缀闭合 + 终点未知的待确认段；会话 recovering
            ├─ 第 3 类 recovering    → 原样保持（不推断、不加工时）
            └─ 第 4 类 paused 无开放/待确认 → 保持 paused，run_id 重绑当前 run（可被 finish/resume 了）
       └─ 随后仍由 P7 的 `scan_recovery` 算门禁快照（`bootstrap.rs:314`，**以符号名为准**）→ `AppState.recovery`（构造点在 `bootstrap::startup` 的 `Arc::new(AppBoundary { state: Mutex::new(AppState { … }) })`；`bootstrap.rs:708-715`，**以符号名为准**）
  └─ 待确认可见：services::recovery::attention_overview（全局列表/数量，含终点未知的片段）
  └─ 用户处理：AppState::{reconcile, correct, backfill, discard_session}
       ├─ reconcile(confirm|discard_uncertain)：一次事务处理该会话全部待确认区间
       ├─ correct：仅 finished 的可信历史（重定时/软删除）
       ├─ backfill：手工补录（不占前台、不伪造完成事件）
       └─ discard_session：作废整次（全部区间 voided + discarded）
  └─ **门禁解除**：AppState::rescan_recovery（S1）在 reconcile / discard_session 提交后重算
       ├─ 仍有未结束/待确认/损坏（别的 run）⇒ requires_recovery() 为真 ⇒ start/resume 继续拒绝
       └─ 全部处理完 ⇒ 门禁放开
  └─ **计时可用**：start / resume（唯一两个受门禁约束的入口，bootstrap.rs:474/483）
       └─ 若本 run 还挂着"已检测但未接受的墙钟校正"（coordinator.rs:172）
            ⇒ sample_and_detect(:983) 继续返回 RecoveryRequired（这与门禁字段是**两把闸**）
            ⇒ 走 AppState::accept_detected_clock_correction（S4）：写审计 + revision，提交后调
              Coordinator::accept_clock_correction(:245) 清标记 ⇒ 计时真正恢复
       └─ 若协调器已进入**故障态**（faulted：异常事务失败 / 提交后重建失败 / 无基线采样失败）
            ⇒ 所有入口被 refuse_if_faulted(:315) 拒绝（含查询与统计）
            ⇒ 走 AppState::retry_recovery（S12）：retry_recovery(:1265) 成功提交后清 faulted
              ⇒ 紧接着 rescan_recovery()（S1）⇒ 计时可用
            ⇒ 例外：MonotonicBackwards 硬故障一律 RECOVERY_REQUIRED（需新 run），S12 解不开
```

时钟校正审计路径（同 run 显式接受）：
1. 检测到墙钟异常 → P2 已提交恢复事务（`handle_anomaly:1055`）→ `recovering`；若再检出墙钟异常则置 `unaccepted_clock_correction`（`:1096-1106`）；
2. 用户显式接受 → S4（`accept_detected_clock_correction`）：`guard_epoch` → 取一次样本并观察 → 写 `time_edit`（`before_json` = 三参照点，`after_json` = 本次样本 + `clock_correction_accepted: true` + `intervals_changed: false`）→ `bump_revision` 恰好一次 → 提交 → `accept_clock_correction(sample)`；
3. **它不确认任何可疑工时**——那是 `reconcile` 的事（02 §4/§10、08 §1 原文）。

---

## Task 1：启动恢复扫描与共享恢复原语

**文件：** `src-tauri/src/services/recovery.rs`（新建）、`src-tauri/src/services/mod.rs`（注册）、`src-tauri/src/services/bootstrap.rs`（第 ④ 步改调 + S1）、`src-tauri/src/storage/session_repo.rs`（S5/S9）、`src-tauri/tests/recovery_scan.rs`（新建）。

**输入 / 输出：**
```rust
// services/recovery.rs
pub fn scan_at_startup(db: &mut Db, current_run_id: &str, now: i64) -> Result<StartupScanReport, AppError>;
// `StartupScanReport` / `SessionAttentionItem` / `PendingIntervalItem` 的**字段形状钉在 §0.5**
// （fix round 2：因 R1——原计划只在 Task 1 给了一半字段，`PendingIntervalItem` 连定义都没有）。
```
`now` = 启动第 ③ 步那次采样的 `sample.wall_ms`（`bootstrap.rs:684`），只用于审计行 `created_at`；**不当作区间终点**（C3）。

**依赖的真实符号：** `session_repo::{unfinished_sessions_of_other_runs:482, pending_intervals_of_other_runs:515, invariant_faults_of_other_runs:552, update_session_state:262, SessionStateUpdate:248, AnomalySplit:309}`、`checkpoint_repo::latest:129`、`session::SessionState::{Running,Paused,Recovering}`、`SessionAttention:179`、`meta::{bump_revision:64, require_meta:37}`、`services/tx.rs::settle:40`、`bootstrap::{scan_recovery:314, RunningApp::recovery:225, AppState::recovery:458}`。

- [ ] **扫描时机与顺序沿用 P7 已建立的入口**：`bootstrap::startup`（`:641`）的第 ④ 步（`:699`）从 `scan_recovery(db.connection(), &run_id)?` 改成 `services::recovery::scan_at_startup(&mut db, &run_id, sample.wall_ms)?`，**紧接着仍调一次 `scan_recovery(db.connection(), &run_id)?`** 算门禁快照（`RecoveryScan`/`scan_recovery` 的语义与签名**不动**：`tests/startup_order.rs:466` 直接调它）。P3 不自建启动流程；P6 在恢复/替换库路径复用同一入口（P6 计划的 Task 1 启动顺序第 ④ 步 + Task 4 的「恢复的执行骨架与顺序」条；原记 `:71`/`:178`）。
- [ ] **判定顺序不可颠倒**（02 §4 表）：先看不变量是否可信，再看有无开放/待确认区间，最后看当前状态。
  1. **状态/区间不变量损坏** → **只诊断、隔离、禁止自动修复事实**；P3 的判据是 S9 的 `invariant_faults(conn, Some(current_run_id))`（= 今天 `:552` 的三条谓词），**不写任何行**、不加版本。**收紧一处判据（C3 衍生）**：第 3 条 `open_interval_outside_running` 要放行 `s.state='recovering' AND i.needs_review=1`——那是 P2 在采样失败分支留下的"终点未知"合法形态（`read_sample` 的失败分支 `:946-957` → `handle_anomaly:1055` → `try_handle_anomaly:1081`；`split_for_anomaly` 的 `sampled_wall_at=None` 两处写 `NULL`：`:379`/`:405`）（08 §1「没有可信检查点则整个当前区间待确认」），不是损坏；否则 P2 的正常输出会被判成自造损坏。
  2. **`running` 且有开放区间**（只可能是别的 run）→ 调 **S5 `normalize_crashed_open_interval(tx, session_id)`**，精确动作（C3）：
     - 闭合点 `t` = `checkpoint_repo::latest(tx, iv_id)?.map(|c| c.attribution_at).filter(|t| *t > iv.started_at)`；
     - **有检查点**：原区间成为**可信前缀**——`ended_at = t`、`duration_ms = t - iv.started_at`、`sampled_end_wall_at = cp.wall_at`、`needs_review = 0`；**另插一行待确认段**——`started_at = t`、`ended_at = t`（候选端点，不是事实）、`duration_ms = NULL`、`sampled_end_wall_at = NULL`、`needs_review = 1`、`voided_at = NULL`；
     - **无检查点**（或 `attribution_at <= started_at`）：没有可信前缀，原区间整体待确认——`ended_at = iv.started_at`、`duration_ms = NULL`、`sampled_end_wall_at = NULL`、`needs_review = 1`；
     - 两种情形都**不得**留下 `voided_at IS NULL AND ended_at IS NULL` 的行（`uq_open_interval:185` + `open_interval_outside_running:569` 的双重理由）；
     - 会话：`state = 'recovering'`、`needs_review = true`（`SessionStateUpdate:248`）、**`run_id` 保持不变**（02 §10：recovering 保持原恢复归属，直到 `reconcile` 更新并审计）、`row_version + 1`（由 `update_session_state` 负责）。
     - **spec 冲突登记（2026-10-04 fix round 2：因 R9）**：02 §4 的恢复表第 2 行（"running 且有开放区间…只将该区间 `needs_review=1`"；原记 `02-data-model.zh.md:138`）写的是「session 设 recovering；**只将该区间 `needs_review=1`**」（单行形态），与本节的**两行分割**（可信前缀闭合 + 新待确认段）不一致。同文件 §3（"在最后可信检查点停止可疑区间并进入 recovering"）、§9（检查点与 `duration_ms` 口径）与 08 §1（"异常时保留至最后可信检查点的闭合前缀，剩余部分标 `needs_review`…没有可信检查点则整个当前区间待确认"）都支持两行分割（原记 `:127`/`:248`、`08-implementation-contracts.zh.md:14`）。⇒ **实现以本节为准；该表行已在 2026-10-04 本轮复审同步订正（中英）**。不写这一句，实施者照 02 §4 表写就会复现 C3。
     - **别把 `TimerSnapshot.pending_ms` 当成"有没有待确认"的判据**：归一后的待确认段是零长度候选（`started_at == ended_at`），`Coordinator::pending_ms_of`（`:1366-1376`）对它求和得 0 ⇒ `pending_ms` 是 `None`。判据是 `state == 'recovering'` / 区间 `needs_review = 1`；P8 的展示规则用 `duration_ms IS NULL` 表示"终点未知"（下游接口第 2 条）。
  3. **`recovering`** → 原样保持（含 P2 留下的 `ended_at=NULL` 待确认段），不增加已知工时、**不写任何行**。
  4. **`paused` 且无开放/待确认区间** → 保持 `paused`，`run_id` 重绑当前 run（`update_session_state`，`SessionStateUpdate { run_id: Some(current), .. }`），`row_version + 1`，写 `time_edit`，**不自动继续计时**。
- [ ] **两类事实分开记录**：第 1 类只进 `faults`（`SessionAttention::InvariantBroken`），第 2/3 类进 `attention` 的 `NeedsReview`，第 4 类 `None`；前者不能用 `reconcile` 混过去（Task 2 会再拒一次）。
- [ ] **版本与审计口径**（02 文末「启动扫描的版本与审计补充」原文）：扫描查询本身不加 revision；同一批事务里若**实际改变了** session 状态、`run_id` 或区间事实 ⇒ 每个被改的 session `row_version` 恰好 +1、整个扫描事务 `revision` 恰好 +1、每个被改的 session 写一条 `time_edit`（`before_json`/`after_json` 记清区间前后值 + `candidate_end_source` ∈ {`last_checkpoint`, `interval_start`}）；**没有字段变化就不写审计、不加任何版本**。实现上用 `settle(&tx, WriteOutcome::Changed(report) | WriteOutcome::Unchanged(report))`（`tx.rs:40`）——它是"`Changed` 才 `bump_revision` 一次 + 同事务读回 `revision`/`data_epoch`"的现成落点，**不要**自己再写一次 `bump_revision`。系统事务不开 epoch 守卫（没有客户端请求），照 `AppState::explicit_exit`（`:546`）里开事务那段（`:552-555`）的写法。
- [ ] 第 2 类归一后**不需要**重建协调器镜像：启动路径上扫描发生在协调器创建之前（`:704`），`live` 还是 `None`。P6 的恢复/替换库路径也必须**先扫描、再新建协调器**（P6 计划 Task 4 的「恢复的执行骨架与顺序」条："同一事务里 `start_run` + `rotate_epoch` → `scan_recovery` → `Coordinator::new`"；原记 `:178`），旧协调器整体丢弃——**不要**让扫描去修一个还活着的 `live`。
- [ ] **门禁解除（C1）**：本任务只负责"扫描 + 让事实可被判据看见"；解除动作由 S1 在 `reconcile`/`discard_session` 提交后触发（Task 2/Task 4）。判据不变：`RecoveryScan::requires_recovery()`（`:306`）只要还有别的 run 的未结束/待确认/损坏就继续拒绝。
- [ ] **测试（`tests/recovery_scan.rs`，用 `startup()` + `lock_app()` 真跑；装置抄 `tests/startup_order.rs:69` 的 `fixture()` 与 `tests/ipc_commands.rs:157/175` 的 startup + guard 写法）**：四类各一例（含 `paused` 保持不自动继续、`recovering` 不重复计入）；**第 2 类断言逐字段**——可信前缀的 `ended_at/duration_ms/sampled_end_wall_at/needs_review`、待确认段的五个字段、会话的 `state/needs_review/run_id/row_version`，并断言**全表不存在** `ended_at IS NULL AND voided_at IS NULL` 且会话非 running 的行；第 2 类**再跑一次扫描无变化**（不加 revision）；损坏记录被隔离且不写事实；扫描幂等；**重复扫描无字段变化时不加 revision，有实际变化时恰好 +1**；第 4 类重绑后可被 `finish` 结束（不再是 `StaleRunContext`）；**防御分支**（R4）：手工造一条 `state='running'` 但**没有开放区间**的行时，扫描把它归第 1 类（`faults` 里 `running_without_open_interval`）并**继续处理后面的行**，不 abort 整批。

## Task 2：`reconcile`——确认或丢弃不确定区间

**文件：** `src-tauri/src/services/recovery.rs`、`src-tauri/src/storage/session_repo.rs`（S7/S8）、`src-tauri/src/services/bootstrap.rs`（S1/S2 包装）、`src-tauri/tests/reconcile.rs`（新建）。

**输入 / 输出**（信封承载 epoch 与**会话版本**，见 0.1 的 `WriteEnvelope` 规则）：
```rust
pub enum ReconcileAction { Confirm, DiscardUncertain }
pub enum ReconcileTargetState { Paused, Finished }
pub struct ConfirmedRange { pub interval_id: String, pub started_at: i64, pub ended_at: i64 }
pub struct ReconcileRequest {
    pub session_id: String,
    pub action: ReconcileAction,
    pub target_state: ReconcileTargetState,
    pub ranges: Vec<ConfirmedRange>,   // Confirm 必填且必须**恰好覆盖**该会话全部待确认区间
}
pub struct ReconcileReport { pub session: SessionRow, pub intervals: Vec<IntervalRow>, pub revision: i64, pub data_epoch: String }

pub fn reconcile(db: &mut Db, env: WriteEnvelope, req: ReconcileRequest, now: i64)
    -> Result<WriteOutcome<ReconcileReport>, AppError>;
```
**依赖的真实符号：** `session_repo::{get_session:91, intervals_of_session:105, update_session_state:262, SessionStateUpdate:248}`、S7/S8、`meta::bump_revision:64`、`services/tx.rs::{write_tx:26, settle:40}`、`domain/interval::{IntervalRange:14, IntervalSet:109}`、`AppError::RecoveryRequired`（`error.rs:26`）、`DomainError::{UnknownSession:125, PendingAndVoided:45}`。

- [ ] 前置：仅 `recovering`；非 `recovering` 返回 `DOMAIN_ERROR` 并给出明确中文规则说明；只有未解决的恢复事实使用 `RECOVERY_REQUIRED`，**不新增错误码**；`running`/`paused` 想改可信历史必须先 `finish`，再走 `correct`。**命中第 1 类（不变量损坏）一律拒绝**，文案指向诊断——不允许"确认一下就修好"。
- [ ] **一次事务处理该会话的全部待确认区间**，不做"一次一条"的多次往返。`Confirm` 的 `ranges` 必须与库里的待确认集合**逐一对应**：缺一条、多一条、`interval_id` 不属于该会话、或指向的区间不是"待确认且未作废" ⇒ 整条命令拒绝（零变化）。**待确认集合为空是合法输入**（`ranges` 必须为空）：P2 在「候选终点正好落在检查点上」时会留下一个没有余段的 `recovering` 会话，那是正常结果（`session_repo.rs:548-551` 的注释），此时 `Confirm` 只做状态跃迁。把 `started_at` 往**前**改会撞上可信前缀，由 S7 拒绝（不是特例）。
- [ ] `Confirm` 校验（逐条，半开区间）：`ended_at >= started_at`（零长度合法）；**不与全部有效人工区间重叠**（S7，跨会话，端点相接不算）；**`ended_at <= now`**（未来的"已发生工时"不是事实；与 `require_available_human_start:455` 的"不能越过未来已有记录"同一口径）。`ranges` 之间也要两两不重叠（用 `IntervalSet::insert:119`，命中即 `DomainError::OverlappingInterval:38`）。已知单调时长**只作候选**（`ended_at = started_at` 的零长度候选就是"终点未知"），用户不接受时不强迫。
- [ ] 写入（同一事务）：每条确认 → `confirm_interval`（S8：`started_at`/`ended_at`、`duration_ms = ended_at - started_at`、`needs_review → 0`、`voided_at` 保持 `NULL`、`sampled_end_wall_at` 保持原值）；`DiscardUncertain` → 该会话**全部**待确认区间 `void_interval(now)`（S8：`voided_at = now`、`needs_review → 0`、无时长的候选 `ended_at → NULL`（已知时长的端点保留），**保留此前有效闭合区间**）。`DiscardUncertain` 时 **`ranges` 必须为空**（作废哪些区间由服务从库里取 `needs_review=1 AND voided_at IS NULL` 的全部，不由客户端指定；非空 ⇒ 整条命令拒绝——否则"一次只作废一条"的歧义又回来了。fix round 2：因 R7）。它**不能**用来作废整次会话（那是 `discard_session`，Task 4）。
- [ ] 会话收尾：`state = target_state`；`run_id → 当前 run`（02 §4/§10：**原始恢复归属保留在 `time_edit` 里**）；**`needs_review → false`（新增-1，闭环必需）**；`target_state = Finished` 时 `ended_at` 按**一条口径**取：`COALESCE((SELECT MAX(ended_at) FROM work_interval WHERE session_id = ? AND voided_at IS NULL AND ended_at IS NOT NULL), session.started_at)`——`voided_at IS NULL` 排除刚被作废的段、`ended_at IS NOT NULL` 排除 P2 遗留的"终点未知"段、`COALESCE` 兜住"一条可用区间都没有"（保证 `ck_session_range:93`）。`target_state = Paused` 时**不写 `ended_at`**（`SessionStateUpdate.ended_at = None`，走 `COALESCE` 保持原值）；两者都**不自动计时**。（fix round 2：因 R6。）V0.1 无番茄钟阶段，`phase_state` 相关分支不写。
- [ ] **收尾后的形态不变量**：该会话**不得**再有 `voided_at IS NULL AND ended_at IS NULL` 的区间——`Confirm` 要求 `ranges` 覆盖全部待确认集合、`DiscardUncertain` 把它们全部作废，两条路都会消掉"终点未知"的遗留形态（与 Task 1 第 2 类的形态约束同一条）。测试里要按**全表**断言这一条，不只断言被处理的那几条。
- [ ] 写 `time_edit`（`before_json`/`after_json` 记清每个被处理区间的**前**值（`started_at`/`ended_at`/`duration_ms`/`needs_review`/`voided_at`）与**后**值，以及 `run_id` 的前后值）；`reason` 写明是 `reconcile:confirm` 还是 `reconcile:discard_uncertain`。
- [ ] 版本：`settle` 保证"这一条用户命令恰好一次 `revision`"；会话 `row_version` 由 `update_session_state` +1（**不新增 interval 版本列**，P4 计划的「本轮开工审查补全」里那条「interval 没有独立 `row_version`」；原记 `:159-160`）。**不写 `task_change`**（不移动任何完成时刻）。
- [ ] 提交后（`AppState::reconcile`，S2+S1）：`rescan_recovery()`（**先**重算门禁，仍不可恢复就继续拒绝）+ `coordinator.refresh_committed_session(db.connection(), &session_id)` 刷新镜像（新增-2）；镜像刷新失败映射 `RECOVERY_REQUIRED`（P2 的提交后约定，`coordinator.rs:606-614`）。
- [ ] **测试（`tests/reconcile.rs`）**：全部待确认区间一次处理完；缺一条/多一条被拒；确认的起止重叠被拒（含与"后来已记录的人工时间"重叠、跨会话重叠）；`ended_at > now` 被拒；丢弃该会话全部待确认区间**不**动作废整次（其他会话与既有闭合区间原样）；非 `recovering` 被拒（逐字段比对：状态、版本、起止、时长、`needs_review`、`voided_at`）；第 1 类被拒；`time_edit` 前后值完整；**确认后 `session.needs_review = false` 且该会话能被 `resume`**（闭环证据）；失败整体回滚；**提交后 `AppState::recovery().requires_recovery()` 随事实变化**（还有别的待处理就仍为真）。

## Task 3：`correct`——仅 `finished` 的可信历史修正

**文件：** `src-tauri/src/services/history.rs`（新建）、`src-tauri/src/storage/session_repo.rs`（S8 的 `retime_interval`）、`src-tauri/src/services/bootstrap.rs`（S2）、`src-tauri/tests/correct.rs`（新建）。

**输入 / 输出：**
```rust
pub enum CorrectAction { Retime { started_at: i64, ended_at: i64 }, Delete }
pub struct CorrectRequest { pub session_id: String, pub interval_id: String, pub action: CorrectAction, pub reason: Option<String> }
pub struct HistoryEditReport { pub session: SessionRow, pub interval: IntervalRow, pub revision: i64, pub data_epoch: String }

pub fn correct(db: &mut Db, env: WriteEnvelope, req: CorrectRequest, now: i64)
    -> Result<WriteOutcome<HistoryEditReport>, AppError>;
```
**依赖的真实符号：** `session_repo::{get_session:91, get_interval:98, intervals_of_session:105, update_session_state:262}`、S7/S8、`domain/interval::{IntervalRange:14, IntervalSet:109, IntervalFacts:56}`、`time_edit_repo::write:27`、`services/tx.rs::settle:40`。

- [ ] 前置：会话必须是 `finished`（`SessionState::is_terminal:51`）。`recovering` 返回 `RECOVERY_REQUIRED` 并把用户指向 `reconcile`；`running`/`paused` 返回明确中文错误（先 `finish`）。被修正的区间必须属于该会话、`voided_at IS NULL`、`needs_review = false`（否则 `RECOVERY_REQUIRED` → 走 `reconcile`）。
- [ ] `Retime` 校验：非负、`ended_at >= started_at`；**与全部既有有效人工区间不重叠**（S7，`exclude_interval = Some(该区间)` —— 否则会与它自己相撞；含其他会话；机器时间按独立口径，不互斥）；`ended_at <= now`。
- [ ] 重算该区间的 `duration_ms = ended_at - started_at`（同时改 `started_at`/`ended_at`/`duration_ms` 三个字段，一次 `UPDATE`），**保留修正前后的值到 `time_edit`**；不得留下"改了起止但 `duration_ms` 没跟着变"的行（`ck_interval_duration` 兜底）。
- [ ] **幂等重复**（04 §9 用例 3）：`Retime` 的新起止与现值逐字段相同 ⇒ `WriteOutcome::Unchanged`（不写审计、不加 `revision`、不加 `row_version`），返回当前行。
- [ ] 并发保护用**所属 `session.row_version`**（`env.expected_row_version`），不新增 `interval.row_version`；所有改变该 session 区间事实的命令在同事务增加 `session.row_version`（`update_session_state` 传原状态即可，`SessionStateUpdate::default()`）；修改不同区间的旧版本请求也拒绝，刷新后重新确认。
- [ ] 删除误记 = `void_interval(now)`（**软删除**，保留审计），不是 `DELETE`。
- [ ] **不改 `session.ended_at`**：它记录的是"会话结束那一刻"的事实，02 §3 只要求修正区间与 `duration_ms`；"最后一个区间的结束"要从区间算（P5 的口径）。**不新增或改写 `task_change`**：已完成任务的"完成时刻"**不因修正区间而移动**（报告按 `task_change` 里完成事件的时刻选完成项，不用 `updated_at`、也不用 session 结束时间——02 §10）。
- [ ] 提交后：`refresh_committed_session` 刷新该会话镜像（新增-2）；`correct` 不改恢复性，**不重扫门禁**。
- [ ] **测试（`tests/correct.rs`）**：负区间被拒；与同会话及其他会话的有效人工区间重叠被拒（含端点相接**不算**重叠的边界、`exclude_interval` 不排除自己时的反例）；`ended_at > now` 被拒；删除是软删除且审计留存（`time_edit` 有行、区间行仍在）；`recovering` 被指向 `reconcile`；修正后 `duration_ms` 与起止一致；幂等重复零变化；被拒时逐字段比对（状态、版本、起止、时长、`needs_review`、`voided_at`、`revision`、`time_edit`/`task_change` 行数）。

## Task 4：`backfill` 与 `discard_session`

**文件：** `src-tauri/src/services/history.rs`、`src-tauri/src/services/recovery.rs`（`discard_session`）、`src-tauri/src/storage/session_repo.rs`（S6/S7/S8）、`src-tauri/tests/backfill_discard.rs`（新建）。

**输入 / 输出：**
```rust
pub struct BackfillRequest { pub task_id: String, pub started_at: i64, pub ended_at: i64 }
pub struct DiscardSessionRequest { pub session_id: String }
// 两个都返回 WriteOutcome<HistoryEditReport>（字段同上；discard 的 interval 取被作废的第一条）

pub fn backfill(db: &mut Db, env: WriteEnvelope, req: BackfillRequest, now: i64, run_id: &str)
    -> Result<WriteOutcome<HistoryEditReport>, AppError>;
pub fn discard_session(db: &mut Db, env: WriteEnvelope, req: DiscardSessionRequest, now: i64)
    -> Result<WriteOutcome<HistoryEditReport>, AppError>;
```
**依赖的真实符号：** S6/S7/S8、`task_repo::get_task:125`、`session_repo::{get_session:91, intervals_of_session:105, update_session_state:262}`、`domain/interval::IntervalRange:14`、`schema_v1.rs::{ck_interval_duration:116, ck_session_range:93, uq_running_foreground:183}`。

- [ ] `backfill`：校验任务存在（`task_repo::get_task`）、范围合法（`ended_at >= started_at`、`ended_at <= now`）、与既有人工时间不重叠（S7）；用 **S6** 创建 `finished` 会话与**可信闭合**区间（`needs_review = 0`、`duration_ms = ended_at - started_at`、`sampled_end_wall_at = NULL`——补录没有采样，**不得**把服务算的时刻写进那一列），写 `time_edit`。**不启动计时、不占前台槽位**（直接插 `finished` 行，`uq_running_foreground:183` 不受影响）、**不伪造 `task_change` 完成事件**（02 §3 原文）、不冻结估时基准（那是 `start` 的事）、不写 `interval_checkpoint`。
- [ ] `backfill` 的固定字段（新增判定）：`mode = FOREGROUND`（02 §6：人工只有 FOREGROUND）、`timer_kind = stopwatch`、`target_duration_ms = NULL`（`ck_timer_budget:94`）；`run_id` = 当前 run。V0.1 不提供"补录一个倒计时/机器会话"的入口。
- [ ] `discard_session`：**全部区间**（含运行中的开放区间）`void_interval(now)`：`voided_at = now`、`needs_review = 0`、**已知时长区间的 `ended_at` 原样；无时长候选的 `ended_at` 清为 `NULL`**（开放区间仍保持 `NULL`——S8 的理由：`ck_interval_duration` 不允许"作废且已闭合却没有 `duration_ms`"，而给终点未知的段补时长就是造数；`open_interval_outside_running` 那条判据只查 `voided_at IS NULL`（`session_repo.rs:572`），作废段不会被判成残留开放区间）；会话设 `discarded`、`ended_at = max(now, session.started_at)`（`ck_session_range:93` 兜底）、`needs_review → false`（新增-1）、**`run_id` 不动**（终态会话不再参与门禁与联动）；写 `time_edit`。**不删除审计、不隐式改变任务状态**（02 §3 原文）。作废后协调器镜像停在 `discarded` 那条——**与 `finish` 之后停在 `finished` 完全同一口径**（`tests/timer_commands.rs:778` 的用例就是钉这个：`out.snapshot.state == Some(SessionState::Finished)`）；`live` 只表示"本 run 最后装载过哪条会话"，不是"正在计时"。**P8 登记界面表现**：作废后计时区显示"无活动会话/已作废"，不得按 running 计暂计（fix round 2：因 R8）。
- [ ] **`discard_session` 不走 `end_session_in_tx`**：那条原语按 `run_id` 拒跨 run（`primitives.rs:75`），而"作废整次"必须能作用于**旧 run 留下的会话**（那正是恢复材料）。它只做 S8 的区间作废 + 会话状态更新，因而不受 `StaleRunContext` 限制；反过来，这也意味着它**不**参与"以可信方式闭合"，不会把停机时间算成工时。
- [ ] 两者的区别要在错误与审计上可分辨：`discard_session` 不能只作废一个区间；`reconcile(discard_uncertain)` 不能作废整次。任何"含糊共用一个丢弃按钮"的实现都要在评审里被打回（02 §3/§4 原文）。
- [ ] 提交后：`discard_session` 调 `rescan_recovery()`（S1）+ `refresh_committed_session` 刷新镜像（新增-2）；`backfill` **不做**任何镜像刷新——它新建的是一条 `finished` 行，不改变当前镜像（`live` 仍指向 `running_foreground` 那条，或本来就是 `None`）。
- [ ] **测试（`tests/backfill_discard.rs`）**：补录不产生 `task_change` 行、不占前台（补录期间另一次 `start` 仍能成功）、`work_session.state = 'finished'` 且 `sampled_end_wall_at IS NULL`；补录区间重叠被拒、`ended_at > now` 被拒；作废整次后该会话**全部**区间 `voided_at` 非空且 `needs_review` 清零、会话 `discarded` 且 `needs_review = false`；作废不改任务状态（逐字段比任务行）；两者都写审计；失败整体回滚不留半个会话；作废后 `AppState::recovery().requires_recovery()` 变化（若它是最后一条待处理事实）；**作废运行中的会话后镜像停在 `discarded`**（`coordinator.live().unwrap().state == Some(SessionState::Discarded)`，与 `tests/timer_commands.rs:778` 的 `finish` 口径同形，R8）。

## Task 5：区间规则与重叠校验，端到端集成

**文件：** `src-tauri/src/services/daily_plan.rs`（S10）、`src-tauri/src/services/history.rs`（规则复用的落点与注释）、`src-tauri/tests/local_day_bounds.rs`（新建）、`src-tauri/tests/recovery_end_to_end.rs`（新建）。

**输入 / 输出（新增的两个公开函数就是本任务的交付物，签名见 §0.3 S10）：**
```rust
pub fn local_day_bounds(timezone: &str, date: LocalDate) -> Result<IntervalRange, AppError>;
pub fn local_days_covering(timezone: &str, from: i64, to: i64) -> Result<Vec<(LocalDate, IntervalRange)>, AppError>;
```
**依赖的真实符号：** `IntervalRange::{overlap_ms:35, overlaps:41, clipped_ms:46}` / `IntervalFacts:56` / `IntervalSet:109`（`insert:119`）、`ck_interval_voided:126`、`IntervalFacts::validate:83`、`daily_plan::{normalize_timezone:52, local_date_at:80, timezone_of:94}`、`LocalDate::{year,month,day}` 与 `LocalDate::from_jiff:82`、`coordinator.rs:857`（resume 切 run_id）。实现用的 jiff API 逐条核过（见 §0.3 S10）。

- [ ] **相交判定：复用，不新写**（I3）。P3 与 P5 都用 `domain::interval` 的既有三件：`IntervalRange::{overlap_ms:35, overlaps:41, clipped_ms:46}`、`IntervalFacts:56`、`IntervalSet:109`（`insert:119` 已经做"与既有区间重叠则拒绝、端点相接允许"）。本任务**不新增**任何相交实现；`services/history.rs` 只在模块文档里点名这三件是唯一入口。
- [ ] **真实日界：这才是 P3 要写的**（S10，落 `services/daily_plan.rs`）：`local_day_bounds(timezone, date)` 与 `local_days_covering(timezone, from, to)`；签名与实现方向见 0.3。**不假设每天 24 小时**：`end` 必须由"次日零点"换算得到（`Date::tomorrow` → `.at(0,0,0,0).to_zoned`），跨日**不整段丢**。P5 只做统计口径，直接用这两个函数 + `clipped_ms`，不重写。
- [ ] 修正/确认路径产出的区间集合必须满足：两两不重叠、非负、`voided_at` 与 `needs_review` 互斥语义清晰（`ck_interval_voided:126` 已兜底；`IntervalFacts::validate:83` 是服务侧的自检入口，待确认的不能同时是已作废的）。
- [ ] **端到端（`tests/recovery_end_to_end.rs`，真临时文件库 + `startup()`）**：跑 `running → 崩溃（模拟：另起 run 的库行 + 检查点）→ 重启扫描 → recovering → reconcile(confirm) → finished`，再跑 `finished → correct → backfill → discard_session`，断言：统计可读的区间集合始终非重叠；`revision` 每次用户命令恰好 +1（扫描事务另计一次）；**门禁在 reconcile 后放开且 `start` 成功**；恢复后 `resume` 把 `run_id` 切到当前 run（`coordinator.rs:857`）；**同一段时间不会被算两遍**（可信前缀计入且只计一次：`duration_ms = t - started_at`；待确认段 `duration_ms IS NULL` ⇒ 不计入任何"已确认"数字）。
- [ ] **测试（`tests/local_day_bounds.rs`）**：日界在跨月、跨年、以及一天不是 24 小时（夏令时切换，例如 `America/Santiago`/`Australia/Lord_Howe` 这类可用 IANA 名的时区）下都正确；`local_days_covering` 覆盖的天数与逐日拼接回原范围（半开、无重叠、无缺口）；端点相接不算重叠；待确认区间不影响既有闭合统计。
- [ ] **完成门槛**：在 `src-tauri`（Windows 侧，先经 `worktrace-apply-src.ps1` 同步镜像）运行 `cargo fmt --check`、`cargo test --offline`、`cargo clippy --all-targets -- -D warnings`，并执行分层检查 `powershell -ExecutionPolicy Bypass -File src-tauri/scripts/check-layers.ps1`（`rg` 缺失时脚本自己回退 `Select-String`，见 `scripts/check-layers.ps1:74-82`；期望输出 `LAYER CHECK PASSED`、退出码 0）。P2 与 P4 的测试无回归（含 `tests/timer_seams.rs`、`tests/startup_order.rs`）；`git diff --stat worktrace-web/src/types/__snapshots__` 为空。

---

## Task 6：任务状态编排与会话联动

**文件：** `src-tauri/src/services/tasks.rs`（新建）、`src-tauri/src/services/timer/coordinator.rs`（S3）、`src-tauri/src/services/bootstrap.rs`（S2 包装）、`src-tauri/src/services/mod.rs`、`src-tauri/tests/task_session_atomicity.rs`（新建）。

**输入 / 输出：**
```rust
pub struct TransitionTaskRequest { pub task_id: String, pub target: TaskStatus, pub cause: TransitionCause }
pub struct TaskTransitionReport {
    pub task: TaskRow,
    pub ended_sessions: Vec<String>,    // 完成/取消联动结束的（running 闭合；paused 直接 finished）
    pub paused_sessions: Vec<String>,   // Blocked/Waiting 联动暂停的
    pub revision: i64,                  // ← 与 P4 的 TaskChange 同口径：**写事务里**由 settle 读回
    pub data_epoch: String,             // ← 同一写事务读回（fix round 2：因 R10，原先漏了这两个）
}
pub fn transition_task(db: &mut Db, coordinator: &mut Coordinator, env: WriteEnvelope, req: TransitionTaskRequest)
    -> Result<WriteOutcome<TaskTransitionReport>, AppError>;
```
**依赖的真实符号：** S3、`task_repo::{get_task:125, transition_task:292, has_running_session:652}`、`domain::task::{TaskStatus, TaskTransition::new:115, TransitionCause:95}`、`session_repo::{intervals_of_session:105, unfinished_sessions_of_run:501, require_no_running_foreground:434}`、`primitives::{end_session_in_tx:64, EndSessionFacts:27}`、`Coordinator::rebuild_from_committed:615`。

- [ ] **服务层入口名（I5）**：`services::tasks::transition_task`（对外就是 02 §3 的 `transition_task`；`AppState::transition_task` 是命令入口）。底层仍是 `storage::task_repo::transition_task`（`:292`，签名与语义**不改**）。**写代码必须带模块路径**；既有三处调用点（`catalog.rs:608`、`coordinator.rs:552/563/834`）**保持不动**（它们已在服务层/协调器自己的事务里，改走新入口＝事务套事务 + 二次取样）。`TransitionCause` **不扩**（I5）。
- [ ] **采样与检测走 S3**：请求校验（epoch + `task.row_version` + 目标状态在跃迁表内）→ `boundary_facts(db)`（取一次样本、观察一次；异常则先提交独立系统事务，原命令返回 `RECOVERY_REQUIRED`，**不执行原意图、不再加 revision**）→ 用返回的 `attributed_at`/`wall_ms`/`run_id` 做同事务的结束动作。**不得**自己取时间、不得自己算 `A(M)`。整个调用发生在 `AppState::transition_task` 内、即 `AppGuard` 内（S2）——这就是"同一串行边界"的保证。
- [ ] **幂等重复**（04 §9 用例 3）：`task.status == target` 且该任务**没有** running/paused 会话 ⇒ `WriteOutcome::Unchanged`（不写 `task_change`、不加版本）；否则按跃迁表判定（`TaskTransition::new`：`Scheduled` 在 V0.1 拒绝、终结态 → Ready 必须 `Reopen`）。
- [ ] 完成/取消前检查该任务**所有**关联会话：存在 `recovering`（`intervals_of_session` 里还有未作废的 `needs_review`）或第 1 类损坏 ⇒ **整体拒绝**（`RECOVERY_REQUIRED`），不部分执行；否则同一事务里复用 **`end_session_in_tx`** 结束全部 `running`/`paused`（`EndSessionFacts` 必须带**当前 run**——`primitives.rs:75` 的跨 run 判据不能放宽；启动扫描已把 paused 重绑当前 run，见 Task 1 第 4 类），随后 `task_repo::transition_task` 写状态与 `task_change`，`settle` 让 `revision` 恰好 +1；各被修改 session 的 `row_version` 由 `update_session_state` 同步 +1。
- [ ] 设为 `Blocked`/`Waiting` 时，同事务**暂停**该任务的 running 会话（`end_session_in_tx` + `target_state = Paused`）并更新任务；正常暂停不自动改变 `Doing`（02 §5：`Doing` 表示任务尚在处理，不等同 running session）。是否能进入目标状态仍以 02 §5 跃迁表为准。
- [ ] `reopen` 显式回 `Ready`、清当前质量并保留历史（`TaskTransition::clears_quality:141` + `task_repo` 既有规则），**不自动恢复旧会话**；单独 `finish` 会话不自动完成任务（P2 既有语义）。
- [ ] **版本口径（fix round 2：因 R10）**：`report.revision` / `report.data_epoch` **只来自这次写事务**——`settle`（`tx.rs:40`）在 `Changed` 时 bump 一次并同事务读回 `require_meta`（与 P4 的 `TaskChange` 逐字同一条路，`catalog.rs:223`）。**不得**在这个路径上调 `Coordinator::snapshot(db)`（`:342-348`）：它自己取样本、并在判为异常时**再提交一笔独立系统恢复事务**，于是"写事务的 revision"与"快照的 revision"可能差 1——DTO 里的版本就再也说不清是哪一次写的。P8 要计时快照就单独发 `timer_snapshot`（P7 的既有协议：广播 `domain.changed` 后前端重拉）。
- [ ] **先生成待应用的内存变化，事务提交后一次应用**（P2 的"提交后收尾"约定）：事务失败字段级回滚且内存不变；提交后收尾失败按 P2 规则处理（**不重复提交**）——**只有当本次确实结束/暂停了会话**（`ended_sessions`/`paused_sessions` 非空）时才调 `coordinator.rebuild_from_committed(db.connection(), &last_touched_session, facts.sample)`（`:615`，与 P2 的 `finish` 完全同一条收尾路径，含"库里还有别的 `running_foreground` 就改载它"的规则 `:640-648`）；它的返回值**只用于重建镜像**，不进 DTO 的版本字段。**没有会话被结束时不动镜像**（任务行变了不影响 `live`：`build` 每次都重读任务行拿 `row_version`/`title`，`coordinator.rs:420-424`），因此这一支既不该有第二次取样、也不该有第二次事务。收尾失败一律映射 `RECOVERY_REQUIRED`（那笔已提交的写因此不发 `domain.changed`，见下一条）。
- [ ] **广播口径（顺带-2）**：本服务的命令层（P8）沿用 P4/P7 的 `announce`（只在 `Changed` 时广播、提交后、放锁前）。**登记已知例外**：像 P2 计时族那样，"提交成功但提交后重建失败"的那一笔**不发 `domain.changed`**（P7 验收记录 §6.5 第 23 条），P3 **不新增第二条广播路径**、也不在服务层直接 `emit`；收敛靠 30 秒 `get_revision`，故障态解锁归 P6。
- [ ] **测试（`tests/task_session_atomicity.rs`）**：完成/取消多会话仅一次 `revision`（含各自的 `row_version`）；`recovering` 整体拒绝且逐字段零变化；`Blocked`/`Waiting` 暂停联动；暂停后任务仍 `Doing`；显式 `reopen` 清质量且不自动恢复会话；幂等重复零变化；末步骤失败全部回滚；任务状态变更与 tick/历史修正并发（同一 `AppGuard` 串行，断言不会交错出半个状态）；异常检测触发时原意图不执行（`RECOVERY_REQUIRED`）且系统事务只提交一次；**版本口径（R10）**：`report.revision` 等于提交后 `meta.revision`，且"没有会话被结束"的那次调用**不产生第二次取样/第二笔事务**（用 `total_changes()` 探针或 `run_id`/`tick_seq` 不变来断言）。

## 完成门槛与人工验收

- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。与本计划直接相关的至少包括：02 §8 的「历史工时重叠」「区间修正后报表重算」「待确认排除与显式确认」「强杀后十秒内重启」「运行/暂停时退出」「暂停后重启」「暂停直接结束」；04 §9 的「重复提交」「旧版本修改」「统计修正」。
- [ ] `cargo fmt --check`、`cargo test --offline`、`cargo clippy --all-targets -- -D warnings` 全绿；`check-layers.ps1` 六条 PASSED；P1/P2/P4/P7 测试无回归；15 份快照 fixture 零改动。
- [ ] 所有用户命令都带 `expected_data_epoch`；修改既有对象带其 `expected_row_version`（会话版本走 `WriteEnvelope`）；一次用户命令恰好加一次 `revision`（按总纲 §9，检测异常的独立系统事务与**整批一次的扫描事务**另计）。
- [ ] 被拒命令按断言口径第 ④ 项**逐字段**验证，不只比行数。
- [ ] **故障路径闭环（R3）**：手工把协调器推成 `faulted`（例如让异常事务在 `busy_timeout` 内失败）后，`AppState::retry_recovery`（S12）能成功提交并清故障态、且**紧接着重算了门禁**；`MonotonicBackwards` 那条**仍然拒绝**（需新 run）。这条用例是"故障态下计时仍可用"的唯一证据。
- [ ] **接口归属已登记（R2）**：`attention_overview` 与 `retry_recovery` 的 IPC 命令都点名归 P8（P3 只交付服务入口/DTO 形状；P8 的 8 条命令登记由控制器统一落盘），且 §0.5 的四个 DTO 形状与实现逐字段一致（P3 测试钉住）。
- [ ] **登记（不在 P3 修）**：计时族与 P3 的会话联动在"提交成功但提交后重建失败"时都不发 `domain.changed`（P7 验收记录 §6.5 第 23 条，归 P6 的故障路径）；`services/tx.rs:9-10` 的使用者清单与 `domain/error.rs:37` 的"同会话"注释在实现时一并更新（顺带-3/顺带-4）。
- [ ] **人工验收（不能用单元测试代替，08 §6）**：P3 交付时能做的那半——**用真实文件库**造四类判定各一条记录（第 1 类用 SQL 直写一条不自洽的行），跑 `startup` 后核对：`AppState::recovery()` 的命中的确来自这四类；`attention_overview` 的列表/数量与四类一一对应；确认一条不确定区间、丢弃一条、作废整次会话各一次，核对 `time_edit` 与统计可读区间集合；**最后确认 `start` 能成功**（闭环）。界面与实机步骤（强杀 → 10 秒内重启 → 界面核对）归 **P8**（顺带-1），记录机器/系统版本与观察结果。

## 下游接口（供 P5 / P6 / P8 消费）

（2026-10-04 修订：因 I6 —— 原清单缺第 1、2 条，并把第 3 条归错了人。）

1. **`accept_clock_correction`：P2 已有内存半部，P3 补事务外壳。** `Coordinator::accept_clock_correction(sample)`（`coordinator.rs:245`）已交付（清 `unaccepted_clock_correction`、前移 `lifetime_ref`，**不写审计、不加 revision**，P2 内部已在 `:1149`/`:1239` 调用）。P3 补的是 **S4 `Coordinator::accept_detected_clock_correction`**（`guard_epoch` + `time_edit` + 恰好一次 `revision` + 提交后调用上面那个原语）与 `AppState::accept_detected_clock_correction`。**不要**再实现第二个 `accept_clock_correction`。
2. **全局待确认列表/数量：P3 补。** 服务入口 `services::recovery::attention_overview(db, expected_data_epoch) -> AttentionOverview`（只读、同一读事务信封，形状抄 `catalog::list_projects:285-307`）；**DTO 字段形状钉在 §0.5**（`AttentionOverview` / `SessionAttentionItem` / `PendingIntervalItem`——fix round 2：因 R1，原先这三个类型只有名字）。语义：**含终点未知的片段**（`needs_review=1` 的区间一律列出，`ended_at` 只是候选端点，`duration_ms IS NULL` 表示"未确认"），与当前计时会话分离（P8 用它，不要拿 `TimerSnapshot.pending_ms`/`needs_attention()` 顶替——P7 计划的「评审补充：恢复提示与时钟校正」第 1 条原文；原记 `:245`）。P5 想排除"可疑区间"时读它的 `fault_sessions`/`attention`，**不要自己写第二份损坏判定**。**IPC 命令由 P8 新增**（P8 计划已登记：其「P8 新增的 IPC 命令」清单第 8 条就是本条，请求带 `expected_data_epoch`、响应 `AttentionOverview`；P3 侧共 8 条 + 导出 1 条。P3 只交付服务入口与 DTO 形状、不新增 `#[tauri::command]`——fix round 2：因 R2）。
3. **`retry_recovery`：实现是 P2 的，生产出口是 P3 的（S12），触发是 P8 的，采集/看门狗是 P6 的。**（fix round 2：因 R3 —— 终审实测 `faulted`（`coordinator.rs:170/322`）除 `retry_recovery:1265` 外**没有生产出口**，而 P6/P8/总纲对它 **0 命中**，故障路径上"计时可用"不闭环。）
   - **P2 已交付**：`Coordinator::retry_recovery(db) -> Result<TimerSnapshot, AppError>`（`:1265`，重试那笔失败的恢复事务，成功才清 `faulted`；硬故障 `MonotonicBackwards` 一律 `RECOVERY_REQUIRED`，`:1270-1272`）。**不重写它**。
   - **P3 补 S12**：`AppState::retry_recovery(expected_data_epoch)`（`services/bootstrap.rs`）——它是 `faulted` 的**唯一生产出口**，因为 `AppState.coordinator` 私有（`bootstrap.rs:344`），服务层与命令层都够不着。包装里 **先** `coordinator.retry_recovery(db)`、**再** `rescan_recovery()`（S1），返回提交后的快照。
   - **触发时机：用户显式重试**（P8 新增一条 IPC 命令——其「P8 新增的 IPC 命令」第 7 条；请求带 `expected_data_epoch`、响应 `TimerSnapshot`；**不做定时自动重试**，08 §1：故障不能自己把证据擦掉）。S12 的 epoch 预检口径见 §0.3。
   - **P6 的活**：采样线程与启动路径的看门狗/重启（P7 验收 §6.7 第 37 条登记的"采样线程 panic 静默死亡"），以及把 `retry_recovery` 写进它自己的入口清单。**P6 计划侧的登记由控制器统一落盘**（本计划只改自己这一份文件）。
   - **与 `rescan_recovery` 的分工**：见 S12——一个清协调器故障态，一个重算门禁字段，互不替代。
   - P7 计划的「遗留与边界（2026-10-04 Task 1b 登记）」把它写成"P3 的对账入口"是记录漂移（原记 `:826`）（P3 只提供包装与触发口，不提供实现）。
4. `reconcile` / `correct` / `backfill` / `discard_session` 的服务入口与 DTO：实施后登记真实 Rust 签名（本计划 Task 2/3/4 已给形状；`env` 承载 epoch 与会话版本）。
5. `services::tasks::transition_task` 及其 DTO：P8 的任务入口与托盘完成动作共用（`platform/tray.rs:104` 的禁用占位由 P8 启用），**不能直调仓储**（`catalog::clarify_ready` 只覆盖无会话的 Inbox/Clarifying → Ready）。
6. 区间规则（半开相交、真实日界）：**相交复用 `domain::interval`（`interval.rs:35/41/46/109`）**；**日界用 `daily_plan::{local_day_bounds, local_days_covering}`（S10）**。P5 直接调用，不复制（P5 计划的「依赖的前置计划」里的 P3 条 + Task 1「统计口径——范围裁剪、排除与人工/机器分离」第一条；原记 `:19`/`:37`）。
7. 恢复扫描入口：P7 已建立启动调用点（`bootstrap.rs:699`），P3 把第 ④ 步接到 `services::recovery::scan_at_startup`，随后仍用 `scan_recovery` 算门禁；P6 在恢复/替换库路径复用**同一入口**（P6 计划的 Task 1「P7 已建立这套顺序并有测试」条 + 启动顺序第 ④ 步 + Task 4「恢复的执行骨架与顺序」；原记 `:65`/`:71`/`:178`）；`AppState::rescan_recovery`（S1）是门禁重算的唯一入口。恢复确认界面与最终实机验收归 P8。

## 评审补充：恢复提示与时钟校正

- [ ] 提供全局待确认会话列表/数量（含终点未知的片段），与当前计时会话分离；开始别的任务后旧记录仍可发现——落点是「下游接口」第 2 条的 `attention_overview`（**新补**，原计划只有这句要求、没有落点）。
- [ ] 显式接受时钟校正（S4）：先原子写审计并增加 revision，提交后调 `Coordinator::accept_clock_correction`（`:245`）更新映射/清除 run 内未接受标记；**不自动确认旧可疑工时**；失败保留标记（不提交、不清标记）；重启按安全启动恢复规则处理（新 run 里该标记本就不存在，08 §1）。
- [ ] 结束原语 `end_session_in_tx` 要求 `EndSessionFacts.run_id`（`primitives.rs:33/75`），跨 run 直接 `StaleRunContext`：本计划的会话联动必须传**当前 run**；跨 run 会话先由启动扫描归一（Task 1 第 4 类把 `paused` 的 `run_id` 更新到当前 run；第 2 类转 `recovering`，走 `reconcile` 而不是联动）再走联动，不能靠放宽该判据绕过。
- [ ] **提交后镜像刷新（新增-2）**：`reconcile`/`discard_session`/`transition_task` 改到协调器正镜像的会话时，提交后必须 `Coordinator::refresh_committed_session`（失败时隔离旧镜像，显式重试只重载已提交事实）或 `rebuild_from_committed`（`:615`）刷新，失败映射 `RECOVERY_REQUIRED`；**不得**留一个继续按旧状态出快照的 `live`（采样线程每拍都出快照，`bootstrap.rs:749`）。

## 当前兼容状态

跨阶段接口、错误载荷、启动归属及 P7 前待办统一见[总纲 §10](2026-10-03-v01-plan-index.md)。P1/P2/P4/P7 核心已验收，P3 尚未实施；历史签名、测试数量和开工记录保留为当时证据，消费接口以当前源码及总纲为准。文档对齐不表示待办代码、IPC 或平台验证已经完成。

**2026-10-04 修订后的自检口径**：本计划里的每个 `file:line` 都按当时 HEAD（`18861e3`，`origin/dev == HEAD`）的本地镜像 `worktrace-src/` 核过；`worktrace-src/` 是仓库 `src-tauri/` 的工作副本，行号漂移时以符号名为准。IPC 与界面仍归 P8（顺带-1）。

## 2026-10-04 本轮复审补正

1. S8/Task 2/Task 4 作废规则与 P1 ck_interval_duration 对齐：无时长候选清 ended_at，有时长区间保留端点；原候选端点由 time_edit 保留，不补造时长。增加启动扫描后 discard_uncertain/discard_session 的端到端用例，覆盖零长度候选与非零候选、开放未知段及已有可信前缀，确保作废全部成功、前缀保留/整次作废边界正确。
2. S12 包装签名显式接收 expected_data_epoch，使正文规定的请求身份预检可实施；P2 的 Coordinator::retry_recovery 签名不变。P8 请求字段映射至该参数。
3. 02 §4 恢复表的单行标记表述已与可信前缀/待确认余段模型对齐。P3 服务仍未实现，此轮只修订契约并验证既有 schema，不虚报功能交付。

## 执行前与异常交接门禁（2026-10-04）

- [ ] 新增 Domain 文案构造点逐条复核，留意既有扫描器文件级/400 字符启发式的误报；Storage 诊断不要求中文，不复用任务跃迁错误描述区间。
- [ ] 开工前读取 [pre-p3-closure](../../validation/pre-p3-closure.md)，运行 check-pre-p3.ps1，全部自动化检查通过；真实平台验收保持未完成。
- [ ] Task 1 明确 SessionAttention 与新恢复 DTO 的消费者，禁止两份生产损坏判定；复核 useRunningTaskId 的现有前台/mode 语义，不提前扩展 V0.2。
- [ ] Task 7 按交接页“P3 必须验证的异常闭环”逐行登记测试名/结果，包含 S1 扫描失败标记、S12 二次重扫、提交后失败不重复用户写入、未知候选作废及完整审计。
- [ ] 交付给 P5/P6/P8 的恢复/故障状态及 DTO 与本计划一致；后续阶段归属不代替本阶段测试，也不把后续界面未实现写成服务不可实施。

---

## P3 实施记录（2026-10-05，控制器落盘）

> 本节在 P3 实施完成后写入，登记**实际交付的签名与口径**，供 P5/P6/P8 与终审消费。
> 实施期的逐条裁决与证据在 SDD 台账 `.superpowers/sdd/2026-10-03-p3-recovery-and-history/progress.md`（工作区产物，不入仓库）。
> 代码范围 `0296827..63eab6c`（11 提交、18 文件、+13218/−42）：`cargo test --offline` **588 passed / 0 failed**；
> `cargo fmt --check` / `clippy --all-targets -D warnings` / `check-layers.ps1` 六条全绿；15 份快照 fixture 与 `src/commands/` 零 diff；无新错误码、无新锁、无新 IPC。

### 实际交付签名（与 §0.3/§0.5 的差异已在下面逐条说明）

```rust
// services/recovery.rs
pub fn scan_at_startup(db: &mut Db, current_run_id: &str, now: i64) -> Result<StartupScanReport, AppError>;
pub fn reconcile(db: &mut Db, env: WriteEnvelope, req: ReconcileRequest, now: i64, current_run_id: &str)
    -> Result<WriteOutcome<ReconcileReport>, AppError>;
pub fn discard_session(db: &mut Db, env: WriteEnvelope, req: DiscardSessionRequest, now: i64)
    -> Result<WriteOutcome<HistoryEditReport>, AppError>;
pub fn attention_overview(db: &Db, expected_data_epoch: &str, current_run_id: &str) -> Result<AttentionOverview, AppError>;

// services/history.rs
pub fn correct(db: &mut Db, env: WriteEnvelope, req: CorrectRequest, now: i64)
    -> Result<WriteOutcome<HistoryEditReport>, AppError>;
pub fn backfill(db: &mut Db, env: WriteEnvelope, req: BackfillRequest, now: i64, run_id: &str)
    -> Result<WriteOutcome<HistoryEditReport>, AppError>;

// services/tasks.rs
pub fn transition_task(db: &mut Db, coordinator: &mut Coordinator, env: WriteEnvelope, req: TransitionTaskRequest)
    -> Result<WriteOutcome<TaskTransitionReport>, AppError>;

// services/daily_plan.rs（S10 真实日界；相交判定仍只来自 domain::interval）
pub fn local_day_bounds(timezone: &str, date: LocalDate) -> Result<IntervalRange, AppError>;
pub fn local_days_covering(timezone: &str, from: i64, to: i64) -> Result<Vec<(LocalDate, IntervalRange)>, AppError>;

// services/timer/coordinator.rs（S3 取样接缝 / S4 时钟校正的显式接受）
pub fn boundary_facts(&mut self, db: &mut Db) -> Result<BoundaryFacts, AppError>;
pub fn accept_detected_clock_correction(&mut self, db: &mut Db, req: AcceptClockCorrectionRequest)
    -> Result<ClockCorrectionAccepted, AppError>;

// services/bootstrap.rs（S1 门禁重扫 / S12 故障态出口；另有 7 个命令瘦包装）
pub fn rescan_recovery(&mut self) -> Result<RecoveryScan, AppError>;
pub fn retry_recovery(&mut self, expected_data_epoch: &str) -> Result<TimerSnapshot, AppError>;
```

### 与计划文本的差异（控制器逐条裁决；每条都记了代价）

1. **`reconcile` 与 `attention_overview` 各多一个 `current_run_id`** —— 服务层够不着协调器（§0.2 私有），而正文要求「`run_id` → 当前 run」与 `is_current_run`。P8 的 IPC 包装从 `state.coordinator().run_id()` 取。
2. **`reconcile` 的 `Unchanged` 不可达** —— 前置只接 `recovering` ⇒ 状态跃迁必然写；重复提交被前置拒绝，**不是**幂等零变化（`correct` 的 `Retime` 才是幂等零变化）。
3. **`discard_session` 幂等** —— 它按设计**无状态前置**，重复作废已 `discarded` 的会话 ⇒ `Unchanged`（零写入/零审计/零版本），且**不移动 `ended_at`**。
4. **零区间会话的 `discard_session` 返回 `DOMAIN_ERROR`**（`HistoryEditReport.interval` 必填，不编造区间）；该形态由 **`reconcile` + 空 `ranges` 的 `Confirm`** 只做状态跃迁解决（**不是死锁**）。
5. **`backfill` 的 `before_json` 是创建型** `{"change":"backfill"}`（同 `create_task` 的 `"{}"`）⇒ P5/P8 按 `before_json.intervals` 重建"改动前事实"时**必须跳过创建型行**。
6. **审计形状**：`candidate_end_source` **只在真有候选端点推导**时写（目前只有启动扫描的 `normalize_crashed_interval`），且写在 `after_json`；`correct` 的用户理由落 `after_json.user_reason`；`voided_at` 键在两种形状里都在。
7. **提交后收尾统一为"仅在镜像那条上刷新"**（`reconcile`/`correct`/`discard_session`：只有被改动的会话正是 `live` 镜像的那条才 `refresh_committed_session`）；`transition_task` 用 `rebuild_from_committed` 且**仅当确有会话被结束/暂停**。
8. **S4 的"本来会返回 `Unchanged`"那条路不再丢弃已观察到的判决**：非 `Trusted` 判决交给既有异常处理（系统事务）后返回 `RECOVERY_REQUIRED`——否则睡眠/休眠（`Suspended` 无法被再次检测）会被**静默计成工时**。`flag == true` 的正常接受路径不变。
9. **`paused` + 仍有待确认区间**这一类四类判定盖不住的形态：**原样保持**、归 `NeedsReview`、**零写入零版本**；出口是 `discard_session`（它无状态前置）。
10. **`correct` 只放行 `Finished`**（`discarded` 单独中文拒绝）；它**不做第 1 类判定、不重扫门禁**，因而存在受控的"门禁快照滞后"窗口（方向是**过度拦截**，安全侧）。

### 下游读数陷阱（两条，务必遵守）

- **`task_change` 与 `time_edit` 同一毫秒可落多行** ⇒ `ORDER BY created_at, id` 在它们之间**不确定**，**禁止"取最后一条"**（P5 的报表、P8 的托盘/历史页尤其危险）；按 `reason` 或内容定位。
- **`retry_recovery` 的 `RECOVERY_REQUIRED`可能来自协调器恢复事务、提交后镜像重载或门禁重扫失败，错误码不能区分原因**；需要区分协调器是否仍被隔离时读 `coordinator().is_faulted()`（包含待重载镜像）。镜像重载重试只读取已提交事实，不重发 P3 写操作；**不新增错误码**。

### 验证落点

新增 8 个集成测试文件：`recovery_scan` / `reconcile` / `correct` / `backfill_discard` / `local_day_bounds` / `recovery_end_to_end` / `task_session_atomicity` / `exception_closure`。
`docs/validation/pre-p3-closure.md` 的「P3 必须验证的异常闭环」7 行**逐行**登记在 `tests/exception_closure.rs`（其中 ⑥⑦ 复用 Task 1/Task 6 的用例，并附实跑结果）。
**实机验收（F-009 / F-011 / F-016、真实双窗口 §2.1–§2.6）仍归 P8**，本阶段不冒充。
