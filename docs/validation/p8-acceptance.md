# P8 验收记录：统计、恢复界面与导出（**骨架，待填**）

日期：____（待填）。范围：`0d4faaf` 及之前 P8 的全部提交（`68979e3..0d4faaf`，见 §1.3；**本任务
（Task 7）的改动未提交**，提交号由控制器补）。依据：
[P8 计划](../superpowers/plans/2026-10-03-p8-stats-recovery-export-ui.md)（含 fix round 2/3/4 与
文末「P8 新增的 IPC 命令（13 条）」）、[总纲 §6](../superpowers/plans/2026-10-03-v01-plan-index.md)
的 V0.1 完成定义；开工交接见 [pre-p8-closure](pre-p8-closure.md)。

> **这是一份"待填"的记录，不是结论。** §2 的每一行都留了**观察**与**结论**两列，
> 由验收人在实机上跑完 `src-tauri/tests/manual-v01.md` 之后逐条填写。
> **本文档当前不含任何实测结论**，也**不得**据它宣称 V0.1 可发布。
>
> **`manual_platform_verified` 保持 `false`**，直到 `manual-v01.md` 的 §0–§13 全部跑完并填了
> 现象、§12 的 500 ppm 跨机器校准在 ≥2 台机器上跑完。
>
> **自动化那半边的证据在**：`p4-gate.ps1`（Rust：fmt / test / clippy / 分层）与
> `p7t6b-web-gate.ps1`（前端：`pnpm test` + `pnpm build`）的原始输出，以及
> `.superpowers/sdd/2026-10-03-p8-stats-recovery-export-ui/task-*-report.md` 逐任务的证据段。

---

## 1. 交付清单

### 1.1 P8 新增的 IPC 命令（13 条）

**登记口径**（计划 fix round 2 的 I3 / fix round 4 的 I-3）：P3 只交付服务层与 `AppState`
入口，下面 13 条的 IPC 包装与前端转发**全部由 P8 新增**。全部走 `run_command`（同一把
`Mutex<AppState>`、维护态拦截、失败经 `capture_error_response` 带权威 `epoch`/`revision`），
**唯一例外是第 11 条 `restore`**（三段流程，见计划「执行骨架」）。

| # | 命令 | 请求 → 响应 | 口径 |
| --- | --- | --- | --- |
| 1 | `reconcile` | `ReconcileRequest` → `ReconcileReport` | 写；`Confirm` 与 `DiscardUncertain` 两个动作，界面上也不合并 |
| 2 | `correct` | `CorrectRequest` → `HistoryEditReport` | 写；**只对 `finished` 开放** |
| 3 | `backfill` | `BackfillRequest` → `HistoryEditReport` | 写；独立入口，不启动计时、不伪造完成事件 |
| 4 | `discard_session` | `DiscardSessionRequest` → `HistoryEditReport` | 写；单独入口 + 二次确认 |
| 5 | `transition_task` | `TransitionTaskRequest` → `TaskTransitionReport` | 写；收件箱跃迁入口 + 托盘「完成」 |
| 6 | `accept_detected_clock_correction` | `AcceptClockCorrectionRequest` → `ClockCorrectionAccepted` | 写；显式接受，不自动 |
| 7 | `retry_recovery` | epoch 请求 → `TimerSnapshot` | 可能提交恢复事务、也可能无变化；**不带 `WriteEnvelope`**；不自动定时重试 |
| 8 | `attention_overview` | epoch 请求 → `AttentionOverview` | **只读**；恢复页与"待确认"的唯一数据源 |
| 9 | `export_data` | `ExportRequest` → `ExportResult { path, bytes, data_epoch, revision }` | 生成 P5 的内容 → 落盘 `<app_data_dir>/exports/` → 返回真实绝对路径 |
| 10 | `backup` | `BackupRequest` → `BackupResult { path, bytes, data_epoch, revision }` | 写（产生库副本，**不改业务事实**、不加 revision、不广播） |
| 11 | `restore` | `RestoreRequest { backup_path, expected_data_epoch, confirmed }` → `RestoreResult` | 写、**全项目唯一的危险操作**；`confirmed` 必须是 `true`；唯一不进 `run_command` 单临界区形状的命令 |
| 12 | `stats_today` | `TodayQuery` → `TodayView` | 只读；一次响应含 F-010 的五项 + `data_epoch`/`revision`/`as_of`/范围/时区 |
| 13 | `history_view` | `HistoryQuery` → `HistoryView { data_epoch, revision, sessions, selected }` | 只读；常规历史（finished/discarded），不依赖 `attention_overview` |

