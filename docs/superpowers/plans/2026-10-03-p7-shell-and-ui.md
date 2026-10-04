# P7 · 桌面外壳与核心交互 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 尽早让应用**能用**：把冻结好的服务接到用户手上（命令接线、状态镜像、捕获→理清→计时、托盘与窗口生命周期），并把 06 §4 的「双窗口同步」实验跑起来。「统计/恢复/导出」的界面在 **P8**，本计划只做不依赖 P3/P5/P6 的那部分。

**为什么提前**：06 §4 把「双窗口同步」列为**实现前**技术验证，它需要两个窗口才能跑；01 §2 也明写「M12 外壳可以使用 mock 提前搭建，随后逐条接入」。把外壳压到最后会让这条实验无处可做，平台风险也要等到收尾才暴露。

**Architecture:** 前端**不含业务规则**：它只做订阅、渲染与转发命令。每个 JS 上下文只有**一个** `domainState` 订阅入口，页面通过 hooks 读取；事件只作缓存失效，计时 tick 只更新展示值。所有判断（状态合法性、统计口径、恢复分流）都留在 Rust。启动只调用本计划先建立的 services/bootstrap 入口；P6 后续扩展同一入口，不复制启动流程。

**Tech Stack:** Tauri 2 · React 19 · TypeScript 6 · Vite 8 · Ant Design（ADR-001 统一 UI 库）· `useSyncExternalStore`（先不上状态库）· vitest + jsdom（仅开发依赖，见「前置任务」）

**Spec:**
- `../specs/2026-10-02-worktrace-architecture/00-architecture.zh.md` §4（IPC 与错误契约；DTO 的落地形状见本计划 Task 1）、§5（快照、去重、计时协议）、§6（前端状态边界）
- `.../04-functional-spec.zh.md` F-001、F-002、F-003、F-004、F-005、F-009、F-010、F-011、F-015、F-017、F-018、F-019、F-020
- `.../03-adr.zh.md` ADR-001（统一 Ant Design）、ADR-012 的 R-04（`DockviewDemo` 不进发布产物）

**依赖的前置计划：** **P1、P2、P4**（领域与持久化、计时协调器、项目/标签/今日计划）。**P3（恢复确认）与 P5/P6 不阻塞本计划**——它们的界面在 P8。本计划建立必要平台接线，只消费已有业务服务的入口与 DTO，不重写任何业务判断，也不为尚未存在的服务造临时实现。

**边界（不要越界）：**
- **HUD 与全局捕获热键属 V0.1b**（F-012/F-013），本计划不做。
- **dockview 布局与布局保存**属 R-04 之后（"已明确需要同时看多面板时再接"），本计划用**固定布局**；`src/components/DockviewDemo.tsx` 保留在仓库但**必须排除在打包产物之外**。
- Mini 小窗待评估，不做。
- 本计划不新增任何统计/计时/恢复逻辑；发现缺什么，回到对应计划补，不在前端兜。
- **不做**「确认人工工时 / 待确认时间」的展示与恢复确认页——它们需要 P5 的统计与 P3 的恢复服务，属 P8。**P7 的 Today 是两条命令的组合**：`daily_plan::plan_for`（`src-tauri/src/services/daily_plan.rs:164`）取「今日选择列表」+ `Coordinator::snapshot`（`src-tauri/src/services/timer/coordinator.rs:268`）取「当前任务 + 运行中的实时计时」；**含确认人工工时/待确认的一次性聚合视图属 P5/P8**，P7 不自造聚合、不在前端拼工时。

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。前端测试只断言**展示与转发**，业务断言留在 Rust 侧。

**范围（诚实声明）：** P7 只做下面「建议执行顺序」里 Task 0–6 中**不依赖 P3/P5/P6** 的部分；依赖项在各自 Task 的「本阶段不做」里逐条列出，且**不进入**本阶段完成门槛。

---

## 建议执行顺序（Task 0 → 1 → 2 → 3/5 并行 → 4 → 6a/6b）

1. **Task 0（平台接线与启动顺序）**——其余一切都站在它上面：没有真实接线，双窗口实验无处可做，托盘与退出也没有可复用的入口。
2. **前置任务（前端测试基建）**——Windows 侧离线安装，**Task 2 开工前**必须完成。
3. **Task 1（命令接线与 DTO）→ Task 2（前端状态镜像）**——串行：Task 1 定下的响应形状就是 Task 2 的输入。
4. **Task 3（捕获/理清/计时）与 Task 5（Projects/Tasks 列表）可并行**——两者只依赖 Task 1/2 建立的外壳与状态镜像，页面与文件不重叠。
5. **Task 4（托盘与窗口生命周期）**——排在 Task 3 之后：托盘动作与界面动作必须复用**同一批命令**，命令名要先定下来。
6. **Task 6a（双窗口同步实验）→ Task 6b（外壳人工验收与完成门槛）**——最后，依赖前面全部落地。

---

## Task 0：平台接线与启动顺序

文件：`src-tauri/src/platform/single_instance.rs`（新建）、`src-tauri/src/platform/scheduler.rs`（新建）、`src-tauri/src/platform/mod.rs`（登记两个新模块）、`src-tauri/src/storage/run_repo.rs`（新建：`application_run` 原语）、`src-tauri/src/storage/session_repo.rs`（新增按 `run_id` 过滤的查询）、`src-tauri/src/storage/mod.rs`（登记）、`src-tauri/src/services/bootstrap.rs`（新建：唯一启动入口）、`src-tauri/src/services/events.rs`（新建：事件信封与去重）、`src-tauri/src/services/mod.rs`（登记）、`src-tauri/tests/startup_order.rs`、`src-tauri/tests/periodic_sampling.rs`、`src-tauri/tests/event_protocol.rs`、`src-tauri/tests/exit.rs`（四个都新建）。

