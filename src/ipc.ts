/**
 * IPC 客户端（P7 Task 1b）：24 条命令的转发、错误规范化、迟到响应丢弃与事件订阅。
 *
 * 这一层**不含业务规则**（00 §6）：它只做转发、形状转换与协议原语。
 * 状态判断（合法性、统计口径、恢复分流）全在 Rust；镜像与接纳策略是 Task 2 的
 * `domainState`，这里只给它可用的原语。
 *
 * ## 三条约定（都来自 Rust 侧的冻结契约）
 *
 * 1. **命令名与参数形状逐个对应** `src-tauri/src/commands/mod.rs`。每条命令只收
 *    **一个**名为 `request` 的参数，所以转发层统一发 `{ request }`；没有入参的
 *    三条（`get_revision` / `timer_snapshot` / `timer_tick`）不发这个键。
 * 2. **失败一律是 {@link IpcError}**，形状 `{code, message, authority,
 *    requires_handshake}`。非法 `project` 形状这类错误走的是 **Tauri 自己的反序列化
 *    失败**，拿不到 `ErrorResponse`——那种情况兜底成 {@link TRANSPORT_ERROR}，
 *    **原始 `message` 原样保留**（前端按 R8 直接展示 message）。
 * 3. **迟到响应不得覆盖新状态**：{@link createFreshnessGate} 是客户端侧的水位，
 *    {@link sendVersioned} 是"发一次请求、旧了就交回 `null`"的那一步。
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  ArchiveProjectRequest,
  ClarifyReadyRequest,
  CommandOutcome,
  CreateProjectRequest,
  CreateTagRequest,
  CreateTaskRequest,
  DailyPlanChange,
  DailyPlanQuery,
  DailyPlanView,
  EpochRequest,
  ErrorAuthority,
  ErrorResponse,
  EventEnvelope,
  ListProjectsRequest,
  ListTagsRequest,
  PlanMutationRequest,
  ProjectChange,
  ProjectList,
  RenameProjectRequest,
  ResumeRequest,
  RevisionSnapshot,
  SessionRequest,
  SetTaskProjectRequest,
  StartTimerRequest,
  TagChange,
  TagList,
  TaskChange,
  TaskProjectChange,
  TaskQueryRequest,
  TaskQueryResult,
  TaskTagRequest,
  TaskTagsRequest,
  TaskTagsChange,
  TimerSnapshot,
} from "./types/ipc";

/**
 * 事件频道名（`src-tauri/src/lib.rs` 的 `EVENT_CHANNEL`）。
 *
 * 它不属于信封（信封是 `payload` 的形状），所以它住在这一层——Rust 侧同理。
 */
export const EVENT_CHANNEL = "worktrace:event";

/**
 * 客户端本地码：**没走到命令体**（命令名不存在、参数反序列化失败、IPC 通道故障）。
 *
 * 它不是 Rust 的五个码之一，也永远不该被展示成"未知错误"以外的东西：
 * 前端按未知 `code` 的规则直接展示 `message`（R8）。
 */
export const TRANSPORT_ERROR = "TRANSPORT_ERROR";

// ─────────────────────────────────────────────────────────────────────────────
// 错误规范化
// ─────────────────────────────────────────────────────────────────────────────

/** 失败响应的可判定形状；`IpcError` 就是它加一个 `Error`。 */
export interface IpcErrorShape {
  code: string;
  message: string;
  authority: ErrorAuthority | null;
  requires_handshake: boolean;
}

/** 前端唯一会接到的错误类型：`code` 可判定，`message` 是给用户看的文案。 */
export class IpcError extends Error implements IpcErrorShape {
  readonly code: string;
  readonly authority: ErrorAuthority | null;
  readonly requires_handshake: boolean;

  constructor(shape: IpcErrorShape) {
    super(shape.message);
    this.name = "IpcError";
    this.code = shape.code;
    this.authority = shape.authority;
    this.requires_handshake = shape.requires_handshake;
  }
}

