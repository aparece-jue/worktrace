# P8 开工前勘察与交接（2026-10-10）

**基线**：`dev = origin/dev = 66c9887`，工作树干净。最近一次完整门禁（2026-10-10，HEAD `a92541f`）：
Rust **753 passed / 0 failed / 1 ignored**、前端 **142 passed**、`tsc`+`vite build` 通过、clippy/格式/分层全过、八项检查全 exit 0
（证据 `D:/ProJect/worktrace-review-20261010-gate/result.json`）。生产代码自 `a92541f` 未变。

**勘察方法**：把 [P8 计划](../superpowers/plans/2026-10-03-p8-stats-recovery-export-ui.md) 里每一条"可检假设"逐条对着当前树核（只读，不跑 cargo/pnpm），
再做 13 条命令的**可接线性**核对。计划写于 2026-10-03…10-08，而 **P5/P6 其后已全部实施** ⇒ 多处前提过期。

---

## 1. 计划假设的订正（会让实施者走错路的排前面）

### A. 硬前置已满足（否则会去重做 P5/P6，或误判不能开工）

| # | 计划原文 | 实测 | 处置 |
| --- | --- | --- | --- |
| **A1** | Task 3 `:91`「**P5 的生成函数不存在**（`services/` 下没有 `stats.rs`/`export.rs`）⇒ **P5 必须先实施**」 | `services/stats.rs`（45.7 KB）与 `services/export.rs`（41.2 KB）**都在**；`AppState::{stats_today:1374, stats_snapshot:1356, export_json:1394, export_weekly_markdown:1418}` 四个入口齐 | **删除该硬前置**。这是全表最危险的一条——照它读会以为 P8 不能开工 |
| **A2** | Task 3 `:98`「**依赖 P6**（Task 4 的服务入口 + 维护态 + 新错误码；**今天都还没有**——`DATA_RESTORE_IN_PROGRESS` 全仓 0 命中）」 | 第六个错误码 **23 处**已落地（`error.rs:55/:143`、`bootstrap.rs:1218`、前端 `types/ipc.ts:93`…）；`services/backup.rs` 65 KB、`restore_from_backup`/`begin_restore`/`prepare_and_swap`/`commit_restore`/`abort_restore` 都在 | **改为历史记录**。文末「2026-10-10 P6 实现交接对齐」已写 P6 已交付，但 **Task 3 正文没改** ⇒ 只读 Task 3 的人仍会误判 |
| **A3** | Task 1「P5 已交付但**未新增 IPC**；P8 新增第 12 条 `stats_today` 包装」 | **仍准**：Rust 侧 `stats_today`/`export_data`/`history_view` **0 命中** | 照做 |
| **A4** | Task 3「能力未登记；零依赖方案」 | **仍准且今天可核**：`capabilities/default.json:1` 只有 `core:default`/`opener:default`（`windows: ["main","sync-lab"]`）；`Cargo.lock` 对 `tauri-plugin-dialog`/`tauri-plugin-fs`/`rfd` **0 命中**；Windows 侧 `~/.cargo/registry/cache` 只有 `tauri-plugin-2.6.3/2.7.0`、`-log-2.9.2`、`-opener-2.6.0` ⇒ **"装不上"成立** | 零依赖方案照做（见 §4 决策 1、2） |

### B. 会写出**编译不过/形状不对**的代码（接口口径，逐条按实测写）

| # | 计划口径 | 实测签名 | 后果 |
| --- | --- | --- | --- |
| **B1** | item 8 写"服务入口 `services::recovery::attention_overview`"（像是现成入口） | 它是**自由函数**：`recovery.rs:822 (db, expected_data_epoch, current_run_id)`；**没有 `AppState` 包装** | 命令体必须自己组装：`AppState::db()`（`:918` pub）+ `AppState::coordinator()`（`:926` pub）+ `Coordinator::run_id()`（`:198` pub）。**别去找 `AppState::attention_overview`** |
| **B2** | 计划 `:278` 让 1–5 类的命令传 `current_run_id` | `AppState::reconcile(env, req)` 已**在内部**取 run_id（`bootstrap.rs:1595-1598`）；`correct`/`backfill`/`discard_session`/`transition_task` 同形 | **包装不要再传 run_id**（照原文会多写一个参数） |
| **B3** | 表格写 `ReconcileRequest → ReconcileReport` | items 1–5 的真实返回是 **`WriteOutcome<T>`**（`storage/mod.rs:50`：`Changed(T)`/`Unchanged(T)`） | 命令 DTO 层必须**解这一层**（`into_parts`），并且**只有 `Changed` 才广播** |
| **B4** | item 10 写"服务入口 = `services/backup.rs`"（像一行转发） | `backup_consistent(dir_override, conn, db_version, clock, stage, diagnostics) -> Result<PathBuf, AppError>`（`backup.rs:281`）——**6 个参数** | conn / db_version / clock / diagnostics 都要从 `AppState`/`RunningApp` 凑；**不是一行转发** |
| **B5** | item 13 `history_view` | **服务层也没有**：`services/history.rs` 只有 `correct():135`/`backfill():322` + DTO，**无列表/详情读函数**（`grep history_view src/ tests/` = 0） | 13 条里**唯一"新建服务"**的一条；计划 `:237/:306` 已如此登记（不算计划错误，但清单口径要读清） |
| **B6** | 文末「2026-10-10」要求"锁内冻结 revision" | **仍然必做**：`RestoreOutcome`（`backup.rs:400-418`）只有 `committed/data_epoch/run_id/recovery/migration_backup/rollback`，**没有 `revision`** | 见 §4 决策 3 |
| **B7** | "`restore` 是唯一不进 `run_command` 的命令" | **有机器判据**：`restore_from_backup` 在**持有 app 锁**时主动拒绝（`backup.rs:610`） | 照做即可，且这条可以被测试钉住 |

