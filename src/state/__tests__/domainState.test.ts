/**
 * `src/state/domainState.ts` 的用例：启动时序、事件只作失效、计时展示值的新旧判定、
 * 30 秒校验的调度、以及卸载/停止后的无泄漏。
 *
 * 断言口径（总纲 §5 第 8 条）：这里只断言**展示与转发**——业务判断（状态合法性、
 * 统计口径、恢复分流）在 Rust 侧。唯一"协议"性质的断言是计时展示值的**新旧顺序**
 * 与事件/快照的**处置**，它们本来就是 00 §5 分给前端的（规范文本在 `events.rs`）。
 *
 * 除一条用例（"真实的 startEventSession"）外，事件会话都由本文件的替身驱动：
 * 替身与真实实现同契约（先订阅、`load()` 期间暂存、`load()` 返回后按序 flush），
 * 真实实现的顺序由 `src/__tests__/ipc.test.ts` 与那一条用例各自钉住。
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  EVENT_DOMAIN_CHANGED,
  EVENT_TIMER_TICK,
  type EventEnvelope,
  type RevisionSnapshot,
  type TimerSnapshot,
} from "../../types/ipc";
import { startEventSession } from "../../ipc";
import {
  VERIFY_INTERVAL_MS,
  createDomainState,
  type DomainDeps,
  type DomainState,
} from "../domainState";

// `listen` 走替身（与 `ipc.test.ts` 同一个做法）：只有这一条用例用真实的
// `startEventSession`，它要看的是"订阅先于快照"这条顺序，而不是 tauri 的实现。
//
// `onRegister` 是 fix round 1 / I3 加的：真实世界的缝隙**从订阅那一刻**就开始了
// （订阅已建立、`unlisten` 还没回来，事件就可能到达）。只在 `load()` **里面**投递
// 事件区分不出"暂存"与"直通"——那时水位已经推过，两种交付的结果一样。
const events = vi.hoisted(() => {
  const listeners = new Map<number, (event: { payload: unknown }) => void>();
  const atRegister: Array<(handler: (event: { payload: unknown }) => void) => void> = [];
  let next = 1;
  return {
    get open() {
      return listeners.size;
    },
    /** 注册那一刻要投递的（模拟"订阅已建立、load 还没开始"）。 */
    onRegister(hook: (handler: (event: { payload: unknown }) => void) => void) {
      atRegister.push(hook);
    },
    reset() {
      atRegister.length = 0;
    },
    async listen(_channel: string, handler: (event: { payload: unknown }) => void) {
      const id = next++;
      listeners.set(id, handler);
      for (const hook of atRegister.splice(0)) hook(handler);
      return async () => {
        listeners.delete(id);
      };
    },
    emit(payload: unknown) {
      for (const handler of [...listeners.values()]) handler({ payload });
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: events.listen }));

const EPOCH = "epoch-a";
const OTHER_EPOCH = "epoch-b";
const RUN = "run-1";
const SESSION = "session-1";
const AT = 1_700_000_000_000;

function identity(dataEpoch: string, revision: number): RevisionSnapshot {
  return { data_epoch: dataEpoch, revision };
}

/** 默认是一份"正在计时的正计时会话"的样本，逐字段覆盖由调用方给。 */
function sample(overrides: Partial<TimerSnapshot> = {}): TimerSnapshot {
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

function envelope(overrides: Partial<EventEnvelope> = {}): EventEnvelope {
  return {
    data_epoch: EPOCH,
    event: EVENT_DOMAIN_CHANGED,
    revision: 5,
    at: AT,
    payload: null,
    ...overrides,
  };
}

/** 冲掉链上的微任务（本模块的异步路径全是 promise，没有定时器）。 */
async function settle(ticks = 20): Promise<void> {
  for (let index = 0; index < ticks; index += 1) await Promise.resolve();
}

function setVisibility(value: "visible" | "hidden"): void {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => value });
  document.dispatchEvent(new Event("visibilitychange"));
}

