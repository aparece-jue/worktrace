/**
 * TS 类型 ↔ 快照的**机械检查**（P7 Task 1b 的验收项，计划 `:74`）。
 *
 * ## 为什么需要这一条
 *
 * `src-tauri/tests/ipc_snapshots.rs` 只钉住 **Rust ↔ JSON**：27 份快照与 Rust 类型
 * 逐字节一致。手写的 `src/types/ipc.ts` 与那些快照之间**没有**机械联系——没有这条
 * 用例，「改了 Rust 类型忘了改前端类型」仍然只能靠人记得（D1 放弃 DTO 生成器之后的
 * 替代承诺就是这两条用例）。
 *
 * ## 两条断言各自钉住什么
 *
 * 1. **键集合**：每个响应 DTO 的键集合（含嵌套的行类型、`authority.records`）必须与
 *    快照逐字相同。编译期的 `Equal<..., keyof T>` 保证"声明的键集合 = TS 接口的键集合"，
 *    运行期的 `toEqual` 保证"声明的键集合 = 快照里的键集合"；两条串起来，
 *    接口与快照之间就没有缝。
 * 2. **字面量联合**：枚举一律是**字符串字面量联合**，且由 `src/types/ipc.ts` 里的
 *    `as const` 数组推导（联合类型在运行期被擦除，数组是它在运行期的唯一影子）。
 *    这里把快照里**出现过的**那些取值收上来，逐个检查落在对应的取值域里——所以把
 *    `"Ready"` 写成 `"ready"`、把 `"Context"` 写成 `"context"` 都会**当场变红**
 *    （`vitest run` 就红，不用等 `tsc`）。
 *
 * ## 诚实声明（这条用例盖不住什么）
 *
 * 快照里只出现**样例值**（例如 `task.status` 只有 `"Ready"`），所以「Rust 新增了一个
 * 枚举变体」在它进入某份快照之前，这里看不出来。覆盖整个取值域的那一半靠
 * `src/types/ipc.ts` 里的 `as const` 数组与 Rust 的 `ALL` 常量人工对齐——
 * 这是"没有生成器"的已知代价，不是遗漏。
 */

import { describe, expect, it } from "vitest";

import {
  AUTHORITY_KINDS,
  ERROR_CODES,
  MEASURES,
  PROJECT_STATUSES,
  RECONCILE_ACTIONS,
  RECONCILE_TARGET_STATES,
  CORRECT_ACTIONS,
  SESSION_ATTENTIONS,
  SESSION_MODES,
  SESSION_STATES,
  STATS_CLASSES,
  TAG_KINDS,
  TASK_STATUSES,
  TIMER_KINDS,
  TRANSITION_CAUSES,
} from "../ipc";
import type {
  AttentionOverview,
  BackupResult,
  ClockCorrectionAccepted,
  CommandOutcome,
  CurrentTask,
  DailyPlanChange,
  DailyPlanView,
  ErrorAuthority,
  ErrorResponse,
  ExportResult,
  HistoryDetail,
  HistoryEditReport,
  HistoryView,
  IntervalRow,
  MeasureColumn,
  PendingIntervalItem,
  ProjectChange,
  ProjectList,
  ProjectRow,
  ReconcileReport,
  RecordVersion,
  RestoreResult,
  RevisionSnapshot,
  SessionAttentionItem,
  SessionRow,
  StatsRange,
  TagChange,
  TagList,
  TagRow,
  TaskChange,
  TaskProjectChange,
  TaskQueryResult,
  TaskRow,
  TaskTagsChange,
  TaskTransitionReport,
  TimeEdit,
  TimerSnapshot,
  TodayView,
} from "../ipc";

// ─────────────────────────────────────────────────────────────────────────────
// 读快照：整目录 glob。**加载**新快照不需要改这个文件，**覆盖**它需要——
// 下面的集合断言会把没在 `TOP_LEVEL`/`NESTED` 里登记的快照判红。
// ─────────────────────────────────────────────────────────────────────────────