### C. 行号 / 定位漂移（照抄会指错，替换即可）

| 计划 | 今天 |
| --- | --- |
| `src/types/ipc.ts:191` `DailyPlanView`、`:202` `TimerSnapshot` | **`:192`** / **`:203`**（P6 在 `:93` 加了第六个错误码，整块下移 1 行） |
| `Cargo.toml` 依赖区间 `:20-:43` | `[dependencies]` `:20-:43` **+** `[target.'cfg(windows)'.dependencies]` `:45-:71`（P6 的 `windows-sys`） |
| `p7-acceptance.md` §5.2「表末行」那句 | 那条"归属已闭环"在 **`:578`**（表末行 `:579` 是"多入口/打包路径"那条）——**按语义读，定位改 `:578`** |
| 计划 I5 行要求改的前端两处（"五个稳定码"→六个） | **已完成**（`types/ipc.ts:86` 已是"六个"、`ipc.test.ts:117` 已是"六个码"） ⇒ 从 P8 清单**删除** |

### D. 逐字核过、可直接引用（不要重核）

`App.tsx:40/42/54/96/7`（`PageKey` 四项、`PAGES` 四项、`PageView` switch、默认 `inbox`、不引路由且 `react-router` 确实不在 `package.json`/`node_modules`）、
`ipc.ts:249/254/259/264`（`planFor`/`addToPlan`/`removeFromPlan`/`timerSnapshot`；**前三条除定义外零调用**，Today 是第一处消费方）、
`viewWatermark.ts:47/42/44`、`commandError.ts:24/32/44`、`duration.ts:9`、`hooks.ts:28/39/49/78`、
`App.test.tsx:126`（断言"导航四项 + 挂载时**恰好四条命令**"）`:157`、`fakeBackend.ts:174/105/83/163`、
`package.json:18`（`@tauri-apps/plugin-opener`，`src/` 里 0 调用）、`:20/:21`（`dockview`/`dockview-react` 8.3.1）、
`capabilities/default.json:1`、`paths.rs:15`（`app_data_dir()`）、`error_response.rs:84`（`DataEpochMismatch ⇒ requires_handshake`）、
`check-layers.ps1:156/157/158/167/171/178`（六条规则）、`commands/mod.rs:27-34/127/38-47`、`envelope.rs:45-48`。

**Task 4 的基线**：仓库根**没有** `scripts/`、`package.json` 里**没有** `check:bundle`；`dist/`（01:40，晚于最后一次前端提交 `34b7c6d`）里 `DockviewDemo` 与 `dockview` **0 命中** ⇒ 门禁的"应通过"基线成立。
`DockviewDemo` 的引用面只有 `App.tsx:22-23`（注释）+ 组件自身（`DockviewDemo.tsx:3-5,25`）。

---

## 2. 13 条命令的真实接缝（照这张表接线，别照计划表格的箭头想象）