- [ ] **启动顺序固定为**（01 §2 + 02 §4，不得调整）：① **单实例检查**（必须**先于**持久化初始化）→ ② 打开库并迁移 → ③ 新建 `application_run`（每次成功启动一行）→ ④ 恢复扫描（P3；未完成时见下方门禁）→ ⑤ 启动协调器（P2）与**周期采样驱动** → ⑥ 开窗口。`lib.rs` **不得**自己重排这段顺序；入口只有 `services/bootstrap.rs` 一个（P6 后续扩展同一入口）。**副作用次序用注入的探针记录并在 `tests/startup_order.rs` 断言调用次序**——不是断言"没崩"。
- [ ] **单实例**（`platform/single_instance.rs`，`platform/` 叶子、不含业务规则）：OS 级文件锁，锁文件用既有的 `platform::paths::instance_lock_file()`（`src-tauri/src/platform/paths.rs:40`，与库同目录）。拿不到锁的进程**通知既有实例后退出**：**不打开库、不迁移、不建 `application_run`、不启动计时**（F-016）。"唤起既有主窗"是**通知**，与锁分离。**交付边界（2026-10-04 实施后订正）**：Task 0 只交付**发送侧**（`single_instance::request_activation`）与**接收原语**（`take_activation_request`）；把请求变成"抬起主窗"的消费侧需要窗口对象，归 **Task 4**——因此本任务结束时 `take_activation_request` 还没有生产调用者，这是分割点，不是遗漏。
- [ ] **`application_run` 原语**（`storage/run_repo.rs` 新建）：表在 P1 的 schema 里就有（`src-tauri/src/storage/schema_v1.rs:28`，字段 `id`/`started_at`/`clean_exit_at`），但**全仓没有任何 storage 原语**（`src-tauri/src/storage/` 下无 `run_repo.rs`）。补最小三个：建 run、读 run、写 `clean_exit_at` 结束 run；都接受调用方的 `&Transaction`/`&Connection`，**事务由 `services/bootstrap.rs` 拥有**。
- [ ] **显式退出（本计划建立的入口，P6 后续硬化）**：**先停定时器（不进事务）**，其余四项——结束 `running`/`paused` 会话、写 `clean_exit_at`、保存 revision、清活动阶段（V0.1 没有阶段列，这一项到 V0.2 番茄钟接入时才有实际写入）——在**同一个事务**内完成。**"停定时器"为什么不在事务里（2026-10-04 实施后订正）**：采样驱动取的是同一把 `Mutex<AppState>`，在事务里 join 采样线程会与持锁的退出路径互锁；先 `stop()`（置位 + join）再开事务既避免互锁，也保证退出事务之后不会再有一拍落进来。**`recovering` 记录保留不清**（02 §4 原文：「显式退出同事务结束 running/paused、保存 revision 与 clean_exit_at；recovering 记录保留」）。不是杀进程；长事务不得跑在 UI 回调里。Task 4 的托盘"退出"与 P8 都复用这一条入口。
- [ ] **周期采样驱动**（`platform/scheduler.rs` 新建）：**窗口全关仍在跑**——定时器不得挂在任何窗口对象上（F-009）。每次触发走与用户命令**同一条串行边界**（同一协调器入口）；**空闲（无活动会话）时不产生任何写入**（不空转制造 revision）。
- [ ] **串行执行边界（D6 裁决，写死）**：单一 `Mutex<AppState{ db, coordinator }>`，或等价的**专用工作线程 + channel**——**二选一由实施者在 Task 0 定稿，并把选择理由写进 `src-tauri/IMPLEMENTATION-NOTES.md`**（那里已有 P1 的实测对照：两种边界同量级，差别在阻塞落在谁的线程上）。硬约束与选型无关：命令一律 `async`；**不得在 UI 回调里跑长事务**；**`Connection` 不得跨 `await` 持有**（依据 `src-tauri/IMPLEMENTATION-NOTES.md` §2）。
- [ ] **事件信封与四条去重规则**（`services/events.rs` 新建）：信封字段固定 `data_epoch`、`event`、`revision`、`at`（Unix 毫秒）、`payload`（00 §5）；**广播按提交顺序**；广播失败只记诊断、不回滚已提交业务。四条规则各一条断言：① 新 `data_epoch` 的权威快照使**全部**业务/计时缓存失效，**未知 epoch 的通知只触发重新握手**；② 应用快照后丢弃**同 epoch 且 `revision <=` 快照版本**的通知；③ 查询响应比已应用/所需版本旧时**不得覆盖**；④ 序号跳号/乱序无法证明一致时**取新快照**。P6 沿用同一协议补故障路径（P6 Task 3），**不新建第二套**。
- [ ] **P3 未完成时的开发验证库门禁（具体化）**：只在独立开发验证库演示；启动时按 **`run_id <> 当前 run`** 查三件事——未结束会话、待确认区间、不变量损坏——**命中任一 ⇒ 拒绝业务计时并提示需完成恢复**，不自动修复、不忽略历史。⚠️ **现有 `session_repo::running_foreground`（`src-tauri/src/storage/session_repo.rs:413`）不带 `run_id`**，需要一条按 `run_id <> ?` 过滤的新查询（本任务在 `storage/session_repo.rs` 新增，`tests/startup_order.rs` 覆盖）；待确认区间/不变量损坏所需的查询同样按现有仓储补齐。P3 完成后在**同一 bootstrap** 接入真实扫描（四类判定归 P3）。
- [ ] **平台事件适配**（正式锁屏/休眠/唤醒/时钟变更）：事件通知进入 P2 **同一串行入口**；晚到或边界不可信按 P2 恢复规则处理，**不另写一套判断**。到达延迟与"关窗后事件仍可达"属**实机步骤**（见文末「仍待与归属」），不在本任务用单元测试冒充。
- [ ] **测试**（`tests/{startup_order,periodic_sampling,event_protocol,exit}.rs`）：启动副作用次序（探针）；第二次启动**不打开库、不迁移、不建 `application_run`**；锁持有者被强杀后新进程能拿到锁；**无窗口引用时周期采样仍被驱动**且空闲不写库；事件四条去重规则各一例；显式退出事务结束 `running`/`paused` 且写 `clean_exit_at`，**`recovering` 行仍在**。
- [ ] **本阶段不做（依赖 P3/P5/P6）**：正式恢复扫描与四类判定（P3）；单实例/启动的故障路径硬化与锁异常释放（P6 Task 1）；维护态隔离与 P6 新增的维护态错误码（P6 Task 2/4）；备份与恢复（P6 Task 4）；平台事件实机验收（本文末登记，P8 复核）。

