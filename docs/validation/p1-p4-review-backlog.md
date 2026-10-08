# P1～P4 结果兼容性待评审清单

> 当前状态（2026-10-08）：本页按日期保留历史评审证据。P3/P5 服务现已交付，P7 核心与部分实机已交付；下文“P3 尚未实施”等只描述当时状态。当前兼容性与未完成事项以[跨阶段复审](cross-stage-review-2026-10-08.md)及各阶段验收为准，P6/P8 仍未实施。


记录日期：2026-10-03。代码核对基线：c981590；文档以当前工作区对齐版本为准。
本清单记录已观察到的问题及建议，不代表已修复或已完成验收。后续评审可以集中决定实施顺序。

## 一、需要补齐的实现问题

| 编号 | 优先级 | 问题与证据 | 影响 | 建议与验收要求 | 状态 |
| --- | --- | --- | --- | --- | --- |
| COMP-01 | P2 | P2 TimerSnapshot 带 data_epoch/revision；P4 catalog 的 ProjectChange、TaskProjectChange、TagChange、TaskTagsChange、TaskChange 及 DailyPlanChange 仅带 revision；list_selectable_projects/list_tags/tags_of_task 返回裸列表。见 src-tauri/src/services/catalog.rs、services/daily_plan.rs | 对外响应不能统一执行旧 epoch/旧 revision 丢弃协议；多窗口和恢复接线存在缺口。当前尚未接 IPC，不声称已经发生缓存错乱（写于 2026-10-03；P7 接线已于 2026-10-04 完成，见 FOLLOW-01） | 写结果的数据与 epoch/revision 在同一写事务取得，提交后返回；业务读结果在同一读事务取得数据和元数据。不得由 IPC 层提交后补读。明确查询请求的 epoch 校验及首次握手方式；覆盖一致快照、旧 epoch 拒绝及 P7 迟到响应丢弃 | 已修复（2026-10-04，提交 27f8a7a；门禁 350 passed / 0 failed）。查询请求的 epoch 校验已在服务侧统一（只吃请求带来的期望值）；**首次握手方式已由 00 §5 规则 1（`docs/superpowers/specs/2026-10-02-worktrace-architecture/00-architecture.zh.md:61`）与 P7 计划（`docs/superpowers/plans/2026-10-03-p7-shell-and-ui.md:108`/`:110`）定义**：窗口先监听并暂存通知、再拉含 epoch/revision 的一致快照；可见窗口至多每 30 秒校验 `get_revision`。P7 只负责**实现**它。回归验证见下方 |
| COMP-02 | P2 | task_repo::create_task 只拒绝 archived，Some(_) 放行 done；set_task_project 拒绝 done；require_active_project 拒绝所有非 active，却都提示“已归档”。见 src-tauri/src/storage/task_repo.rs | 可以创建归属 done 项目但随后无法计时的任务；新建与重新绑定规则不同，拒绝理由不准确。V0.1 无 done 创建入口，但 schema/读模型允许该状态 | 新建任务归属、重新绑定、start/resume 均仅允许 active；archived/done 历史可读。done 使用准确的中文拒绝理由。补新建、绑定、开始/恢复路径测试，拒绝时断言 revision、版本、审计和相关字段零变化 | 已修复（2026-10-04，提交 9e7a89a：**340 passed（当时）**；追加「无法识别 `project.status`」用例后 341，COMP-01/COMP-03 收尾后当前 350 passed / 0 failed），回归验证见下方 |
| COMP-03 | P2 | project_repo::list_projects 支持完整/按状态查询；catalog 只提供 active 的 list_selectable_projects，注释引导完整列表直接调用仓储。见 src-tauri/src/services/catalog.rs、storage/project_repo.rs | Projects 页查询归档历史缺少符合 commands→services 分层的服务入口 | 补完整项目列表服务及可选状态过滤，保留 active 选择入口，统一 COMP-01 响应信封；验证归档/done 历史可读、选择列表只含 active，命令层不直调仓储 | 已修复（2026-10-04，提交 27f8a7a；门禁 350 passed / 0 failed），回归验证见下方 |

优先级 P2 表示需在依赖功能接入前完成，不表示当前已出现数据损坏。三个问题属于既有交付的兼容收尾，不新增产品功能或实体。

## 二、文档对齐后的实施与核对事项

