# P5 · 最小统计与导出实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把已经确认的工时事实变成可核对的结果：按范围裁剪、把人工与机器分列、给出 Today 聚合，并导出 JSON 明细与 Markdown 周回顾——口径写在 DTO 里，用户能自己核对，且全程不依赖 AI。

**Architecture:** 新增 `services/stats.rs`（口径与聚合）与 `services/export.rs`（两种导出）。**区间规则只从 P3 取**（半开相交、按真实日界裁剪），本计划不重写一份。统计一律经只读查询 + 服务层纯函数完成，不产生新的持久化事实；唯一写操作是导出文件的落盘（P7 决定路径）。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· `serde_json`（P1 已在）· 无新依赖（Markdown 手写拼接，不引模板引擎）

**Spec:**
- `../specs/2026-10-02-worktrace-architecture/02-data-model.zh.md` §6（统计与标签口径）、§9（明细历史）
- `.../08-implementation-contracts.zh.md` §1（运行区间的归属终点）、§8（`timer_kind` 字段矩阵）
- `.../00-architecture.zh.md` §5（快照必须带 `data_epoch`/`revision`）
- `.../04-functional-spec.zh.md` F-010（Today 与最小统计）、F-018（最小导出与周回顾）

**依赖的前置计划：**
- **P1**：`storage::{meta, task_repo, session_repo, checkpoint_repo}`、`error::AppError`
- **P3**：区间规则（半开相交、按日界裁剪）、`reconcile`/`correct` 产出的非重叠区间集合
- **P4**：`domain::localdate::LocalDate` 与 Rust 时区校验能力（周界与日界按同一套时区策略，不另选第二套）、`services/catalog.rs`（标签、项目）、`services/daily_plan.rs`（今日选择列表）
- **P2**：协调器的**同一次快照**——运行区间的终点只能取它，不能另采墙钟

**边界（不要越界）：**
- **加权统计属 V0.2**（F-109 在 `F-101…F-113` 内）。本计划只做**无权重**的关联口径：所有关联标签都计全部人工时长，**不做**任何按 `weight` 的分配。
- Knowledge 标签与层级属 V0.2（F-107），本计划不做层级去重汇总。
- 能力画像、KPA、`report_snapshot` 属 V0.4/V0.5。
- 文件落盘路径、导出按钮、界面呈现归 **P7**；本计划交付服务层、序列化结果与测试。

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。统计类函数的断言要**给出期望数值本身**（如"两天各 10 分钟"），不要只断言"结果非空"。

---

## Task 1：统计口径——范围裁剪、排除与人工/机器分离

文件：services/stats.rs、tests/stats_window.rs。

- [ ] 统计范围统一为**半开区间** `[from, to)`。每段有效区间与范围的交集按 02 §6 的公式：`max(0, min(end, to) - max(start, from))`。这是全项目唯一一处裁剪实现，P3 与本计划共用，**不得复制第二份**。
- [ ] **运行中区间的终点取同一协调器快照的单调归属终点**，不另采墙钟（02 §6、08 §1）。实现上即：由 P2 传入该次快照的终点值，本层不读时钟。
- [ ] 排除口径：`needs_review = 1`、`voided_at` 非空、以及 `discarded` 会话的区间，一律不计入任何"已确认"数字；只有未作废的待确认区间进入“待确认”栏；voided_at 非空或 discarded 的记录不显示为待确认，仅可在历史/审计中查看。
- [ ] 同一串行边界内先经 P2 取得已验证样本并完成必要异常事务，再开启一致读事务读取事实与 epoch/revision，构造内部 StatsSnapshot 后释放边界；聚合/序列化在外部基于此快照进行。采样至读快照完成之间不允许 pause/resume/finish 等写入穿插。
- [ ] 内部 StatsSnapshot 带 epoch/revision、as_of、run_id、session_id/session_version、开放 interval_id 及 attribution_end；先确认该区间仍是快照中的有效开放区间，再计 live，闭合事实不得重复叠加。公共 TimerSnapshot 可保留展示字段，P2 提供内部统计接缝而非暴露 Instant。
- [ ] 三类分列，**不得合并**：已确认闭合区间（含 `recovering` 会话里已可信的前缀）；实时暂计（当前开放区间）；待确认部分。
- [ ] `measure` 分离：**人工仅 `FOREGROUND`**；机器分 `BACKGROUND` 与 `PASSIVE` 两项；`WAITING` 单列。**禁止把并行机器时长加成人工**——这是 F-103 的核心，1h 前台 + 1h 后台必须报 1h 人工而不是 2h。
- [ ] 每个结果 DTO 必须带 `measure`、`timezone`、`range`、`as_of`、`revision`（02 §6 原文）。`revision` 与数据来自**同一读事务**（00 §5）。
- [ ] 测试：统计取样与 pause/resume/finish/异常分割并发，输出只能对应跃迁前或后的完整快照，不混用版本、不重复计时；作废记录不显示待确认。
- [ ] 测试：半开区间端点相接不重叠；范围完全在区间内/外/部分相交的边界；跨日 `23:50–00:10` 无暂停 session **两天各 10 分钟**（F-010 的验收项，不是"不计入任一日"）；`needs_review`/`voided_at`/`discarded` 被排除；1h 前台 + 1h 后台 → 人工 1h、机器 1h；`WAITING` 不被并入任何一项；DTO 五个字段齐全且 `revision` 与快照一致。

