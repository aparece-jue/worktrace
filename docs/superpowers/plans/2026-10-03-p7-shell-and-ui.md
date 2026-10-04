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
- [ ] **单实例**（`platform/single_instance.rs`，`platform/` 叶子、不含业务规则）：OS 级文件锁，锁文件用既有的 `platform::paths::instance_lock_file()`（`src-tauri/src/platform/paths.rs:40`，与库同目录）。拿不到锁的进程**通知既有实例后退出**：**不打开库、不迁移、不建 `application_run`、不启动计时**（F-016）。"唤起既有主窗"是**通知**，与锁分离。**交付边界（2026-10-04 实施后订正）**：Task 0 只交付**发送侧**（`single_instance::request_activation`）与**接收原语**（`take_activation_request`）；把请求变成"抬起主窗"的消费侧需要窗口对象，归 **Task 4**——因此本任务结束时 `take_activation_request` 还没有生产调用者，这是分割点，不是遗漏。**已闭环（2026-10-04 Task 4 落地）**：消费侧是 `platform::window::spawn_activation_watcher`（轮询请求文件 → `plan_activation` → 抬起/重建主窗），由 `lib.rs` 在启动成功后挂上；决策函数的断言在 `tests/shell_lifecycle.rs`，真机步骤在 `src-tauri/tests/manual-shell.md` §3。
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
  - **请求类型加 `Deserialize`**：`SessionRequest`/`ResumeRequest`（`services/timer/coordinator.rs:52`/`:60`）、`DailyPlanQuery`（`services/daily_plan.rs:129`）、`ProjectTarget`/`ProjectSelector`/`TaskQueryRequest`（`services/catalog.rs`）。**`StartRequest` 不加**（2026-10-04 实施后订正，见计划末尾「Task 1 的 Rust 半边已实现」一节）：它的 IPC 形状是「`mode`/`timer_kind` 为字符串」，给它加 `Deserialize` 就得给这两个枚举派生反序列化，而那条路径拿不到 `ErrorResponse.code`（见下面「非法参数」那条）；本仓又不留「定义了没人调」的 API，所以不造这份死面——IPC 侧收的是 `commands::StartTimerRequest`（字符串字段），命令体解析后再构造 `StartRequest`；
  - **行类型也要能序列化**：`ProjectList.items`/`TaskQueryResult.tasks`/`DailyPlanView.tasks` 直接装 `ProjectRow`/`TagRow`/`TaskRow`，所以 `storage/{project_repo,tag_repo,task_repo}.rs` 的行类型加 `Serialize`；但**命令层不得出现 `storage::` 的名字**（分层门禁），命令层只是把服务结果原样交回 IPC；
  - **状态/类别枚举不派生 `Serialize`**：在各自领域文件里写一份 `Serialize` 实现，内部 `serialize_str(self.as_str())`（`domain/task.rs:36`、`domain/project.rs:28`、`domain/tag.rs:22`、`domain/session.rs:26`/`:100` 已有 `as_str()`）——等价于在每个字段上写 `serialize_with`，但**只有一份**。⚠️ **JSON 里就是 `as_str()` 那套字符串，不是统一小写**（2026-10-04 订正）：`TaskStatus`（`"Ready"`/`"Inbox"`）与 `TagKind`（`"Context"`）**首字母大写**，与 schema 的 CHECK 逐字一致；只有 `ProjectStatus`（`active`）、`SessionState`（`running`）、`TimerKind`（`stopwatch`）是小写。**Task 1b 的 TS 联合类型必须照这个写**，写成全小写就是永远不命中的静默 bug；
  - **前端类型手写** `src/types/ipc.ts`（不手写第二份业务规则）；
  - **一致性由一条 Rust 集成用例钉住**：`tests/ipc_snapshots.rs` 把每个响应 DTO 序列化成 JSON，与 `src/types/__snapshots__/*.json` **逐字节比对**（`serde_json` 已在依赖里，**零新增依赖**）。改了 Rust 类型忘了改前端类型或快照 ⇒ 红灯。**替代物是这条用例，不是生成器。** 快照按 LF 存放（仓库根 `.gitattributes` 钉了 `text eol=lf`——本仓库 `core.autocrlf=true`，不钉的话换台机器 checkout 出 CRLF 会让逐字节比对在没改任何类型的情况下变红）；
  - **Task 1b 的验收项（这条一致性承诺的另一半，2026-10-04 登记）**：快照只钉住 **Rust ↔ JSON**，手写的 `src/types/ipc.ts` 与快照之间**没有机械联系**。Task 1b 必须补一条 vitest 用例：读 `src/types/__snapshots__/*.json`，对每个类型做**键集合 + 字面量联合**的类型级断言（例如状态联合类型必须与快照里出现过的取值集合一致）。没有这条，「改了 Rust 类型忘了改前端类型」仍然只能靠人记得。
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
- [ ] **写命令广播 `domain.changed`（2026-10-04 实施，fix round 1 裁决）**：00 §5 的「同一 epoch 内一次业务写对应一条 `domain.changed`」。落点是命令层：拿到写结果后、**释放锁之前**（同一临界区，所以「广播顺序 = 提交顺序」）广播一条，**载荷就是该命令的响应 DTO**。**仅 `Changed` 才广播**——`Unchanged`（改同名、重复打标、重复加入计划）没有 revision 变化，也就没有缓存要失效；判据是 `storage::WriteOutcome::into_parts()` 的第二个返回值。**响应形状不变**：不加 `{changed, value}` 信封（那会改 15 份快照，而且规格没给它位置），这一位只用于「要不要广播」这个内部判断。广播失败只记诊断，不影响命令结果、不回滚已提交业务。计时命令（`start`/`pause`/`resume`/`finish`）没有幂等重复这一支，能走到广播就说明跃迁真的提交了。
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

文件（**2026-10-04 落地后订正为实际交付的文件**）：`src-tauri/src/platform/tray.rs`（新建：菜单装配）、`src-tauri/src/platform/window.rs`（新建：主窗生命周期与唤醒接收的消费侧）、`src-tauri/src/platform/mod.rs`（登记两个新模块）、`src-tauri/Cargo.toml`（给 `tauri` 加 `features=["tray-icon"]`）、`src-tauri/src/commands/mod.rs`（托盘动作复用同一批命令）、`src-tauri/src/lib.rs`（接线：托盘、`RunEvent::ExitRequested`、唤醒轮询）、`src-tauri/tests/shell_lifecycle.rs`（新建：能自动化的那半边）、`src-tauri/tests/manual-shell.md`（新建：人工验收记录模板，Task 6b 一起用）。**`tauri.conf.json` 与 `capabilities/default.json` 本轮未改**：托盘是 Rust 侧手工装配的（不是配置里的 `app.trayIcon`），主窗 label 仍是 `main`、本来就在权限名单里；两处的一致性由 `tests/shell_lifecycle.rs` 读文件核对。

