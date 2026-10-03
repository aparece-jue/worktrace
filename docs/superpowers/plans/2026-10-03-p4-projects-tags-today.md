# P4 · 项目、标签与今日计划实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** 把 V0.1 的三组辅助实体做成可用服务：项目（建/重命名/归档，归档后从新建任务的选择列表消失）、四类标签与任务打标、按用户时区记录的今日选择列表。

**Architecture:** `domain/` 放纯校验（`ProjectStatus`、`TagKind`、`LocalDate`），`storage/` 放仓储并一律接受 `&Transaction`，`services/catalog.rs` 与 `services/daily_plan.rs` 拥有事务并负责 epoch/版本校验。本计划可在 P1 之后独立实施，不需要等 P2。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· **时区库已选定并离线实测：`jiff 0.2`（必须用非默认特性，见文末「开工前已核实」）**

**Spec:** `../specs/2026-10-02-worktrace-architecture/02-data-model.zh.md` §2/§9 · `04-functional-spec.zh.md` F-002/F-004/F-005/F-010 · `05-roadmap.zh.md` §1

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。

状态：**已实施并验收（2026-10-03）**。本轮按 P1/P2 真实签名复核（见文末「开工前已核实」），时区库离线实测选定 `jiff 0.2`（非默认特性）；6 个任务 + 各自评审修复轮全部落地，门禁 339 测试与四项检查全绿。验收核对见 `docs/validation/p4-acceptance.md`。依赖：[P1](2026-10-03-worktrace-v01-foundation.md)。上游：[总纲](2026-10-03-v01-plan-index.md)、02/04/05。覆盖 F-002 项目/标签、F-004/F-005、F-010 今日选择；不做权重、Knowledge/层级、排期、UI。

## Task 1：领域输入和时区校验

文件：domain/project.rs、domain/tag.rs、domain/localdate.rs、services/catalog.rs、services/daily_plan.rs、services/mod.rs、tests/catalog_validation.rs。

- [x] ProjectStatus 支持 active/archived/done，与 02 一致；本版 UI 只提供创建、重命名和归档。TagKind 只有 Domain/Activity/Context/Report，拒绝 Knowledge 与非空 parent_id。
- [x] LocalDate::parse 校验真实 YYYY-MM-DD，含闰年。标签名称去首尾空白，同 kind 大小写敏感唯一。
- [x] Rust 服务校验 IANA 时区及 UTC，不能把任意无空白字符串当合法时区；依赖行固定为
  `jiff = { version = "0.2", default-features = false, features = ["std", "tz-system", "tzdb-bundle-platform"] }`
  ——**默认特性在本机离线环境下构建不了**（会拉 `jiff-static`，缓存里没有），已实测并把证据写在「开工前已核实」。
- [x] 时区别名处理使用一致策略：若规范化则所有读写同样转换；用户更换时区不静默移动旧计划。日期按所选时区计算，不取 updated_at。
- [x] 测试：闰年/不存在日期、未知时区 Mars/Olympus、UTC/上海/纽约、空输入、非法标签 kind/parent，发布环境离线校验。

## Task 2：项目仓储与服务

文件：storage/project_repo.rs、services/catalog.rs、tests/projects.rs。

- [x] 仓储提供 get_project/list_projects、create_project(tx, input)、rename_project(tx, id, expected_version, name, now)、archive_project(tx, id, expected_version, now)；&Transaction 写入不自行提交或增加 revision。
- [x] 服务入口带 expected_data_epoch，编辑带 project.row_version；同事务校验、写入、版本/revision 变化并返回提交结果（当前仅 revision，完整 epoch 信封待总纲 §10 收尾）。实际值没有变化则不增加版本/revision。
- [x] 新建任务选择列表只列 active；归档保留既有任务/历史。新关联归档项目及在归档项目启动计时均拒绝，检查在事务中执行，不只依赖 UI 过滤。
- [x] 测试：重命名、归档保留历史、归档选择过滤、旧 epoch/version、归档与创建/启动竞态、故障回滚。

## Task 3：标签及幂等关联

文件：storage/tag_repo.rs、services/catalog.rs、tests/tags.rs。