## Task 2：Today 聚合

文件：services/stats.rs、tests/today.rs。

- [ ] Today 一次返回（避免 N+1，00 §4）：今日选择列表（来自 P4 的 `plan_for`）、当前任务与运行状态、确认人工工时、运行暂计、待确认时间——**五项分别显示，不预先相加**（F-010 原文）。
- [ ] Today 不依赖 `time_block`（V0.2）；日界按用户时区的真实日界，不假设每天 24 小时（08 §1）。
- [ ] 当前任务与运行状态取自 P2 的协调器快照；本层不自行推断 session 状态。
- [ ] 今日选择列表按 P4 的稳定顺序（`task.created_at, id`）返回；完成的任务**保留在当天列表里**并带状态，不因完成而消失（P4 的约定）。
- [ ] 测试：五项数值分别正确且互不覆盖；跨日边界（含夏令时切换日）归属正确；无任何 session 时返回空结构而非错误；列表顺序稳定（连续两次调用结果一致）；`revision` 随一次业务写前进而前进，纯读不前进。

## Task 3：JSON 明细导出

文件：services/export.rs、tests/export_json.rs。

- [ ] 导出内容**与界面显示一致**（F-018 原文）：用与 Today/报表相同的 `services/stats.rs` 函数，不另写一套取数逻辑。
- [ ] 顶层必须含 `schema_version`、单位（毫秒/分钟的口径写清）、`timezone`、生成时间；并带 `data_epoch` 与 `revision`，便于与界面核对是同一份数据。
- [ ] 冻结**筛选口径**：范围、时区、分类依据（R-03：按当前标签/项目归属重算）一并写进导出，使第三方能复现这次结果（02 §6、02 §9）。
- [ ] **导出不依赖 AI**（F-018 原文），也不需要网络。
- [ ] 序列化用 `serde`，字段名与 DTO 一致；`schema_version` 是**导出格式**的版本，与入站导入协议（07 的 `schema_version`）不是同一个东西——注释里写清，避免以后混用。
- [ ] 测试：同一输入两次导出字节一致（除生成时间外）；导出里的数字与 `stats` 的返回值逐字段相等；`schema_version`/单位/时区/生成时间/epoch/revision 齐全；空范围导出合法且不 panic；导出的区间集合两两不重叠（P3 的保证在此可见）。

## Task 4：Markdown 周回顾

文件：services/export.rs、tests/export_markdown.rs。

- [ ] 周回顾含三部分（F-018 原文）：**人工投入**（按本服务明确的周界与用户时区（首版周一至下一周一，半开范围））、**完成任务**（按 `task_change` 的完成事件时刻选，不用 `updated_at`、也不用 session 结束时间——02 §10）、**待确认记录**（单列，不与确认工时混在一起）。
- [ ] 明确标注口径，不宣称 AI 结论：文案只说事实与已确认数字，不含"效率提升""节省时间"这类推断（04 的 F-206 禁止把采纳率说成节省时间）。
- [ ] 手写 Markdown 拼接，不引模板引擎；表格列宽不做对齐美化（等宽字体外无意义）。
- [ ] 测试：跨周边界的记录归到正确的周；已完成任务按完成事件时刻入周（改过 `updated_at` 不影响）；待确认单独成节且不影响人工合计；同一个周重复生成内容稳定；空周也能生成合法文档。

## Task 5：集成与人工验收

文件：tests/stats_end_to_end.rs。

- [ ] 端到端：造一组含 `FOREGROUND`/`BACKGROUND`/`PASSIVE`/`WAITING`、含暂停、含一条待确认、含一条被作废、跨日、跨周的真实数据，断言 Today、JSON 导出、Markdown 周回顾三者的数字**互相一致**，且都能追溯到同一批区间。
- [ ] 校验"修正后一致"（F-017 的验收项）：用 P3 的 `correct` 改一条区间，再查 Today 与导出，三者同步变化且口径字段（`as_of`/`revision`）随之前进。
- [ ] **人工验收（不能用单元测试代替，08 §6）**：在真实界面上核对 Today 的五项数字与数据库里的区间明细一致；导出 JSON 后用外部工具重算人工合计，与界面一致；生成一次周回顾并逐项核对"完成任务"与任务变更记录。记录机器/系统版本与观察结果。
- [ ] 完成门槛：在 `src-tauri` 运行 `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets`，并执行 P1 的分层检查脚本；P1–P4 的测试无回归。
- [ ] 确认本计划**没有引入加权逻辑**：全局搜索 `weight` 在 `services/` 下的使用应只在透传或断言为空处；若出现按权重分配的实现，属越界到 V0.2，评审打回。

---

## 下游接口（供 P7 消费）

- `services/stats.rs`：Today 聚合、范围报表入口（实施后登记真实 Rust 签名与 DTO 字段）。
- `services/export.rs`：JSON 与 Markdown 的生成函数，返回**字符串或字节**而不是自己写文件——落盘路径与权限由 P7 决定。
- 两者都返回带 `data_epoch`/`revision` 的信封，P7 的界面据此丢弃过期结果。
