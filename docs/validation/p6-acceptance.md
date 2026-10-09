# P6 验收记录：平台硬化（备份恢复、维护态隔离与故障路径）

日期：2026-10-09。范围：`9242915..4295cad`（`dev` 分支，**15 个提交 / 48 文件**；代码面 +11306 −773，终审修复波再 +1506 −41）。
依据：[P6 计划](../superpowers/plans/2026-10-03-p6-platform-closure.md)（含 4 轮 fix round 与文末「P6 开工前补正 G1–G11」）、
[总纲 §5 第 9 条](../superpowers/plans/2026-10-03-v01-plan-index.md)的权威清单；开工交接见 [pre-p6-closure](pre-p6-closure.md)。

> **本记录只证明自动化与服务级那一半。** P6 交付的是**平台层与服务层**（备份编排、维护态与隔离、采样看门狗与故障态检测、正式 OS 事件源、WAL 一致备份与恢复、第六个错误码）。
> **IPC 命令、界面呈现与"在真实机器上核对"归 P8**（P6 计划 2026-10-08 收口第四条）。未完成项在文末单列，**不得**据本文件宣称 V0.1 可发布。

## 1. 自动化门禁

| 项 | 结果 |
| --- | --- |
| `cargo test --offline` | **753 passed / 0 failed / 1 ignored**（P6 前 659；终审修复波后 746；P6 后存量清理 +7） |
| `cargo fmt --check` | 0 |
| `cargo clippy --all-targets --offline -- -D warnings` | 0 告警 |
| `scripts/check-layers.ps1` | **六条全 PASSED**（含 `platform/` 不得出现 `services|storage|commands`、`services/` 不得取系统时间、入口点不得自己 `Db::open`/`migrate(`/`run_repo::`） |
| 新增测试文件 | `tests/system_events.rs`（11+3）、`tests/fault_detection.rs`、`tests/backup_restore.rs`（16）、`tests/offline_and_recovery.rs`（2）、`tests/event_protocol.rs`（+6）、`tests/maintenance_isolation.rs`（10）等 |
| 冻结面 | **无新增 IPC 命令**（13 条新增命令归 P8）；**schema 未改**；前端只动 3 个文件（`types/ipc.ts` 码表 + 两处注释、`__tests__/ipc.test.ts` 一处用例名、`src/ipc.ts` 两处注释）；DTO 快照 fixture 零 diff |

`weight` 越界自查（沿用 P5 口径）：`services/` 下 4 处命中全是"不做按 `weight` 分配"的文案；全仓非注释仅 3 处（列定义、`INSERT … weight … VALUES(?1,?2,NULL)` 恒写 NULL、字符串字面量）⇒ **零按权重分配的实现**。

## 2. 交付内容（八个派单，各自都有独立评审 + 修复轮）

| 派单 | 提交 | 交付 |
| --- | --- | --- |
| Task 1 | `87c58b1` | 单实例/启动顺序硬化 + **按需迁移前一致备份**（`Db::open` → 读 `PRAGMA user_version` → 需迁移时 `VACUUM INTO` → `migrate`；**三条按需判据成对且按原因断言**；备份失败 ⇒ 拒绝迁移；保留最近 5 份） |
| Task 2a | `97ad8cf` | **维护态与隔离**（同一临界区、不加第二把锁；`sampling_action` 取锁后第一句整拍跳过；13 个拒绝点）+ 退出意图互斥 + `platform/diagnostics.rs` 正式诊断落点 |
| Task 2b | `0b26f34`+`c7536bc`+`7595f7d` | **采样失活看门狗**（方案②："`ticks` 停涨"，**不改 release profile**）+ **协调器故障态检测半边**（`timer_faulted` 复合语义、按跃迁记一条、启动点名） |
| Task 2c | `2627055` | **真 Win32 OS 事件源**（隐藏消息窗 + 锁屏/解锁、休眠/唤醒、系统改时）+ `SystemClock: Clone` + `AppState::system_boundary`（`system_pause` 唯一生产入口） |
| Task 3 | `0cff86c`+`36cf260` | **事件去重协议的故障路径**（只加测试：①③④ 各可失败、② 性质断言、F-020 收敛、乱序/迟到响应/订阅者异常） |
| Task 4a | `c948e64` | **第六个错误码 `DATA_RESTORE_IN_PROGRESS`** 四处联动 + `meta::rotate_epoch` + 备份原语搬到 `services/backup.rs` |
| Task 4b | `34b7c6d`+`8baf938`+`ff4cbc6` | **三个访问器改 `Result`**（696 处触点）+ `Runtime` 装卸 + **恢复三段流程**（不持锁的在途段、同事务 `start_run + rotate_epoch`、两步重扫、两条路径都不复用旧协调器） |
| Task 5 | `74a6f06` | 服务级端到端链路（启动→计时→关窗→重开→崩溃→重启→备份→恢复八步全确定值）+ F-014 离线口径 + 权威清单逐条 |