| 编号 | 事项 | 当前状态与后续归属 |
| --- | --- | --- |
| FOLLOW-01 | P7 统一接入 capture_error_response | **已接入**（P7 Task 1a，提交 `12f0453` + `695cd57`）：24/24 命令经 `src-tauri/src/commands/mod.rs` 的 `run_command`，失败在持锁、原事务结束后调 `capture_error_response`（`src-tauri/src/commands/mod.rs:141`）；两处**边界**：（a）计时族命令提交后重建失败会返回 `Err`，该笔已提交的写不发 `domain.changed`；（b）包装层的逐条 `targets` **无用例**（见 [`p7-acceptance`](p7-acceptance.md) §6.5 第 22 条与 [P7 计划](../superpowers/plans/2026-10-03-p7-shell-and-ui.md)）。其余口径不变：在原事务结束后、同一串行边界捕获；按 kind/id 匹配 records，不按请求下标；读取失败要求整份重新握手，不用 timer.snapshot 补错误版本 |
| FOLLOW-02 | error_response.rs 的返回顺序注释 | 文件开头**原写**“按同一份顺序逐条返回”（已于 2026-10-04 修正为按 kind 白名单分组、组内保持请求顺序）。实际实现符合 P4 计划；返回顺序未改变 |
| FOLLOW-03 | P3 扫描版本与审计规则 | 文档已明确：扫描查询零写；实际修改 session 状态/run_id/区间事实时修改对象版本，并在同一批事务增加一次 revision、记审计。paused 重绑定适用；recovering 保持原恢复归属直至 reconcile。P3 尚未实施，后续按真实扫描测试验证（2026-10-04 核：仍准确——源码里没有 `reconcile`/`correct`/`backfill`/`discard_session` 的生产实现；P7 Task 0 只加了启动**检测**门禁，不做恢复） |
| FOLLOW-04 | 中文错误提示一致性 | 已修复（2026-10-04，提交 **a8c4376**，复评 fix round 提交 **5181cd9**，门禁 **357 passed / 0 failed**）：`DomainError` 新增 `UnknownSession`/`UnknownInterval`（变体清单 26 → 28），`task_repo`/`session_repo`/`checkpoint_repo`/`coordinator` 的列名与英文文案全部改成中文领域变体，会话状态补 `zh_session_state`，零调用的 `guard_row_version_of` 删除；「用户可见文案必须是中文」已变成三条机器门禁（变体级 / 构造点级 / 禁用子串），复评 I1 又补上**变体清单的编译期证人**（穷尽 match）与三张映射表的 ALL 循环覆盖，共 7 处篡改反向验证。Storage.detail 仅诊断（保留英文），Domain.detail 会进入用户 message，不能混用 |
| FOLLOW-05 | 平台验收 | 正式系统事件、锁屏/休眠/改时、多窗口/托盘、备份恢复及跨机器容差验证由 P6/P7/P8 承接；自动测试不能替代实机验收。**登记处**：P7 的自动化半边见 [`p7-acceptance`](p7-acceptance.md) §5.1；只有实机能验的逐条清单（含「平台事件实机验收」）见同文件 §5.2 与文末「仍未达成 / 存疑」索引表（该表已写明**实现归 P6、实机结论归 P8**）；P6 见 [`2026-10-03-p6-platform-closure.md`](../superpowers/plans/2026-10-03-p6-platform-closure.md)（Task 2 的「正式 OS 事件接线」条目）、P8 见 [`2026-10-03-p8-stats-recovery-export-ui.md`](../superpowers/plans/2026-10-03-p8-stats-recovery-export-ui.md)（Task 5「V0.1 端到端人工验收」）。其中 P7 的 IPC/前端接线已完成（见 FOLLOW-01），**实机验收仍未做** |
| FOLLOW-06 | P2 提交后重建的一致读 | 已修复（2026-10-04）：rebuild_from_committed 在提交后开启一个读事务，session、区间、前台会话、快照元数据和 task_version 均在同一读快照内取得；CommandOutcome.revision 复用 snapshot.revision，不再另读。保留同次采样及提交后失败进入 RECOVERY_REQUIRED 的原规则。这是计时结果的明确例外：提交后应用内存/重建响应，不要求在业务写事务内生成最终展示快照。 |
| FOLLOW-07 | commands 分层门禁 | 已修复（2026-10-04）：check-layers.ps1 增加 src/commands 对 storage::、rusqlite、Connection 的检查；正常通过，并须执行反向注入验证。命令层继续只调用服务，不接受连接。（2026-10-04 核：仍准确；今天 `check-layers.ps1` 共**六条**规则——commands/domain/storage/services/platform/入口点，后两条由 P7 Task 0 补上，见 [`p7-acceptance`](p7-acceptance.md) §3.1） |

