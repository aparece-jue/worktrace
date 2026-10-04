/**
 * 双窗口同步实验台（P7 Task 6a）：**两个互不共享内存的 JS 上下文** + 一个**共用的假后端**。
 *
 * ⚠️ **这是实验用的注入手段，不是生产 API。** 它只活在 `__tests__/` 下，只有测试会
 * import 它；生产代码（`src/ipc.ts`、`src/state/domainState.ts`）**一个签名都没为它改过**
 * ——实验跑的就是那两个生产模块本身，替身只落在它们本来就有的两个边界上：
 *
 * | 边界 | 替身 | 为什么不改生产代码 |
 * | --- | --- | --- |
 * | Tauri 的 `listen` | `syncLabBus.ts`（`vi.mock` 装进本用例文件的模块图） | 生产代码本来就只通过这个函数订阅；改它等于测试替身，不是改协议 |
 * | `invoke`（IPC） | `@tauri-apps/api/mocks` 的 `mockIPC` | 官方提供，Task 5 的页面用例已在用 |
 *
 * ## 这个实验证明什么、不证明什么
 *
 * - **证明**：00 §5 规则 1–4 那套规则在**两个各自独立的上下文**里各自成立——A 写之后
 *   B 会失效并重新取数、旧响应不覆盖新状态、末次通知丢了仍会在 30 秒校验周期内收敛、
 *   跳号/乱序取新快照。每个上下文各有**自己那把**水位闸门与自己的展示值。
 * - **不证明**：真实双 WebView 的**广播时序**（Tauri 把一条事件投给两个 WebView 的先后与
 *   可靠性）。那需要真机：集成测试进程里没有事件循环，也没有第二个 WebView。
 *   步骤与记录模板见 `src-tauri/tests/manual-sync.md`。
 *
 * ## 两个上下文是真的"互不共享内存"吗
 *
 * 是：`createLabWindow` 每次调用 `createDomainState(...)`，模块级的 `domainState` 单例
 * **根本没被碰过**（它是"每个 JS 上下文一个"的那一份；两个窗口各自加载一次模块，
 * 所以实验里显式建两份）。两个上下文之间唯一的通路是假后端（进程侧的真相）与事件通道
 * （`syncLabBus` 的广播），也就是真实系统里仅有的那两条。
 */

import { mockIPC, clearMocks } from "@tauri-apps/api/mocks";
import { vi } from "vitest";

import { createViewWatermark } from "../../components/viewWatermark";
import {
  EVENT_DOMAIN_CHANGED,
  type CommandOutcome,
  type EventEnvelope,
  type RevisionSnapshot,
  type TaskChange,
  type TaskQueryResult,
  type TaskRow,
  type TimerSnapshot,
} from "../../types/ipc";
import {
  createTask,
  getRevision,
  listTasks,
  pauseTimer,
  startEventSession,
  timerSnapshot,
  type VersionStamp,
} from "../../ipc";
import { createDomainState, type DomainState } from "../domainState";
import { transport, type Delivery, type EventTransport } from "./syncLabBus";

// 契约样例的取值（与 `src/types/__snapshots__/*.json` 同风格；本文件自成一份，
// 不复用页面用例的 `fakeBackend.ts`——那份的记账与硬编码版本号是为页面用例定的）。
export const EPOCH = "epoch-a";
export const RUN = "run-1";
export const SESSION = "session-1";
export const TASK = "task-1";
export const AT = 1_700_000_000_000;
/** 起始业务版本（两条窗口握手后都在这一版上）。 */
export const START_REVISION = 5;

function taskRow(overrides: Partial<TaskRow> = {}): TaskRow {
  return {
    id: TASK,
    project_id: null,
    title: "写周报",
    status: "Ready",
    quality: null,
    row_version: 0,
    created_at: AT,
    updated_at: AT,
    ...overrides,
  };
}

