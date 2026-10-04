/**
 * 页面用例的**假后端**（P7 Task 3）：按 `src/types/ipc.ts` 的契约形状回应命令。
 *
 * 它不是业务实现，只是一个可脚本化的替身——**业务规则仍在 Rust**，所以默认行为尽量
 * 贴近真实服务（`create_task` 回一个新任务、`clarify_ready` 把状态改成 Ready 并 +1 版本、
 * `start_timer` 把状态改到 Doing 并给出**提交后**的任务版本），个别用例再用 `fail`
 * （模拟服务拒绝）与 `hold`（模拟命令还在飞）覆盖。
 *
 * 记账三件：`commands`（发过哪些命令，按顺序）、`requests`（每条命令的入参）、
 * `count(name)`。用例的"只发一次"就是数这里的。
 */

import { mockIPC } from "@tauri-apps/api/mocks";

import type {
  CommandOutcome,
  ErrorResponse,
  ProjectRow,
  TaskRow,
  TimerSnapshot,
} from "../../types/ipc";

export const EPOCH = "epoch-a";
export const RUN = "run-1";
export const SESSION = "session-1";
export const AT = 1_700_000_000_000;

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

/** 没有活动会话的快照（`TimerSnapshot::idle` 的形状）。 */
export function idleSnapshot(revision = 5): TimerSnapshot {
  return {
    data_epoch: EPOCH,
    revision,
    run_id: RUN,
    session_id: null,
    session_version: null,
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

/** 假后端：可脚本化的返回值 + 记账。 */
export interface Backend {
  /** 按顺序记下每一条命令名。 */
  commands: string[];
  /** 每条命令的入参（`{ request }` 里那个）。 */
  requests: Array<{ command: string; request: unknown }>;
  /** `list_tasks` 交回的任务。 */
  tasks: TaskRow[];
  /** `list_selectable_projects` 交回的项目。 */
  projects: ProjectRow[];
  /** `timer_snapshot` 交回的快照（命令会在它上面推状态）。 */
  snapshot: TimerSnapshot;
  /** 命令名 ⇒ 要抛出的失败响应（模拟服务拒绝）。 */
  fail: Record<string, ErrorResponse>;
  /** 命令名 ⇒ 响应要挂着的 promise（模拟"这条命令还在飞"）。 */
  hold: Record<string, Promise<unknown>>;
  /** 某个命令被调用了几次。 */
  count(command: string): number;
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
  const backend: Backend = {
    commands: [],
    requests: [],
    tasks: [],
    projects: [],
    snapshot: idleSnapshot(),
    fail: {},
    hold: {},
    count: (command) => backend.commands.filter((name) => name === command).length,
  };

  mockIPC(async (command: string, payload: unknown) => {
    backend.commands.push(command);
    const request = (payload as { request?: unknown } | undefined)?.request;
    backend.requests.push({ command, request });
    if (command in backend.fail) throw backend.fail[command];
    if (command in backend.hold) return backend.hold[command];

    switch (command) {
      case "get_revision":
        return { data_epoch: EPOCH, revision: 5 };
      case "timer_snapshot":
        return backend.snapshot;
      case "list_tasks":
        return {
          tasks: backend.tasks,
          total: backend.tasks.length,
          data_epoch: EPOCH,
          revision: 5,
        };
      case "list_selectable_projects":
        return { items: backend.projects, data_epoch: EPOCH, revision: 5 };
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
