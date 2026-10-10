/**
 * IPC 契约的 TypeScript 形状（P7 Task 1b，**手写**，D1 裁决）。
 *
 * 权威在 Rust 侧，两处：
 *
 * - 请求形状：`src-tauri/src/commands/mod.rs` 的请求 DTO（每条命令只收一个
 *   `request` 参数，字段名就是那些结构体的 snake_case 字段名）；
 * - 响应形状：仓库根 `src/types/__snapshots__/*.json`（21 份，由
 *   `src-tauri/tests/ipc_snapshots.rs` 与 Rust 类型**逐字节**比对）。
 *
 * 手写镜像与快照之间靠一条用例保持机械联系
 * （`src/types/__tests__/snapshot-contract.test.ts`）：键集合逐层比对，
 * 枚举取值域逐字比对。**改了这边不改那边就会红。**
 *
 * ## 两条不要"顺手统一"的约定
 *
 * 1. **字段名是 snake_case**：这些类型是线上形状，不是本地风格。转成 camelCase 就
 *    再也对不上 Rust 的字段名（Tauri 只对**参数名**做 rename_all，结构体字段不转）。
 * 2. **枚举大小写照抄 `as_str()`，不是统一小写**：`TaskStatus`（`"Ready"`）与
 *    `TagKind`（`"Context"`）**首字母大写**，与 schema 的 CHECK 逐字一致；
 *    只有 `ProjectStatus`（`active`）、`SessionState`（`running`）、
 *    `TimerKind`（`stopwatch`）是小写。写成全小写就是永远不命中的静默 bug。
 *
 * ## 枚举取值域为什么是 `as const` 数组
 *
 * `type X = (typeof X_VALUES)[number]` 让**联合类型与运行期数组同源**：
 * 类型层面不会与数组漂移，而运行期数组又能被测试拿去验证「快照里出现过的取值」。
 * 这一条是「改枚举串即红」能落到 `vitest run` 而不是只落到 `tsc` 的**唯一**办法
 * ——联合类型在运行期是被擦除的。
 */

// ─────────────────────────────────────────────────────────────────────────────
// 枚举取值域（唯一出处：Rust 侧的 `as_str()` / schema 的 CHECK）
// ─────────────────────────────────────────────────────────────────────────────

/** `task.status`（`src-tauri/src/domain/task.rs` 的 `TaskStatus::ALL`）。 */
export const TASK_STATUSES = [
  "Inbox",
  "Clarifying",
  "Ready",
  "Scheduled",
  "Doing",
  "Blocked",
  "Waiting",
  "Review",
  "Done",
  "Cancelled",
] as const;
export type TaskStatus = (typeof TASK_STATUSES)[number];

/** `project.status`（`src-tauri/src/domain/project.rs`）。**小写**。 */
export const PROJECT_STATUSES = ["active", "archived", "done"] as const;
export type ProjectStatus = (typeof PROJECT_STATUSES)[number];

/** `tag.kind`（`src-tauri/src/domain/tag.rs`）。**首字母大写**。 */
export const TAG_KINDS = ["Domain", "Activity", "Context", "Report"] as const;
export type TagKind = (typeof TAG_KINDS)[number];

/** `session.state`（`src-tauri/src/domain/session.rs`）。**小写**。 */
export const SESSION_STATES = [
  "running",
  "paused",
  "recovering",
  "finished",
  "discarded",
] as const;
export type SessionState = (typeof SESSION_STATES)[number];

/** `timer_kind`（会话的计时类型）。**小写**。 */
export const TIMER_KINDS = ["stopwatch", "countdown"] as const;
export type TimerKind = (typeof TIMER_KINDS)[number];

/** 会话模式。**全大写**；`start_timer` 的 `mode` 走它。 */
export const SESSION_MODES = [
  "FOREGROUND",
  "BACKGROUND",
  "PASSIVE",
  "WAITING",
] as const;
export type SessionMode = (typeof SESSION_MODES)[number];

