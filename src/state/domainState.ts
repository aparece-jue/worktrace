/**
 * 前端状态镜像（P7 Task 2）：本 JS 上下文里**唯一**的 `domainState` 订阅入口（00 §6）。
 *
 * 这一层只做三件事，**不含业务规则**（合法性、统计口径、恢复分流都在 Rust）：
 *
 * 1. **订阅**：先监听并暂存通知，再拉一致快照，最后按序交付暂存的通知
 *    （00 §5 规则 1；顺序由 {@link startEventSession} 保证）；
 * 2. **失效**：`domain.changed` 只让缓存失效（`invalidated` 计数 +1，页面据此重拉
 *    自己显示的数据），**不把载荷并进镜像**；`timer.tick` 只更新展示值
 *    （`view.timer`），**不推业务水位**；
 * 3. **收敛**：可见窗口至多每 30 秒（{@link VERIFY_INTERVAL_MS}）校验一次
 *    `get_revision`，隐藏窗口在显示前校验；版本比已见版本靠前（末次通知丢了）或
 *    epoch 变了（恢复换库）⇒ 合并刷新 + 取新快照。
 *
 * ## 与 Rust 的对应关系
 *
 * | 这里 | Rust（规范文本） |
 * | --- | --- |
 * | `gate.onNotification` / `applySnapshot` / `onQueryResponse` | `services/events.rs` 的 `RevisionGate`（`:247`–`:363`，四条规则） |
 * | `gate.isStaleNotification` | 00 §5 规则 3：应用快照后丢弃同 epoch 且 `revision <=` 快照版本的通知 |
 * | `orderTimer` | 00 §5 的计时判据（`:69`）；Rust 侧对应 `Coordinator::is_stale_tick`（`services/timer/coordinator.rs:363`） |
 * | {@link VERIFY_INTERVAL_MS} | 00 §5 规则 4 的「可见窗口至多每 30 秒」 |
 *
 * ⚠️ {@link VERIFY_INTERVAL_MS} 与 `Coordinator` 的 `HEARTBEAT_INTERVAL_MS = 30_000`
 * （`services/timer/coordinator.rs`，**检查点**频率）**无关**：数字相同纯属巧合，
 * 改一个不影响另一个。
 *
 * ## 多窗口
 *
 * 每个窗口是一个独立的 JS 上下文（各自的模块实例），{@link domainState} 因此天然
 * 各有一份、互不共享内存。{@link createDomainState} 存在的理由是两个：测试要能注入
 * 替身、以及将来显式创建第二个上下文（Task 6a 的双窗口实验）。
 */

import {
  EVENT_DOMAIN_CHANGED,
  EVENT_TIMER_TICK,
  type EventEnvelope,
  type RevisionSnapshot,
  type TimerSnapshot,
} from "../types/ipc";
import {
  createFreshnessGate,
  getRevision,
  startEventSession,
  timerSnapshot,
  type EventStream,
  type FreshnessGate,
} from "../ipc";

/**
 * 可见窗口校验 `get_revision` 的周期（毫秒）。
 *
 * 00 §5 规则 4 的「至多每 30 秒」：这是**前端校验**的节拍，不是采样节拍，也不是
 * `Coordinator` 的检查点频率。隐藏窗口不轮询，改为在**显示前**校验一次。
 */
export const VERIFY_INTERVAL_MS = 30_000;

/** 握手/校验的状态。状态栏（`data-region="status"`）按它展示。 */
export type HandshakePhase = "idle" | "connecting" | "ready" | "failed";

/**
 * 镜像对外的一次性快照（`useSyncExternalStore` 的 `getSnapshot` 返回值）。
 *
 * **引用稳定**：只有内容变了才会换对象——`getSnapshot` 每次新建对象会让 React 认为
 * 一直在变（无限重渲染）。所以派生读一律走 hooks 里的选择器，不要在这里现算。
 */
