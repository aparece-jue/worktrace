# P4 验收核对记录（项目、标签与今日计划）

日期：2026-10-03。核对对象：P4 计划（`docs/superpowers/plans/2026-10-03-p4-projects-tags-today.md`）从
`3013640` 起的全部改动（6 个任务 + 各自的评审修复轮）。**校验基线：339 个测试**（P4 开始时 209 → 结束 339）。

**依据**：总纲 §5 第 9 条点名的三份权威清单（02 §8 / 04 §9 / 06 §4）中与 P4 相关的条目，
加上 §5 第 1–8 条横切约定、§4 的 F-ID 覆盖矩阵与 P4 计划自身的勾选项。
**方法**：与 P1/P2 相同——逐条把清单条目映射到**可指名的测试**，或明确写出它归哪份后续计划。
「测试全绿」不等于「清单已覆盖」，本记录就是来分辨这两件事的。

## 一、结论

- **核心验收（库与服务层）：可以验收。** 339 个测试全绿（P4 新增 130 条：`catalog_validation` 29、
  `projects` 28、`tags` 16、`task_filters` 26、`daily_plan` 22、`error_contract` 新增 9）；
  `cargo fmt --check`、`cargo clippy --all-targets --offline -- -D warnings`、
  `scripts/check-layers.ps1`（domain / storage / services 三条，本轮**补强**了
  services→commands、storage→commands、domain→commands/services 四条反向边）全过。
- **权威清单中属于 P4 的条目逐条有指名用例**（第二节）；04 §9 与 06 §4 的其余条目明确不属 P4（第三节、第四节）。
- **不做的事一件没做**：标签权重规则（F-108）、Knowledge 与标签层级（F-107）、`time_block` 排期、
  Goal/Milestone、UI 页面——都在 V0.2 / P7（第七节）。
- **P4 计划自身的勾选项**：33 条逐条落地，其中 4 条（验收与下游接口）在本记录完成；
  5 条「本轮开工审查补全」全部落地（含错误上下文载荷形状与 `set_task_project`）。
- **整体评审（whole-branch review）已完成**：终评结论「**无 Critical，服务层可验收可合并**」，
  并在合并前处理掉 3 条 Important（一条计划点名的用例抓不住目标、一类「拿被测函数当 oracle」的
  漏传播、一处与事实相反的登记）与 5 条顺手项；triage 见第十一节。
- **已知缺陷（P1/P2 遗留，P4 未修，见第九节）**：用户可见文案仍有一批半英文/英文
  （`EmptyText` 的列名与 `"session"`/`"interval"`、`guard_row_version_of` 的 `no such {table}`、
  `coordinator.rs` 的 `"no such session"`），以及 `create_task` 对 `done` 项目放行与新入口拒绝的不一致。
  **这些不是 P4 引入的**，按总纲 §5 第 7 条「只碰计划列出的文件」与工作区约定
  （发现无关问题要说出来、不顺手改）报给用户裁定；其中**唯一还活着的内部标识泄漏路径**
  （`NotInThisVersion`）已在最终修复波修掉。

## 二、02 §8 M01/M05 必测案例（14 条）中与 P4 相关的条目

| # | 条目 | P4 侧状态 | 证据 / 归属 |
| --- | --- | --- | --- |
| 7 | 同名根标签 | **已覆盖（本轮补齐）** | `tags.rs::duplicate_names_are_refused_within_a_kind_but_allowed_across_kinds`（同 kind 重名拒、跨 kind 同名放行）、`tags.rs::tag_names_are_case_sensitive`（大小写敏感）、`catalog_validation.rs::tag_uniqueness_is_per_kind_and_case_sensitive`（唯一索引兜底）；实现见 `storage/tag_repo.rs` + `uq_tag_root` |
| 12 | 旧版本编辑冲突 | **已覆盖（P4 新增半边）** | 项目：`projects.rs::renaming_rejects_a_stale_version_without_touching_the_row`、`archiving_rejects_a_stale_version`、`update_commands_refuse_an_envelope_without_a_row_version`；归属：`binding_rejects_a_stale_task_version`、`binding_rejects_a_stale_epoch`；理清：`task_filters.rs::clarifying_with_a_stale_version_is_refused_with_version_conflict`；查询：`a_query_with_a_stale_data_epoch_is_refused_with_data_epoch_mismatch`；标签与计划的旧 epoch：`tags.rs::a_stale_epoch_is_refused_before_anything_is_written`、`daily_plan.rs::a_stale_epoch_is_rejected_before_anything_is_written` |
| 其余 12 条 | — | 不适用 | 属 P1/P2 的计时、会话、区间、恢复与统计范畴（P1/P2 记录已逐条核对），P4 未触碰计时链路 |