/**
 * 恢复与历史的三个动作词（P8 Task 2a 的五条写命令）。
 *
 * 取值与 Rust 侧命令层的显式 `parse` 逐字一致（`src-tauri/src/commands/mod.rs` 的
 * `parse_reconcile_action` / `parse_reconcile_target_state` / `parse_correct_action` /
 * `parse_transition_cause`）——那边**不**用 serde 的枚举反序列化，所以取值域写错
 * 得到的是稳定错误码 `DOMAIN_ERROR`，不是 Tauri 的反序列化错误。
 *
 * 大小写不统一是**故意的**：`reconcile` 的两个词与审计里的
 * `reconcile:confirm` / `reconcile:discard_uncertain` 同词（小写），
 * 而 `target` 就是落库用的 `TaskStatus` 名（首字母大写）。
 */
export const RECONCILE_ACTIONS = ["confirm", "discard_uncertain"] as const;
export type ReconcileAction = (typeof RECONCILE_ACTIONS)[number];

/** 对账之后会话停在哪个状态。**只有两个**（`running`/`recovering`/`discarded` 都不是）。 */
export const RECONCILE_TARGET_STATES = ["paused", "finished"] as const;
export type ReconcileTargetState = (typeof RECONCILE_TARGET_STATES)[number];

/** 历史修正的动作。`retime` 必须带起止，`delete` 是软删除（作废，不 `DELETE`）。 */
export const CORRECT_ACTIONS = ["retime", "delete"] as const;
export type CorrectAction = (typeof CORRECT_ACTIONS)[number];

/**
 * 任务跃迁的原因。`reopen` 是终结态（`Done`/`Cancelled`）回到 `Ready` 的**唯一**
 * 合法原因：同一个「回 `Ready`」请求，`cause: "user"` 会被服务拒绝，`"reopen"` 才通过。
 */
export const TRANSITION_CAUSES = ["user", "reopen"] as const;
export type TransitionCause = (typeof TRANSITION_CAUSES)[number];

/**
 * 统计口径里的**三类分列**（`src-tauri/src/services/stats.rs` 的 `StatsClass`）。**小写**。
 *
 * 02 §6：已确认 / 实时暂计 / 待确认是三件**不同的事实**，**不得合并**，也没有
 * 「三类相加」这个入口。
 */
export const STATS_CLASSES = ["confirmed", "live", "pending"] as const;
export type StatsClass = (typeof STATS_CLASSES)[number];

/**
 * 统计口径里的 **measure 维度**（`src-tauri/src/services/stats.rs` 的 `Measure`）。**小写**。
 *
 * 固定四项、顺序与 Rust 侧 `Measure::ALL` 一致（消费方按 `class` + `measure` 取列，
 * 不按下标）。人工**只有** `human`（`FOREGROUND` 会话）：机器时长无论与人工并行多久
 * 都不并入人工；`waiting` 单列，谁也不并进谁（F-103）。
 */
export const MEASURES = [
  "human",
  "machine_background",
  "machine_passive",
  "waiting",
] as const;
export type Measure = (typeof MEASURES)[number];

/** 错误上下文里的受控实体种类（`src-tauri/src/error.rs` 的白名单，**小写**）。 */
export const AUTHORITY_KINDS = ["task", "session", "project", "tag"] as const;
export type AuthorityKind = (typeof AUTHORITY_KINDS)[number];

/** `AppError::code()` 的六个稳定码（00 §4）。前端只按它分支。 */
export const ERROR_CODES = [
  "DATA_EPOCH_MISMATCH",
  "VERSION_CONFLICT",
  "RECOVERY_REQUIRED",
  "DOMAIN_ERROR",
  "STORAGE_ERROR",
  "DATA_RESTORE_IN_PROGRESS",
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];

/** 事件信封里的两个事件名（`src-tauri/src/services/events.rs`）。 */
export const EVENT_DOMAIN_CHANGED = "domain.changed";
export const EVENT_TIMER_TICK = "timer.tick";