- [x] 仓储 create_tag/get_tag/list_tags/tags_of_task；同 kind 内重复名拒绝，跨 kind 可同名。新建 tag 保存 row_version，未来重命名使用版本校验；首版不提供未规划的删除/层级接口。
- [x] tag_task(tx, task_id, tag_id)/untag_task(tx, task_id, tag_id) 返回是否真正改变集合。服务校验 epoch 及实体存在，重复增删幂等，无变化不加 revision；首次关联 weight 恒 null。
- [x] 关系增删是显式集合操作，不冒充整个任务表单覆盖；若以后批量替换标签集合，必须加关联集合版本契约。
- [x] 测试：四类标签、多标签、同类重名/跨类同名、重复增删无 revision、未知实体、非法权重/层级请求、失败不留半个关联。

## Task 4：今日选择列表

文件：storage/daily_plan_repo.rs、services/daily_plan.rs、tests/daily_plan.rs。

- [x] 仓储 add_to_plan(tx, task_id, date, timezone)/remove_from_plan(tx, ...)/plan_for(conn, date, timezone)；所有写入只操作调用方事务。
- [x] 服务校验 epoch、真实日期、有效时区和任务存在。复合键仍为 task/date/timezone，重复增删不加 revision。按 task.created_at/id 稳定排序。
- [x] 计划表示某日人工选择，不自动设置 Scheduled、不建 time_block、不启动计时。完成项可以保留在当天历史列表，由 UI 显示状态；不因任务更新日期自动搬移。
- [x] 测试：日期/时区隔离、跨日、同创建时刻稳定排序、重复增删、任务不存在、改时区旧项保留、旧 epoch 拒绝、事务失败回滚。

## 验收与下游接口


- [x] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [x] src-tauri 下 cargo fmt --check、cargo test、cargo clippy --all-targets 通过，执行 P1 分层检查。
- [x] P1/P2 测试无回归；P4 可在 P1 后独立实施，不需要等 P2。
- [x] 登记服务输入/输出及真实 Rust 签名供 P5/P7 消费；项目/标签版本字段与迁移一致。实际 UI 选择和发布应用离线验收由 P7 完成，不能将仓储测试标为 UI 已验收。
- [x] **P7 接线前补齐接口兼容（总纲 §10）**（**已完成**，2026-10-04，提交 **27f8a7a**，门禁 350 passed / 0 failed）：
  ① 写结果 DTO 补 `data_epoch` —— `services/tx.rs` 的 `settle` 改为返回 `WriteOutcome<(T, Settled{revision, data_epoch})>`，
  `Changed` 先 `bump_revision`、两个分支都在同一写事务里用 `require_meta` 读回；11 个写入口（catalog 9 + daily_plan 2）
  的 Change DTO（`ProjectChange`/`TaskProjectChange`/`TagChange`/`TaskTagsChange`/`TaskChange`/`DailyPlanChange`）全部带 `data_epoch`；
  ② 项目/标签/任务标签读服务补同一读事务的 epoch/revision —— `list_projects`/`list_selectable_projects`/`list_tags`/`tags_of_task`
  都收请求带来的 `expected_data_epoch`，在读事务内 `guard_epoch`，返回新增信封 `ProjectList`/`TagList` 的
  `{items, data_epoch, revision}`（`list_tasks_filtered` 与 `plan_for` 已是这个形状，未改；仓储签名与实现未动）；
  ③ 补完整项目列表服务 —— `catalog::list_projects(db, expected_data_epoch, Option<ProjectStatus>)`，`None` 含 archived/done、
  `Some(s)` 只列该状态，`list_selectable_projects` 复用同一路径且仍只含 active，命令层不再直调 `project_repo`（原引导注释已删）。
  用例共 9 条（全仓 341 → 350）：`projects.rs::write_results_carry_the_request_epoch_and_the_authoritative_revision`、
  `tags.rs::tag_write_results_carry_the_request_epoch_and_the_authoritative_revision`、
  `daily_plan.rs::plan_write_results_carry_the_request_epoch_and_the_authoritative_revision`；四个读信封用例
  （`projects.rs::list_projects_returns_a_same_read_transaction_envelope`、
  `projects.rs::the_selection_list_returns_a_same_read_transaction_envelope_with_active_projects_only`、
  `tags.rs::list_tags_returns_a_same_read_transaction_envelope`、`tags.rs::tags_of_task_returns_a_same_read_transaction_envelope`）；
  `projects.rs::the_full_project_list_covers_every_status_while_the_selection_entry_stays_active_only`；
  `projects.rs::an_unknown_project_status_fails_by_column_for_binding_without_changes`。
  跟踪编号见 [P1～P4 结果兼容性待评审清单](../../validation/p1-p4-review-backlog.md) 的 COMP-01 / COMP-03（两行已标已修复）。

