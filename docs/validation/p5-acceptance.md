# P5 验收记录：最小统计与导出

日期：2026-10-06。范围：`0503f65..1d6a3f6`（`dev` 分支，10 个提交）。依据：`docs/superpowers/plans/2026-10-03-p5-stats-and-export.md`
（含文末「P5 开工前补正 G1–G7」）与[总纲 §5 第 9 条](../superpowers/plans/2026-10-03-v01-plan-index.md)的权威清单；
开工交接见 [pre-p5-closure](pre-p5-closure.md)。

> **本记录只证明自动化与"库明细 ↔ 服务输出"那半。** P5 交付的是**服务层**（`services/stats.rs`、`services/export.rs`、两个仓储读取入口、`AppState` 四个瘦包装）。
> **IPC 命令、界面呈现与"在真实界面上核对"归 P8**（P5 计划「边界」与 Ruling P5-3）。未完成项在文末单列，**不得**据本文件宣称 V0.1 可发布。

## 1. 自动化门禁

| 项 | 结果 |
| --- | --- |
| `cargo test --offline` | **654 passed / 0 failed / 1 ignored**（P5 前 599；1 ignored 为既有 startup helper） |
| `cargo fmt --check` | 0 |
| `cargo clippy --all-targets --offline -- -D warnings` | 0 告警 |
| `scripts/check-layers.ps1` | 六条 PASSED（含"`services` 不得读系统时钟"） |
| **项目级八项门禁** `check-pre-p3.ps1`（名字历史遗留，跑的是通用八项） | **8/8 exit 0、`automated_passed=true`**，rust-tests 求和 654 ⇒ **P1–P4 无回归**。证据目录 `…\worktrace-p5t5-20261006-final` |
| 前端与契约面 | 未改：15 份快照 fixture 与 `src/` 零 diff；**无新增 IPC 命令、无新错误码、无新依赖、无 schema 迁移** |

新增 5 个集成测试文件：`stats_window` / `today` / `export_json` / `export_markdown` / `stats_end_to_end`（合计约 6200 行）。

## 2. 总纲 §5 第 9 条权威清单：逐条核对

### 2.1 02 §8 的 M01/M05 必测案例（与本阶段相关的 5 条）

| 条目 | 结论 | 依据（用例 + 数字） |
| --- | --- | --- |
| 跨午夜含暂停 | **已核对** | 事实侧 `tests/timer_commands.rs::pausing_across_midnight_keeps_pause_out_of_effort`（P2）；分桶侧 `tests/today.rs`、`tests/stats_window.rs`（每日之和 == 不分组总和、端点落界、UTC/上海归属不同但总和相同）；端到端 `tests/stats_end_to_end.rs::today_json_and_weekly_report_the_same_intervals_and_the_same_numbers`（03-09 23:50→00:00 + **暂停跨午夜** + 00:10→00:20 ⇒ 今天人工 `4_200_000`、本周 `4_800_000`、日桶 03-09 = `600_000`、03-10 = `4_200_000`，暂停那 10 分钟无人覆盖） |
| 空范围 | **已核对** | `tests/stats_end_to_end.rs::an_empty_range_and_a_day_without_intervals_are_zero_in_every_view`（`[WALL, WALL)` ⇒ 四项 `ms = Some(0)`、`intervals = 0`、`days`/明细为空、口径字段齐全）；`tests/export_json.rs::an_empty_range_is_a_valid_export_with_zero_columns_and_no_detail` |
| 历史工时重叠 | **已核对（P3 面，P5 不新增判定）** | `tests/correct.rs` 的重叠四条 + `tests/reconcile.rs`；P5 的 `correct` 只走 P3 既有判据（T5 那次是"收短 + 排除自身"，落在边界用例内） |
| 区间修正后报表重算 | **已核对** | `tests/stats_end_to_end.rs::correcting_one_interval_moves_today_json_and_weekly_together`：1 小时 → 30 分钟 ⇒ 今天人工 `4_200_000 → 2_400_000`、本周 `4_800_000 → 3_000_000`、Markdown「本周合计：50 分钟（3000000 毫秒），共 3 条已确认区间」；`revision` +1、`as_of` 前进、`data_epoch` 不变；机器/等待/待确认三列**一毫秒未动** |
| 待确认排除与显式确认 | **已核对（两半各有依据）** | 排除：`tests/stats_window.rs` 三条（作废/丢弃不进已确认且已作废不显示为待确认、零长度候选计数不贡献跨度、已知端点候选按 clipped 贡献）+ T5（`confirmed.human = 4_200_000` 不含候选，`pending.human` 1 条 / `600_000`，明细里 `class=pending` 且 `duration_ms = null`）。显式确认：`tests/reconcile.rs::confirming_every_pending_interval_in_one_command_closes_the_session` + `tests/recovery_end_to_end.rs`（`reconcile(Confirm)` 后 `assert_confirmed` 链路）。**T5 只覆盖"排除"半边**——按裁决它只允许 `correct` 一条写命令 |