interface Calls {
  getRevision: number;
  timerSnapshot: number;
  sessions: number;
  closes: number;
}

interface Harness {
  state: DomainState;
  calls: Calls;
  /** 下一次握手交回的库身份（队列空则沿用最后一次）。传函数可以在这时抛错。 */
  queueIdentity(...values: Array<RevisionSnapshot | (() => RevisionSnapshot)>): void;
  /** 下一次 `timer_snapshot` 交回的样本；传函数可以在**取样期间**投递事件。 */
  queueSample(...values: Array<TimerSnapshot | (() => TimerSnapshot)>): void;
  emit(event: EventEnvelope): void;
}

function harness(): Harness {
  const calls: Calls = { getRevision: 0, timerSnapshot: 0, sessions: 0, closes: 0 };
  const identities: Array<RevisionSnapshot | (() => RevisionSnapshot)> = [];
  const samples: Array<TimerSnapshot | (() => TimerSnapshot)> = [];
  let lastIdentity = identity(EPOCH, 5);
  let lastSample = sample();

  /**
   * 当前那一次会话（每次 `startEventSession` 各自一份，fix round 2 的 start→stop→start
   * 用例会同时有两次在飞；共用一个槽位的话，先落地那次关订阅会把后一次的直通目标清掉）。
   */
  let current: {
    handler: (event: EventEnvelope) => void;
    buffering: boolean;
    buffered: EventEnvelope[];
  } | null = null;

  const deps: DomainDeps = {
    async getRevision() {
      calls.getRevision += 1;
      const next = identities.shift();
      lastIdentity = typeof next === "function" ? next() : (next ?? lastIdentity);
      return lastIdentity;
    },
    async timerSnapshot() {
      calls.timerSnapshot += 1;
      const next = samples.shift();
      lastSample = typeof next === "function" ? next() : (next ?? lastSample);
      return lastSample;
    },
    // 与真实的 startEventSession 同契约：先订阅（此时到达的通知暂存）→ load() →
    // 按原顺序交付暂存的通知 → 转直通。
    async startEventSession(h, load) {
      calls.sessions += 1;
      const session = { handler: h, buffering: true, buffered: [] as EventEnvelope[] };
      current = session;
      let value;
      try {
        value = await load();
      } catch (cause) {
        calls.closes += 1;
        if (current === session) current = null;
        throw cause;
      }
      session.buffering = false;
      for (const event of session.buffered) h(event);
      session.buffered = [];
      return {
        value,
        stream: {
          async close() {
            calls.closes += 1;
            if (current === session) current = null;
          },
        },
      };
    },
  };

  return {
    state: createDomainState(deps),
    calls,
    queueIdentity: (...values) => identities.push(...values),
    queueSample: (...values) => samples.push(...values),
    emit(event) {
      if (current === null) throw new Error("事件会话还没打开");
      if (current.buffering) current.buffered.push(event);
      else current.handler(event);
    },
  };
}

const created: DomainState[] = [];

function track(state: DomainState): DomainState {
  created.push(state);
  return state;
}

afterEach(async () => {
  for (const state of created.splice(0)) await state.stop();
  vi.useRealTimers();
  Reflect.deleteProperty(document, "visibilityState");
  events.reset();
});

/**
 * 用**真实的** `startEventSession` 起一次会话，并在订阅建立的那一刻投递给定的通知
 * （fix round 1 / I2、I3 用）。第一次 `timerSnapshot`（load 里那次）交回第 5 版，
 * 之后每次（resync 里那次）交回第 8 版。
 */
