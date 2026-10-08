# P6 执行前收口与开工交接

日期：2026-10-08。开工基线：**`dev` = `origin/dev` = `9242915`**（工作树干净、镜像 `differing=0`）。
本文是 P6 的开工交接；接口补正在 [P6 计划文末「P6 开工前补正」](../superpowers/plans/2026-10-03-p6-platform-closure.md)（G1–G11），
实施期的逐条裁决与证据在 `.superpowers/sdd/2026-10-03-p6-platform-closure/`（工作区产物，不入仓库）。

## 交付边界

**P6 交付**：单实例/启动顺序的**故障路径**与**按需迁移前一致备份的编排**（Task 1）、周期采样的故障路径与**维护态隔离** + 采样失活看门狗 + 协调器故障态的检测半边 + **正式 OS 事件源**（Task 2）、
事件去重协议的故障路径（Task 3）、**WAL 一致备份与恢复（新 `data_epoch`）** + `DATA_RESTORE_IN_PROGRESS` 新码 + 三个访问器改 `Result`（Task 4）、本地优先与端到端（Task 5）。
**不交付**：界面与 IPC 接线（P8）、完整平台/实机验收（P8）、V0.2 的加权与层级。

## 已核实的关键接缝（现场 `grep`，不是照计划假设）

| 要用的东西 | 真实位置 / 签名 | 状态 |
| --- | --- | --- |
| 启动顺序与探针 | `StartupStep`（`services/bootstrap.rs:93`，9 变体）+ `ALL`（`:115`）；顺序断言在 `tests/startup_order.rs:123-136`（**8 个字面量**）+ 另用例四重不变量（`:225-328`） | 已交付，**别重写也别"修正"成 9 条**（G10） |
| 备份插入点 | `Db::open`（`:1080`）与 `migrate`（`:1083`）之间；读版本用 `migrations::current_version`（`storage/migrations.rs:29`，读 `PRAGMA user_version`） | 今天那里**没有** `user_version` 读取，要新增 |
| 正式恢复扫描 | **已在 `startup` 第④步**：`recovery::scan_at_startup(&mut db, &run_id, now)`（`bootstrap.rs:1123`，`services/recovery.rs:196`） | 已交付（G1）——Task 1 不要重做 |
| 周期采样 | `sampling_action`（`:1177`）→ `lock_app`（`:1178`）→ `sample_tick`（`:677`，= heartbeat + tick，**永不调 `system_pause`**）；节拍 `DEFAULT_SAMPLING_INTERVAL_MS = 1_000`（`:80`） | 已交付；维护态判据加在**取锁之后第一句** |
| `Scheduler` | `spawn:71` / `ticks:112` / `stop:126`（**不可逆**）/ `Drop:147`（**无条件 `stop()`、不查 `holds_app_lock`**）；**无 pause/resume** | 已交付；**装卸运行态的自死锁陷阱见 G11** |
| 单锁边界 | `AppBoundary:381` / `AppGuard:390` / `lock_app:419` / `holds_app_lock:432`；`AppState:354` 四字段私有 | 已交付 |
| 故障态 | `Coordinator::{faulted:172, refuse_if_faulted:345, is_faulted:352, retry_recovery:1333, system_pause:1364}`；`is_faulted` 生产 0 调用、测试 32 处 | 已交付；**语义比字段宽，见 G3** |
| 长间隔阈值 | `services/timer/anchor.rs:236`：`expected_interval_ms > 0 && d_mono > expected_interval_ms * 3 ⇒ Suspended` | 逐字准确（维护窗口的补偿靠它 + 新 run，不另写规则） |
| 事件去重 | `RevisionGate::on_notification:397`（先②`:403` 后③`:406`）；`apply_snapshot:357-377` 恒保证 `seen_revision >= applied_revision` | 已交付；② 不可被黑盒杀掉是**性质**，不是缺口 |
| 广播 | `Broadcaster:168` / `EventEnvelope:52` / `EventSink:113`；**只有两个事件名**（`:46`/`:48`）；前端另有一份 `worktrace-web/src/types/ipc.ts:97-98`，无机械一致性检查 | 已交付；**不新增事件名** |
| 托盘（绕过 `run_command` 的两条） | `spawn_tray_pause:1288`（自己 `lock_app:1295` → `tray_pause_impl:1240`）、`spawn_tray_quit:1317`（不取锁 → `tray_quit_impl:1271` = `shutdown:1272`） | **`guard_writable` 挡不住它们**，各自判维护态/退出意图 |
| 元数据 | `meta::{read_meta:21, require_meta:37, init_meta:47, bump_revision:64}`；**无 `rotate_epoch`**；`app_meta.singleton` PK（`schema_v1.rs:23`） | `rotate_epoch` 由 P6 新增 |
| 错误码 | `error.rs`：`AppError:11` 五变体、`code():39` 五码、`message():50`、码表用例 `:109`、中文用例 `:161` | **`DATA_RESTORE_IN_PROGRESS` 全仓 0 命中**，P6 新增第 6 个 |
| 前端联动点 | `types/ipc.ts:86/87-93/94`、`__tests__/ipc.test.ts:117`、`types/__tests__/snapshot-contract.test.ts:291`、`components/commandError.ts:31`、`services/error_response.rs:84` | 行号全准；P6 只动前两个文件 |
| 备份原语 | `VACUUM INTO` **0 命中**；`Cargo.toml:34` 仅 `["bundled"]`；bundled SQLite 版本**未复核**（勘察未跑 cargo） | Task 1 首跑时顺手用一条用例确认 `VACUUM INTO` 可用，别把"3.53.2"当既成事实 |