## 三、04 §9 必做集成用例（6 条）中与 P4 相关的条目

| # | 条目 | P4 侧状态 | 证据 / 归属 |
| --- | --- | --- | --- |
| 4 | 旧版本修改 | **已覆盖** | 同 02 §8 第 12 条 |
| 3 | 重复提交 | **幂等半边已覆盖** | 重复提交在 P2 已覆盖（重放 `start` ⇒ `VERSION_CONFLICT`、会话数不变）。P4 的对应面是**幂等集合操作**：`projects.rs::renaming_to_the_same_name_changes_nothing`、`archiving_an_archived_project_changes_nothing`、`setting_the_same_project_changes_nothing`；`tags.rs::tagging_the_same_tag_twice_changes_nothing_the_second_time`、`untagging_a_tag_that_is_not_there_changes_nothing`；`daily_plan.rs::adding_the_same_task_twice_is_idempotent`、`removing_a_task_that_is_not_in_the_plan_is_idempotent`。每条都断言 `revision` 不变、返回值表示「没有变化」、且行数/字段/审计三处逐字不变（「零写语句」是**效果层**的断言，语句级没有直接观察——终评指出措辞宜更准确） |
| 2 | 磁盘不足 | P4 侧覆盖「审计写失败整体回滚」 | `projects.rs::a_failed_audit_write_rolls_the_whole_assignment_back`、`tags.rs::a_failed_audit_write_leaves_no_half_link`、`daily_plan.rs::a_failed_audit_write_leaves_no_half_plan_row`（触发器注入 `RAISE(ABORT)`）。**真正的 OS/磁盘故障仍归 P6**，P4 不冒充 |
| 1/5/6 | 迁移失败 / 跨午夜暂停 / 统计修正 | 不适用 | 归 P1/P2 与 P5 |

## 四、06 §4 实现前技术验证

P4 **不涉及**这四项（DB 执行边界与单调/墙钟映射已由 P1/P2 完成并记录；双窗口同步与 HUD 构建归 P7）。
**P4 唯一新增的技术验证是时区库选型**，已在计划「开工前已核实」中离线实测并落到依赖行
（`jiff 0.2`，非默认特性），本轮 `catalog_validation.rs` 的用例在 Windows 上复验：
`timezone_resolution_works_without_system_tzdata`（5 个 IANA 名字都能解析出转换规则）。
**注意口径**：该用例**不能**区分「数据来自打进产物的 tzdb」与「来自系统 zoneinfo」——
「不依赖系统 tzdata」是**构建期特性**的结论（`jiff-static` 不在缓存、依赖行必须显式带
`tzdb-bundle-platform`，见计划「开工前已核实」），运行期/打包验证归 **P6/P7**。

## 五、横切约定 §5 第 1–8 条逐条核对