/**
 * 事件信封（00 §5）。字段就是这五个，不得增删。
 *
 * - `domain.changed` 的 `payload` **就是**发出该写命令的响应 DTO；
 * - `timer.tick` 的 `payload` **就是** {@link TimerSnapshot}。
 *
 * 载荷写成 `unknown` 而不是联合类型：它是**按 `event` 分支**才有形状的，
 * 给一个"看起来能直接读字段"的类型反而会诱导调用方不判 `event`。
 */
export interface EventEnvelope {
  data_epoch: string;
  event: string;
  revision: number;
  /** Unix 毫秒。 */
  at: number;
  payload: unknown;
}

// ─────────────────────────────────────────────────────────────────────────────
// 行类型（仓储的投影，直接出现在响应里，没有第二份 DTO）
// ─────────────────────────────────────────────────────────────────────────────

/** `project` 的一行。 */
export interface ProjectRow {
  id: string;
  name: string;
  /** V0.1 没有描述的写入口，读得到但恒为 `null`。 */
  description: string | null;
  row_version: number;
  status: ProjectStatus;
  created_at: number;
  updated_at: number;
}

/** `tag` 的一行。 */
export interface TagRow {
  id: string;
  kind: TagKind;
  name: string;
  /** V0.1 的标签是平铺的，恒为 `null`。 */
  parent_id: string | null;
  row_version: number;
  created_at: number;
}

/** `task` 的一行。 */
export interface TaskRow {
  id: string;
  project_id: string | null;
  title: string;
  status: TaskStatus;
  quality: string | null;
  row_version: number;
  created_at: number;
  updated_at: number;
}

/**
 * `work_session` 的一行（P8 Task 2a：恢复与历史的报告里直接装它）。
 *
 * 三个枚举字段都是**落库字符串**：`mode`（`FOREGROUND`…）、`state`（`running`…）、
 * `timer_kind`（`stopwatch`/`countdown`）。
 */
export interface SessionRow {
  id: string;
  task_id: string;
  /** 本次进程运行代次；对账之后会切到**当前** run（原始归属留在审计里）。 */
  run_id: string;
  mode: SessionMode;
  state: SessionState;
  timer_kind: TimerKind;
  /** 正计时恒为 `null`；倒计时才有正预算。 */
  target_duration_ms: number | null;
  started_at: number;
  ended_at: number | null;
  /** 会话级「待确认」标记；对账收尾会把它清假。 */
  needs_review: boolean;
  row_version: number;
}

/**
 * `work_interval` 的一行。
 *
 * 四个 `null` 分支各有含义，**不要**合并处理：
 * - `ended_at: null` = 终点未知（正在计时，或候选端点未定）；
 * - `duration_ms: null` = **没有已确认时长**（不是 0）；
 * - `voided_at` 非空 = 已作废（整次作废之后行还在，只是不计入任何工时）；
 * - `sampled_end_wall_at: null` 是**正常**分支：手工补录与用户确认都没有采样点，
 *   只有机器采样闭合的段才有。
 */
export interface IntervalRow {
  id: string;
  session_id: string;
  started_at: number;
  ended_at: number | null;
  voided_at: number | null;
  duration_ms: number | null;
  sampled_end_wall_at: number | null;
  needs_review: boolean;
}

// ─────────────────────────────────────────────────────────────────────────────
// 响应 DTO（21 份快照逐一对上）
// ─────────────────────────────────────────────────────────────────────────────

/** `get_revision`：只给库身份与版本，不含业务数据。 */
export interface RevisionSnapshot {
  data_epoch: string;
  revision: number;
}

/** `list_projects` / `list_selectable_projects`。 */
export interface ProjectList {
  items: ProjectRow[];
  data_epoch: string;
  revision: number;
}

/** `list_tags` / `tags_of_task`。 */
export interface TagList {
  items: TagRow[];
  data_epoch: string;
  revision: number;
}

/** `list_tasks`。 */
export interface TaskQueryResult {
  tasks: TaskRow[];
  /** 满足条件的总数，与分页窗口无关。 */
  total: number;
  data_epoch: string;
  revision: number;
}