## P6 必须验证的清单（开工登记，收尾逐行给结果）

1. **按需迁移前备份的三条分支必须成对且按原因断言**：需迁移 ⇒ 确实写出一份可独立打开/通过完整性与版本校验的产物；`user_version == SCHEMA_VERSION` ⇒ `backups/` **零新增**且 `migrate` 是空操作；库文件不存在（首启）⇒ 走"没有可备份的事实"分支（**注意新库 `user_version == 0` 属"需要迁移"**）；需迁移但备份失败 ⇒ `migrate` 零调用、`code() == STORAGE_ERROR`、不建 `application_run`、不开窗口。**判据读 `user_version`，不是 `meta::read_meta`**。
2. **备份目录可注入**（G6）：测试不得写进真实 `%APPDATA%`；"需迁移有产物 / 无需迁移零产物"两条断言在同一测试文件里且互不污染。
3. **维护态**：置位/清位在同一把锁内；期间**采样整拍跳过**（不 heartbeat、不写 checkpoint、不广播、`sampling_errors` 不涨而 `ticks` 照涨）；写命令返回 `DATA_RESTORE_IN_PROGRESS` 且"四件事"成立（`revision` 不变、无新行、无审计、既有记录字段一致）；**统计/导出四个 `&mut self` 入口必须在采样之前就被挡住**（它们也会经 P2 异常事务写库）；托盘两条路径各自判维护态；维护态与退出意图互斥。
4. **故障态检测半边**：进入/清除**各记一条**（不是每拍一条）；`sampling_errors` 口径不变；`timer_faulted()` 照实反映复合语义；**P6 不自动重试、不自动清故障**（清除只走 P3 的 `retry_recovery`）。
5. **采样失活**：按 G2 实现"`ticks` 停涨"的看门狗，并有一条注入必 panic `on_tick` 的可失败用例。
6. **OS 事件**：`platform/system_events.rs` 只做适配（登记进 `platform/mod.rs`，满足 platform 分层规则）；处理路径经 `lock_app` → `AppState::system_boundary` → `Coordinator::system_pause`（**与 `sampling_action` 并列，不是同一条**）；事件到达时取不到锁要**排队后按当时状态重判**，不得丢弃或 panic；维护态判据与采样同一处。
7. **恢复**：临时路径验证 → 同目录可回滚切换 → 新 `data_epoch` + 新 `run_id`（**不沿用备份里的 epoch**）→ 两步重扫（先 `scan_at_startup` 归一，再 `scan_recovery` 门禁）→ 新协调器 + 锚点 → 装卸运行态 → 广播新 epoch；**两条路径都不得复用维护前的 `Coordinator`**；失败回滚重开原库并重建；旧 epoch 的写请求一律 `DATA_EPOCH_MISMATCH` 且不写入。
8. **本地优先/离线**：断网跑一遍 V0.1 功能无报错无降级；端到端链路（启动→计时→关窗→重开→崩溃→重启→备份→恢复）每一步可观测状态都断言。
9. **分层六条全过**（含新模块）：`platform/system_events.rs` 不得出现 `services|storage|commands`；`services/backup.rs` 不得取系统时间、不得引用 `commands`；入口点规则不许 `lib.rs` 自己 `Db::open`/`migrate(`/`run_repo::`。
10. **不能宣称的**：完整平台验收、真实拔网线/锁屏/休眠/改时的实机结论、安装包与多 DPI —— 全部归 P8；`manual_platform_verified` 保持 **false**。

## 门禁与工作流（同 P5，一句速查）

- 门禁：`src-tauri/scripts/check-pre-p3.ps1`（通用八项，名字历史遗留）；迭代用 `.dsh_tmp/p4-gate.ps1`（会 `cargo fmt` 并回拉镜像 ⇒ **先落盘再跑**）。
- 落盘：显式清单 `.dsh_tmp/p3-apply.ps1 -FilesFile …`；落盘后**必须** `git status --porcelain` 复核"恰好是清单里的文件"；每次红/绿都要看到 `Compiling worktrace`（或门禁打印的 `compile lines for worktrace: N ≥ 1`）。
- 提交：`.dsh_tmp/p4-commit.ps1 -MsgFile … -PathsFile …`（显式路径）；推送用 `p3-push.ps1`（用户授权后）。
- 进程：`subagent-driven-development`（每任务：实施 → 独立评审 → 修复 → 定向复核；终审后只一次修复波次）。
