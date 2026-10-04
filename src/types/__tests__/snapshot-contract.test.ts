/**
 * TS 类型 ↔ 快照的**机械检查**（P7 Task 1b 的验收项，计划 `:74`）。
 *
 * ## 为什么需要这一条
 *
 * `src-tauri/tests/ipc_snapshots.rs` 只钉住 **Rust ↔ JSON**：15 份快照与 Rust 类型
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
  PROJECT_STATUSES,
  SESSION_STATES,
  TAG_KINDS,
  TASK_STATUSES,
  TIMER_KINDS,
} from "../ipc";
import type {
  CommandOutcome,
  DailyPlanChange,
  DailyPlanView,
  ErrorAuthority,
  ErrorResponse,
  ProjectChange,
  ProjectList,
  ProjectRow,
  RecordVersion,
  RevisionSnapshot,
  TagChange,
  TagList,
  TagRow,
  TaskChange,
  TaskProjectChange,
  TaskQueryResult,
  TaskRow,
  TaskTagsChange,
  TimerSnapshot,
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

/** 15 个响应 DTO 的顶层键集合（照抄快照；顺序按字典序，便于与 `Object.keys().sort()` 对读）。 */
const TOP_LEVEL = {
  revision_snapshot: ["data_epoch", "revision"],
  project_list: ["data_epoch", "items", "revision"],
  tag_list: ["data_epoch", "items", "revision"],
  task_query_result: ["data_epoch", "revision", "tasks", "total"],
  daily_plan_view: ["data_epoch", "revision", "tasks"],
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
];

/** 编译期：嵌套对象的键集合断言（与上面同一形状）。 */
export type NestedKeyChecks = [
  Expect<Equal<(typeof PROJECT_ROW_KEYS)[number], keyof ProjectRow>>,
  Expect<Equal<(typeof TAG_ROW_KEYS)[number], keyof TagRow>>,
  Expect<Equal<(typeof TASK_ROW_KEYS)[number], keyof TaskRow>>,
  Expect<Equal<(typeof ERROR_AUTHORITY_KEYS)[number], keyof ErrorAuthority>>,
  Expect<Equal<(typeof RECORD_VERSION_KEYS)[number], keyof RecordVersion>>,
];

/** 嵌套对象的运行期比对表：`where` 是点分路径，`[]` 表示取数组第一项。 */
const NESTED: ReadonlyArray<{ where: string; keys: readonly string[] }> = [
  { where: "project_list.items[]", keys: PROJECT_ROW_KEYS },
  { where: "tag_list.items[]", keys: TAG_ROW_KEYS },
  { where: "task_query_result.tasks[]", keys: TASK_ROW_KEYS },
  { where: "daily_plan_view.tasks[]", keys: TASK_ROW_KEYS },
  { where: "command_outcome.snapshot", keys: TOP_LEVEL.timer_snapshot },
  { where: "project_change.project", keys: PROJECT_ROW_KEYS },
  { where: "task_project_change.task", keys: TASK_ROW_KEYS },
  { where: "task_change.task", keys: TASK_ROW_KEYS },
  { where: "tag_change.tag", keys: TAG_ROW_KEYS },
  { where: "task_tags_change.tags[]", keys: TAG_ROW_KEYS },
  { where: "daily_plan_change.tasks[]", keys: TASK_ROW_KEYS },
  { where: "error_response.authority", keys: ERROR_AUTHORITY_KEYS },
  { where: "error_response.authority.records[]", keys: RECORD_VERSION_KEYS },
];

// ─────────────────────────────────────────────────────────────────────────────
// 用例
// ─────────────────────────────────────────────────────────────────────────────

describe("快照契约", () => {
  it("快照集合与登记表逐份对上（多一份、少一份都红 —— 新增快照必须在 TOP_LEVEL 登记）", () => {
    // 精确集合比对，不是"至少 15 份"：`>=` 挡不住"第 16 份快照谁也没碰"——
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
