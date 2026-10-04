/**
 * IPC 契约的 TypeScript 形状（P7 Task 1b，**手写**，D1 裁决）。
 *
 * 权威在 Rust 侧，两处：
 *
 * - 请求形状：`src-tauri/src/commands/mod.rs` 的请求 DTO（每条命令只收一个
 *   `request` 参数，字段名就是那些结构体的 snake_case 字段名）；
 * - 响应形状：仓库根 `src/types/__snapshots__/*.json`（15 份，由
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

/** 错误上下文里的受控实体种类（`src-tauri/src/error.rs` 的白名单，**小写**）。 */
export const AUTHORITY_KINDS = ["task", "session", "project", "tag"] as const;
export type AuthorityKind = (typeof AUTHORITY_KINDS)[number];

/** `AppError::code()` 的五个稳定码（00 §4）。前端只按它分支。 */
export const ERROR_CODES = [
  "DATA_EPOCH_MISMATCH",
  "VERSION_CONFLICT",
  "RECOVERY_REQUIRED",
  "DOMAIN_ERROR",
  "STORAGE_ERROR",
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

// ─────────────────────────────────────────────────────────────────────────────
// 响应 DTO（15 份快照逐一对上）
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