| 条 | 内容 | P4 侧结论 |
| --- | --- | --- |
| 1 | 分层与依赖方向 | **已核对并加强**。`check-layers.ps1` 本轮补上 `services→commands`、`storage→commands`（P4 Task 2）与 `domain→commands/services`（P4 Task 6）四条反向边，且做了反向验证（注入一行反向引用 ⇒ LEAK + exit 1）。`WriteEnvelope` 因此从 `commands/` 搬到 crate 根（`src/envelope.rs`），IPC 层改从 `crate::envelope` 引用 |
| 2 | 工具链固定 | **已遵守**：全部 `cargo` 命令只在 Windows 侧执行（WSL 缺 pkg-config/gtk/webkit，编译不了 `tauri` 依赖）；同一轮只用一套工具链 |
| 3 | 错误契约 | **已核对**：预期失败一律 `Result<_, AppError>`，`code()` 是唯一分支依据；`DomainError` 从 14 增至 23 个变体，文案全部中文且经 `zh_status`/`zh_kind` 映射。**终评纠正**：本轮之前仍有**一条活路径**会漏内部标识——`NotInThisVersion { what: to.as_str() }` ⇒「当前版本还没有「Scheduled」…」，且 `error_contract` 的禁用词表恰好缺 `Scheduled`/`Review`/`Cancelled`；已在最终修复波改为渲染点映射并补齐词表（反向验证：改回原样 ⇒ 该用例变红） |
| 4 | 写事务信封 | **已核对**：一次业务写恰好 `revision + 1`；被拒命令零变化（每条拒绝用例都断言四件事；终评发现两处版本冲突用例只断 code，已在最终修复波补齐）。**口径纠正**：`WriteEnvelope` 有 **11 个服务消费者**（只接收 `env` 参数、不构造它），而 `for_create`/`for_update` 在 P4 的**生产代码里零调用**（9 次构造全在测试）——终评指出 `src/envelope.rs` 的「调用方清单」与事实相反，已按事实重写；构造函数保留，**P7 的 IPC 层是第一个生产构造者** |
| 5 | 测试策略 | **已遵守**：时间经 `FakeClock` 注入；库用 `tempfile`；不追求一仓储函数一测试（按规则分组） |
| 6 | 人工验收单列 | **P4 无 UI/托盘/OS 事件**，人工验收归 P7/P8（第七节）；仓储测试没有被标成「UI 已验收」 |
| 7 | 改动纪律 | **已遵守**。计划外但**均经计划/裁决授权**的改动有四处：① Task 5 修复轮改 `task_repo::create_task` 的两处**拒绝文案**；② 按计划 Task 6 要求同步改 `tests/timer_regressions.rs` 的 3 条错误响应用例；③ 按 R-T6-j 删除 `src/commands/envelope.rs` 转发路径与其用例；④ 按 R-T6-k 扩展 `scripts/check-layers.ps1`（services/storage/domain 的反向边）。`services/timer/**` **业务逻辑零改动** |
| 8 | 断言口径 | **已核对**，且本轮抓到三类恒真断言并修掉：① `detail().is_some()`／在 `message()` 上找中文（Task 1，`assert_domain_error`）；② 拿**被测函数**当零变化快照 oracle（Task 5，改直连 SQL）；③ `error_contract` 的「文案是中文」断言落在 `message()` 上（Task 6，改断 `detail()`）。错误码一律断字符串；CHECK 边界与幂等返回值都有专项用例 |

## 六、F-ID 覆盖（P4 那半边）