## Task 1：Tauri 命令接线与 DTO 形状

文件：`src-tauri/src/commands/mod.rs`（加 IPC 命令与入参 DTO；**先集中在一个文件**，超出可读规模再按域拆）、`src-tauri/src/lib.rs`（注册真实 handler、删 `greet`、`setup` 只调 bootstrap）、`src-tauri/src/services/catalog.rs`（**D3 的 IPC 请求 DTO** + serde）、`src-tauri/src/services/timer/snapshot.rs`、`src-tauri/src/services/timer/coordinator.rs`、`src-tauri/src/services/daily_plan.rs`（以上三个加 serde）、`src-tauri/src/services/handshake.rs`（核对：`RevisionSnapshot` 已有 `Serialize`）、`src-tauri/src/storage/project_repo.rs`、`src-tauri/src/storage/tag_repo.rs`、`src-tauri/src/storage/task_repo.rs`（行类型加 serde）、`src-tauri/src/domain/task.rs`、`src-tauri/src/domain/project.rs`、`src-tauri/src/domain/tag.rs`、`src-tauri/src/domain/session.rs`（枚举的 `Serialize` 实现）、`src-tauri/src/error.rs`（核对：`ErrorResponse`/`ErrorAuthority`/`RecordVersion` 已有 `Serialize`，`AuthorityTarget` 已有 `Deserialize`）、`src-tauri/tests/ipc_snapshots.rs`（新建）、`src-tauri/tauri.conf.json`（D4 的 `identifier`）、仓库根前端 `src/ipc.ts`（**新建**：命令转发、错误规范化与迟到响应丢弃）、`src/types/ipc.ts`（**新建、手写**）、`src/types/__snapshots__/*.json`（新建）、`src/App.tsx`、`src/main.tsx`、`index.html`。

- [ ] `commands/` 层**不接受 `Connection`**（总纲 §9）：它只做参数反序列化、调用 `services::*`、把 `Result<T, AppError>` 映射成前端可用的形状。事务与 epoch/version 校验由服务与 `storage::guards` 负责。命令层只 `import` `services::*` + `crate::envelope` + `crate::error`（分层门禁 `commands 禁 storage::`）。
- [ ] 启动只调 **Task 0 建立的 `services/bootstrap.rs`** 一个入口；`lib.rs` 不得自己重排"单实例 → 库 → run → 扫描 → 协调器 → 窗口"的顺序。
- [ ] **DTO 形状（D1 裁决，取代原先的「生成器」口径）**：**不上 DTO 生成器**——`ts-rs`/`specta`/`typeshare` 在本机**两侧 cargo 缓存都是 0 命中**（离线取不到），引入需联网并**经用户确认**。改为：
  - **命令层显式声明 serde DTO**：请求 `Deserialize`、响应 `Serialize`；
  - **服务结果类型加 `Serialize`**：`ProjectList`/`TagList`（`services/catalog.rs:233`/`:245`）、`TaskQueryResult`（`:616`）、`DailyPlanView`（`services/daily_plan.rs:137`）、`TimerSnapshot`（`services/timer/snapshot.rs:14`）、`CommandOutcome`（`services/timer/coordinator.rs:70`），以及写命令返回的 Change 家族（`ProjectChange`/`TaskProjectChange`/`TaskChange`/`TagChange`/`TaskTagsChange`/`DailyPlanChange`）；
  - **请求类型加 `Deserialize`**：`TaskQueryRequest`（本任务新增，见下）、`StartRequest`/`SessionRequest`/`ResumeRequest`（`services/timer/coordinator.rs:39`/`:52`/`:60`）；
  - **行类型也要能序列化**：`ProjectList.items`/`TaskQueryResult.tasks`/`DailyPlanView.tasks` 直接装 `ProjectRow`/`TagRow`/`TaskRow`，所以 `storage/{project_repo,tag_repo,task_repo}.rs` 的行类型加 `Serialize`；但**命令层不得出现 `storage::` 的名字**（分层门禁），命令层只是把服务结果原样交回 IPC；
  - **状态/类别枚举不派生 `Serialize`**：在各自领域文件里写一份 `Serialize` 实现，内部 `serialize_str(self.as_str())`（`domain/task.rs:36`、`domain/project.rs:28`、`domain/tag.rs:22`、`domain/session.rs:26`/`:100` 已有 `as_str()`）——等价于在每个字段上写 `serialize_with`，但**只有一份**，JSON 里就是库里那套小写字符串；
  - **前端类型手写** `src/types/ipc.ts`（不手写第二份业务规则）；
  - **一致性由一条 Rust 集成用例钉住**：`tests/ipc_snapshots.rs` 把每个响应 DTO 序列化成 JSON，与 `src/types/__snapshots__/*.json` **逐字节比对**（`serde_json` 已在依赖里，**零新增依赖**）。改了 Rust 类型忘了改前端类型或快照 ⇒ 红灯。**替代物是这条用例，不是生成器。**
