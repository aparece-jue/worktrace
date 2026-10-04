# P3 验收记录：恢复确认、历史修正与补录

日期：2026-10-05。范围：`0296827..63eab6c`（`dev` 分支，11 提交 + 1 修复波次；未 push）。
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
