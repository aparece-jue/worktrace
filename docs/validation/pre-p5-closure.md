# P5 执行前收口与开工交接

日期：2026-10-05。开工基线：**`dev` = `origin/dev` = `a9ba04a`**（工作树干净、三处镜像 `differing=0`）。
本文是 P5 的开工交接清单；计划的接口补正在 [P5 计划文末「P5 开工前补正」](../superpowers/plans/2026-10-03-p5-stats-and-export.md)，
实施期的逐条裁决与证据在 `.superpowers/sdd/2026-10-03-p5-stats-and-export/`（工作区产物，不入仓库）。

## 开工与交付边界

**P5 交付服务层**：`services/stats.rs`（口径、范围报表、Today 聚合）与 `services/export.rs`（JSON 明细、Markdown 周回顾），
加对应的 5 个集成测试文件与 `AppState` 瘦包装。**不交付**：IPC 命令与界面（P8）、导出落盘与路径（P8）、
加权统计与 Knowledge 层级（V0.2）、能力画像/KPA/`report_snapshot`（V0.4/V0.5）。

**开工门禁已跑**：`src-tauri/scripts/check-pre-p3.ps1`（通用八项，名字历史遗留）
八项全部退出 0、Rust **599 passed / 0 failed / 1 ignored**、前端 142。
证据：`C:\Users\lenovo\AppData\Local\Temp\worktrace-pre-p3-20261005-185046`。

## P5 消费面：真实签名（现场 grep 核对，不是照计划假设）

| 要用的东西 | 真实位置与签名 | 状态 |
| --- | --- | --- |
| 运行区间终点（**只能用这一处**） | `Coordinator::stats_sample(&mut self, db: &mut Db) -> Result<StatsSample, AppError>`，`StatsSample{run_id, session_id, session_version, open_interval_id, attributed_end, closed_trusted_ms, live_ms, state}` + `total_ms()`（`services/timer/coordinator.rs:1655/1680`）。内部已含"采样→检测→异常则先提交恢复事务→隔离" | 已交付；有测试（`timer_regressions.rs` 5 处，含成功路径）；**零生产消费者**，P5 是第一个 |
| 日界（唯一入口） | `services::daily_plan::{local_day_bounds(tz, LocalDate) -> IntervalRange, local_days_covering(tz, from, to) -> Vec<(LocalDate, IntervalRange)>}`（P3 S10） | 已交付（13 条测试，含 DST 23/25 小时） |
| 半开相交 | `domain::interval::{IntervalRange::{new, overlap_ms, overlaps, clipped_ms, duration_ms, is_empty}, IntervalFacts, IntervalSet}` | 已交付；`IntervalSet::insert` 已含"累计不可表示则拒绝" |
| 今日选择列表 | `services::daily_plan::plan_for(db, DailyPlanQuery{date, timezone, expected_data_epoch}) -> DailyPlanView{tasks, data_epoch, revision}`；repo 级 `daily_plan_repo::plan_for(&tx, &date, &timezone)` | 已交付；**Today 必须用 repo 级那条**（G1） |
| 待确认 / 损坏排除 | `services::recovery::attention_overview(db, expected_data_epoch, current_run_id) -> AttentionOverview{items, pending_intervals, pending_sessions, fault_sessions, data_epoch, revision}` | 已交付；**P5 读它，不写第二份损坏判定**（G2） |
| 区间按范围读 | **不存在**（只有 `session_repo::intervals_of_session(conn, session_id)` 按会话） | **Task 1 必须新增**（G7） |
| `task_change` 按形状读 | **不存在**（只有写入；`src/` 里没有 `SELECT … FROM task_change`）。三种 JSON 形状见 `src/storage/mod.rs:6-27` | **Task 4 必须新增**（G3） |
| 任务/标签/项目 | `services::catalog::{list_tasks_filtered, list_tags, tags_of_task, list_projects, list_selectable_projects}` | 已交付 |
| 修正历史 | P3 的 `services::history::correct`（`correct:retime` / `correct:delete`，`time_edit` 审计） | 已交付（T5 的"修正后一致"要用它） |

## P5 必须验证的清单（开工时登记，收尾逐行给结果）