function sessionSnapshot(overrides: Partial<TimerSnapshot> = {}): TimerSnapshot {
  return {
    data_epoch: EPOCH,
    revision: START_REVISION,
    run_id: RUN,
    session_id: SESSION,
    session_version: 1,
    task_id: TASK,
    task_row_version: 0,
    task_title: "写周报",
    tick_seq: 1,
    as_of: AT,
    active_ms: 60_000,
    pending_ms: null,
    state: "running",
    timer_kind: "stopwatch",
    remaining_ms: null,
    overtime_ms: null,
    ...overrides,
  };
}

/**
 * 假后端：**两个窗口共用的那一个进程真相**（`app_meta.revision` + 业务行 + 计时）。
 *
 * 它不是业务实现：筛选、合法性、事务都在 Rust，这里只按 `src/types/ipc.ts` 的响应形状
 * 回话，并且**只做两件真实服务一定会做的事**——写命令把 `revision` +1、`timer_snapshot`
 * 报**当前**版本（真机里那条快照与库身份出自同一次读，报旧版本会让水位数不动）。
 */
export interface LabBackend {
  readonly epoch: string;
  /** 当前业务版本（`app_meta.revision`）。 */
  revision(): number;
  /** 业务真相：任务行。 */
  rows(): TaskRow[];
  /** 业务真相：计时快照（`revision` 字段总等于当前版本）。 */
  timer(): TimerSnapshot;
  /** 收到过的命令名（按顺序）。 */
  readonly commands: string[];
  /** 某个命令被调用过几次。 */
  count(command: string): number;
  /** 最后一次失败响应的文案（没有失败时为 `null`）。 */
  lastError(): string | null;
}

function failure(code: string, message: string): unknown {
  return { code, message, authority: null, requires_handshake: false };
}

/** 建一个假后端并把它装到 IPC 边界上（`mockIPC`）。 */
export function createLabBackend(): LabBackend {
  const epoch = EPOCH;
  let revision = START_REVISION;
  let rows: TaskRow[] = [taskRow()];
  let timer: TimerSnapshot = sessionSnapshot();
  const commands: string[] = [];
  let lastError: string | null = null;

  mockIPC(async (command, payload) => {
    commands.push(command);
    const request = (payload as { request?: Record<string, unknown> } | undefined)?.request;
    /** 写命令的 epoch 守卫：真实服务在事务里先校验 `expected_data_epoch`。 */
    const requireEpoch = (): void => {
      if (request?.expected_data_epoch !== epoch) {
        throw failure("DATA_EPOCH_MISMATCH", "数据库身份已变化，请重新握手。");
      }
    };
    try {
      switch (command) {
        case "get_revision":
          return { data_epoch: epoch, revision } satisfies RevisionSnapshot;
        case "timer_snapshot":
          // 报**当前**版本：与真机一致（快照与库身份同一读事务）。
          return { ...timer, revision } satisfies TimerSnapshot;
        case "list_tasks": {
          const limit = (request?.limit as number | undefined) ?? 20;
          const offset = (request?.offset as number | undefined) ?? 0;
          return {
            tasks: rows.slice(offset, offset + limit),
            total: rows.length,
            data_epoch: epoch,
            revision,
          };
        }
        case "create_task": {
          requireEpoch();
          const created = taskRow({
            id: `task-${rows.length + 1}`,
            title: String(request?.title ?? ""),
            status: "Inbox",
          });
          rows = [...rows, created];
          revision += 1;
          return { task: created, revision, data_epoch: epoch } satisfies TaskChange;
        }
        case "pause_timer": {
          requireEpoch();
          revision += 1;
          timer = {
            ...timer,
            revision,
            state: "paused",
            session_version: (timer.session_version ?? 0) + 1,
          };
          return {
            snapshot: timer,
            revision,
            task_version: timer.task_row_version ?? 0,
          } satisfies CommandOutcome;
        }
        default:
          throw new Error(`这条用例没有脚本化命令 ${command}`);
      }
    } catch (cause) {
      lastError = cause instanceof Error ? cause.message : String(cause);
      throw cause;
    }
  });

  return {
    epoch,
    revision: () => revision,
    rows: () => rows,
    timer: () => ({ ...timer, revision }),
    commands,
    count: (command) => commands.filter((name) => name === command).length,
    lastError: () => lastError,
  };
}