**业务命令总数：P8 之前 24 → 之后 37**（含 P8 的 13 条；口径见 Ruling P8-6 与 Task 6 的计数改口）。

### 1.2 八块页面（外壳是固定布局 + 一次 `useState` 切页，**不引路由**）

| # | 页 | 承载的 F-ID | 关键入口 |
| --- | --- | --- | --- |
| 1 | 收件箱 | F-001 / F-002 / F-003 | 捕获输入框、置为 Ready、开始、完成/取消/阻塞/等待/重开 |
| 2 | 项目 | F-004 / F-005 | 创建/改名/归档、项目详情（含终结态任务） |
| 3 | 任务 | F-004 / F-005 / F-006 | 三个列表（Ready/Waiting/Blocked）、情境筛选 |
| 4 | 计时 | F-006 / F-007 | 开始/暂停/继续/结束、当前任务标题 |
| 5 | 今日（P8 Task 1） | F-010 | 五项 + 今日选择列表增删（`add_to_plan`/`remove_from_plan`） |
| 6 | 恢复（P8 Task 2c） | F-015 | 确认 / 丢弃不确定区间 / 作废整次 + 重试恢复 + 接受时钟校正 |
| 7 | 历史（P8 Task 2c） | F-017 | 会话详情、修正起止、删除误记（软删）、补录 |
| 8 | 数据（P8 Task 3b） | F-018 / F-019 | 导出（JSON/周回顾）、备份、恢复（二次确认）、`revealItemInDir` |

**默认页仍是收件箱**（不是今日页）——`App.test.tsx` 的"挂载时恰好四条命令"是它的哨兵。

### 1.3 提交号（`dev` 分支；本任务改动**未提交**，由控制器补）

| 提交 | 内容 |
| --- | --- |
| `68979e3` | P8 开工前勘察（`pre-p8-closure.md`：假设订正 / 13 条命令真实签名 / 8 条必验 / 4 个决策） |
| `cc383b3` | Task 1：Today 页与 `stats_today` IPC（契约三处联动 + 第 5 个导航项） |
| `a33aff0` | Task 2a：恢复与历史修正的 5 条写命令 + 快照契约 |
| `b79652d` | Task 2b：恢复读取/重试 3 条命令 + `history_view` 新服务（**13 条命令齐**） |
| `309d854` | Task 2c：恢复页与历史页（F-015/F-017）+ 外壳接线（M8） |
| `6e1ebed` | Task 3a：导出 / 备份 / 恢复三条命令 + opener 能力收窄（业务命令 37 条） |
| `10cd0fc` | Task 3b：Data 页（F-018 导出与 F-019 备份/恢复） |
| `49d3e6a` | Task 2d：`transition_task` 的两个消费方（收件箱跃迁入口 + 托盘「完成」） |
| `86faf74` | Task 4：发布产物门禁（R-04） |
| `0d4faaf` | Task 6：收口批次（拆 `commands/mod.rs` + 计数改口 + flaky 用例 + 三处小加固） |
| **（待补）** | **Task 7：托盘视图跳转 + `manual-v01.md` + 本文档**（未提交；含 **fix round 1**：2 Critical + 4 Important + 6 Minor 的修复，其中 Critical-1 是前端聚焦行为真缺陷、Critical-2 与 Important-2/3 是**登记**） |

### 1.4 门禁与产物

