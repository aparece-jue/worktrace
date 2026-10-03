# P4 · 项目、标签与今日计划实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 V0.1 的三组辅助实体做成可用服务：项目（建/重命名/归档，归档后从新建任务的选择列表消失）、四类标签与任务打标、按用户时区记录的今日选择列表。

**Architecture:** `domain/` 放纯校验（`ProjectStatus`、`TagKind`、`LocalDate`），`storage/` 放仓储并一律接受 `&Transaction`，`services/catalog.rs` 与 `services/daily_plan.rs` 拥有事务并负责 epoch/版本校验。本计划可在 P1 之后独立实施，不需要等 P2。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· **时区库已选定并离线实测：`jiff 0.2`（必须用非默认特性，见文末「开工前已核实」）**

**Spec:** `../specs/2026-10-02-worktrace-architecture/02-data-model.zh.md` §2/§9 · `04-functional-spec.zh.md` F-002/F-004/F-005/F-010 · `05-roadmap.zh.md` §1

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。

状态：**已按 P1/P2 真实签名复核（见文末「开工前已核实」），时区库已选定并离线实测**；实施未开始。依赖：[P1](2026-10-03-worktrace-v01-foundation.md)。上游：[总纲](2026-10-03-v01-plan-index.md)、02/04/05。覆盖 F-002 项目/标签、F-004/F-005、F-010 今日选择；不做权重、Knowledge/层级、排期、UI。

## Task 1：领域输入和时区校验

文件：domain/project.rs、domain/tag.rs、domain/localdate.rs、services/catalog.rs、services/daily_plan.rs、services/mod.rs、tests/catalog_validation.rs。

- [ ] ProjectStatus 支持 active/archived/done，与 02 一致；本版 UI 只提供创建、重命名和归档。TagKind 只有 Domain/Activity/Context/Report，拒绝 Knowledge 与非空 parent_id。
- [ ] LocalDate::parse 校验真实 YYYY-MM-DD，含闰年。标签名称去首尾空白，同 kind 大小写敏感唯一。
- [ ] Rust 服务校验 IANA 时区及 UTC，不能把任意无空白字符串当合法时区；依赖行固定为
  `jiff = { version = "0.2", default-features = false, features = ["std", "tz-system", "tzdb-bundle-platform"] }`
  ——**默认特性在本机离线环境下构建不了**（会拉 `jiff-static`，缓存里没有），已实测并把证据写在「开工前已核实」。
- [ ] 时区别名处理使用一致策略：若规范化则所有读写同样转换；用户更换时区不静默移动旧计划。日期按所选时区计算，不取 updated_at。
- [ ] 测试：闰年/不存在日期、未知时区 Mars/Olympus、UTC/上海/纽约、空输入、非法标签 kind/parent，发布环境离线校验。

## Task 2：项目仓储与服务

文件：storage/project_repo.rs、services/catalog.rs、tests/projects.rs。

- [ ] 仓储提供 get_project/list_projects、create_project(tx, input)、rename_project(tx, id, expected_version, name, now)、archive_project(tx, id, expected_version, now)；&Transaction 写入不自行提交或增加 revision。
- [ ] 服务入口带 expected_data_epoch，编辑带 project.row_version；同事务校验、写入、版本/revision 变化并返回提交信封。实际值没有变化则不增加版本/revision。
- [ ] 新建任务选择列表只列 active；归档保留既有任务/历史。新关联归档项目及在归档项目启动计时均拒绝，检查在事务中执行，不只依赖 UI 过滤。
- [ ] 测试：重命名、归档保留历史、归档选择过滤、旧 epoch/version、归档与创建/启动竞态、故障回滚。

## Task 3：标签及幂等关联

文件：storage/tag_repo.rs、services/catalog.rs、tests/tags.rs。

- [ ] 仓储 create_tag/get_tag/list_tags/tags_of_task；同 kind 内重复名拒绝，跨 kind 可同名。新建 tag 保存 row_version，未来重命名使用版本校验；首版不提供未规划的删除/层级接口。
- [ ] tag_task(tx, task_id, tag_id)/untag_task(tx, task_id, tag_id) 返回是否真正改变集合。服务校验 epoch 及实体存在，重复增删幂等，无变化不加 revision；首次关联 weight 恒 null。
- [ ] 关系增删是显式集合操作，不冒充整个任务表单覆盖；若以后批量替换标签集合，必须加关联集合版本契约。
- [ ] 测试：四类标签、多标签、同类重名/跨类同名、重复增删无 revision、未知实体、非法权重/层级请求、失败不留半个关联。