/** 一个窗口（一个 JS 上下文）里能看到的东西，按真实页面的读法读。 */
export interface LabScreen {
  /** 已上屏的那份查询结果（`null` = 还没上屏，界面在加载态）。 */
  shown(): { titles: string[]; revision: number; total: number } | null;
  /** 已经真的**用上**过几份响应（"收到" ≠ "用上"）。 */
  applied(): number;
  /** 被判过期而**丢弃**的响应数（注入 (b) 的判据）。 */
  dropped(): number;
  /** 查询失败次数（epoch 不符等）。 */
  failed(): number;
  /** 现在被扣住、还没交回的响应数（注入 (b) 生效的证据）。 */
  held(): number;
  /**
   * **本视图水位**的投影（页面里那份 `listWatermark` 的等价物）：与传给
   * `ViewWatermark.applied()` 的是同一个戳。
   *
   * 它是投影、不是真对象的读口（`ViewWatermark` 只有 `isStale`/`applied` 两个方法）：
   * 判断"这份响应旧不旧"用的始终是真对象，所以"把水位推平"（`applied` 空实现）这类变异
   * 由 {@link LabScreen.dropped} 与 {@link LabScreen.shown} 抓住，不由这个投影抓。
   */
  watermark(): VersionStamp | null;
  /**
   * **注入 (b)：旧响应晚到**——把这一页的**下一次**查询响应扣住。
   *
   * 语义与真实竞态一致：请求照发、数据按**发起那一刻**的真相取好（所以它可能比
   * 后来上屏的那份旧），只是晚回来。返回放行函数。
   */
  holdNextRead(): () => void;
  /** 重拉一次（页面 effect 里那次 `load()`）。 */
  reload(): Promise<void>;
}

/** 本上下文发过哪些命令（诊断计数，用于"谁在什么时候取数"这类断言）。 */
export interface LabCalls {
  getRevision: number;
  timerSnapshot: number;
  listTasks: number;
  sessions: number;
}

/** 一个窗口（一个 JS 上下文）。 */
export interface LabWindow {
  readonly label: string;
  readonly state: DomainState;
  readonly screen: LabScreen;
  readonly calls: LabCalls;
  start(): Promise<void>;
  stop(): Promise<void>;
}

/**
 * 页面同构的消费者：`invalidated` 一变就重拉，旧响应按**本视图水位**判过期，
 * **上屏之后**才推进本视图水位。
 *
 * ⚠️ **与 `src/pages/Tasks.tsx` 的 `load()` 同源——改页面必须同步改这里。**
 * 用的是同一份 `src/components/viewWatermark.ts` 契约（一个视图一个实例，页面里是
 * `useState(createViewWatermark)[0]`）：页面那条 `list_tasks` 是**过滤 + 分页后的局部
 * 视图**，不许推全局水位（P7 Task 5 fix round 1 / I1：推了会吞掉同 `revision` 的失效
 * 通知，并让 30 秒校验失去判据）。
 *
 * ⚠️ 这里少了页面那条「问题身份」判据（判据①：响应回答的是不是**现在这个问题**）——
 * 本实验的页面只有一个问题（当前全部任务），没有第二个筛选可切。页面里判据①挡的是
 * "切换筛选后旧筛选的迟到响应"，与本实验要验的**版本**判据不是同一条。
 *
 * 本轮之前这里用的是全局 `isStaleResponse` + `markApplied`（旧口径），场景 1/3 的四处
 * 断言也跟着旧契约走——**替身落后于生产契约**正是评审预测的那处耦合，已按新契约对齐。
 */