目标对外响应必须包含 epoch/revision：本计划的这一条**已兑现**（写 DTO 与项目/标签读服务；P2 的计时快照此前已满足）。P7 的 IPC 接线（命令层构造信封、接线 `capture_error_response`、迟到响应丢弃）仍不在本计划范围，见总纲 §10 的门禁第 4 项。服务拥有事务；仓储只依赖 domain/shared error，不依赖 commands/platform。版本拒绝和任一步失败不得写审计、增加 revision 或留下部分变更。

## Task 5：任务筛选查询（轻量 GTD 列表）

文件：storage/task_repo.rs、services/catalog.rs、tests/task_filters.rs。

- [x] 提供 list_tasks_filtered(filter, page) 查询：filter 包含 status 集合、project_id 与 context_tag_id；未选择条件不限制，多条件取交集。project_id 区分“不限制”和“无项目”，不把二者共用 null。
- [x] context_tag_id 必须是 Context 标签；使用 EXISTS 或去重查询，多个标签关联不能重复返回任务或重复计数。参数绑定，不拼接用户输入 SQL。
- [x] 稳定排序 created_at/id，首版分页 limit/offset，limit 范围 1..100、offset 非负。数据、total、epoch/revision 在同一读事务取得；查询不增加 revision。
- [x] 服务提供捕获任务的创建/查询入口供 P7 消费，复用 P1 仓储，拥有事务及 epoch 校验；新建只增加一次 revision。
- [x] 测试：Ready/Waiting/Blocked、项目与情境组合、无项目、多标签去重、分页同时间戳稳定排序、非法情境/分页输入、空结果及只读 revision。
- [x] **收掉 P1 遗留的 `task_repo::list_tasks`**：P2 计划第 61 条写明「零调用的 `list_tasks` 是留给 P4 的」，而本计划的筛选查询是新增 `list_tasks_filtered`。二选一并写清：**(a)** 让 `list_tasks_filtered` 在 filter 为空时覆盖它的语义，同时**删掉** `list_tasks`（无过滤的整表列表在 V0.1 没有消费者，留着就是死 API）；**(b)** 保留 `list_tasks` 并在此登记它的实际调用方。不允许两者并存却都不被调用——那正好是 P1 收尾时要清的账。

下一步行动首版仅指 Ready 任务，不自动生成或拆分任务。Project 是多步工作容器，普通叶子 Task 可直接表示行动；不新增 Action 表或 Inbox 转项目命令。

- [x] catalog 的 clarify_ready(request) 校验 task 版本并复用 P1 跃迁原语，单事务写 task_change/revision；仅允许无运行会话的 Inbox/Clarifying → Ready。其它状态编排仍归 P3，不通过该入口绕过联动规则。

## 开工前已核实（2026-10-03，按 P1/P2 真实代码）

**一、符号与表核对通过。** 计划引用的四张表与 P1 已建结构逐字一致：

| 表 | 已核实要点 |
| --- | --- |
| `project` | `status IN ('active','archived','done')`、`row_version`、`name` 非空 trim 约束 |
| `tag` | `kind IN ('Domain','Activity','Context','Report')`、`row_version`、`ck_tag_parent_kind CHECK (parent_id IS NULL)`（Knowledge/层级天然被结构挡住） |
| `task_tag` | 主键 `(task_id, tag_id)`（幂等增删的依据）、`weight` 可空且限 0..1 |
| `daily_plan` | 主键 `(task_id, local_date, timezone)`、`local_date` 有 GLOB 形状约束 |

既有符号均存在：`task_repo::{get_task, list_tasks, transition_task, require_active_project}`、
`session_repo::running_foreground`、`guards::guard_epoch`、`meta::{require_meta, bump_revision}`、
`task_change` 表、`TransitionCause::{User, Reopen}`。计划要新建的 8 个文件（`domain/{project,tag,localdate}.rs`、
`services/{catalog,daily_plan}.rs`、`storage/{project,tag,daily_plan}_repo.rs`）**当时都不存在**，无重名冲突；现已完成实现。本节为开工历史证据，不是当前符号清单。