export interface DomainView {
  /** 已应用的一致快照 epoch；还没握手成功时为 `null`。 */
  dataEpoch: string | null;
  /** 已应用的一致快照版本（**水位**；不是"最后见到的通知版本"）。 */
  revision: number;
  /** 缓存失效计数：每接纳一条 `domain.changed`（或整体失效）加一，页面据此重拉。 */
  invalidated: number;
  /** 计时展示值：最近一次被接纳的 `timer.tick` 或计时快照。 */
  timer: TimerSnapshot | null;
  /** 握手/校验状态。 */
  phase: HandshakePhase;
}

/** 镜像依赖的三条 IPC 能力（默认就是 `src/ipc.ts` 的真实实现）。 */
export interface DomainDeps {
  getRevision(): Promise<RevisionSnapshot>;
  timerSnapshot(): Promise<TimerSnapshot>;
  startEventSession<T>(
    handler: (event: EventEnvelope) => void,
    load: () => Promise<T>,
  ): Promise<{ value: T; stream: EventStream }>;
}

/** 本上下文的状态镜像。 */
export interface DomainState {
  /** **唯一**订阅入口：页面通过 `src/state/hooks.ts` 的 hooks 用它。 */
  subscribe(listener: () => void): () => void;
  /** 当前快照（引用稳定）。 */
  getView(): DomainView;
  /** 诊断：当前订阅者个数（卸载泄漏与双窗口实验用；不参与任何业务判断）。 */
  subscriberCount(): number;
  /** 打开事件会话并完成首次握手；**幂等**（重复调用共享同一次启动）。 */
  start(): Promise<void>;
  /** 撤掉监听、停掉轮询并把镜像复位；**幂等**。 */
  stop(): Promise<void>;
}

const INITIAL_VIEW: DomainView = {
  dataEpoch: null,
  revision: 0,
  invalidated: 0,
  timer: null,
  phase: "idle",
};

/** 计时展示值的两条来源：命令的响应是权威样本，事件是**只更新展示值**的 tick。 */
type TimerSource = "tick" | "snapshot";

/** 展示值该不该被这份候选样本替换。 */
type TimerDecision = "apply" | "stale" | "resync" | "rehandshake";

/**
 * 计时展示值的新旧判定（00 §5 `:69` 的原文顺序，**不要重排**）：
 * 先 `data_epoch` → `run_id` → `session_version`，**再**比 `tick_seq`。
 *
 * - `tick`（事件）：判不出来（未知）时**不自行推导状态跃迁**，交回 `resync` 去取一份
 *   计时快照；旧状态生成的 tick（更小的 `session_version`）即使序号较新也是 `stale`。
 * - `snapshot`（我们自己发出去的 `timer_snapshot` / `timer_tick` 的响应）：它是对
 *   "现在"的一次权威采样，所以"更新但我们没见过"的版本由它来落定（`apply`）；
 *   只有"明确更旧"才是 `stale`。
 *
 * `tick_seq` 只在 epoch / run / 会话 / 会话版本**全部相同**时才比：它每个 run 内递增、
 * 新 run 才重置，跨 run 或跨会话没有可比性。
 */
function orderTimer(
  candidate: TimerSnapshot,
  held: TimerSnapshot | null,
  source: TimerSource,
): TimerDecision {
  if (held === null) return "apply";
  const authoritative = source === "snapshot";

  // ① epoch：另一个库的数据。事件不许接纳（00 §5 规则 2 的"未知 epoch 通知"），
  //    我们主动拉回的权威样本才作数（它回答的就是"现在"）。
  if (candidate.data_epoch !== held.data_epoch) return authoritative ? "apply" : "rehandshake";

  // ② run_id：重启会换 run，序号跨 run 无法比较 ⇒ 取一份计时快照。
  if (candidate.run_id !== held.run_id) return authoritative ? "apply" : "resync";

  // ③ 会话切换：另一次会话的序号同样不可比。
  if (candidate.session_id !== held.session_id) return authoritative ? "apply" : "resync";

  // ④ 会话版本：旧状态生成的 tick 即使序号较新也不能覆盖暂停/切换后的展示。
  const candidateVersion = candidate.session_version;
  const heldVersion = held.session_version;
  if (candidateVersion !== heldVersion) {
    if (candidateVersion !== null && heldVersion !== null && candidateVersion < heldVersion) {
      return "stale";
    }
    // 更"新"但我们没见过的版本：tick 先触发计时快照（"较新 session_version 的未知
    // tick 先触发计时快照，不自行推导状态跃迁"）；权威样本直接落定。
    return authoritative ? "apply" : "resync";
  }

  // ⑤ 只有会话版本相同，`tick_seq` 才有可比性。
  return candidate.tick_seq > held.tick_seq ? "apply" : "stale";
}

