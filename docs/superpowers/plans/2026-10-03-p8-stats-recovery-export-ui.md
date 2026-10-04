# P8 · 统计、恢复与导出的界面 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 P5/P6/P7 的服务接到用户手上，补齐 V0.1 最后三块界面（确认工时与待确认分列、恢复确认与修正补录、导出与备份恢复），加上 R-04 的发布产物门禁，并完成 V0.1 的端到端人工验收。

**Architecture:** **不重做外壳**。P7 已经搭好 Tauri 命令接线、`domainState` 订阅入口、状态镜像与托盘；本计划在其上加页面与命令透传，业务判断仍全部留在 Rust。新增的界面只消费 P5（统计与导出）、P6（备份恢复与维护态）、P3（恢复确认与历史修正）的服务入口。

**Tech Stack:** Tauri 2 · React 19 · TypeScript 6 · Vite 8 · Ant Design · `useSyncExternalStore`（沿用 P7，不引状态库）

**Spec:**
- `.../04-functional-spec.zh.md` F-010（完整 Today）、F-013 之外的 V0.1 项、F-015、F-017、F-018、F-019、F-020
- `.../02-data-model.zh.md` §3（命令表：`correct` 仅 finished、`reconcile` 仅 recovering、丢弃与作废分开）、§6（统计口径与"按当前分类"）
- `.../03-adr.zh.md` ADR-012 的 R-04（`DockviewDemo` 不进发布产物）
- `.../00-architecture.zh.md` §5（快照、去重、维护态）

**依赖的前置计划：** **P3、P5、P6、P7**（恢复确认与修正、统计与导出、平台硬化、外壳与核心交互）。**（2026-10-04 修订：因 M13）今天的实施状态**：P7 已交付并验收（实机项结论为空，归本计划 Task 5）；**P3/P5/P6 都还没实施**（`worktrace-src/src/services/` 下没有 `stats.rs`/`export.rs`，`reconcile`/`backfill`/`discard_session` 无生产实现，`DATA_RESTORE_IN_PROGRESS` 0 命中）。⇒ Task 1（要 P5 的 Today 聚合）、Task 2（要 P3 的 `correct`/`reconcile`/`backfill`）、Task 3（要 P5 的生成函数 + P6 的维护态与恢复入口）各自写了**硬前置**，前置未落地时不得声称对应 F-ID 完成。

**边界（不要越界）：**
- **HUD 与全局捕获热键属 V0.1b**（F-012/F-013），不做。
- **dockview 布局**属 R-04 之后，本计划继续用固定布局。
- 不新增统计/计时/恢复逻辑；不一致就回到对应计划改服务，不在前端补。**（2026-10-04 修订：因 M9；裁决② 已定为"V0.1 不做"）"前端补不了"的一个具体例子**：判"当前任务"是否需要 `mode`（`FOREGROUND AND running`）——`TimerSnapshot`（`src-tauri/src/services/timer/snapshot.rs:19`-`:75`）里没有 `mode` 字段，前端 `src/state/hooks.ts:63`-`:66` 只能判 `state`。**但 V0.1 不需要它**：`start` 明确只接受前台（`services/timer/coordinator.rs:492`-`:496`），三个非前台变体没有生产构造点 ⇒ **本计划不做**；将来引入后台计时时才先扩 `TimerSnapshot.mode` 契约（并同步 `src/types/ipc.ts:202` 与 2 份快照），**届时另开变更**。

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条；前端测试只断言展示与转发。

---

## 开工前修订记录（2026-10-04）

> **开工前必读。** 下面每条都是「照原措辞写会写出编译不过或语义错的代码」或「引用与今天的树不符」。
> 行号与符号都是 2026-10-04 在 `worktrace-web/`（= 仓库前端）与 `worktrace-src/`（= 仓库 `src-tauri/`）里
> `grep` 实测的。只改措辞与引用，**不改 P8 的交付边界**。

| # | 级别 | 原措辞（位置） | 新措辞 / 落地口径 | 依据（实测 file:line） | 为什么必须改 |
| --- | --- | --- | --- | --- | --- |
| **C3** | **Critical** | 「`src/pages/Today.tsx`（**在 P7 的基础上扩展**）」「**P7 已做前两项**，本任务补后三项」（Task 1） | **前提为假**：今天**没有 Today 页**，本任务是**从零建**（第 5 个导航项），复用件清单见 Task 1 | `worktrace-web/src/pages/` 只有 `Inbox/Timer/Projects/Tasks`；`src/App.tsx:40`（`PageKey` 四项）、`:42`（`PAGES` 四项）、`:54`（`PageView` 的 switch）；`src/ipc.ts:249` 有 `planFor` 转发而 `grep -rn planFor src/pages/` = **0 命中**；"当前任务"今天只有 `src/pages/Timer.tsx:113` 与 `src/App.tsx:89` 读 `snapshot.task_title` | 按原文写会去"扩展"一个不存在的文件、并继承一条不存在的既有实现 |
| **C4** | **Critical** | Task 3 首条「**触发 P5 的生成函数并落盘**」把 P5 当既成事实 | P8 **先决定并登记落盘能力**（不预设插件名），再实现"写文件 + 给出可打开的位置"；**明确依赖 P5 的生成函数**，P5 今天**未实施** ⇒ 本任务有硬前置 | `worktrace-src/src/services/` 下**没有** `stats.rs`/`export.rs`；`src-tauri/src/commands/` 只有 `mod.rs` 与 debug-only 的 `dev.rs`（无导出命令）；`src-tauri/Cargo.toml:20`-`:43` 无 fs/dialog 插件；`src-tauri/capabilities/default.json:1` 的 `permissions` 只有 `core:default`/`opener:default`（P5 计划 `:27`/`:96`/`:101` 已把落盘订正给 P8） | 原文两处问题：把未实现的 P5 当既有；且没有任何能力登记却写"落盘" |
| **M8** | Minor | 「`RECOVERY_REQUIRED` 要把用户导到恢复页」（Task 2） | 今天**导不过去**：本任务要先扩两处——`CommandErrorAction` 加一档、外壳给出最小的视图切换入口（**不引路由**） | `src/components/commandError.ts:24`（三档 `notice\|rehandshake\|refresh`）、`:31`-`:35`（`RECOVERY_REQUIRED` 落 `notice`）、`:16`-`:17`（注释自己写着"它的用户界面（恢复确认页）属 P3/P8"）；`src/App.tsx:7`（"**不引路由**（离线取不到 `react-router`）"）、`:54`（`switch` + `useState` 固定布局） | 照原文写"把用户导到恢复页"会没有落点：既没有分支、也没有路由 |
| **M9** | Minor | 「`useRunningTaskId` 补 `mode`」（§6.4-14 的转述） | **V0.1 不做**（**裁决 2026-10-04**）：V0.1 的会话模式**恒为前台**，所以 `useRunningTaskId` **不需要** `mode` 判别；若后续引入后台计时，**先扩 `TimerSnapshot.mode` 契约**（会动 2/15 份 `src/types/__snapshots__` 与 Rust 侧 serde），届时**另开变更** | **只拒前台**：`services/timer/coordinator.rs:492`-`:496`（`start` 里 `if req.mode != SessionMode::Foreground { return Err(AppError::Domain { detail: "当前版本仅支持前台工作会话。" }) }`，`:494` 是那句文案）；`SessionMode::{Background,Passive,Waiting}`（`domain/session.rs:69`-`:71`）**没有任何生产构造点**（只在定义 / `ALL:75`-`80` / `as_str:85`-`87` / `parse:91` 的取值域里出现）；`TimerSnapshot` 无 `mode`（`snapshot.rs:19`-`:75`），前端 `hooks.ts:63`-`:66` 只能判 `state` | 原措辞把一件**今天不需要**的事写成"要补一个字段"；裁决后它从"前置是 Rust 契约扩展"变成"**V0.1 明确不做**"，理由可核（拒绝点在 `coordinator.rs:492`） |
| **M10** | Minor | 引用 `viewWatermark.isStale` 时写成 `string \| null` | 正确签名是 **`isStale(stamp: VersionStamp, requestEpoch: string): boolean`**（`requestEpoch` 是 `string`，不是 `string \| null`） | `worktrace-web/src/components/viewWatermark.ts:42`；`string \| null` 那个是**全局**闸门的 `domainState.isStaleResponse`（`src/state/domainState.ts:149`）。P7 验收记录 §4.7（`:469`/`:474`-`:476`）已就这点写了警告并把两把水位分开；本条只订正**本计划**里的引用 | 两个 `isStale` 不是同一个东西；把全局那个的签名抄到本视图水位上，接线时会多写一个无意义的 `null` 分支 |
| **M11** | Minor | 路径 `scripts/check-bundle.*`（Task 4）、`tests/manual-v01.md`（Task 5） | 仓库根**没有** `scripts/`：门禁脚本在 `src-tauri/scripts/`（既有 `check-layers.ps1`），人工验收文档在 `src-tauri/tests/`（既有 `manual-shell.md`/`manual-sync.md`）；前端测试是 **vitest 就地**在 `src/**/__tests__/` | `ls /mnt/d/ProJect/worktrace`（无 `scripts/`）；`src-tauri/scripts/check-layers.ps1`；`src-tauri/tests/{manual-shell.md,manual-sync.md}`；`worktrace-web/src/pages/__tests__/`、`src/state/__tests__/` | 路径不存在 ⇒ 实施者会新建一棵错的目录树 |
| **M12** | Minor | P7 验收记录里归 P8 的 10+ 项在 P8 计划里没有 | 逐条登记「做 / 不做 + 理由 + 落在哪个 Task」，见文末「P7 验收记录里归 P8 的条目：逐条登记」 | 来源 `docs/validation/p7-acceptance.md` §6.3 第 9 条、§6.4 第 11–17 条、§6.5 第 19/24 条、§6.6 第 29–36 条 | 不登记 ⇒ P7 交出来的东西在 P8 手里再次落空（这正是上一轮审计的成因） |
| **M13** | Minor | 依赖的前置计划只写「P3、P5、P6、P7」 | 补一句今天的实施状态：**P3/P5/P6 均未实施**（全仓 0 命中），P7 已交付；Task 1/2/3 各自写了硬前置 | `worktrace-src/src/services/` 无 `stats.rs`/`export.rs`；`grep -rn "reconcile\|backfill\|discard_session" src/` 无生产实现（`docs/validation/p1-p4-review-backlog.md:22` FOLLOW-03 已核） | 三个前置计划今天都不在树里，计划必须按"顺序执行"读，而不是"都有了" |