| F-ID | P4 交付 | 指名用例（示例） | 仍缺（归后续） |
| --- | --- | --- | --- |
| **F-002** 任务理清（可选项目/标签） | `catalog::set_task_project`（只改 `project_id` + 审计 + 版本，不建项目、不迁属性、不改状态）、`catalog::create_task`（捕获）、`catalog::clarify_ready`（Inbox/Clarifying → Ready，单事务） | `binding_a_task_to_a_project_updates_the_row_and_appends_one_audit_row`、`binding_is_allowed_only_before_the_task_starts`、`a_clarified_ready_task_is_findable_by_its_project`、`task_filters.rs::clarifying_an_inbox_task_moves_it_to_ready_with_one_revision_and_one_audit_row` | 界面与 `start` 原子理清并启动归 P7/P3 |
| **F-004** 项目管理 | 建/重命名/归档、归档后从选择列表消失、归档保留历史、旧版本拒绝 | `creating_a_project_writes_an_active_row_and_bumps_revision_once`、`the_selection_list_hides_archived_projects_while_the_history_stays`、`archiving_a_project_keeps_its_tasks_and_history`、`starting_a_timer_on_a_task_whose_project_was_archived_is_refused` | Projects 页与项目任务列表归 P7 |
| **F-005** 基础标签 | 四类标签创建与按 kind 列举、多标签打标/去标、拒绝 Knowledge 与层级、Context 用于筛选 | `tags.rs::all_four_tag_kinds_are_created_and_read_back`、`a_task_can_carry_several_tags_and_untagging_removes_only_one`、`hierarchy_requests_are_refused_in_this_version`、`task_filters.rs::a_context_tag_filter_returns_each_task_once_even_when_it_carries_several_tags`、`an_unknown_or_non_context_tag_is_refused_without_touching_the_database` | 页面归 P7；权重与层级归 V0.2 |
| **F-010** Today（选择列表半边） | `daily_plan` 按用户时区记录某日人工选择，不自动排期、不搬移，日期不取 `updated_at` | `daily_plan.rs::a_plan_read_lists_only_the_requested_day_and_timezone`、`the_same_task_on_the_same_day_under_another_timezone_key_is_a_separate_row`、`adding_a_task_to_the_plan_does_not_touch_the_task_row`、`a_task_state_change_does_not_move_its_plan_rows` | 统计聚合 DTO 与导出归 P5/P8；跨午夜分桶归 P5 |

**轻量 GTD 列表（用户确认的补充验收）**：Ready/Waiting/Blocked 列表、项目与 Context 组合筛选、
空结果与稳定分页、多标签不重复显示——全部落在 `tests/task_filters.rs`（26 条），
其中 `a_status_set_combines_with_a_project_and_a_context_tag` 是计划点名的「项目与情境组合」用例。

## 七、平台与后续项（明确不属于 P4，不能算已完成）

1. **界面**：Projects 页、标签选择器、筛选面板、Today 页 → **P7/P8**。P4 只交付服务与 DTO 载荷。
2. **IPC 接线**：`capture_error_response` 的新形状（`targets: &[AuthorityTarget]` → `records`）需要
   P7 在命令层接上；`AuthorityTarget` 的 `Deserialize` 目前**无生产调用方**，就是为它的请求边界准备的。
3. **`start` 原子理清并启动**（F-002 的另一半）→ **P3/P7**；P4 的 `clarify_ready` 不替代它。
4. **统计与导出**（F-010 的统计半边、F-018）→ **P5**；`task_change` 现有三种 JSON 形状，
   P5 取完成项**必须按形状过滤**（已集中登记在 `storage/mod.rs` 的模块文档）。
5. **备份恢复、维护态、故障路径**（F-019/F-020）→ **P6**；P4 的审计失败回滚用例不冒充磁盘故障验收。
6. **`daily_plan` 的读索引**：`(local_date, timezone)` 查询用不上主键前缀（PK 首列是 `task_id`），
   V0.1 未加索引（加索引需要迁移）→ 留给 P6/V0.2 评估。

## 八、下游接口登记（供 P5 / P7 消费）

完整签名见各任务报告 §3（`task-1-report.md` … `task-6-report.md`）。P4 结束时的公开面：