1. **口径**：半开 `[from,to)` 裁剪；三类**分列不合并**（已确认闭合 / 实时暂计 / 待确认）；人工只算 `FOREGROUND`，
   机器分 `BACKGROUND`/`PASSIVE`，`WAITING` 单列；**1h 前台 + 1h 后台必须报人工 1h 而不是 2h**。
2. **排除**：`needs_review=1`、`voided_at` 非空、`discarded` 会话的区间不进任何"已确认"数字；
   已作废/已丢弃的**不显示为待确认**。
3. **跨午夜与日界分桶**：`23:50–00:10` 无暂停会话**两天各 10 分钟**；每日之和 == 不分组总和；
   端点落在日界上不产生零长度段；不同查询时区归属日期不同但总和相同。
4. **同一读事务**：DTO 带 `measure`/`timezone`/`range`/`as_of`/`revision`，且 `revision` 与数据来自同一事务。
5. **Today 五项分别显示不预先相加**；日界按真实日界；列表顺序稳定（`task.created_at, id`）；完成的任务保留在当天列表。
6. **导出**：与界面同一套取数函数；顶层含 `schema_version`/单位/`timezone`/生成时间/`data_epoch`/`revision`；
   导出格式的 `schema_version` 与 07 入站协议**不是同一个东西**（注释写清）；同一输入两次导出除生成时间外字节一致；空范围合法。
7. **周回顾**：人工投入（周一→下周一，半开，用户时区）/ 完成任务（按 `task_change` 完成事件，**不是** `updated_at` 或 session 结束）/
   待确认单列；不含"效率提升"这类推断文案。
8. **修正后一致**：用 P3 的 `correct` 改一条区间，Today / JSON / Markdown 三者同步变化且 `as_of`/`revision` 前进。
9. **不越界**：全局搜 `weight` 在 `services/` 下只见透传或空断言；不引入文件系统/对话框依赖（落盘归 P8）。
10. **人工验收分两半**：P5 做"库明细 ↔ 服务输出"的真库交叉核对；"在真实界面上核对"依赖 P8 的界面 ⇒ **归 P8**，不冒充。

## 不阻塞 P5 的遗留（归属已定，别在这里做）

- **P6**：维护态隔离、`VACUUM INTO` 备份与恢复切换/新 `data_epoch`、单实例跨进程分支、采样线程看门狗、
  退出失败提示与诊断、P4 信封并发证据（第二连接 WAL 写 + 读快照）、正式 OS 事件源（锁屏/休眠/唤醒/改时）。
- **P8**：IPC 命令与全部界面（含 Today 统计半边、恢复确认页、导出与备份恢复页）、导出落盘与路径、
  托盘「完成」项启用、**原生窗口可见性适配**（最小化 ≠ 网页 hidden，已在实机确认）、R-04 发布产物门禁、
  安装包与多 DPI、V0.1 端到端人工验收。
- **实机遗留**（第一轮已做一半，见 [实机验收记录](manual-acceptance-2026-10-05.md)）：F-011 四个托盘动作、
  双窗口 §2.1–§2.6 其余竞态、强杀后 10 秒内重启、500 ppm 跨机器校准。
- `manual_platform_verified` 保持 **false**，直到上述实机项逐条有结论。

## 门禁与工作流

- **门禁**：`src-tauri/scripts/check-pre-p3.ps1`（通用八项，**P5 沿用，不新建脚本**——两份清单必然漂移）。
  迭代期用 `.dsh_tmp/p4-gate.ps1`（fmt + 拉回镜像 + fmt --check + 测试 + clippy + 分层 + git status）。
- **落盘**：代码经 `.dsh_tmp/p3-apply.ps1 -FilesFile <清单>`（显式清单 + 防呆 + SHA256）；文档经 `.dsh_tmp/p3-sync-docs.ps1`。
  落盘后**必须** `git status --porcelain` 复核"恰好是清单里的文件"；测试输出必须出现 `Compiling worktrace`（否则跑的是旧二进制）。
- **提交**：`.dsh_tmp/p4-commit.ps1 -MsgFile … -PathsFile …`（显式路径，`git add -A` 禁用）；推送用 `.dsh_tmp/p3-push.ps1`（含 fast-forward 预检）。
- **进程**：`subagent-driven-development`（每任务：实施 → 独立评审 → 修复 → 定向复核；终审后只一次修复波次）。