**二、时区库已选定并实测（原本是「待选型」，会挡住 Task 1）。** 本机 cargo 注册表缓存 324 个
crate，其中 `jiff-0.2.37`、`jiff-core`、`jiff-tzdb`、`jiff-tzdb-platform`、`windows-link` 都在，
**`jiff-static` 不在**。三个特性组合在临时 crate 上实测（`cargo run --offline`）：

| 组合 | 结果 |
| --- | --- |
| `jiff = "0.2"`（默认特性） | **失败**：`failed to download jiff-static v0.2.37 ... --offline was specified` |
| `default-features = false, features = ["std","tzdb-concatenated"]` | 编译通过但**运行期解析时区失败**（exit 2） |
| `default-features = false, features = ["std","tzdb-bundle-platform"]` | 通过：UTC / Asia/Shanghai / America/New_York 全部解析 |
| `default-features = false, features = ["std","tz-system","tzdb-bundle-platform"]` | **通过（推荐行）**：三时区解析、`TimeZone::system()` 可读（本机 `Asia/Shanghai`）、`Mars/Olympus` 被拒、`2023-02-29` 被拒 |

```toml
jiff = { version = "0.2", default-features = false, features = ["std", "tz-system", "tzdb-bundle-platform"] }
```

`tzdb-bundle-platform` 把 IANA 库打进产物，正好满足「Windows 打包不依赖系统 tzdata」；
`tz-system` 让「今日」默认取机器时区。**默认特性在本机离线环境不可用**，不要写 `jiff = "0.2"`。

**三、离线约束。** 本项目的门禁一律 `cargo test --offline`。**引入任何未缓存的 crate 都需要一次
联网 `cargo fetch`**，那会破坏「离线可复现」这条口径——要加就先经用户确认；上面的推荐行不需要。

**四、两个零调用 `pub fn` 的归属。** `task_repo::list_tasks` 是 P2 计划明确留给 P4 的
（已在 Task 5 加条要求收掉）；`session_repo::get_interval(conn, id)` 目前没有任何计划点名，
而 P3 的 `correct(request)` 收 `interval_id`，正是它的消费者——不阻断，执行 P3 时登记真实签名即可。


## 本轮开工审查补全

- [x] 为已有捕获任务提供 set_task_project(request)：project 参数显式表示绑定某项目或解除关联；请求带 expected_data_epoch/task_expected_version。仓储接受调用方事务，服务在同一事务检查任务版本、目标项目存在且 active，更新 project_id/updated_at/task.row_version，写 task_change 并恰好增加一次 revision。相同关联不写审计、不增加版本/revision；拒绝非法状态或版本时零变化。
- [x] F-002 理清界面通过上述项目关联与既有标签服务完成可选分类，然后调用 clarify_ready；不要求全部操作合为一次事务，不引入强制表单。已捕获 Inbox/Clarifying 任务不必重新创建才能归项目。此最小关联入口仅用于无运行会话的 Inbox/Clarifying/Ready；其它任务属性编辑和状态联动继续归 P3，不能绕过其规则。
- [x] 测试项目关联/解除、同值幂等、未知/归档项目、旧版本/epoch、非法状态或运行占用、失败回滚；验证理清为 Ready 后筛选能找到该任务。
- [x] **与 V0.1 边界的区分（写清以免被读成越界）**：规格里「不做 Inbox 转项目及属性迁移」指的是把任务**转成**独立 Project 实体并搬运属性；给任务**指定/解除**项目是 F-002 原文「Inbox 可直接 Ready；**可选项目/标签**」要求的能力——此前计划缺这个入口，等于该条落不了地。实现时守住边界：只改 `task.project_id`（连同审计与版本），**不创建项目、不迁移属性、不改变任务状态**。
- [x] P4 返回错误上下文时沿用同一读事务捕获规则，增加明确的 project/tag 版本载荷与消费者；开工时 ErrorAuthority 仅有 task/session，当前已完成 records 扩展，不能拿 task 版本代替项目/标签版本。该扩展在 P4 实施中完成，P7 再统一接入 IPC。
  **形状（本轮定死，避免实现时各写一套）**：
  - 载荷是**受控实体种类**的列表，本阶段支持 `task` / `session` / `project` / `tag`；
    `kind` 必须是**枚举或白名单**，不能由客户端任意指定表名（它决定读哪张表，绝不能拼进 SQL）。
  - **响应按「被请求的目标」逐条返回**：`{kind, id, row_version: Option<i64>}`，
    **`row_version = null` 表示"已显式确认不存在"**（例如版本冲突后目标已被删除）。
    不接受「不在列表里」这种表示——它与「根本没请求」无法区分，前端就只能说"我没拿到"，
    说不出"确认不存在"。请求侧同样按目标逐条给出，请求与响应一一对应。
  - **顺序确定**：按 `kind` 白名单顺序、同 kind 内按请求顺序；用例与前端 diff 才稳定。
  - **缺失 ≠ 读取失败**：目标行不存在是**数据**（照上面的 `null` 表示）；
    而任何一次读取**报错**则整体不返回上下文（`authority = null`、`requires_handshake = true`）——
    「读取失败不返回部分上下文」这条语义不变。
  - **interval 没有独立 `row_version`**：P3 的历史修正返回**所属 session 的版本**，
    不新增区间版本列（与 P3 计划第 70 条一致）。
  - **已同步改的历史用例**：3 条原先直接断言 `authority.task/session` 的错误用例已改为 records 形状——
    `error_response_reports_committed_recovery_versions_without_sampling`、
    `error_response_unavailable_metadata_requires_handshake_and_redacts_detail`、
    `error_response_capture_during_open_transaction_degrades_to_handshake`。