| 项 | 命令 | 判定 | 观察（待填） |
| --- | --- | --- | --- |
| Rust 门禁 | `p4-gate.ps1`（fmt / test / clippy / 分层） | 全绿；**日志里 `compile lines for worktrace` ≥ 1**（否则可能跑的是旧二进制） | |
| Rust 测试数 | 同上 | **实施者期望值**：Task 6 收口后 783、Task 7 后 **786**（+3 条 `shell_lifecycle` 用例）——**待验收人复跑确认** | |
| 前端门禁（**官方判定**） | `p7t6b-web-gate.ps1`（`pnpm test` + `pnpm build`） | `test EXIT = 0`、`build EXIT = 0` | |
| 前端测试数 | 同上 | **实施者期望值**：Task 6 收口后 18 files / 199 tests、Task 7（含 fix round 1）后 **19 files / 219 tests**——**待验收人复跑确认** | |
| 发布产物门禁 | `pnpm check:bundle` | 退出码 0；**`dist/` 缺失必须非零退出** | |
| 安装包一次启动 | 见 `manual-v01.md` §13.2 | 打包产物能起来并跑通主流程 | |
| 契约三处联动 | `tests/ipc_snapshots.rs` + `src/types/__snapshots__/*.json` + `snapshot-contract.test.ts` | 快照逐字节一致；新快照必须登记 | |
| 跨语言常量 | `event-constants.test.ts`（读 Rust 源码） | 频道名 / 两个广播事件名 / **托盘视图事件名**两侧一致 | |
| 分层 | `src-tauri/scripts/check-layers.ps1` | 六条全过 | |

> ⚠️ `event-constants.test.ts` 用 `?raw` 读 `src-tauri/`，**WSL 镜像里必红**（没有那一级目录）
> ⇒ 它只在仓库侧跑得起来；前端门禁的官方判定永远是 `p7t6b-web-gate.ps1`。

---

## 2. 逐条验收判据（观察与结论**待填**）

> 步骤与"怎么算通过 / 不通过"在 **`src-tauri/tests/manual-v01.md`**（本文档只列判据与留空位）。
> 自动化已覆盖的那些，观察列填**测试名 + 数字**即可，不必重复手跑。

### 2.1 F-ID 逐条（18 项）

| F-ID | 判据（一句话） | 自动化半边（已交付） | 观察（待填） | 结论（待填） |
| --- | --- | --- | --- | --- |
| F-001 | 回车立即出现、空标题拒绝 | `Inbox.test.tsx`（展示与转发） | | |
| F-002 | 理清为 Ready；`start` 原子理清并启动（**一条**命令） | `Inbox.test.tsx`、事务用例 | | |
| F-003 | 状态机全表的入口开合；非法跃迁被服务拒绝且文案正确 | `Inbox.test.tsx`、`tests/*` 跃迁用例 | | |
| F-004 | 归档项目不出现在选择列表 | `Projects.test.tsx`、`tests/projects.rs` | | |
| F-005 | 四类标签；Context 用于任务筛选 | `Tasks.test.tsx`、`tests/tags.rs` | | |
| F-006 | 暂停值冻结、恢复预算不变、到点只提示 | `Timer.test.tsx`、`tests/timer_*.rs` | | |
| F-007 | 模拟暂停 30 分钟不计人工；改时/休眠事件注入 | `tests/system_events.rs`、`tests/timer_clock.rs` | | |
| F-008 | 同一 session 暂停恢复、结束后新 session；区间是唯一依据 | `tests/task_session_atomicity.rs` 等 | | |
| F-009 | 关窗后计时继续（**判据在 `interval_checkpoint`，不在界面秒数**） | `shell_lifecycle.rs` 的决策半边 | | |
| F-010 | Today 五项与库明细一致；跨日 `23:50–00:10` 两天各 10 分钟 | `tests/today.rs`、`tests/stats_end_to_end.rs`、`Today.test.tsx` | | |
| F-011 | 关掉所有窗口后托盘仍可用；五项全部可点 | `shell_lifecycle.rs` 的表半边 | | |
| F-014 | 拔网线跑完整功能，无报错、无降级提示 | `tests/offline_and_recovery.rs` 的离线口径 | | |
| F-015 | 强杀后 10 秒内重启，四类判定各一条 | `tests/recovery_scan.rs`、`recovery_end_to_end.rs`、`Recovery.test.tsx` | | |
| F-016 | 第二次启动不重复初始化、唤起既有主窗 | `shell_lifecycle.rs`、`single_instance` 用例 | | |
| F-017 | 负区间/人工重叠被拒；修正后 Today 与导出一致 | `tests/correct.rs`、`backfill_discard.rs`、`History.test.tsx` | | |
| F-018 | 导出内容与界面一致；周总结含量与逐项 | `tests/export_json.rs`、`export_markdown.rs`、`Data.test.tsx` | | |
| F-019 | 备份 → 恢复 → 旧请求被拒 | `tests/backup_restore.rs`、`maintenance_isolation.rs`、`Data.test.tsx` | | |
| F-020 | 乱序/丢通知收敛；可见窗口 ≤30 秒校验 | `tests/event_protocol.rs`、`revision_gate_vectors.rs`、`dualContextSync.test.ts` | | |