## 三、已核对合理的边界（避免后续误当冲突）

- P4 clarify_ready 只做无运行会话的 Inbox/Clarifying→Ready；P2 start 可原子理清并启动。两条入口用途不同。
- 今日计划是人工选择集合，不自动改变任务状态、不建立排期、不启动计时。
- P2 finish 只结束会话；P3 transition_task 才负责任务完成/取消及会话原子联动。
- 标签/今日计划增删是 epoch-only 显式集合操作；不增加 task.row_version，真实变化增加全局 revision 并写审计；重复增删无变化。
- P3 尚未实施，恢复确认、历史修正、补录及任务完成联动不能按“已交付”评审（2026-10-04 核：仍有效）。

## 四、证据与范围

此前全仓 339 个测试、Clippy、格式、分层和 diff 检查通过；这证明既有核心测试通过，不证明上述待办已完成。此次仅记录问题，未修改 Rust 实现，未重新运行代码测试。

统一契约见[总纲 §10](../superpowers/plans/2026-10-03-v01-plan-index.md)；P4 现有验收及历史清理项见[P4 验收记录](p4-acceptance.md)。本清单不替代计划复选框，修复后须同步状态、测试证据与消费者。

## 2026-10-04 复审修复

核对基线 a3f9f80 为文档收口，未覆盖既有核心实现；本轮代码与测试提交为 **9e7a89a**（`src-tauri/src/storage/task_repo.rs`、`src-tauri/tests/projects.rs`、`src-tauri/src/services/error_response.rs`）。

COMP-02 已修复。**三处都不再冒充「已归档」，但新建/绑定与计时用的不是同一句话**：

- 新建任务归属 done 项目、把任务重新绑定到 done 项目：走 `DomainError::NotInThisVersion`，渲染成「当前版本还没有「把任务关联到已完成的项目」这项功能。」——`create_task` 与 `set_task_project` 逐字相同；
- start / resume 计时：走 `AppError::Domain`，渲染成「项目已完成，不能开始或继续计时。」——两者共用 `require_active_project` 的同一句文案。

状态判定统一改走 `project_repo::get_project` 的 `ProjectStatus` 领域枚举，裸字符串加 `_` 兜底的写法已删除：库里出现无法识别的 `project.status` 取值时，由 `project_repo` 按列报 `UnknownEnumValue`（「「project.status」里是一个无法识别的值 …」），不再被说成「已完成」或「本版本不支持」。归档（archived）的拒绝理由与文案不变；done 的两句文案沿用本轮之前的收口结果，本次只改了判定来源与「无法识别取值」的去向。

回归用例 `completed_project_rejects_capture_binding_start_and_resume_without_changes`（`src-tauri/tests/projects.rs`）覆盖四条路径：新建任务归属 done 项目、把任务重新绑定到 done 项目、对 done 项目上的任务 start、对 done 项目上的任务 resume。断言面：

- 四条路径都断言 `DOMAIN_ERROR` 且理由是具体中文（`assert_domain_error`，含 bind 段原先只断 `code()` 的那处）；
- 四条路径都调用 `assert_unchanged`：`revision` 不变、`project` 表行数与全部项目行字段逐字不变、**`task` 表行数不变**、目标任务行字段逐字不变、`task_change` 审计无新增；
- 新建、关联、启动三段额外负断言拒绝文案不含「已归档」（恢复与启动共用同一句计时文案）；
- 启动被拒额外断言 `work_session` / `work_interval` / `interval_checkpoint` 三个计数不变，不留半条记录；
- 恢复被拒额外逐字段比对 `session` 行与 `intervals_of_session` 全量结果，并断言 `time_edit` 审计计数不变。

本轮再补一条同主题用例 `an_unknown_project_status_fails_by_column_for_capture_and_start_without_changes`（`src-tauri/tests/projects.rs`，提交 **2fda0e8**），把上面第 49 行那句「按列报错」也钉住：用 `PRAGMA ignore_check_constraints` 把 CHECK 挡不住的 `project.status='paused'` 写进库（连接级开关，写完即关回 `OFF`），验证两个入口都走 `project_repo::get_project` 的同一分支——`catalog::create_task` 新建任务归属该项目、该项目下任务的 `Coordinator::start` 都返回 `STORAGE_ERROR`，`detail()` 同时含列名 `project.status` 与脏值；两处都调用 `assert_unchanged`（`revision`、`project`/`task` 表行数与字段、`task_change` 无新增），start 那次另断 `work_session` / `work_interval` / `interval_checkpoint` 三个计数不变。

