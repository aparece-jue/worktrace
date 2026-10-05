# P3 验收记录：恢复确认、历史修正与补录

## 2026-10-05 待确认时长补充复审

进一步检查发现 pending_ms_of 仍普通相减并累加，上一轮已确认工时的保护未覆盖这个独立路径。现候选跨度使用 checked_sub，累计使用 checked_add，超界返回 DOMAIN_ERROR，不把待确认时长截断为虚假的可表示数字。新增 pending_duration_overflow_returns_an_error_without_changing_persisted_facts，验证两段合法候选累加超界时快照返回错误，区间数量与会话版本不变。

本轮针对修改范围复跑：库单元测试 59 passed、timer_snapshot 17 passed（含新增回归），Clippy all-targets / fmt / diff 检查通过。前述完整门禁 598 与前端 142 是上一轮证据，不冒充新增回归后的全套数量。本轮未改前端、IPC 或 schema。pre-p3-closure 已标注为历史开工记录，避免“P3 尚未实现”与当前交付冲突。

仍需后续关闭：P5 统计/导出、P6 平台事件与维护备份恢复/采样失活等故障闭环、P8 恢复界面及真实多窗口/托盘/打包验收。它们是未交付或未验证的范围，不能称为已完成阶段的新接口冲突。

## 2026-10-05 再复审（基线 adcb11c）

上轮修复已纳入 adcb11c，专用 IntervalSpanOverflow 变体仍映射 DOMAIN_ERROR，错误文案调整与 P7 前端契约兼容；P1 schema、P4 写信封和既有 IPC 未变。再次检查恢复重试、条件化镜像刷新、任务跃迁与待办承接，未发现新的阶段职责冲突。

另补一类数值边界：逐段合法不保证累计时长可表示。IntervalSet 插入前检查累计跨度并拒绝失败插入；协调器镜像装载、快照 active_ms 与 stats_sample 的累计改为 checked_add，返回既有 DOMAIN_ERROR，提交后刷新失败仍走 RECOVERY_REQUIRED 隔离。回归覆盖集合累计溢出时内容不变，以及持久化的两段合法区间累加溢出时装载返回错误、提交后刷新保持隔离。未采用饱和累加，因为那会静默少报工时。

平台实机、真实多窗口和 P8 恢复交互仍按原责任阶段承接，本轮不把自动化通过解释为 V0.1 发布验收。

完整门禁：Rust **598 passed / 0 failed / 1 ignored**、前端 **142 passed**，八项检查全部退出 0，证据：`C:\Users\lenovo\AppData\Local\Temp\worktrace-pre-p3-20261005-141806`；此前新增代码/测试的编译失败已修正，该次结果来自修复后的完整重跑。

**订正（2026-10-05 复审）**：上面那次 598 是在 `tests/timer_snapshot.rs` 只有 **16** 条用例时跑的；该文件现为 **17** 条（两条时长累加回归都已在），因此它不是当前状态的最终数字。控制器在**当前 5 文件状态**上复跑完整门禁：Rust **599 passed / 0 failed / 1 ignored**、前端 **142 passed**、八项检查全部退出 0，证据 `C:\Users\lenovo\AppData\Local\Temp\worktrace-pre-p3-20261005-142510`。以这一条为准。

## 2026-10-05 已完成阶段兼容性复审

再次检查 P1/P2/P4/P7 与 P3 的接缝，发现并修复提交后镜像刷新失败的隔离缺口：P3 数据已提交后，重载失败原先只返回 RECOVERY_REQUIRED，旧 running 镜像仍可能继续输出。现在 refresh_committed_session 在读取失败时记录待重载会话，is_faulted / 计时命令 / 快照统一隔离；retry_recovery 只重新读取已提交事实，读取仍失败则保持隔离，不重复写入、不新增审计或版本递增。

回归 timer_snapshot::a_failed_postcommit_refresh_isolates_the_mirror_and_retry_only_reloads_facts：服务实际作废当前会话后，通过临时改名读取列制造重载失败，验证失败时及失败重试后均禁止旧快照；恢复读取后重试得到 Discarded，revision 和审计数量不变。修复前已复现旧镜像未隔离的失败。

兼容性结论：本次未发现需要改变 P1 schema、P4 写信封或 P7 IPC 的接口冲突；P2 的计时结束与 P3 的任务状态跃迁继续分别遵守各自事务边界。P3 只刷新被修改且当前镜像着的会话，避免处理 A 时替换正在计时的 B。P8 文档已纠正“待处理列表非空等价于计时门禁关闭”：当前 run 待确认事实与运行时故障需要分别呈现，不能由列表推导计时可用性；P3/P8 重试契约同步为不重发已提交写操作。