### 2.2 本阶段新增功能（P8 的 13 条命令 / 8 块页面）

| 项 | 判据 | 观察（待填） | 结论（待填） |
| --- | --- | --- | --- |
| Today 页 + `stats_today` | 五项一次查询同水位；机器/等待分列；作废不算待确认；空数据显 0 | | |
| 恢复页三条动作 | 确认（重叠给具体冲突）/ 丢弃不确定 / 作废整次 —— 三个入口**不合并** | | |
| 恢复的二次确认 | 点开确认框**零命令**；取消 ⇒ 零命令零报错 | | |
| 维护态提示 | 恢复期间界面显示"正在恢复"、写入入口禁用、**退出被拒**（`tray.quit.refused`） | | |
| 历史页 `correct` | 只对 `finished` 开放；成功后 Today 与导出跟着变 | | |
| 历史页 `backfill` | 不启动计时、不伪造完成事件；负区间/重叠被拒 | | |
| Data 页导出落盘 | 返回真实绝对路径 + 字节数；**不推进 revision**、不广播 | | |
| `revealItemInDir` **真能弹出资源管理器** | 点「打开所在位置」⇒ 真的打开并选中文件（Tauri 2 无 ACL 查询接口，**只能实机验**） | | |
| 托盘「完成」 | 计时中 ⇒ `Done` + 会话 `finished` + `revision` 恰好 +1；暂停中 ⇒ 零写入 | | |
| **托盘视图跳转**（P8 Task 7） | 窗口**已存在**（可见 / 最小化 / 被遮挡）时：「当前任务」⇒ **切到计时视图**；「快速捕获」⇒ **切到收件箱 + 捕获输入框拿到焦点**（**人已经在收件箱**时也要聚焦）；两者都**不丢抬窗**。窗口**被关掉后重建**那一支**不跳转**（§3 第 26 条，已登记） | | |
| `RECOVERY_REQUIRED` 切页（M8） | 失败 ⇒ **切到恢复页**并带上 Rust 的原文（不是只弹提示） | | |
| P7 归 P8 第 2 条 | **Windows 打包产物启动一次**（安装包或 `target\release\worktrace.exe`） | | |
| P7 归 P8 第 3 条 | 退出事务失败的**用户可见提示** —— 见 §3 的登记（当前**没有**界面出口） | | |
| P6 的 OS 事件 | 锁屏/休眠/唤醒/改时的**到达延迟与行为**（`manual-v01.md` §11 的表） | | |
| 500 ppm 跨机器校准 | **≥2 台机器**，五类场景，逐条记实测漂移与界值结论 | | |

### 2.3 完成门槛（总纲 §6 的 5 条）