其余 9 条（暂停后重启、暂停直接结束、恢复前台冲突、并发 start、同名根标签、强杀后十秒内重启、运行/暂停时退出、前后改系统时间、旧版本编辑冲突）**不适用**：属 P2 计时语义 / P4 标签 / P3 恢复 / P6 平台事件的范围。

### 2.2 04 §9 的必做集成用例（与本阶段相关的 2 条）

| 条目 | 结论 | 依据 |
| --- | --- | --- |
| 重复提交 | **已核对（P3/P4 面；P5 无新增写路径）** | P5 的统计/导出是只读（T5 里同一范围连读两次 `revision` 不变），导出**不落盘**（落盘归 P8）；幂等仍由既有写命令用例钉住（`tests/correct.rs` / `daily_plan.rs` / `tags.rs` / `reconcile.rs`） |
| 统计修正 | **已核对** | 同 2.1「区间修正后报表重算」 |

其余 4 条（迁移失败、磁盘不足、旧版本修改、跨午夜暂停）**不适用**：迁移与磁盘属 P1/P6，旧版本修改属 P1/P3 的写命令，跨午夜暂停属 P2。

### 2.3 06 §4 实现前技术验证

三项**全部不适用**，理由如下：DB 执行边界（P5 未改任何执行边界，只调用既有仓储读入口并沿用 P2/P7 的单锁串行）；单调/墙钟映射（P5 的**数字**不读时钟——区间终点只来自 `stats_sample().attributed_end`，`services/stats.rs` 内零时钟调用；`generated_at` 只是"文件何时产出"的标注，由包装层从同一时钟接缝取一次）；双窗口同步（界面归 P8；P5 的贡献是三条路径都带 `as_of`/`revision`/`data_epoch` 信封）。

## 3. 完成门槛的其余条目

| 门槛 | 结论 |
| --- | --- |
| G1 日计划在同一读事务里用 repo 级入口 | 已核对：`services/stats.rs` 内 `guard_epoch` 零调用（全路径只有 `attention_overview` 一次守卫），并有与 `services::daily_plan::plan_for` **逐字段含顺序**一致的对拍用例 |
| G2 待确认/损坏读 `attention_overview`，不写第二份判定 | 已核对：`pending_ids`/`fault_session_ids` 取自 `overview.items`；`services/stats.rs` 内**零 SQL** |
| G3 `task_change` 生产读取入口 | 已落地：`storage::task_repo::done_events_within`，谓词 `json_extract(after_json,'$.status') = 'Done'` + 半开事件时刻 + `ORDER BY created_at, task_id, id`（**返回全部事件、不取最后一条**）；"只贴标签/只加今日计划"不会被算成完成（三种非完成形状的用例 + sabotage 钉住） |
| G4 日界只走 S10 | 已核对：周界与日桶端点只由 `local_day_bounds`/`local_days_covering` 给出（含 NY 夏令时切换周 `167` 小时、`00:30→03:30` 真实 2 小时） |
| G5 `AppState` 瘦包装 | 已落地：`stats_snapshot` / `stats_today` / `export_json` / `export_weekly_markdown`；**无新增 `#[tauri::command]`、`commands/mod.rs` 未改** |
| G6 完成门槛 | 见 §1（两套门禁） |
| G7 按范围读区间的仓储入口 | 已落地：`session_repo::intervals_overlapping`，谓词与 `require_no_human_overlap` 同形 + P3 的"空区间不占时间"；**待确认按类分判**（含零长度候选），`Confirmed`/`Live` 语义未放宽（复审做过代数等价核对） |
| R-03 分类依据可复现 | 已落地（Ruling P5-20）：导出冻结范围/时区/分类依据，并附"任务 → 项目/标签"的数据级连接（零时长字段），使第三方能按标签/项目归并 `clipped_ms` |
| `generated_at` 由调用方传入 | 已落地（Ruling P5-19）：包装层取一次 `Coordinator::wall_ms`，`services/export.rs` 零时钟读取；文档写明"数字截至 `as_of`，`generated_at` 只说明文件何时产出，两者可以不等" |
| **没有引入加权逻辑**（V0.2 边界） | 已核对：`services/` 下 `weight` 的 4 处命中**全在注释与导出文案**；全 `src/` 里参与运算的表达式 **0 命中**（报告 §4 有命令与输出） |

## 4. 人工验收（08 §6）：**半格，界面半边归 P8**

**P5 已做的那半**（"库明细 ↔ 服务输出"交叉核对，真库 + 真入口）：`tests/stats_end_to_end.rs` 造出含
`FOREGROUND`/`BACKGROUND`/`PASSIVE`/`WAITING`、含暂停、含一条待确认、含已作废与已丢弃、跨日、跨周的真实数据，
断言 **Today / JSON / Markdown 三者数字互相一致**且可追溯到同一批区间；并用**直连 SQL 的手写 02 §6 公式**（不调 `clipped_ms`）做独立 oracle 复核。