**控制器裁决回填（2026-10-04；本计划审前留白的一处 + `mode` 的归属）**（**fix round 2（同日终审）见文末「P8 新增的 IPC 命令（9 条）」一节**：P8 要新增哪些命令、请求/响应与信封口径在那里逐条写死）：

- **②`mode`（§6.4 第 14 条）→ V0.1 判"不做"**：`start` 明确拒绝非前台（`coordinator.rs:492`-`:496`），三个非前台变体没有任何生产构造点 ⇒ 会话模式恒为前台，`useRunningTaskId` 不需要 `mode`。落地位置：本节的 **M9 行**、「边界」一节那条、文末登记表的 **§6.4-14 行**（三处口径一致：**V0.1 不做**；要做得先扩 `TimerSnapshot.mode`，**届时另开变更**）。
- **③导出落盘的能力选型 → 先按"离线约束"确认，不得默认插件可用**：本机缓存实测**没有** `tauri-plugin-dialog`/`tauri-plugin-fs`/`rfd`（Windows 侧 `~/.cargo/registry/cache` 只有 `tauri-plugin`/`tauri-plugin-log`/`tauri-plugin-opener` 三个 `.crate`；`src-tauri/Cargo.lock` 全文 grep 0 命中）⇒ **离线环境下它们装不上**，必须走零依赖方案。落地位置：Task 3 的「能力的选型与登记」条（含"可打开的位置"这半边**零新增能力**的实测依据）。

---

## Task 1：Today 的统计部分

文件：`src/pages/Today.tsx`（**新建**）、`src/App.tsx`（导航加第 5 项）、`src/pages/__tests__/Today.test.tsx`（新建）。

- [ ] **前提订正（2026-10-04 修订：因 C3）**：原措辞「在 P7 的基础上扩展」「P7 已做前两项（今日选择列表、当前任务）」**为假**——`src/pages/` 今天只有 `Inbox`/`Timer`/`Projects`/`Tasks`（`src/App.tsx:40` 的 `PageKey` 与 `:42` 的 `PAGES` 都是四项，没有 Today）；`src/ipc.ts:249` 的 `planFor` 虽然有转发，但 `grep -rn "planFor" src/pages/` = **0 命中**；"当前任务"今天只有 `src/pages/Timer.tsx:113`（计时页标题）与 `src/App.tsx:89`（状态栏）读 `snapshot.task_title`，**没有任何"今日页"**。⇒ 本任务是**从零建一个 Today 页**，不是扩展现成页面。
- [ ] **数据源（2026-10-04 fix round 2：因 I3）**：本页要用的命令今天**大半不存在**——`plan_for`/`timer_snapshot` 已有转发（`src/ipc.ts:249`/`:264`），而**今日聚合**那条命令属 **P5 新增**、**待确认/恢复**那几条属 **P8 新增**（见文末「P8 新增的 IPC 命令（9 条）」）。⇒ 本任务五项里，今天只有「今日选择列表 + 当前任务」两项有数据源。
- [ ] **硬前置（2026-10-04 修订：因 C4/M13）**：F-010 的后三项（确认人工工时 / 运行暂计 / 待确认时间）必须来自 **P5 的同一次查询**（同一 `as_of`/`revision`），而 `services/stats.rs`/`services/export.rs` 今天**不存在**（P5 未实施）。所以：**P5 的 Today 聚合入口（一条命令 + 一个 DTO，含 `as_of`/`revision`/`range`/`timezone`）落地之前，本任务只能做前两项**（今日选择列表 + 当前任务）**，不得**把 `TimerSnapshot.active_ms`（那是**当前会话**的暂计，`snapshot.rs:63`）冒充"今日确认人工工时"，也不得声称 F-010 完成。
- [ ] **要复用的既有件（逐条实测；本任务不新造）**：
  - 本视图水位：`createViewWatermark()`（`src/components/viewWatermark.ts:47`，签名 `isStale(stamp: VersionStamp, requestEpoch: string): boolean`，`:42`；`applied(stamp)` 在 `:44`）。用法照 `Inbox.tsx:114`/`:140`（`useState(createViewWatermark)` 拿稳定实例 → 响应 `isStale` 判旧 → 上屏后 `applied`）。
  - 命令转发：`planFor`（`src/ipc.ts:249`）、`timerSnapshot`（`:264`）、`addToPlan`（`:254`）、`removeFromPlan`（`:259`）——**后两条今天全仓零调用**（`grep -rn "addToPlan\|removeFromPlan" src/` 除 `ipc.ts` 定义处外 0 命中），Today 是它们的第一处消费方（今日选择列表的增删）。
  - 订阅入口：`src/state/hooks.ts` 的 `useDomainView:28` / `useDataEpoch:39` / `useTimerSnapshot:49` / `useInvalidation:78`（缓存失效计数进 `useEffect` 依赖，事件只作失效、数据从命令拉）。
  - 失败处置：`reportCommandError`（`src/components/commandError.ts:44`）+ `ErrorNotice`（`src/components/ErrorNotice.tsx`，文案恒为 Rust 的 `message`）。
  - 展示工具：`formatDuration`（`src/components/duration.ts:9`）。
  - DTO：`DailyPlanView`（`src/types/ipc.ts:191`：`tasks`/`data_epoch`/`revision`）、`TimerSnapshot`（`:202`）、`TaskRow`。
  - 测试替身：`src/pages/__tests__/fakeBackend.ts`（`createBackend:174`、`runningSnapshot:105`、`idleSnapshot:83`、`failure:163`）。