| 模块 | 关键入口 |
| --- | --- |
| `domain::{localdate, project, tag}` | `LocalDate::{parse,new,year,month,day}` + `Display`；`ProjectStatus::{ALL,as_str,parse,is_writable_in_v01}`；`TagKind::{ALL,as_str,parse}`、`normalize_name`、`ensure_no_parent` |
| `storage::project_repo` | `get_project`、`list_projects(conn, Option<ProjectStatus>)`、`create_project`、`rename_project`、`archive_project`（写全取 `&Transaction`） |
| `storage::tag_repo` | `create_tag`、`get_tag`、`list_tags`、`tags_of_task`、`tag_task`、`untag_task` |
| `storage::daily_plan_repo` | `plan_for(conn, &LocalDate, tz_key)`、`add_to_plan`、`remove_from_plan` |
| `storage::task_repo` | 新增 `set_task_project`、`ProjectFilter/TaskFilter/Page/TaskPage/list_tasks_filtered`；**删除** `list_tasks`（零调用） |
| `storage` | `WriteOutcome<T>{Changed,Unchanged}`、`require_task` |
| `services::catalog` | `parse_tag_kind`、`normalize_tag_name`、`normalize_project_name`、`parse_project_status`；`create_project`/`rename_project`/`archive_project`/`list_selectable_projects`；`set_task_project(env, task_id, ProjectTarget)`；`create_tag`/`list_tags`/`tags_of_task`/`tag_task`/`untag_task`；`create_task`（捕获）、`clarify_ready`、`list_tasks_filtered`（只读，返回 `epoch/revision`） |
| `services::daily_plan` | `parse_local_date`、`normalize_timezone`（返回**存储键**）、`system_timezone_name`、`local_date_at`；`plan_for`（只读）、`add_to_plan`、`remove_from_plan` |
| `services::error_response` | `capture_error_response(db, error, targets: &[AuthorityTarget]) -> ErrorResponse`；`ErrorAuthority{data_epoch, revision, records: Vec<RecordVersion>}`、`RecordVersion{kind: AuthorityKind, id, row_version: Option<i64>}` |
| `envelope`（crate 根） | `WriteEnvelope::{for_create, for_update}`——IPC 层从 `crate::envelope` 引用 |

## 九、证据与数字

- **门禁**（Windows，离线）：`cargo test --offline` **339 passed / 0 failed**；
  `cargo fmt --check` EXIT 0；`cargo clippy --all-targets --offline -- -D warnings` 无告警；
  `scripts/check-layers.ps1` PASSED（domain/storage/services）。
- **P4 提交链**（`3013640` → 最终，17 个提交，**未 push**）：Task 1 `55c99f7`/修复 `c6e8cc9`；
  Task 2 `a2ba883`/修复 `9d7cde3`；Task 3 `662e7d6`/修复 `6e622b0`；Task 5 `1938b47`/修复 `b6b99bb`/`272887e`；
  Task 4 `03efff6`/修复 `7c3826e`；Task 6 `7be2512`/`b5de78f`/`ee4b9f6`/修复 `00505c3`；
  终评修复波 `118520b`（门禁补 storage→services、状态名走中文映射、文档与引用订正）与
  `c312640`（联合过滤用例补上「只由情境条件排除」的候选、零变化基线改直连 SQL 快照）。
- **反向验证**：每个修复轮都做了——把修复去掉/改坏，确认**恰好**对应用例变红
  （Task 1 改 detail 语言 ⇒ 1 红；Task 2 塞回旧 import ⇒ 只有分层门禁红；Task 3 改回英文映射 ⇒ 4 红；
  Task 4 去掉 `, task.id` ⇒ 1 红；Task 5 三处篡改各 1 红；Task 6 五处篡改 + 分层注入 + 骨架篡改）。
  空测试与恒真断言是本轮抓到的**真实缺陷**（三次），不是形式。
- **已知缺陷（P1/P2 遗留，未修）**：
  1. `task_repo.rs` 的 `EmptyText { field: "task.title" }` / `"task"`（`transition_task`、
     `freeze_baseline_estimate` 两处）⇒ 用户看到「「task.title」不能为空。」这类半英文文案；
     `session_repo.rs`（`"session"`/`"interval"`）与 `checkpoint_repo.rs`（`"interval"`）同属一类；
     另有第三套「任务不存在」文案在 `task_repo.rs::require_active_project`（手写中文）。
  2. ~~`domain/task.rs` 的 `NotInThisVersion { what: to.as_str() }`~~ —— 终评指出这是**唯一还活着的**
     内部标识泄漏路径，已在最终修复波修掉（渲染点走 `zh_status`，禁用词表补 `Scheduled`/`Review`/`Cancelled`）；
  3. `storage/guards.rs` 的 `guard_row_version_of` ⇒ `AppError::Domain { detail: "no such {table}" }`（整句英文）；
  4. `services/timer/coordinator.rs` 的 `AppError::Domain { detail: "no such session" }`（整句英文，
     P2 遗留、当前不可达）；
  5. `task_repo::create_task` 对 `done` 项目放行，而 P4 的 `set_task_project` 拒绝 `done`——
     V0.1 写不出 `done` 项目，当前无实际差异，但 V0.2 引入 `done` 前必须对齐。
  **P4 已修的同类问题**：`TagNameTaken` 漏 `Domain`（Task 3）、`create_task` 的两处借用 P1 文案（Task 5）、
  `TaskHasRunningSession` 的「再改归属」措辞（Task 5）。