FOLLOW-02 的返回顺序注释已修（`error_response.rs`，同一提交 9e7a89a），返回顺序未改变。

门禁数字与归属（三次实跑，不是同一个数）：**`9e7a89a`：340 passed（当时）**；**`2fda0e8`：341 passed（当时）**（相对 `a3f9f80` 共含两条新增回归用例——done 项目三处拒绝口径、无法识别的 `project.status` 按列报错，故 339 → 341）；**当前（COMP-01/COMP-03 收尾提交 `27f8a7a`）：350 passed / 0 failed**（本轮新增 9 条用例，见下）。三项检查（`cargo fmt --check`、`cargo clippy --all-targets --offline -- -D warnings`、`scripts/check-layers.ps1`）在各自提交上均全绿。

## 2026-10-04 COMP-01 / COMP-03 收尾

代码与测试提交 **27f8a7a**（`src-tauri/src/services/{tx,catalog,daily_plan}.rs`、`src-tauri/tests/{projects,tags,daily_plan}.rs`）。两项都不新增表/列/迁移，也不改 P2 的计时命令与 `TimerSnapshot`。

**COMP-01 写路径**：`services/tx.rs` 新增 `pub(super) struct Settled { revision, data_epoch }`，`settle` 改为返回 `WriteOutcome<(T, Settled)>`——`Changed` 分支先 `bump_revision`，再与 `Unchanged` 分支一样用**同一个写事务里的** `require_meta` 读回库身份（不做提交后补读）。11 个写入口的 Change DTO 全部补上 `pub data_epoch: String`：`ProjectChange`（新建/改名/归档）、`TaskProjectChange`（任务归属）、`TagChange`（新建标签）、`TaskTagsChange`（打标/去标）、`TaskChange`（捕获/理清）、`DailyPlanChange`（加入/移除今日计划）。

**COMP-01 读路径**：`catalog::list_projects` / `list_selectable_projects` / `list_tags` / `tags_of_task` 都改为收 `expected_data_epoch: &str`，在**同一个读事务内**用 `storage::guards::guard_epoch` 校验请求带来的期望值，并把数据与 `app_meta` 一起取回，返回新增的信封 `ProjectList { items, data_epoch, revision }` / `TagList { items, data_epoch, revision }`。`list_tasks_filtered`（catalog）与 `plan_for`（daily_plan）形状不变——它们是本形状的样板；`tags_of_task` 的内部写路径仍用 `tag_repo::tags_of_task(&tx, …)`。

**COMP-03**：新增 `catalog::list_projects(db, expected_data_epoch, Option<ProjectStatus>)`——`status = None` 不限制（归档与 done 的历史都在），`Some(s)` 只列该状态；仓储 `project_repo::list_projects(conn, Option<ProjectStatus>)` 的语义与实现一字未动，服务直接使用。`list_selectable_projects` 复用同一条读路径并仍**只含 active**。原注释里那句「要完整列表用 `storage::project_repo::list_projects(conn, None)`」已删除，改为指向本模块的 `list_projects`。

**覆盖用例（本轮新增 9 条，341 → 350）**：

