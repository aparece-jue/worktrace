# Worktrace 总体架构

状态：评审修订草案，待用户评估。日期：2026-10-03。对应：[英文版](00-architecture.en.md)。
产品愿景见 [PROJECT_SPEC](../../../PROJECT_SPEC.md)；修订和待评估项见 [评审摘要](06-review-notes.zh.md)。本套文件描述目标设计，不表示功能已实现；当前仓库仍是前端页面与 greet 命令脚手架。

## 1. 定位与文档权责

面向个人工作的任务、时间、成果与回顾工具，支持能力短板分析及可追溯 KPA 材料。Local First、AI Optional、User > Rule > AI 保留；AI 不可用不影响核心功能。暂不做通用 Agent、插件市场、Office/CAD 替代或通用工作流。

PROJECT_SPEC 是愿景与原始版本草案；04 决定验收范围、05 决定依赖及实施顺序、02 决定数据口径、03 记录技术理由、99 统一术语。R-01～R-08 已确认；技术验证仍未完成，本轮协议与恢复行为修订待审核。新增产品取舍单列在 06，不能用实现状态代替批准状态。中文为工作主稿，英文随同次修订同步；冲突先修订，不能让多个文件分别宣称最高优先级。

## 2. 技术与运行拓扑

Tauri 2、React 19、TypeScript 6、Vite 8、pnpm；推荐统一 Ant Design，MUI/emotion 待设计评估后清理。保留复制来的组件，不把 DockviewDemo 自动接入产品；R-04 已定固定布局优先，保留 dockview 依赖和演示，不接入 V0.1 发布入口。

Rust 为单一应用核心进程，WebView/系统渲染可产生其他进程；不要承诺整个应用只有一个 OS 进程。Rust 服务是业务状态唯一权威，SQLite 是持久化依据，React 维护可重建视图缓存及 UI 临时状态。多个窗口可短暂不同步，以下协议提供收敛，不能宣称永不出现展示差异。

主窗、HUD、Mini 各有独立 JS 上下文；关闭窗口不终止核心。托盘直接调用同一服务。单实例启动检查在数据库/计时初始化之前完成；第二次启动的临时进程通知原实例后退出，不产生第二个核心或计时引擎。

HUD/Mini 建议独立轻量入口；单入口+懒加载也是有效备选。Vite 8 的多入口配置以当前安装版本和构建验证为准，不凭旧配置名推断。各窗口只有需要的 capabilities；HUD 默认只读，文件/网络/修改能力由 Rust 边界控制。

## 3. 模块边界与事务

| 目录 | 职责 | 允许依赖 |
| --- | --- | --- |
| domain/ | 纯类型、状态跃迁、校验 | 纯事件类型，不依赖运行时总线/IO |
| storage/ | SQLite、迁移、事务、备份、查询 | domain、纯事件类型 |
| platform/ | OS 窗口、托盘、锁屏、热键、单实例 | Tauri/OS 适配；不做业务判断 |
| services/ | 意图编排、计时、统计、上下文、AI | domain、storage、platform、events |
| commands/ | 参数校验、错误/DTO、调用服务 | services、domain 类型 |
| events/ | 纯事件类型与独立运行时分发适配 | 纯类型无依赖；广播适配可依赖 Tauri |

lib.rs 是组合根。commands 不直连 SQL，domain 无 IO，storage 不调用平台。M00 的原生回调由 M11/组合根接到 services，不在平台层偷偷写数据库。

服务必须直接在一个事务内完成需一致的变更：如 complete_task 结束相关 session、写 Task 结果、增加 revision。关键步骤不能交给事件订阅者异步补齐。统计正常查询时聚合，无须每次强制维护第二份总工时。

SQLite 的同步访问放入受控阻塞执行边界；连接不跨 await 持锁。采用串行数据库工作线程还是受控 spawn_blocking，在 M01 小实验后确定。写事务内校验全局不变量，数据库约束兜底。备份/恢复暂停写入；明确 busy_timeout、WAL、外键、迁移前备份及磁盘不足处理。备份实现优先 SQLite 一致快照 API 或经验证的 VACUUM INTO，不直接复制活跃 WAL 主文件。

![模块边界与依赖方向](images/architecture-layers.svg)

> 图：依赖恒为由上至下；`events/` 为横切，不参与依赖栈。

![运行时与进程拓扑](images/runtime-topology.svg)

> 图：核心进程持有全部状态；三个 WebView 各自独立，只是订阅者。

## 4. IPC 与错误

Command 按意图命名，一次完成业务事务；Today 等聚合视图一次返回，列表避免 N+1。DTO 类型从 Rust 生成，选型在实现阶段验证。更新命令带 expected_row_version，冲突返回 VERSION_CONFLICT；长耗时 AI 请求另带输入版本，结果不覆盖已变更任务。

预期失败使用 Result<T,AppError>：code、message、脱敏 detail。panic 是缺陷；当前 release panic=abort 会终止进程，不能承诺 catch 后转 AppError。必须依靠诊断和恢复，并避免 unwrap 用于可预期用户/IO 错误。

已提交后广播失败只记诊断，不返回“事务失败”让用户重复操作。非幂等命令须避免重复提交；需要自动重试时加 request_id 并在同事务保存可重放结果，否则客户端不得盲目重试。

## 5. 数据代次、业务 revision 与计时协议