| # | 命令 | 真实服务入口（按符号） | 特别提醒 |
| --- | --- | --- | --- |
| 1 | `reconcile` | `AppState::reconcile(env: WriteEnvelope, req: ReconcileRequest) -> Result<WriteOutcome<ReconcileReport>>`（`bootstrap.rs:1590`） | run_id 内部取；解 `WriteOutcome`；`ReconcileReport` **无 serde** ⇒ DTO 自己定 |
| 2 | `correct` | `AppState::correct(env, CorrectRequest) -> WriteOutcome<HistoryEditReport>`（`:1658`） | 只对 `finished` 开放（界面据 `state` 禁用） |
| 3 | `backfill` | `AppState::backfill(env, BackfillRequest) -> WriteOutcome<HistoryEditReport>`（`:1682`） | 独立入口，不启动计时 |
| 4 | `discard_session` | `AppState::discard_session(env, DiscardSessionRequest) -> WriteOutcome<HistoryEditReport>`（`:1714`） | 与 `reconcile(discard_uncertain)` **不许合并** |
| 5 | `transition_task` | `AppState::transition_task(env, TransitionTaskRequest) -> WriteOutcome<TaskTransitionReport>`（`:1743`） | 顺带启用 P7 暂缓的托盘「完成」占位项 |
| 6 | `accept_detected_clock_correction` | `AppState::accept_detected_clock_correction(&str) -> Result<ClockCorrectionAccepted, AppError>`（`:1761`） | 响应**自带** `data_epoch`/`revision`（`coordinator.rs:1477/1478`） |
| 7 | `retry_recovery` | `AppState::retry_recovery(&str) -> Result<TimerSnapshot, AppError>`（`:1805`） | 内部已 `guard_writable`；**不带 `WriteEnvelope`**；用户显式触发，不自动重试 |
| 8 | `attention_overview` | **自由函数** `services::recovery::attention_overview(db, expected_data_epoch, current_run_id)`（`recovery.rs:822`） | 见 B1：命令体自己组装三个 pub 访问器 |
| 9 | `export_data` | `AppState::export_json(:1394)` / `export_weekly_markdown(:1418)` → `ExportJson{text,data_epoch,revision}` / `ExportMarkdown{text,data_epoch,revision,week_start,week_end,range,timezone}` | 两者**无 serde**（正合计划 `:308`：只序列化自己的 `ExportResult`）；**落盘函数不存在**，由 P8 新增 |
| 10 | `backup` | `services::backup::backup_consistent(dir_override, conn, db_version, clock, stage, diagnostics) -> Result<PathBuf, AppError>`（`backup.rs:281`） | 见 B4：6 参自己凑；维护态被 `guard_writable` 拒 |
| 11 | `restore` | `services::backup::restore_from_backup(app: &SharedApp, broadcaster: &Broadcaster, backup: &Path, backup_dir: Option<&Path>, clock: &ClockSource) -> Result<RestoreOutcome, AppError>`（`:598`） | 持锁调用**主动拒绝**（`:610`）；`RestoreOutcome` 无 `revision`（B6）；时钟取 `RunningApp::clock_source()`（`:550`） |
| 12 | `stats_today` | `AppState::stats_today(&TodayQuery) -> Result<TodayView, AppError>`（`:1374`） | `TodayQuery` 有 `Deserialize`（`stats.rs:709`）、`TodayView` 有 `Serialize`（`:749`）且自带 `date/timezone/range/as_of/data_epoch/revision` ⇒ **可直接当 IPC DTO** |
| 13 | `history_view` | **入口缺**（服务层与 IPC 都要新写） | 见 B5：唯一"新建服务"的一条 |

其它已核实可用的：`AppState::stats_snapshot(:1356)`、`AppState::rescan_recovery(:1276)`、`RunningApp::clock_source(:550)`、`RunningApp::sampling_errors(:521)`。

---

## 3. 必须验证的 8 条（P8 的验收要点）

1. **13 条命令逐条接通且各有用例**（`tests/ipc_commands.rs` 的既有姿势：24 条命令体逐条覆盖 ⇒ 扩到 37 条）；每条都断言 `data_epoch`/`revision` 能供页面判旧。
2. **`restore` 不进 `run_command` 且持锁即拒**：接线错误（放进 `run_command` 的闭包）必须**红**，而不是死锁——今天有机器判据（`backup.rs:610`），用例要钉住它。
3. **恢复的三条硬约束**（`p6-acceptance.md` §8）：不进 `run_command`；时钟取自 `RunningApp::clock_source()`；界面能区分"正在恢复"与"恢复失败卡住"。
4. **`RestoreResult.revision` 在锁内冻结**（B6/决策 3）：不得换库放锁后再读一次拼上去；失败**不创建伪成功信封**。
5. **维护态对 UI 只有两个出口**：被拒时的 `DATA_RESTORE_IN_PROGRESS` + 维护结束后的 `data_epoch` 变化 ⇒ **不得假定存在维护态事件**；另一窗口只能靠"写被拒 ⇒ 按 `code` 禁用；握手成功 ⇒ 解禁"。
6. **Today 五项来自同一次查询**（同一 `as_of`/`revision`），人工与机器**不合并**、作废不进待确认；本视图水位（`viewWatermark`）判旧。
7. **导出数字与界面同源**：`export_data` 与 `stats_today` 用同一个 `services/stats.rs` 入口；落盘返回**真实存在的绝对路径**；能力未登记时**明确不可用**而不是静默失败。
8. **发布产物门禁**（Task 4）：`dist/` 里 `DockviewDemo`/`dockview` 命中即失败；**反向验证**（故意 import 一次 ⇒ 门禁必须红）；`.ps1` ASCII-only；`dist/` 从仓库根解析、找不到就报错退出。