function createScreen(state: DomainState, calls: LabCalls): LabScreen {
  /**
   * 这一页自己的水位（页面里那份 `listWatermark` 的等价物）。
   *
   * `heldStamp` 只是它的**投影**（与传给 `applied()` 的是同一个戳），用来给断言一个
   * 可读出口；判断"旧不旧"用的始终是 `watermark` 这个真对象。
   */
  const watermark = createViewWatermark();
  let heldStamp: VersionStamp | null = null;
  let shown: { titles: string[]; revision: number; total: number } | null = null;
  let applied = 0;
  let dropped = 0;
  let failed = 0;
  let hold: { promise: Promise<void>; release: () => void } | null = null;
  /** 已经取到数据、正在等放行的响应数（注入 (b) 生效的证据）。 */
  let gated = 0;
  let lastInvalidated = state.getView().invalidated;

  async function reload(): Promise<void> {
    const epoch = state.getView().dataEpoch;
    // 还没握手：一条带 `expected_data_epoch` 的命令都不发（与页面同一条守卫）。
    if (epoch === null) return;
    calls.listTasks += 1;
    let result: TaskQueryResult;
    try {
      result = await listTasks({
        statuses: [],
        project: "any",
        context_tag_id: null,
        limit: 20,
        offset: 0,
        expected_data_epoch: epoch,
      });
    } catch {
      failed += 1;
      return;
    }
    // 注入 (b)：响应已经算好，但要等放行才交给这一页（"先取数据、后交回"）。
    const gate = hold;
    hold = null;
    if (gate !== null) {
      gated += 1;
      await gate.promise;
      gated -= 1;
    }
    // 判据②：这条响应是不是比**本视图已上屏的那一份**更旧（比 `data_epoch`/`revision`，
    // 不比到达顺序）。用的是本视图水位，不是全局那把。
    if (watermark.isStale(result, epoch)) {
      dropped += 1;
      return;
    }
    // 「收到」≠「用上」：真的上屏之后才推进本视图水位。
    watermark.applied(result);
    heldStamp = { data_epoch: result.data_epoch, revision: result.revision };
    applied += 1;
    shown = { titles: result.tasks.map((row) => row.title), revision: result.revision, total: result.total };
  }

  state.subscribe(() => {
    const invalidated = state.getView().invalidated;
    if (invalidated === lastInvalidated) return;
    lastInvalidated = invalidated;
    void reload();
  });

  return {
    shown: () => shown,
    applied: () => applied,
    dropped: () => dropped,
    failed: () => failed,
    held: () => gated,
    watermark: () => heldStamp,
    holdNextRead() {
      let release!: () => void;
      const promise = new Promise<void>((resolve) => {
        release = resolve;
      });
      hold = { promise, release };
      return release;
    },
    reload,
  };
}

/** 建一个窗口：真实的 `domainState` + 真实的 IPC 转发，读命令按上下文记账。 */
export function createLabWindow(label: string): LabWindow {
  const calls: LabCalls = { getRevision: 0, timerSnapshot: 0, listTasks: 0, sessions: 0 };
  const state = createDomainState({
    getRevision: () => {
      calls.getRevision += 1;
      return getRevision();
    },
    timerSnapshot: () => {
      calls.timerSnapshot += 1;
      return timerSnapshot();
    },
    // 真实实现（`src/ipc.ts`）：先订阅并暂存 → `load()` → 按原顺序交付 → 转直通。
    // 这里只加一层记账，签名一个字没动。
    startEventSession: (handler, load) => {
      calls.sessions += 1;
      return startEventSession(handler, load);
    },
  });
  const screen = createScreen(state, calls);
  return {
    label,
    state,
    screen,
    calls,
    async start() {
      await state.start();
      await screen.reload();
    },
    stop: () => state.stop(),
  };
}