- [ ] **外壳改动（必须一起改，否则既有用例变红）**：`src/App.tsx` 的 `PageKey`（`:40`）加 `"today"`、`PAGES`（`:42`）加一项（如「今日」）、`PageView`（`:54`）加一个 `case`；**默认页保持 `inbox`**（`:96`）——`src/__tests__/App.test.tsx:126` 断言"导航四项 + 挂载时**恰好四条命令**"、`:157` 断言四项都在，两处要改成五项，而"默认页仍是收件箱"这条要保住（否则"恰好四条命令"那条判据失去意义）。`App.tsx:7` 明写**不引路由**（离线取不到 `react-router`），Today 与恢复页都走同一个 `switch`。
- [ ] **F-010 完整五项**：今日选择列表、当前任务、**确认人工工时**、**运行暂计**、**待确认时间****分别显示**，不预先相加（后三项依赖上面的硬前置）。
- [ ] 三个数字必须来自**同一次** P5 查询（同一 `as_of`/`revision`）；界面上要能看到这次口径的标注（范围、时区、"按当前分类"——R-03）。
- [ ] **作废记录不显示为待确认**：`voided_at` 非空或 `discarded` 的记录只在历史/审计里可查，不进"待确认"栏。
- [ ] 人工与机器**不合并**：人工只算 FOREGROUND；机器分列（BACKGROUND/PASSIVE）；WAITING 单列。界面上不得出现"总计 = 人工 + 机器"这类相加。
- [ ] 测试：五项分别渲染且互不覆盖；作废不进待确认；范围/时区标注可见；`revision` 变化后数字随之刷新；空数据时显示 0 而不是空白或错误；本视图水位判旧（旧响应不上屏）——用 `fakeBackend` 造"旧响应晚到"。

## Task 2：恢复确认、修正与补录界面

文件：`src/pages/Recovery.tsx`（新建）、`src/pages/History.tsx`（新建）、`src/pages/__tests__/{Recovery,History}.test.tsx`（新建；vitest 就地，见 M11）。

- [ ] **数据源（2026-10-04 fix round 2：因 I3；fix round 4：命令总数 9 → 11）**：本任务要用的 8 条命令（`reconcile`/`correct`/`backfill`/`discard_session`/`transition_task`/`accept_detected_clock_correction`/`retry_recovery`/`attention_overview`）**全部由 P8 新增**，逐条形状见文末「P8 新增的 IPC 命令（9 条）」；P3 只交付 `AppState` 入口与 DTO（P3 计划 §0.3 的 S1/S2/S4/S12 与 §0.5）。⇒ 恢复页与历史页的**第一跳读查询就是第 8 条 `attention_overview`**。
- [ ] **硬前置（2026-10-04 修订：因 M13）**：本任务整条依赖 **P3** 的 `reconcile` / `correct` / `backfill` / `discard_session` 服务与命令，而 P3 **今天未实施**（`grep -rn "reconcile\|backfill\|discard_session" src/` 无生产实现，见 `docs/validation/p1-p4-review-backlog.md:22` FOLLOW-03）。⇒ 本任务只能在 P3 落地后开工；**不得**在前端伪造这四个动作的语义（那正是本计划「边界」里"不在前端补"的意思）。
- [ ] **F-015 恢复确认**：recovering 记录要能看清"哪一段可信、哪一段待确认"；确认时要让用户给出**合法且不重叠**的起止，重叠时显示服务返回的具体冲突而不是笼统失败；已知单调时长**只作候选**，不作为默认值强迫接受。
- [ ] **丢弃与作废必须是两个动作**（02 §3 原文）：`reconcile(discard_uncertain)` 只丢不确定区间、保留此前闭合工时；`discard_session` 作废整次并单独入口 + 二次确认。界面上**不许合并成一个"丢弃"按钮**，文案要说清各自的影响范围。
- [ ] **F-017 修正与补录**：`correct` 只对 `finished` 开放，界面据状态禁用入口；`backfill` 独立入口，明确"不启动计时、不伪造完成事件"；删除误记是软删除，界面要说明"保留审计"。
- [ ] 版本冲突（`VERSION_CONFLICT`）要给出可操作提示（刷新后重新确认），不静默重试；`RECOVERY_REQUIRED` 要把用户导到恢复页而不是弹通用错误（**2026-10-04 修订：因 M8——今天导不过去，本任务要先补两处落点**）：① `src/components/commandError.ts:24` 的 `CommandErrorAction` 加第四档（如 `"recovery"`）并在 `reportCommandError`（`:44`）接上；② 外壳（`src/App.tsx:54` 的 `PageView` + `:96` 的 `useState`）给出最小的"切到恢复页"入口——**不引路由**（`:7` 写明离线取不到 `react-router`）。判据：造一条 `RECOVERY_REQUIRED` 失败 ⇒ 页面切到恢复页，且**不是**只弹一条通用提示。
- [ ] 测试：可信/待确认的视觉区分正确；丢弃与作废走不同命令；非 `finished` 时 `correct` 入口禁用；冲突与待恢复两类错误的提示文案正确。

## Task 3：导出与备份/恢复界面

文件：`src/pages/Data.tsx`（新建）、`src/pages/__tests__/Data.test.tsx`（新建；vitest 就地，见 M11）。

- [ ] **F-018 导出（2026-10-04 修订：因 C4——先把前置与能力写清，再谈界面）**：触发 P5 的生成函数、把内容**落盘**、给出可打开的位置；**界面数字与导出内容来自同一次查询**（同一 `as_of`/`revision` 要能在两处对上——导出 DTO 与 Today 用同一个 `services/stats.rs` 函数，P5 计划 `:64`）。**今天两件事都不具备**：① P5 的生成函数不存在（`worktrace-src/src/services/` 下没有 `stats.rs`/`export.rs` ⇒ **P5 必须先实施**，否则本任务只能先做备份/恢复那半边）；② 全仓**没有**文件系统/对话框能力（`src-tauri/src/commands/` 只有 `mod.rs` 与 debug-only 的 `dev.rs`；`src-tauri/Cargo.toml:20`-`:43` 无 fs/dialog 插件；`src-tauri/capabilities/default.json:1` 的 `permissions` 只有 `core:default`/`opener:default`）。**所以第一个交付物是"决定并登记这个能力"，不是一行落盘代码——裁决③（2026-10-04）已把结论定为零依赖方案，见下一条。**
- [ ] **能力的选型与登记（本任务第一步，先做这一步再看别的；2026-10-04 裁决③：先确认离线约束，不得默认插件可用）**：
  - **离线约束（必须先确认，再登记）**：本机是**离线**环境（crate 只能来自既有缓存）。**实测今天的结论是"装不上"**：Windows 侧 `~/.cargo/registry/cache` 里只有 `tauri-plugin-2.x` / `tauri-plugin-log-2.9.2` / `tauri-plugin-opener-2.6.0` 三个 `.crate`，**没有** `tauri-plugin-dialog`、`tauri-plugin-fs`（`rfd` 也没有）；`src-tauri/Cargo.lock` 全文 grep 这两者 **0 命中** ⇒ 它们从未进入依赖图，离线也拉不下来。开工前按同一方法**再确认一次**（命令：`grep -c "tauri-plugin-dialog\|tauri-plugin-fs" /mnt/d/ProJect/worktrace/src-tauri/Cargo.lock` + `ls /mnt/c/Users/$USER/.cargo/registry/cache/*/ | grep tauri-plugin`——注意缓存是 **Windows 侧**那份，WSL 的 `~/.cargo` 不是它），结论写进本任务的验收记录。
  - **不在 ⇒ 走零依赖方案（推荐默认，且今天是唯一可行路径）**：① **写文件**那半边由**新增一条 Rust 命令**完成（`commands/` 里落，走 `run_command` 与既有错误契约）：目标目录用 `platform::paths::app_data_dir()`（`platform/paths.rs:15`）下的 `exports/`，文件名带时间戳，**返回真实存在的绝对路径**；② **"给出可打开的位置"**那半边**不需要任何新能力**——`capabilities/default.json:1` 现有的 `opener:default` 已经包含 `allow-reveal-item-in-dir`（实测 `tauri-plugin-opener-2.6.0/permissions/default.toml`：`permissions = ["allow-open-url", "allow-reveal-item-in-dir", "allow-default-urls"]`），前端用已装好的 `@tauri-apps/plugin-opener`（`package.json:18`；**今天 `src/` 里 0 调用**，本任务是它的第一处消费方）调 `revealItemInDir(path)` 即可；③ **不引原生保存对话框**：路径固定 + 展示可复制的路径（剪贴板）就是 V0.1 的"手填/复制路径"口径；**若将来要"用户自选路径"**，那是需要网络的一次依赖引入，**另开变更**。
  - 需要写进 `src-tauri/Cargo.toml`（依赖）与 `src-tauri/capabilities/default.json`（`permissions`；`windows` 今天只有 `["main","sync-lab"]`）的具体条目在这里定稿并登记——**按上面的零依赖方案，两处都不需要改动**（这正是选它的第二个理由）。**在能力登记完成前，不得把导出落盘写成已完成**，也不得把落盘代码塞回 P5（P5 计划 `:27`/`:96`/`:101` 已把落盘订正给 P8）。