async function startWithSeam(
  atRegister: Array<Partial<EventEnvelope>>,
): Promise<{ state: DomainState; calls: { getRevision: number; timerSnapshot: number } }> {
  const calls = { getRevision: 0, timerSnapshot: 0 };
  events.onRegister((handler) => {
    for (const note of atRegister) handler({ payload: envelope(note) });
  });
  const state = track(
    createDomainState({
      async getRevision() {
        calls.getRevision += 1;
        return identity(EPOCH, 5);
      },
      async timerSnapshot() {
        calls.timerSnapshot += 1;
        return sample({
          revision: calls.timerSnapshot === 1 ? 5 : 8,
          tick_seq: calls.timerSnapshot,
        });
      },
      startEventSession,
    }),
  );

  await state.start();
  await settle();
  return { state, calls };
}

describe("启动顺序与水位前置", () => {
  it("先订阅、再拉快照；load 返回前已经推过水位（load 期间的通知按闸门规则②/④判）", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    // 快照与通知之间那条缝里到达两条：水位之下的一条、水位之上的一条。
    h.queueSample(() => {
      h.emit(envelope({ revision: 4 }));
      h.emit(envelope({ revision: 6 }));
      return sample({ revision: 5 });
    });

    await h.state.start();

    expect(h.calls.sessions).toBe(1);
    // 若 load 没有在水位里留下快照：rev 4 会被当成"跳号"触发闸门规则④（多一次取快照），
    // 两条通知也会各自推一次失效计数 —— 下面三条会同时红。
    expect(h.calls.getRevision).toBe(1);
    expect(h.calls.timerSnapshot).toBe(1);
    expect(h.state.getView()).toMatchObject({ dataEpoch: EPOCH, revision: 5, invalidated: 1 });
  });

  it("真实的 startEventSession：订阅那一刻起的缝里的事件都被暂存，快照之后再按水位判", async () => {
    const calls = { getRevision: 0, timerSnapshot: 0 };
    let emitted = false;
    // fix round 1 / I3：投递点在**注册那一刻**（订阅已建立、load 还没开始）。
    // 这一条是区分性的：把 startEventSession 的暂存改成直通，rev 4 会在水位还是
    // `null` 的时候到达 ⇒ 被当成跳号、**多取一次计时快照**（下面 `timerSnapshot`
    // 那条断言就是它的红点）。
    // ⚠️ 措辞订正（fix round 2）：有区分力的**只有** `timerSnapshot` 那一条——同一变异下
    // `invalidated` 仍是 1（rev 4 触发的 resync 与 rev 6 触发的 resync 被 `resyncing`
    // 合并成一次），别把 `invalidated` 说成这条用例的判据。
    events.onRegister((handler) => {
      handler({ payload: envelope({ revision: 4 }) });
    });
    const state = track(
      createDomainState({
        async getRevision() {
          calls.getRevision += 1;
          return identity(EPOCH, 5);
        },
        async timerSnapshot() {
          calls.timerSnapshot += 1;
          if (!emitted) {
            emitted = true;
            // 快照还没进水位：这两条同样必须被暂存而不是丢掉。
            events.emit(envelope({ revision: 6 }));
            events.emit(
              envelope({
                event: EVENT_TIMER_TICK,
                revision: 5,
                payload: sample({ tick_seq: 2 }),
              }),
            );
          }
          return sample({ revision: 5 });
        },
        startEventSession,
      }),
    );

    await state.start();

    expect(events.open).toBe(1);
    expect(calls.getRevision).toBe(1);
    expect(calls.timerSnapshot).toBe(1);
    const view = state.getView();
    expect(view).toMatchObject({ dataEpoch: EPOCH, revision: 5, invalidated: 1 });
    expect(view.timer?.tick_seq).toBe(2);

    await state.stop();
    expect(events.open).toBe(0);
  });

  it("启动缝里的跳号通知当场就推失效并取新快照（不等 30 秒轮询）", async () => {
    // fix round 1 / I2：`startEventSession` 的 flush 发生在它返回之前，那一刻
    // `domainState` 还没有 `stream`——用 `stream` 当判据会把这条动作静默丢掉。
    const seeded = await startWithSeam([{ revision: 4 }, { revision: 8 }]);
    expect(seeded.state.getView().invalidated).toBe(1);
    expect(seeded.calls.timerSnapshot).toBe(2);
    expect(seeded.state.getView().revision).toBe(8);
  });

  it("启动缝里单独一条跳号通知也一样（不是靠前一条通知带出来的）", async () => {
    const seeded = await startWithSeam([{ revision: 8 }]);
    expect(seeded.state.getView().invalidated).toBe(1);
    expect(seeded.calls.timerSnapshot).toBe(2);
  });

  it("启动缝里的未知 epoch 通知当场重新握手（不落 apply）", async () => {
    const seeded = await startWithSeam([{ data_epoch: OTHER_EPOCH, revision: 1 }]);
    expect(seeded.calls.getRevision).toBe(2);
    expect(seeded.state.getView().invalidated).toBe(0);
    expect(seeded.state.getView().dataEpoch).toBe(EPOCH);
  });

  it("start() 幂等：并发调用只握手一次、只开一个事件会话", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());

    await Promise.all([h.state.start(), h.state.start()]);
    await h.state.start();

    expect(h.calls.sessions).toBe(1);
    expect(h.calls.getRevision).toBe(1);
    expect(h.calls.timerSnapshot).toBe(1);
    expect(h.state.getView().phase).toBe("ready");
  });

  it("load 失败：start() 抛错、状态置 failed、订阅被撤掉（不留悬挂监听）", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(() => {
      throw new Error("握手失败");
    });

    await expect(h.state.start()).rejects.toThrow("握手失败");
    expect(h.state.getView().phase).toBe("failed");
    expect(h.calls.closes).toBe(1);
  });

  it("计时快照拿不到（协调器故障态）也不让启动失败：水位来自握手，展示等下一拍 tick", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(() => {
      throw new Error("RECOVERY_REQUIRED");
    });

    await h.state.start();

    expect(h.state.getView()).toMatchObject({
      dataEpoch: EPOCH,
      revision: 5,
      timer: null,
      phase: "ready",
    });

    // 水位已经生效：rev 6 是"紧跟着的那一版"，不是跳号（否则会多取一次快照）。
    h.emit(envelope({ revision: 6 }));
    expect(h.state.getView().invalidated).toBe(1);
    expect(h.calls.timerSnapshot).toBe(1);

    h.emit(envelope({ event: EVENT_TIMER_TICK, payload: sample({ tick_seq: 2 }) }));
    expect(h.state.getView().timer?.tick_seq).toBe(2);
  });

  it("展示值还没有基线时，另一个 epoch 的 tick 也只触发重新握手（闸门规则① 对 tick 一视同仁）", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(() => {
      throw new Error("RECOVERY_REQUIRED");
    });
    await h.state.start();
    expect(h.state.getView().timer).toBeNull();

    h.queueIdentity(identity(EPOCH, 5));
    h.emit(
      envelope({
        event: EVENT_TIMER_TICK,
        data_epoch: OTHER_EPOCH,
        payload: sample({ data_epoch: OTHER_EPOCH }),
      }),
    );
    await settle();

    expect(h.state.getView().timer).toBeNull();
    expect(h.calls.getRevision).toBe(2);
  });
});