- [ ] **F-011 托盘（R4 裁决）**：P7 实际提供**四项**——当前任务、暂停、快速捕获、退出。**「完成」是预留项**：P3 的 `transition_task` 服务入口接入后**由 P8 启用**，P7 不开放尚未存在的动作（也不由前端先 finish 再改状态）。**P7 的落法**：菜单里放一个**禁用项**「完成（P8 启用）」（id `tray.finish_reserved`，`action: None`）——菜单看得见、点了不会有动作，也不存在一条通往尚未存在服务的路径。**关闭所有窗口后托盘仍可用且计时继续**——验收硬条件，须在真实环境手测（步骤见 `src-tauri/tests/manual-shell.md` §2）。
- [ ] **托盘 feature 的文件与离线可行性**：`src-tauri/Cargo.toml:21` 的 `tauri = { version = "2", features = [] }` 改为 `features = ["tray-icon"]`。**离线可行**（Windows 侧 cargo 缓存已有 `tray-icon-0.24.2/0.25.1`、`muda`、`tao`；`Cargo.lock` 里也已有 `tray-icon 0.25.1`/`muda 0.20.0`——可选依赖本来就在解析图里），**构建只在 Windows 侧跑**。**托盘图标用 `app.default_window_icon()`**，**不启用 `image-png`/`image-ico`**：`image` crate 不在 `Cargo.lock`，离线取不到。**实测（2026-10-04）**：`cargo test --offline` 在该 feature 下全绿，**`Cargo.lock` 逐字节未变**（可选依赖本来就在解析图里，启用 feature 不引入新包）。
- [ ] **F-009 窗口独立性**：关闭或隐藏所有窗口时核心继续运行；重开窗口**立即拉快照**而不是等下一次通知。托盘菜单的动作与界面动作走**同一批命令**，不另开路径。**P7 的落法**：`RunEvent::ExitRequested { code: None }`（用户关掉最后一个窗口）→ `api.prevent_exit()`，于是进程与托盘留下、周期采样继续；`code: Some(_)`（程序化 `exit`，含托盘「退出」）一律放行。判定函数 `platform::window::should_prevent_exit(code)` 与唤醒决策 `plan_activation(requested, exists)` 都有断言（`tests/shell_lifecycle.rs`）。
- [ ] 显示 HUD 的托盘项属 V0.1b，本计划**不加**该菜单项。
- [ ] **退出流程（R6 裁决）**：托盘"退出"走 **Task 0 建立的显式退出入口**（`services/bootstrap.rs` → `storage/run_repo.rs` 写 `clean_exit_at`）：**先停定时器（不进事务）**，其余四项——结束 `running`/`paused` 会话、写 `clean_exit_at`、保存 revision、清活动阶段——在**同一个事务**内完成；**`recovering` 记录保留不清**（02 §4）。「停定时器」为什么不在事务里见 Task 0 第 4 条（2026-10-04 订正：此行原先与那里矛盾）。不是直接杀进程，也不在托盘回调里跑长事务。**P7 的落法**：`commands::tray_quit_impl` 内部就是 `RunningApp::shutdown()`（唯一入口），托盘只是它的第二个调用方；执行切到 `spawn_blocking`，不在 UI 回调里开事务。**退出事务失败时**（例如库里有一条结束不了的会话）记诊断并**以非零码退出**——事务已回滚、库是一致的，这一次 run 以「没有 `clean_exit_at`」结束正是恢复扫描的输入（F-015），把用户困在没有窗口的托盘里更糟。
- [ ] `capabilities/default.json`：这份名单现在只有 `"windows": ["main"]`——**主窗之外的新窗口必须逐个加进来**，否则它的 JS 没有任何权限。Task 6a 的 `sync-lab` 窗口要在这里登记（见 Task 6a）。**主窗重建后 label 不变**（`platform::window::MAIN_WINDOW_LABEL`），所以重建出来的窗口照样在这份名单里；「label 与配置/权限名单一致」由 `tests/shell_lifecycle.rs` 读两个文件核对。
- [ ] 测试（能自动化的部分）：托盘动作与界面动作调用同一命令；关窗不触发退出；重开窗口触发快照。**其余必须人工验收。****P7 实际钉住的**（`tests/shell_lifecycle.rs`，11 条）：菜单四项 + 预留禁用项 + id↔动作一一对应；`should_prevent_exit` 两个分支；`plan_activation` 四格真值表；主窗 label 与 `tauri.conf.json`/`capabilities` 一致；托盘暂停与 IPC 暂停**效果逐项相等**、没有会话/已暂停时零写入零广播；托盘退出走显式退出入口（`clean_exit_at` 落库、`recovering` 保留、先停定时器）；没有窗口对象时采样照跑。「重开窗口立即拉快照」的 Rust 半边 = `Rebuild` 分支（全新页面加载 ⇒ 前端挂载时先握手再拉快照），前端那一半在 Task 2。
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

首次与恢复后身份握手、周期版本校验统一调用 services::handshake::get_revision(db)，无需 expected_data_epoch；业务查询仍必须带握手得到的 epoch。顺序为监听并缓冲事件→握手→业务快照；epoch 变化时丢弃旧请求结果并重新握手。get_revision 仅返回身份/版本，不代替完整业务视图。P2 命令结果在提交后从一个读事务重建，结果 revision 与 snapshot.revision 相同。分层脚本已检查 commands 禁止依赖 storage/rusqlite/Connection；IPC 与事件协议当时仍待实现（2026-10-04 晚订正：**IPC 的 Rust 半边已落地**，见下一节）。

## Task 3 的契约补口：`TimerSnapshot` 带任务身份（2026-10-04）

**为什么这是契约补口，不是计划外扩张**：缺口是 **Task 3 实施前端时发现的**，并按纪律登记成
「待接线」而不是悄悄绕过去（见下面 Task 3 落地一节的最后一条）：「继续」按钮要发 `resume_timer`，
而 `ResumeRequest` 需要 `task_id` + `task_expected_version`（`services/timer/coordinator.rs`，
`resume` 用它 `guard_row_version_of_ro(conn, "task", …)` 并可能 `Ready → Doing`）。
但 `TimerSnapshot`（`services/timer/snapshot.rs`）里**没有任何任务字段**，24 条命令里也
**没有** session→task 的读路径（`TaskRow` 不带会话、`list_tasks` 只按 status/project/context 筛、
托盘只做 `pause`）。后果是**本窗口之外的会话**——冷启动「重开窗口」（F-009 的正常路径）
或托盘暂停之后——根本拿不到这条 paused 会话属于哪个任务，Task 3 里「继续」这一项
**在这一处是断的**。

**前端为什么绕不过去**：唯一的徒手办法是让发起 `start_timer` 的那一方把任务身份记在内存里，
再经外壳交给计时页——那**只覆盖"本窗口自己开的会话"**，冷启动（重开窗口）与托盘暂停这两条
正常路径仍然没有身份；而「会话 → 任务」的读路径在 24 条命令里不存在，前端**拿不到**，也不该
为此新增一条命令（那要动命令面）。所以正确的落点是让**已有的那条读路径**（计时快照）带上任务
身份——这也是 Task 3 落地一节当时写明的收口方式。

- **新增两个字段**（`TimerSnapshot`，仍是 `Serialize`，仍是 `services`）：`task_id:
  Option<String>` + `task_row_version: Option<i64>`。**空闲时一起是 `None`**（JSON 里是
  `null`，键始终存在——**不加** `skip_serializing_if`，否则前端声明的 `string | null`
  会在运行期变成 `undefined`）；有会话时 `task_id` 取自会话行、`task_row_version` 取自任务行。
- **任务版本每次采样重读**，不缓存在协调器内存镜像里：暂停期间改任务（例如改标题）会 bump
  `task.row_version`，缓存的值会让「继续」拿着过期版本去撞 `VERSION_CONFLICT`。
  `LiveSession` 只多带 `task_id`（会话一生不变，属身份，不是第二份真相源）。
- **纯读**：不采样（样本由入口传入）、不写库、不 bump `revision`、**不新开事务**（沿用调用方
  给的连接）——既有事务边界一处未动。分层六条规则不变。
- **同轮落地，缺一即红**：三份快照重生成（`timer_snapshot` / `timer_snapshot_idle` /
  `command_outcome`，`WORKTRACE_UPDATE_IPC_SNAPSHOTS=1 cargo test --offline --test ipc_snapshots`；
  复核 diff 只多这两行）、`src/types/ipc.ts` 的两个字段、`snapshot-contract.test.ts` 的键集合
  登记，以及 4 处 `TimerSnapshot` **完整字面量**夹具（`tsconfig.json` 的 `include` 是 `src`，
  漏一处 `pnpm build` 的 `tsc` 就红）。