- [ ] **落盘的失败路径与"取消不算失败"口径**（与"能力登记"同等重要）：① **取消 ≠ 失败**——**按裁决③的零依赖方案，V0.1 没有"选择保存位置"这一步**（路径固定、对话框不引入），所以这一档今天只落在**其它用户主动放弃**的场景上（例如恢复的二次确认被取消）：不弹错误、不写文件、不重试，界面回到可再次触发的状态；**用例仍要能区分"取消"与"失败"**，并在注释里写明"将来若引入保存对话框，用户取消走的就是这一档"；② 目标目录不可写/磁盘满/路径过长 ⇒ 用 Rust 的错误契约返回（`AppError` 那套 `code`/`message`，`src/error.rs:11`-`:58`），界面走 `reportCommandError`（`commandError.ts:44`）——**不要**在 `invoke` 的 `catch` 里自己编文案；③ 能力**未登记/不可用**时必须**明确不可用**（按钮禁用 + 一句说明），不得静默失败。
- [ ] **命令编号（2026-10-04 fix round 2：因 I3；fix round 4：因 I-3 补齐）**：导出是「P8 新增的 IPC 命令（**11 条**）」里的**第 9 条 `export_data`**；**备份与恢复是第 10、11 条**（`backup` / `restore`）——它们的请求/响应形状、二次确认硬校验、以及"`restore` 是唯一不用 `run_command` 形状的命令"都写在文末那张表里。P6 只给服务原语与维护态，`commands/` 侧的包装**全部**由 P8 新增。
- [ ] **F-019 备份/恢复**：恢复是危险操作，需明确的二次确认与维护态提示；恢复期间用户命令被禁用，`DATA_RESTORE_IN_PROGRESS` 显示为"正在恢复"而不是通用错误；恢复完成后界面必须重新握手（`data_epoch` 已变），旧数据不得残留。**依赖 P6**（Task 4 的服务入口 + 维护态 + 新错误码；今天都还没有——`DATA_RESTORE_IN_PROGRESS` 全仓 0 命中，见 P6 计划「产出接口」）。
- [ ] **F-014 的界面侧**：导出与备份均为**用户明确操作**（04 §9），不做后台自动上传或联网校验。
- [ ] 恢复/备份的**跨窗口**口径（2026-10-04 修订：因 C4 同批）：维护态对 UI 的出口只有"被拒时的新码 + 维护结束后的 `data_epoch` 变化"两条（P6 不新造第三个事件名）。⇒ 另一个窗口**不会**立刻收到"开始恢复"的通知，它的入口只能靠"写命令被拒 ⇒ 按 `code` 禁用；下次握手成功 ⇒ 解禁"。**不要**假定存在维护态事件。

- [ ] **导出落盘归 P8（P5 只负责生成内容）**：P5 的生成函数按契约只返回字符串/字节、不自己写文件（P5 计划 `:7`/`:27`/`:96`；`services/export.rs` 尚未落地），落盘路径、权限与「可打开的位置」由本计划决定并实现——P5 计划把落盘划给 P7、P7 又划回 P5/P8，今天两边都没有落地，本计划是它唯一的落点。
  - **（2026-10-04 修订：因 C4）这条登记与本任务上面四条是同一件事的两半**：上面写"怎么做/怎么失败/什么算取消"，这里写"为什么只有 P8 能做"。另**订正一处引用**：P5 计划 `:7` 那句"唯一写操作是导出文件的落盘（**P7 决定路径**）"**尚未订正**（`:27`/`:96` 已加"落盘改归 P8"，`:101` 的注为准）；引用时以 `:27`/`:96`/`:101` 为准，别再把 `:7` 当权威。
  - **现状（实施前必读，别当成已存在）**：全仓**没有导出命令**（`src-tauri/src/commands/` 下只有 `dev.rs`/`mod.rs`），也**没有**文件系统/对话框类能力——`tauri-plugin-fs`/`dialog` 既不在 `src-tauri/Cargo.toml` 的依赖里，也不在 `src-tauri/capabilities/default.json` 的 permissions 里（该文件只有 `core:default` 与 `opener:default`）。
  - 所以本任务第一步是**决定并登记这个能力**：选型（离线可用性、权限最小范围）与要写进 `src-tauri/Cargo.toml`、`src-tauri/capabilities/default.json` 的具体条目在这里定稿，**不预设插件名**；能力登记完成后再实现「写文件 + 给出可打开的位置」。**在能力登记完成前，不得把导出落盘写成已完成**，也不得把落盘代码塞回 P5。验收：导出后拿到真实存在的文件路径并可打开所在位置；权限未登记时该功能明确不可用而不是静默失败。
- [ ] 测试：导出后能拿到路径；恢复维护态下按钮禁用且提示正确；恢复完成后 `data_epoch` 变化导致旧展示被丢弃；`DATA_EPOCH_MISMATCH` 触发重新握手而不是报错停摆（**这条今天就有机制**：`src-tauri/src/services/error_response.rs:84` 对 `AppError::DataEpochMismatch` 置 `requires_handshake = true`，前端 `src/components/commandError.ts:32` 因此走 `"rehandshake"`——用例要钉的是"**P8 新增的页面没有绕过这条**"）。

## Task 4：发布产物门禁（R-04）

文件：`src-tauri/scripts/check-bundle.ps1`（**新建**；**2026-10-04 修订：因 M11**——仓库根**没有** `scripts/` 目录，既有门禁脚本在 `src-tauri/scripts/check-layers.ps1`，放同目录才能被同一套调用姿势接上）+ `package.json` 里一条转发脚本（如 `"check:bundle"`）+ 一次实跑输出。