| # | 条 | 判据 | 观察（待填） | 结论（待填） |
| --- | --- | --- | --- | --- |
| 1 | 18 项 F-ID 逐条通过，人工验收记录可复现 | `manual-v01.md` 填满 | | |
| 2 | 依赖预先安装后测试与构建通过；断网跑全部功能 | `p4-gate.ps1` + `p7t6b-web-gate.ps1` + `manual-v01.md` §9 | | |
| 3 | 强杀 → 10 秒内重启 → 四类恢复判定走通 | `manual-v01.md` §5.1 | | |
| 4 | 一次"备份 → 恢复 → 旧请求被拒"完整演练 | `manual-v01.md` §6 | | |
| 5 | `DockviewDemo` 不在发布产物里 | `pnpm check:bundle` = 0 | | |

---

## 3. 已知边界与登记项（**未修 / 已登记**；供验收人判断，不替你下结论）

> **这一节不是全量清单**（fix round 1 / Minor-6）：它只收 **能追到出处** 的条目——P8 台账
> `.superpowers/sdd/2026-10-03-p8-stats-recovery-export-ui/progress.md` 里逐条登记的
> deferred minor 与逐任务评审结论。台账里还有若干"评审 out-of-scope 备注"与更早的观察
> **没有**搬进来（多数是"用例自身强度"一类问题，不是产品行为）。要全量一览，请查台账原文。
> 「未修」= P8 明确不做；「已登记」= 有意留着、有代价说明。**它们都不是"未知的 bug"**：
> 每一条都能在代码或台账里找到出处。

