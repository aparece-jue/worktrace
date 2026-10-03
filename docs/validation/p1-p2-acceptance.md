# P1 / P2 验收核对记录

日期：2026-10-03。核对对象：`9db5898` 之后 P1/P2 的全部改动（含用户手改的跨 run 采样隔离，
以及本次核对中补的重复提交用例）。校验基线：**209 个测试**。

**依据**：总纲 §5 第 9 条点名的三份权威清单（02 §8 / 04 §9 / 06 §4），加上 §5 第 1–8 条横切约定
与两份计划自身的勾选项。
**方法**：逐条把清单条目映射到**可指名的测试**，或明确写出它归哪份后续计划。
「测试全绿」不等于「清单已覆盖」——本记录就是来分辨这两件事的。

## 一、结论

- **核心验收（库与服务层）：可以验收。** 209 个测试（43 单元 + 166 集成）全绿；
  `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`scripts/check-layers.ps1`
  （domain/storage/services）、`git diff --check` 全过；权威清单中属于 P1/P2 的条目**逐条有指名用例**。
- **平台验收：不在 P1/P2 的完成门槛内，尚未完成。** 分工见第七节。
- **第四节的两处已在本次补齐**（用户裁定"都现在补"）：跨午夜含暂停由 **P2 验证事实、P5 验证分桶**
  两边承接；前台占用冲突改为业务事务内的领域检查、返回 `DOMAIN_ERROR`。详见第四节。
- **两份计划自身的勾选项**：P1 计划 **24/24 全部勾选**；P2 计划 65 勾选、5 未勾，
  而这 5 条**全部明确归后续计划**——P6 维护态期间禁止采样写入、P7/P8 平台接线与实机验收、
  P3 的同 run 显式接受校正、P3/P7 的旧 running 启动扫描。**没有一条落在 P1/P2 自身范围内**。

## 二、02 §8 M01/M05 必测案例（14 条）

| # | 条目 | 状态 | 证据 / 归属 |
| --- | --- | --- | --- |
| 1 | 暂停后重启 | 域与仓储已钉；启动扫描归 P3 | 跨 run 装载 paused 可正常继续：`loaded_paused_session_without_anchor_can_resume_in_new_run`；跨 run running 一律隔离：`old_running_session_is_isolated_before_sampling_with_or_without_new_anchor`（7 入口 × 有/无基线 = 14 组）；四类扫描 → P3 Task 1 |
| 2 | 暂停直接结束 | **已覆盖** | `a_paused_session_can_finish_directly` |
| 3 | 恢复前台冲突 | **已覆盖**（错误码见四-3） | `a_second_foreground_start_is_refused_as_a_domain_conflict`、`resuming_while_another_foreground_runs_is_a_domain_conflict`、`a_second_concurrent_foreground_start_is_refused_by_the_index`、`finishing_another_paused_session_preserves_active_timer` |
| 4 | 并发 start | **已覆盖** | 服务占用领域冲突用例与并发唯一索引兜底用例 + `a_stale_epoch_request_writes_nothing` + `freezing_with_a_stale_version_is_refused` |
| 5 | 跨午夜含暂停 | **已覆盖**（P2 事实 + P5 分桶） | P2：`pausing_across_midnight_keeps_pause_out_of_effort`（23:45 工作到 23:55 → 暂停跨午夜两小时 → 次日 01:55 再工作 30 分钟 → 结束；暂停不计入、两段各自不跨午夜）；域层公式：`a_session_crossing_midnight_splits_evenly`、`clipping_follows_the_spec_formula`；分桶归 P5（见 P5 计划「跨午夜与日界分桶」） |
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
| 5 | 跨午夜暂停 | **已覆盖**（P2 事实 + P5 分桶） | 同 02 §8 第 5 条；P5 半边已写进 P5 计划 Task 1 |
| 6 | 统计修正 | 归 P3/P5 | 同 02 §8 第 13 条 |

## 四、核对中发现的三处（均已在本次补齐）

1. **跨午夜含暂停（02 §8 与 04 §9 同时列了它）——按「P2 验证事实、P5 验证分桶」切开承接。**
   - **P2（事实）**：`pausing_across_midnight_keeps_pause_out_of_effort`——23:45 工作到 23:55 →
     暂停两小时跨过午夜 → 次日 01:55 继续 30 分钟 → 结束；断言暂停一毫秒不计入
     （`active_ms = 40 分钟`）、两段区间各自的起止与 `duration_ms`、两段都可信，
     且**没有任何一段跨越午夜**（日界恰好落在两段之间）。
   - **P5（分桶）**：已写进 P5 计划 Task 1「跨午夜与日界分桶」——按查询时区实际日界拆分可信区间，
     每日之和 == 不分组总和，跨日区间拆成两段且两段之和等于原时长，不同查询时区归属日期不同但总和相同。
   - 归属因此明确：**事实从 P2 来，P5 不重造**。
2. **重复提交（04 §9）：本轮补齐。** `replaying_the_same_start_request_is_refused_without_a_second_session`
   —— 补之前只有"提交后失败不重复创建"这一类间接证据。
3. **前台占用冲突的错误码：已修（原来是 `STORAGE_ERROR`）。**
   根因：`require_available_human_start` 的判据 `i.ended_at > ?1` 对 running 区间
   （`ended_at IS NULL`）为 NULL ⇒ **这条守卫看不见"正在计时"**，冲突一路落到唯一索引上，
   被报成 `STORAGE_ERROR`（文案「存储暂时不可用，请稍后重试」）——可预期的业务冲突被报成
   基础设施故障，还引导用户去重试一个永远不会成功的操作。
   现在：新增 `session_repo::require_no_running_foreground(conn, exclude)`，在 `start`/`resume`
   的**业务事务内**先判，`resume` 传 `Some(&session_id)` **排除目标自身**；
   `uq_running_foreground` **保留为兜底**（`transaction_boundary.rs` 里那条绕过服务、
   直连第二个连接写 `create_session` 的用例继续覆盖它）。
   回归两条：`a_second_foreground_start_is_refused_as_a_domain_conflict`、
   `resuming_while_another_foreground_runs_is_a_domain_conflict`，都断言 `DOMAIN_ERROR`、
   文案含「正在计时」，且会话/区间/行版本/revision 一律不变。
   **反向验证**：把 `start` 里那行领域检查去掉 ⇒ 测试失败并报
   `left: "STORAGE_ERROR" / right: "DOMAIN_ERROR"`——正是修复前的行为。

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

- **209 个测试** = 43 个库内单元测试（`src/`）+ 166 个集成测试（12 个套件）：
  `domain_invariants` 18、`transaction_boundary` 14、`timer_clock` 12、`timer_anomaly` 11、
  `timer_commands` 30、`timer_regressions` 41、`timer_seams` 10、`timer_snapshot` 9、
  `error_contract` 6、`migrations` 6、`db_execution_boundary` 5、`timer_checkpoint` 4。
- 最近一次全量运行在本记录对应的合并状态上：`cargo test --offline` 209 passed / 0 failed；
  `cargo fmt --check`、`cargo clippy --offline --all-targets -- -D warnings`、
  `scripts/check-layers.ps1`、`git diff --check` 均通过。
- 本记录汇总核心测试与后续归属；相关产品行为变更包含前台占用错误码修复，不宣称已经完成后续报表或平台验收。


## 本轮 P4 开工审查补正

跨午夜 P2 事实测试保留 00:00 暂停场景，并追加 23:45 开始工作、23:55 暂停、01:55 恢复、02:25 结束的场景。暂停区间严格跨过日界，工作仍为 10+30=40 分钟；P5 负责实际查询时区分桶。测试总数不增加，因两场景在同一行为测试内参数化。

修正旧前台占用测试名及遗漏的 timer_checkpoint 4 条，使十二套件之和与 165 个集成测试一致。总纲拦截机制统一为独立标记。P4 补上已捕获任务的项目关联服务，避免 F-002 “理清时可选项目”只有创建任务时能做到；这些为实施前计划补全，尚未实现 P4 服务。

本轮独立验证：208 个测试全部通过，严格 Clippy、格式检查、分层检查通过。P1/P2 核心无新增阻断项，P4 计划就绪；本轮未实施 P4，平台与后续验收仍按第七节归属。