const MODULES = import.meta.glob<Record<string, unknown>>("../__snapshots__/*.json", {
  eager: true,
  import: "default",
});

/** 文件名（去掉目录与 `.json`）→ 快照对象。 */
const SNAPSHOTS: Record<string, Record<string, unknown>> = {};
for (const [path, value] of Object.entries(MODULES)) {
  SNAPSHOTS[path.slice(path.lastIndexOf("/") + 1, -".json".length)] = value;
}

function snapshot(name: string): Record<string, unknown> {
  const value = SNAPSHOTS[name];
  if (value === undefined) {
    throw new Error(`快照 ${name}.json 不存在；现有：${Object.keys(SNAPSHOTS).sort().join(", ")}`);
  }
  return value;
}

/** 按点分路径取值，`[]` 后缀表示取数组的第一项（`a.b[].c`）。 */
function at(root: unknown, path: string): unknown {
  let node = root;
  for (const step of path.split(".")) {
    const isList = step.endsWith("[]");
    const key = isList ? step.slice(0, -2) : step;
    const record = node as Record<string, unknown>;
    node = isList ? (record[key] as unknown[])[0] : record[key];
  }
  return node;
}

function keysOf(value: unknown): string[] {
  return Object.keys(value as object).sort();
}

function sorted(keys: readonly string[]): string[] {
  return [...keys].sort();
}

// ─────────────────────────────────────────────────────────────────────────────
// 键集合：声明 + 编译期与 TS 接口对齐
// ─────────────────────────────────────────────────────────────────────────────

