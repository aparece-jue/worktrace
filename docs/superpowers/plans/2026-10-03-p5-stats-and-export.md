# P5 · 最小统计与导出实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把已经确认的工时事实变成可核对的结果：按范围裁剪、把人工与机器分列、给出 Today 聚合，并导出 JSON 明细与 Markdown 周回顾——口径写在 DTO 里，用户能自己核对，且全程不依赖 AI。

**Architecture:** 新增 `services/stats.rs`（口径与聚合）与 `services/export.rs`（两种导出）。**区间规则只从 P3 取**（半开相交、按真实日界裁剪），本计划不重写一份。统计一律经只读查询 + 服务层纯函数完成，不产生新的持久化事实；唯一写操作是导出文件的落盘（P7 决定路径）（2026-10-04 修订：落盘改归 **P8**；文末注为准）。

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
- 文件落盘路径、导出按钮、界面呈现归 **P7**（2026-10-04 修订：**落盘改归 P8**，本计划只负责生成内容；文末注为准）；本计划交付服务层、序列化结果与测试。

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
- [ ] **跨午夜与日界分桶（02 §8「跨午夜含暂停」的 P5 半边）**：按**查询时区**的实际日界拆分可信区间，逐日工时与总和一致——每日之和 == 不分组的总和（分桶不丢不重）；同一区间跨日时被拆成两段且**两段之和等于原区间时长**；端点恰好落在日界上不产生零长度段；查询时区不同（UTC / Asia/Shanghai）时**归属的日期不同但总和相同**。
  与 **P2 的分工**：P2 的 `pausing_across_midnight_keeps_pause_out_of_effort` 验证**事实**（午夜前工作 → 暂停跨过午夜 → 次日继续 → 结束，暂停不计入、两段各自落在自己那一天）；P5 只验证**分桶**——事实从 P2 来，P5 不重造。两者合起来才是 02 §8 那一条的完整覆盖。

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
- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [ ] 完成门槛：在 `src-tauri` 运行 `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets`，并执行 P1 的分层检查脚本；P1–P4 的测试无回归。
- [ ] 确认本计划**没有引入加权逻辑**：全局搜索 `weight` 在 `services/` 下的使用应只在透传或断言为空处；若出现按权重分配的实现，属越界到 V0.2，评审打回。

---

## 下游接口（供 P7 消费）

- `services/stats.rs`：Today 聚合、范围报表入口（实施后登记真实 Rust 签名与 DTO 字段）。
- `services/export.rs`：JSON 与 Markdown 的生成函数，返回**字符串或字节**而不是自己写文件——落盘路径与权限由 P7 决定（2026-10-04 修订：**落盘改归 P8**，本计划只负责生成内容；文末注为准）。
- 两者都返回带 `data_epoch`/`revision` 的信封，P7 的界面据此丢弃过期结果。

---

**注（2026-10-04 归属订正）**：落盘归 **P8**（本计划只负责生成内容）——上方「边界」与本节里「落盘路径与权限由 P7 决定」的措辞不改变本计划的交付边界；P8 计划 Task 3 已登记该责任与「先决定并登记落盘能力」的要求，本计划不引入文件系统/对话框依赖。

## P3 前遗留的消费门槛（2026-10-04）

- [ ] 消费 [pre-p3-closure](../../validation/pre-p3-closure.md) 的 #4–5 和异常表：复用 P3 四类判定，损坏/待确认/作废分别排除，不能只看当前 timer。
- [ ] 跨午夜含暂停的报表用例、修正后重算、不同 task_change 形状的完成项过滤必须提供结果；P2 已有计时证据不能替代报表验证。

---

## P5 开工前补正（2026-10-05，控制器落盘；开工基线 `a9ba04a`）

> 开工前把本计划的**消费面**逐条对照真实代码核对过（P3 已交付：`origin/dev` = `a9ba04a`，Rust 599 passed、八项门禁全 0）。
> 下面是必须补的 7 处；每条都给依据与代价。裁决全文见 SDD 台账 `.superpowers/sdd/2026-10-03-p5-stats-and-export/progress.md`。

