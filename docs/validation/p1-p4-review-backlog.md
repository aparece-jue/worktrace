# P1～P4 结果兼容性待评审清单

记录日期：2026-10-03。代码核对基线：c981590；文档以当前工作区对齐版本为准。
本清单记录已观察到的问题及建议，不代表已修复或已完成验收。后续评审可以集中决定实施顺序。

## 一、需要补齐的实现问题

| 编号 | 优先级 | 问题与证据 | 影响 | 建议与验收要求 | 状态 |
| --- | --- | --- | --- | --- | --- |
| COMP-01 | P2 | P2 TimerSnapshot 带 data_epoch/revision；P4 catalog 的 ProjectChange、TaskProjectChange、TagChange、TaskTagsChange、TaskChange 及 DailyPlanChange 仅带 revision；list_selectable_projects/list_tags/tags_of_task 返回裸列表。见 src-tauri/src/services/catalog.rs、services/daily_plan.rs | 对外响应不能统一执行旧 epoch/旧 revision 丢弃协议；多窗口和恢复接线存在缺口。当前尚未接 IPC，不声称已经发生缓存错乱 | 写结果的数据与 epoch/revision 在同一写事务取得，提交后返回；业务读结果在同一读事务取得数据和元数据。不得由 IPC 层提交后补读。明确查询请求的 epoch 校验及首次握手方式；覆盖一致快照、旧 epoch 拒绝及 P7 迟到响应丢弃 | 未修复，P7 接线前门禁 |
| COMP-02 | P2 | task_repo::create_task 只拒绝 archived，Some(_) 放行 done；set_task_project 拒绝 done；require_active_project 拒绝所有非 active，却都提示“已归档”。见 src-tauri/src/storage/task_repo.rs | 可以创建归属 done 项目但随后无法计时的任务；新建与重新绑定规则不同，拒绝理由不准确。V0.1 无 done 创建入口，但 schema/读模型允许该状态 | 新建任务归属、重新绑定、start/resume 均仅允许 active；archived/done 历史可读。done 使用准确的中文拒绝理由。补新建、绑定、开始/恢复路径测试，拒绝时断言 revision、版本、审计和相关字段零变化 | 已修复（2026-10-04，提交 9e7a89a；门禁 340 passed / 0 failed），回归验证见下方 |
| COMP-03 | P2 | project_repo::list_projects 支持完整/按状态查询；catalog 只提供 active 的 list_selectable_projects，注释引导完整列表直接调用仓储。见 src-tauri/src/services/catalog.rs、storage/project_repo.rs | Projects 页查询归档历史缺少符合 commands→services 分层的服务入口 | 补完整项目列表服务及可选状态过滤，保留 active 选择入口，统一 COMP-01 响应信封；验证归档/done 历史可读、选择列表只含 active，命令层不直调仓储 | 未修复，P7 接线前门禁 |

优先级 P2 表示需在依赖功能接入前完成，不表示当前已出现数据损坏。三个问题属于既有交付的兼容收尾，不新增产品功能或实体。

## 二、文档对齐后的实施与核对事项

| 编号 | 事项 | 当前状态与后续归属 |
| --- | --- | --- |
| FOLLOW-01 | P7 统一接入 capture_error_response | 服务已实现，生产 IPC 尚未接入。在原事务结束后、同一串行边界捕获；按 kind/id 匹配 records，不按请求下标；读取失败要求整份重新握手，不用 timer.snapshot 补错误版本 |
| FOLLOW-02 | error_response.rs 的返回顺序注释 | 文件开头仍写“按同一份顺序逐条返回”，实际按 kind 白名单分组、组内保持请求顺序。实际实现符合 P4 计划；2026-10-04 已修正注释，返回顺序未改变 |
| FOLLOW-03 | P3 扫描版本与审计规则 | 文档已明确：扫描查询零写；实际修改 session 状态/run_id/区间事实时修改对象版本，并在同一批事务增加一次 revision、记审计。paused 重绑定适用；recovering 保持原恢复归属直至 reconcile。P3 尚未实施，后续按真实扫描测试验证 |
| FOLLOW-04 | 中文错误提示一致性 | P4 验收记录仍登记 P1/P2 遗留的内部列名、英文提示等；建议 P7 前集中清理。Storage.detail 仅诊断，Domain.detail 会进入用户 message，不能混用 |
| FOLLOW-05 | 平台验收 | 正式系统事件、锁屏/休眠/改时、多窗口/托盘、备份恢复及跨机器容差验证由 P6/P7/P8 承接；自动测试不能替代实机验收 |

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

FOLLOW-02 的返回顺序注释已修（`error_response.rs`，同一提交 9e7a89a），返回顺序未改变。

门禁（在 9e7a89a 上重跑）：`cargo test --offline` **340 passed / 0 failed**（本次补强只加强断言；相对上一提交 `a3f9f80` 还含新增的那条回归用例，故 339 → 340）；`cargo fmt --check`、`cargo clippy --all-targets --offline -- -D warnings`、`scripts/check-layers.ps1` 全绿。

COMP-01（统一响应信封）与 COMP-03（完整项目列表服务）仍未修，本轮未改动公开读取签名。