describe("事件：只作缓存失效 / 只更新展示值", () => {
  it("乱序的通知与 tick 都不改变展示", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 3, active_ms: 3_000 }));
    await h.state.start();

    h.emit(envelope({ event: EVENT_TIMER_TICK, payload: sample({ tick_seq: 2, active_ms: 2_000 }) }));
    h.emit(
      envelope({ event: EVENT_TIMER_TICK, payload: sample({ tick_seq: 3, active_ms: 9_999 }) }),
    );
    expect(h.state.getView().timer).toMatchObject({ tick_seq: 3, active_ms: 3_000 });

    h.emit(envelope({ revision: 6 }));
    expect(h.state.getView().invalidated).toBe(1);
    h.emit(envelope({ revision: 5 }));
    h.emit(envelope({ revision: 6 }));
    expect(h.state.getView().invalidated).toBe(1);
  });

  it("domain.changed 只让缓存失效：载荷（那条命令的响应）不并进展示", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 3, active_ms: 3_000 }));
    await h.state.start();

    h.emit(
      envelope({
        revision: 6,
        payload: sample({ tick_seq: 99, active_ms: 99_000, state: "paused" }),
      }),
    );

    expect(h.state.getView().invalidated).toBe(1);
    expect(h.state.getView().timer).toMatchObject({
      tick_seq: 3,
      active_ms: 3_000,
      state: "running",
    });
  });

  it("timer.tick 只更新展示值，不推业务水位（tick 不是业务写）", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 1 }));
    await h.state.start();

    // tick 身上带着当时的 revision 9：它只是一次采样，不是"我已经应用了第 9 版"。
    h.emit(
      envelope({
        event: EVENT_TIMER_TICK,
        revision: 9,
        payload: sample({ revision: 9, tick_seq: 2 }),
      }),
    );
    expect(h.state.getView().timer?.tick_seq).toBe(2);

    // 因此 rev 6 的通知照常接纳；若 tick 推了水位，它会被闸门规则②吞掉 → 这条红。
    h.emit(envelope({ revision: 6 }));
    expect(h.state.getView().invalidated).toBe(1);
    expect(h.state.getView().revision).toBe(5);
  });

  it("形状不符的 tick 载荷与不认识的事件名都被忽略，不改展示也不抛错", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 3 }));
    await h.state.start();

    h.emit(envelope({ event: EVENT_TIMER_TICK, payload: "not a snapshot" }));
    h.emit(envelope({ event: EVENT_TIMER_TICK, payload: { data_epoch: EPOCH } }));
    h.emit(envelope({ event: "unknown.event", revision: 9 }));

    expect(h.state.getView().timer?.tick_seq).toBe(3);
    expect(h.state.getView().invalidated).toBe(0);
  });
});