## 3. 总纲 §5 第 9 条权威清单：逐条核对

Task 5 的核对表把三份清单逐条给了结论（**21 个 `file.rs::用例名` 经评审全量核对 21/21 存在**）：

- **02 §8 的 M01/M05 必测案例（14 条）**：与 P6 相关的逐条对应——跨午夜含暂停、空范围、历史工时重叠、区间修正后报表重算、待确认排除与显式确认（继承 P3/P5），另加 P6 自己的：**强杀后重启走四类判定**、**维护态拒绝的四件事**、**旧 epoch 写入被拒且不写入**、**恢复后新 epoch/新 run**、**并发写不撕裂**。
- **04 §9 的必做集成用例（6 条）**：重复提交、统计修正、恢复/异常闭环等逐条对应。
- **06 §4 的实现前技术验证（6 项）**：**多数判"不适用"并写明载体**——DB 执行边界（P6 未改执行边界）、单调/墙钟映射（P6 的检测半边复用 P2 判据，未另写映射）、双窗口同步（实验载体在 P7/P8）、HUD/中文检索（V0.1b/V0.3+）。

## 4. 证据强度（本阶段的判据：改坏实现必须红）

每个派单都做了**在已提交干净树上重跑的定向变异**，日志头部内嵌 `git rev-parse HEAD` + `git status --porcelain`（多数另内嵌变异 diff）：
Task 1 两次（判据恒 false ⇒ 4 红；恒 true ⇒ 零产物红）、Task 2a 三次（判据挪到 `sample_tick` 后 / 第四条入口先取样本 / `shutdown` 不置退出意图）、Task 2b 六次（含"把归因改回旧序 ⇒ 两条 origin 用例红"与"删身份判据 ⇒ 15 条全绿暴露该判据无判别力后补用例"）、
Task 2c 两次（删两处维护态判据 ⇒ 2 红；`try_lock` 替排队等锁 ⇒ 3 红）、Task 3 六次（含 D3 把"锁中毒容忍"从恒真变成单条红）、Task 4a 两次、**Task 4b 八次**（含 S5 删造库判据 ⇒ 红在 `!exists()`、S6 空操作 `rollback_files` ⇒ 红、S7 commit 侧判据、S8 身份判据）、Task 5 三次（断 heartbeat / 恢复沿用旧 epoch / 去掉 `scan_at_startup`）。

## 5. 未做（登记归 P8，不得声称通过）

- **IPC 与界面**：13 条新增命令（含恢复、备份、Today、`history_view`）与全部页面；恢复入口的 `expected_data_epoch` 守卫。
- **实机链路**（P6 计划 2026-10-08 收口第四条明确归 P8）：真实拔网线跑 V0.1 功能；手动触发备份与恢复并核对 `data_epoch` 变化与旧请求被拒；界面拿旧请求被拒；强杀后重启核对单实例未重复初始化；**锁屏 30 分钟 / 休眠唤醒（含 Modern Standby 可能不发 `PBT_APMSUSPEND`）/ 正反改时的到达延迟与行为**；OS 级磁盘耗尽与 WAL 写失败；双窗口实机；HUD/安装包与 R-04 发布产物门禁。
- **发布收口**：`tauri-plugin-opener` 是脚手架遗留、**全仓零调用点**（4 处全是注册/配置），建议在 P8 的发布收口一并摘掉（零调用，摘掉会动 `Cargo.lock` 与 capabilities，属构建形态变更）。
- `manual_platform_verified` 保持 **false**。

## 6. 与 P8 的接口口径（交接）