完整门禁：Rust **596 passed / 0 failed / 1 ignored**、前端 **142 passed**，八项检查全部退出 0（Clippy、格式、分层、构建及脚本回归均通过）。证据：`C:\Users\lenovo\AppData\Local\Temp\worktrace-pre-p3-20261005-134005`。这些结果证明自动化范围的兼容性；P6 平台事件/锁屏休眠实机、P8 恢复交互及真实多窗口验收仍未完成，不能宣称产品全链路已验收。

## 2026-10-05 外部复审修复（基线 d85f66f）

发现并复现两类边界缺陷，已在当前工作区修复，未提交：

- S7 重叠查询只使用非空区间公式，误拒绝已有人工记录内部的 `[t,t)` 补录，也误把既有零长度记录当作占用。现查询先放行空请求，并排除已有闭合空区间；有效非空区间和开放区间的冲突判据不变。
- P3 用户端点可使 `duration_ms` 相减溢出而 panic。IntervalRange::new 现在拒绝无法用 i64 表示的跨度（DOMAIN_ERROR），两个相距很远的合法短区间计算交集时先比较边界再相减，避免负差下溢。拒绝发生在写入前，不新增错误码/表/DTO。
- 上述拒绝最初借用 `DomainError::NotInThisVersion`，渲染成「当前版本还没有「跨度超出可表示范围的计时区间」这项功能。」——那是"功能没做"的说法，与"值不合法"不符（该变体别处都用于 V0.1 真不支持的枚举取值）。现改为专用变体 **`DomainError::IntervalSpanOverflow`**，文案「这段时间跨得太长，无法用毫秒表示。」；`AppError` 码仍是 `DOMAIN_ERROR`，前端与 DTO 不受影响。这是对 P3 计划「尽量不新增 `DomainError` 变体」的一次**有意例外**：`tests/error_contract.rs` 的穷尽见证（漏登记即编译失败）与"文案必须是面向用户中文"两条断言同步覆盖，代价是变体清单 +1。

回归：backfill_discard 新增空补录落在可信区间内部、已有空记录不阻挡非空补录、超大跨度拒绝且世界快照不变；domain::interval 新增远距离短区间交集为零。前两条在修复前均报重叠，超大跨度用例在修复前报 subtraction overflow，修复后通过。下方 591 等数字保留为原验收基线，当前完整验证结果以本节追加记录为准。

完整门禁复跑：Rust **595 passed / 0 failed / 1 ignored**，前端 **142 passed**，八项检查均退出 0（含 Clippy、格式、分层、构建）。证据目录：`C:\Users\lenovo\AppData\Local\Temp\worktrace-pre-p3-20261005-133113`。首次在新增溢出回归尚未修复时执行的门禁确实失败；本次通过来自修复后的完整重跑，未用失败记录充数。平台与真实 UI 验收仍按下方未完成项处理。

日期：2026-10-05。**原始验收范围**：`0296827..63eab6c`（`dev` 分支，11 提交 + 1 修复波次），已推送为 `origin/dev = d85f66f`。其上还有两轮外部复审修复（见本文件顶部两节），随 `d85f66f` 之后的提交入库；下方第 1 节的 591 与各处旧数字是**当时的基线快照**，当前完整结果以上方两节为准。
依据：`docs/superpowers/plans/2026-10-03-p3-recovery-and-history.md` 的「完成门槛与人工验收」与[总纲 §5 第 9 条](../superpowers/plans/2026-10-03-v01-plan-index.md)的权威清单。

> **本记录只证明自动化部分。** P3 交付的是**服务层**（`services::{recovery, history, tasks}` + `AppState` 七个命令入口 + 一个只读概览）；
> **IPC 命令、恢复/历史界面与全部实机验收归 P8**（P3 计划「边界」第 4 条与「下游接口」第 2/3 条）。
> 未完成项在本文件末尾单列，**不得**据本文件宣称 V0.1 可发布。

## 1. 自动化门禁（唯一入口：`src-tauri/scripts/check-pre-p3.ps1` 的八项 + `p4-gate.ps1`）

| 项 | 结果 |
| --- | --- |
| `cargo test --offline` | **591 passed / 0 failed / 1 ignored**（P3 前 467；1 ignored 为既有 startup helper） |
| `cargo fmt --check` | 0 |
| `cargo clippy --all-targets --offline -- -D warnings` | 0 告警 |
| `scripts/check-layers.ps1` | 六条 PASSED |
| 前端 | **未改**：`src/types/__snapshots__/` 15 份 fixture 与 `src/` 零 diff（前端 142 passed 为 P7 基线，本次未重跑） |
| `git diff --check` | 0 |
| P1/P2/P4/P7 回归 | 无（同一 suite 全绿；`tests/{startup_order,timer_commands,timer_seams,error_contract,transaction_boundary}.rs` 均在） |