**G1（Ruling P5-1）｜Today 必须在自己的读事务里用 repo 级入口取日计划**
`services::daily_plan::plan_for` **自己开事务并自己 `guard_epoch`**；在 Today 的读事务里嵌套调用它会变成两次 epoch 守卫 + 两个快照，
与"每个 DTO 的 `revision` 与数据来自同一读事务"（00 §5）冲突。⇒ 用 `storage::daily_plan_repo::plan_for(&tx, &date, &timezone)`
在自己的事务里取；T2 必须有"与 `services::daily_plan::plan_for` 结果一致"的用例钉住排序与筛选。
**代价**：与 P4 的读入口有一处轻微重复，靠该用例兜住。

**G2（Ruling P5-6）｜"待确认 / 损坏"的排除口径读 P3 的 `attention_overview`，不要写第二份判定**
P3 计划「下游接口」第 2 条明写：P5 排除可疑区间时读 `attention_overview` 的 `fault_sessions`/`attention`，**不要自己写第二份损坏判定**。
真实签名：`services::recovery::attention_overview(db: &Db, expected_data_epoch: &str, current_run_id: &str) -> Result<AttentionOverview, AppError>`，
`AttentionOverview{items, pending_intervals, pending_sessions, fault_sessions, data_epoch, revision}`。
**注意**：它的列表口径是"不变量损坏 ∪ 未作废待确认区间 ∪ **别的 run 未结束会话**"，与"计时门禁是否关闭"**不等价**（P8 计划已订正这条），
所以别拿"列表空不空"当门禁判据。**代价**：Today 多一次只读查询。

**G3（Ruling P5-7）｜`task_change` 的生产读取入口不存在，Task 4 必须新增**
全仓只有**写入**（`task_repo::record_change` 等）与**测试里的裸 SQL**——`src/` 里没有一条 `SELECT … FROM task_change`。
Task 4 的「完成任务按 `task_change` 的完成事件时刻选」因此需要新增**仓储层**读取入口（服务层不写 SQL）。
必须按**形状**过滤：三种形状见 `src/storage/mod.rs:6-27`（任务字段 / 标签集合 / 今日计划集合），
完成事件是 `json_extract(after_json,'$.status') = 'Done'`；**"有没有 `task_change` 行"回答不了任何业务问题**
（贴标签、加今日计划同样会写一行）。排序口径：**禁止"取最后一条"**——P3 台账的携带项写着 `task_change` 同毫秒可落多行、
`ORDER BY created_at, id` 在它们之间不确定，要按形状 + 事件时刻取值。**代价**：Task 4 多一个仓储函数 + 它自己的测试。

**G4（Ruling P5-8）｜日界只走 S10 的两个函数**
`services::daily_plan::{local_day_bounds(timezone, date) -> IntervalRange, local_days_covering(timezone, from, to) -> Vec<(LocalDate, IntervalRange)>}`
（P3 交付：`end` 由"次日零点"换算、跨日不整段丢、`from == to` 返空、`from > to` 报 `NegativeInterval`）。
Task 1 的跨日分桶与 Task 4 的周界都用它们，**不新写第二份日界**；相交一律用 `domain::interval` 的 `overlap_ms`/`clipped_ms`/`IntervalSet`。

**G5（Ruling P5-9）｜需要 `AppState` 瘦包装才够得着协调器**
`AppState.coordinator` 是私有字段，而运行区间的终点只能来自 `Coordinator::stats_sample(&mut self, db)`。
⇒ 按 P3 的 S2 模式加**瘦包装**（解构 `AppState { db, coordinator, .. }`、取样本、调服务）。
**IPC 命令仍归 P8**（P5 不新增 `#[tauri::command]`、不改 `commands/mod.rs`）。
`stats_sample` 不是无主接缝：`tests/timer_regressions.rs` 有 5 处调用（含成功路径），P5 是它的**第一个生产消费者**。

**G6（Ruling P5-5）｜完成门槛里的 cargo 命令一律加 `--offline`**
本机无网络；`cargo test` / `cargo clippy --all-targets` 都按 `--offline` 跑；分层脚本必须在 `src-tauri` 目录里执行。

**G7（Ruling P5-10）｜"按范围读区间"的仓储入口也不存在，Task 1 必须新增**
`session_repo` 只有 `intervals_of_session(conn, session_id)`（**按会话**），没有"与 `[from, to)` 相交的区间"查询。
⇒ Task 1 新增一个仓储级入口，谓词沿用既有半开相交形状（`i.started_at < ?to AND (i.ended_at IS NULL OR ?from < i.ended_at)`），
并保留 P3 收紧的"**空区间不占时间**"（`(i.ended_at IS NULL OR i.started_at < i.ended_at)`），排除 `voided_at IS NOT NULL`。
不要用"遍历会话各查一次"绕过（计划明写 Today 避免 N+1）。**代价**：Task 1 多一个查询；索引与大数据量下的表现留给后续阶段观察。