- **恢复必须留在一次命令体内**：`restore_from_backup` 连起三段（`begin_restore` → `prepare_and_swap` → `commit_restore`/`abort_restore`），命令层**不得**把三段拆成多次 IPC；取消/超时语义要在此约束下设计。
- **时钟同源**：P8 新建协调器/事件源时必须用同一只 `SystemClock`（`Clone` 共享 `origin`），否则 R-02 的归属会静默落空。
- **回滚副本可发现**：`RestoreOutcome.rollback` 会给出 `<db>.restore-rollback` 路径（恢复前那个世界的唯一完整副本）；界面要能把它告诉用户，**不要**当垃圾清理。
- **`Today.current` 可能指向已结束会话**（`CurrentTask` 带 `state`）⇒ 界面必须按 `state` 分支，不能把"有 current"当成"正在计时"。
- **维护态对 UI 的出口只有两条**：写命令返回 `DATA_RESTORE_IN_PROGRESS`（按码显示"正在恢复"并禁用入口）、维护结束后的 `data_epoch` 变化走既有收敛路径。**没有第三个事件名**。
- **恢复期间不能退出进程**（托盘退出被拒）；界面要显示"正在恢复"并禁用退出入口。


## 9. P6 之后的存量清理（2026-10-10）

P6 收口后、P8 开工前，把 P5/P6 两阶段评审累积的存量问题一次清完（用户要求"先修复之前存在的问题"）：

- **`1bf6496`**（16 文件 +577/−81）：**A 真缺陷 7 条 + B 断言强度 5 条 + C 诊断文案 8 条 = 22 条**。要点：
  ① **保留策略不再可能删掉用户正在恢复的那份产物**（恢复的"旧版本先备份再迁移"分支复用用户备份目录）；
  ② **`error_contract.rs` 的"禁用英文片段"扫描改成只认字符串字面量**——原先 `contains` 会误伤任何 `task.title` 字段访问（当初为躲它把变量改名 `task_row`，现已还原）；
  ③ 导出周回顾在只有机器/等待候选时不再印"本周没有待确认记录。"；
  ④ `system_events` 两条 Windows 单测把"访问被拒"当环境不适用（其余失败仍红）；
  ⑤ 离线 needle 补 `tauri::http`/`std::process::Command`；
  ⑥ `Today` 空库用例钉住三类×全部 measure 列（`TodayView::column` 的 `expect` 若失效会在 P8 运行时 panic）；
  ⑦ `wal_concurrency` 的判别力声明按事实降级（并补一次撑窗变异证明断言不空）；
  ⑧ 若干诊断/文案（`tx::write_tx` 旁写明 DEFERRED + 先读后写 ⇒ 第二写者会让升级立刻 BUSY；看门狗诊断行补 `run_id` 等）。
- **`3deda6b` + `af6fd9e`**：该轮复审留下的 5 条 Minor 收口（扫描器的引号配对漏报模式 + 正控、`Drop` 里不再 `debug_assert!`（撞上展开中的栈会 abort 整个进程）、注释不再写会漂的行号、保护集合按规范路径比 + 不同拼写用例、诊断正控断到落点路径）。
- **`ee09bc6`**：保护键的**回退方向取"宽"**（能 canonicalize 的按规范路径比，拿不到的退到文件名比；旧写法退回原路径逐字节比会让用户选中那份重新可删）+ 一条能钉住回退分支的单测。

清理后的门禁：**753 passed / 0 failed / 1 ignored**、fmt/clippy `-D warnings`/分层六条全过。
**副产品（供后续避坑）**：Rust 的 `Path` 相等会消掉 `.` 段但**不消 `..`**；"在注释里写行号"必然漂移（评审两次抓到）；工作树里是 CRLF 的文件不止 `commands/mod.rs`（`export.rs`/`stats.rs` 也是，而提交 blob 仍是 LF）——一律按文件探测 EOL。

**仍留给 P8**（与 §8 一致）：实机验收、`Db::open_existing`、前端 `pnpm exec tsc --noEmit`、`tauri-plugin-opener` 摘除、`services/tx.rs::write_tx` 的事务模式（引入第二写者前必须先改）。

## 7. 终审与修复波

**全分支终审**（`9242915..74a6f06`，评分为 896 KB 的 diff 分五轮读完）：**With fixes**（0 Critical，3 Important，13 Minor）。
评审独立复核了镜像与工作树逐文件一致、**供审 diff 与真 `git diff -U10` 的增删行集合逐行相等**（12080 行双向 0 差异）、门禁算术、`Cargo.lock` 恰好 +1 行、`schema/migrations/db/events` 零 diff、`invoke_handler` 命令集合逐行恒等；
并枚举了**全部 12 个生产侧 `lock_app(` 点**与三条维护态判据的全部调用点，确认没有第四条漏网路径。