- [ ] **`DockviewDemo` 及其 CSS 不得出现在发布产物**。加一条构建后检查：在产物（仓库根的 `dist/`）里搜 `DockviewDemo` 与 dockview 的标记，命中即失败。源码保留不动（ADR-003 的保留意见只要求"排除在打包产物之外"）。
- [ ] 用**固定布局**；`dockview` 依赖保留在 `package.json` 但没有任何入口引用它——门禁要能区分"依赖存在"与"代码被打进产物"。
- [ ] 把这条检查加进发布前必须通过的清单，并在本计划的验收记录里给出一次实跑输出。
- [ ] **脚本自身的两条约束**（既有工程约定）：① `.ps1` 一律 **ASCII-only**（Windows PowerShell 5.1 按 ANSI 读无 BOM 的 UTF-8 脚本）；② `dist/` 路径从**仓库根**解析（`pnpm build` 的产物在仓库根，不在 `src-tauri/` 下），脚本被从哪个 cwd 调用都要能找到它——找不到就**报错退出**，不得当成"检查通过"。
- [ ] **顺带并入一项（来源 `docs/validation/p7-acceptance.md` §6.4 第 16 条）**：把 `cargo check --offline --lib --release`（发布档位编译探针）也接进这条门禁的调用链或显式登记为"**不并入**"并写明理由（P7 当时是手工一次性证据，理由是会给其他人加约 93 秒）。二选一都要写清，不能悬空。
- [ ] 测试：故意在入口里 import 一次 `DockviewDemo`，确认门禁**会失败**（否则门禁是假绿）；移除后恢复通过。

## Task 5：V0.1 端到端人工验收

文件：`src-tauri/tests/manual-v01.md`（**新建**；**2026-10-04 修订：因 M11**——人工验收文档在 `src-tauri/tests/`，既有两份 `manual-shell.md`/`manual-sync.md`，本文照它们的 §0 环境表与"记观察到的现象"写法）、对照 [总纲 §6](2026-10-03-v01-plan-index.md)。

- [ ] 按 F-ID 逐条人工验收并记录观察结果（不能用单元测试代替平台行为，08 §6）：
  - F-001/F-002/F-003：捕获、理清、非法跃迁；
  - F-009/F-011：关掉全部窗口后托盘可用、计时继续；重开立即拉快照；
  - F-010：Today 五项数字与数据库明细一致；跨日 `23:50–00:10` 两天各 10 分钟；
  - F-015/F-017：强杀重启后四类判定各一条，确认/丢弃/作废各一次；
  - F-018：导出 JSON 用外部工具重算与界面一致；周回顾逐项核对；
  - F-019：备份 → 恢复 → 旧请求被拒；
  - F-020：多窗口制造乱序/丢通知并确认收敛；
  - F-014：拔网线跑完整 V0.1 功能，无报错、无降级提示。
  - **（2026-10-04 修订：因 M12）本节还要覆盖前文登记的三条**：托盘「快速捕获」/「当前任务」的**视图跳转**（§6.4 第 11 条，判据：点到之后前端确实切到捕获输入/计时视图，不是只抬窗）、**Windows 打包产物启动一次**（§6.4 第 15 条）、**退出事务失败时的用户可见提示**（§6.3 第 9 条；归 P6 实现，P8 只核对现象）。
- [ ] **500 ppm 跨机器校准（人工 / 跨机器验证项；来源：`docs/validation/p1-p2-acceptance.md:116` 与 `docs/validation/p1-p4-review-backlog.md:24` 的 FOLLOW-05）**：在**至少两台不同机器**上验证长期漂移界 `abs(wall - L(M)) > 2000 + floor(elapsed_ms × 500 / 1_000_000)` 是否成立（判据原文见 `docs/validation/p2-clock-mapping.md:152`/`:168`）。本机观察约 233 ppm、500 ppm 只是**初始可版本化策略、不是通用平台结论**（`docs/validation/p1-p2-stability.md:21`），所以必须换机器复验。做法：按 `src-tauri/tests/manual-sync.md` §0 的环境表登记**机器 / CPU、Windows 版本（`winver`）、提交号、验收人 / 日期**，每台机器至少覆盖「长时间挂机 + 一次休眠/唤醒 + 正向改时 + 反向改时 + 前台计时进行中」几类情况，跑足够长的 elapsed，逐条记下**实测漂移值**与界值结论。**结论必须写明是哪台机器、哪个系统版本、哪个提交**；界值不成立时给出实测数值与复现步骤并回写 `docs/validation/p2-clock-mapping.md`，不得把单机结论当成跨机器结论。
  - 同一轮实机验收里复核 P6 新增的「正式 OS 事件接线」条目（锁屏/休眠/唤醒/改时的到达延迟与行为）：实现归 P6、**结论归本计划**——P7 计划 `:49` 已声明 V0.1 没有事件源，`docs/validation/p7-acceptance.md` 的 **§5.2「只有实机才能验」表末行** 与 **「仍未达成 / 存疑」表**两处**现**为「**归属已闭环：实现归 P6、实机结论归 P8**」（原记「仍无归属」，2026-10-04 订正），V0.1 收尾时这一格不能仍是空的。
- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [ ] 逐条核对 [总纲 §6「V0.1 的完成定义」](2026-10-03-v01-plan-index.md)：18 项验收通过、断网可用、强杀恢复、备份恢复演练、`DockviewDemo` 不在产物里。
- [ ] 完成门槛：`cargo fmt --check`、`cargo test`、`cargo clippy --all-targets` 全绿；前端 `pnpm test`（vitest run）与 `pnpm build`（含 `tsc`）通过；`src-tauri/scripts/check-layers.ps1` **六条规则**全过（**2026-10-04 修订：因 M11**——脚本在 `src-tauri/scripts/` 下，规则位置 `:156`/`:157`/`:158`/`:167`/`:171`/`:178`），Task 4 的 `check-bundle.ps1` 通过；**P1–P7 测试无回归**。
- [ ] **不得把仓储/服务层测试标为"UI 已验收"**。人工验收记录要能对上具体版本与机器。
- [ ] 验收通过后，对照 06 §5 的记录项开始**两周本人真实使用评估**（记录漏记/忘停/修正频率、捕获/切换操作负担、周回顾是否实用）——这是 V0.1 之后的动作，不属于本计划的完成条件，但要在本计划收尾时启动。

---

## P7 验收记录里归 P8 的条目：逐条登记（2026-10-04 修订：因 M12）

> **来源**：`docs/validation/p7-acceptance.md` 第六节（§6.3 / §6.4 / §6.5 / §6.6）与文末「仍未达成 / 存疑」索引表。
> 上一轮审计的结论是"**10+ 项在 P8 计划里没有**"——这一节把它们逐条落到"做 / 不做 + 理由 + 落在哪个 Task"，
> 目的是让 P8 收尾时**没有一条是悬空的**。**不做的条目必须写明理由**，不得只留空。