describe("计时展示值的新旧判定（00 §5 的先后顺序）", () => {
  it("旧状态生成的 tick 即使序号较新也不能覆盖暂停后的展示", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(
      sample({ session_version: 2, state: "paused", tick_seq: 10, active_ms: 4_000 }),
    );
    await h.state.start();

    // 暂停之前生成的 tick 迟到了：序号 11 更大，会话版本却更旧（1 < 2）。
    h.emit(
      envelope({
        event: EVENT_TIMER_TICK,
        payload: sample({ session_version: 1, state: "running", tick_seq: 11, active_ms: 5_000 }),
      }),
    );

    expect(h.state.getView().timer).toMatchObject({
      session_version: 2,
      state: "paused",
      tick_seq: 10,
      active_ms: 4_000,
    });
    expect(h.calls.timerSnapshot).toBe(1);
  });

  it("较新 session_version 的未知 tick 先触发计时快照，不自行推导状态跃迁", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ session_version: 1, state: "running", tick_seq: 5 }));
    await h.state.start();

    // pause 已经提交（session_version 2），但我们还没拿到那份权威快照。
    h.queueSample(sample({ session_version: 2, state: "paused", tick_seq: 6, active_ms: 6_000 }));
    h.emit(
      envelope({
        event: EVENT_TIMER_TICK,
        payload: sample({ session_version: 2, state: "paused", tick_seq: 6, active_ms: 6_000 }),
      }),
    );

    // 展示仍然是 running：状态跃迁不靠 tick 推导，只靠那份快照。
    expect(h.state.getView().timer).toMatchObject({
      session_version: 1,
      state: "running",
      tick_seq: 5,
    });
    expect(h.calls.timerSnapshot).toBe(2);

    await settle();
    expect(h.state.getView().timer).toMatchObject({
      session_version: 2,
      state: "paused",
      tick_seq: 6,
    });
  });

  it("迟到的计时快照旧于已应用水位时不覆盖展示（水位判在会话版本之前）", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 9, session_version: 1, active_ms: 9_000 }));
    await h.state.start();

    // 一份自相矛盾的迟到响应：会话版本更新（v2）、revision 却更旧（4 < 水位 5）。
    // 现实里它来自一次跨越了版本边界的延迟返回；构造得"看起来更新"正是为了证明
    // 先判水位、再判 session_version/tick_seq 这条顺序真的在生效。
    h.queueSample(
      sample({ revision: 4, session_version: 2, tick_seq: 99, active_ms: 99_000 }),
    );
    h.emit(envelope({ revision: 8 })); // 闸门规则④：跳号 ⇒ 取新快照
    await settle();

    const view = h.state.getView();
    expect(view.revision).toBe(5);
    expect(view.timer).toMatchObject({ tick_seq: 9, session_version: 1, active_ms: 9_000 });
    expect(view.invalidated).toBe(1);
  });
});