**唯一一次修复波**（`4295cad`，9 文件 +1506 −41，14 条新用例）：
- **I-1**：`RunningApp::{attach_clock, clock_source()}` 把同源时钟交给 P8（未挂时明确 Err，绝不新建兜底）；用例走真 `SystemClock` 断言恢复后锁屏边界被接受 ⇒ `paused`，**带负控**（另建一只钟 ⇒ `recovering`）。
- **I-2**：`startup.failed`（包住整个 `bootstrap::startup`，故**迁移前备份失败**也被记下）+ 两条恢复错误臂写 `restore.failed`（stage/rolled_back/code/回滚副本路径）⇒ release 无控制台也可诊断，"恢复失败卡住"与"正在恢复"从此可区分。
- **I-3**：`stats.rs` 两处文档改成"最后装载的会话、可能已结束"，并在 `tests/today.rs` 补断言钉住。
- M-2（锁重入防线从 release 失效的 `debug_assert!` 换成硬判据）、M-8（托盘退出拒绝事件名按分支取）、M-13（测试标题口径）、M-3（保留策略清理失败改走正式诊断；选的是"把 `Diagnostics` 传进去"）。
- **无主计划条目**（终审点名）：新增 `tests/wal_concurrency.rs` —— **第二连接 WAL 并发快照证据**（四读信封 lockstep `Δ计数 == Δrevision`；P2 提交后单一读事务），**两条真 RED** 证明断言非恒真。

修复波复审：**All findings addressed，无新 Critical/Important**，结论 **"P6 可以交给 P8"**。

## 8. 留给 P8 的清单（终审分诊的结论）

**硬约束（做错会静默降级或死锁）**：
1. **恢复命令不得走 `run_command`**：`restore_from_backup` 自己会取锁，放进 `run_command` 的闭包（那里正持锁）会在非重入 `Mutex` 上**死锁**。（修复波已把这条从 `debug_assert!` 升级为硬判据。）
2. **`ClockSource` 必须取自组合根的同源句柄**：各建一个 `SystemClock` 会让恢复之后的锁屏/休眠边界全部被拒（会话掉 `recovering` 而不是 `paused`，R-02 静默失效）。（修复波已提供组合根句柄。）
3. **界面必须能区分"正在恢复"与"恢复失败卡住"**：后者进程留在维护态、托盘拒绝退出，只能强杀；日志里现在两态可区分（修复波补了 `startup.failed`/`restore.failed`）。
4. **引入第二写者前必须先改 `services::tx::write_tx`**：它是 `DEFERRED` + 先读（`guard_epoch`）后写 ⇒ 任何第二写者会让升级**立刻 `SQLITE_BUSY`（不等 `busy_timeout`）**。生产今天只有一条写连接（`Db` 不实现 `Clone`、访问全过 `lock_app`）故不可达，但这条风险必须落在这里，别只留在测试注释里。

**要接线/要验的**：
- 13 条新增 IPC（含恢复、备份、Today、`history_view`）与全部页面；恢复入口的 `expected_data_epoch` 守卫。
- 实机：真实拔网线、手动备份/恢复并核对 `data_epoch` 与旧请求被拒、强杀后重启核对单实例、**锁屏 30 分钟 / 休眠唤醒 / 正反改时的到达延迟与行为**、OS 级磁盘耗尽与 WAL 写失败、双窗口实机、HUD/安装包与 R-04。
- 前端 `pnpm exec tsc --noEmit` 跑一次（P6 只改了注释与 `ERROR_CODES`，未跑前端门禁）。
- `tauri-plugin-opener` 零调用点 ⇒ 发布收口时摘掉（会动 `Cargo.lock` 与 capabilities）。
- 结构债（建议在 P8 动 storage 时一起做，别在 P6 收口轮扩大 diff）：`storage::Db::open_existing`（一次关掉 `require_library_in_place` 的逐入口判据与 `backup.rs` 的 copy→open 窗口）。

**已明确归属、不算遗漏**：OS 磁盘耗尽/WAL 写失败 → P8 实机；Task 5 的人工验收 → P8（`manual_platform_verified` 保持 false）；`platform/single_instance.rs` 未改（P7 既有用例已覆盖，G10 明令不要重写）。