**归属 Task 5（实机 / 人工）**：F-001…F-020 逐条人工验收、500 ppm **跨机器**校准（≥2 台机器，记机器/系统版本/提交号/实测漂移值）、P6 的 OS 事件到达延迟与行为、Windows 打包产物启动一次、托盘视图跳转、退出失败的可见提示。**这些不得用仓储/服务层测试顶替。**

---

## 4. 开工前要定的四个决策

1. **`opener` 的能力范围**：`opener:default` 今天同时给了 `allow-reveal-item-in-dir`（P8 需要）与 `allow-open-url`/`allow-default-urls`（`mailto:*`/`tel:*`/`http://*`/`https://*`，P8 不需要）。
   ⇒ 建议**保留插件、把 capability 收窄成 `opener:allow-reveal-item-in-dir`**（同时满足"能打开所在位置"与"不留未使用的出网能力"）。要动 `capabilities/default.json` 并触发 schema 重生成 ⇒ 归 Task 3 的"能力登记"一步。
2. **导出落盘方案复核**：计划裁决③的零依赖方案（Rust 命令写 `<app_data_dir>/exports/`，前端用既有 `revealItemInDir` 给位置）今天**仍然成立且无需改 `Cargo.toml`/`capabilities`**；唯一没验的是"缓存里有 `.crate` ⇔ 能离线编译"（本方案用不到，故不影响）。
3. **`RestoreOutcome` 的 `revision`**：在 P6 的提交/回滚**锁内**冻结并扩展返回材料（+测试），不要在放锁后用一次独立读拼上去。
4. **Task 4 的 release 探针**：`cargo check --offline --lib --release`（P7 的手工一次性证据，约 93 秒）**并入 `check-bundle.ps1` 调用链**，还是**显式登记"不并入"并写明理由**——二选一，不能悬空（计划 `:116` 的要求）。

---

## 5. 门禁与工作流

**Rust 侧**（沿用 P1–P6 的姿势）：改镜像 `worktrace-src/` → 显式清单 → `p3-apply.ps1` 落盘 → `git status --porcelain` 复核 → `p4-gate.ps1`
（fmt / `cargo test --offline` / clippy `-D warnings` / `check-layers.ps1` 六条）；提交前在**干净树**上跑，日志**前后各内嵌一次** HEAD/porcelain，并确认 `compile lines for worktrace: N ≥ 1`。
`sabotage` 日志头部要内嵌 HEAD + porcelain + 变异 diff，跑完按 blob 哈希逐字节还原。

**前端侧（P8 首次成为主力，今天有两处缺口要补）**：
- **落盘没有专用脚本**：前端镜像在 `worktrace-web/`（== 仓库根的前端面），既有 `p3-apply.ps1` 只认 `src-tauri/`。
  ⇒ P8 第一件事是**补一个显式清单式的 web 落盘脚本**（`p4b-apply-web.ps1` 之类；上一轮存量清理是临时手写的，没有沉淀），并要求落盘后 `git status --porcelain` **恰好**是清单里的文件。
- **前端门禁**：`pnpm test`（vitest run）与 `pnpm build`（含 `tsc`）——2026-10-10 那次八项门禁里都跑过（142 passed、build exit 0），但**它们不在 `p4-gate.ps1` 里** ⇒ P8 每次改前端都要单独跑并把输出留档。
- ⚠️ **工作树 CRLF 不止一个文件**（`commands/mod.rs`、`export.rs`、`stats.rs` 实测都是）⇒ 脚本改文件一律**按文件探测 EOL**（env-brief 第 7/8 条）。

**收尾**：本计划完成条件 = Task 1–5 全过 + 总纲 §6 的 18 项 + Task 5 的人工验收记录；
`manual_platform_verified` 在实机验收前保持 **false**。

---

## 6. 与 P6 的接口（实施 P8 时不要重新发明）

- 维护态、`Runtime` 装卸、访问器可失败（`db()`/`db_mut()`/`coordinator()` 缺运行态 ⇒ `DataRestoreInProgress`）都已就位；**新页面不要绕过它们**。
- 恢复的三段（`begin_restore` →（锁外）`prepare_and_swap` → `commit_restore`/`abort_restore`）**必须留在一次调用体内**——`restore_from_backup` 就是那个一次调用。
- 诊断落点：`platform::diagnostics`（`startup.failed`/`restore.failed`/`backup.prune_failed`/`restore.finished`/`maintenance.begin`/`maintenance.end`…）⇒ 界面的"失败原因/诊断位置"直接从这些记录取，不要另造一套。
- `services/tx.rs::write_tx` 是 **DEFERRED + 先读后写**：**引入第二写连接前必须先改它**（今天单写者模型不变）。