| 来源 | 项 | 做 / 不做 | 理由与落点 |
| --- | --- | --- | --- |
| §6.3-9 | 退出事务失败的用户可见提示（当前只记诊断 + 非零码退出） | **做（归 P6 实现，本计划只在 Task 5 核对）** | 诊断出口归 P6（P7 已判"P6/P8"）；P8 的落点是 Task 5 的实机步骤里加一条"制造一次退出失败 ⇒ 有可见提示"，若 P6 未实施则记「未做，原因：无界面出口」 |
| §6.4-11 | 视图跳转（托盘「快速捕获」跳到捕获输入框；「当前任务」跳到计时/任务） | **做** | 今天 `lib.rs:264` 的 `tray_dispatch` 把 `CurrentTask`/`QuickCapture` 都归到 `TrayDispatch::Window`（`:266`），`on_tray_action:277` 只抬窗（`:281`，其注释 `:279`-`:280` 已把视图跳转登记给 P8）。做法：Task 1 建立的那套"外壳视图切换"（Task 2 因 M8 也要用）+ 托盘动作到达前端（新增一条事件名或复用既有窗口事件，**要先登记再实现**——见 P6 计划「下游接口」关于"不新造第三个事件名"的口径） |
| §6.4-12 | 动态菜单标签（把当前任务标题做成菜单项） | **不做** | 菜单是 `platform/tray.rs:88` 的**静态** `MENU_ITEMS`（`:131` `build`）；动态标签要"每秒重建菜单 + 把标题送进托盘"，属体验项且不在 F-011 的四项判据里。**登记为 V0.1 之后**，理由写进验收记录 |
| §6.4-13 | 项目详情分页（详情只列第一页，服务端上限 100） | **做（最小）** | `src/pages/Projects.tsx:144`/`:150` 已用 `statuses: []` + `project:{id}` 且文案已明说"本次列出最早的 N 条"（不静默截断）。最小增量：按服务端 `total` 给"加载更多"或分页；若时间不够，**登记为不做**并写明"现状不静默截断，判据已就位" |
| §6.4-14 | `useRunningTaskId` 判别不了 `mode` | **V0.1 不做（2026-10-04 裁决②）** | 理由可核：`start` 明确**只接受前台**（`services/timer/coordinator.rs:492`-`:496`，`detail` 在 `:494`），`SessionMode::{Background,Passive,Waiting}`（`domain/session.rs:69`-`:71`）没有任何生产构造点 ⇒ V0.1 会话模式恒为前台，`useRunningTaskId`（`hooks.ts:63`-`:66`）判 `state === "running"` 就够。**若后续引入后台计时**：先扩 `TimerSnapshot.mode` 契约（`snapshot.rs:19`-`:75`；会动 2/15 份 `src/types/__snapshots__` 与 Rust 侧 serde），**届时另开变更** |
| §6.4-15 | 多入口开发/打包路径 + Windows 打包验证（00 §7） | **做** | 与 Task 4 的 R-04 门禁同一轮：Task 5 的实机步骤里加"安装包/`pnpm tauri build` 产物启动一次"（`manual-shell.md` §0 已有构建方式一栏可复用） |
| §6.4-16 | `cargo check --lib --release` 进正式门禁 | **做或显式登记不做** | 见 Task 4 那条（并入门禁链，或写明"不并入 + 约 93 秒代价"） |
| §6.4-17 | Inbox 页的标签入口（`tag_task`/`untag_task`） | **做** | 与 Today 同病：`ipc.ts:219`/`:224` 有转发，`src/pages/**` **0 调用**。落点：Inbox 或任务页加最小打标/去标入口（复用 `useInvalidation` + `reportCommandError`）；不新造查询（`list_tags` 已在 `ipc.ts:204`） |
| §6.4-18 | 归档/完成状态的批量操作 | **不做** | 计划边界已声明"V0.1 无此入口"，验收记录里复述即可 |
| §6.5-19 | `Drop for Scheduler` 不查 `holds_app_lock`（自死锁防线可被绕过） | **条件项：本计划若新增"拥有并显式丢弃 `RunningApp`"的退出路径就必须先处理** | `platform/scheduler.rs:147`-`:158` 的注释已写明"当前生产路径不可达"。⇒ Task 5/Task 3 若要引入新的退出/重启入口（例如恢复后重启进程），**必须先放掉那把锁**（与 `RunningApp::shutdown:256` 的姿势一致）；否则登记为"不做，生产路径不可达" |
| §6.5-24 | `manual-sync.md` §2.5 两条不可观察（缺计数出口） | **做（与 §6.6-34 合并）** | 两条都要一个**实机可读的计数出口**：给 `BroadcastDiagnostics::dropped`（`services/events.rs:130`-`:134`，`diagnostics():237`）与"恢复后立刻有一次 `get_revision`"补一条 **dev-only 只读命令**（与 `commands/dev.rs` 同一开关纪律）或明确标"不可观察"并说明理由；**不得凭感觉判通过**（P7 §6.5-24 原话） |
| §6.6-29 | 计时判据链的"同一条规则两处实现"（前端 `orderTimer` 五级 vs Rust `is_stale_tick` 两级，后者生产零调用） | **做（本计划的双窗口实机验收正落在这一格）** | Task 5 的 F-020 实机项要覆盖它；若结论是"要共享向量"，落点是 `src/types/__vectors__/`（既有 `revision-gate.json` 的两侧共读模式），**不在本计划临时造** |
| §6.6-30 | `applied == None` 时的通知（两侧都没覆盖） | **做** | Rust 侧补一例（`src-tauri/tests/event_protocol.rs` 现有 5 个 `#[test]`，`§6.6` 第 30 条给了行号），并在 `src/types/__vectors__/revision-gate.json` 的 `note` 里把"刻意不含这一格"的理由与新增用例对上（向量是两侧共读，改它要同步两侧读法） |
| §6.6-32 | `AuthorityTarget::Deserialize` 零调用、注释与事实相反 | **做（最小：改注释）** | `src-tauri/src/error.rs:244`-`:245` 的理由是假的（P7 的 IPC 从没从字符串解析过 `kind`）。删 `Deserialize` 属删代码（需用户确认），⇒ 本计划只**订正注释**（或按裁决删，二选一都要登记） |
| §6.6-33 | 拆 `services/catalog.rs`（771 行）/ `commands/mod.rs`（1338 行） | **做（本计划还要往 `commands/mod.rs` 加命令）** | P4 账本与 P7 Task 5 报告都建议拆；P8 是"下一个加命令的人"。最小落点：把请求 DTO 与命令体按域拆成子模块，`commands/mod.rs` 只留骨架与注册表（拆分不改变任何 IPC 形状） |
| §6.6-34 | `BroadcastDiagnostics::dropped` 无实机出口 | **做** | 与 §6.5-24 同一处合并处理（一条 dev-only 只读命令，或把注释改成事实） |
| §6.6-35 | 跨语言三对常量无机械检查（`worktrace:event`、`domain.changed`、`timer.tick`） | **做** | 一条用例读两侧源码比对三个字面量：`src-tauri/src/lib.rs:74` vs `worktrace-web/src/ipc.ts:66`；`src-tauri/src/services/events.rs:46`/`:48` vs `worktrace-web/src/types/ipc.ts:97`/`:98`。做法照 `src/types/__tests__/snapshot-contract.test.ts`（**不要造生成器**） |
| §6.6-36 | 死组件与残留文件（`FloatingInput`/`FloatingSelect`/`DockviewDemo` + 过期 README、两个 stray `.log`） | **部分做** | `DockviewDemo` 与 R-04 一起处理（Task 4 门禁 + 反向验证）；`FloatingInput`/`FloatingSelect`/`components/README.md`/两个 `.log` **不做**——删文件与删依赖都**需用户确认**（只登记，不在本计划里删） |

**同批复核过、不在上表里的条目**（避免"漏读"的误会）：§6.6-31 `SessionAttention` 零调用归 **P3**（四类判定接入时决定去留）；§6.7-37 采样线程 panic 静默死亡归 **P6**（已写进 P6 计划 Task 2）；§6.7-38（第二次启动用同进程模拟）、§6.7-40（枚举字符串解析三份同形拷贝）、§6.7-41（`ipc_requests.rs` 的平凡访问器用例）三条已由 P7 裁定为"可选加固 / 本轮不做 / 留着"，**本计划不动**；§6.7-39（快照"逐字节"其实是经 `serde_json::Value` 归一化后的字节）与 §6.7-42（`list_tasks` 的 `statuses: []` IPC 层只读用例钉不住语义）是 P8 候选：前者与 Task 4 的门禁同一批（改判据或改文案，二选一），后者**登记为不做**（P7 已判"可选"，且本计划不加任务查询）。


## fix round 2（2026-10-04 终审：P8 侧 2 处 Important）

| # | 级别 | 原措辞 | 新措辞 / 落地 | 依据（实测 file:line） | 为什么必须改 |
| --- | --- | --- | --- | --- | --- |
| **I3** | Important | P8 计划里**没有**"要新增哪几条 IPC 命令"的登记（恢复页与今日页因此没有数据源） | 新增下节「P8 新增的 IPC 命令（9 条）」，逐条给命令名 / 请求→响应 / 信封与广播口径 | P3 计划的「边界」一节明写 **P3 不新增 `#[tauri::command]`、不改 `commands/mod.rs`**（只交付服务层 + `AppState` 入口 + 测试）；其 §0.5 末尾与「完成门槛」的接口归属条目把 `attention_overview` 与 `retry_recovery` 的 **IPC 命令点名给 P8** | 不登记 ⇒ P3 的服务入口没人接，恢复页/今日页没有数据源 |
| **I5** | Important | 新增第六个错误码的"四处联动"不含前端 | 见 P6 计划「产出接口」第 1 条：`worktrace-web/src/types/ipc.ts:86` 注释"五个稳定码"、`src/__tests__/ipc.test.ts:117` 用例名"五个码"要改成六个 | `types/ipc.ts:87`-`:93`（`ERROR_CODES`）；`types/__tests__/snapshot-contract.test.ts:291` 把它当**取值域**用（加第六项**不破坏**该用例） | 改了码不改测试名与注释 = 又一处"文档说的与树不一样" |