**未做（不得记通过，归 P8）**：
- 在**真实界面**上核对 Today 五项数字与库明细一致、导出 JSON 后用外部工具重算人工合计、生成一次周回顾并逐项核对"完成任务"。
- 导出**落盘**与路径（P5 只生成字符串；计划已把落盘改归 P8）。
- `ExportJson` / `ExportMarkdown` 都**没有 `Serialize`**：若 P8 要把信封经 IPC 返回，由 P8 侧加 derive。
- 双窗口/托盘的实机项、锁屏/休眠/改时等平台事件（P6 实现 + P8 验收）。

`manual_platform_verified` 保持 **false**。

## 5. 2026-10-08 复审修复收口

此前两处排除文案已修正：损坏会话的已确认区间排除，待确认候选仍保留；本次采样认可的开放区间按 live 分支处理。JSON 回归使用 running 且带待确认候选的损坏会话核对实际明细。

空范围现在在统一范围读取入口返回零条；实时区间还必须与查询范围有真实交集，未来范围不再产生零时长 live 明细。新增回归同时覆盖 confirmed、pending、live 与 JSON。

统计/导出服务在**正常采样下**只读；AppState 入口的采样若发现异常，走 P2 的异常路径——**可能**提交恢复事务及其审计（**幂等分支与硬故障回滚分支零写入**，只置故障态），随后返回恢复错误。既有回归是**协调器级**的 `tests/timer_regressions.rs::old_running_session_is_isolated_before_sampling_with_or_without_new_anchor`（含 `stats_sample` 在内 7 个入口，断言 `RECOVERY_REQUIRED`、`revision` 不变、无新审计）；**AppState 入口级（`stats_today`/`export_json`/`export_weekly_markdown`）的时钟异常联动用例本轮未新增**——入口级要额外证明"包装层不吞异常、也不在异常后拿旧样本产出数字"。Today 与导出目前在同一串行边界完成，以维持数字与任务、标签、完成事件的版本一致。

错误 ruling 引用和 Markdown 水位解析锚点已修正；水位仍是裸 Unix 毫秒，没有“毫秒”后缀。历史验收数字保留，当前复验结果见[跨阶段复审](cross-stage-review-2026-10-08.md)。

## 6. 携带给下游（终审建议：这些不在 workspace 台账里，P8/P6 看不到）

| 项 | 内容 | 归属 |
| --- | --- | --- |
| 两类导出缺 `Serialize` | `ExportJson` / `ExportMarkdown` 都没有 `Serialize`。P8 要把信封经 IPC 返回时得自己加 derive，**并顺带决定信封字段**（建议带 `as_of`，否则 P8 只能解析 `text` 或二次查询） | **P8** |
| 两个无索引点 | `task_change`（周回顾按 `json_extract` 过滤 ⇒ 全表扫）与 `work_interval(started_at)`（范围报表排序）。**现在不要加**：加索引＝改已发布 schema + 迁移，会打红 `schema_v1.rs` 的"恰好八个 spec 索引"断言，也与"P5 未改 schema"的完成前提冲突；先测量，再在拥有 schema 演进的阶段决定 | **P6/后续** |
| 查询接口的两个代价 | `TodayQuery` 不带 `date`（「今天」由同一次样本的 `A(M)` 算）⇒ 查不了"昨天的 Today"；`WeeklyQuery.anchor` 是可选的毫秒时间戳 ⇒ 查历史周要传时间戳。两者都是为了"日期与样本同源"（Ruling P5-17/P5-21） | P8 知情即可 |
| 扫描器误报仍挂着 | `tests/error_contract.rs` 的退役英文子串门禁对 `task.title` 这种**字段访问**误报，导致 `stats.rs` 里一处变量改名为 `task_row`（Ruling P5-18）。正确修法是把它收紧到构造点/字符串字面量并重验它能抓真回潮 | 后续小改动 |
| 证据链卫生 | 本阶段所有 sabotage 都跑在 `cargo fmt` 之前的字节上（断言文本逐条对过、结论不变）。后续固定"对冻结产物重跑 sabotage 集，或给 sabotage → 最终用例名的映射表" | 流程 |
| P8 前瞻 | 聚合与导出目前都在 `AppState` 串行边界**内**完成——这是"完成事件/任务必须与数字同版本"逼出来的取舍（代码注释里已认账）。大数据量导出时注意它仍持锁（心跳 30s 级，实际风险低），必要时再谈快照外聚合 | **P8** |

## 2026-10-08 异常入口回归补齐（56980b1 后复审）

新增 stats_entry_anomaly 的两条测试，分别覆盖硬故障零写入和运行期墙钟异常恢复一次/后续幂等。复审已将固定入口顺序改为独立夹具轮换三个首入口，Today、JSON、Weekly 均真正触发首次异常；共六个独立异常场景。新 run 再读 Today 的可信前缀/待确认数字与恢复后版本一致。

当前全量 Rust 659 passed / 0 failed / 1 ignored，前端 142 passed；静态、格式、分层及差异检查通过。此前 654/657 为各轮历史记录，保留不覆盖。此次仅调整测试覆盖，不修改生产行为；详情与日志见[跨阶段复审](cross-stage-review-2026-10-08.md)。