/**
 * 认出一个真正的 `ErrorResponse`。
 *
 * 判据是**三个必填字段的类型**，不是"有 code 就算"：Tauri 的传输层错误是字符串，
 * 别的东西也可能恰好带一个 `code`。`authority` 允许缺失或 `null`。
 */
function asErrorResponse(cause: unknown): ErrorResponse | null {
  if (typeof cause !== "object" || cause === null) return null;
  const value = cause as Record<string, unknown>;
  if (typeof value.code !== "string" || typeof value.message !== "string") return null;
  if (typeof value.requires_handshake !== "boolean") return null;
  const authority = value.authority;
  if (authority !== null && authority !== undefined && typeof authority !== "object") return null;
  return {
    code: value.code,
    message: value.message,
    authority: (authority as ErrorAuthority | null | undefined) ?? null,
    requires_handshake: value.requires_handshake,
  };
}

/** 兜底文案：`Error` 取 `message`，其余原样 `String(...)`，**绝不吞掉内容**。 */
function describe(cause: unknown): string {
  if (cause instanceof Error) return cause.message;
  return String(cause);
}

/**
 * 把 Tauri 抛出的任何东西统一成 {@link IpcError}。
 *
 * 两条路径：
 * - 命令体返回 `Err(ErrorResponse)` ⇒ 原样五个字段（五个码原样透传）；
 * - **Tauri 自己的失败**（参数反序列化、命令不存在、IPC 故障）⇒ `TRANSPORT_ERROR` +
 *   原始 message。**这条兜底是必需的**：非法 `project` 形状走的就是它，
 *   那种失败在类型上根本到不了 `ErrorResponse`。
 *
 * 兜底的 `requires_handshake` 是 `false`：反序列化失败发生在命令体**之前**，
 * 库身份没有被动过，没有理由要求重新握手。
 */