/** `plan_for`。 */
export interface DailyPlanView {
  tasks: TaskRow[];
  data_epoch: string;
  revision: number;
}

/** 统计范围（半开 `[from, to)`，Unix 毫秒）。 */
export interface StatsRange {
  from: number;
  to: number;
}

/**
 * 一列统计结果（02 §6 的「每个结果 DTO」，五个口径字段齐全）。
 *
 * 取列按 `class` + `measure` 找（每一类固定四项），**不按下标**，也没有任何
 * 「相加」的入口。
 */
export interface MeasureColumn {
  /** 属于三类中的哪一类。 */
  class: StatsClass;
  /** 这一列报的是哪个 measure。 */
  measure: Measure;
  /** 这一列按哪个时区算（已归一）。 */
  timezone: string;
  /** 这一列覆盖的范围（半开）；日桶里是「该日真实日界 ∩ 报表范围」。 */
  range: StatsRange;
  /** 这一列的数字截至哪一刻（本次采样的归属挂钟 `A(M)`）。 */
  as_of: number;
  /** 事实来自哪个库身份。 */
  data_epoch: string;
  /** 事实来自哪个业务版本（与列里的数字同一次读事务）。 */
  revision: number;
  /**
   * 该列的毫秒数。
   *
   * **待确认列**：只有**已知端点**候选的跨度之和（零点长度的候选贡献 0）；这一 measure
   * 一条已知端点的候选都没有时才是 `null`——「终点未知不推算」，不是「整栏不给数」。
   * 零长度候选有已知端点但跨度 0，所以它计数、给 0。
   */
  ms: number | null;
  /**
   * 该列覆盖的区间条数。
   *
   * **待确认列**：与范围相交（**含零长度候选**与终点未知但已开始的候选）的未作废条数
   * ⇒ 判「有没有待确认」看它，**不要**看 `ms === null`（Ruling P5-12）。
   */
  intervals: number;
}

/**
 * 当前任务与运行状态（`TodayView.current`）。
 *
 * ⚠️ **有 `current` ≠ 正在计时**：它是协调器镜像里**最后装载**的那条会话，那条会话
 * **可能已经结束**（`state` 为 `finished` / `discarded`）。判「是否正在计时」必须看
 * `state === "running"` 与 `TodayView.live` 那一列的 `intervals`。
 */
export interface CurrentTask {
  /** 当前会话。暂停中的会话**也是**当前会话，只是没有开放区间。 */
  session_id: string;
  task_id: string;
  /**
   * 任务标题。
   *
   * 只有 id 的话界面渲染不出「当前任务」：没有「按 id 取任务」的读路径
   * （`list_tasks` 只按 status / project / context 筛），而当前任务未必在今天的列表里。
   */
  task_title: string;
  state: SessionState;
}

/**
 * `stats_today`（F-010）：一次返回五项，**分别显示、不预先相加**。
 *
 * 五项 = {@link TodayView.tasks}（今日选择列表）、{@link TodayView.current}
 * （当前任务与运行状态），以及三组里的 `human` 列（确认人工工时 / 运行暂计 /
 * 待确认时间）。**没有**合计字段：三段时间分属三类事实，加起来既不是工时也不是待办。
 *
 * 三组工时都固定四项（与 `Measure::ALL` 同序）；三组的 `as_of` / `revision` /
 * `data_epoch` 与视图本身**同源**（同一个读事务、同一次采样）——页面的本视图水位
 * （`viewWatermark`）就用后两个判旧，机器与等待因此不会跟人工出现两个水位。
 */