describe("orderTimer 的前三级判据（跨库 / 跨 run / 跨会话）", () => {
  // fix round 1 / I1：这三条各只改**一个字段**，而且断言的是"旧 tick 不覆盖展示"。
  // 之前 42 条用例里这三行代码各自都能改成 `if (false)` 而全绿——正是计划 `:113`
  // 点名的那三级（跨 run 的旧 tick 会按 tick_seq 覆盖新 run 的展示）。
  it("判据 1（data_epoch）：另一个库的 tick 不覆盖展示，只触发重新握手", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 9 }));
    await h.state.start();

    h.queueIdentity(identity(EPOCH, 5)); // 握手回来说：库没变
    h.emit(
      envelope({
        event: EVENT_TIMER_TICK,
        data_epoch: OTHER_EPOCH,
        payload: sample({ data_epoch: OTHER_EPOCH, tick_seq: 10 }),
      }),
    );

    expect(h.state.getView().timer).toMatchObject({ data_epoch: EPOCH, tick_seq: 9 });
    await settle();
    expect(h.state.getView().timer).toMatchObject({ data_epoch: EPOCH, tick_seq: 9 });
    expect(h.calls.getRevision).toBe(2); // 只重新握手
    expect(h.calls.timerSnapshot).toBe(1); // 没把它当成"未知 tick"去取快照
  });

  it("判据 2（run_id）：另一个 run 的 tick 不覆盖展示（tick_seq 跨 run 不可比），先取计时快照", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ run_id: RUN, tick_seq: 9 }));
    await h.state.start();

    h.emit(
      envelope({
        event: EVENT_TIMER_TICK,
        payload: sample({ run_id: "run-2", tick_seq: 10 }),
      }),
    );

    expect(h.state.getView().timer).toMatchObject({ run_id: RUN, tick_seq: 9 });
    expect(h.calls.timerSnapshot).toBe(2);
    await settle();
    expect(h.state.getView().timer).toMatchObject({ run_id: RUN, tick_seq: 9 });
  });

  it("判据 3（session_id）：另一个会话的 tick 不覆盖展示（会话版本可以相同），先取计时快照", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ session_id: SESSION, session_version: 1, tick_seq: 9 }));
    await h.state.start();

    h.emit(
      envelope({
        event: EVENT_TIMER_TICK,
        payload: sample({ session_id: "session-2", session_version: 1, tick_seq: 10 }),
      }),
    );

    expect(h.state.getView().timer).toMatchObject({ session_id: SESSION, tick_seq: 9 });
    expect(h.calls.timerSnapshot).toBe(2);
    await settle();
    expect(h.state.getView().timer).toMatchObject({ session_id: SESSION, tick_seq: 9 });
  });
});