- **前端仍是「待接线」，不是「已接线」**：落点是**一个函数**——
  `src/components/timerRequests.ts` 的 `buildResumeRequest(snapshot, taskIdentity)`。契约字段到了
  之后，接线就是把第二个入参换成 `snapshot.task_id` / `snapshot.task_row_version`（三行），
  并删掉 `TaskIdentity` 类型、外壳那份过渡状态与 `Inbox.onSessionStarted`；
  「身份不可得 ⇒ 按钮不出现」这条判据语义不变（它退化成"快照里没有会话/任务 ⇒ 没有可继续的会话"）。
  **本轮只补契约字段**：24 条命令的签名一条未动、没有新增命令、`src/pages/**` 与
  `src/state/**` 的实现一行未动。
- 细节、逐条「怎么才会红」与两处反向验证的原始输出见
  `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task3-contract-report.md`。

## Task 1 的 Rust 半边已实现（2026-10-04 晚）

**前端（Task 1b）要消费的 IPC 契约已冻在代码里**，细节与逐条验证见
`.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task1a-report.md`：

- **24 条命令**（`src-tauri/src/commands/mod.rs`，全部注册进 `lib.rs`）：`get_revision`；
  项目 `list_projects`/`list_selectable_projects`/`create_project`/`rename_project`/`archive_project`；
  标签 `list_tags`/`create_tag`/`tags_of_task`/`tag_task`/`untag_task`；
  任务 `list_tasks`/`create_task`/`clarify_ready`/`set_task_project`；
  今日计划 `plan_for`/`add_to_plan`/`remove_from_plan`；
  计时 `timer_snapshot`/`timer_tick`/`start_timer`/`pause_timer`/`resume_timer`/`finish_timer`。
- **每条命令只收一个参数 `request`**，字段名就是 Rust 结构体里的 snake_case；
  `mode`/`timer_kind`/`statuses` 一律是**字符串**，非法取值拿 `DOMAIN_ERROR`
  （不依赖 serde 的枚举反序列化）。三值项目选择器 `"any"`/`"none"`/`{"id":"…"}`；
  改归属是二值的 `{"bind":"…"}`/`"clear"`。
- **响应 DTO 的形状快照**在仓库根 `src/types/__snapshots__/*.json`（15 份），由
  `src-tauri/tests/ipc_snapshots.rs` 逐字节钉住；重生成用
  `WORKTRACE_UPDATE_IPC_SNAPSHOTS=1 cargo test --offline --test ipc_snapshots`。
  快照按 LF 存放（`.gitattributes` 钉了 `text eol=lf`——本仓库 `core.autocrlf=true`）。
- **事件频道名 `worktrace:event`**（`lib.rs` 的 `EVENT_CHANNEL`），Task 2 的 `domainState` 按它订阅。
- **写命令广播 `domain.changed`**（fix round 1 补上）：命令体在提交之后、释放锁之前广播一条，
  载荷 = 该命令的响应 DTO；**仅 `Changed` 才发**（`WriteOutcome::into_parts` 的第二位），
  `Unchanged` 不发；广播失败只记诊断。响应形状不变（不加 `{changed, value}` 信封）。
- **24 条命令体逐条有用例**（fix round 1 补上）：`tests/ipc_commands.rs` 调 `commands::*_impl`
  （包装只剩一行转发），覆盖「走对服务 / 带对信封 / 各自不同的终态」。
- **仍未接线**：无。Task 1b 的前端已落地（见下一节）；Task 1 的 IPC 契约、事件发送侧与
  TS 侧的一致性检查**都已接上**。

## Task 1b 的 TS 半边已实现（2026-10-04 晚）