export interface TodayView {
  /** ① 今日选择列表（P4 的顺序）；**完成的任务保留在列表里**并带自己的状态。 */
  tasks: TaskRow[];
  /** ② 当前任务与运行状态；**没有装载过任何会话时为 `null`**。 */
  current: CurrentTask | null;
  /** ③ 已确认，固定四项。`human` 那一列就是 F-010 的「确认人工工时」。 */
  confirmed: MeasureColumn[];
  /** ④ 运行暂计：当前开放区间裁剪到今日，固定四项。 */
  live: MeasureColumn[];
  /** ⑤ 待确认，固定四项。有没有待确认看 `intervals`。 */
  pending: MeasureColumn[];
  /** 这个视图算的是哪一天：**查询时区**的本地日期（`YYYY-MM-DD`）。 */
  date: string;
  /** 已归一的查询时区。 */
  timezone: string;
  /** 今日的**真实**半开日界（夏令时切换日是 23 / 25 小时）。 */
  range: StatsRange;
  /** 这些数字截至哪一刻（同一次样本的归属终点 `A(M)`）。 */
  as_of: number;
  data_epoch: string;
  /** 这些事实来自哪个业务版本（与五项出自**同一个读事务**）。 */
  revision: number;
}

/**
 * `timer_snapshot` / `timer_tick` 的响应，也是 `timer.tick` 事件的载荷。
 *
 * `tick_seq` 在 Rust 侧是 `u64`：JS 安全整数以内（2^53）不会丢精度。
 */
export interface TimerSnapshot {
  data_epoch: string;
  revision: number;
  run_id: string;
  session_id: string | null;
  session_version: number | null;
  /**
   * 当前会话所属的任务；无会话时为 `null`。
   *
   * `resume_timer` 要 `task_id` + `task_expected_version`，而「这条会话属于哪个任务」
   * 没有第二条读路径（`TaskRow` 不带会话、24 条命令里没有 session→task 的查询）——
   * 冷启动（重开窗口）或托盘暂停之后，只有快照能给出任务身份。
   */
  task_id: string | null;
  /**
   * 任务行的并发版本（`task.row_version`）：直接填 `ResumeRequest.task_expected_version`。
   *
   * Rust 侧**每次采样重读**（暂停期间改任务会 bump 它），所以这里拿到的总是当前值。
   */
  task_row_version: number | null;
  /**
   * 任务标题（`task.title`）；无会话时为 `null`。
   *
   * **这是界面唯一的标题来源**：24 条命令里没有「按 id 取任务」的读路径（`list_tasks`
   * 只按 status / project / context 筛），所以冷启动或托盘暂停之后，计时页与状态栏的
   * 「当前任务」只能从这里取。与 `task_row_version` 同一时机：Rust 侧**每次采样重读**，
   * 暂停期间改标题下一拍就跟着变。
   */
  task_title: string | null;
  tick_seq: number;
  as_of: number;
  active_ms: number;
  pending_ms: number | null;
  state: SessionState | null;
  timer_kind: TimerKind | null;
  remaining_ms: number | null;
  overtime_ms: number | null;
}

/** `start_timer` / `pause_timer` / `resume_timer` / `finish_timer` 的响应。 */
export interface CommandOutcome {
  snapshot: TimerSnapshot;
  revision: number;
  /** 请求目标任务的提交后版本（快照可能指向另一条仍在运行的会话）。 */
  task_version: number;
}

/** `create_project` / `rename_project` / `archive_project`。 */
export interface ProjectChange {
  project: ProjectRow;
  revision: number;
  data_epoch: string;
}

/** `set_task_project`。 */
export interface TaskProjectChange {
  task: TaskRow;
  revision: number;
  data_epoch: string;
}

/** `create_task` / `clarify_ready`。 */
export interface TaskChange {
  task: TaskRow;
  revision: number;
  data_epoch: string;
}

/** `create_tag`。 */
export interface TagChange {
  tag: TagRow;
  revision: number;
  data_epoch: string;
}

/** `tag_task` / `untag_task`：这个任务**当前**的标签集合。 */
export interface TaskTagsChange {
  tags: TagRow[];
  revision: number;
  data_epoch: string;
}

/** `add_to_plan` / `remove_from_plan`。 */
export interface DailyPlanChange {
  tasks: TaskRow[];
  revision: number;
  data_epoch: string;
}