describe("重新握手与整体失效（闸门规则①）", () => {
  it("未知 epoch 的通知只触发重新握手，不落 apply", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());
    await h.state.start();

    h.queueIdentity(identity(EPOCH, 5)); // 握手回来说：库没变
    h.emit(envelope({ data_epoch: OTHER_EPOCH, revision: 1 }));
    await settle();

    expect(h.calls.getRevision).toBe(2); // 只重新握手
    expect(h.state.getView().invalidated).toBe(0); // 那条通知没有被接纳
    expect(h.state.getView().dataEpoch).toBe(EPOCH);
    expect(h.calls.timerSnapshot).toBe(1);
  });

  it("握手发现库身份变了：全部缓存失效、旧库的计时展示作废，并按新 epoch 重取样本", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 4 }));
    await h.state.start();
    h.emit(envelope({ revision: 6 }));
    expect(h.state.getView().invalidated).toBe(1);

    h.queueIdentity(identity(OTHER_EPOCH, 1));
    h.queueSample(
      sample({
        data_epoch: OTHER_EPOCH,
        revision: 1,
        session_id: null,
        session_version: null,
        state: null,
        tick_seq: 0,
        active_ms: 0,
      }),
    );
    h.emit(envelope({ data_epoch: OTHER_EPOCH, revision: 1 }));
    await settle();

    const view = h.state.getView();
    expect(view.dataEpoch).toBe(OTHER_EPOCH);
    expect(view.revision).toBe(1);
    expect(view.invalidated).toBe(2);
    expect(view.timer?.data_epoch).toBe(OTHER_EPOCH);
    expect(h.calls.timerSnapshot).toBe(2);
  });
});

describe("版本校验的调度（00 §5 规则 4）", () => {
  it("可见窗口至多每 30 秒校验一次 get_revision", async () => {
    vi.useFakeTimers();
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());
    await h.state.start();
    expect(VERIFY_INTERVAL_MS).toBe(30_000);
    expect(h.calls.getRevision).toBe(1); // 启动那次握手

    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS - 1);
    expect(h.calls.getRevision).toBe(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(h.calls.getRevision).toBe(2);
    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS);
    expect(h.calls.getRevision).toBe(3);
  });

  it("隐藏窗口不轮询，显示前校验一次", async () => {
    vi.useFakeTimers();
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());
    await h.state.start();

    setVisibility("hidden");
    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS * 3);
    expect(h.calls.getRevision).toBe(1);

    setVisibility("visible");
    await settle();
    expect(h.calls.getRevision).toBe(2);
  });

  it("末次通知丢失：周期校验发现版本靠前 ⇒ 合并刷新并取新快照", async () => {
    vi.useFakeTimers();
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample({ tick_seq: 1 }));
    await h.state.start();

    // 库里已经到第 8 版（有一条通知丢了），我们只见过第 5 版。
    h.queueIdentity(identity(EPOCH, 8));
    h.queueSample(sample({ revision: 8, tick_seq: 2 }));
    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS);
    await settle();

    const view = h.state.getView();
    expect(view.invalidated).toBe(1);
    expect(view.revision).toBe(8);
    expect(view.timer?.tick_seq).toBe(2);
  });
});