## Task 4：今日选择列表

文件：storage/daily_plan_repo.rs、services/daily_plan.rs、tests/daily_plan.rs。

- [ ] 仓储 add_to_plan(tx, task_id, date, timezone)/remove_from_plan(tx, ...)/plan_for(conn, date, timezone)；所有写入只操作调用方事务。
- [ ] 服务校验 epoch、真实日期、有效时区和任务存在。复合键仍为 task/date/timezone，重复增删不加 revision。按 task.created_at/id 稳定排序。
- [ ] 计划表示某日人工选择，不自动设置 Scheduled、不建 time_block、不启动计时。完成项可以保留在当天历史列表，由 UI 显示状态；不因任务更新日期自动搬移。
- [ ] 测试：日期/时区隔离、跨日、同创建时刻稳定排序、重复增删、任务不存在、改时区旧项保留、旧 epoch 拒绝、事务失败回滚。

## 验收与下游接口


- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [ ] src-tauri 下 cargo fmt --check、cargo test、cargo clippy --all-targets 通过，执行 P1 分层检查。
- [ ] P1/P2 测试无回归；P4 可在 P1 后独立实施，不需要等 P2。
- [ ] 登记服务输入/输出及真实 Rust 签名供 P5/P7 消费；项目/标签版本字段与迁移一致。实际 UI 选择和发布应用离线验收由 P7 完成，不能将仓储测试标为 UI 已验收。

服务拥有事务并返回 epoch/revision；仓储只依赖 domain/shared error，不依赖 commands/platform。版本拒绝和任一步失败不得写审计、增加 revision 或留下部分变更。

## Task 5：任务筛选查询（轻量 GTD 列表）

文件：storage/task_repo.rs、services/catalog.rs、tests/task_filters.rs。

- [ ] 提供 list_tasks_filtered(filter, page) 查询：filter 包含 status 集合、project_id 与 context_tag_id；未选择条件不限制，多条件取交集。project_id 区分“不限制”和“无项目”，不把二者共用 null。
- [ ] context_tag_id 必须是 Context 标签；使用 EXISTS 或去重查询，多个标签关联不能重复返回任务或重复计数。参数绑定，不拼接用户输入 SQL。
- [ ] 稳定排序 created_at/id，首版分页 limit/offset，limit 范围 1..100、offset 非负。数据、total、epoch/revision 在同一读事务取得；查询不增加 revision。
- [ ] 服务提供捕获任务的创建/查询入口供 P7 消费，复用 P1 仓储，拥有事务及 epoch 校验；新建只增加一次 revision。
- [ ] 测试：Ready/Waiting/Blocked、项目与情境组合、无项目、多标签去重、分页同时间戳稳定排序、非法情境/分页输入、空结果及只读 revision。
- [ ] **收掉 P1 遗留的 `task_repo::list_tasks`**：P2 计划第 61 条写明「零调用的 `list_tasks` 是留给 P4 的」，而本计划的筛选查询是新增 `list_tasks_filtered`。二选一并写清：**(a)** 让 `list_tasks_filtered` 在 filter 为空时覆盖它的语义，同时**删掉** `list_tasks`（无过滤的整表列表在 V0.1 没有消费者，留着就是死 API）；**(b)** 保留 `list_tasks` 并在此登记它的实际调用方。不允许两者并存却都不被调用——那正好是 P1 收尾时要清的账。

下一步行动首版仅指 Ready 任务，不自动生成或拆分任务。Project 是多步工作容器，普通叶子 Task 可直接表示行动；不新增 Action 表或 Inbox 转项目命令。

- [ ] catalog 的 clarify_ready(request) 校验 task 版本并复用 P1 跃迁原语，单事务写 task_change/revision；仅允许无运行会话的 Inbox/Clarifying → Ready。其它状态编排仍归 P3，不通过该入口绕过联动规则。

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
`services/{catalog,daily_plan}.rs`、`storage/{project,tag,daily_plan}_repo.rs`）**确实都还不存在**，无重名冲突。

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
