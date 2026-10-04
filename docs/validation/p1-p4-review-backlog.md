# P1～P4 结果兼容性待评审清单

记录日期：2026-10-03。代码核对基线：c981590；文档以当前工作区对齐版本为准。
本清单记录已观察到的问题及建议，不代表已修复或已完成验收。后续评审可以集中决定实施顺序。

## 一、需要补齐的实现问题

| 编号 | 优先级 | 问题与证据 | 影响 | 建议与验收要求 | 状态 |
| --- | --- | --- | --- | --- | --- |
| COMP-01 | P2 | P2 TimerSnapshot 带 data_epoch/revision；P4 catalog 的 ProjectChange、TaskProjectChange、TagChange、TaskTagsChange、TaskChange 及 DailyPlanChange 仅带 revision；list_selectable_projects/list_tags/tags_of_task 返回裸列表。见 src-tauri/src/services/catalog.rs、services/daily_plan.rs | 对外响应不能统一执行旧 epoch/旧 revision 丢弃协议；多窗口和恢复接线存在缺口。当前尚未接 IPC，不声称已经发生缓存错乱 | 写结果的数据与 epoch/revision 在同一写事务取得，提交后返回；业务读结果在同一读事务取得数据和元数据。不得由 IPC 层提交后补读。明确查询请求的 epoch 校验及首次握手方式；覆盖一致快照、旧 epoch 拒绝及 P7 迟到响应丢弃 | 已修复（2026-10-04，提交 27f8a7a；门禁 350 passed / 0 failed）。查询请求的 epoch 校验已在服务侧统一（只吃请求带来的期望值）；**首次握手方式已由 00 §5 规则 1（`docs/superpowers/specs/2026-10-02-worktrace-architecture/00-architecture.zh.md:61`）与 P7 计划（`docs/superpowers/plans/2026-10-03-p7-shell-and-ui.md:48`/`:50`）定义**：窗口先监听并暂存通知、再拉含 epoch/revision 的一致快照；可见窗口至多每 30 秒校验 `get_revision`。P7 只负责**实现**它。回归验证见下方 |
| COMP-02 | P2 | task_repo::create_task 只拒绝 archived，Some(_) 放行 done；set_task_project 拒绝 done；require_active_project 拒绝所有非 active，却都提示“已归档”。见 src-tauri/src/storage/task_repo.rs | 可以创建归属 done 项目但随后无法计时的任务；新建与重新绑定规则不同，拒绝理由不准确。V0.1 无 done 创建入口，但 schema/读模型允许该状态 | 新建任务归属、重新绑定、start/resume 均仅允许 active；archived/done 历史可读。done 使用准确的中文拒绝理由。补新建、绑定、开始/恢复路径测试，拒绝时断言 revision、版本、审计和相关字段零变化 | 已修复（2026-10-04，提交 9e7a89a：**340 passed（当时）**；追加「无法识别 `project.status`」用例后 341，COMP-01/COMP-03 收尾后当前 350 passed / 0 failed），回归验证见下方 |
| COMP-03 | P2 | project_repo::list_projects 支持完整/按状态查询；catalog 只提供 active 的 list_selectable_projects，注释引导完整列表直接调用仓储。见 src-tauri/src/services/catalog.rs、storage/project_repo.rs | Projects 页查询归档历史缺少符合 commands→services 分层的服务入口 | 补完整项目列表服务及可选状态过滤，保留 active 选择入口，统一 COMP-01 响应信封；验证归档/done 历史可读、选择列表只含 active，命令层不直调仓储 | 已修复（2026-10-04，提交 27f8a7a；门禁 350 passed / 0 failed），回归验证见下方 |

优先级 P2 表示需在依赖功能接入前完成，不表示当前已出现数据损坏。三个问题属于既有交付的兼容收尾，不新增产品功能或实体。

## 二、文档对齐后的实施与核对事项