- 写结果带权威 epoch/revision（含 `Unchanged` 分支）：`projects.rs::write_results_carry_the_request_epoch_and_the_authoritative_revision`（新建项目、任务绑定、捕获、理清、改名幂等）、`tags.rs::tag_write_results_carry_the_request_epoch_and_the_authoritative_revision`（建标签、打标、去标与两种幂等）、`daily_plan.rs::plan_write_results_carry_the_request_epoch_and_the_authoritative_revision`（加入、移除与两种幂等）；每条都断言 DTO 的 `data_epoch` 等于请求带的 epoch 且等于 `app_meta` 现值、`revision` 与 `app_meta.revision` 一致。**口径**：这让「返回值等于写当时库里的权威值」可观察；但 `guard_epoch` 通过之后，「回显请求带来的 epoch」与「同事务读 `app_meta`」原理上不可区分，断言分不出这两者——**「同一写事务」由结构保证**（只经 `services/tx.rs::settle` / 同事务里的 `require_meta`，`commit` 之后没有第二次读）。
- 四个读服务各一条信封用例：正常读断言 `items` + `data_epoch` + `revision` 与库里一致（每条另核对信封里的 `revision` 等于**读之前**记下的库版本），旧 epoch 拒绝（`DATA_EPOCH_MISMATCH`）且零变化（`revision`、项目/标签/关联/任务行数与字段、审计计数均不动）。**口径**：**「与 `items` 同一次读」也是由结构保证**（服务在一次 `unchecked_transaction` 内同时取数据与 `app_meta`），这批用例**证明不了**「没有在返回前补读一次元数据」——无并发写时两种实现给出同一个 revision；可观察的证伪方式见下方遗留登记。用例注释与断言文案已按此改正（评审 I-1）。
- COMP-03 语义：`the_full_project_list_covers_every_status_while_the_selection_entry_stays_active_only`——active + archived + done 三种项目都在 `list_projects(epoch, None)` 里（归档/done 历史可读），`Some(Archived)` / `Some(Done)` 只列该状态，`list_selectable_projects` 只含 active。
- 既有用例跟着改签名（`tests/projects.rs` **5 处**（`:383`/`:643`/`:655`/`:662`/`:1165`）、`tests/tags.rs` 的 `tags_of` 助手与 `list_tags` 各 1 处），断言只加强：原先直接调 `project_repo::list_projects(…, None)` 的那处历史可读断言改走服务入口，不再直调仓储。
- 另补一条小用例 `projects.rs::an_unknown_project_status_fails_by_column_for_binding_without_changes`（独立测试函数，未并入既有用例）：用 `PRAGMA ignore_check_constraints` 写入 CHECK 挡不住的 `project.status='paused'`（写完即关回，并直连 SQL 回读自证脏值确实落库），验证 `set_task_project` 的 Bind 分支与新建/计时一样按列报 `STORAGE_ERROR`（`detail()` 含列名与脏值）且零变化。

**门禁**（Windows，离线，提交 `27f8a7a` 上实跑）：`cargo test --offline` **350 passed / 0 failed**；`cargo fmt --check` EXIT 0；`cargo clippy --all-targets --offline -- -D warnings` 无告警；`scripts/check-layers.ps1` PASSED。**反向验证**：去掉 `list_projects` 的 `guard_epoch` ⇒ 两条读信封用例变红；把 `Settled::read` 的 `data_epoch` 换成常量 ⇒ 写结果用例变红（`left: "not-the-epoch"`）。

**首次握手时序已定，服务入口现已补齐**：00 §5 规则 1 与 P7 计划已经定义好形状——窗口**先监听并暂存通知，再拉**含 epoch/revision 的一致快照；可见窗口至多每 30 秒校验 `get_revision`（`00-architecture.zh.md:61`、`2026-10-03-p7-shell-and-ui.md:108`/`:110`）。本轮已统一服务侧的 epoch 校验口径（所有查询只接受**请求带来的**期望值，`guard_epoch` 在同一个读事务内跑），P7 负责实现上述握手与迟到响应丢弃。

**仍未做**：P7 的 IPC 接线（命令层构造 `WriteEnvelope`、接线 `capture_error_response`、迟到响应丢弃、先监听后拉的握手时序）与 UI 不在本轮范围。（2026-10-04 注：本轮之后 P7 已交付这些接线与 UI；实机验收归 P8。本行保留为当时口径。）

**本轮登记的新遗留**：

1. **「同一读/写事务」缺可观察证据**：现有断言只能证明「返回值等于当时的库值」，证伪「返回前补读」需要第二连接并发写 + 快照校验（`unchecked_transaction` 是 DEFERRED，要在另一连接提交后确认旧信封仍持旧 revision），约 1–2 小时，本轮不做；「同事务」目前由结构保证（四个读服务与 `settle` 的代码形状）。
2. **P2 计时快照是提交后重建**（FOLLOW-06）：严格按「写结果与元数据同一写事务」这条口径 P2 不满足，R-E 冻结范围，P7 接线前评估。

## 首次握手与 P2 快照收口（2026-10-04）

新增 services::handshake::get_revision(db) -> RevisionSnapshot {data_epoch,revision}，不需要 expected_data_epoch，纯读、不采样、不初始化库、不增加 revision。P7 先监听并缓冲事件，再调用此入口获得库身份，随后携该 epoch 拉业务一致快照；期间发生恢复则业务查询返回 DATA_EPOCH_MISMATCH，重新握手。get_revision 不是包含全部业务数据的快照，窗口仍需拉取所需业务视图，并保留事件缓冲与版本丢弃协议。周期校验与恢复后重新握手复用同一入口，生产 IPC 尚未接线。（2026-10-04 注：P7 Task 1a 已接线——24/24 命令经 `run_command`，失败映射在 `src-tauri/src/commands/mod.rs:141`；「尚未」只对写这一节时成立。）

