# P3 · 恢复确认、历史修正与补录实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把"计时已经跑完之后"的四件事做扎实：崩溃重启后的四类判定与恢复确认（`reconcile`）、对已完成历史的修正（`correct`）、手工补录（`backfill`）、以及作废整次会话（`discard_session`）——全部带 `time_edit` 审计，且不产生重叠或负时长。

**Architecture:** 新增 `services/recovery.rs`（启动扫描与恢复确认）与 `services/history.rs`（修正、补录、作废）。恢复与历史服务**只消费** P2 已提交的事实与 P1 的仓储原语：区间分割用 P2 已经实现的那一套，本计划不另写一份时钟逻辑。服务拥有事务，仓储接受 `&Transaction`；一次用户命令恰好加一次 `revision`。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· 无新依赖

**Spec:**
- `../specs/2026-10-02-worktrace-architecture/02-data-model.zh.md` §3（命令表）、§4（崩溃恢复四类判定）、§6（统计排除口径）、§9（明细历史与恢复实现）、§10（服务契约补充）
- `.../08-implementation-contracts.zh.md` §1（异常分割与跨日日界）、§7（`reconcile` 语义）
- `.../00-architecture.zh.md` §4/§5（错误契约、写事务信封）
- `.../04-functional-spec.zh.md` F-006、F-008、F-009、F-015、F-017

**依赖的前置计划：**
- **P1**（`2026-10-03-worktrace-v01-foundation.md`）：`storage::{meta, guards, task_repo, session_repo, checkpoint_repo}`、`error::AppError`、`domain::{session, interval}`
- **P4**：`domain::localdate::LocalDate` 与 Rust 时区校验能力；本计划的日界裁剪使用相同的时区库与别名策略，不再选第二套。
- **P2**（`2026-10-03-p2-timer-coordinator.md`）：协调器已提交的 `recovering` 事实与区间分割原语（可信前缀 + 待确认余段）、`TimerSnapshot`、`start/pause/resume/finish`

**边界（不要越界）：**
- `start/pause/resume/finish` **归 P2**，本计划只读它们提交的事实，不重复实现。
- `switch` 与打断记录属 **V0.2**（F-106，在 `F-101…F-113` 内），本计划不实现。
- 统计口径（范围裁剪、人工/机器分离）归 **P5**；本计划只保证被它读取的事实不重叠、非负、半开。
- UI 与 IPC 命令接线归 **P7**；本计划交付服务层与测试。

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条（尤其第 ④ 项：被拒命令要逐字段比对，不只比行数）。
**异常事务时序：** 见总纲 §9「异常事务、用户命令拒绝与查询的边界」。

---

## Task 1：启动恢复扫描与共享恢复原语

文件：services/recovery.rs、storage/session_repo.rs（补扫描查询）、tests/recovery_scan.rs。

- [ ] 扫描时机与顺序由 **P6** 调用：单实例检查 → 打开库/迁移 → 新建 `application_run` → **扫描旧 run 未结束会话** → 协调器 → 窗口。本任务只提供扫描入口，不接管启动顺序。
- [ ] 按 02 §4 的四类判定分流，**判定顺序不可颠倒**（先看不变量是否可信，再看有无开放/待确认区间）：
  1. 状态/区间不变量损坏 → 隔离该记录 + 诊断，**禁止自动修复事实**；可疑区间暂不计入任何统计。
  2. `running` 且有开放区间 → session 设 `recovering`，**只把该开放区间标 `needs_review=1`**，既有闭合区间不动。
  3. `recovering` → 保持待确认，不增加已知工时。
  4. `paused` 且无开放/待确认区间 → 保持 `paused`，只把 `run_id` 更新到当前 run，**不自动继续计时**。
- [ ] 两类事实要分开记录：**不变量损坏**（需诊断、禁止推断）与**普通待确认**（用户确认即可）。前者不能用 `reconcile` 混过去。
- [ ] 扫描本身**不加 `revision`**（它不是业务变化）；只有实际发生状态跃迁（第 2 类）才按系统状态事务记一次，并按总纲 §9 提交。第 3、4 类若无状态变化则只更新 `run_id`，是否计入 revision 由 P1 的 `Meta` 规则决定——**不得为了"有个事务"而凭空加 revision**。
- [ ] 复用 P2 的区间分割原语；本计划**不另写**"找一个可信前缀"的逻辑。
- [ ] 测试：四类各一例（含 `paused` 保持不自动继续、`recovering` 不重复计入）；损坏记录被隔离且不写事实；扫描幂等（跑两次不产生两倍状态）；扫描不加多余 revision。

## Task 2：`reconcile`——确认或丢弃不确定区间

文件：services/recovery.rs、tests/reconcile.rs。