### fix round 4（2026-10-04 定向复评）：备份/恢复的两条 IPC 命令（I-3）

| # | 级别 | 原措辞 | 新措辞 | 依据 | 为什么 |
| --- | --- | --- | --- | --- | --- |
| **I-3** | Important | Task 3 写着备份/恢复"`commands/` 侧的包装由 P8 在 Task 3 一并新增（**同属该表**）"，但那张表只有 **9 条**（8 条 P3 + `export_data`）——**这两条命令没人登记**；P6 只给了服务原语（`begin_restore` / `prepare_and_swap` / `commit_restore` / `abort_restore`） | 表里**补第 10、11 条**：`backup` 与 `restore`；「**9 条**」一律改成「**11 条**」，连带更新注册表/`ipc_commands.rs`/`src/ipc.ts` 的计数口径（**24 → 35**） | P6 计划「产出接口」第 5 条只到服务原语；P8 Task 3 的"同属该表"在**表里没有对应行**——登记与内容自相矛盾 | 恢复页与导出/备份页**没有数据源**；且 `restore` 是全项目唯一的危险操作，它的二次确认与维护态语义必须在命令层写死 |

### fix round 3（2026-10-04）：P3 引用改为符号/小节定位

| # | 级别 | 原措辞 | 新措辞 | 依据 | 为什么 |
| --- | --- | --- | --- | --- | --- |
| **②** | Minor（跨文件） | 命令表里引用 P3 一律带行号（`p3:501`/`532`/`556`/`557`/`601`/`95`/`96`/`199`/`233`-`240`/`641`/`645`/`632`/`204`/`271`） | 全部改成**符号/小节定位**（`ReconcileRequest`、P3 计划 **§0.3 的 S1/S2/S4/S12**、**§0.5 的 `AttentionOverview`/`PendingIntervalItem`**、其「下游接口」与「完成门槛」的接口归属条目）；行号**只在一张「P3 侧符号 ↔ 行号」对照表里给一次**，并标明"**仅供参考，以符号/小节名为准**" | P3 计划本轮 511 → **667 行**；按现场 `grep -n` 重定位：`AcceptClockCorrectionRequest:96`、`ClockCorrectionAccepted:97`、`rescan_recovery:77`、S12 `:194`-`:206`、`AttentionOverview:235`、`PendingIntervalItem:260`、`ReconcileRequest:503`、`CorrectRequest:534`、`BackfillRequest:558`、`DiscardSessionRequest:559`、`TransitionTaskRequest:603`、`P3 不新增命令:27`、`P8 的 8 条:634`、`attention_overview IPC 归 P8:273` | 行号会因并行改动漂移（本轮 P3 就漂了 +2/±1）；符号与小节名不会 |

### P8 新增的 IPC 命令（11 条）

**共同口径（全部照 P7 既有姿势，别新造）**：

- 每条 = `#[tauri::command] pub async fn x(…)`（包装，只有一行转发）+ `pub fn x_impl(app: &mut AppState, request: XRequest) -> Result<XResponse, AppError>`（命令体）——见 `commands/mod.rs:27`-`:34` 的「命令体与包装分开」；
- 包装一律走 `run_command`（`commands/mod.rs:127`）⇒ 自动落在同一把 `Mutex<AppState>` 上、自动被维护态挡住（P6 的 I1）、失败时自动经 `capture_error_response` 带权威 `epoch`/`revision`；
- **写命令**（1–6、7、9）一次成功业务写**恰好一次** `bump_revision`（服务层做，命令层不碰），并用 `WriteOutcome::into_parts()` 的第二位决定要不要 `announce` `domain.changed`；**只读命令**（8）不带信封、不加 revision；
- 需要 epoch/版本校验的写命令一律用 `crate::envelope::WriteEnvelope`（新建 ⇒ `for_create`；改既有对象 ⇒ `for_update`）；**集合/关系类操作传 `expected_row_version: None`**（P7 的口径，`envelope.rs:45`-`:48`）；
- 请求 DTO 一律**一个 `request` 参数**、枚举走**字符串**（`commands/mod.rs:38`-`:47`）；
- 响应必须能给出 `data_epoch`/`revision`（要么自带这两个字段，要么是带它们的报告/`CommandOutcome`）——页面的**本视图水位**（`viewWatermark`）就靠它判旧。

**P3 侧一律按符号/小节定位（2026-10-04 fix round 3）**：P3 计划本轮从 511 行长到 **667 行**，行号会因并行改动漂移 ⇒
下表引用 P3 时**只写符号名与小节**：DTO 形状见 P3 计划的 **§0.5「P3 新增的 DTO 形状（钉死；P8 照抄）」**，
接缝编号（S1/S2/S12…）见其 **§0.3「P3 必须新增的接缝」**，命令入口清单见其 **「下游接口」** 一节。
**行号只在下面这张对照表里给一次**，且**以符号/小节名为准**：

| P3 侧的符号 / 小节 | 截至 2026-10-04 21:2x 的 P3 稿行号（**仅供参考，以符号为准**） |
| --- | --- |
| `pub struct AcceptClockCorrectionRequest` / `ClockCorrectionAccepted` | `:96` / `:97` |
| §0.3 的 **S1**（`AppState::rescan_recovery`） | `:77` |
| §0.3 的 **S12**（`AppState::retry_recovery`，故障态唯一生产出口） | `:194`-`:206` |
| §0.5 的 `AttentionOverview` / `PendingIntervalItem` | `:235` / `:260` |
| `pub struct ReconcileRequest` | `:503` |
| `pub struct CorrectRequest` | `:534` |
| `pub struct BackfillRequest` / `DiscardSessionRequest` | `:558` / `:559` |
| `pub struct TransitionTaskRequest` | `:603` |
| 「P3 交付服务层 + `AppState` 入口 + 测试，**不新增 `#[tauri::command]`**」 | `:27` |
| 「P8 的 8 条命令」/「`attention_overview` 的 IPC 由 P8 新增」 | `:634` / `:273` |