export function toIpcError(cause: unknown): IpcError {
  if (cause instanceof IpcError) return cause;
  const response = asErrorResponse(cause);
  if (response !== null) return new IpcError(response);
  return new IpcError({
    code: TRANSPORT_ERROR,
    message: describe(cause),
    authority: null,
    requires_handshake: false,
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// 转发（24 条，命令名与参数形状逐个对应 commands/mod.rs）
// ─────────────────────────────────────────────────────────────────────────────

/**
 * 唯一一次 `invoke`。
 *
 * 参数名固定是 `request`（Rust 侧每条命令的形参名），字段名保持 snake_case
 * ——Tauri 的 `rename_all = "camelCase"` 只作用于**参数名**层面的默认约定，
 * 这里显式给参数名，所以不受它影响。
 */
async function call<T>(command: string, request?: object): Promise<T> {
  try {
    return await invoke<T>(command, request === undefined ? undefined : { request });
  } catch (cause) {
    throw toIpcError(cause);
  }
}

/** `get_revision`：不要求已知 epoch 的唯一入口（`services::handshake::get_revision`）。 */
export function getRevision(): Promise<RevisionSnapshot> {
  return call<RevisionSnapshot>("get_revision");
}

/** `list_projects`：`status` 为 `null` 时含归档与 `done` 的历史。 */
export function listProjects(request: ListProjectsRequest): Promise<ProjectList> {
  return call<ProjectList>("list_projects", request);
}

/** `list_selectable_projects`：只列 `active`（新建任务的选择器用）。 */
export function listSelectableProjects(request: EpochRequest): Promise<ProjectList> {
  return call<ProjectList>("list_selectable_projects", request);
}

/** `create_project`。 */
export function createProject(request: CreateProjectRequest): Promise<ProjectChange> {
  return call<ProjectChange>("create_project", request);
}

/** `rename_project`。改成同名 ⇒ 幂等（零写入、`revision` 不动）。 */
export function renameProject(request: RenameProjectRequest): Promise<ProjectChange> {
  return call<ProjectChange>("rename_project", request);
}

/** `archive_project`。归档不删任务、不动历史。 */
export function archiveProject(request: ArchiveProjectRequest): Promise<ProjectChange> {
  return call<ProjectChange>("archive_project", request);
}

/** `list_tags`：`kind` 为 `null` 时四类都返回。 */
export function listTags(request: ListTagsRequest): Promise<TagList> {
  return call<TagList>("list_tags", request);
}

/** `create_tag`。 */
export function createTag(request: CreateTagRequest): Promise<TagChange> {
  return call<TagChange>("create_tag", request);
}

/** `tags_of_task`（纯读）。 */
export function tagsOfTask(request: TaskTagsRequest): Promise<TagList> {
  return call<TagList>("tags_of_task", request);
}

/** `tag_task`。重复打标 ⇒ 幂等。 */
export function tagTask(request: TaskTagRequest): Promise<TaskTagsChange> {
  return call<TaskTagsChange>("tag_task", request);
}

/** `untag_task`。本来就不在集合里 ⇒ 幂等。 */
export function untagTask(request: TaskTagRequest): Promise<TaskTagsChange> {
  return call<TaskTagsChange>("untag_task", request);
}

/** `list_tasks`：筛选与分页的校验在服务层，非法状态串 ⇒ `DOMAIN_ERROR`。 */
export function listTasks(request: TaskQueryRequest): Promise<TaskQueryResult> {
  return call<TaskQueryResult>("list_tasks", request);
}

/** `create_task`（捕获）。空标题被服务拒绝。 */
export function createTask(request: CreateTaskRequest): Promise<TaskChange> {
  return call<TaskChange>("create_task", request);
}

/** `clarify_ready`。只接受没有在计时的 `Inbox` / `Clarifying`。 */
export function clarifyReady(request: ClarifyReadyRequest): Promise<TaskChange> {
  return call<TaskChange>("clarify_ready", request);
}

/** `set_task_project`：绑定到 active 项目 / 解除关联。同值 ⇒ 幂等。 */
export function setTaskProject(request: SetTaskProjectRequest): Promise<TaskProjectChange> {
  return call<TaskProjectChange>("set_task_project", request);
}

/** `plan_for`：今日选择列表。 */
export function planFor(request: DailyPlanQuery): Promise<DailyPlanView> {
  return call<DailyPlanView>("plan_for", request);
}

/** `add_to_plan`。重复加入 ⇒ 幂等。 */
export function addToPlan(request: PlanMutationRequest): Promise<DailyPlanChange> {
  return call<DailyPlanChange>("add_to_plan", request);
}

/** `remove_from_plan`。 */
export function removeFromPlan(request: PlanMutationRequest): Promise<DailyPlanChange> {
  return call<DailyPlanChange>("remove_from_plan", request);
}

/** `timer_snapshot`：查询命令（自己取一次采样），不是纯读。 */
export function timerSnapshot(): Promise<TimerSnapshot> {
  return call<TimerSnapshot>("timer_snapshot");
}

/** `timer_tick`：与快照同形，另外让 `tick_seq` 前进一步。 */
export function timerTick(): Promise<TimerSnapshot> {
  return call<TimerSnapshot>("timer_tick");
}

/** `start_timer`。开始新计时要过恢复门禁（`RECOVERY_REQUIRED`）。 */
export function startTimer(request: StartTimerRequest): Promise<CommandOutcome> {
  return call<CommandOutcome>("start_timer", request);
}

/** `pause_timer`。暂停值冻结，不在前端算。 */
export function pauseTimer(request: SessionRequest): Promise<CommandOutcome> {
  return call<CommandOutcome>("pause_timer", request);
}

/** `resume_timer`。 */
export function resumeTimer(request: ResumeRequest): Promise<CommandOutcome> {
  return call<CommandOutcome>("resume_timer", request);
}

/** `finish_timer`。到点只提示、不自动完成。 */
export function finishTimer(request: SessionRequest): Promise<CommandOutcome> {
  return call<CommandOutcome>("finish_timer", request);
}

// ─────────────────────────────────────────────────────────────────────────────
// 迟到响应丢弃（00 §5 规则①②③）
// ─────────────────────────────────────────────────────────────────────────────

/**
 * 响应/通知身上的版本标记。
 *
 * **不是**每个业务响应 DTO 都直接带这两个字段：`CommandOutcome` 是例外——它的
 * `data_epoch` 在 `snapshot` 里，顶层只有 `revision`（见
 * `src/types/__snapshots__/command_outcome.json`）。拿它做迟到判定要取
 * `outcome.snapshot` 那个戳，别把 `outcome` 自己当戳用。
 */
export interface VersionStamp {
  data_epoch: string;
  revision: number;
}

/**
 * 「已应用水位」闸门。
 *
 * 只做**比较**，不做接纳决策：接纳一个快照、丢弃一条通知、要不要重新握手，
 * 都是 Task 2 的 `domainState` 的事（这里给它一条可测的原语）。
 *
 * 三条规则的分工（别合并成一个 `shouldApply`：它们的**处置不同**）：
 * - **规则①（未知 epoch）**：[`FreshnessGate.isUnknownEpoch`] ⇒ **重新握手**；
 * - **规则③（查询响应）**：[`FreshnessGate.isStaleResponse`]，同 epoch 且 `revision`
 *   **更小** ⇒ 丢弃。同版本是合法的（重复查询），不丢；
 * - **规则②（通知）**：[`FreshnessGate.isStaleNotification`]，同 epoch 且 `revision`
 *   **小于等于**已应用水位 ⇒ 丢弃（快照已经把这一版包含进去了）。
 */
export interface FreshnessGate {
  /** 已应用的水位；还没应用过任何响应时为 `null`。 */
  applied(): VersionStamp | null;
  /** 记下一个已应用的响应。水位只前进：更旧或同版的标记不会把它拉回去。 */
  markApplied(stamp: VersionStamp): void;
  /**
   * 规则①：这个版本标记来自**未知 epoch**（与已应用水位不是同一个库）。
   *
   * 处置与规则②**不同**，所以是单独一个方法：规则②是安静地扔掉（快照已经包含它），
   * 规则①要**重新握手**再拉一致快照。`isStaleNotification` 对未知 epoch 返回 `false`
   * 正是为了不让它被"静默丢弃"那条路吞掉——调用方必须**显式先问这一条**。
   *
   * `applied()` 为 `null`（还没应用过任何快照）时不判未知：启动阶段"先订阅、再拉快照"
   * 的顺序由 [`startEventSession`] 保证。
   */
  isUnknownEpoch(stamp: VersionStamp): boolean;
  /**
   * 规则③：这个查询响应该不该被丢弃。
   *
   * `requestEpoch` 是**发起请求时**调用方手上的 epoch（`null` = 这条命令不带 epoch，
   * 如 `get_revision`）。响应回来的 epoch 与它不一致 ⇒ 这次响应回答的不是调用方
   * 问的那个世界，丢弃并由调用方重新握手（规则①）。
   */
  isStaleResponse(stamp: VersionStamp, requestEpoch: string | null): boolean;
  /** 规则②：这条通知该不该被丢弃（**不含**未知 epoch 那一支）。 */
  isStaleNotification(stamp: VersionStamp): boolean;
}

/** 建一个水位闸门。每个 JS 上下文一个（00 §6）。 */
export function createFreshnessGate(): FreshnessGate {
  let applied: VersionStamp | null = null;

  return {
    applied: () => applied,

    markApplied(stamp) {
      if (
        applied !== null &&
        applied.data_epoch === stamp.data_epoch &&
        stamp.revision <= applied.revision
      ) {
        return;
      }
      applied = { data_epoch: stamp.data_epoch, revision: stamp.revision };
    },

    isUnknownEpoch(stamp) {
      return applied !== null && applied.data_epoch !== stamp.data_epoch;
    },

    isStaleResponse(stamp, requestEpoch) {
      if (requestEpoch !== null && stamp.data_epoch !== requestEpoch) return true;
      if (applied === null || applied.data_epoch !== stamp.data_epoch) return false;
      return stamp.revision < applied.revision;
    },

    isStaleNotification(stamp) {
      if (applied === null || applied.data_epoch !== stamp.data_epoch) return false;
      return stamp.revision <= applied.revision;
    },
  };
}

/**
 * 发一次带版本的请求，迟到的响应交回 `null`（调用方**不得**拿它覆盖新状态）。
 *
 * 只判断、不改水位：水位由调用方在**真的把响应应用进镜像**之后
 * {@link FreshnessGate.markApplied}。两者分开是刻意的——"收到了"不等于"用上了"。
 *
 * 失败照常抛出 {@link IpcError}（迟到判定不吞错误）。
 */
export async function sendVersioned<T extends VersionStamp>(
  send: () => Promise<T>,
  gate: FreshnessGate,
  requestEpoch: string | null,
): Promise<T | null> {
  const response = await send();
  return gate.isStaleResponse(response, requestEpoch) ? null : response;
}

// ─────────────────────────────────────────────────────────────────────────────
// 事件订阅（00 §5 规则①的调用顺序）
// ─────────────────────────────────────────────────────────────────────────────

/** 一个已打开的事件会话；`close()` 之后不再有任何回调。 */
export interface EventStream {
  close(): Promise<void>;
}

/**
 * **先订阅、再拉快照**的唯一入口（00 §5 规则①）。
 *
 * 顺序固定，不可调换：
 *
 * 1. 订阅 `worktrace:event`，到达的通知**按到达顺序暂存**（此时还没有快照可比）；
 * 2. 执行 `load()`——典型实现是「握手取 epoch → 拉业务快照 → **应用**快照」，
 *    返回什么由调用方决定；
 * 3. `load()` 成功之后，把暂存的通知**按原顺序**交给 `handler`，随后转入直通模式。
 *
 * 第 2 步必须在第 3 步之前完成，否则「快照与通知之间」的那条缝就会丢事件。
 * `load()` 抛错 ⇒ 订阅立刻撤掉并原样抛出（不留悬挂监听）。
 *
 * ⚠️ `handler` 拿到的通知**还没有经过水位过滤**（Task 2 的 `domainState` 自己判），
 * 而且判的顺序不能反：
 *
 * 1. [`FreshnessGate.isUnknownEpoch`] 为真 ⇒ **重新握手**，**不能落到 apply 分支**
 *    （规则①：那是"另一个库的数据"，不是"更新的数据"）；
 * 2. 否则 [`FreshnessGate.isStaleNotification`] 为真 ⇒ 丢弃（规则②，安静地扔）；
 * 3. 两条都不为真 ⇒ 才 apply。
 */
export async function startEventSession<T>(
  handler: (event: EventEnvelope) => void,
  load: () => Promise<T>,
): Promise<{ value: T; stream: EventStream }> {
  const buffered: EventEnvelope[] = [];
  let closed = false;
  let deliver = (event: EventEnvelope): void => {
    buffered.push(event);
  };

  const unlisten: UnlistenFn = await listen<EventEnvelope>(EVENT_CHANNEL, (event) => {
    if (!closed) deliver(event.payload);
  });

  const stream: EventStream = {
    async close() {
      if (closed) return;
      closed = true;
      deliver = () => undefined;
      buffered.length = 0;
      await unlisten();
    },
  };

  let value: T;
  try {
    value = await load();
  } catch (cause) {
    await stream.close();
    throw cause;
  }

  for (const event of buffered) handler(event);
  buffered.length = 0;
  deliver = handler;
  return { value, stream };
}