/**
 * tick 载荷的最小形状校验：不合形状的载荷**忽略**（它是通知，不保证可用），
 * 不能让一条坏载荷把整个界面打崩。
 */
function asTimerSnapshot(payload: unknown): TimerSnapshot | null {
  if (typeof payload !== "object" || payload === null) return null;
  const value = payload as Record<string, unknown>;
  if (typeof value.data_epoch !== "string" || typeof value.run_id !== "string") return null;
  if (typeof value.tick_seq !== "number") return null;
  if (value.session_id !== null && typeof value.session_id !== "string") return null;
  if (value.session_version !== null && typeof value.session_version !== "number") return null;
  return payload as TimerSnapshot;
}

/** 默认依赖：`src/ipc.ts` 的真实转发层。 */
const IPC_DEPS: DomainDeps = { getRevision, timerSnapshot, startEventSession };

/** 建一个状态镜像。生产路径用模块底部的单例 {@link domainState}。 */
export function createDomainState(deps: DomainDeps = IPC_DEPS): DomainState {
  let gate: FreshnessGate = createFreshnessGate();
  let view: DomainView = INITIAL_VIEW;
  const listeners = new Set<() => void>();

  let stream: EventStream | null = null;
  let starting: Promise<void> | null = null;
  let verifying = false;
  let resyncing = false;
  let poll: ReturnType<typeof setInterval> | null = null;
  let onVisibilityChange: (() => void) | null = null;
  /** `stop()` 之后在途的异步结果不再写回（每停一次加一）。 */
  let generation = 0;

  function publish(patch: Partial<DomainView>): void {
    view = { ...view, ...patch };
    for (const listener of [...listeners]) listener();
  }

  /** 可见性：隐藏窗口不轮询，显示前校验（00 §5 规则 4）。 */
  function isVisible(): boolean {
    return typeof document === "undefined" || document.visibilityState !== "hidden";
  }

  /**
   * 把一份权威快照应用进水位，并按结果更新视图。
   *
   * `stale_ignored`（同 epoch 但比水位旧）⇒ **什么都不动**：旧快照不得覆盖新状态。
   * `cache_invalidated`（换 epoch）⇒ 全部业务/计时缓存失效：失效计数 +1、计时展示
   * 作废（旧库的值不能留在屏幕上）。第一次拿到快照（`previousEpoch === null`）也是
   * `cache_invalidated`，但那时没有任何缓存可失效，所以不推失效计数。
   */
  function applyStamp(stamp: RevisionSnapshot): "applied" | "cache_invalidated" | "stale_ignored" {
    const previousEpoch = gate.epoch();
    const effect = gate.applySnapshot(stamp.data_epoch, stamp.revision);
    if (effect === "stale_ignored") return effect;
    const changedEpoch = previousEpoch !== null && previousEpoch !== stamp.data_epoch;
    publish({
      dataEpoch: stamp.data_epoch,
      revision: stamp.revision,
      invalidated: changedEpoch ? view.invalidated + 1 : view.invalidated,
      timer: changedEpoch ? null : view.timer,
    });
    return effect;
  }

  /** 应用一份**我们自己拉回来的**计时样本（水位 + 展示值两条判据）。 */
  function applySample(sample: TimerSnapshot): void {
    // 规则③对快照的同一条要求：旧于已应用水位就不覆盖（数据与展示都不动）。
    if (applyStamp(sample) === "stale_ignored") return;
    if (orderTimer(sample, view.timer, "snapshot") === "apply") publish({ timer: sample });
  }

  async function pullTimerSample(token: number): Promise<void> {
    const sample = await deps.timerSnapshot();
    if (token !== generation) return;
    applySample(sample);
  }

  /**
   * 规则④：跳号/乱序无法证明一致 ⇒ 取新快照。
   *
   * 先推失效计数（受影响视图合并刷新），再拉一份计时快照把水位拉回连续。
   */
  async function resync(): Promise<void> {
    if (resyncing || stream === null) return;
    resyncing = true;
    const token = generation;
    try {
      publish({ invalidated: view.invalidated + 1 });
      await pullTimerSample(token);
    } catch {
      // 取不到快照：失效计数已经推过，下一次通知/轮询会再试（末次通知丢失仍要能收敛）。
    } finally {
      resyncing = false;
    }
  }

  /**
   * 一次版本校验：握手 → 比 epoch → 比已见版本。
   *
   * 三个触发源共用它：可见窗口的 30 秒周期、隐藏窗口显示前的校验、以及
   * **未知 epoch 的通知**（规则①的"重新握手"就是再调一次 `get_revision`）。
   *
   * 同 epoch 且版本比已见版本靠前 ⇒ 中间有通知丢了（末次通知丢失也包括在内）：
   * 无法证明一致 ⇒ 走 {@link resync} 取新快照。
   */
  async function verify(): Promise<void> {
    if (verifying || stream === null) return;
    verifying = true;
    const token = generation;
    try {
      const identity = await deps.getRevision();
      if (token !== generation) return;
      if (gate.epoch() === null) {
        applyStamp(identity);
        return;
      }
      if (gate.isUnknownEpoch(identity)) {
        // 规则①：库身份变了（恢复/换库）⇒ 全部失效，并按新 epoch 重新拉一致样本。
        if (applyStamp(identity) === "cache_invalidated") await pullTimerSample(token);
        return;
      }
      if (identity.revision > gate.seenRevision()) await resync();
    } catch {
      // 校验失败不改动任何状态：等下一次通知/轮询（收敛靠的就是这个周期）。
    } finally {
      verifying = false;
    }
  }

  /**
   * `domain.changed`：**只作缓存失效**（00 §6）。
   *
   * 载荷（就是那条写命令的响应 DTO）**不并进镜像**——事件不是权威状态，
   * 权威状态要么来自我们主动拉的一致快照，要么来自 Rust 的判定。
   */
  function onDomainChanged(envelope: EventEnvelope): void {
    switch (gate.onNotification(envelope)) {
      case "apply":
        publish({ invalidated: view.invalidated + 1 });
        break;
      case "drop":
        break;
      case "resync":
        void resync();
        break;
      case "rehandshake":
        void verify();
        break;
    }
  }

  /** `timer.tick`：只更新展示值，**不推业务水位**（tick 不是业务写）。 */
  function onTimerTick(envelope: EventEnvelope): void {
    const tick = asTimerSnapshot(envelope.payload);
    if (tick === null) return;
    // 展示值还没有基线时（例如启动时那份计时快照没拿到），没有可比的对象：
    // 先用闸门判一次未知 epoch —— 规则① 对通知一视同仁，tick 也不例外。
    if (view.timer === null && gate.isUnknownEpoch(tick)) {
      void verify();
      return;
    }
    switch (orderTimer(tick, view.timer, "tick")) {
      case "apply":
        publish({ timer: tick });
        break;
      case "stale":
        break;
      case "resync":
        void resync();
        break;
      case "rehandshake":
        void verify();
        break;
    }
  }

  function onEvent(envelope: EventEnvelope): void {
    if (envelope.event === EVENT_TIMER_TICK) onTimerTick(envelope);
    else if (envelope.event === EVENT_DOMAIN_CHANGED) onDomainChanged(envelope);
    // 其它事件名：这一层不认识，也不去猜（不认识就不接纳）。
  }

  /**
   * `load()`：**必须在返回之前把快照应用进水位**。
   *
   * 这正是 Task 1b 说的「`markApplied` 前置」：{@link startEventSession} 会在
   * `load()` 返回之后才交付暂存的通知，那时水位已经推过，规则②（`revision <=`
   * 快照版本 ⇒ 丢弃）与规则④（跳号）才判得动；否则启动期那条缝里的通知会因为
   * "还没应用过任何快照"落进 apply 分支。
   */
  async function load(): Promise<void> {
    // ① 握手：全仓唯一不要求已知 epoch 的入口（`services::handshake::get_revision`）。
    //    它失败 ⇒ 这次启动失败（没有库身份就没有镜像可言），订阅由 startEventSession 撤掉。
    const identity = await deps.getRevision();
    applyStamp(identity);
    // ② 携该 epoch 拉业务一致快照。本阶段镜像持有的业务数据只有计时展示值。
    //
    //    拿不到它**不该**让整个镜像起不来：协调器故障态下 `timer_snapshot` 会返回
    //    `RECOVERY_REQUIRED`（`Coordinator::refuse_if_faulted`，那是 P3/P6 的正常路径，
    //    见计划「计时族命令提交后重建失败」那条边界）。握手给的 epoch 与水位已经生效，
    //    启动期暂存的通知照常按规则判；展示值由后续 `timer.tick` 或 30 秒校验补齐。
    try {
      await pullTimerSample(generation);
    } catch {
      // 诊断留给调用方（IPC 错误已经规范化过）；这里不改动任何已生效的水位。
    }
  }

  function startPolling(): void {
    if (poll !== null) return;
    poll = setInterval(() => {
      void verify();
    }, VERIFY_INTERVAL_MS);
  }

  function stopPolling(): void {
    if (poll === null) return;
    clearInterval(poll);
    poll = null;
  }

  function attachLifecycle(): void {
    if (typeof document !== "undefined") {
      onVisibilityChange = () => {
        if (isVisible()) {
          // 规则④：隐藏窗口在**显示前**校验（这条与 30 秒周期是两个触发源，
          // 同时到达时由 verify() 的 in-flight 合并挡住重复请求）。
          void verify();
          startPolling();
        } else {
          stopPolling();
        }
      };
      document.addEventListener("visibilitychange", onVisibilityChange);
    }
    if (isVisible()) startPolling();
  }

  function detachLifecycle(): void {
    stopPolling();
    if (onVisibilityChange !== null && typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    }
    onVisibilityChange = null;
  }

  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },

    getView: () => view,

    subscriberCount: () => listeners.size,

    start() {
      if (starting !== null) return starting;
      if (stream !== null) return Promise.resolve();
      publish({ phase: "connecting" });
      starting = (async () => {
        try {
          const opened = await deps.startEventSession<void>(onEvent, load);
          stream = opened.stream;
          attachLifecycle();
          publish({ phase: "ready" });
        } catch (cause) {
          // startEventSession 已经把订阅撤掉了（不留悬挂监听），这里只记状态。
          publish({ phase: "failed" });
          throw cause;
        } finally {
          starting = null;
        }
      })();
      return starting;
    },

    async stop() {
      generation += 1;
      detachLifecycle();
      const open = stream;
      stream = null;
      if (open !== null) await open.close();
      gate = createFreshnessGate();
      publish({ ...INITIAL_VIEW });
    },
  };
}

/**
 * 本 JS 上下文的唯一入口（00 §6）。
 *
 * 每个窗口是一个独立上下文（各自的模块实例），所以多窗口天然各持一份、互不共享内存；
 * 页面一律通过 `src/state/hooks.ts` 的 hooks 读它，不要在这里再开第二个订阅入口。
 */
export const domainState: DomainState = createDomainState();