| 编号 | 事项 | 当前状态与后续归属 |
| --- | --- | --- |
| FOLLOW-01 | P7 统一接入 capture_error_response | 服务已实现，生产 IPC 尚未接入。在原事务结束后、同一串行边界捕获；按 kind/id 匹配 records，不按请求下标；读取失败要求整份重新握手，不用 timer.snapshot 补错误版本 |
| FOLLOW-02 | error_response.rs 的返回顺序注释 | 文件开头仍写“按同一份顺序逐条返回”，实际按 kind 白名单分组、组内保持请求顺序。实际实现符合 P4 计划；2026-10-04 已修正注释，返回顺序未改变 |
| FOLLOW-03 | P3 扫描版本与审计规则 | 文档已明确：扫描查询零写；实际修改 session 状态/run_id/区间事实时修改对象版本，并在同一批事务增加一次 revision、记审计。paused 重绑定适用；recovering 保持原恢复归属直至 reconcile。P3 尚未实施，后续按真实扫描测试验证 |
| FOLLOW-04 | 中文错误提示一致性 | P4 验收记录仍登记 P1/P2 遗留的内部列名、英文提示等；建议 P7 前集中清理。Storage.detail 仅诊断，Domain.detail 会进入用户 message，不能混用 |
| FOLLOW-05 | 平台验收 | 正式系统事件、锁屏/休眠/改时、多窗口/托盘、备份恢复及跨机器容差验证由 P6/P7/P8 承接；自动测试不能替代实机验收 |
| FOLLOW-06 | P2 提交后重建的一致读 | 已修复（2026-10-04）：rebuild_from_committed 在提交后开启一个读事务，session、区间、前台会话、快照元数据和 task_version 均在同一读快照内取得；CommandOutcome.revision 复用 snapshot.revision，不再另读。保留同次采样及提交后失败进入 RECOVERY_REQUIRED 的原规则。这是计时结果的明确例外：提交后应用内存/重建响应，不要求在业务写事务内生成最终展示快照。 |
| FOLLOW-07 | commands 分层门禁 | 已修复（2026-10-04）：check-layers.ps1 增加 src/commands 对 storage::、rusqlite、Connection 的检查；正常通过，并须执行反向注入验证。命令层继续只调用服务，不接受连接。 |

## 三、已核对合理的边界（避免后续误当冲突）

- P4 clarify_ready 只做无运行会话的 Inbox/Clarifying→Ready；P2 start 可原子理清并启动。两条入口用途不同。
- 今日计划是人工选择集合，不自动改变任务状态、不建立排期、不启动计时。
- P2 finish 只结束会话；P3 transition_task 才负责任务完成/取消及会话原子联动。
- 标签/今日计划增删是 epoch-only 显式集合操作；不增加 task.row_version，真实变化增加全局 revision 并写审计；重复增删无变化。
- P3 尚未实施，恢复确认、历史修正、补录及任务完成联动不能按“已交付”评审。

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

**首次握手时序已定，服务入口现已补齐**：00 §5 规则 1 与 P7 计划已经定义好形状——窗口**先监听并暂存通知，再拉**含 epoch/revision 的一致快照；可见窗口至多每 30 秒校验 `get_revision`（`00-architecture.zh.md:61`、`2026-10-03-p7-shell-and-ui.md:48`/`:50`）。本轮已统一服务侧的 epoch 校验口径（所有查询只接受**请求带来的**期望值，`guard_epoch` 在同一个读事务内跑），P7 负责实现上述握手与迟到响应丢弃。

**仍未做**：P7 的 IPC 接线（命令层构造 `WriteEnvelope`、接线 `capture_error_response`、迟到响应丢弃、先监听后拉的握手时序）与 UI 不在本轮范围。

**本轮登记的新遗留**：

1. **「同一读/写事务」缺可观察证据**：现有断言只能证明「返回值等于当时的库值」，证伪「返回前补读」需要第二连接并发写 + 快照校验（`unchecked_transaction` 是 DEFERRED，要在另一连接提交后确认旧信封仍持旧 revision），约 1–2 小时，本轮不做；「同事务」目前由结构保证（四个读服务与 `settle` 的代码形状）。
2. **P2 计时快照是提交后重建**（FOLLOW-06）：严格按「写结果与元数据同一写事务」这条口径 P2 不满足，R-E 冻结范围，P7 接线前评估。

## 首次握手与 P2 快照收口（2026-10-04）

新增 services::handshake::get_revision(db) -> RevisionSnapshot {data_epoch,revision}，不需要 expected_data_epoch，纯读、不采样、不初始化库、不增加 revision。P7 先监听并缓冲事件，再调用此入口获得库身份，随后携该 epoch 拉业务一致快照；期间发生恢复则业务查询返回 DATA_EPOCH_MISMATCH，重新握手。get_revision 不是包含全部业务数据的快照，窗口仍需拉取所需业务视图，并保留事件缓冲与版本丢弃协议。周期校验与恢复后重新握手复用同一入口，生产 IPC 尚未接线。

历史 FOLLOW-06 所谓“提交后只读一次”不准确：原实现 build 和随后 require_meta 各读一次元数据，且实体分次读取。现改为一个提交后读事务，并直接复用 snapshot.revision。历史记录中的未完成说法以本节和最新表格为准。

本轮验证：352 个测试通过；分层正向通过，临时注入 commands 对 storage 的引用后门禁明确返回 1，移除探针后再次通过。首次握手用例覆盖无初始 epoch、恢复后库身份改变/旧查询拒绝、未初始化元数据拒绝且零创建。
