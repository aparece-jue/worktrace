/**
 * 页面用例的**假后端**（P7 Task 3 建立，Task 5 扩到项目/标签/筛选查询，P8 Task 1b 扩到
 * 今日页，P8 Task 2c 扩到恢复页与历史页）：按 `src/types/ipc.ts` 的契约形状回应命令。
 *
 * 它不是业务实现，只是一个可脚本化的替身——**业务规则仍在 Rust**，所以默认行为尽量
 * 贴近真实服务（`create_task` 回一个新任务、`clarify_ready` 把状态改成 Ready 并 +1 版本、
 * `start_timer` 把状态改到 Doing 并给出**提交后**的任务版本、项目改名/归档校验
 * `expected_row_version`、`list_selectable_projects` 只回 active、`add_to_plan` /
 * `remove_from_plan` 改 `view.tasks` 并对重复动作保持幂等），个别用例再用
 * `fail`（模拟服务拒绝）、`hold`（某条命令整体挂起）与 `holdNext`（只挂起**下一次**调用，
 * 用于"旧响应晚到"这类竞态）覆盖。
 *
 * 记账三件：`commands`（发过哪些命令，按顺序）、`requests`（每条命令的入参）、
 * `count(name)`。用例的"只发一次"就是数这里的。
 *
 * ⚠️ 三条**刻意不重实现**的服务端行为，别把它们当遗漏：
 * - `list_tasks` **不按 `statuses` / `project` / `context_tag_id` 过滤**，只按窗口切片。
 *   页面级用例要断的是「条件进了同一条请求」与「显示的是响应里的东西」，重实现一遍
 *   服务端筛选只会让替身自己成为被测对象（真实的交集在 `task_repo::filter_clause`）。
 * - `list_projects` / `list_tags` 只按 `status` / `kind` 过滤，其余照原样回。
 * - 恢复/历史的四条写命令（`reconcile` / `correct` / `backfill` / `discard_session`）
 *   只回**形状正确**的报告并推一版 `revision`：`reconcile` 的"ranges 必须恰好覆盖"、
 *   `discard_session` 的"全部区间软作废"这些判据都在 Rust（`services/recovery.rs` /
 *   `history.rs`），替身重实现一遍就等于把服务端的规则抄成第二份。要断"作废掉的那条
 *   从列表里消失了"，用 `attention` / `history` 夹具在写回之后换一份**新夹具**表达
 *   （与今日页用 `backend.view = …` 造新版同一姿势）。
 */

import { mockIPC } from "@tauri-apps/api/mocks";

import {
  MEASURES,
  type AttentionOverview,
  type CommandOutcome,
  type DailyPlanChange,
  type ErrorResponse,
  type HistoryDetail,
  type HistoryView,
  type IntervalRow,
  type Measure,
  type MeasureColumn,
  type PendingIntervalItem,
  type ProjectChange,
  type ProjectRow,
  type SessionAttentionItem,
  type SessionRow,
  type StatsClass,
  type TagList,
  type TagRow,
  type TaskRow,
  type TimeEdit,
  type TimerSnapshot,
  type TodayView,
} from "../../types/ipc";

export const EPOCH = "epoch-a";
export const RUN = "run-1";
export const SESSION = "session-1";
export const AT = 1_700_000_000_000;

/** 今日视图的默认口径：上海的一天（`2026-10-03`，整 24 小时的半开区间）。 */
export const TODAY_DATE = "2026-10-03";
export const TODAY_ZONE = "Asia/Shanghai";
const TODAY_FROM = Date.UTC(2026, 9, 2, 16, 0, 0);
const TODAY_TO = TODAY_FROM + 86_400_000;

/** 一条任务行（默认是一条刚捕获进来的 Inbox 任务）。 */
export function task(overrides: Partial<TaskRow> = {}): TaskRow {
  return {
    id: "task-1",
    project_id: null,
    title: "写周报",
    status: "Inbox",
    quality: null,
    row_version: 0,
    created_at: AT,
    updated_at: AT,
    ...overrides,
  };
}

/** 一个项目行（默认 active）。 */
export function project(overrides: Partial<ProjectRow> = {}): ProjectRow {
  return {
    id: "project-1",
    name: "项目甲",
    description: null,
    row_version: 0,
    status: "active",
    created_at: AT,
    updated_at: AT,
    ...overrides,
  };
}