/**
 * `reconcile`（P8 Task 2a）：会话 + 该会话**全部**区间（不只是被处理的那几条）。
 *
 * `intervals` 里既有刚确认的段，也可能有更早被作废的段——「作废」是软删除，
 * 行还在（判「这段算不算工时」看 `voided_at` 与 `duration_ms`，不看它是否在列表里）。
 */
export interface ReconcileReport {
  session: SessionRow;
  intervals: IntervalRow[];
  revision: number;
  data_epoch: string;
}

/**
 * `correct` / `backfill` / `discard_session`（P8 Task 2a）：三条命令**共用一个形状**。
 *
 * 差别在语义与字段分支上，不在形状上：`correct` 返回被重定时/软删除的那条区间，
 * `backfill` 返回新建的会话与区间，`discard_session` 返回被作废的第一条区间
 * （`voided_at` 非空、会话 `discarded`）。
 */
export interface HistoryEditReport {
  session: SessionRow;
  interval: IntervalRow;
  revision: number;
  data_epoch: string;
}

/**
 * `transition_task`（P8 Task 2a）：任务行 + **同事务**的两个联动名单。
 *
 * 两个名单是**已经发生的事实**（谁被结束了、谁被暂停了），不是建议：
 * 空数组表示这次跃迁没有联动任何会话（例如已经是目标状态的幂等请求）。
 */
export interface TaskTransitionReport {
  task: TaskRow;
  ended_sessions: string[];
  paused_sessions: string[];
  revision: number;
  data_epoch: string;
}

/** 一个被请求目标的版本。 */
export interface RecordVersion {
  kind: AuthorityKind;
  /** 请求里给的那条 ID 照原样回显。 */
  id: string;
  /** `null` = **已显式确认不存在**（记录已被删除），不是"没请求"。 */
  row_version: number | null;
}

/** 失败响应的权威上下文（与请求一一对应）。 */
export interface ErrorAuthority {
  data_epoch: string;
  revision: number;
  records: RecordVersion[];
}

/**
 * 失败响应（`src-tauri/src/error.rs`）。
 *
 * 前端**只按 `code` 决定行为**（提示 / 重新握手 / 冲突刷新），文案取 `message`；
 * 未知 `code` 直接展示 `message`。`authority.records` 按 `kind` + `id` 匹配，
 * **不按下标**（白名单顺序 ≠ 请求顺序）。
 */