历史 FOLLOW-06 所谓“提交后只读一次”不准确：原实现 build 和随后 require_meta 各读一次元数据，且实体分次读取。现改为一个提交后读事务，并直接复用 snapshot.revision。历史记录中的未完成说法以本节和最新表格为准。

本轮验证：352 个测试通过；分层正向通过，临时注入 commands 对 storage 的引用后门禁明确返回 1，移除探针后再次通过。首次握手用例覆盖无初始 epoch、恢复后库身份改变/旧查询拒绝、未初始化元数据拒绝且零创建。

## FOLLOW-04 收口（2026-10-04，提交 a8c4376）

**交付**：用户可见错误文案全部中文化，且从此由**测试门禁**守着，不再是约定。门禁数字：`cargo test --offline` **357 passed / 0 failed**、`cargo fmt --check` EXIT 0、`cargo clippy --all-targets --offline -- -D warnings` 无告警、`scripts/check-layers.ps1` PASSED（`a8c4376` 收口时为 355，复评 fix round `5181cd9` 补两条映射表覆盖用例后为 357）。

**改动的点位**（行号为提交后位置；`file:line` 均在 `src-tauri/` 下）：

1. `src/services/timer/coordinator.rs:195`：手写 `AppError::Domain { detail: "no such session" }` → `DomainError::UnknownSession`。P4 验收记录里登记的最后一条「整句英文进用户文案」。
2. `src/storage/task_repo.rs:75`：`EmptyText { field: "task.title" }` → 「任务标题」（字段字面量在 `:76`）。这是**校验**错误（标题为空），字段名会被拼进「「…」不能为空。」，所以照 `catalog.rs`/`domain/localdate.rs`/`tag_repo.rs` 的先例用中文，不用列名。
3. `src/storage/task_repo.rs:297` 与 `:442`：`EmptyText { field: "task" }` → `UnknownTask`。这两处语义是**找不到任务**，与 `:574` `set_task_project` 的注释口径（「『找不到』用 `UnknownTask`，不是 `EmptyText`」）统一。
4. `src/storage/session_repo.rs:182`/`:265`：`EmptyText { field: "session" }` → **新增** `DomainError::UnknownSession`；`src/storage/session_repo.rs:204`、`src/storage/checkpoint_repo.rs:70`：`EmptyText { field: "interval" }` → **新增** `DomainError::UnknownInterval`。两个变体照 `UnknownTask`/`UnknownProject`/`UnknownTag` 写（「找不到这个会话。」「找不到这段计时区间。」），`tests/error_contract.rs` 的变体清单同步 26 → 28。
5. `src/storage/session_repo.rs:145`：`NotInThisVersion { what: "stopwatch with a budget" }` → 「给正计时设预算」。
6. **顺带审计的两处发现（原清单之外，一并改）**：
   - `src/storage/checkpoint_repo.rs:88`/`:93`/`:103` 的三条不变量违反用的是 **`AppError::Domain`**（不是 `AppError::Storage`），detail 会原样进用户句子（用户会读到「操作不被允许：checkpoint must not move backwards.」）⇒ 改中文。`AppError::Storage` 的诊断（`task vanished after insert`、`expected journal_mode=wal…`、`app_meta is not initialised` 等）按契约**保留英文**，本次一字未动。
   - `DomainError::IntervalOpenInWrongState` 原先走 `zh_status`（**任务**状态映射表），而它的 `state` 来自 `SessionState::as_str()` ⇒ 五个会话状态一个都不在表里，整句漏成英文（「会话处于「recovering」时不该有开放的计时区间。」）⇒ 新增 `zh_session_state`（运行中/已暂停/待确认/已结束/已作废）；同时 `session_repo::close_interval` 里借用 `IllegalTransition { from: "closed", to: "closed" }` 的分支改回 `NoOpenInterval`（该分支语义本就是 NoOpenInterval 的注释所写「没有开放区间，却要求闭合」，原先那句会渲染成关于**任务**跃迁、且带内部标识的胡话）。