| # | 命令名（`lib.rs` 注册表 + `commands/mod.rs`） | 请求 → 响应 | 口径与备注 |
| --- | --- | --- | --- |
| 1 | `reconcile` | `ReconcileRequest`（P3 计划 §0.3 的 S2 命令入口）→ `ReconcileReport` | 写；服务入口 = P3 的 `AppState::reconcile`（S2）。**只做 `discard_uncertain`**——"作废整次"是第 4 条，两个动作在界面上也不许合并 |
| 2 | `correct` | `CorrectRequest`（P3 §0.3 的 S2）→ `HistoryEditReport` | 写；**只对 `finished` 开放**，界面据会话 `state` 禁用入口 |
| 3 | `backfill` | `BackfillRequest`（P3 §0.3 的 S2）→ `HistoryEditReport` | 写；独立入口，明确"不启动计时、不伪造完成事件" |
| 4 | `discard_session` | `DiscardSessionRequest`（P3 §0.3 的 S2）→ `HistoryEditReport` | 写；单独入口 + 二次确认，文案说清影响范围 |
| 5 | `transition_task` | `TransitionTaskRequest`（P3 §0.3 的 S2）→ `TaskTransitionReport` | 写；P3 完成后启用 P7 暂缓的完成/取消、Blocked/Waiting、reopen 与托盘「完成」占位项（`p7-acceptance` §6.1 第 1/2 条） |
| 6 | `accept_detected_clock_correction` | `AcceptClockCorrectionRequest`（P3 §0.3 的 **S4**）→ `ClockCorrectionAccepted` | 写；"显式接受一次已检测的墙钟校正"（08 §1），不自动接受 |
| 7 | `retry_recovery` | epoch 请求 → `TimerSnapshot` | **它会提交一笔恢复事务**（不是纯读）⇒ 按写命令处理，但**不带 `WriteEnvelope`**（没有用户可编辑对象，判据在协调器里）；服务入口 = P3 的 `AppState::retry_recovery`（P3 §0.3 的 **S12**），**触发是用户显式点"重试"**，不做定时自动重试（S12 末条） |
| 8 | `attention_overview` | epoch 请求 → `AttentionOverview`（P3 计划 **§0.5**；服务入口 `services::recovery::attention_overview`） | **只读**：同一读事务取 `data_epoch`/`revision`；恢复页与"待确认"栏的**唯一数据源**（P3「下游接口」第 2 条） |
| 9 | `export_data` | `ExportRequest { format: "json"｜"markdown", range, timezone, expected_data_epoch }` → `ExportResult { path: String, bytes: u64, data_epoch: String, revision: i64 }` | **P8 自己的**（Task 3）：调 P5 的生成函数（`services/export.rs`，返回字符串/字节）→ 写入 `<app_data_dir>/exports/` → 返回**真实绝对路径**（界面用 `revealItemInDir` 打开所在位置）。**依赖 P5**：P5 未实施前只能先做落盘骨架 |
| 10 | `backup` | `BackupRequest { expected_data_epoch: String }` → `BackupResult { path: String, bytes: u64, data_epoch: String, revision: i64 }` | **写**（它产生一份库副本，**不修改业务事实** ⇒ 不加 `revision`、不广播 `domain.changed`；`WriteEnvelope` 只用 `expected_data_epoch` 做身份校验，`expected_row_version: None`）。服务入口 = P6 的 `services/backup.rs`（`VACUUM INTO`，见 P6 Task 1 的裁决）；**维护态期间被 `guard_writable` 拒绝**（P6 的 I1）。**进程内串行**：备份走同一把锁的短临界区，不放长活到锁外 |
| 11 | `restore` | `RestoreRequest { backup_path: String, expected_data_epoch: String, confirmed: bool }` → `RestoreResult { data_epoch: String, revision: i64, applied: bool }` | **写、且是全项目唯一的危险操作**：① `confirmed` 必须为 `true`（前端的**二次确认**是硬前置，命令层再校验一次，缺它就拒绝——不要只靠界面）；② 服务入口 = P6 的三段流程（`begin_restore` →（锁外）`prepare_and_swap` → `commit_restore`/`abort_restore`），**这是唯一不进 `run_command` 单临界区形状的命令**（见下「执行骨架」）；③ 成功后返回**新 `data_epoch`**，前端据此重新握手（旧展示必须丢）；④ 失败（回滚成功）返回 `applied: false` + **原 `data_epoch`**，**不是错误**——界面按"恢复未生效"提示，进程不死 |

**两条归属订正（写给控制器）**：你给的清单是"7 条 P3 命令 + 导出 = 8"；但 P3 计划**自己**把 `retry_recovery` 的 IPC 也点名给 P8（P3 计划 §0.3 的 **S12 末条**与「完成门槛」里的接口归属条目），所以 P3 侧是 **8 条**（正是 P3 计划写的"P8 的 8 条"），加上导出共 **9 条**。

**注册与门禁的连带改动（P8 的交付物，不能只写 Rust 半边）**：

- `lib.rs:113`-`:159` 的 `invoke_handler` 注册表加 **11** 条；`tests/ipc_commands.rs` 逐条覆盖 **24 → 35**；
- `tests/ipc_snapshots.rs` 与 `src/types/__snapshots__/*.json`（现 15 份）为**新响应类型**补快照；`tests/ipc_requests.rs` 补新请求 DTO 的字段样本；
- `src/ipc.ts` 加 **11** 个转发函数 + `src/types/ipc.ts` 加对应 DTO（P3 的抄其 §0.5 / 各 Task；`backup`/`restore` 的抄 P6「产出接口」第 5 条的返回形状——**形状不自己发明**）；
- **`restore` 的执行骨架（写死；与其余 10 条不同）**：它要跨**两个**临界区（进维护态/装回）＋一段**不持锁**的长活，所以**不能**用 `run_command` 的"一次 `lock_app` 包住整个 body"形状 ⇒ 需要一条专用的包装（名字实施时定，如 `run_maintenance_command`），它把 `SharedApp` 交给命令体、由命令体自己按段 `lock_app`；**其余 10 条一律用 `run_command`**。这条差异要写进实现注释与用例（否则下一个人会把它改回 `run_command`，长活重新占住锁 ⇒ 维护态"快速失败"失效）；
- `src/__tests__/App.test.tsx:126` 的"挂载时**恰好四条命令**"这类断言要按新页面重新核对（Today/Data/Recovery 页各自的第一跳读查询会改变命令集合）。

---

## 下游
本计划是 V0.1 的最后一份。完成后 V0.1 的全部 18 项验收应可逐条出示记录；后续版本（V0.1b 的 HUD 与全局热键、V0.2 的并发/权重/排期）另立计划。

## 轻量 GTD 与联动入口的最终整合

- [ ] Today/周回顾链接到 P7 的 Projects、下一步行动、等待、阻塞和情境列表，不新建第二套查询或重复页面。项目创建/改名/归档界面验收归 F-004/P7，本计划复核回归。
- [ ] P3 完成后接入 transition_task，启用 P7 暂缓的完成/取消、Blocked/Waiting、reopen 和托盘完成动作；同一服务一次事务结束/暂停相关会话，recovering 整体拒绝。补全 F-003/F-011 最终验收。
- [ ] 在正式发布库上完成 P3 扫描接入和 P6 恢复演练后，才将早期 P7 演示视为完整 V0.1；早期开发库测试不是恢复功能验收。

## P3 接入补正（2026-10-04）

retry_recovery 的 IPC 请求 expected_data_epoch 映射到 AppState::retry_recovery(expected_data_epoch)。恢复丢弃/整次作废不得承诺无时长候选的端点原样留在区间行；这些候选端点保留在 time_edit.before_json，区间行清 ended_at 为 NULL，以兼容既有 CHECK，已知可信时长区间的起止不动。界面不显示候选端点为已确认工时。

## P3 前遗留的消费门槛（2026-10-04）

- [ ] 按 [pre-p3-closure](../../validation/pre-p3-closure.md) 关闭 #1–5/#11–17/#19–24/#29/#33–34 及平台实机/发布项；项目详情分页改为必做，未关闭对应项不能宣告 P8/发布完成。
- [ ] 新增 IPC 前先拆命令职责，并补真实包装消费的 targets 构造测试、TS 请求字段/必填性/值类型及完整枚举域机械检查；反向验证必须可失败，不用一份脱离包装的期望列表充数。
- [ ] manual-sync §2.1–§2.5 执行前交付 debug-only 的按窗口 get_revision、延迟命中和广播 dropped 计数出口；release 不注册，不写业务库；确认注入命中后才记实验结论。
- [ ] 修正同步手册计时判据：至多 30 秒启动校验与 IPC/渲染耗时分别记录，不能一处要求 ≤30 秒、另一处接受 31 秒、失败却只看 60 秒。真实隐藏判据以 visibilityState 为准，遮挡不保证 hidden。
- [ ] 保留复制组件/模板依赖不等于接入；DockviewDemo 不进发布产物的反向验证仍必做。Context 显示沿用“上下文”，tick 完整排序只由前端 orderTimer 承担；无需复制第二套 Rust 展示状态机。
- [ ] P3 已提交、后置门禁刷新失败时刷新权威版本与提示，禁止自动重发原写命令；重试入口复用 S12，确认旧工时与接受时钟校正分开。
