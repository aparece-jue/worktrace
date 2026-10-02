# Worktrace 总体架构

状态：评审修订草案，待用户评估。日期：2026-10-02。对应：[英文版](00-architecture.en.md)。
产品愿景见 [PROJECT_SPEC](../../../PROJECT_SPEC.md)；修订和待评估项见 [评审摘要](06-review-notes.zh.md)。本套文件描述目标设计，不表示功能已实现；当前仓库仍是前端页面与 greet 命令脚手架。

## 1. 定位与文档权责

面向个人工程工作的本地任务、工时与回顾工具。Local First、AI Optional、User > Rule > AI 保留；AI 不可用不影响核心功能。暂不做通用 Agent、插件市场、Office/CAD 替代或通用工作流。

PROJECT_SPEC 是愿景与原始版本草案；04 决定验收范围、05 决定依赖及实施顺序、02 决定数据口径、03 记录技术理由、99 统一术语。任何新增产品取舍在 06 标为待评估，不能仅凭技术 ADR 宣称用户已批准。中文为工作主稿，英文随同次修订同步；冲突先修订，不能让多个文件分别宣称最高优先级。

## 2. 技术与运行拓扑

Tauri 2、React 19、TypeScript 6、Vite 8、pnpm；推荐统一 Ant Design，MUI/emotion 待设计评估后清理。保留复制来的组件，不把 DockviewDemo 自动接入产品；dockview 是否用于正式工作区待评估。

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

## 5. 业务 revision 与同步协议

app_meta.revision 随成功业务写事务加 1 并持久化；同事务多项变化合并为一条 domain.changed 失效通知，附变化实体 ID/类别。无业务变化不加；心跳和 timer.tick 不加。快照 revision 与数据在同一读事务取得。

事件信封：event、revision、at（Unix 毫秒）、payload。多个事务提交后按 revision 顺序广播；前端仍必须处理延迟、重复及乱序。

1. 窗口先注册监听并暂存通知，然后拉取带 revision 的一致快照。
2. 应用快照，丢弃 revision <= 快照值的通知；后续通知让受影响查询失效。
3. 查询响应带 revision；比该视图已应用/所需版本旧的响应不得覆盖，新请求合并去重。
4. 窗口再次显示/恢复、IPC 重连时检查 get_revision；可见窗口最多每 30 秒轻量校验，补救丢失最后一条事件。后台窗口重显前校验。版本检查不拉全库。
5. 跳号/乱序且缓存无法证明一致时重新取快照；允许短暂延迟，不依赖通知永不丢失。

timer.tick 独立携带 run_id、tick_seq、as_of、session_id、active_ms、remaining_ms。每个 run 内递增；旧 run/旧序号丢弃，不触发业务 revision 写入。重开窗口立即查询计时快照，不等下一秒 tick。

## 6. 前端状态与外发边界

每个 JS 上下文只有一个 domainState 订阅入口；页面通过 hooks 读取，清理监听。事件只作缓存失效，计时 tick 更新展示值；业务规则留在 Rust。先用 useSyncExternalStore 等简单方案，前端缓存复杂后允许评估状态库，不能把“不使用库”当领域权威的必要条件。

AI Gateway 只接收 M09 构造的白名单 Context Bundle。V0.3 即引入最小任务/项目保密策略，完整文档提取到 V0.4。默认 STRICT_LOCAL；用户按提供商显式授权允许外发的条目。STRICT_LOCAL 与 CONFIDENTIAL 默认禁止云端，INTERNAL 需明确授权，PUBLIC 可在启用云 AI 后发送。派生摘要/提取缓存继承最严格来源等级，日志仅记录 ID、等级与计数，不记录机密正文。凭据使用系统凭据存储，不放 SQLite 或前端 localStorage；模型建议的采纳不等于外发授权。

## 7. 待验证与索引

在实现阶段验证：Windows HUD 穿透/无焦点与 DPI、多入口开发/打包路径、数据库执行线程与关闭/备份竞争、断网/锁屏/休眠/改时计时行为。尚无实测结论，不能写“已验证”。HUD 可行性实验不阻塞核心记录闭环。

[模块拆分](01-module-breakdown.zh.md) · [数据模型](02-data-model.zh.md) · [ADR](03-adr.zh.md) · [功能验收](04-functional-spec.zh.md) · [路线图](05-roadmap.zh.md) · [术语](99-glossary.zh.md)