7. 删除零调用的 `storage::guards::guard_row_version_of`（`detail: format!("no such {table}")`，生产零调用、只剩测试用）。`tests/transaction_boundary.rs` 三处调用改用 `task_repo::get_task` + `guards::guard_row_version`：`start_session` 的请求校验用 `ok_or(DomainError::UnknownTask)`，「未知记录 = `DOMAIN_ERROR`」与「版本不符 = `VERSION_CONFLICT`」两个区分与断言强度都保留（后者仍断言 `expected/actual = (7, 1)`）。其余构造点逐个核对，**确认已是中文**：`services/catalog.rs:46/79/86/525`、`storage/project_repo.rs:96/123/203`、`storage/tag_repo.rs:112`、`domain/tag.rs:48/59`、`domain/project.rs:60`、`domain/localdate.rs:38`（`field: FIELD`，表达式）、`storage/task_repo.rs:91`、`domain/task.rs:113`（`what: to.as_str()`，表达式、经 `zh_status`，未动）。

**门禁（`src-tauri/tests/error_contract.rs`，三条规则分工）**：

- **变体级**：除 `UnknownEnumValue` 外，其余 `DomainError` 变体渲染出的用户文案不得含 ASCII 字母。豁免写在断言处：`UnknownEnumValue` 要回显**非法取值与列名**（「「state」里是一个无法识别的值 "???"。」），那是诊断所需；它的生产构造点（`services/daily_plan`）用的是中文列名「时区」。另加一条把 `SessionState` 五个取值逐一核对（映射表漏一个就红）。这条规则抓出的正是第 6 条那两处：①的「至少含一个 CJK」对「中文句子里夹一个英文词」恒真，抓不住。
- **构造点级**：扫 `src/**/*.rs`（`env!("CARGO_MANIFEST_DIR")` 定位），**内联字面量**形式的 `EmptyText { field: … }`、`NotInThisVersion { what: … }` 与 `AppError::Domain { detail: … }` 必须至少含一个 CJK 字符；表达式形式（`field: FIELD`、`what: to.as_str()`、`detail: other.to_string()`）跳过。只剔**整行注释**，注释里引用历史文案（如「原先借用 `EmptyText{field:"project"}`」）不算产出文案。
- **禁用子串**：`no such` / `task.title` / `stopwatch with a budget` / checkpoint 三句英文片段不得回到 `src`（表里没有 `vanished`——Storage 诊断按契约保留英文，那些片段由用户可见文本侧的禁用词表守）。同时把 `no such` / `vanished` / `task.title` / `stopwatch with a budget` 加进 `errors_never_leak_paths_sql_or_payload` 既有的禁用词表。

**反向验证**（三处篡改，逐字还原后与备份 SHA-256 一致：`error.rs` `56180f67…`、`task_repo.rs` `f9ac21fa…`、`checkpoint_repo.rs` `9158a1a9…`、`error_contract.rs` `743679c3…`）：

| 篡改 | 结果 | 红在哪条断言 |
| --- | --- | --- |
| `UnknownSession` 文案改成「找不到这个 session。」 | 1 failed | 变体级规则 ④：`error_contract.rs:311`「除 UnknownEnumValue 外的用户文案不得含 ASCII 字母：找不到这个 session。」——①的 CJK 断言放行，正是新规则补上的缺口 |
| `task_repo` 的 `field` 改回 `"task"` | 1 failed | 构造点级：`error_contract.rs:468` 点名 `src/storage/task_repo.rs:75`「必须含中文："task"」 |
| `checkpoint_repo` 的 detail 改回 `checkpoint must not move backwards.` | 2 failed | 禁用子串：`error_contract.rs:503` 点名 `checkpoint_repo.rs`；构造点级：`:468` 点名 `checkpoint_repo.rs:102` |

另有一次「整句英文」的对照篡改（`UnknownSession` → `"no such session."`）同时打红变体级 ①（CJK）与禁用子串两条，说明两道网都在工作。

**复评 fix round 1（2026-10-04，提交 `5181cd9`，门禁 357 passed / 0 failed）**：复评判「七处漏点全部真收口、两处清单外发现处理正确、4 处反向验证可信」，剩 1 条 Important（门禁完整性）与 3 条 Minor，已全部收掉——只改 `src-tauri/tests/error_contract.rs`，生产代码一字未动。