新增 8 个集成测试文件：`recovery_scan` / `reconcile` / `correct` / `backfill_discard` / `local_day_bounds` / `recovery_end_to_end` / `task_session_atomicity` / `exception_closure`。

## 2. 总纲 §5 第 9 条权威清单：逐条核对

### 2.1 02 §8 的 M01/M05 必测案例（14 条）

| 条目 | 结论 | 证据（用例名） |
| --- | --- | --- |
| 暂停后重启 | **已核对** | `recovery_scan::an_old_paused_session_rebound_to_this_run_can_then_be_finished`、`recovery_end_to_end::resuming_a_paused_session_of_a_previous_run_rebinds_it_to_the_current_run` |
| 暂停直接结束 | **已核对** | 同上（重绑后可 `finish`）；`recovery_scan` 第 4 类用例断言"不自动继续计时" |
| 恢复前台冲突 | **已核对** | `reconcile::a_confirmed_session_can_be_resumed`、`recovery_end_to_end`（门禁放开后 `start` 成功）；前台互斥判据本身属 P2，P3 不重写 |
| 并发 start | **不适用（P2）** | P3 不新增启动路径；单锁边界由 P7 的 `AppBoundary` 保证 |
| 跨午夜含暂停 | **不适用（P5 统计口径）** | P3 只交付日界原语：`local_day_bounds` 13 条（含 DST 23/25 小时、空范围、反向拒绝、逐日无缝无重叠拼接） |
| 空范围 | **已核对** | `local_day_bounds::a_zero_length_range_covers_no_day`、`a_reversed_range_is_rejected_instead_of_swapped` |
| 同名根标签 | **不适用（P4）** | — |
| 历史工时重叠 | **已核对** | `correct::{a_retime_that_overlaps_another_interval_of_the_same_session_is_rejected, a_retime_that_overlaps_another_sessions_interval_is_rejected, a_retime_that_only_touches_a_neighbouring_interval_is_allowed}`、`backfill_discard::backfilling_an_overlapping_range_is_rejected_without_any_write`、`reconcile::a_confirmed_range_that_overlaps_confirmed_human_time_is_rejected` |
| 强杀后十秒内重启 | **自动化部分已核对；实机未完成（P8）** | `recovery_end_to_end::a_crash_restart_recovery_and_history_chain_stays_single_counted`（模拟崩溃形态 + 真实 `startup()` + 四类判定 + 门禁放开 + `start` 成功）。**真实强杀与 10 秒内重启的实机步骤未做** |
| 运行/暂停时退出 | **自动化部分已核对；实机未完成（P8）** | `recovery_scan::the_startup_scan_keeps_the_four_classes_of_old_record_apart`（旧 run 的 running/paused/recovering 三种残留各一例、`paused` 不自恢复） |
| 前后改系统时间 | **自动化部分已核对；真实改时属 P6/P8** | `exception_closure::{an_unaccepted_clock_correction_keeps_rejecting_start_and_resume, accepting_the_correction_commits_the_audit_then_clears_the_flag, accepting_the_correction_observes_the_sample_exactly_once, a_long_gap_seen_by_the_correction_command_is_not_counted_as_work, a_backwards_monotonic_clock_is_not_accepted_as_a_correction}` |
| 旧版本编辑冲突 | **已核对** | `reconcile::a_stale_session_version_is_rejected_without_writing`、`correct::a_stale_version_is_refused_even_when_the_request_targets_another_interval`、`backfill_discard::discard_session_refuses_a_stale_version_an_unknown_session_and_an_empty_session`、`task_session_atomicity`（信封与版本） |
| 区间修正后报表重算 | **不适用（P5/P8）** | P3 的职责是让被修正后的事实**非重叠、非负、半开**（`correct` 全量断言）；Today/导出的重算属 P5，界面属 P8 |
| 待确认排除与显式确认 | **已核对（统计排除口径属 P5）** | `reconcile::{confirming_every_pending_interval_in_one_command_closes_the_session, discarding_uncertain_intervals_voids_only_the_pending_ones}`、`reconcile::{attention_overview_keeps_the_live_session_out_and_the_terminal_ones_in, attention_overview_marks_current_run_sessions_and_rejects_a_stale_epoch}`、`backfill_discard::reconcile_discards_only_uncertain_intervals_while_discard_session_voids_all` |

### 2.2 04 §9 的必做集成用例