type Equal<A, B> =
  (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type Expect<T extends true> = T;

/** 27 个响应 DTO 的顶层键集合（照抄快照；顺序按字典序，便于与 `Object.keys().sort()` 对读）。 */
const TOP_LEVEL = {
  revision_snapshot: ["data_epoch", "revision"],
  project_list: ["data_epoch", "items", "revision"],
  tag_list: ["data_epoch", "items", "revision"],
  task_query_result: ["data_epoch", "revision", "tasks", "total"],
  daily_plan_view: ["data_epoch", "revision", "tasks"],
  today_view: [
    "as_of",
    "confirmed",
    "current",
    "data_epoch",
    "date",
    "live",
    "pending",
    "range",
    "revision",
    "tasks",
    "timezone",
  ],
  timer_snapshot: [
    "active_ms",
    "as_of",
    "data_epoch",
    "overtime_ms",
    "pending_ms",
    "remaining_ms",
    "revision",
    "run_id",
    "session_id",
    "session_version",
    "state",
    "task_id",
    "task_row_version",
    "task_title",
    "tick_seq",
    "timer_kind",
  ],
  timer_snapshot_idle: [
    "active_ms",
    "as_of",
    "data_epoch",
    "overtime_ms",
    "pending_ms",
    "remaining_ms",
    "revision",
    "run_id",
    "session_id",
    "session_version",
    "state",
    "task_id",
    "task_row_version",
    "task_title",
    "tick_seq",
    "timer_kind",
  ],
  command_outcome: ["revision", "snapshot", "task_version"],
  project_change: ["data_epoch", "project", "revision"],
  task_project_change: ["data_epoch", "revision", "task"],
  task_change: ["data_epoch", "revision", "task"],
  tag_change: ["data_epoch", "revision", "tag"],
  task_tags_change: ["data_epoch", "revision", "tags"],
  daily_plan_change: ["data_epoch", "revision", "tasks"],
  error_response: ["authority", "code", "message", "requires_handshake"],
  // P8 Task 2a：恢复与历史的五条写命令。**只有三个响应类型**——`correct` /
  // `backfill` / `discard_session` 共用 `HistoryEditReport`，三条各一份快照是因为
  // 它们钉的是不同分支（照 `timer_snapshot` / `timer_snapshot_idle` 的先例）。
  reconcile_report: ["data_epoch", "intervals", "revision", "session"],
  correct_report: ["data_epoch", "interval", "revision", "session"],
  backfill_report: ["data_epoch", "interval", "revision", "session"],
  discard_session_report: ["data_epoch", "interval", "revision", "session"],
  task_transition_report: [
    "data_epoch",
    "ended_sessions",
    "paused_sessions",
    "revision",
    "task",
  ],
  // P8 Task 2b：恢复读取 / 重试与历史读取（命令 6/8/13）。命令 7 `retry_recovery` 的
  // 响应**就是** `TimerSnapshot`（已有两份快照），所以这里没有它自己的那一条。
  clock_correction_accepted: ["accepted", "data_epoch", "revision"],
  attention_overview: [
    "data_epoch",
    "fault_sessions",
    "items",
    "pending_intervals",
    "pending_sessions",
    "revision",
  ],
  history_view: ["data_epoch", "revision", "selected", "sessions"],
  // P8 Task 3a：导出 / 备份 / 恢复（命令 9/10/11）。三个形状的键集合相同、语义不同：
  // 导出与备份**不改业务事实**（信封就是当时的权威值），恢复的 `revision` 是**锁内
  // 冻结**的那一个（可以低于恢复前那个值——换库了），`applied` 成功时恒为 true。
  export_result: ["bytes", "data_epoch", "path", "revision"],
  backup_result: ["bytes", "data_epoch", "path", "revision"],
  restore_result: ["applied", "data_epoch", "revision"],
} as const;

/** 嵌套对象（行类型与错误上下文）的键集合。 */
const PROJECT_ROW_KEYS = [
  "created_at",
  "description",
  "id",
  "name",
  "row_version",
  "status",
  "updated_at",
] as const;
const TAG_ROW_KEYS = [
  "created_at",
  "id",
  "kind",
  "name",
  "parent_id",
  "row_version",
] as const;
const TASK_ROW_KEYS = [
  "created_at",
  "id",
  "project_id",
  "quality",
  "row_version",
  "status",
  "title",
  "updated_at",
] as const;
const ERROR_AUTHORITY_KEYS = ["data_epoch", "records", "revision"] as const;
const RECORD_VERSION_KEYS = ["id", "kind", "row_version"] as const;
/** Today 的三组工时列（`MeasureColumn`）：口径字段必须与视图本身同源。 */
const MEASURE_COLUMN_KEYS = [
  "as_of",
  "class",
  "data_epoch",
  "intervals",
  "measure",
  "ms",
  "range",
  "revision",
  "timezone",
] as const;
const STATS_RANGE_KEYS = ["from", "to"] as const;
const CURRENT_TASK_KEYS = ["session_id", "state", "task_id", "task_title"] as const;
/** `work_session` 的一行（P8 Task 2a：恢复与历史的报告里直接装它）。 */
const SESSION_ROW_KEYS = [
  "ended_at",
  "id",
  "mode",
  "needs_review",
  "row_version",
  "run_id",
  "started_at",
  "state",
  "target_duration_ms",
  "task_id",
  "timer_kind",
] as const;
/** `work_interval` 的一行。四个 `null` 分支各有含义，一个都不能少。 */
const INTERVAL_ROW_KEYS = [
  "duration_ms",
  "ended_at",
  "id",
  "needs_review",
  "sampled_end_wall_at",
  "session_id",
  "started_at",
  "voided_at",
] as const;
/** `time_edit` 的一行（P8 Task 2b：历史详情带审计）。两个 JSON 列是**字符串**。 */
const TIME_EDIT_KEYS = [
  "after_json",
  "before_json",
  "created_at",
  "id",
  "reason",
  "session_id",
] as const;
/** `attention_overview` 的列表项（P8 Task 2b）。`attention` 是 `SessionAttention`。 */
const SESSION_ATTENTION_ITEM_KEYS = [
  "attention",
  "fault_reason",
  "intervals",
  "is_current_run",
  "run_id",
  "session_id",
  "session_needs_review",
  "session_row_version",
  "state",
  "task_id",
] as const;
/** 一条待确认区间（列表项的子项）：没有 `session_id` / `voided_at`（父项与集合口径）。 */
const PENDING_INTERVAL_ITEM_KEYS = [
  "duration_ms",
  "ended_at",
  "id",
  "needs_review",
  "sampled_end_wall_at",
  "started_at",
] as const;
/** `history_view.selected`：会话 + **全部**区间 + **全部**审计。 */
const HISTORY_DETAIL_KEYS = ["edits", "intervals", "session"] as const;

/**
 * 编译期：声明的键集合**恰好**是 TS 接口的键集合（多一个、少一个、改个名都不行）。
 *
 * 导出是为了 `noUnusedLocals`——这几个别名本身就是断言，没有运行期用处。
 */
export type TopLevelKeyChecks = [
  Expect<Equal<(typeof TOP_LEVEL)["revision_snapshot"][number], keyof RevisionSnapshot>>,
  Expect<Equal<(typeof TOP_LEVEL)["project_list"][number], keyof ProjectList>>,
  Expect<Equal<(typeof TOP_LEVEL)["tag_list"][number], keyof TagList>>,
  Expect<Equal<(typeof TOP_LEVEL)["task_query_result"][number], keyof TaskQueryResult>>,
  Expect<Equal<(typeof TOP_LEVEL)["daily_plan_view"][number], keyof DailyPlanView>>,
  Expect<Equal<(typeof TOP_LEVEL)["today_view"][number], keyof TodayView>>,
  Expect<Equal<(typeof TOP_LEVEL)["timer_snapshot"][number], keyof TimerSnapshot>>,
  Expect<Equal<(typeof TOP_LEVEL)["timer_snapshot_idle"][number], keyof TimerSnapshot>>,
  Expect<Equal<(typeof TOP_LEVEL)["command_outcome"][number], keyof CommandOutcome>>,
  Expect<Equal<(typeof TOP_LEVEL)["project_change"][number], keyof ProjectChange>>,
  Expect<Equal<(typeof TOP_LEVEL)["task_project_change"][number], keyof TaskProjectChange>>,
  Expect<Equal<(typeof TOP_LEVEL)["task_change"][number], keyof TaskChange>>,
  Expect<Equal<(typeof TOP_LEVEL)["tag_change"][number], keyof TagChange>>,
  Expect<Equal<(typeof TOP_LEVEL)["task_tags_change"][number], keyof TaskTagsChange>>,
  Expect<Equal<(typeof TOP_LEVEL)["daily_plan_change"][number], keyof DailyPlanChange>>,
  Expect<Equal<(typeof TOP_LEVEL)["error_response"][number], keyof ErrorResponse>>,
  Expect<Equal<(typeof TOP_LEVEL)["reconcile_report"][number], keyof ReconcileReport>>,
  // 三条命令共用 `HistoryEditReport`：三份快照各自与**同一个** TS 类型对上。
  Expect<Equal<(typeof TOP_LEVEL)["correct_report"][number], keyof HistoryEditReport>>,
  Expect<Equal<(typeof TOP_LEVEL)["backfill_report"][number], keyof HistoryEditReport>>,
  Expect<Equal<(typeof TOP_LEVEL)["discard_session_report"][number], keyof HistoryEditReport>>,
  Expect<Equal<(typeof TOP_LEVEL)["task_transition_report"][number], keyof TaskTransitionReport>>,
  // P8 Task 2b：三个新响应类型（命令 7 复用 `TimerSnapshot`，不另登记一份）。
  Expect<
    Equal<(typeof TOP_LEVEL)["clock_correction_accepted"][number], keyof ClockCorrectionAccepted>
  >,
  Expect<Equal<(typeof TOP_LEVEL)["attention_overview"][number], keyof AttentionOverview>>,
  Expect<Equal<(typeof TOP_LEVEL)["history_view"][number], keyof HistoryView>>,
  // P8 Task 3a：三个新响应类型（导出 / 备份 / 恢复）。
  Expect<Equal<(typeof TOP_LEVEL)["export_result"][number], keyof ExportResult>>,
  Expect<Equal<(typeof TOP_LEVEL)["backup_result"][number], keyof BackupResult>>,
  Expect<Equal<(typeof TOP_LEVEL)["restore_result"][number], keyof RestoreResult>>,
];

/** 编译期：嵌套对象的键集合断言（与上面同一形状）。 */
export type NestedKeyChecks = [
  Expect<Equal<(typeof PROJECT_ROW_KEYS)[number], keyof ProjectRow>>,
  Expect<Equal<(typeof TAG_ROW_KEYS)[number], keyof TagRow>>,
  Expect<Equal<(typeof TASK_ROW_KEYS)[number], keyof TaskRow>>,
  Expect<Equal<(typeof ERROR_AUTHORITY_KEYS)[number], keyof ErrorAuthority>>,
  Expect<Equal<(typeof RECORD_VERSION_KEYS)[number], keyof RecordVersion>>,
  Expect<Equal<(typeof MEASURE_COLUMN_KEYS)[number], keyof MeasureColumn>>,
  Expect<Equal<(typeof STATS_RANGE_KEYS)[number], keyof StatsRange>>,
  Expect<Equal<(typeof CURRENT_TASK_KEYS)[number], keyof CurrentTask>>,
  Expect<Equal<(typeof SESSION_ROW_KEYS)[number], keyof SessionRow>>,
  Expect<Equal<(typeof INTERVAL_ROW_KEYS)[number], keyof IntervalRow>>,
  // P8 Task 2b：审计行、恢复列表项（含子项）与历史详情。
  Expect<Equal<(typeof TIME_EDIT_KEYS)[number], keyof TimeEdit>>,
  Expect<Equal<(typeof SESSION_ATTENTION_ITEM_KEYS)[number], keyof SessionAttentionItem>>,
  Expect<Equal<(typeof PENDING_INTERVAL_ITEM_KEYS)[number], keyof PendingIntervalItem>>,
  Expect<Equal<(typeof HISTORY_DETAIL_KEYS)[number], keyof HistoryDetail>>,
];

/** 嵌套对象的运行期比对表：`where` 是点分路径，`[]` 表示取数组第一项。 */
const NESTED: ReadonlyArray<{ where: string; keys: readonly string[] }> = [
  { where: "project_list.items[]", keys: PROJECT_ROW_KEYS },
  { where: "tag_list.items[]", keys: TAG_ROW_KEYS },
  { where: "task_query_result.tasks[]", keys: TASK_ROW_KEYS },
  { where: "daily_plan_view.tasks[]", keys: TASK_ROW_KEYS },
  { where: "today_view.tasks[]", keys: TASK_ROW_KEYS },
  { where: "today_view.current", keys: CURRENT_TASK_KEYS },
  { where: "today_view.confirmed[]", keys: MEASURE_COLUMN_KEYS },
  { where: "today_view.live[]", keys: MEASURE_COLUMN_KEYS },
  { where: "today_view.pending[]", keys: MEASURE_COLUMN_KEYS },
  { where: "today_view.range", keys: STATS_RANGE_KEYS },
  { where: "command_outcome.snapshot", keys: TOP_LEVEL.timer_snapshot },
  { where: "project_change.project", keys: PROJECT_ROW_KEYS },
  { where: "task_project_change.task", keys: TASK_ROW_KEYS },
  { where: "task_change.task", keys: TASK_ROW_KEYS },
  { where: "tag_change.tag", keys: TAG_ROW_KEYS },
  { where: "task_tags_change.tags[]", keys: TAG_ROW_KEYS },
  { where: "daily_plan_change.tasks[]", keys: TASK_ROW_KEYS },
  { where: "error_response.authority", keys: ERROR_AUTHORITY_KEYS },
  { where: "error_response.authority.records[]", keys: RECORD_VERSION_KEYS },
  // ── P8 Task 2a：恢复与历史（三条命令共用 `HistoryEditReport`） ──────────────
  { where: "reconcile_report.session", keys: SESSION_ROW_KEYS },
  { where: "reconcile_report.intervals[]", keys: INTERVAL_ROW_KEYS },
  { where: "correct_report.session", keys: SESSION_ROW_KEYS },
  { where: "correct_report.interval", keys: INTERVAL_ROW_KEYS },
  { where: "backfill_report.session", keys: SESSION_ROW_KEYS },
  { where: "backfill_report.interval", keys: INTERVAL_ROW_KEYS },
  { where: "discard_session_report.session", keys: SESSION_ROW_KEYS },
  { where: "discard_session_report.interval", keys: INTERVAL_ROW_KEYS },
  { where: "task_transition_report.task", keys: TASK_ROW_KEYS },
  // ── P8 Task 2b：恢复读取与历史读取（命令 8/13） ────────────────────────────
  { where: "attention_overview.items[]", keys: SESSION_ATTENTION_ITEM_KEYS },
  { where: "attention_overview.items[].intervals[]", keys: PENDING_INTERVAL_ITEM_KEYS },
  { where: "history_view.sessions[]", keys: SESSION_ROW_KEYS },
  { where: "history_view.selected", keys: HISTORY_DETAIL_KEYS },
  { where: "history_view.selected.session", keys: SESSION_ROW_KEYS },
  { where: "history_view.selected.intervals[]", keys: INTERVAL_ROW_KEYS },
  { where: "history_view.selected.edits[]", keys: TIME_EDIT_KEYS },
];

// ─────────────────────────────────────────────────────────────────────────────
// 用例
// ─────────────────────────────────────────────────────────────────────────────

describe("快照契约", () => {
  it("快照集合与登记表逐份对上（多一份、少一份都红 —— 新增快照必须在 TOP_LEVEL 登记）", () => {
    // 精确集合比对，不是"至少若干份"：`>=` 挡不住"多出来的那一份快照谁也没碰"——
    // 契约文件加了、登记表没加，那条断言照样绿（评审 I2）。
    expect(Object.keys(SNAPSHOTS).sort()).toEqual(Object.keys(TOP_LEVEL).sort());
  });

  it("运行期：每个响应 DTO 的顶层键集合与快照逐字相同", () => {
    const failures: string[] = [];
    for (const [name, expected] of Object.entries(TOP_LEVEL)) {
      const actual = keysOf(snapshot(name));
      if (actual.join(",") !== sorted(expected).join(",")) {
        failures.push(`${name}: 类型声明 ${sorted(expected).join(",")} ≠ 快照 ${actual.join(",")}`);
      }
    }
    expect(failures, failures.join("\n")).toEqual([]);
  });

  it("运行期：嵌套对象（行类型与错误上下文）的键集合与快照逐字相同", () => {
    const failures: string[] = [];
    for (const { where, keys } of NESTED) {
      const [name, path] = where.split(/\.(.+)/);
      const actual = keysOf(at(snapshot(name), path));
      if (actual.join(",") !== sorted(keys).join(",")) {
        failures.push(`${where}: 类型声明 ${sorted(keys).join(",")} ≠ 快照 ${actual.join(",")}`);
      }
    }
    expect(failures, failures.join("\n")).toEqual([]);
  });

  it("运行期：快照里出现过的枚举取值都在 TS 的取值域里", () => {
    // 同一字段名可能落在两个取值域里：`status` 既是 TaskStatus 也是 ProjectStatus，
    // `kind` 既是 TagKind 也是 AuthorityKind（错误上下文）。这不是含糊，是真实的两个域。
    const domains: ReadonlyArray<{ field: string; domain: readonly string[]; label: string }> = [
      { field: "status", domain: [...TASK_STATUSES, ...PROJECT_STATUSES], label: "TaskStatus | ProjectStatus" },
      { field: "kind", domain: [...TAG_KINDS, ...AUTHORITY_KINDS], label: "TagKind | AuthorityKind" },
      { field: "state", domain: SESSION_STATES, label: "SessionState" },
      // P8 Task 2a 起会话行进快照 ⇒ `mode` 首次出现，取值域必须一起登记。
      { field: "mode", domain: SESSION_MODES, label: "SessionMode" },
      // P8 Task 2b 起恢复列表项进快照 ⇒ `attention` 首次出现（它不落库，取值域只在
      // Rust 的 `SessionAttention::as_str()` 定义一次）。
      { field: "attention", domain: SESSION_ATTENTIONS, label: "SessionAttention" },
      { field: "class", domain: STATS_CLASSES, label: "StatsClass" },
      { field: "measure", domain: MEASURES, label: "Measure" },
      { field: "timer_kind", domain: TIMER_KINDS, label: "TimerKind" },
      { field: "code", domain: ERROR_CODES, label: "ErrorCode" },
    ];

    const failures: string[] = [];
    for (const { field, domain, label } of domains) {
      const observed = collectStrings(Object.values(SNAPSHOTS), field);
      // 空集合等于没检查——这条下限是防"用例自己空过"的。
      if (observed.length === 0) {
        failures.push(`快照里一个 ${field} 都没有，这条断言会空过`);
        continue;
      }
      for (const value of observed) {
        if (!domain.includes(value)) {
          failures.push(`${field}=${JSON.stringify(value)} 不在 ${label} 里`);
        }
      }
    }
    expect(failures, failures.join("\n")).toEqual([]);
  });

  /**
   * P8 Task 2a 的四个**请求**动作词（`reconcile` / `reconcile.target_state` /
   * `correct` / `transition_task.cause`）。
   *
   * ⚠️ **这半是手抄的对读，不是生成器**：请求形状没有快照（快照只覆盖响应），
   * 所以它挡的是「顺手改了 `ipc.ts` 的取值域」这一类改动，**挡不住** Rust 与 TS 的
   * 漂移。权威在 `src-tauri/src/commands/mod.rs` 的四个 `parse_*`（显式解析，不走
   * serde 的枚举反序列化）；那边逐词的成功/失败用例在
   * `src-tauri/tests/ipc_commands.rs`（`confirm`、`discard_uncertain`、`retime`、
   * 以及 `user` / `reopen` 在终结态回 `Ready` 上的区别）。
   */
  it("写命令的四个动作词与 Rust 的显式 parse 对读（手抄的一半，理由见注释）", () => {
    expect({
      reconcile_action: [...RECONCILE_ACTIONS],
      reconcile_target_state: [...RECONCILE_TARGET_STATES],
      correct_action: [...CORRECT_ACTIONS],
      transition_cause: [...TRANSITION_CAUSES],
    }).toEqual({
      reconcile_action: ["confirm", "discard_uncertain"],
      reconcile_target_state: ["paused", "finished"],
      correct_action: ["retime", "delete"],
      transition_cause: ["user", "reopen"],
    });
  });
});

/** 递归收集某个字段名下的所有字符串取值（任意深度、穿透数组）。 */
function collectStrings(node: unknown, field: string): string[] {
  const found: string[] = [];
  const walk = (current: unknown): void => {
    if (Array.isArray(current)) {
      for (const item of current) walk(item);
      return;
    }
    if (typeof current !== "object" || current === null) return;
    for (const [key, value] of Object.entries(current)) {
      if (key === field && typeof value === "string") found.push(value);
      walk(value);
    }
  };
  walk(node);
  return found;
}