- [ ] **`TaskQuery` 的 IPC 口（D3 裁决）**：`TaskQuery` 的字段是 `TaskFilter`/`Page`（`src-tauri/src/storage/task_repo.rs:151`/`:163`/`:175`），命令层构造它必踩 `commands 禁 storage::`。在 `services::catalog` 新增 IPC 友好的请求 DTO **`TaskQueryRequest`**：
  - `statuses: Vec<String>`——每个值过 `TaskStatus::parse`（`src-tauri/src/domain/task.rs:51`）校验，非法值 ⇒ 稳定错误码；
  - `project: ProjectSelector`——三值，对应 `task_repo::ProjectFilter`（`Any` / `None` / `Id(String)`）；JSON 形状固定 `"any"` / `"none"` / `{"id":"<project_id>"}`；
  - `context_tag_id: Option<String>`（必须是 `Context` 类标签，那条拒绝留在服务入口）；
  - `limit: i64`、`offset: i64`（1..=100 / >=0，越界由既有的 `Page` 校验拒绝）；
  - `expected_data_epoch: String`。
  并实现 `impl TryFrom<TaskQueryRequest> for TaskQuery`，内部构造 `TaskQuery` 后仍走**唯一那条读路径** `catalog::list_tasks_filtered`（`services/catalog.rs:636`）——不新增第二条查询实现。**读路径的项目状态用 `ProjectStatus::parse`**（`src-tauri/src/domain/project.rs:41`），**不是**写路径的 `catalog::parse_project_status`（`services/catalog.rs:76`，它额外拒绝 V0.1 不写的 `done`）。