| 条目 | 结论 | 证据 |
| --- | --- | --- |
| 迁移失败 | **不适用（P1/P6）** | P3 无 schema 变更（`user_version` 不变） |
| 磁盘不足 | **已核对（故障路径）** | `exception_closure::{a_failed_anomaly_transaction_rolls_everything_back_and_faults_the_coordinator, retry_recovery_clears_the_fault_only_after_a_successful_commit, a_hard_monotonic_fault_cannot_be_released_by_retry_recovery}` |
| 重复提交 | **已核对** | `correct::repeating_the_same_retime_changes_nothing`、`task_session_atomicity::repeating_the_same_transition_writes_nothing`、`backfill_discard::discarding_an_already_discarded_session_changes_nothing` |
| 旧版本修改 | **已核对** | 见 2.1「旧版本编辑冲突」 |
| 跨午夜暂停 | **不适用（P5）** | 见 2.1「跨午夜含暂停」 |
| 统计修正 | **不适用（P5）** | P3 的 `correct` 只改事实；统计修正后的重算属 P5 |

### 2.3 06 §4 实现前技术验证

DB 执行边界、单调/墙钟映射：**P1/P2 已完成**；双窗口同步：实验载体在 P7、**实机结论归 P8**；HUD/构建：**P8**。P3 不适用。

## 3. 完成门槛的其余条目

| 门槛 | 结论与证据 |
| --- | --- |
| 全部用户命令带 `expected_data_epoch`；编辑既有对象带 `expected_row_version` | 已核对：`reconcile`/`correct`/`backfill`/`discard_session`/`transition_task` 经 `WriteEnvelope` + `guard_epoch`；`exception_closure::a_stale_epoch_and_a_stale_version_are_rejected_before_any_sampling` |
| 一次用户命令**恰好一次** `revision` | 已核对：`settle` 是唯一落点；`task_session_atomicity::the_report_revision_comes_from_the_write_transaction_only`、`reconcile`（`revision == 1`）、`recovery_scan`（无变化零版本） |
| 被拒命令**逐字段**验证（总纲 §5 第 8 条 ④） | 已核对：多个测试用整份 `WorldFacts`/`Facts` 比对（状态、版本、区间逐字段、审计行数、`revision`） |
| 故障路径闭环（R3） | 已核对：`retry_recovery_clears_the_fault_only_after_a_successful_commit`（成功后**重算门禁**）、`a_hard_monotonic_fault_cannot_be_released_by_retry_recovery`（硬故障仍需新 run） |
| 接口归属已登记（R2） | 已登记：`attention_overview` 与 `retry_recovery` 的 IPC 命令归 **P8**（P8 计划「P8 新增的 IPC 命令」与「P3 实际交付签名与界面口径」）；P3 未新增任何 `#[tauri::command]` |
| 已知例外登记（顺带-2） | 已登记：「提交成功但提交后重建失败」那一笔**不发 `domain.changed`**（P7 验收 §6.5 第 23 条），P3 不新增第二条广播路径 |
| 顺带-3/顺带-4 | 已做：`domain/error.rs` 的 `OverlappingInterval` 注释改为"可跨会话"（仅注释）；`services/tx.rs` 的使用者清单更新为 `catalog`/`daily_plan`/`recovery`/`history`/`tasks`/`timer::coordinator` |
| `pre-p3-closure.md`「P3 必须验证的异常闭环」7 行逐行登记 | 已做：`tests/exception_closure.rs` 的 14 条覆盖 ①②③④⑤，⑥⑦ 复用 Task 1/Task 6 用例并附实跑结果（见各任务报告与逐行表） |

## 4. 人工验收（08 §6）：**未完成，归 P8**

P3 交付时能做的那半已做：**真实文件库** + 真实 `startup()`（`tests/recovery_end_to_end.rs` 走完
`running → 崩溃形态 → 重启扫描 → recovering → reconcile(confirm) → finished → correct → backfill → discard_session`，
并断言门禁放开后 `start` 成功、`resume` 切 `run_id`、同一段时间不被算两遍）。

**未做（不得记通过）**：

- 强杀进程 → 10 秒内重启 → 界面核对四类判定（F-015 的实机部分）；**界面本身尚不存在**（P8）。
- F-009（关窗后托盘可用且计时继续）、F-011、F-016 的实机验收。
- 真实双窗口 §2.1–§2.6（`src-tauri/tests/manual-sync.md`）。
- 平台事件（锁屏/休眠/唤醒/改时）注入——归 P6 的事件源 + P8 的展示。
- `mode`（并行计时）语义复核：V0.1 前台唯一，`useRunningTaskId` 的 mode/kind 语义已在任务侧复核，未扩展 V0.2。

`manual_platform_verified` 保持 **false**。