/** 一个标签行（默认 `Context` 类）。 */
export function tag(overrides: Partial<TagRow> = {}): TagRow {
  return {
    id: "tag-1",
    kind: "Context",
    name: "办公室",
    parent_id: null,
    row_version: 0,
    created_at: AT,
    ...overrides,
  };
}

/** 没有活动会话的快照（`TimerSnapshot::idle` 的形状）。 */
export function idleSnapshot(revision = 5): TimerSnapshot {
  return {
    data_epoch: EPOCH,
    revision,
    run_id: RUN,
    session_id: null,
    session_version: null,
    task_id: null,
    task_row_version: null,
    task_title: null,
    tick_seq: 1,
    as_of: AT,
    active_ms: 0,
    pending_ms: null,
    state: null,
    timer_kind: null,
    remaining_ms: null,
    overtime_ms: null,
  };
}

/** 一份正在计时的正计时会话快照。 */
export function runningSnapshot(overrides: Partial<TimerSnapshot> = {}): TimerSnapshot {
  return {
    data_epoch: EPOCH,
    revision: 5,
    run_id: RUN,
    session_id: SESSION,
    session_version: 1,
    task_id: "task-1",
    task_row_version: 1,
    task_title: "写周报",
    tick_seq: 1,
    as_of: AT,
    active_ms: 1_000,
    pending_ms: null,
    state: "running",
    timer_kind: "stopwatch",
    remaining_ms: null,
    overtime_ms: null,
    ...overrides,
  };
}

/**
 * 一列统计结果（默认：已确认的人工、0 毫秒、当天口径）。
 *
 * `class` / `measure` 是这一列的**身份**，所以默认值只是"最常用的一格"；要别的格子就
 * 显式给（`measureGroup` 就是这么拼的）。
 */
export function column(overrides: Partial<MeasureColumn> = {}): MeasureColumn {
  return {
    class: "confirmed",
    measure: "human",
    timezone: TODAY_ZONE,
    range: { from: TODAY_FROM, to: TODAY_TO },
    as_of: AT,
    data_epoch: EPOCH,
    revision: 5,
    ms: 0,
    intervals: 0,
    ...overrides,
  };
}

/**
 * 一组固定四项的列（次序与 `MEASURES` 一致，也就是 Rust 侧 `Measure::ALL` 的次序）。
 *
 * `values` 里没给的 measure 用 `0`——真实服务对"库里就是没有区间"正是这么答的；
 * 要表达"未给数"（待确认列一条已知端点的候选都没有）就显式传 `null`（**不是**省略）。
 */
export function measureGroup(
  cls: StatsClass,
  values: Partial<Record<Measure, number | null>> = {},
  intervals: Partial<Record<Measure, number>> = {},
): MeasureColumn[] {
  return MEASURES.map((measure) => {
    const given = values[measure];
    return column({
      class: cls,
      measure,
      ms: given === undefined ? 0 : given,
      intervals: intervals[measure] ?? 0,
    });
  });
}

/**
 * 一份今日视图（`stats_today` 的响应，五项齐全）。
 *
 * 默认是"这一天库里什么都没有"：三组列各四项、数字 0，待确认一条候选都没有
 * （`ms: null` + `intervals: 0`，与 `MeasureColumn.ms` 的口径注释一致）。
 */
export function todayView(overrides: Partial<TodayView> = {}): TodayView {
  return {
    tasks: [],
    current: null,
    confirmed: measureGroup("confirmed"),
    live: measureGroup("live"),
    pending: measureGroup("pending", { human: null }),
    date: TODAY_DATE,
    timezone: TODAY_ZONE,
    range: { from: TODAY_FROM, to: TODAY_TO },
    as_of: AT,
    data_epoch: EPOCH,
    revision: 5,
    ...overrides,
  };
}

/** 一条会话行（默认：一条已结束的正计时人工会话）。 */
export function session(overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    id: SESSION,
    task_id: "task-1",
    run_id: RUN,
    mode: "FOREGROUND",
    state: "finished",
    timer_kind: "stopwatch",
    target_duration_ms: null,
    started_at: AT,
    ended_at: AT + 3_600_000,
    needs_review: false,
    row_version: 1,
    ...overrides,
  };
}