当前结论仅表示可进入 P4 实施准备与开发，不表示 UI/平台验收完成。任务状态、时区校验及新关联入口仍须按本计划逐项实现和测试。


---

## 实施记录（2026-10-03 收口）

- **提交链**：Task 1 `55c99f7`/修复 `c6e8cc9` · Task 2 `a2ba883`/修复 `9d7cde3` ·
  Task 3 `662e7d6`/修复 `6e622b0` · Task 5 `1938b47`/修复 `b6b99bb`+`272887e` ·
  Task 4 `03efff6`/修复 `7c3826e` · Task 6 `7be2512`+`b5de78f`+`ee4b9f6`/修复 `00505c3`（未 push）。
- **门禁**：339 passed / 0 failed；`fmt --check`、`clippy -D warnings`、分层检查（本轮补强四条反向边）全绿。
- **本轮抓到的三类真缺陷**（都有反向验证）：① `assert_domain_error` 两条断言恒真（Task 1）；
  ② 零变化快照拿被测函数当 oracle（Task 5）；③ `error_contract` 的「文案是中文」断言落在 `message()` 上（Task 6）。
- **裁决要点**（详见 `../validation/p4-acceptance.md` 与 SDD ledger）：`WriteEnvelope` 搬到 crate 根
  （services 不得依赖 commands）并把该规则做成机器门禁；`list_tasks` 取计划选项 (a) 删除；
  `set_task_project` 只改 `project_id`，请求用 `ProjectTarget::{Bind,Clear}` 两态；
  错误上下文按「逐条请求 → 逐条返回、`null` = 已确认不存在」定死；
  `task_change` 承载标签/计划/字段三种 JSON 形状并集中登记（P5 取完成项必须按形状过滤）。
- **未做（按边界）**：UI/IPC 接线（P7）、统计与导出（P5）、平台故障验收（P6）、
  标签权重与层级、`time_block`、Goal/Milestone（V0.2）。
- **待用户裁定**：术语「上下文 vs 情境」；九条 P1/P2 遗留文案缺陷（P4 只修了其中由新入口暴露的两处）；
  是否推送本地提交（工作区另有一处用户自己的 `README.md` 改动未被任何提交 stage）。

## 当前兼容状态

跨阶段接口、错误载荷、启动归属及 P7 前待办统一见[总纲 §10](2026-10-03-v01-plan-index.md)。P1/P2/P4 核心已验收，P3 尚未实施；历史签名、测试数量和开工记录保留为当时证据，消费接口以当前源码及总纲为准。文档对齐不表示待办代码、IPC 或平台验证已经完成。