- [ ] 命令**按意图命名、一次完成业务事务**（00 §4）；Today 等聚合视图一次返回，列表避免 N+1。**Today 只由两条命令组合**：`plan_for` + `Coordinator::snapshot`（见「边界」节），不在前端或命令层另做聚合。
- [ ] 所有修改类命令都带 `expected_data_epoch`，更新既有对象带 `expected_row_version`；`WriteEnvelope` 从 **`crate::envelope`** 引用（权威路径在 crate 根，`commands::envelope` 的转发路径已删），命令层用 `for_create`/`for_update` 构造——`src/envelope.rs` 的注释把 IPC 定为**规范构造点**。
- [ ] **错误透传（R3 裁决）**：现存**五**个码 `RECOVERY_REQUIRED` / `DATA_EPOCH_MISMATCH` / `VERSION_CONFLICT` / `DOMAIN_ERROR` / `STORAGE_ERROR`（`src-tauri/src/error.rs:41`–`:45`）原样透传到前端，载荷遵循共享 `ErrorResponse{code,message,authority,requires_handshake}`（`src/error.rs:189`）。失败响应统一走 `services::error_response::capture_error_response`（`src-tauri/src/services/error_response.rs:29`），在**原事务结束后、同一串行边界内**捕获，不另调 `timer.snapshot` 补版本（总纲 §10 门禁第 4 项）。**`DATA_RESTORE_IN_PROGRESS` 由 P6 加入后再登记为透传项**（P6 Task 4；P7 不实现、不声称）。
- [ ] **前端外壳与 `greet` 的去留（R2）**：`greet` 命令（`src-tauri/src/lib.rs:22`，注册在 `:30`）与 `App.tsx` 的模板连接页（仓库根 `src/App.tsx:15` 调 `invoke("greet")`）**在 P7 一并替换**：`lib.rs` 只注册真实命令、删掉 `greet`；`src/App.tsx` 改为真实首页外壳（固定布局 + 页面挂载）；`src/main.tsx`（antd `ConfigProvider`）与 `index.html`（标题/挂载点）按真实首页调整，保留这两个文件的现有职责。总纲 §5 第 7 条写的"既有前端文件与 `greet` 命令保留不动，**直到 P7 明确处理**"——本计划就是那次明确处理。
- [ ] **`APP_ID` 与 `identifier` 统一（D4 裁决）**：`src-tauri/tauri.conf.json:5` 的 `identifier` 由 `com.worktrace.app` 改为 **`com.worktrace.desktop`**，与 `platform::paths::APP_ID`（`src-tauri/src/platform/paths.rs:9`）一致。**库路径不动**：数据目录仍是 `%APPDATA%\com.worktrace.desktop\`，`worktrace.db`（`paths.rs:33`）与同目录的 `instance.lock`（`paths.rs:40`）保持现状；P6 的备份/日志目录沿用同一个 `app_data_dir()`，**不因改名迁移任何数据**（P7 是首次接线，没有待迁移的旧库）。
- [ ] **非法参数必须拿到稳定错误码**：IPC 形状里 `mode`/`timer_kind`/`statuses` 一律是**字符串**，在命令体内用 `SessionMode::parse`/`TimerKind::parse`（`src-tauri/src/domain/session.rs:82`/`:107`）、`TaskStatus::parse` 校验后再构造服务请求——**不要**依赖 serde 的枚举反序列化：它的错误拿不到 `ErrorResponse` 的 `code`，非法取值会退化成 Tauri 的反序列化错误。请求类型上的 `Deserialize` 因此只用于**已是强类型**的入参（测试与内部复用），IPC 路径不依赖它。
- [ ] 测试：命令层不直接引用 `rusqlite`/`storage::`（`src-tauri/scripts/check-layers.ps1` + grep 自查）；未知/非法参数返回稳定错误码而不是 panic；每个响应 DTO 的 JSON 快照与 `src/types/__snapshots__/*.json` 一致；`TaskQueryRequest` 的非法状态串与越界分页 ⇒ 领域/校验错误且**零写入**。
- [ ] **本阶段不做（依赖 P3/P5/P6）**：恢复确认相关命令（P3）；统计与导出命令（P5，P8 接入）；维护态分流（错误码由 P6 引入、展示由 P8 做）。

## 前置任务：前端测试基建（Task 2 开工前，Windows 侧）

文件：`package.json`（scripts 与 devDependencies）、`vitest.config.ts`（新建）、`src/state/__tests__/`（Task 2 起使用）、`tsconfig.json`（仅在选用全局 API 时改）。

- [ ] 仓库当前**没有** vitest/jsdom。在 **Windows 侧**（不是 WSL）离线安装：`pnpm add -D vitest jsdom @testing-library/react @vitejs/plugin-react`。实测 `D:\.pnpm-store\v11` 的 `index.db` 里有这四个包的完整条目（`vitest@5.0.1`、`jsdom@30.1.1`、`@testing-library/react@16.3.3`、`@vitejs/plugin-react@6.1.1`）。
- [ ] **不引入 `@testing-library/jest-dom`**：断言用原生 DOM 属性（`textContent` / `getAttribute` / `disabled` / `value`），不为断言再引第二个库。
- [ ] **不引入路由或状态库**：`react-router`/`zustand`/`jotai`/`redux`/`@tanstack/*` 在该 store 里 **0 条目**（离线取不到）⇒ 固定布局 + `useLocalState`/`useSyncExternalStore` 的既定路线不变。
- [ ] `vitest.config.ts`：`environment: "jsdom"`，并复用 `@vitejs/plugin-react`（`vite.config.ts` 已在用它）。
- [ ] `package.json` 增加脚本 `"test": "vitest run"`（`build` 保持 `tsc && vite build`）。
- [ ] **`tsconfig` 与测试全局**：`tsc && vite build` 会类型检查 `src/**`（`tsconfig.json` 的 `include: ["src"]`），所以测试文件**显式 `import { describe, it, expect } from "vitest"`**（首选，不动 `tsconfig`）；若坚持全局 API，则必须在 `tsconfig.json` 加 `"types": ["vitest/globals"]`，否则 `tsc` 会拒绝测试全局、`pnpm build` 直接失败。
- [ ] 装完先跑一条 smoke 用例（`pnpm test` 至少一条断言通过）再写真正的测试；**装不上（离线缺包 / peer 冲突）就停下来回报**，不要绕路换测试框架。

## Task 2：前端状态镜像

文件：`src/state/domainState.ts`（新建）、`src/state/hooks.ts`（新建）、`src/state/__tests__/`（新建）、`src/types/ipc.ts`（Task 1 新建，这里是消费方）、`src/ipc.ts`（Task 1 建立的命令转发层）。

- [ ] **每个 JS 上下文只有一个 `domainState` 订阅入口**（00 §6 原文）；页面通过 hooks 读取，卸载时清理监听。多窗口各自一个上下文，互不共享内存。
- [ ] 事件只作**缓存失效**，计时 tick 只更新**展示值**（00 §6）；两者都不触发业务判断。
- [ ] 窗口启动顺序：**先监听并暂存通知，再拉一致快照**（00 §5 规则 1），避免快照与通知之间丢事件。首次握手用 `services::handshake::get_revision`（`src-tauri/src/services/handshake.rs:25`，不要求已知 epoch），拿到 epoch 后再拉业务快照。
- [ ] 收到快照后丢弃**同 epoch 且 `revision <=` 快照版本**的通知；未知 epoch 的通知**只触发重新握手**，不直接接纳（规则 2）。
- [ ] 可见窗口**至多每 30 秒**校验 `get_revision`（返回 epoch+revision）；隐藏窗口在显示前校验（规则 4）。末次通知丢失仍要能收敛。**此 30 秒是前端校验 `get_revision` 的周期**，与 `Coordinator` 的 `HEARTBEAT_INTERVAL_MS = 30_000`（`src-tauri/src/services/timer/coordinator.rs:368`，**检查点**频率）**无关**：数字相同纯属巧合，改一个不影响另一个。
- [ ] 计时状态的前端判断顺序：先查 `data_epoch`、`run_id`、`session_version`，再比 `tick_seq`；**旧状态生成的 tick 即使序号较新也不能覆盖**暂停/切换后的展示（00 §5）。较新 `session_version` 的未知 tick 先触发计时快照，**不自行推导状态跃迁**。
- [ ] **TS 侧逐条引用 `services/events.rs` 的四条规则，并以 Rust 侧为规范文本**（`RevisionGate` 是那套规则的唯一规范实现，前端镜像不得自行解释或增删分支）；本阶段 Rust 侧只有测试在用 `RevisionGate`，**两侧的交叉校验机制归本任务**（评审 M2 的另一半）：至少要做到「改一条规则必须同时改两侧」，而不是靠人记得。
- [ ] 先用 `useSyncExternalStore` 等简单方案（00 §6）；**不得**把"不使用状态库"当成领域权威的必要条件——以后允许评估状态库，但业务规则不得因此搬到前端。
- [ ] 测试（**前置任务必须先做完**）：乱序通知不改变展示；旧 tick 不覆盖新状态；快照版本旧于已应用时不覆盖；监听先于快照的时序正确；卸载后不再有监听泄漏。
- [ ] **本阶段不做（依赖 P3/P5/P6）**：恢复/统计相关的状态展示（P8）。

## Task 3：捕获、理清与计时控制

文件：`src/pages/Inbox.tsx`（新建）、`src/pages/Timer.tsx`（新建）、`src/components/`（按需新建）、`src/state/domainState.ts`、`src/state/hooks.ts`、`src/pages/__tests__/`（组件测试，新建）、`src/ipc.ts`（Task 1 建立）。

- [ ] **F-001 快速捕获**：主窗内输入一句话回车即创建并**立即可见**；不要求项目/标签/日期；空标题拒绝并给出可理解提示。全局快捷键属 V0.1b，不做。
- [ ] **F-002 任务理清**：Inbox 可直接置 Ready；项目/标签可选，**无强制表单**；`start` 可在同一命令内原子理清并启动（`Coordinator::start` 在**一个事务**里走 `Inbox → Ready → Doing` 两步，`src-tauri/src/services/timer/coordinator.rs:391`——前端只发一次命令，不自己拆成两步）。
- [ ] 任务状态联动与托盘"完成"在 P3 服务接入后由 P8 启用；P7 不开放尚未存在的动作，不能由前端先 finish 再改状态；历史 correct 发送 session.expected_row_version，不发送不存在的 interval 版本。
- [ ] **F-003 状态机（R13 裁决）**：**P7 只在三处展示状态——捕获、理清 Ready、开始计时**，入口形状由 02 §5 允许的跃迁决定；**完整 F-003（完成/取消/Blocked/Waiting/reopen）归 P8**（P3 的 `transition_task` 接入后）。「**即使出现也要被 Rust 拒绝**」这条要求**保留**，但 **P7 无对应入口可验**（`transition_task` 尚不存在），该项验收由 P8 执行；显式 reopen 同理。
- [ ] **计时控制**：开始/暂停/继续/结束四个动作各对应一条命令，不做本地状态机；暂停值冻结、到点只提示不自动完成，均由 P2 的 DTO 驱动展示。
- [ ] 归档项目不出现在新建任务的选择列表；**即使前端漏过滤，服务也会拒绝**（P4 的保证），前端要正确显示该错误。
- [ ] **错误文案（R8 裁决）**：用户文案**由 Rust 的 `ErrorResponse.message` 提供**（`src-tauri/src/error.rs:50`–`:57`，已全中文；`AppError::Domain{detail}` 的 detail 本身就是用户文案）。前端**只按 `code` 决定行为**——提示 / 重新握手（`requires_handshake`）/ 冲突刷新——**不维护第二份「码 → 文案」表**；**未知 `code` 直接展示 `message`**（不是"未知错误"）。`authority.records` **按 `kind` + `id` 匹配，不按下标**（白名单顺序 ≠ 请求顺序，见 `src/error.rs` 的 `AuthorityTarget`/`RecordVersion`）。
- [ ] 测试：空标题被拒；回车创建后列表立即可见；start 只发一次 IPC；非法跃迁不产生请求（且服务拒绝时提示正确）；**提示文案取自 Rust 的 `message`**（前端没有「码 → 文案」表可测）；未知 `code` 展示 `message` 而不是"未知错误"。
- [ ] **本阶段不做（依赖 P3/P5/P6）**：完成/取消/Blocked/Waiting/reopen 的入口与联动（P8，依赖 P3）；恢复确认页与待确认区间展示（P8/P5）；HUD 与全局热键（V0.1b）。

## Task 4：托盘与窗口生命周期

文件：`src-tauri/src/platform/tray.rs`（新建，或等价位置）、`src-tauri/src/platform/mod.rs`（登记）、`src-tauri/Cargo.toml`（给 `tauri` 加 `features=["tray-icon"]`）、`src-tauri/tauri.conf.json`、`src-tauri/capabilities/default.json`、`src-tauri/src/commands/mod.rs`（托盘动作复用同一批命令）、`src-tauri/tests/manual-shell.md`（新建：人工验收记录模板，Task 6b 一起用）。

- [ ] **F-011 托盘（R4 裁决）**：P7 实际提供**四项**——当前任务、暂停、快速捕获、退出。**「完成」是预留项**：P3 的 `transition_task` 服务入口接入后**由 P8 启用**，P7 不开放尚未存在的动作（也不由前端先 finish 再改状态）。**关闭所有窗口后托盘仍可用且计时继续**——验收硬条件，须在真实环境手测。
- [ ] **托盘 feature 的文件与离线可行性**：`src-tauri/Cargo.toml:21` 的 `tauri = { version = "2", features = [] }` 改为 `features = ["tray-icon"]`。**离线可行**（Windows 侧 cargo 缓存已有 `tray-icon-0.24.2/0.25.1`、`muda`、`tao`；`Cargo.lock` 里也已有 `tray-icon 0.25.1`/`muda 0.20.0`——可选依赖本来就在解析图里），**构建只在 Windows 侧跑**。**托盘图标用 `app.default_window_icon()`**，**不启用 `image-png`/`image-ico`**：`image` crate 不在 `Cargo.lock`，离线取不到。
- [ ] **F-009 窗口独立性**：关闭或隐藏所有窗口时核心继续运行；重开窗口**立即拉快照**而不是等下一次通知。托盘菜单的动作与界面动作走**同一批命令**，不另开路径。
- [ ] 显示 HUD 的托盘项属 V0.1b，本计划**不加**该菜单项。
- [ ] **退出流程（R6 裁决）**：托盘"退出"走 **Task 0 建立的显式退出入口**（`services/bootstrap.rs` → `storage/run_repo.rs` 写 `clean_exit_at`），**一个事务**里结束 `running`/`paused` 会话、保存 revision、停定时器；**`recovering` 记录保留不清**（02 §4）。不是直接杀进程，也不在托盘回调里跑长事务。
- [ ] `capabilities/default.json`：这份名单现在只有 `"windows": ["main"]`——**主窗之外的新窗口必须逐个加进来**，否则它的 JS 没有任何权限。Task 6a 的 `sync-lab` 窗口要在这里登记（见 Task 6a）。
- [ ] 测试（能自动化的部分）：托盘动作与界面动作调用同一命令；关窗不触发退出；重开窗口触发快照。**其余必须人工验收。**
- [ ] **本阶段不做（依赖 P3/P5/P6）**：托盘"完成"的启用与 F-003 完整联动（P8，依赖 P3）；维护态下的托盘禁用（P6）。

## Task 5：Projects 与轻量 GTD 列表

文件：`src/pages/Projects.tsx`（新建）、`src/pages/Tasks.tsx`（新建）、`src/ipc.ts`（Task 1 建立）、`src/state/domainState.ts`、`src/pages/__tests__/`（新建）。

- [ ] F-004 界面归属本计划：Projects 页创建、改名、归档；项目详情列出任务并可新建第一条行动。归档保留历史，提交带 epoch/项目版本；确认归档后更新列表。列表用 `catalog::list_projects`（含归档/done 历史可读）与 `catalog::list_selectable_projects`（只含 active，供选择器），**不直调 `project_repo`**。
- [ ] Tasks 页使用 P4 查询（Task 1 的 `TaskQueryRequest` → `list_tasks_filtered`）：下一步行动=Ready，等待中=Waiting，阻塞=Blocked，按情境选择 Context。可结合项目筛选；不把 Waiting/Blocked 混为同一状态。
- [ ] 展示无项目、无情境、空列表、加载、查询失败、分页及版本冲突（`VERSION_CONFLICT` 按 R8 只决定**行为**：刷新 + 展示 Rust 给的 `message`）；改变条件重置分页，旧响应不能覆盖新筛选结果（比 `data_epoch`/`revision`，不比到达顺序）。筛选与计数都用服务数据（`TaskQueryResult.total` 与 `tasks` 出自**同一个读事务**，`services/catalog.rs:616`）。
- [ ] 理清项目级想法时可创建项目并手工新增行动；不承诺原 Inbox 项自动转换、迁移来源/标签或删除。AI 生成下一步行动不属本次首版范围。
- [ ] 测试：项目 CRUD 与归档保留任务、版本冲突、多标签任务只出现一次、筛选交集、分页、空列表、旧筛选响应被忽略。
- [ ] **本阶段不做（依赖 P3/P5/P6）**：统计视图与导出（P5/P8）；归档/完成状态的批量操作（V0.1 无此入口）。

## Task 6：双窗口同步实验与外壳验收

文件：`src-tauri/src/platform/sync_lab.rs`（新建，或等价位置：第二个窗口 `sync-lab` 的 `WebviewWindowBuilder` 创建与 dev 注入开关）、`src-tauri/src/platform/mod.rs`（登记）、`src-tauri/src/services/events.rs`（dev 注入点）、`src-tauri/capabilities/default.json`、`src-tauri/tests/manual-shell.md`（记录模板，新建）、`src-tauri/scripts/check-layers.ps1`（沿用 P1，不改）。

### 6a 双窗口同步实验（06 §4，必须在 P8 之前跑完并记录结论）

- [ ] **第二个窗口**：label **`sync-lab`**，由 Rust 侧用 `WebviewWindowBuilder` 创建（实验时开，不做成 `tauri.conf.json` 的静态窗口）；**`src-tauri/capabilities/default.json` 的 `windows` 必须包含 `sync-lab`**，否则该窗口的 JS 无权限调命令（现在只有 `"main"`）。
- [ ] 验证"先监听后快照"。三种竞态**用明确的注入手段造**（不能只写"人为制造"）：
  - **(a) 末次事件丢失**：`services/events.rs` 的广播出口加 `#[cfg(debug_assertions)]` 丢弃开关（dev 命令 `__p7_drop_next_event`），丢一次通知后确认展示在 30 秒 `get_revision` 周期内仍收敛；
  - **(b) 旧响应晚到**：dev 命令 `__p7_delay_next_query_ms(ms)` 让窗口 B 的下一次查询延迟返回（**先取数据再 `sleep`，绝不跨 `await` 持 `Connection`**），A 窗口在延迟窗口内先暂停再继续，B 的旧响应必须被丢弃、不覆盖新状态；
  - **(c) 乱序通知**：dev 命令 `__p7_replay_event(seq)` 用旧 `event_seq` 重播一条通知，验证同 epoch 且 `revision <=` 已应用版本的通知被丢弃。
  三条注入**只在 debug 构建编译**（`#[cfg(debug_assertions)]`），并加一条测试断言发布 handler 列表里没有它们。
- [ ] 记录机器/系统版本与观察结果到 `src-tauri/tests/manual-shell.md`。

### 6b 时序验证、外壳人工验收与完成门槛

- [ ] **时序验证**：窗口 A 暂停 → 窗口 B 的展示在 30 秒内收敛（规则 4 的 `get_revision` 校验）；窗口 B 在隐藏后重新显示时先校验再展示。
- [ ] **外壳人工验收**（不能用单元测试代替，08 §6）：F-001/F-002（捕获、理清、计时非法请求）；F-003/F-011 完整联动由 P8 验收；F-009（关掉全部窗口后托盘可用、计时继续；重开立即拉快照）；F-020 的界面侧（多窗口一致性）。
- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [ ] 完成门槛：`cargo fmt --check`、`cargo test`、`cargo clippy --all-targets` 全绿；前端 `tsc`、`pnpm build` 与 `pnpm test` 通过；分层检查 **`src-tauri/scripts/check-layers.ps1`** 通过（现查**六条**规则：commands 禁 `storage::|rusqlite|Connection`、domain 禁 `rusqlite|std::fs|platform::|storage::|commands::|services::`、storage 禁 `platform::|commands::|services::`、services 禁 `std::time|SystemTime|Instant::now|commands::`（含 `services/` 不得直接取时间）、**platform 禁 `crate::services::|crate::storage::|crate::commands::`**（P7 Task 0 加的第五条：托盘"复用同一批命令"正是 platform→services 反向边的入口）、以及**入口点规则**：`lib.rs`/`main.rs` 禁 `Db::open|migrate(|run_repo::`（把"唯一启动入口"从约定变成机器检查））；**P1/P2/P4 测试无回归**。
- [ ] **不得把仓储/服务层测试标为"UI 已验收"**（P4 的约定）。人工验收记录要能对上具体版本与机器。
- [ ] **本阶段不做（依赖 P3/P5/P6）**：F-003/F-011 的完整联动（P8）；平台事件的实机验收——P7 只登记步骤，结论由实机跑出、P8 复核（见文末）；「多入口开发/打包路径」与 Windows 打包验证（00 §7，登记在文末）；维护态验收（P6）。

---

## 下游

- **P8** 接手"统计 / 恢复 / 导出"的界面，以及 R-04 的发布产物门禁与 V0.1 的完整人工验收。P8 直接在本计划搭好的外壳、状态镜像与托盘上加页面，**不重做外壳**。
- 本计划完成时，V0.1 已经"能用"：能捕获、理清、计时，关窗后仍继续。**但还不能交付**——统计、恢复确认与导出尚未接界面。

## 评审补充：恢复提示与时钟校正

- [ ] 展示独立全局待确认数量/入口，来自 P3 查询；不使用当前快照 pending_ms/needs_attention() 代替。P3 未接入前注明接口依赖，不能宣称旧记录提醒已完成。
- [ ] 系统事件实测验证历史 boundary 不推进采样 last，当前样本检测一次；可信离开边界与同时发生的改时/单调钟故障不能互相覆盖。

- [ ] IPC 失败响应接入 services/error_response::capture_error_response 与 ErrorResponse：读取提交后的 epoch/revision 和请求对象版本；在同一串行边界且原事务结束后捕获，不另调 timer.snapshot。requires_handshake 时先重新握手，不自动重试非幂等命令。

## P1～P4 兼容接入前置门禁

接线前完成[总纲 §10](2026-10-03-v01-plan-index.md)的三项服务兼容补全：完整 epoch/revision 响应信封、完整项目列表服务、仅 active 项目可新关联的统一检查。之后再接所有错误响应的权威捕获；不得由 commands 直连仓储或补读元数据绕过门禁。P1/P2/P4 核心已验收不代表上述补全或本阶段 IPC 已实现。

## 已实现的接入基座（2026-10-04）

首次与恢复后身份握手、周期版本校验统一调用 services::handshake::get_revision(db)，无需 expected_data_epoch；业务查询仍必须带握手得到的 epoch。顺序为监听并缓冲事件→握手→业务快照；epoch 变化时丢弃旧请求结果并重新握手。get_revision 仅返回身份/版本，不代替完整业务视图。P2 命令结果在提交后从一个读事务重建，结果 revision 与 snapshot.revision 相同。分层脚本已检查 commands 禁止依赖 storage/rusqlite/Connection；IPC 和事件协议仍待本阶段实现。

## 仍待与归属（2026-10-04 登记）

- **平台事件实机验收**（锁屏 / 休眠 / 唤醒 / 改时 / 关窗后采样 / 事件到达延迟）目前**无归属**：登记为「**P7 实机步骤 + P8 复核**」。`docs/validation/p2-clock-mapping.md` §6/§7（`:156`–`:172`）已声明这些**未验证、不得当成已验证**：探针是前台进程，证明不了关窗后仍采样，也证明不了系统事件的可靠性与到达延迟。
- **「多入口开发/打包路径」与 Windows 打包验证**（`00-architecture.zh.md` §7 的待验证项）同样**无归属**：登记到 P8，与 R-04 的发布产物门禁一起做。
- **`@mui/material` 与 `@emotion/*` 是模板遗留死依赖**（`src/App.tsx` 未使用，`package.json` 里仍在）：**只登记，不在 P7 删**——删依赖属清理任务且需用户确认。
- **维护态错误码**：P6 Task 4 引入后，再登记为 Task 1 的透传项（P7 只登记、不实现）。

## 开工前已核实（2026-10-04）

- **复核基线**：仓库 HEAD `ae9ec00`（工作树干净），`src-tauri/` 镜像与之一致（仅 `main.rs` 未镜像、`tests/` 逐字节相同）；门禁 **357 passed**（据 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/progress.md` 第六轮记录，本次未重跑）。**本计划尚未实施**——本节只写复核过的事实，不含任何「已验证通过」的实施结论。
- **符号面**：`handshake::get_revision`/`RevisionSnapshot`、`error_response::capture_error_response`、`WriteEnvelope`（crate 根）、`Coordinator::{start,pause,resume,finish,snapshot,heartbeat}`、四个读服务的入参与返回、`src-tauri/scripts/check-layers.ps1` 的四条规则**全部对上**；`AppError::code()` 只有五个码；P6 才引入的维护态错误码在本仓 0 命中（属 P6 Task 4）。
- **改名与不一致（已按裁决写进本计划）**：分层脚本真名是 `src-tauri/scripts/check-layers.ps1`；`tauri.conf.json` 的 `identifier` 是 `com.worktrace.app`，与 `platform::paths::APP_ID = "com.worktrace.desktop"` 不一致；托盘需要 `tauri` 的 `tray-icon` feature。
- **离线可行性（实测缓存，不是推断）**：DTO 生成器 `ts-rs`/`specta`/`typeshare` 在 Windows 与 WSL 两侧 cargo 缓存 **0 命中** ⇒ 不上生成器；`tray-icon`/`muda`/`tao` 在 Windows cargo 缓存与 `Cargo.lock` 里都在 ⇒ 托盘离线可行；`image` **不在** `Cargo.lock` ⇒ 不启用 `image-png`/`image-ico`；`D:\.pnpm-store\v11` 的 `index.db` 有 `vitest@5.0.1`、`jsdom@30.1.1`、`@testing-library/react@16.3.3`、`@vitejs/plugin-react@6.1.1` 的完整条目，`react-router`/`zustand`/`jotai`/`redux`/`@tanstack/*` 则是 **0 条目**。
- **本计划要补的代码缺口**：`storage/` 无 `run_repo.rs`（`application_run` 只有 schema，`src/storage/schema_v1.rs:28`）；`platform/` 只有 `clock.rs`/`paths.rs`；`services/` 无 `bootstrap.rs`/`events.rs`；`commands/` 只有模块头注释；服务层**除 `handshake::RevisionSnapshot` 外没有任何 serde derive**；`TaskQuery` 的字段是 `storage::task_repo` 类型（命令层不能构造）；`session_repo::running_foreground` 不带 `run_id`。
- **本次修订**：按复核结论落地 **D1–D6** 与 **R1–R14**，并把原「提前交付的平台边界」升格为 **Task 0**；因为 Task 0 挪到最前，总纲与 `p1-p4-review-backlog.md` 里指向本计划的行号引用一并更新。