## 十、待用户裁定（不影响 P4 验收）

1. **术语「上下文」vs「情境」**：用户文案按 `99-glossary.zh.md` §5 用了「上下文」（`zh_kind`），
   而 04 F-005 与 P4 计划的筛选条款写「情境」。若最终选「情境」，`domain/error.rs` 的 `zh_kind`
   与 `tests/error_contract.rs` 的逐字断言/禁用词表、`tests/tags.rs` 的两条 needle 共 **5 处**一起改。
2. **第九节的 5 条 P1/P2 遗留缺陷**：是否现在收掉（各约 1–3 行）还是并入 V0.2 前的清理任务。
3. **推送状态（已核实，不再是待办）**：P4 的全部提交与 README 那处修订都已推送到 `origin/dev`；
   推送后 `git fetch` 复核过 `local == remote == c981590`，工作树干净。


## 十一、最终整体评审（whole-branch review）与 triage

**范围**：`3013640..c312640`（17 个提交、30 余文件）。**结论：无 Critical，服务层可验收可合并。**
逐条核实为真的部分：计划的 33 个勾选项无「勾了没做」；11 个写入口全部经 `services::tx` 的骨架、
每个恰好 `revision + 1`；`guard_epoch` 一律吃请求带来的期望值且在事务内；
`task_change` 只有一个写入口、恒与实体更新同事务（三种 JSON 形状已集中登记）；
SQL 全参数化（含分页）；FK 的 `ON DELETE RESTRICT` 未被破坏；`services/timer/**` 整分支零改动。

**合并前处理掉的（最终修复波，两个提交）**

| 项 | 内容 | 反向验证 |
| --- | --- | --- |
| I1 | 计划点名的「项目 × 情境」用例原先抓不住「情境子句被丢弃」（没有候选只由该子句排除） | 短路 EXISTS ⇒ 交集断言处红（`["t1","t9"]` vs `["t1"]`） |
| I2 | `projects.rs`/`tags.rs` 的零变化基线拿被测仓储当 oracle、缺行数守卫（T5 的同类修复未传播） | 快照恒空 ⇒ 行数断言红（0 vs 1） |
| I3 | `envelope.rs` 的「调用方清单」与事实相反（构造函数生产零调用） | 文档订正；构造函数保留（P7 是第一个生产构造者） |
| M2 | `NotInThisVersion` 印出内部状态名 + 禁用词表缺 3 个词 | 改回原样 ⇒ `不得漏出内部标识 "Scheduled"` 变红 |
| M4/M5/M6/M7 | 两处补零变化断言；状态循环补 `Done`；storage 规则补 `services::`；悬空引用订正 | 塞 `use crate::services::…` ⇒ LEAK + exit 1 |

**triage：明确留给后续（不阻断 P4）**

1. **文案一致性小任务（建议 P7 接线前做，约 1 小时）**：`EmptyText` 的列名（`"task.title"`/`"task"`/
   `"session"`/`"interval"`）、`guards.rs` 的 `no such {table}`、`coordinator.rs` 的 `"no such session"`；
   同批处理 `create_task` 对 `done` 项目放行（`task_repo.rs` 的 `Some(_) => {}`）与
   `require_active_project` 把 `done` 说成「已归档」。
2. **口径已定、代码待补全（P7 前门禁）**：所有对外写结果必须回 `data_epoch`/`revision`（P2 已回、P4 只回 `revision`，尚待补全）；
   同模块读路径两种 epoch 契约（一致性读带、单语句读不带）；同一列两种入参形状
   （`create_task` 的 `Option<&str>` vs `set_task_project` 的 `ProjectTarget`）。