**门禁（Ruling P5-11）｜不新建 P5 门禁脚本**
`src-tauri/scripts/check-pre-p3.ps1` 跑的就是通用八项（脚本回归 / Rust 测试 / Clippy / 格式 / 分层 / 前端测试 / 构建 / diff），
名字是历史的；两份清单必然漂移，所以 P5 沿用同一入口。开工基线证据：
`C:\Users\lenovo\AppData\Local\Temp\worktrace-pre-p3-20261005-185046`（八项全 0、599 passed / 0 failed / 1 ignored）。

**已考虑但决定不做**：不抽 `tests/common/` 共享夹具——P3 的 8 个测试文件各自带夹具（900–2100 行/文件）可读性更好，
P5 规模更小（5 个文件），统一抽象反而会把各任务的独立性绑在一起。若实施中出现明显重复，由该任务的评审提出。

---

## P5 实施记录（2026-10-06，控制器落盘）

> 代码范围 `0503f65..1d6a3f6`（10 提交 / 11 文件 / **+8123 行、0 删除**——纯新增，未改动任何一行既存代码）。
> `cargo test --offline` **654 passed / 0 failed / 1 ignored**（P5 前 599）；`cargo fmt --check` / `clippy -D warnings` / 分层六条全绿；
> 项目级八项门禁 **8/8 exit 0、`automated_passed = true`**（⇒ P1–P4 无回归）；无新 IPC/错误码/依赖/schema 迁移，前端与 15 份快照 fixture 零 diff。
> 逐条验收与未完成项见 `docs/validation/p5-acceptance.md`；实施期裁决见 `.superpowers/sdd/2026-10-03-p5-stats-and-export/progress.md`。

### 实际交付签名（P8 按这些接线；「下游接口」那一节以本记录为准）