/** 双窗口实验台。 */
export interface SyncLab {
  backend: LabBackend;
  bus: EventTransport;
  a: LabWindow;
  b: LabWindow;
  /** 冲掉链上的异步续体（按**宏任务轮次**，不是数微任务个数——见 `flush` 的注释）。 */
  settle(turns?: number): Promise<void>;
  /** 一条 `domain.changed` 信封（同 epoch 的下一版）。 */
  envelope(revision: number, payload: unknown): EventEnvelope;
  /** A 发一条业务写命令（`create_task`）+ 命令层那条广播（载荷就是响应 DTO）。 */
  writeFromA(title: string): Promise<{ change: TaskChange; envelope: EventEnvelope; delivery: Delivery }>;
  /** A 的 `pause_timer`（请求完全按页面那条路构造：取自己快照里的会话身份）。 */
  pauseFromA(): Promise<{ outcome: CommandOutcome; envelope: EventEnvelope; delivery: Delivery }>;
}

/**
 * 冲掉链上的异步续体。
 *
 * ⚠️ **按宏任务轮次冲，不是数微任务个数**：回到宏任务之前，微任务队列一定被跑空，
 * 所以生产代码里多一层 `await` 不会让这里假红（旧版写死 30 个 `await Promise.resolve()`，
 * 那是个隐式深度上限——评审 Minor）。假时钟下用 `advanceTimersByTimeAsync(0)` 转同一轮。
 *
 * 需要"等到某个条件成立"的地方（例如被扣住的响应真的到达扣留点）**用 `vi.waitFor` 断言**，
 * 不要靠多冲几轮。
 */
async function flush(turns = 3): Promise<void> {
  for (let index = 0; index < turns; index += 1) {
    if (vi.isFakeTimers()) {
      await vi.advanceTimersByTimeAsync(0);
    } else {
      await new Promise<void>((resolve) => {
        setTimeout(resolve, 0);
      });
    }
  }
}

/**
 * 建起两个窗口：各自握手、各自读首屏。两边**没有共享内存**——只有假后端与事件通道。
 *
 * ⚠️ 必须在使用**假时钟**的用例里先 `vi.useFakeTimers()` 再调它：30 秒校验的
 * `setInterval` 是在 `domainState.start()` 里挂上的。
 */
export async function createSyncLab(): Promise<SyncLab> {
  transport.reset();
  const backend = createLabBackend();
  const a = createLabWindow("A");
  const b = createLabWindow("B");
  await a.start();
  await b.start();
  await flush();

  const envelope = (revision: number, payload: unknown): EventEnvelope => ({
    data_epoch: backend.epoch,
    event: EVENT_DOMAIN_CHANGED,
    revision,
    at: AT,
    payload,
  });

  return {
    backend,
    bus: transport,
    a,
    b,
    settle: flush,
    envelope,

    async writeFromA(title) {
      const change = await createTask({ expected_data_epoch: backend.epoch, title });
      const note = envelope(change.revision, change);
      // 命令层的 `announce`（`src-tauri/src/commands/mod.rs`）：拿到写结果之后、
      // 释放锁之前广播一条，载荷就是这条命令的响应 DTO。
      const delivery = transport.broadcast(note);
      return { change, envelope: note, delivery };
    },

    async pauseFromA() {
      const snapshot = a.state.getView().timer;
      const sessionId = snapshot?.session_id ?? null;
      const sessionVersion = snapshot?.session_version ?? null;
      if (sessionId === null || sessionVersion === null) throw new Error("A 手上没有会话身份");
      const outcome = await pauseTimer({
        expected_data_epoch: backend.epoch,
        session_id: sessionId,
        session_expected_version: sessionVersion,
      });
      const note = envelope(outcome.revision, outcome);
      const delivery = transport.broadcast(note);
      return { outcome, envelope: note, delivery };
    },
  };
}

/** 收尾：撤掉两个上下文的订阅与 IPC 替身（用例的 `afterEach` 调它）。 */
export async function disposeSyncLab(lab: SyncLab): Promise<void> {
  await lab.a.stop();
  await lab.b.stop();
  transport.reset();
  clearMocks();
}