细节与逐条验证见 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task1b-report.md`：

- **前端测试基建**（「前置任务」那一节的落地）：`pnpm add -D --offline`（**必须钉版本**，
  见下）装上 `vitest@5.0.1` / `jsdom@30.1.1` / `@testing-library/react@16.3.3`；
  `vitest.config.ts`（`environment: "jsdom"` + 复用 `@vitejs/plugin-react`）；
  `package.json` 加 `"test": "vitest run"`。测试**显式 import** vitest 的
  `describe/it/expect`，`tsconfig.json` 因此一字未动。
  ⚠️ **必须钉版本**：本地 store 只有 `vitest@5.0.1`，而**不钉版本**时 pnpm 按缓存下来的
  registry 元数据解析到 `5.0.3`，随后 `ERR_PNPM_NO_OFFLINE_TARBALL` 失败（实测）。
- **`src/types/ipc.ts`（手写）**：请求/响应 DTO、行类型与枚举取值域。枚举是
  **字符串字面量联合，且由 `as const` 数组推导**——联合类型在运行期被擦除，数组是它
  在运行期唯一的影子，也是「改枚举串即红」能落到 `vitest run` 而不是只落到 `tsc` 的前提。
- **`src/ipc.ts`**：24 条命令的薄封装（统一发 `{ request }`，三条无入参的不发这个键）、
  错误规范化（`IpcError`；非法 `project` 形状这类**走 Tauri 反序列化**、拿不到
  `ErrorResponse` 的失败兜底成客户端本地码 `TRANSPORT_ERROR`，**原始 message 原样保留**）、
  `createFreshnessGate()` + `sendVersioned()`（规则②③的水位原语）、
  `startEventSession()`（「先订阅并暂存 → 拉快照 → 按序交付」的调用顺序原语）。
- **外壳替换**：`src/App.tsx` 换成固定布局 + 三个区域占位（`data-region="nav"/"page"/"status"`），
  `index.html` 标题改为真实首页；`greet` 在**代码树里 0 命中**（`git grep -n greet -- src src-tauri index.html README.md`）。
- **TS ↔ 快照的机械检查**（Task 1 的 D1 那条验收项）：`src/types/__tests__/snapshot-contract.test.ts`
  读 `src/types/__snapshots__/*.json`，断言每个响应 DTO 的**键集合**（编译期 `keyof` 对齐 +
  运行期与快照相等，含嵌套行类型与 `authority.records`）与**字面量联合**（快照里出现过的
  取值必须落在 TS 的取值域里）。快照集合与登记表**逐份比对**（多一份、少一份都红）。
  反向验证：改 TS 的枚举串 ⇒ `pnpm test` 红；改 TS 的字段名 ⇒ `pnpm build`（`tsc`）红；
  往目录里丢第 16 份快照 ⇒ `pnpm test` 红。
- **请求 DTO 的 Rust→TS 机械联系**（2026-10-04 fix round 1，评审 I1）：
  `src-tauri/tests/ipc_requests.rs` 给 **18 个请求 DTO** 各钉一份「字段一个不少」的 JSON
  样本并逐个断言取值（`Option` 字段也要断言——serde 默认忽略未知字段，只 `unwrap()`
  挡不住字段改名），另有一条「必填字段缺失必须被拒」的对手用例。
- **外壳冒烟用例**：`@tauri-apps/api/mocks` 的 `mockIPC` 记账，断言应用能挂载、三个区域都在、
  且一个命令都没被调用。

## Task 2 的落地（2026-10-04 晚）

细节与逐条反向验证见 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task2-report.md`：

- **状态镜像**（`src/state/domainState.ts`）：每个 JS 上下文一个订阅入口——模块单例
  `domainState`；`createDomainState(deps)` 只服务测试与显式创建第二个上下文（多窗口各自
  一个上下文，天然不共享内存）。页面通过 `src/state/hooks.ts` 的 `useSyncExternalStore`
  hooks 读（`useDomainView` / `useTimerSnapshot` / `useInvalidation`），卸载由 React 调退订
  函数，不额外挂监听。`App.tsx` **未改动**：外壳不自己开会话，启动接线归 Task 3/4——
  Task 1b 那条「一个命令都不调用」的冒烟断言因此仍然成立。
- **启动顺序**（00 §5 规则 1）：`startEventSession`（先订阅暂存 → `load()` 握手 + 拉计时快照
  → 按原顺序 flush → 直通）。`load()` **在返回前把快照应用进水位**（Task 1b 说的
  `markApplied` 前置），所以启动期那条缝里的通知按规则②/④判，而不是因为"还没应用过快照"
  落到 apply 分支。计时快照拿不到**不让启动失败**（协调器故障态下 `timer_snapshot` 会返回
  `RECOVERY_REQUIRED`，那是 P3/P6 的正常路径）：握手给的 epoch 与水位已经生效，展示值等
  下一拍 tick 或 30 秒校验补齐。
- **事件只作缓存失效**：`domain.changed` 只推 `invalidated` 计数（载荷**不**并进镜像——
  权威状态只来自主动拉的一致快照），`timer.tick` 只更新展示值且**不推业务水位**（tick 不是
  业务写；推了水位会让紧随其后的通知被规则②吞掉）。
- **四条规则的 TS 镜像**：`src/ipc.ts` 的 `createFreshnessGate` 扩成 `RevisionGate` 的**逐条**
  镜像（`applySnapshot` / `onNotification` / `onQueryResponse` + `seenRevision`）。两侧**共读**
  `src/types/__vectors__/revision-gate.json`：Rust 的 `src-tauri/tests/revision_gate_vectors.rs`
  与前端的 `src/state/__tests__/revision-protocol.test.ts` replay 同一份步骤、断言同一串判决
  ——「改一条规则必须同时改两侧」是机械的（评审 M2 的另一半）。唯一保留的字面差异：
  `epoch == None`（还没应用过快照）时 Rust 判 `Rehandshake`、前端 `isUnknownEpoch` 不判未知
  （规则 1 的启动顺序让它不可达），向量因此不含这一格，改由两处用例单独钉住。
- **计时展示值的判定顺序**（00 §5）：先 `data_epoch` → `run_id` → 会话/`session_version`，
  **再**比 `tick_seq`；旧状态生成的 tick 即使序号较新也丢（不覆盖暂停/切换后的展示）；
  较新 `session_version` 的未知 tick **先取计时快照**，不自行推导状态跃迁；展示值还没有基线
  时，另一个 epoch 的 tick 也只触发重新握手（规则① 对 tick 一视同仁）。
- **收敛**（规则 4）：可见窗口至多每 30 秒（`VERIFY_INTERVAL_MS = 30_000`）校验 `get_revision`；
  隐藏窗口不轮询、**显示前**校验（`visibilitychange`）。校验发现 epoch 变了 ⇒ 全量失效 +
  按新 epoch 重取；发现版本比已见版本靠前（末次通知丢了）⇒ 合并刷新 + 取新快照。
  这个 30 秒与 `Coordinator` 的 `HEARTBEAT_INTERVAL_MS`（检查点频率）无关。
- **测试**：`src/state/__tests__/{domainState.test.ts,hooks.test.tsx,revision-protocol.test.ts}`；
  前端 18 → **60 条**（fix round 1 之后），Rust 437 → **438 条**（新增的向量 replay）。
  反向验证 25 处（Task 2 的 16 处 + fix round 1 的 9 处）逐条落在对应断言上（见报告），
  另加 `src/__tests__/ipc.test.ts` 里那条把「未知 epoch」与「同 epoch 旧 revision」
  区分开的断言（上一轮定向复评的 Minor）。
- **遗留（2026-10-04 Task 2 登记）**：`FreshnessGate.onQueryResponse` 本阶段**没有生产调用者**
  ——它是 `RevisionGate::on_query_response` 的逐条镜像，为的是四条规则在 TS 侧一处分叉都不少；
  页面查询（Task 3/5）接上 `sendVersioned` / `onQueryResponse` 之前，它只被向量用例驱动。

## Task 2 fix round 1（2026-10-04 深夜）

独立评审对 Task 2 做了 26 处变异（20 处被杀、6 处存活）。本轮修完 3 条 Important + 5 条 Minor，
细节与逐条反向验证见 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task2-report.md` 的
「Fix round 1」一节：

- **I2（真实行为缺口）**：`resync()`/`verify()` 原先以 `stream === null` 早退，而 `stream` 在
  `startEventSession` 返回之后才赋值——**整条启动缝里的「立即取快照 / 立即重新握手」都被静默丢掉**
  （闸门记下了缺口，动作没发生，规则④退化成"等 30 秒轮询"）。改成 `live` 标志（`start()` 在
  await 之前置位、`stop()` 与启动失败清位），并补 3 条用**真实 `startEventSession`** 的用例。
- **I1**：`orderTimer` 的前三级判据（`data_epoch` / `run_id` / `session_id`）原先零覆盖——三行
  各自改成 `if (false)` 都全绿。补 3 条只改一个字段的用例（跨库 / 跨 run / 跨会话），
  每条都被对应变异杀掉。
- **I3**：「load 期间暂存」没有区分性断言——只在 `load()` 里面投递事件时，"暂存"与"直通"
  结果相同。改成**订阅那一刻**投递（`startEventSession` 的缝隙从注册就开始了）。
- **M1/M3**：`stop()` 的 `detachLifecycle()` 删掉也无症状（泄漏 1 个 interval + 1 个监听）
  ⇒ 补 `vi.getTimerCount()` 断言；以隐藏态启动仍会挂轮询的那个变异 ⇒ 补「启动即隐藏」用例。
- **M4**：同一份源码里 `§5 规则 4`（30 秒）与 `闸门规则④`（跳号）混用 ⇒ `domainState.ts` 与
  `src/ipc.ts` 各加一节「编号约定」（§5 规则 1–5 / 闸门规则①②③④ / orderTimer 的判据 1–5），
  并把 `isStaleNotification` 那行标注成"只在文档里对照"。
- **M5**：`markApplied` 与 `applySnapshot` 语义分叉（`<=` vs `<`、清不清缺口标记）⇒ 前者改成
  后者的**同一次**状态迁移（只有一份实现），并补一条区分性用例；它本阶段仍没有生产调用者，
  与 `onQueryResponse` 一起登记在下面。
- **M8**：报告里「只删 `load()` 的握手水印没红」的说法是**当时那批用例**下的结论；在最终树上
  这一处变异会红（`计时快照拿不到` / `展示值还没有基线` 两条用例的 `revision: 5` 对不上），
  报告已按实测订正。

## Task 2 fix round 2（2026-10-04 深夜）

定向复评对 fix round 1 的 8 条 finding + 3 条登记全部 ADDRESSED，但指出修复引入的一条新
Important：`start()` 复用 `starting` 时**没有代次概念**，而 `stop()` 不清它——「启动在飞 →
`stop()` → 立刻再 `start()`」会拿到一个注定关掉会话的旧 promise，调用方看到 `start()` 正常
resolve，镜像却永久停在 idle（StrictMode 的 mount→cleanup→mount 与 Task 4 的窗口生命周期
正好走这条路径）。

- **改法**：`starting` 从裸 promise 变成 `{ generation, promise }`——代次**跟 promise 存在
  一起**，不新增标量（"这个 promise 还能不能用"与"属于哪一代"不会再各说各话）；
  `start()` 只在代次相同时复用，`stop()` 清掉它；启动体抽成 `beginSession(token)`，
  作废的那次只关自己的订阅、不写回，`finally` 只清自己那一格。
- **用例**：`start → stop → start`（第二次的 `start()` **不 await 第一次**）断言订阅、轮询、
  失效计数都真的起来了，不是只 resolve；事件替身同时改成每次会话各一份（两次启动同时在飞时
  不能共用一个槽位）。
- **反向验证**：单独摘掉"代次检查"或单独摘掉"`stop()` 清 `starting`"都仍然绿（两道护栏各自
  独立生效）；两处都还原才复现 `expected 'idle' to be 'ready'`，另有 R11 钉住"作废的那次
  必须关掉自己的订阅、不能复活"。
- 细节与原始输出见 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task2-report.md`
  的「Fix round 2」一节（前端 61 条）。

## Task 3 的落地（2026-10-04）

细节、逐条反向验证与原始输出见
`.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task3-report.md`：

- **收件箱页**（`src/pages/Inbox.tsx`，F-001/F-002）：回车一条 `create_task` 并**当场重拉**
  （"立即可见"不赌事件到得比响应早）；空白标题在界面层拦住（判据与
  `services::catalog::create_task` 的 `title.trim().is_empty()` 相同，连命令都不发，省一次
  往返；Rust 对同一输入同样拒绝，那条 `message` 才是用户看到的文案）；置为 Ready；
  **开始只发一条 `start_timer`** —— `Inbox → Ready → Doing` 两步跃迁在 Rust 的**同一个事务**
  里，前端不拆成两条命令（用例用 `mockIPC` 计数钉住）。列表列 Inbox/Clarifying/Ready/Doing
  四个状态：`finish` 不改任务状态、而 P7 又没有"完成/重开"入口，不带 `Doing` 的话任务只要
  开始过一次就再也点不到。
- **入口只出现在 02 §5 允许的状态上**（`canClarify`/`canStart`），但**前端过滤只是体验**：
  服务拒绝时按 R8 把 `message` 原样上屏（用例两条：漏过滤时按钮出现即红；服务拒绝时文案正确）。
  完成/取消/Blocked/Waiting/reopen 的入口按计划属 P8（`transition_task` 尚不存在）。
- **计时页**（`src/pages/Timer.tsx`）：暂停/继续/结束各一条命令，请求由
  `src/components/timerRequests.ts` **从快照**构造；**不做本地状态机** —— 按钮集合只由快照的
  `state` 决定，点击与命令响应都不改本地状态（用例钉住"响应到了按钮也还没变，等那一拍
  `timer.tick`"）；`active_ms`/`pending_ms`/`remaining_ms`/`overtime_ms` 原样展示（前端不读
  时钟、不做减法，暂停值冻结是 P2 的事）；倒计时到点只弹一句提示，**不发 `finish_timer`**、
  也不改任务状态。
- **错误口径（R8）**：`src/components/commandError.ts` 只按 `code`/`requires_handshake` 决定
  **行为**（重新握手 / 冲突刷新 / 只提示），文案只有 `ErrorResponse.message` 一个来源——
  没有「码 → 文案」表，未知 `code` 也原样展示（两条用例各钉一半）。
- **接线**：`src/App.tsx` 导航两项（仍不引路由）+ 状态栏（单独组件，外壳不跟着每拍 tick
  重渲染）；挂载 `domainState.start()`、卸载 `domainState.stop()` —— 页面自己不开会话、
  也不各自订阅事件，只从 `src/state/hooks.ts` 读镜像（新增 `useDataEpoch` /
  `useHandshakePhase`）。Task 1b 那条「一个命令都不调用」的冒烟断言因此改写为
  「命令集合恰好是那四条」。
- **镜像的三处增补**（都是页面用得上、且**不新增协议分支**的只读入口）：`rehandshake()`
  （R8 的 `requires_handshake`：再校验一次，必要时重取）、`refresh()`（R8 的冲突刷新，与
  闸门规则④共用同一份 `resync`）、`isStaleResponse()`（页面查询响应按本上下文**唯一那把**
  闸门判旧，页面不各自维护第二份水位）。
- **待接线（不是"遗留掉就算了"）**：「继续」要 `resume_timer` 的 `task_id` +
  `task_expected_version`，而**当前**的 `TimerSnapshot` 没有任务字段、24 条命令里也没有
  「会话 → 任务」的读路径 ⇒ 冷启动（重开窗口 / 托盘暂停之后）**构造不出**这条请求。
  本轮过渡落法：请求构造收在 `buildResumeRequest(snapshot, taskIdentity)` **一个函数**里，
  任务身份由发起 `start_timer` 的收件箱页经外壳交给计时页；**身份不可得时「继续」不出现**。
  契约侧给 `TimerSnapshot` 补上 `task_id`/`task_row_version` 之后，接线就是把这个函数的第二个
  入参换成快照字段（三行），外壳那份过渡状态与 `Inbox.onSessionStarted` 一并删掉。
  **（2026-10-04 同日收口）契约字段已落地**——`task_id` / `task_row_version` 已在快照里，
  连同三份重生成的 IPC 快照与 TS 侧类型；理由、边界与反向验证见上面
  「Task 3 的契约补口：`TimerSnapshot` 带任务身份」一节。
- **顺带收（Task 2 复验点名）**：`beginSession` 的事件入口**按代次过滤**（`stop()` 之后
  `unlisten` 回来之前旧订阅 flush 出来的通知不再被新代次接纳）；补「旧代次握手失败 ⇒
  新代次不受影响」用例；`start()` 里那处永不触发的代次检查在注释里写明是**不变式断言**。
- **测试**：前端 61 → **84 条**（收件箱 11、计时 7、镜像 +4、外壳 1 → 2）；四个提交**各自**
  `pnpm test` / `pnpm build` EXIT 0（逐提交把工作树临时还原成该提交的 `src/` 后实测）。
  本轮**一个 Rust 文件都没改**，分层门禁与 cargo 侧由 Rust 实施者负责。

## 遗留与边界（2026-10-04 Task 2 fix round 1 登记）

- **协议向量能把"两侧不一致"逼出来，但替代不了"同一条规则两处实现"的风险，而且完全
  不覆盖计时判据链**（评审的关键判断）：
  - Rust 的 `Coordinator::is_stale_tick`（`src-tauri/src/services/timer/coordinator.rs:363`）
    只判 `session_id` + `row_version`；前端的 `orderTimer`（`src/state/domainState.ts`）是
    **五级**判据（`data_epoch` → `run_id` → `session_id` → `session_version` → `tick_seq`）。
    两者**不是同一个函数**，没有共享向量、也没有 Rust 侧的对应断言——最密集的那半时序规则
    仍只有一份前端实现 + 一份前端用例（这正是 6 处存活变异的根因）。
    `domainState.ts` 头部那张对照表已把"对应"收紧成"部分对应"，但这只是措辞：
    **要不要给计时判据链也造一份两侧共读的向量，是一个待定项**（要么把 `is_stale_tick`
    扩成同一条链，要么承认它是展示侧独有、在 P8 的实机验收里覆盖）。
  - 向量刻意不含的「`applied == None` 时的通知」那一格，**Rust 侧 `tests/event_protocol.rs`
    也没覆盖**：该文件里三处 `on_notification` 全都跟在 `apply_snapshot` 之后
    （逐条核对过），所以 `epoch == None ⇒ Rehandshake` 这条分支目前两侧都只有
    "前端的不判未知"这一半有断言。
- **`FreshnessGate.markApplied` 与 `onQueryResponse` 本阶段都没有生产调用者**（同前一条登记）：
  它们是 `RevisionGate` 的逐条镜像，为的是四条规则在 TS 侧一处分叉都不少；页面（Task 3/5）
  应用自己的带 epoch 查询响应时会用上它们，在那之前只被用例驱动。

## Task 4 的落地（2026-10-04）

**状态**：代码与测试已落地（`b0290c3` 平台半边 + `dabb79e` 接线），门禁 `449 passed / 0 failed`、`clippy --all-targets -D warnings` 0、`check-layers.ps1` 六条规则 PASSED、`Cargo.lock` 未变（**零新增 crate**，只是启用了 `tauri` 的 `tray-icon` feature）。**实机验收结论为空**（见上面「仍待与归属」第 1 条）。

**分层选择（本轮最容易踩的坑，先说结论）**：`platform` 不得引用 `services`/`storage`/`commands`（`check-layers.ps1` 第六条），而 F-011 又要求托盘动作复用同一批命令。于是**把「点到了什么」与「点了之后干什么」切开**：

| 位置 | 职责 | 为什么不放到对面 |
| --- | --- | --- |
| `platform/tray.rs` | `TrayAction`（四项）、`MENU_ITEMS`（四项 + 预留禁用项）、`action_for`、`build`（图标 + 菜单 + 事件回调） | 一个业务字都不出现；菜单事件只把 `TrayAction` 交给回调 |
| `platform/window.rs` | `MAIN_WINDOW_LABEL`、`plan_activation`、`apply_activation`、`raise_or_rebuild_main`、`should_prevent_exit`、`spawn_activation_watcher` | 都是窗口/请求文件的事；重建走 `tauri.conf.json` 那一份配置 |
| `commands/mod.rs` | `tray_pause_impl`（→ `pause_timer_impl`）、`tray_quit_impl`（→ `RunningApp::shutdown`）、`spawn_tray_pause`/`spawn_tray_quit`（阻塞线程 + 同一把锁） | 「动作 → 命令体」是命令层的事，放这里才叫**复用**；放 `platform` 就违门禁，放 `lib.rs` 就是业务逻辑散在组合根 |
| `lib.rs` | `on_tray_action`：窗口动作 → `platform::window`，服务动作 → `commands::spawn_tray_*` | 组合根只接线，不判断业务 |

**行为变化（真机可见）**：关掉最后一个窗口**不再结束进程**——`Builder::run(context)` 改成 `build(context)?.run(cb)`，回调里 `RunEvent::ExitRequested { code: None }` ⇒ `api.prevent_exit()`；`code: Some(_)`（托盘「退出」、第二次启动的自己退出）照旧放行。

**签名放宽（两处，理由写进了代码注释）**：`RunningApp::shutdown` 与 `Scheduler::stop` 由 `&mut self` 改为 `&self`。托盘「退出」在组合根里只拿得到 `&RunningApp`（Tauri 托管状态给的就是共享引用），要它走**唯一那条**显式退出入口就不能另开 `&mut` 通道；内部可变性收在 `Scheduler`（`AtomicBool` + `Mutex<Option<JoinHandle>>`），语义不变、`stop` 仍幂等。连带 `tests/exit.rs` 去掉 4 个不再需要的 `mut`。

**异常路径**：托盘「退出」的事务失败（例如库里有一条结束不了的会话）⇒ 记诊断并**以非零码退出**。此时事务已回滚、库是一致的，这一次 run 以「没有 `clean_exit_at`」结束正是恢复扫描的输入（F-015）；把用户困在一个没有窗口的托盘里更糟。**不做**：退出失败的用户可见提示（P6/P8）、托盘在维护态下的禁用（P6）、菜单里的「完成」（P8，依赖 P3）、把当前任务标题做成动态菜单项与视图跳转（需要前端的视图/路由，P8）。

## Task 4 fix round 1（2026-10-04）

**状态**：`4050c8f` 落地，门禁 `452 passed / 0 failed`（+3 条用例）、`clippy -D warnings` 0、`check-layers.ps1` PASSED、`Cargo.lock` 未变。独立评审给的必改两条 + 顺带四条全部落地。

- **I1 自死锁防线（必改）**：`shutdown`/`stop` 放宽到 `&self` 之后，「先取锁、再调退出」也能编译——而退出要先 `join` 采样线程、采样线程每一拍都要取那把锁 ⇒ **静默卡死**（评审用 rustc + `timeout` 复现过）。现在串行边界是 `AppBoundary`（`Mutex<AppState>` + **持锁线程 id**），`lock_app` 返回 `AppGuard`（`Deref`/`DerefMut` + `Drop` 清位）；`shutdown` 用 `holds_app_lock` 检测「调用线程自己就是持锁者」，命中即 `AppError::Storage`（`detail` 说清正确姿势），**拒绝时不碰任何东西**。**没有**改成外面套 `Mutex`，也**没有**恢复 `&mut`（评审已验证：先 `Arc::clone` 再持锁，旧屏障照样编译通过——它只是减速带）。
- **I2 托盘路由表（必改）**：`lib.rs` 抽出 `pub fn tray_dispatch(action) -> TrayDispatch`（`Window`/`Pause`/`Quit`），`on_tray_action` 只 match 它。原先把 `Pause` 接到 `Quit` 上不会有任何用例变红，而「托盘动作与界面动作走同一批命令」这条要求的接缝正是这几行。
- **M1 并发 `stop` 的语义**：`JoinHandle` 只能被取走一次，第二个调用者拿不到句柄就直接返回会让「返回 ⇒ 线程已退出」不成立。补完成位（线程退出前置位、panic 展开由 RAII 兜底），第二个调用者等它。
- **M2/M3 验收判据**：`manual-shell.md` 补「诊断走 `println!`/`eprintln!`、release 的 Windows 子系统没有控制台 ⇒ 用 `pnpm tauri dev` 或重定向」；「窗口立即出现」改成可操作判据（先暂停再重开，第一眼就该是 `paused`）并写明 Rust 半边与前端半边的分工。
- **M4 退出语义**：`RunEvent::Exit` **不**兜底再调一次显式退出（`App::run` 最终 `process::exit`，`Drop` 不会跑），注释里写明「写 `clean_exit_at` 只发生在显式退出路径上」以及 P8 新增退出入口时必须显式调 `shutdown()`。
- **C1 判据纠错（评审列在必改之前，会让 F-009 假通过）**：`manual-shell.md` 原先要求观察「`revision` 只按心跳前进」——**心跳不加 revision**（`services/timer/coordinator.rs` 的 `heartbeat` 原文），操作者会看到 revision 不动而误记「通过」；而且**界面秒数是从 `started_at` 算出来的**，采样线程死了也照样「继续走」。现改为两条真判据：① `revision` **不变**（正确现象）；② `interval_checkpoint` 的 `wall_at`/`elapsed_ms` 在关窗 60 秒后**前进 ≥ 20 秒**（该表以 `interval_id` 为主键做 upsert，**行数不会涨，必须读列值**）。另补本次 `run id` 的取值 SQL。

**反向验证（原始输出见 `p7-task4-report.md` 的 fix round 1 一节）**：拆掉 I1 防线 ⇒ 对应用例**卡死**（`running 1 test` + 「has been running for over 60 seconds」，90 秒未返回被 kill）；对调 `Pause`/`Quit` ⇒ 对应用例 FAILED（`left: Quit / right: Pause`）。

## Task 5 的落地（2026-10-04）

细节、逐条反向验证与原始输出见
`.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task5-report.md`：

- **任务页**（`src/pages/Tasks.tsx`，F-002 的轻量 GTD + F-005 情境）：下一步行动（`Ready`）/
  等待中（`Waiting`）/ 阻塞（`Blocked`）三个列表**各查各的状态**——一次只发
  `statuses: [选中的那一个]`，把 Waiting 与 Blocked 合成一条查询等于在界面上把它们当成
  同一个状态（04 §「V0.1 轻量 GTD 补充验收」把它们列成两个列表；sabotage 验证：
  合并 ⇒ 对应用例红）。项目筛选走**三值** `ProjectSelector`（不限 / 无项目 / 指定 id，
  D3 裁决的形状，没有压成 `Option<Option<_>>`），情境筛选走 `list_tags(kind="Context")`；
  **三个条件进同一条 `list_tasks`**，交集在服务端算（`task_repo::filter_clause`）。
- **计数与分页都用服务数据**：`TaskQueryResult.total` 与 `tasks` 出自**同一个读事务**，
  分页器与「共 N 条」都读 `total`，前端不数 `tasks.length`；**改条件即把页号归 1**
  （否则新条件会带着旧 `offset` 去查，服务端只会照做）。无项目 / 无情境（情境下拉禁用，
  不编选项）/ 空列表 / 加载 / 查询失败各有明确状态；查询失败**只提示不刷新**
  （`refresh()` 会自失效 ⇒ 自触发重拉）。
- **旧响应的两条判据**（比 `data_epoch`/`revision`，**不比到达顺序**）：
  ① **问题身份**——响应发起时的条件+分页窗口要与"现在这个问题"一致（条件一变，旧响应
  与旧结果都不得上屏）；② **版本**——`domainState.isStaleResponse`。两条都过了才
  `setView`，**上屏之后**才 `markApplied`。用例各钉一半：同一筛选下旧响应晚到（rev 5 迟于
  rev 6）只能靠②挡；切换筛选后旧筛选响应迟到（两份同 rev）只能靠①挡。
- **镜像补一个只读原语**（`src/state/domainState.ts`）：`markApplied(stamp)` —— 页面的
  查询响应也是权威快照，真的用上之后把本上下文**那唯一一把**水位推到
  `data_epoch`/`revision`（同版/更旧是 `stale_ignored`，水位只前进）。它是计划里
  「页面（Task 3/5）应用自己那份带 epoch 查询响应时会用 `markApplied`」那句的落地；
  于是页面不必各自维护第二份水位。**辅助查询（标签 / 可选项目）不进水位**：让它推水位会把
  一条正在飞的主列表响应按"更旧"丢掉，界面就停在加载态（代码注释里写明）。
- **项目页**（`src/pages/Projects.tsx`，F-004）：列表用 `list_projects(status=null)`——
  归档与 `done` 的历史照样读得到；创建 / 改名 / 归档都提交 epoch 与**列表里那一行的项目
  版本**，归档先确认（Popconfirm），命令成功之后**当场重拉列表**（"确认归档后更新列表"
  不赌 `domain.changed` 到得比响应早；sabotage 验证：去掉重拉 ⇒ 对应用例红）。入口与 Rust
  写入口径逐条对齐：归档只给 `active`（`archived` 再归档是幂等空转、`done` 被
  `ensure_writable_in_v01` 拒绝），改名不给 `done`——不给必败入口。
- **项目详情**：`statuses: []`（空集合 = 不限制状态）列出该项目的任务、`total` 照实显示；
  「新增第一条行动」是**一条** `create_task`（项目在请求里定死，不先建 Inbox 任务再改归属）。
  **只列第一页**（服务端上限 100）：04 把"稳定分页"挂在 F-002 的下一步/等待/阻塞列表上
  （任务页有分页器），F-004 这一句只要求"项目任务列表"；条数超过一页时文案会说出来，
  不做静默截断。若复核要求详情也分页，改动点是详情自己的 page 状态 + 一个 `Pagination`，
  两条判据已经就位。
- **R8**：`VERSION_CONFLICT` 只决定**行为**（冲突刷新 + 展示 Rust 的 `message`），没有第二份
  「码 → 文案」表；用例断言冲突后重拉一次、且**不重发**那条命令。
- **接线**：`src/App.tsx` 导航四项（收件箱 → 项目 → 任务 → 计时，按 GTD 动线），页面挂载区
  改成一个 `switch`；仍不引路由、页面仍无 props、外壳仍不持有跨页面业务状态。默认页仍是
  收件箱，所以 Task 3 那条「命令集合恰好四条」的断言不受影响。
- **测试**：前端 92 → **112 条**（任务页 10、项目页 7、镜像 +1、外壳 +2）。新增
  `src/pages/__tests__/jsdomBridges.ts`：jsdom 没有 `matchMedia` / `ResizeObserver`，
  而 antd 的分页器与浮层要用（判据写"不是函数"而不是"属性不存在"——jsdom 把 `matchMedia`
  声明在 window 上但值不可调用）。**六处定向 sabotage 逐项变红且只红点名用例**
  （去掉版本判据 / 去掉问题身份判据 / 改条件不重置分页 / 归档后不重拉 / 合并 Waiting+Blocked
  / `markApplied` 空实现），每次逐字节还原。四个提交**各自**导出复跑：93 / 103 / 110 / 112
  条全绿 + `pnpm build` EXIT 0。零新增依赖，`src-tauri/` 一个文件都没动。
- **仍未做（照计划）**：Inbox 页的标签入口（Task 3 评审 M6）仍无入口——本轮没有新增
  `tag_task`/`untag_task` 的界面（Task 5 只做查询侧的情境筛选）；统计视图与导出（P5/P8）；
  归档/完成状态的批量操作（V0.1 无此入口）；完成/取消/Blocked/Waiting/reopen 的状态入口
  （P8，依赖 P3 的 `transition_task`）。

## 仍待与归属（2026-10-04 登记）

- **F-009 / F-011 / F-016 的实机验收**（2026-10-04 Task 4 登记）：**步骤已就位**——`src-tauri/tests/manual-shell.md`（Task 4 建立，Task 6b 一起用），分三节：
  1. **F-011 托盘**：右键托盘图标，确认可点项恰好四项（当前任务 / 暂停 / 快速捕获 / 退出）+ 一个禁用项「完成（P8 启用）」；「暂停」在计时中把会话置 `paused` 且 `revision` +1，没有计时时**零写入**；
  2. **F-009 关掉全部窗口**：关窗后进程与托盘仍在、`clean_exit_at` 仍为 NULL、开放区间仍在；**等 60 秒**后从托盘重开窗口，计时**继续走了这 60 秒**且界面**立即**有数据（不是等下一次 tick）；再从托盘「退出」：进程结束且 `clean_exit_at` 已写、会话 `finished`、开放区间闭合；
  3. **F-016 单实例唤起**：主窗关掉后再启动第二个实例 ⇒ 第二个进程自己退出、`application_run` 不增加、既有实例把主窗**重建**出来；主窗开着时 ⇒ 被**抬起**（不重建）。

  **为什么不能用单元测试代替**：集成测试进程里没有事件循环，也就没有窗口与托盘（`tauri::test` 的 mock 运行时本轮没有启用）。`tests/shell_lifecycle.rs` 钉住的是**决策函数与命令路径**（菜单映射、`should_prevent_exit`、`plan_activation`、托盘暂停/退出的库内证据、label 与配置一致性），钉不住「真实托盘图标/菜单交互」与「关窗后仍在计时」。**这一步的结论目前为空**，P8 复核。
- **平台事件实机验收**（锁屏 / 休眠 / 唤醒 / 改时 / 关窗后采样 / 事件到达延迟）目前**无归属**：登记为「**P7 实机步骤 + P8 复核**」。`docs/validation/p2-clock-mapping.md` §6/§7（`:156`–`:172`）已声明这些**未验证、不得当成已验证**：探针是前台进程，证明不了关窗后仍采样，也证明不了系统事件的可靠性与到达延迟。
- **「多入口开发/打包路径」与 Windows 打包验证**（`00-architecture.zh.md` §7 的待验证项）同样**无归属**：登记到 P8，与 R-04 的发布产物门禁一起做。
- **`@mui/material` 与 `@emotion/*` 是模板遗留死依赖**（`src/App.tsx` 未使用，`package.json` 里仍在）：**只登记，不在 P7 删**——删依赖属清理任务且需用户确认。
- **维护态错误码**：P6 Task 4 引入后，再登记为 Task 1 的透传项（P7 只登记、不实现）。

## 遗留与边界（2026-10-04 Task 1b 登记）

- **包装层 `targets` 未覆盖**：`run_command` 的 `targets`（错误响应的 `authority.records`
  就由它决定）只活在 `#[tauri::command]` 包装里，命令体拿不到它；要观测它就得有 Tauri
  运行时（`tauri/test` 的 `mock_builder`），本轮没有启用。`tests/error_contract.rs` 覆盖的是
  `capture_error_response` 这个**机制**（喂显式 targets），`tests/ipc_snapshots.rs` 钉的是一份
  **样例** `ErrorResponse`——两者都**不是**「逐条命令的 targets」的断言。
  `tests/ipc_commands.rs` 的文件头原先声称「逐条断言在那两份里」，**不实，已订正**。
  **登记为遗留**：补它的代价是要么启用 `tauri/test`，要么为 15 处包装加一份只服务测试的
  `pub` 助手——两者都超出 Task 1b 的范围，不值得在本轮顺手做。
- **计时族命令「提交后重建失败」那一笔不发 `domain.changed`**：`start`/`pause`/`resume`/`finish`
  在 `tx.commit()` **之后**才 `rebuild_from_committed`（失败映射为 `RECOVERY_REQUIRED`，
  见 `services/timer/coordinator.rs` 那段约定）。命令因此返回 `Err`，命令层的 `announce`
  不会被执行——**这一次已提交的写没有对应通知**。这是 00 §5「一次业务写对应一条
  `domain.changed`」的一个已知例外，**本轮不改代码**（要改就得让"提交成功"与"响应成功"
  解耦，那是 P6 的故障路径范围）。**收敛路径（2026-10-04 fix round 1 订正）**：靠可见窗口
  那个 30 秒的 `get_revision`——它直读 `app_meta`、**不经协调器**
  （`services::handshake::get_revision`），所以故障态照样能拿到新 `revision`。原先这里写
  「后续 `timer.tick` 带新的 `revision`」**不成立**：`Coordinator::tick`/`snapshot` 第一句就是
  `refuse_if_faulted()`，故障态下它们只返回 `RECOVERY_REQUIRED`。协调器解锁要等**下一次成功
  重建**（`rebuild_from_committed` 成功才清 `faulted`；P3 的对账入口 `retry_recovery` 也走
  同一条恢复路径）——那属 P3/P6 范围。实质结论不变：无通知，且 `requires_handshake` 为
  **false**（`capture_error_response` 只在 `DataEpochMismatch` 或权威捕获失败时置真），
  客户端不会因此重新握手。
- **请求 DTO 的 TS→Rust 方向仍然只能人工对齐**（2026-10-04 fix round 1，评审 I1）：
  Rust→TS 那一半现在有机械联系了——`tests/ipc_requests.rs` 给 **18 个请求 DTO** 各钉了一份
  「字段一个不少」的 JSON 样本（Rust 改字段名/删必填字段/加必填字段 ⇒ 立刻红）。
  但 `src/types/ipc.ts` 的**请求**接口没有任何东西去核对它：TS 侧把 `expected_row_version`
  写成 `expected_revision`、或者少写一个字段，`tsc` 与 `vitest` 都不会红，
  要等运行期那条命令退化成 `TRANSPORT_ERROR` 才暴露。补齐它要么上 DTO 生成器（D1 已否决，
  离线取不到），要么给请求也造一套「TS 声明 ↔ Rust 样本」的镜像检查——**登记为遗留**。
- **TS ↔ 快照检查盖不住的两件事**（2026-10-04 fix round 1，评审 M6/M7）：
  ① **枚举取值域两个方向都盖不住**：快照里只有**样例值**（例如 `task.status` 只有 `"Ready"`），
  所以（a）Rust 新增变体在它进入某份快照之前看不出来，（b）**TS 侧写错一个从未出现在快照里的
  取值同样无人发现**（只有快照用过的那些值会被核对）。整个取值域靠 `src/types/ipc.ts` 的
  `as const` 数组与 Rust 的 `ALL` 常量人工对齐——这是「不上 DTO 生成器」的已知代价（D1 裁决），
  不是遗漏；
  ② **键集合断言只比 `keyof`，不含值类型与可选性**：接口字段的**名字**被两侧钉死了，
  但把 `pending_ms: number | null` 写成 `pending_ms: string | null`（或把可选写成必填）
  不会红，要到消费点才报类型错。它与 ① 合起来说明这份检查的边界是「键与已出现的取值」，
  不是「完整类型等价」。

## 开工前已核实（2026-10-04）

- **复核基线**：仓库 HEAD `ae9ec00`（工作树干净），`src-tauri/` 镜像与之一致（仅 `main.rs` 未镜像、`tests/` 逐字节相同）；门禁 **357 passed**（据 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/progress.md` 第六轮记录，本次未重跑）。**本计划尚未实施**——本节只写复核过的事实，不含任何「已验证通过」的实施结论。
- **符号面**：`handshake::get_revision`/`RevisionSnapshot`、`error_response::capture_error_response`、`WriteEnvelope`（crate 根）、`Coordinator::{start,pause,resume,finish,snapshot,heartbeat}`、四个读服务的入参与返回、`src-tauri/scripts/check-layers.ps1` 的四条规则**全部对上**；`AppError::code()` 只有五个码；P6 才引入的维护态错误码在本仓 0 命中（属 P6 Task 4）。
- **改名与不一致（已按裁决写进本计划）**：分层脚本真名是 `src-tauri/scripts/check-layers.ps1`；`tauri.conf.json` 的 `identifier` 是 `com.worktrace.app`，与 `platform::paths::APP_ID = "com.worktrace.desktop"` 不一致；托盘需要 `tauri` 的 `tray-icon` feature。
- **离线可行性（实测缓存，不是推断）**：DTO 生成器 `ts-rs`/`specta`/`typeshare` 在 Windows 与 WSL 两侧 cargo 缓存 **0 命中** ⇒ 不上生成器；`tray-icon`/`muda`/`tao` 在 Windows cargo 缓存与 `Cargo.lock` 里都在 ⇒ 托盘离线可行；`image` **不在** `Cargo.lock` ⇒ 不启用 `image-png`/`image-ico`；`D:\.pnpm-store\v11` 的 `index.db` 有 `vitest@5.0.1`、`jsdom@30.1.1`、`@testing-library/react@16.3.3`、`@vitejs/plugin-react@6.1.1` 的完整条目，`react-router`/`zustand`/`jotai`/`redux`/`@tanstack/*` 则是 **0 条目**。
- **本计划要补的代码缺口**：`storage/` 无 `run_repo.rs`（`application_run` 只有 schema，`src/storage/schema_v1.rs:28`）；`platform/` 只有 `clock.rs`/`paths.rs`；`services/` 无 `bootstrap.rs`/`events.rs`；`commands/` 只有模块头注释；服务层**除 `handshake::RevisionSnapshot` 外没有任何 serde derive**；`TaskQuery` 的字段是 `storage::task_repo` 类型（命令层不能构造）；`session_repo::running_foreground` 不带 `run_id`。
- **本次修订**：按复核结论落地 **D1–D6** 与 **R1–R14**，并把原「提前交付的平台边界」升格为 **Task 0**；因为 Task 0 挪到最前，总纲与 `p1-p4-review-backlog.md` 里指向本计划的行号引用一并更新。
