# P1 / P2 验收核对记录

日期：2026-10-03。核对对象：`9db5898` 之后 P1/P2 的全部改动（含用户手改的跨 run 采样隔离，
以及本次核对中补的重复提交用例）。校验基线：**206 个测试**。

**依据**：总纲 §5 第 9 条点名的三份权威清单（02 §8 / 04 §9 / 06 §4），加上 §5 第 1–8 条横切约定
与两份计划自身的勾选项。
**方法**：逐条把清单条目映射到**可指名的测试**，或明确写出它归哪份后续计划。
「测试全绿」不等于「清单已覆盖」——本记录就是来分辨这两件事的。

## 一、结论

- **核心验收（库与服务层）：可以验收。** 206 个测试（43 单元 + 163 集成）全绿；
  `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`scripts/check-layers.ps1`
  （domain/storage/services）、`git diff --check` 全过；权威清单中属于 P1/P2 的条目**逐条有指名用例**。
- **平台验收：不在 P1/P2 的完成门槛内，尚未完成。** 分工见第七节。
- **一处仍需用户裁定**（第四节第 3 条：可预期业务冲突被报成 `STORAGE_ERROR`），
  一处**仍是缺口**（第四节第 1 条：跨午夜含暂停的端到端）。
- **两份计划自身的勾选项**：P1 计划 **24/24 全部勾选**；P2 计划 65 勾选、5 未勾，
  而这 5 条**全部明确归后续计划**——P6 维护态期间禁止采样写入、P7/P8 平台接线与实机验收、
  P3 的同 run 显式接受校正、P3/P7 的旧 running 启动扫描。**没有一条落在 P1/P2 自身范围内**。

## 二、02 §8 M01/M05 必测案例（14 条）

| # | 条目 | 状态 | 证据 / 归属 |
| --- | --- | --- | --- |
| 1 | 暂停后重启 | 域与仓储已钉；启动扫描归 P3 | 跨 run 装载 paused 可正常继续：`loaded_paused_session_without_anchor_can_resume_in_new_run`；跨 run running 一律隔离：`old_running_session_is_isolated_before_sampling_with_or_without_new_anchor`（7 入口 × 有/无基线 = 14 组）；四类扫描 → P3 Task 1 |
| 2 | 暂停直接结束 | **已覆盖** | `a_paused_session_can_finish_directly` |
| 3 | 恢复前台冲突 | **已覆盖**（错误码见四-3） | `a_second_foreground_start_is_refused_by_the_index`、`a_second_concurrent_foreground_start_is_refused_by_the_index`、`finishing_another_paused_session_preserves_active_timer` |
| 4 | 并发 start | **已覆盖** | 同上两条索引用例 + `a_stale_epoch_request_writes_nothing` + `freezing_with_a_stale_version_is_refused` |
| 5 | 跨午夜含暂停 | **缺口**（见四-1） | 域层公式：`a_session_crossing_midnight_splits_evenly`、`clipping_follows_the_spec_formula`；**端到端无** |
| 6 | 空范围 | 域层已覆盖；查询归 P5 | 零长度区间合法：`zero_length_intervals_are_valid_but_negative_ones_are_not`；裁剪公式同上；空查询范围属统计 → P5 |
| 7 | 同名根标签 | P1 已建机制，行为归 P4 | `uq_tag_root ON tag(kind,name) WHERE parent_id IS NULL`（`schema_v1.rs:187`）；**无测试踩过** → P4 Task 3 |
| 8 | 历史工时重叠 | **已覆盖** | `rebased_clock_cannot_start_inside_confirmed_history` + `require_available_human_start`（在 start/resume 事务内） |
| 9 | 强杀后十秒内重启 | 事实层已备；扫描归 P3 | `timer_anomaly.rs` 11 条（可信前缀、待确认余段、审计、幂等、字段级回滚）；启动扫描 → P3 Task 1 |
| 10 | 运行/暂停时退出 | 归 P6/P7 | 显式退出原语与 `application_run` 生命周期 → P6；P7 接线 |
| 11 | 前后改系统时间 | **已覆盖**（本轮重点） | `a_500ms_wall_setback_triggers_the_recovery_transaction`、`paused_clock_correction_is_audited_once_and_resume_stays_running`、`trusted_departure_boundary_does_not_hide_wall_clock_jump`、`unaccepted_clock_correction_cannot_expire_with_lifetime_allowance`、`wall_jump_before_first_session_rebases_instead_of_shifting_attribution`、`system_events_do_not_forgive_the_lifetime_drift_bound`；实测依据 `docs/validation/p2-clock-mapping.md` |
| 12 | 旧版本编辑冲突 | **已覆盖** | `a_stale_epoch_request_writes_nothing`、`version_conflict_and_unknown_record_are_distinguishable`、`mismatched_resume_is_rejected_before_sampling_or_writes`、`freezing_with_a_stale_version_is_refused`、`a_stale_epoch_is_rejected_before_any_sampling_or_writing` |
| 13 | 区间修正后报表重算 | 归 P3/P5 | P2 交付审计与事务内原语：`a_time_edit_rolls_back_with_its_transaction`、`time_edits_come_back_in_order`、`audit_rows_are_append_only`，以及 `timer_seams.rs` 的 6 条原语用例 |
| 14 | 待确认排除与显式确认 | 排除**已覆盖**；显式确认归 P3 | `pending_time_does_not_pollute_confirmed_totals`、`intervals_of_session_splits_trusted_pending_and_voided`、`stats_split_closed_and_live_without_double_counting`、`a_recovering_session_freezes_live_accrual`；`reconcile` → P3 Task 2 |

## 三、04 §9 必做集成用例（6 条）

| # | 条目 | 状态 | 证据 / 归属 |
| --- | --- | --- | --- |
| 1 | 迁移失败 | **已覆盖** | `a_future_database_version_is_refused_not_downgraded`、`interrupted_migration_leaves_no_partial_schema_on_disk`、`a_refused_migration_changes_nothing` |
| 2 | 磁盘不足 | **已覆盖**（SQLite 容量层；OS 层归 P6） | `sqlite_full_start_rolls_back_all_business_facts`、`sqlite_full_heartbeat_keeps_checkpoint_and_retries`、`sqlite_full_recovery_rolls_back_and_isolates_until_retry`（用 `max_page_count` 真实触发 `SQLITE_FULL`，且先断言原生错误码） |
| 3 | 重复提交 | **已覆盖**（本轮补齐） | `replaying_the_same_start_request_is_refused_without_a_second_session`（重放同一条 `start` ⇒ `VERSION_CONFLICT`、会话数仍为 1、revision 不再增加、第一次的会话原样保留）、`the_post_commit_recovery_path_never_creates_anything`、`a_post_commit_failure_is_recovery_not_retryable`、`one_command_bumps_revision_exactly_once` |
| 4 | 旧版本修改 | **已覆盖** | 同 02 §8 第 12 条 |
| 5 | 跨午夜暂停 | **缺口**（见四-1） | 同 02 §8 第 5 条 |
| 6 | 统计修正 | 归 P3/P5 | 同 02 §8 第 13 条 |

## 四、核对中发现的三处

1. **跨午夜含暂停仍是缺口（02 §8 与 04 §9 同时列了它）。**
   域层裁剪公式有用例，但**没有端到端**：真实跑一段跨 00:00 的会话、中间暂停、再按日界求和。
   要么在 P2 补一条端到端（FakeClock 推到 23:50 → pause → 跨日 resume → 断言两天的归属之和），
   要么在 P5 的报表用例里点名承接。**现状是两边都没写**——这是本记录里唯一没有任何计划认领的清单条目。
2. **重复提交（04 §9）：本轮补齐。** 见上表第 3 条。补之前只有"提交后失败不重复创建"这一类用例，
   没有"重放同一请求"的直接证据。
3. **待用户裁定：「已在计时时再 start/恢复」的错误码。**
   `require_available_human_start` 的判据是 `i.ended_at > ?1`，而 running 区间的 `ended_at IS NULL`
   使该比较为 NULL ⇒ **这条守卫看不见"正在计时"**；冲突最终由 `uq_running_foreground` 兜住，
   返回 `STORAGE_ERROR`，用户看到「存储暂时不可用，请稍后重试」——一个可预期的**业务冲突被报成
   基础设施故障**，而且文案引导重试（重试永远不会成功）。
   状态不变量没破（`a_second_foreground_start_is_refused_by_the_index` 断言会话数仍为 1），
   但**语义与文案都指错方向**。建议 P7 接线前补一条 domain 级检查（存在 running 前台时拒绝
   start/resume，返回 `DOMAIN_ERROR` + 可理解文案），并同步改掉那条断言的期望码。

## 五、06 §4 实现前技术验证

- **DB 执行边界** ✓ 已完成并记录：`IMPLEMENTATION-NOTES.md` §2（两种边界必然串行，92.4ms vs 87.9ms）
  + `tests/db_execution_boundary.rs` 5 条。
- **单调/墙钟映射** ✓ 已完成并记录：`docs/validation/p2-clock-mapping.md`（两钟 233 ppm 分叉、
  分辨率均为 100ns、挂起 129s 时 QPC 照常推进），分析器在 `platform/clock.rs`。
- 双窗口同步、HUD/构建 → **P7**，不在 P1/P2 范围。

## 六、横切约定 §5 第 1–8 条的核对

| # | 约定 | 状态与证据 |
| --- | --- | --- |
| 1 | 分层与依赖方向 | ✓ `check-layers.ps1` 输出 domain / storage / services 三项 clean |
| 2 | 工具链固定 | ✓ 全部命令在 `src-tauri/` 下跑；`rusqlite` 用 bundled，不依赖系统 SQLite |
| 3 | 错误契约 | ✓ `error_contract.rs` 6 条，含 `errors_never_leak_paths_sql_or_payload`、`version_conflict_carries_numbers_but_not_in_message`、`domain_error_messages_are_user_facing_chinese` |
| 4 | 写事务信封 | ✓ `transaction_boundary.rs` 14 条：`one_business_write_bumps_revision_exactly_once`、`writing_a_checkpoint_does_not_bump_revision`、`a_stale_epoch_request_writes_nothing`、`a_failure_in_the_last_step_rolls_back_everything` |
| 5 | 测试策略 | ✓ 时间一律 `FakeClock` 注入；库用 `tempfile` 或 `Db::open_in_memory()`（错误响应用例即用后者） |
| 6 | 人工验收单列 | ✓ 平台项逐条列在第七节，不冒充已验收 |
| 7 | 改动纪律 | ✓ 未碰前端与 `greet` |
| 8 | 断言口径 | ✓ 见下 |

**第 8 条（断言口径）的具体落点**：被拒命令四件事 —— `a_stale_epoch_request_writes_nothing`、
`old_running_session_is_isolated_before_sampling_with_or_without_new_anchor`（同时断言 revision、
会话行版本、区间 `ended_at`/`duration_ms`/`needs_review`、`time_edit` 零新增）；
幂等 —— `a_repeated_anomaly_is_idempotent`、`replaying_the_same_start_request_...`；
CHECK 边界 —— `the_boundary_at_2000ms_is_exact`；错误码一律断言 `code()` 字符串（多处）。

## 七、平台与后续项（明确不属于 P1/P2，不能算已完成）

| 项 | 归属 |
| --- | --- |
| 崩溃启动扫描四类判定、`reconcile`/`correct`/`backfill`/`discard_session` | P3 |
| 统计口径与日界裁剪、Today 聚合、导出、周回顾 | P5 |
| 正式系统事件接线（锁屏/休眠/唤醒）、关窗后继续采样、单实例、`application_run` 生命周期、显式退出、WAL 一致备份、恢复后 epoch 切换、维护态隔离 | P6 / P7 |
| OS 磁盘耗尽、WAL 写失败、释放空间后的恢复 | P6 |
| IPC 接线（含失败响应携带权威版本）、托盘、页面 | P7 / P8 |
| 500 ppm 容差跨机器校准、改时/锁屏的实机复核 | P8 |
| 19 张图与 10 份文档的「待审核」标记 | 用户 |

## 八、证据与数字

- **206 个测试** = 43 个库内单元测试（`src/`）+ 163 个集成测试（12 个套件）：
  `domain_invariants` 18、`transaction_boundary` 14、`timer_clock` 12、`timer_anomaly` 11、
  `timer_commands` 28、`timer_regressions` 40、`timer_seams` 10、`timer_snapshot` 9、
  `error_contract` 6、`migrations` 6、`db_execution_boundary` 5、其余为库内单元。
- 最近一次全量运行在本记录对应的合并状态上：`cargo test --offline` 206 passed / 0 failed；
  `cargo fmt --check`、`cargo clippy --offline --all-targets -- -D warnings`、
  `scripts/check-layers.ps1`、`git diff --check` 均通过。
- 本记录**不改变任何产品行为**，只补了一条清单缺口的测试（重复提交）。