```rust
// services/stats.rs
pub fn snapshot(db: &mut Db, sample: StatsSample, query: &StatsRangeQuery) -> Result<StatsSnapshot, AppError>;
impl StatsSnapshot {
    pub fn report(&self) -> RangeReport;           // 唯一聚合点；聚合发生在串行边界之外，纯函数
    // + run_id / session_id / session_version / open_interval_id / attributed_end / state / data_epoch / revision
}
pub fn today(db: &Db, sample: StatsSample, query: &TodayQuery) -> Result<TodayView, AppError>;
pub struct StatsRangeQuery { pub from: i64, pub to: i64, pub timezone: String, pub expected_data_epoch: String }
pub struct TodayQuery { pub timezone: String, pub expected_data_epoch: String }   // 不带 date：「今天」由同一次样本的 A(M) 算
pub enum Measure { Human, MachineBackground, MachinePassive, Waiting }
pub enum StatsClass { Confirmed, Live, Pending }
pub struct MeasureColumn { class, measure, timezone, range, as_of, data_epoch, revision, ms: Option<i64>, intervals: usize }
pub struct DayTotal { pub date: String, pub confirmed: Vec<MeasureColumn> }        // 固定四项
pub struct StatsInterval { id, session_id, task_id, class, measure, started_at, ended_at: Option<i64>,
                           duration_ms: Option<i64>, clipped_ms: i64, needs_review: bool }
pub struct RangeReport { confirmed/live/pending: Vec<MeasureColumn>, days: Vec<DayTotal>, intervals: Vec<StatsInterval>,
                         fault_sessions_excluded: usize, timezone, range, as_of, data_epoch, revision }
impl RangeReport { pub fn column(&self, class: StatsClass, measure: Measure) -> &MeasureColumn }
pub struct TodayView { tasks: Vec<TaskRow>, current: Option<CurrentTask>,
                       confirmed/live/pending: Vec<MeasureColumn>,   // 三类各四项，**无合计字段**
                       date, timezone, range, as_of, data_epoch, revision }
impl TodayView { pub fn column(&self, class: StatsClass, measure: Measure) -> &MeasureColumn }

// services/export.rs
pub fn json(db: &Db, sample: StatsSample, query: &StatsRangeQuery, generated_at: i64) -> Result<ExportJson, AppError>;
pub fn weekly(db: &Db, sample: StatsSample, query: &WeeklyQuery, generated_at: i64) -> Result<ExportMarkdown, AppError>;
pub struct WeeklyQuery { pub timezone: String, pub anchor: Option<i64>, pub expected_data_epoch: String }  // None = 本周
pub struct ExportJson { pub text: String, pub data_epoch: String, pub revision: i64 }
pub struct ExportMarkdown { pub text: String, pub data_epoch: String, pub revision: i64,
                            pub week_start: String, pub week_end: String, pub range: StatsRange, pub timezone: String }

// storage（服务层不写 SQL）
pub fn session_repo::intervals_overlapping(conn: &Connection, from: i64, to: i64) -> Result<Vec<IntervalWithSession>, AppError>;
pub fn task_repo::done_events_within(conn: &Connection, from: i64, to: i64) -> Result<Vec<DoneEvent>, AppError>;

// AppState（services/bootstrap.rs；G5 瘦包装，**不新增 `#[tauri::command]`**）
pub fn stats_snapshot(&mut self, &StatsRangeQuery) -> Result<StatsSnapshot, AppError>;
pub fn stats_today(&mut self, &TodayQuery) -> Result<TodayView, AppError>;
pub fn export_json(&mut self, &StatsRangeQuery) -> Result<ExportJson, AppError>;
pub fn export_weekly_markdown(&mut self, &WeeklyQuery) -> Result<ExportMarkdown, AppError>;
```

### 实施期改动的口径（都在代码注释里可追 Ruling 编号）

1. **`pending` 列 = 条数 + 已知候选端点的跨度之和**（Ruling P5-12）：`intervals` **含零长度候选**（`[t,t)` 按"点落在 `[from,to)` 内、含起点"判；`ended_at IS NULL` 按"会话已开始"判）；
   `ms` 是"已知端点候选的 clipped 跨度之和"，**全未知才 `None`**（只有 `[t,t)` 时是 `Some(0)`）。与 `attention_overview.pending_intervals` 的差**只剩范围裁剪**这一处。
   ⇒ **判断"有没有待确认"用 `intervals`，不要用 `ms.is_some()`**（P2 的 `pending_ms` 把 0 折成 `None`）。
2. **Today 给全四类 measure 的列组**（Ruling P5-15）：F-010 的五项 = `tasks` / `current` / 三组里的 `Human` 三列；**没有任何"总计"字段**，界面不得相加。
3. **`generated_at` 是"生成本刻的墙钟"**（Ruling P5-19），由包装层取一次平台时钟；`as_of` 是数据水位。两者**可以不等**，文档写清了这层区别。`services/` 一律不读时钟（机器强制）。
4. **R-03 分类依据可复现**（Ruling P5-20）：JSON 顶层 `criteria` 冻结范围/时区/分类依据，并附"任务 → 项目/标签"的数据级连接（**零时长字段**，只做连接不重算）。
5. **周回顾三节只收人工**（Ruling P5-24）：机器/等待的时长与待确认记录都指向 JSON 导出；完成项按 `task_change` 的 `done` 事件时刻入周，**每个 Done 事件各归其周**（同任务可出现在两周）；多一列「当前状态」（完成→重开后显示"已重新打开"）。
6. **`WeeklyQuery.anchor`**（Ruling P5-21）：`None` = 同一次样本的 `A(M)`（本周）；查历史周要传时间戳。

### 下游必须知道的（终审分诊后落在这里，P8/P6 请照此规划）

- **两类导出都没有 `Serialize`**：P8 要把信封经 IPC 返回时自己加 derive，**并顺带决定信封字段**（建议带 `as_of`，否则只能解析 `text` 或二次查询）。
- **`task_change` 与 `work_interval(started_at)` 都没有索引**：**现在不要加**——加索引＝改已发布 schema + 迁移，会打红 `schema_v1.rs` 的"恰好八个 spec 索引"断言，也与"P5 未改 schema"冲突；先测量，再在拥有 schema 演进的阶段决定。
- **导出与聚合都在 `AppState` 串行边界内完成**（"完成事件/任务必须与数字同版本"逼出来的取舍）：P8 做大数据量导出时注意它仍持锁（心跳 30s 级，风险低），必要时再谈快照外聚合。
- **`task.title` 扫描器误报仍挂着**（Ruling P5-18）：`tests/error_contract.rs` 的退役英文子串门禁对字段访问误报，正确修法是把它收紧到构造点/字面量并重验它能抓真回潮。