app_meta 保存 data_epoch（UUID）与 revision。新数据库生成 epoch；成功恢复/替换数据库生成全新 epoch，不能沿用备份内的 epoch。同一 epoch 内业务写事务 revision +1，同事务多项变化合成一条 domain.changed。无业务变化、心跳、timer.tick 不加业务 revision。快照的数据、epoch、revision 在同一读事务取得。

所有业务响应/快照/通知携带 data_epoch、revision；修改命令带 expected_data_epoch 和 expected_row_version。epoch 不匹配返回 DATA_EPOCH_MISMATCH，不写入；非幂等 request_id 的重放也只在同一 epoch 内。revision 仅同一 epoch 内可比较，不能用恢复后的低版本号推断旧数据。

1. 窗口先监听并暂存通知，再拉一致快照。
2. 新 epoch 的权威快照使全部业务/计时缓存失效；丢弃旧 epoch 响应、通知和未应用建议。普通未知 epoch 通知只触发重新握手，不直接接纳，防止迟到通知把缓存切回旧库。
3. 应用快照后丢弃同 epoch 且 revision <= 快照版本的通知。受影响视图合并刷新，查询响应比已应用/所需版本旧时不得覆盖。
4. 重显、恢复、IPC 重连以及可见窗口至多每 30 秒检查 get_revision，返回 epoch+revision；隐藏窗口显示前校验。最后事件丢失仍可收敛。
5. 跳号/乱序无法证明一致时取新快照。恢复期间暂停服务受理新写入，返回 DATA_RESTORE_IN_PROGRESS；取消旧队列/查询/AI 请求，完成后恢复握手。

事件信封：data_epoch、event、revision、at（Unix 毫秒）、payload。广播按提交顺序；失败只诊断，不回滚已提交业务。

timer.tick 和计时查询均携带 data_epoch、run_id、session_id、session_version（work_session.row_version）、tick_seq、as_of、active_ms、remaining_ms、state。tick_seq 每个 run 内递增，查询/命令结果也返回同一展示序列基线。前端先检查 epoch、run、会话/状态版本，再比较 tick_seq；旧状态生成的 tick 即使序号较新也不能覆盖暂停/切换后的展示。较新 session_version 的未知 tick 先触发计时快照，不能自行推导状态跃迁。

计时状态变更由一个串行协调器按 start/pause/resume/finish/系统事件顺序处理：同一时钟采样生成事务所需区间事实 → 提交 DB 并增加 session_version → 应用内存计时基线 → 返回结果/发送状态快照。tick 和计时查询经同一协调器采样，不在提交与基线应用之间生成不一致 DTO。提交失败不应用内存变更；提交成功后内存应用失败进入故障恢复，不返回普通可重试的失败。持久化事实是恢复依据，内存基线是可重建运行态。

![版本与快照同步](images/revision-sync.svg)

> 图展示常规同步；`data_epoch` 切换、计时状态版本与恢复队列隔离见下一张。

![数据代次切换与恢复隔离](images/epoch-switch-restore.svg)

> 图：恢复替换数据，并生成新的数据库身份。代次一变，旧响应、旧通知、未应用建议同时作废。

## 6. 前端状态与外发边界

每个 JS 上下文只有一个 domainState 订阅入口；页面通过 hooks 读取，清理监听。事件只作缓存失效，计时 tick 更新展示值；业务规则留在 Rust。先用 useSyncExternalStore 等简单方案，前端缓存复杂后允许评估状态库，不能把“不使用库”当领域权威的必要条件。

AI Gateway 只接收 M09 按本次用户选择构造的 Context Bundle。AI 默认关闭；用户预览实际输入、确认目的地后才发送。任务理清、拆分、估时、标签、优先级/排期和总结保留，程序计算事实统计，AI 组织建议与表达。应用不读取关联文件正文；外部 Agent 结果先导入确认，不自动转发。凭据使用系统凭据存储，日志不记录正文或密钥。

## 7. 待验证与索引

在实现阶段验证：Windows HUD 穿透/无焦点与 DPI、多入口开发/打包路径、数据库执行线程与关闭/备份竞争、断网/锁屏/休眠/改时计时行为。尚无实测结论，不能写“已验证”。HUD 可行性实验不阻塞核心记录闭环。

[模块拆分](01-module-breakdown.zh.md) · [数据模型](02-data-model.zh.md) · [ADR](03-adr.zh.md) · [功能验收](04-functional-spec.zh.md) · [路线图](05-roadmap.zh.md) · [评审摘要](06-review-notes.zh.md) · [范围与导入契约](07-scope-and-agent-import.zh.md) · [实施契约](08-implementation-contracts.zh.md) · [术语](99-glossary.zh.md)

## 8. AI 输入与外部 Agent 边界

应用内不建设四级文件密级、文件权限继承或提取缓存。保留关闭 AI、本次选择/预览/确认、目的地与输入版本校验；输入或提供商/端点改变须重新确认，撤销阻止尚未发出的请求，不承诺撤回已发内容。外部 Agent 的文件授权由用户在对应工具管理；导入不等于允许发送。完整导入契约见 [07](07-scope-and-agent-import.zh.md)。

![AI 输入与外部 Agent 边界](images/ai-agent-boundary.svg)

> 图：文件正文不进来，只进来结构化结果；AI 不自己开，每次都要人点一下。

实施细节补充见 [08：计时、阶段、AI 与报告快照](08-implementation-contracts.zh.md)。

V0.2 番茄钟在同一信封增加 phase、phase_state、cycle_index、phase_elapsed_ms、phase_remaining_ms、phase_overtime_ms，定义及命令状态表见 08 §7；休息时 session paused 不代表阶段计时冻结。