/** 一条区间行（默认：10 分钟、已确认、未作废、无采样点）。 */
export function interval(overrides: Partial<IntervalRow> = {}): IntervalRow {
  return {
    id: "interval-1",
    session_id: SESSION,
    started_at: AT,
    ended_at: AT + 600_000,
    voided_at: null,
    duration_ms: 600_000,
    sampled_end_wall_at: null,
    needs_review: false,
    ...overrides,
  };
}

/** 一条审计行（默认：一次 `reconcile` 确认）。 */
export function timeEdit(overrides: Partial<TimeEdit> = {}): TimeEdit {
  return {
    id: "edit-1",
    session_id: SESSION,
    before_json: '{"change":"reconcile_confirm","session":{}}',
    after_json: '{"change":"reconcile_confirm","session":{}}',
    reason: "reconcile:confirm",
    created_at: AT,
    ...overrides,
  };
}

/**
 * 一条待确认候选区间（默认：**候选终点已知但时长未确认**，`duration_ms: null`）。
 *
 * 这正是"已知单调时长只作候选"的形状：候选端点是材料，不是事实。
 */
export function pendingInterval(overrides: Partial<PendingIntervalItem> = {}): PendingIntervalItem {
  return {
    id: "pending-1",
    started_at: AT,
    ended_at: AT + 120_000,
    duration_ms: null,
    sampled_end_wall_at: null,
    needs_review: true,
    ...overrides,
  };
}

/** 一条需要处理的会话（默认：`recovering` + 一条待确认候选）。 */
export function attentionItem(overrides: Partial<SessionAttentionItem> = {}): SessionAttentionItem {
  return {
    session_id: SESSION,
    task_id: "task-1",
    state: "recovering",
    run_id: RUN,
    is_current_run: false,
    attention: "needs_review",
    intervals: [pendingInterval()],
    fault_reason: null,
    session_row_version: 1,
    session_needs_review: true,
    ...overrides,
  };
}

/** 一份待确认概览（`attention_overview` 的响应；默认：一张空表）。 */
export function attentionOverview(overrides: Partial<AttentionOverview> = {}): AttentionOverview {
  return {
    items: [],
    pending_intervals: 0,
    pending_sessions: 0,
    fault_sessions: 0,
    data_epoch: EPOCH,
    revision: 5,
    ...overrides,
  };
}

/** 一个会话的详情（`history_view.selected`；默认：一条已结束会话 + 一条区间 + 一条审计）。 */
export function historyDetail(overrides: Partial<HistoryDetail> = {}): HistoryDetail {
  return {
    session: session(),
    intervals: [interval()],
    edits: [timeEdit()],
    ...overrides,
  };
}

/** 一页历史（`history_view` 的响应；默认：一页空表、没有详情）。 */
export function historyView(overrides: Partial<HistoryView> = {}): HistoryView {
  return {
    sessions: [],
    selected: null,
    data_epoch: EPOCH,
    revision: 5,
    ...overrides,
  };
}

/** 一个可手工放行的响应（`holdNext` 交回它）。 */
export interface Deferred<T = unknown> {
  promise: Promise<T>;
  /** 交回这条响应（形状按对应命令的契约）。 */
  resolve(value: T): void;
  /** 让这次调用失败（`ErrorResponse` 形状，走真实的错误通路）。 */
  reject(cause: unknown): void;
}

/** 假后端：可脚本化的返回值 + 记账。 */
export interface Backend {
  /** 按顺序记下每一条命令名。 */
  commands: string[];
  /** 每条命令的入参（`{ request }` 里那个）。 */
  requests: Array<{ command: string; request: unknown }>;
  /** `list_tasks` 交回的任务（**不筛条件**，只按窗口切片，见模块头）。 */
  tasks: TaskRow[];
  /** `list_projects` / `list_selectable_projects` 交回的项目。 */
  projects: ProjectRow[];
  /** `list_tags` 交回的标签。 */
  tags: TagRow[];
  /** 当前业务版本：读写响应都报它（写命令会 +1）。 */
  revision: number;
  /** `timer_snapshot` 交回的快照（命令会在它上面推状态）。 */
  snapshot: TimerSnapshot;
  /** `stats_today` 交回的今日视图（五项）；`data_epoch` / `revision` 由假后端统一给。 */
  view: TodayView;
  /** `attention_overview` 交回的待确认概览；`data_epoch` / `revision` 由假后端统一给。 */
  attention: AttentionOverview;
  /** `history_view` 交回的那一页（`sessions` 按窗口切片，`selected` 只在指名时给）。 */
  history: HistoryView;
  /** `accept_detected_clock_correction` 交回的 `accepted`（默认 `false` = 没有待接受的校正）。 */
  clockAccepted: boolean;
  /** 命令名 ⇒ 要抛出的失败响应（模拟服务拒绝）。 */
  fail: Record<string, ErrorResponse>;
  /** 命令名 ⇒ 响应要挂着的 promise（模拟"这条命令还在飞"）。 */
  hold: Record<string, Promise<unknown>>;
  /** 某个命令被调用了几次。 */
  count(command: string): number;
  /** 让某条命令的**下一次**调用挂起（精确到调用序号，见 `holdNext` 的实现）。 */
  holdNext<T = unknown>(command: string): Deferred<T>;
}