3. **零调用公开面**：`guard_row_version_of`（建议直接删，而不是翻译它的英文）；`AuthorityTarget::new`、
   `LocalDate::{year,month,day}`、`parse_project_status`、`system_timezone_name` 保留并登记「P4 内无生产消费者」。
4. **`daily_plan` 的 `(local_date, timezone)` 读索引**（需新迁移）与**容器拆分**
   （`services/catalog.rs` 已 509 行、四类职责，P7 再加任务命令时拆 `services/tasks.rs`）。
5. **`WriteEnvelope::for_create` 的改名**（它在 4 处集合操作上语义错位）——P7 定 IPC 形状时一起做。

## 文档对齐后的兼容收尾

P4 核心验收结论保留；完整响应信封、完整项目列表服务以及 done 项目检查一致性是 P7 接线前待完成的接口收尾，不标为已实现。（其中 **done 项目检查一致性已于 2026-10-04 修复并提交 `9e7a89a`**，见下方「2026-10-04 复审更新」；完整响应信封与完整项目列表服务仍未完成。）统一契约、验收要求及其它阶段归属见[总纲 §10](../superpowers/plans/2026-10-03-v01-plan-index.md)。

## 2026-10-04 复审更新

历史记录中的 done 项目检查不一致现已修复并提交：COMP-02，提交 **9e7a89a**（其后追加「无法识别的 `project.status` 按列报错」回归用例，提交 **2fda0e8**），门禁 `cargo test --offline` 341 passed / 0 failed，`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`scripts/check-layers.ps1` 全绿。

事实口径（三处都不再冒充“已归档”，但新建/绑定与计时用的不是同一句话）：新建任务归属、重新绑定、start/resume 都只放行 active；新建/绑定走 `DomainError::NotInThisVersion`，渲染成“当前版本还没有「把任务关联到已完成的项目」这项功能。”；计时（start 与 resume 共用 `require_active_project`）渲染成“项目已完成，不能开始或继续计时。”。项目状态判定统一改走 `project_repo::get_project` 的 `ProjectStatus` 领域枚举，裸字符串加 `_` 兜底已删除：无法识别的 `project.status` 取值由 `project_repo` 按列报错，不再被说成“已完成”或“本版本不支持”。

回归用例 `completed_project_rejects_capture_binding_start_and_resume_without_changes` 覆盖新建、绑定、启动、恢复四条路径，断言 `revision`、`project`/`task` 表行数与字段、`task_change` 计数，外加启动的 `work_session`/`work_interval`/`interval_checkpoint` 三个计数与恢复的 session/interval 全行、`time_edit` 计数。上文 triage 第 1 条点名的 `create_task` 对 done 放行（`Some(_) => {}`）与 `require_active_project` 把 done 说成“已归档”两项，已在本轮处理完毕。

上面那句「无法识别的 `project.status` 取值按列报错」原本没有用例，2026-10-04 已补一条（`src-tauri/tests/projects.rs`，提交 **2fda0e8**）：`an_unknown_project_status_fails_by_column_for_capture_and_start_without_changes`。它用 `PRAGMA ignore_check_constraints` 把 CHECK 挡不住的 `project.status='paused'` 写进库（连接级开关，写完即关回），再验证 `catalog::create_task` 的新建归属与 `Coordinator::start` 计时两处都返回 `STORAGE_ERROR`，`detail()` 同时含列名 `project.status` 与脏值，且两处 `revision`、`project`/`task` 行数字段、`task_change` 计数均不变，启动那次另断三个计时表计数不变。测试总数随之 340 → 341。

错误权威返回顺序注释亦已订正（FOLLOW-02，同一提交 9e7a89a，仅注释、返回顺序未变）。此前“未修”描述保留为当时证据，以本节和兼容待评审清单的最新状态为准。完整响应信封（COMP-01）与完整项目列表服务（COMP-03）仍未实现，不能视为 P7 门禁全部通过。