| # | 项 | 影响面 | 状态 |
| --- | --- | --- | --- |
| 1 | Today 页的候选列表（`list_tasks`）**完全不参与判旧**：同 epoch 两次 load 交叠时，迟到的 `list_tasks` 可覆盖更新的候选 | 只影响辅助下拉 | 已登记（Task 1，评审 Minor） |
| 2 | 候选列表失败**完全静默**："读失败导致空"与"本来没有候选"不可区分、无重拉入口 | 辅助读 | 已登记（Task 1） |
| 3 | `addToPlan`/`removeFromPlan` 的 `planFor` 零调用（`DailyPlanView`/`planFor` 留作别的消费方） | 无 | 已登记（Ruling P8-8/P8-9） |
| 4 | 命令动作词对读是**手抄自证**，挡不住 Rust↔TS 漂移；命令层有一处裸 `AppError::Domain { detail }` | 契约维护 | 已登记（Task 2a，不进修复轮） |
| 5 | `tests/ipc_requests.rs` 的请求样本 + "TS 请求形状机械检查"未做 | 契约维护 | 已登记（Ruling P8-21） |
| 6 | `correct` 的可选 `reason` **没有界面入口**（契约允许省略） | 体验 | 已登记（Task 2c） |
| 7 | 历史页**没有**维护态禁用态（只上屏 Rust 的"正在恢复数据…"），与恢复页的差异 | 一致性问题 | 已登记（Task 2c） |
| 8 | 补录的任务下拉用 `list_tasks(statuses: [])` 且失败静默；下拉只能"加载更多"、无后端关键词搜索 | 任务多时要多次点击 | 已登记（Ruling P8-32） |
| 9 | 翻页用 `offset`：**补录**一条起点更早的时间会让后续页位移 | 分页 | 已登记（V0.1 不做游标分页） |
| 10 | 命令 6 的 `now = app.now_ms()?` 在服务预检**之前**引入一个时钟失败点（时钟读不到 ⇒ `STORAGE_ERROR`，而服务那条路本会映射 `RECOVERY_REQUIRED`） | 故障路径，无测试覆盖 | 已登记（Task 2b triage） |
| 11 | 已作废会话的整句文案把同一条限制说了两遍，且"已丢弃/已作废"两个词指同一状态 | 文案 | 已登记（Task 2c 复审） |
| 12 | 失败残留的 `<名>.partial` **没有清理机制**（导出目录里能看到它） | 磁盘残留 | 已登记（Task 3a 观察） |
| 13 | `rename` 前**无 fsync**（断电语义下"不留半份"不完全成立） | 断电安全 | 已登记（Task 3a 观察） |
| 14 | 同毫秒、同形状的第二次导出拿到 `STORAGE_ERROR`（`artifact already exists`），**不静默覆盖**（有意） | 批量导出时要加序号 | 已登记（Ruling P8-33） |
| 15 | 能力未登记档是**事后锁定**（正确构建下不可达）；`revealError` 是**一次性闩锁**（路径级失败也会永久禁用） | 只影响"打开所在位置" | 已登记（Task 3b，7 Minor 之一） |
| 16 | `RestoreResult.data_epoch` 被丢弃：清空旧展示完全依赖 `rehandshake()` 真观察到新 epoch（已有 verify 在飞时直接返回 ⇒ **≤30 秒自愈的竞态**） | 恢复后旧数据可能短暂残留 | 已登记（Task 3b Minor-3） |
| 17 | `viewWatermark.isStale` 在"已上屏的是另一个 epoch"时返回 false ⇒ 换库瞬间在飞的旧响应可能覆盖新状态（Today/Recovery 同病） | 换库瞬间 | 已登记（Task 3b Minor-4） |
| 18 | 写失败文案可能被随后成功的样本重拉清掉（只影响 `requires_handshake` 那一档） | 提示 | 已登记（Task 3b Minor-5） |
| 19 | 一处用例注释声称的"改坏会红"不成立（夹具无首尾空白）；`App.test.tsx` 的计数断言用 `toBeGreaterThan` | 断言强度 | 已登记（Task 3b Minor-6/7） |
| 20 | **`Blocked`/`Waiting` 没有回 `Ready` 的出口**（需要第六条动作）；`reopen` 在真实查询下的落点要抽共享件 | **产品口径缺口** | 已登记（Ruling P8-34，明确不塞进 2d） |
| 21 | 退出事务失败**没有用户可见提示**（只有 `worktrace.log` 的 `event=tray.quit.failed` 与非零退出码）；维护态那一档是"进程留着 + 界面显示正在恢复" | **P8 计划 `:327` 已订正前提：P6 已实施 ⇒ Task 5 必须真的加这一步，不能记「未做」**。现状 = **未达成**：`manual-v01.md` §13.3 的结论按**发现**写「P6 的用户可见提示未交付」，交用户/终审裁决 | **未达成（V0.1 收尾项）**（P6 只落了诊断出口；P7 §6.3-9 登记，P8 只核对现象） |
| 22 | `CANDIDATE_STATUSES`（Today）与 `LISTED_STATUSES`（Inbox）是**两份字面量**；`formatMs` 对 0 显示 `0` 而计时页是 `00:00` | 两处口径将来可能分叉；两页观感不一致 | 已登记（Ruling P8-11/P8-13） |
| 23 | 前端 `event-constants.test.ts`（跨语言常量对读）只在**仓库侧**跑得起来（`?raw` 读 `src-tauri/`） | WSL 侧必红，主门禁不受影响 | 已登记（Ruling P8-3） |
| 24 | 托盘视图跳转引入了**第三条事件名**（`worktrace:tray-view`，**窗口作用域定向事件**，只发给主窗） | 它不在 `services/events.rs` 那对**业务广播**里（信封是业务事实的词表：`data_epoch` + `revision`），因此不违反 P6 的"维护态只有两个出口"；但"第三名字"这件事本身要由终审/用户确认 | **本轮新增，待确认** |
| 25 | 新加的托盘事件**没有 ACL 判据**（Tauri 2 无 ACL 查询接口） | 能力是否真可用只能靠实机（`manual-v01.md` §10.5） | 已登记 |
| 26 | **主窗被关掉后重建的那一次视图跳转丢失**：`emit_to` 发给尚不存在的窗口（零接收者、通常不报 `Err`），随后重建出来的窗口按默认页（收件箱）挂载 | **托盘视图跳转的判据要分情形**：窗口**已存在**（可见/最小化/被遮挡）⇒ 跳转成立；**关掉后重建** ⇒ 只保证"窗口回来 + 抬到前台"，**不跳转**。**原因与代价**：补发要"待办视图队列 + 页面加载后重放"（或 `on_page_load` / 新 IPC 命令）= **第二套投递机制**，与"前端没收到就丢弃、不留待办队列"的既有口径冲突 ⇒ **裁定不补发**（fix round 1，评审 Critical-2）。`manual-v01.md` §10.5 给了"怎么区分两种情形"的判据 | **已登记（本轮，有意不修）** |
| 27 | **原生最小化 ≠ `document.visibilityState === "hidden"`**（Windows/WebView2：2026-10-05 起就有实机观察：原生 `isMinimized()` 为 true 而页面仍 `visible`、周期校验照跑） | **最小化那一格不能当"隐藏窗口"场景通过**，必须单独登记（`manual-v01.md` §8 与 `manual-sync.md` §2.5 同一条口径） | 已登记（P7 实机观察，P8 照录） |
| 28 | **"原生窗口可见性适配"（2026-10-05 计划那条：监听原生最小化、恢复首屏校验、最小化期间停止业务轮询）本轮未实现**——代码里今天的判据仍只有 `document.visibilityState` | `manual-v01.md` §8 的"可见窗口每 30 秒校验"这一格**不得**被当成"隐藏/最小化路径已验"；最小化期间是否仍在轮询要照实记（很可能仍在轮询） | **未达成（V0.1 收尾项）**（fix round 1，评审 Important-2） |