接口：`reconcile(request)`。`request` 含 `expected_data_epoch`、`session_id`、`session.expected_row_version`、`action`（`confirm` / `discard_uncertain`）、`target_state`（`paused` / `finished`），`confirm` 时另带**用户确认后的归属起止**。

- [ ] 前置：仅 `recovering`。非 `recovering` 返回明确错误码（不要压成 `DOMAIN_ERROR`）；`running`/`paused` 想改可信历史必须先 `finish`，再走 `correct`。
- [ ] **一次事务处理该会话的全部待确认区间**，不做"一次一条"的多次往返——否则用户确认到一半崩溃会留下半确认状态。
- [ ] 用户给出的起止必须**合法且不与既有人工时间重叠**（半开区间：端点相接不算重叠）。已知单调时长**只作候选**，用户不接受时不强迫。
- [ ] `discard_uncertain` 只把目标区间置 `voided_at` 并清 `needs_review`，**保留此前有效闭合区间**；它**不能**用来作废整次会话（那是 `discard_session`）。
- [ ] 写 `time_edit`（`before_json`/`after_json` 记清每个被处理区间的前后值）；更新 `run_id` 到当前 run，原始恢复归属保留在 `time_edit` 里（02 §10）。
- [ ] `target_state=finished` 时设 `session.finished`/`ended_at`；`=paused` 时**不自动计时**。V0.1 无番茄钟阶段，`phase_state` 相关分支不写。
- [ ] 测试：全部待确认区间一次处理完；确认的起止重叠被拒（含与"后来已记录的人工时间"重叠）；丢弃单个区间不动作废整次；非 `recovering` 被拒；`time_edit` 前后值完整；失败整体回滚。

## Task 3：`correct`——仅 `finished` 的可信历史修正

文件：services/history.rs、tests/correct.rs。

接口：`correct(request)`。`request` 含 `expected_data_epoch`、`session_id`、`interval_id`、`session.expected_row_version`、新的起止或删除意图、`reason`。

- [ ] 前置：会话必须是 `finished`。`recovering` 返回 `RECOVERY_REQUIRED` 并把用户指向 `reconcile`；`running`/`paused` 返回明确错误（先 `finish`）。
- [ ] 修正后的区间必须满足：非负、`ended_at >= started_at`、**与同会话其他有效区间不重叠**（半开区间）。负区间与人工重叠一律拒绝。
- [ ] 历史修正用所属 session.row_version 做并发保护，不新增 interval.row_version。所有改变该 session 区间事实的命令在同事务增加 session.row_version；修改不同区间的旧版本请求也拒绝，刷新后重新确认。
- [ ] 重算该区间的 `duration_ms`（用户确认的起止之差），**保留修正前后的值到 `time_edit`**；不得留下"改了起止但 `duration_ms` 没跟着变"的行。
- [ ] 已完成任务的"完成时刻"**不因修正区间而移动**：报告按 `task_change` 里完成事件的时刻选完成项，不用 `updated_at`、也不用 session 结束时间代替（02 §10）。本任务不得新增或改写完成事件。
- [ ] 删除误记 = 置 `voided_at`（保留审计），不是 `DELETE`。
- [ ] 测试：负区间被拒；与同会话其他区间重叠被拒（含端点相接**不算**重叠的边界）；删除是软删除且审计留存；`recovering` 被指向 `reconcile`；修正后 `duration_ms` 与起止一致；被拒时逐字段比对（状态、版本、起止、时长、`needs_review`、`voided_at`）。

## Task 4：`backfill` 与 `discard_session`

文件：services/history.rs、tests/backfill_discard.rs。

接口：`backfill(request)`、`discard_session(request)`；两者都带 `expected_data_epoch`。

- [ ] `backfill`：校验任务存在、范围合法、与既有人工时间不重叠；创建 `finished` session 与**可信闭合**区间（`needs_review=0`、`duration_ms` 与起止一致），写 `time_edit`。**不启动计时、不占前台槽位、不伪造 `task_change` 完成事件**（02 §3 原文）。
- [ ] `backfill` 的区间必须能通过 P1 的 `ck_interval_duration`；为此 `ended_at` 与 `duration_ms` 由服务算好再交给仓储，仓储不推算。
- [ ] `discard_session`：关闭运行区间（若有），**全部区间置 `voided_at` 并清 `needs_review`**，session 设 `discarded`/`ended_at`，写 `time_edit`。**不删除审计、不隐式改变任务状态**（02 §3 原文）。
- [ ] 两者的区别要在错误与审计上可分辨：`discard_session` 不能只作废一个区间；`reconcile(discard_uncertain)` 不能作废整次。任何"含糊共用一个丢弃按钮"的实现都要在评审里被打回。
- [ ] `backfill` 创建的 session 不计入"运行中"；`uq_running_foreground` 不受影响。
- [ ] 测试：补录不产生 `task_change` 完成事件、不占前台；补录区间重叠被拒；作废整次后全部区间 `voided_at` 非空且 `needs_review` 清零；作废不改任务状态；两者都写审计；失败整体回滚不留半个会话。