- **I1 变体清单的编译期证人**：`cases.len() == 28` 不会因为「加了新变体却没登记」而红（`Display` 有编译期强制，用例清单没有）。现在 `domain_error_variants!` 从一份标签清单展开出 `Variant`/`Variant::ALL`/`variant_of`，后者的 `match` **没有通配 arm** ⇒ 新增变体不补一行就**编译失败**（E0004）；用例清单 `representative_cases()` 仍是显式清单，由覆盖率断言与证人对齐——重复登记 ⇒ 第一条断言红，漏登记 ⇒ 第二条断言红并点名。三条「从真实跃迁入口造出来」的用例拆到 `entry_point_cases()` 接在后面跑。
- **Minor ①** 构造点级扫描改为**逐模式自证**（`EmptyText`/`NotInThisVersion`/`AppError::Domain` 各自 > 0；当前真实分布 10 / 6 / 26 写在注释里）。
- **Minor ②** `UnknownEnumValue` 的豁免理由写全：另有 6 处**英文列名**构造点（`task.status`/`project.status`/`work_session.mode|state|timer_kind`/`tag.kind`），它们安全的原因是经 `enum_error` → `FromSqlConversionFailure` → `map_sqlite` 降级成 **Storage** 的 detail、永不进用户文案，不是「列名是中文」。
- **Minor ③** 「映射表漏一个就红」变成真的：新增 `TaskStatus::ALL`（10 个取值 × 5 个携带状态名的变体）与 `TagKind::ALL`（4 个取值 × 2 个变体）两条 ALL 循环用例——此前 `Clarifying` 从未被任何用例渲染过。
- **反向验证**（逐字还原、SHA-256 对照）：新增变体不补证人 ⇒ 编译失败 `error[E0004]`（指向 `error_contract.rs:174` 的 match）；重复登记 ⇒「同一个变体在清单里登记了两次」（left 25 / right 26）；漏登记 ⇒「代表清单与变体清单不一致：缺 [UnknownTask]」。

**仍未做**（按边界）：P7 的 IPC 接线与前端时序、`capture_error_response` 接线不在本轮（FOLLOW-01）；`tests/` 里仍有两处**手造**的英文 detail 夹具（`error_contract.rs` 的 `"no such task"`/`"illegal transition"`，用途是证明两个 detail 可辨）与一句注释，扫描范围本就只覆盖 `src/**/*.rs`，未动。复评同意留后续、已在报告登记的 4 项：退役子串表是**文件级**（可能误伤将来合法的英文 Storage 诊断，如 `no such table`）；加进 message 侧禁用表的 4 个词在常量 fixture 下打不响（不是第二道网）；扫描器「token 后 400 字符取第一个 anchor」在「Domain detail 变表达式 + 紧邻 Storage 字面量」时会误报（今天不发生）；`IllegalTransition` 渲染仍写「任务不能从…」（用于区间时措辞不贴）。


## 2026-10-04 P3 开工契约复审（基线 4a96903）

- 已修复计划冲突：S5 产出有候选 ended_at、无 duration_ms 的待确认区间，原 S8 作废时只清 needs_review 会触发 P1 ck_interval_duration。现在无时长候选清 ended_at，原候选值在 time_edit.before_json 保留；已有可信时长区间不动端点。已同步 P3 S8/Task 2/Task 4、P8 和中英文 02 恢复表。
- 已修复 S12 签名缺口：AppState::retry_recovery 显式接收 expected_data_epoch，使正文要求的身份预检可实施；P2 内部原语不变。
- 新增 transaction_boundary::voiding_a_pending_candidate_requires_clearing_its_unknown_endpoint，验证零长度/非零候选的原操作触发准确 CHECK 且字段不变，修订操作可落库；有可信时长区间作废保留时长与端点。它验证已发布约束与计划兼容，不冒充 P3 服务实现或审计验收。
- 本轮 Rust 测试通过（存在一条既有 ignored 用例）；139 个前端测试通过；Clippy、fmt、分层和 diff 检查通过。P7 上轮身份/订阅代次修复仍保留，P3 服务与平台实机验收仍未完成。

## P3 执行前闭环（2026-10-04）

当前遗留已统一到 [pre-p3-closure](pre-p3-closure.md)，关联总纲/P3/P5/P6/P8；P7 的 42 条工程遗留逐项有明确裁定、责任和门槛。新增首份快照前通知、跨语言事件常量门禁，空状态查询改多状态夹具；源码注释与归一化 JSON 诊断同步。新增可重复的 check-pre-p3.ps1，日志记录当前 HEAD/工作区，实机验证固定未完成。P3 S1 新增提交后重扫失败闭环契约，生产实现仍归 P3。本页此前数字为历史证据，当前检查以收口页和脚本当次日志为准。