**另有三条与本阶段无关、但验收人可能撞上的既有事实**（只说，不改）：

- `check-layers.ps1` / `check-pre-p3.ps1` 里有少量非 ASCII 注释且无 BOM（PS 5.1 按 ANSI 读会乱码，只是注释）。
- `tauri-plugin-opener` 曾"全仓零调用"，P8 Task 3b 起 `Data.tsx` 是它的第一处消费方（能力收窄为 `opener:allow-reveal-item-in-dir`）。
- 前端仍留着不参与发布产物的 `DockviewDemo` 与两个死组件（`FloatingInput`/`FloatingSelect`）——删文件要用户确认，P8 只做门禁（`check-bundle.ps1` 保证不进产物）。

---

## 4. 实机项（步骤都在 `src-tauri/tests/manual-v01.md`）

| 组 | `manual-v01.md` 的节 | 为什么自动化替代不了 |
| --- | --- | --- |
| F-001/F-002/F-003 真界面路径 | §2 | 真 IPC、真事务、真错误响应与真焦点 |
| 托盘与窗口生命周期 | §3（+ `manual-shell.md` §1/§2/§3） | 要有真实托盘图标、菜单与事件循环 |
| Today 五项与跨日 | §4 | 真日界（时区/夏令时）与真库明细的交叉核对 |
| 强杀/四类判定/确认·丢弃·作废 | §5 | 需要真的杀进程、真的重启 |
| 备份 → 恢复 → 旧请求被拒 | §6 | 需要真的换库与真实 `data_epoch` 变化 |
| 导出落盘与周回顾逐项核对 | §7 | 需要真的写文件、真的用外部工具重算 |
| 多窗口乱序/丢通知 | §8（+ `manual-sync.md`） | 需要两个真实 WebView 与真实广播时序；**原生最小化 ≠ hidden**，那一格单独登记（§3 第 27/28 条） |
| 断网跑全部功能 | §9 | 需要物理断网 |
| 本阶段新增功能逐条 | §10（含 **`revealItemInDir` 真弹出资源管理器**） | Tauri 2 无 ACL 查询接口，能力只能实机验 |
| P6 的 OS 事件到达延迟 | §11 | 需要真的锁屏/休眠/改时 |
| **500 ppm 跨机器校准** | §12 | 必须 **≥2 台机器**、多类场景、足够长的 elapsed |
| P7 归 P8 的三条 | §13 | 含**打包产物启动一次**与退出失败的可见性 |

跑完之后：把每一条的**观察**与**结论**填回本文档 §2，并在 §5 签字。

---

## 5. 结论（由验收人填）

- 日期 / 提交 / 机器（与 `manual-v01.md` §0 的表一致）：
- `manual_platform_verified`：**false**（当前）→ ____（只有实机跑完才允许改）
- 未通过项与复现步骤：
- 未覆盖 / 存疑：
- 验收人签字：