## Task 5：区间分割与重叠校验，端到端集成

文件：services/history.rs（分割与校验的公共服务）、tests/interval_rules.rs、tests/recovery_end_to_end.rs。

- [ ] 提供**一处**区间规则实现供 P3 内部与 P5 消费：半开区间 `[start, end)` 相交判定、按查询时区的**真实日界**裁剪（08 §1：不假设每天 24 小时；跨日不整段丢）。P5 只做统计口径，不重写这套规则。
- [ ] 修正/确认路径产出的区间集合必须满足：两两不重叠、非负、`voided_at` 与 `needs_review` 互斥语义清晰（待确认的不能同时是已作废的）。
- [ ] 端到端：用真实临时文件库跑一遍 `running → 崩溃 → 重启扫描 → recovering → reconcile(confirm) → finished`，再跑一遍 `finished → correct → backfill → discard_session`，断言：统计可读的区间集合始终非重叠；`revision` 每次用户命令恰好 +1；恢复后 `resume` 把 `run_id` 切到当前 run。
- [ ] 测试：日界裁剪在跨月、跨年、以及一天不是 24 小时（夏令时切换）的时区下都正确；端点相接不算重叠；待确认区间不影响既有闭合统计。
- [ ] 完成门槛：在 `src-tauri` 运行 `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets`，并执行 P1 的分层检查脚本（含 `Select-String` 回退路径）。P2 与 P4 的测试无回归。

---

## Task 6：任务状态编排与会话联动

文件：services/tasks.rs、services/mod.rs、tests/task_session_atomicity.rs。

- [ ] 提供 transition_task(request) 服务；输入含 expected_data_epoch、task_id、task.expected_row_version、目标状态与 cause。P1 task_repo 仅为事务原语，P7/托盘只调用此服务，不拆成任务和会话多个命令。
- [ ] 在同一串行协调器边界采样；按总纲 §9 先验证请求，再检测异常。若独立异常事务已提交，原用户任务命令返回 RECOVERY_REQUIRED，不执行原意图。
- [ ] 完成/取消任务前检查所有关联会话：存在 recovering 或隔离故障则整体拒绝；否则同事务复用 P2 结束原语结束全部 running/paused、更新任务状态/质量、写 task_change，并增加一次 revision。各被修改 session 的 row_version 同步增加。
- [ ] 设为 Blocked/Waiting 时，同事务暂停该任务 running session 并更新任务；正常暂停不自动改变 Doing。是否能进入目标状态仍以 02 §5 跃迁表为准。
- [ ] reopen 显式回 Ready、清当前质量并保留历史，不自动恢复旧会话；单独 finish 会话不自动完成任务。
- [ ] 先生成待应用的内存变化，事务提交后一次应用；事务失败字段级回滚且内存不变，提交后应用失败按 P2 故障恢复规则处理，不重复提交。
- [ ] 测试：完成/取消多会话仅一次 revision、recovering 整体拒绝、Blocked/Waiting 暂停联动、暂停仍 Doing、显式 reopen、末步骤失败全部回滚、任务状态变更与 tick/历史修正并发。

## 完成门槛与人工验收


- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [ ] `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets` 全绿；P1 分层检查通过；P2/P4 测试无回归。
- [ ] 所有用户命令都带 `expected_data_epoch`；修改既有对象带其 `expected_row_version`；一次用户命令恰好加一次 `revision`（按总纲 §9，检测异常的独立系统事务另计）。
- [ ] 被拒命令按断言口径第 ④ 项**逐字段**验证，不只比行数。
- [ ] **人工验收（不能用单元测试代替，08 §6）**：强杀应用 → 10 秒内重启 → 四类判定各造一条真实记录并核对界面；确认一条不确定区间、丢弃一条、作废整次会话各一次，检查审计与统计口径。记录机器/系统版本与观察结果。

## 下游接口（供 P5 / P7 消费）

- `reconcile` / `correct` / `backfill` / `discard_session` 的服务入口与 DTO：实施后登记真实 Rust 签名。
- transition_task 服务及其 DTO：P7 任务入口与托盘完成动作共用，不能直调仓储。
- 区间规则（半开相交、按日界裁剪）：P5 直接调用，不复制。
- 恢复扫描入口：P6 在启动顺序里调用；P7 负责把四类判定的结果呈现给用户。