/** 一次 `ErrorResponse` 形状的失败。 */
export function failure(overrides: Partial<ErrorResponse> = {}): ErrorResponse {
  return {
    code: "DOMAIN_ERROR",
    message: "领域规则拒绝。",
    authority: null,
    requires_handshake: false,
    ...overrides,
  };
}

/** 装上 `mockIPC` 并交回这个假后端。 */
export function createBackend(): Backend {
  /** 按调用顺序排队的挂起调用（`holdNext` 往里塞）。 */
  const pending = new Map<string, Array<Deferred>>();

  const backend: Backend = {
    commands: [],
    requests: [],
    tasks: [],
    projects: [],
    tags: [],
    revision: 5,
    snapshot: idleSnapshot(),
    view: todayView(),
    attention: attentionOverview(),
    history: historyView(),
    clockAccepted: false,
    fail: {},
    hold: {},
    count: (command) => backend.commands.filter((name) => name === command).length,
    holdNext<T>(command: string): Deferred<T> {
      let resolve!: (value: T) => void;
      let reject!: (cause: unknown) => void;
      const promise = new Promise<T>((settle, fail) => {
        resolve = settle;
        reject = fail;
      });
      const queue = pending.get(command) ?? [];
      queue.push({ promise, resolve, reject } as Deferred);
      pending.set(command, queue);
      return { promise, resolve, reject };
    },
  };

  mockIPC(async (command: string, payload: unknown) => {
    backend.commands.push(command);
    const request = (payload as { request?: unknown } | undefined)?.request;
    backend.requests.push({ command, request });
    if (command in backend.fail) throw backend.fail[command];
    const queued = pending.get(command);
    if (queued !== undefined && queued.length > 0) return queued.shift()!.promise;
    if (command in backend.hold) return backend.hold[command];

    /** 项目写命令的版本守卫（真实服务在 `project_repo` 里判，用例可以据此驱动冲突）。 */
    const requireVersion = (projectId: string, expected: number): ProjectRow => {
      const row = backend.projects.find((item) => item.id === projectId);
      if (row === undefined) {
        throw failure({ code: "DOMAIN_ERROR", message: "项目不存在。" });
      }
      if (row.row_version !== expected) {
        throw failure({ code: "VERSION_CONFLICT", message: "项目已被别处改动，请刷新后重试。" });
      }
      return row;
    };

    switch (command) {
      case "get_revision":
        return { data_epoch: EPOCH, revision: backend.revision };
      case "timer_snapshot":
        return backend.snapshot;
      case "list_tasks": {
        const { limit, offset } = request as { limit: number; offset: number };
        return {
          // 只按窗口切片：条件的交集在服务端算，替身不重实现（见模块头）。
          tasks: backend.tasks.slice(offset, offset + limit),
          total: backend.tasks.length,
          data_epoch: EPOCH,
          revision: backend.revision,
        };
      }
      case "list_selectable_projects":
        return {
          // 与 `catalog::list_selectable_projects` 同义：**只回 active**。
          items: backend.projects.filter((row) => row.status === "active"),
          data_epoch: EPOCH,
          revision: backend.revision,
        };
      case "list_projects": {
        const status = (request as { status?: string | null }).status ?? null;
        return {
          items:
            status === null
              ? backend.projects
              : backend.projects.filter((row) => row.status === status),
          data_epoch: EPOCH,
          revision: backend.revision,
        };
      }
      case "list_tags": {
        const kind = (request as { kind?: string | null }).kind ?? null;
        const items = kind === null ? backend.tags : backend.tags.filter((row) => row.kind === kind);
        return { items, data_epoch: EPOCH, revision: backend.revision } satisfies TagList;
      }
      case "create_project": {
        const created = project({
          id: `project-${backend.projects.length + 1}`,
          name: (request as { name: string }).name,
        });
        backend.projects = [...backend.projects, created];
        backend.revision += 1;
        return {
          project: created,
          revision: backend.revision,
          data_epoch: EPOCH,
        } satisfies ProjectChange;
      }
      case "rename_project": {
        const { project_id, expected_row_version, name } = request as {
          project_id: string;
          expected_row_version: number;
          name: string;
        };
        const before = requireVersion(project_id, expected_row_version);
        backend.revision += 1;
        // 与 `project_repo::rename_project` 同一条幂等规则：同名 ⇒ 零写入（版本不动）。
        if (before.name === name) {
          return { project: before, revision: backend.revision, data_epoch: EPOCH };
        }
        const renamed = project({ ...before, name, row_version: before.row_version + 1 });
        backend.projects = backend.projects.map((row) => (row.id === renamed.id ? renamed : row));
        return { project: renamed, revision: backend.revision, data_epoch: EPOCH };
      }
      case "archive_project": {
        const { project_id, expected_row_version } = request as {
          project_id: string;
          expected_row_version: number;
        };
        const before = requireVersion(project_id, expected_row_version);
        // 归档**不删任务、不动历史**：只改 `projects` 里这一行的状态与版本。
        const archived = project({ ...before, status: "archived", row_version: before.row_version + 1 });
        backend.projects = backend.projects.map((row) => (row.id === archived.id ? archived : row));
        backend.revision += 1;
        return { project: archived, revision: backend.revision, data_epoch: EPOCH };
      }
      case "create_task": {
        const created = task({
          id: `task-${backend.tasks.length + 1}`,
          title: (request as { title: string }).title,
          project_id: (request as { project_id?: string | null }).project_id ?? null,
        });
        backend.tasks = [...backend.tasks, created];
        return { task: created, revision: 6, data_epoch: EPOCH };
      }
      case "clarify_ready": {
        const before = backend.tasks.find((row) => row.id === (request as { task_id: string }).task_id);
        const changed = task({ ...before, status: "Ready", row_version: (before?.row_version ?? 0) + 1 });
        backend.tasks = backend.tasks.map((row) => (row.id === changed.id ? changed : row));
        return { task: changed, revision: 6, data_epoch: EPOCH };
      }
      case "stats_today":
        // 五项与 `data_epoch` / `revision` 同源：假后端统一给这两个字段，用例改
        // `backend.revision` 就能造出"更新的一版"。`date` / `timezone` / `range` 来自夹具。
        return { ...backend.view, data_epoch: EPOCH, revision: backend.revision };
      case "attention_overview":
        return { ...backend.attention, data_epoch: EPOCH, revision: backend.revision };
      case "history_view": {
        const { limit, offset, session_id } = request as {
          limit: number;
          offset: number;
          session_id?: string | null;
        };
        return {
          // 与 `session_repo::history_sessions` 同形：只按分页窗口切片（筛选在服务端）。
          sessions: backend.history.sessions.slice(offset, offset + limit),
          // `session_id` 省略 / `null` = 只要列表；给了就给那条详情（替身不判它存不存在）。
          selected: session_id === undefined || session_id === null ? null : backend.history.selected,
          data_epoch: EPOCH,
          revision: backend.revision,
        } satisfies HistoryView;
      }
      case "reconcile": {
        const { session_id } = request as { session_id: string };
        const item = backend.attention.items.find((row) => row.session_id === session_id);
        backend.revision += 1;
        // 写响应只保证**形状**（业务判据在 Rust，见模块头）：会话推成 finished、区间照给。
        return {
          session: session({ id: session_id, task_id: item?.task_id ?? "task-1" }),
          intervals: [interval({ session_id })],
          revision: backend.revision,
          data_epoch: EPOCH,
        };
      }
      case "correct": {
        const { interval_id, session_id } = request as {
          interval_id: string;
          session_id: string;
        };
        backend.revision += 1;
        return {
          session: session({ id: session_id }),
          interval: interval({ id: interval_id, session_id }),
          revision: backend.revision,
          data_epoch: EPOCH,
        };
      }
      case "backfill": {
        const { task_id } = request as { task_id: string };
        backend.revision += 1;
        return {
          session: session({ id: "session-backfilled", task_id }),
          interval: interval({ id: "interval-backfilled", session_id: "session-backfilled" }),
          revision: backend.revision,
          data_epoch: EPOCH,
        };
      }
      case "discard_session": {
        const { session_id } = request as { session_id: string };
        backend.revision += 1;
        // 作废整次：会话 `discarded`、区间 `voided_at` 非空（报告形状见 `HistoryEditReport`）。
        return {
          session: session({ id: session_id, state: "discarded", needs_review: false }),
          interval: interval({ session_id, voided_at: AT + 3_600_000, duration_ms: null }),
          revision: backend.revision,
          data_epoch: EPOCH,
        };
      }
      case "retry_recovery":
        // 恢复重试：返回提交后的权威快照（不保证推进 `revision`，所以版本照原样给）。
        return backend.snapshot;
      case "accept_detected_clock_correction": {
        // `accepted: false` 是**正常路径**（没有待接受的校正、零写入、不加版本）。
        if (backend.clockAccepted) backend.revision += 1;
        return {
          accepted: backend.clockAccepted,
          data_epoch: EPOCH,
          revision: backend.revision,
        };
      }
      case "add_to_plan": {
        const taskId = (request as { task_id: string }).task_id;
        const added = backend.tasks.find((row) => row.id === taskId);
        if (added === undefined) {
          throw failure({ code: "DOMAIN_ERROR", message: "任务不存在。" });
        }
        // 重复加入 ⇒ 幂等（`daily_plan::add_to_plan` 同一条规则）：版本不动。
        if (!backend.view.tasks.some((row) => row.id === taskId)) {
          backend.view = { ...backend.view, tasks: [...backend.view.tasks, added] };
          backend.revision += 1;
        }
        return {
          tasks: backend.view.tasks,
          revision: backend.revision,
          data_epoch: EPOCH,
        } satisfies DailyPlanChange;
      }
      case "remove_from_plan": {
        const taskId = (request as { task_id: string }).task_id;
        const kept = backend.view.tasks.filter((row) => row.id !== taskId);
        // 本来就不在今日列表里 ⇒ 幂等：版本不动。
        if (kept.length !== backend.view.tasks.length) {
          backend.view = { ...backend.view, tasks: kept };
          backend.revision += 1;
        }
        return {
          tasks: backend.view.tasks,
          revision: backend.revision,
          data_epoch: EPOCH,
        } satisfies DailyPlanChange;
      }
      case "start_timer": {
        const before = backend.tasks.find((row) => row.id === (request as { task_id: string }).task_id);
        // `Inbox → Ready → Doing` 两步在 Rust 的同一个事务里：版本 +2。
        const changed = task({ ...before, status: "Doing", row_version: (before?.row_version ?? 0) + 2 });
        backend.tasks = backend.tasks.map((row) => (row.id === changed.id ? changed : row));
        backend.snapshot = runningSnapshot({ session_version: 1, tick_seq: 1 });
        return {
          snapshot: backend.snapshot,
          revision: 6,
          task_version: changed.row_version,
        } satisfies CommandOutcome;
      }
      case "pause_timer":
        backend.snapshot = {
          ...backend.snapshot,
          state: "paused",
          session_version: (backend.snapshot.session_version ?? 1) + 1,
        };
        return { snapshot: backend.snapshot, revision: 7, task_version: 2 } satisfies CommandOutcome;
      case "resume_timer":
        backend.snapshot = {
          ...backend.snapshot,
          state: "running",
          session_version: (backend.snapshot.session_version ?? 1) + 1,
        };
        return { snapshot: backend.snapshot, revision: 8, task_version: 2 } satisfies CommandOutcome;
      case "finish_timer":
        backend.snapshot = idleSnapshot(9);
        return { snapshot: backend.snapshot, revision: 9, task_version: 2 } satisfies CommandOutcome;
      default:
        throw new Error(`这条用例没有脚本化命令 ${command}`);
    }
  });

  return backend;
}