export interface ErrorResponse {
  code: string;
  message: string;
  authority: ErrorAuthority | null;
  requires_handshake: boolean;
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求 DTO（每条命令只收一个 `request` 参数）
// ─────────────────────────────────────────────────────────────────────────────

/** 只带库身份的请求。 */
export interface EpochRequest {
  expected_data_epoch: string;
}

/** `list_projects`。`status` 省略 / `null` = 不限制状态（归档与历史都在里面）。 */
export interface ListProjectsRequest {
  expected_data_epoch: string;
  status?: ProjectStatus | null;
}

/** `create_project`。 */
export interface CreateProjectRequest {
  expected_data_epoch: string;
  name: string;
}

/** `rename_project`。改既有对象 ⇒ 必须带项目版本。 */
export interface RenameProjectRequest {
  expected_data_epoch: string;
  project_id: string;
  expected_row_version: number;
  name: string;
}

/** `archive_project`。 */
export interface ArchiveProjectRequest {
  expected_data_epoch: string;
  project_id: string;
  expected_row_version: number;
}

/** `list_tags`。`kind` 省略 / `null` = 全部四类。 */
export interface ListTagsRequest {
  expected_data_epoch: string;
  kind?: TagKind | null;
}

/** `create_tag`。V0.1 没有层级：非空 `parent_id` 一律被服务拒绝。 */
export interface CreateTagRequest {
  expected_data_epoch: string;
  kind: TagKind;
  name: string;
  parent_id?: string | null;
}

/** `tag_task` / `untag_task`：一次一个标签、一个任务。 */
export interface TaskTagRequest {
  expected_data_epoch: string;
  task_id: string;
  tag_id: string;
}

/** `tags_of_task`（纯读）。 */
export interface TaskTagsRequest {
  expected_data_epoch: string;
  task_id: string;
}

/** `create_task`（F-002 的 Inbox 入口）。新建 ⇒ 只需 epoch。 */
export interface CreateTaskRequest {
  expected_data_epoch: string;
  title: string;
  project_id?: string | null;
}

/** `clarify_ready`。 */
export interface ClarifyReadyRequest {
  expected_data_epoch: string;
  task_id: string;
  expected_row_version: number;
}

/** 改任务归属的**二值**目标：`{"bind":"<id>"}` / `"clear"`（没有第三态）。 */
export type ProjectTarget = { bind: string } | "clear";

/** `set_task_project`。 */
export interface SetTaskProjectRequest {
  expected_data_epoch: string;
  task_id: string;
  expected_row_version: number;
  project: ProjectTarget;
}

/** `add_to_plan` / `remove_from_plan`。日期与时区是**原始输入**，由服务校验。 */
export interface PlanMutationRequest {
  expected_data_epoch: string;
  task_id: string;
  date: string;
  timezone: string;
}

/** `plan_for`。 */
export interface DailyPlanQuery {
  date: string;
  timezone: string;
  expected_data_epoch: string;
}

/**
 * `stats_today`。
 *
 * **不带日期**：「今天」由服务从**同一次样本的归属终点** `A(M)` 算，所以
 * `date` / `range` / `as_of` 三者天然同源——另传一个日期只会得到一份「数字是这一天的、
 * 口径字段是那一天」的视图。`timezone` 是原始输入，归一在服务入口。
 */
export interface TodayQuery {
  timezone: string;
  expected_data_epoch: string;
}

/**
 * 项目筛选的**三值**形状（D3 裁决）：`"any"` / `"none"` / `{"id":"<project_id>"}`。
 *
 * 三态不能压成 `Option<Option<String>>`——那样「不限制项目」与「只要没有项目的」
 * 在类型上长得一样。它与写路径的 {@link ProjectTarget} 是**两个**枚举。
 */
export type ProjectSelector = "any" | "none" | { id: string };

/** `list_tasks`。`statuses` 空集合 = 不限制状态。 */
export interface TaskQueryRequest {
  statuses: TaskStatus[];
  project: ProjectSelector;
  /** 情境（上下文）标签 id；必须是 `Context` 类，那条拒绝在服务入口。 */
  context_tag_id?: string | null;
  /** 每页条数，1..=100。越界由读路径拒绝。 */
  limit: number;
  /** 偏移，>= 0。 */
  offset: number;
  expected_data_epoch: string;
}

/**
 * `start_timer`。
 *
 * `expected_interval_ms` 只用于**识别挂起**（采样隔了多久没来），不是「多久记一次
 * 工时」：省略时 Rust 侧按本进程的采样节拍取值，客户端不该猜。
 * `target_duration_ms` 与 `timer_kind` 必须匹配（倒计时要有正预算、正计时必须没有），
 * 相反的组合由服务拒绝。
 */
export interface StartTimerRequest {
  expected_data_epoch: string;
  task_id: string;
  task_expected_version: number;
  mode: SessionMode;
  timer_kind: TimerKind;
  target_duration_ms?: number | null;
  expected_interval_ms?: number;
}

/** `pause_timer` / `finish_timer`。 */
export interface SessionRequest {
  expected_data_epoch: string;
  session_id: string;
  session_expected_version: number;
}

/** `resume_timer`。**两份版本**：任务与会话各自有自己的并发版本。 */
export interface ResumeRequest {
  expected_data_epoch: string;
  task_id: string;
  task_expected_version: number;
  session_id: string;
  session_expected_version: number;
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求 DTO：恢复与历史（P8 Task 2a 的五条写命令）
// ─────────────────────────────────────────────────────────────────────────────

/**
 * `ranges` 的一项：用户确认的一段区间。
 *
 * 起止是**用户给定的值**（不是候选端点推导出来的），`ended_at === started_at`
 * 的零长度是合法的（那表示「这一段确实没花时间」）。
 */
export interface ConfirmedRangeRequest {
  interval_id: string;
  started_at: number;
  ended_at: number;
}

/**
 * `reconcile`：确认或丢弃**该会话全部**待确认区间。`expected_row_version` 是会话版本。
 *
 * - `action: "confirm"` ⇒ `ranges` 必须**恰好覆盖**该会话的全部待确认区间
 *   （缺一条、多一条、指向别的会话/别的区间都整条拒绝；待确认集合为空时必须是空数组）；
 * - `action: "discard_uncertain"` ⇒ `ranges` 必须是空数组（作废哪些区间由服务从库里取）。
 *
 * 「作废整次」**不是**这条命令，是 {@link DiscardSessionRequest}：两个动作在界面上也不许
 * 合并成一个「丢弃」按钮。
 */
export interface ReconcileRequest {
  expected_data_epoch: string;
  session_id: string;
  expected_row_version: number;
  action: ReconcileAction;
  target_state: ReconcileTargetState;
  ranges: ConfirmedRangeRequest[];
}

/**
 * `correct`：重定时 / 软删除一条可信区间。只对 `state === "finished"` 的会话开放
 * （界面据会话 `state` 禁用入口，服务层也会拒）。
 *
 * `expected_row_version` 是**所属会话**的版本（区间没有独立版本列），**必填**：
 * 与其他四条写命令、以及 `expected_data_epoch` 同一口径——必填标量少了就是传输层
 * （serde）错误。**不要**为了凑过校验随便填一个数：服务拿这个版本做乐观并发校验，
 * 填错只会得到 `VERSION_CONFLICT`（刷新后重取真值）。
 *
 * `started_at` / `ended_at` / `reason` 三个键**可以省略**（Rust 侧是 `Option`，serde
 * 对缺键的 `Option` 一律填 `None`，省略因此等价于传 `null`）。
 */
export interface CorrectRequest {
  expected_data_epoch: string;
  session_id: string;
  expected_row_version: number;
  interval_id: string;
  action: CorrectAction;
  /** 只有 `retime` 用（那时必填）；`delete` 省略或传 `null`。 */
  started_at?: number | null;
  /** 只有 `retime` 用（那时必填）；`delete` 省略或传 `null`。 */
  ended_at?: number | null;
  /** 用户给的理由（落审计）；省略或 `null` = 没写理由。 */
  reason?: string | null;
}

/**
 * `backfill`：把一段**已经发生**的人工时间补录成一条 `finished` 会话。
 *
 * **独立入口**：不启动计时、不伪造完成事件。新建 ⇒ 只带 epoch（没有可校验的行版本）。
 * 固定字段（服务层写死）：`mode = FOREGROUND`、`timer_kind = stopwatch`、
 * `target_duration_ms = null`，`run_id` 取当前 run。
 */
export interface BackfillRequest {
  expected_data_epoch: string;
  task_id: string;
  started_at: number;
  ended_at: number;
}

/**
 * `discard_session`：作废整次（**全部**区间软作废 + 会话 `discarded`）。
 *
 * 无状态前置：`running` / `paused` / `recovering` / `finished` 都能作废；已经作废过的
 * 重复提交是幂等的（零写入、不加版本）。它与 {@link ReconcileRequest} 的
 * `discard_uncertain` **语义不同**（那条只丢待确认区间、保留可信前缀）。
 */
export interface DiscardSessionRequest {
  expected_data_epoch: string;
  session_id: string;
  expected_row_version: number;
}

/**
 * `transition_task`：任务跃迁 + 同事务联动它的会话（完成/取消结束，阻塞/等待暂停）。
 *
 * `target` 是落库用的状态名（`"Done"` / `"Blocked"` / `"Ready"`…，大小写敏感）；
 * 终结态回 `Ready` 必须带 `cause: "reopen"`。
 */
export interface TransitionTaskRequest {
  expected_data_epoch: string;
  task_id: string;
  expected_row_version: number;
  target: TaskStatus;
  cause: TransitionCause;
}