describe("生命周期：订阅、卸载与多窗口", () => {
  it("订阅入口只有一个：退订之后不再被通知（组件卸载清理的就是这一条）", async () => {
    const h = harness();
    track(h.state);
    const seen: number[] = [];
    const unsubscribe = h.state.subscribe(() => seen.push(h.state.getView().invalidated));
    expect(h.state.subscriberCount()).toBe(1);

    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());
    await h.state.start();

    const before = seen.length;
    h.emit(envelope({ revision: 6 }));
    expect(seen.length).toBe(before + 1);

    unsubscribe();
    expect(h.state.subscriberCount()).toBe(0);
    h.emit(envelope({ revision: 7 }));
    expect(seen.length).toBe(before + 1);
  });

  it("stop() 撤掉事件会话与轮询，之后不再有任何校验（无泄漏）", async () => {
    vi.useFakeTimers();
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());
    await h.state.start();

    // fix round 1 / M1：可见窗口那一个 30 秒 interval 真的挂上了，stop() 之后真的撤了
    // （把 stop() 里的 detachLifecycle() 删掉，这两条断言就红）。
    expect(vi.getTimerCount()).toBe(1);
    await h.state.stop();
    expect(vi.getTimerCount()).toBe(0);

    expect(h.calls.closes).toBe(1);
    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS * 3);
    expect(h.calls.getRevision).toBe(1);
    setVisibility("visible"); // 可见性监听也撤掉了：不会再触发校验
    await settle();
    expect(h.calls.getRevision).toBe(1);
    expect(h.state.getView()).toMatchObject({
      dataEpoch: null,
      revision: 0,
      timer: null,
      invalidated: 0,
      phase: "idle",
    });
  });

  it("以隐藏态启动：一个 interval 都不挂，显示时才校验（§5 规则 4）", async () => {
    vi.useFakeTimers();
    setVisibility("hidden");
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());
    await h.state.start();

    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS * 3);
    expect(h.calls.getRevision).toBe(1); // 90 秒里一次校验都没有

    setVisibility("visible");
    await settle();
    expect(h.calls.getRevision).toBe(2); // 显示前校验
    expect(vi.getTimerCount()).toBe(1); // 并且开始轮询
  });

  it("start → stop → start：第二次启动必须真的重新就绪，而不是复用被作废的那一次", async () => {
    // fix round 2：StrictMode 的 mount→cleanup→mount 与 Task 4 的窗口生命周期正好走这条路径。
    vi.useFakeTimers();
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());

    const first = h.state.start(); // 第一次启动还在飞
    await h.state.stop(); // 还没落地就停
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());
    // ⚠️ 关键：**不 await 第一次**就再起一次（StrictMode 的 mount→cleanup→mount 就是这样）。
    // 若第二次 start() 复用了那个已被作废的 promise，它只会把会话关掉、不会重新开。
    const second = h.state.start();
    await Promise.all([first, second]);

    expect(h.calls.closes).toBe(1); // 旧的那次只把会话关掉，不复活

    // 订阅真的开起来了（不是只 resolve）
    expect(h.state.getView().phase).toBe("ready");
    expect(h.calls.sessions).toBe(2);
    expect(h.calls.closes).toBe(1);
    // 轮询真的挂上了
    expect(vi.getTimerCount()).toBe(1);
    // 失效计数真的会动
    h.emit(envelope({ revision: 6 }));
    expect(h.state.getView().invalidated).toBe(1);
  });

  it("启动过程中 stop()：会话不会被复活（订阅撤掉、不挂轮询、不写回状态）", async () => {
    const h = harness();
    track(h.state);
    h.queueIdentity(identity(EPOCH, 5));
    h.queueSample(sample());

    const starting = h.state.start();
    await h.state.stop(); // load 还没走完就停
    await starting;

    expect(h.calls.closes).toBe(1);
    expect(h.state.getView()).toMatchObject({ dataEpoch: null, phase: "idle" });
  });

  it("两个实例互不共享内存（多窗口各自一个 JS 上下文）", async () => {
    const a = harness();
    const b = harness();
    track(a.state);
    track(b.state);
    a.queueIdentity(identity(EPOCH, 5));
    a.queueSample(sample({ tick_seq: 1 }));
    b.queueIdentity(identity(OTHER_EPOCH, 9));
    b.queueSample(sample({ data_epoch: OTHER_EPOCH, revision: 9, tick_seq: 7 }));
    await a.state.start();
    await b.state.start();

    a.emit(envelope({ revision: 6 }));
    a.emit(envelope({ event: EVENT_TIMER_TICK, payload: sample({ tick_seq: 2 }) }));

    expect(a.state.getView()).toMatchObject({ dataEpoch: EPOCH, invalidated: 1, revision: 5 });
    expect(a.state.getView().timer?.tick_seq).toBe(2);
    expect(b.state.getView()).toMatchObject({
      dataEpoch: OTHER_EPOCH,
      revision: 9,
      invalidated: 0,
    });
    expect(b.state.getView().timer?.tick_seq).toBe(7);
  });
});
