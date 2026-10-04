/**
 * 双窗口同步实验（P7 Task 6a）的**事件通道替身**：一个可脚本化的 `worktrace:event` 频道。
 *
 * ⚠️ **这是实验用的注入手段，不是生产 API。**
 *
 * 它只被 `dualContextSync.test.ts` 通过 `vi.mock("@tauri-apps/api/event")` 装进**那一条
 * 用例文件**的模块图里；生产代码一个签名字段都没为它改过——注入点落在生产代码**本来就要
 * 调用的那个边界**上（Tauri 的 `listen`），所以实验里跑的是真实的
 * `src/ipc.ts::startEventSession`（先订阅并暂存 → `load()` → 按原顺序交付 → 转直通）
 * 与真实的 `src/state/domainState.ts`。替身只回答一件事：「这条通知投给谁、什么时候投、
 * 投不投」。
 *
 * ## 三种竞态的注入手段（都只在这一层，且只在这条用例里）
 *
 * | 竞态 | 注入原语 | 造出来的样子 |
 * | --- | --- | --- |
 * | (a) 末次事件丢失 | {@link createTransport} 的 `dropNext()` | 下一条广播**谁都收不到**（不会投给任何上下文） |
 * | (b) 旧响应晚到 | 不在这一层：`syncLab.ts` 的 `holdNextRead()` | 把一次**查询响应**先算好、扣住，稍后再交回 |
 * | (c) 乱序 / 跳号 | `holdNext()` + `broadcast()` + `releaseHeld()` | 把第 N 版扣住、先投第 N+1 版（跳号），再把第 N 版迟到投出 |
 *
 * 三条都不改生产签名，也不进打包产物：本文件在 `__tests__/` 下，只有测试会 import 它。
 */

/** 一条广播的结局（用例据此断言"注入真的生效了"，而不是只信自己的注释）。 */
export type Delivery = "delivered" | "dropped" | "held";

/** Tauri `listen` 的回调形状（`event.payload` 才是信封）。 */
type RawHandler = (event: { payload: unknown }) => void;

export interface EventTransport {
  /**
   * Tauri `listen` 的逐字替身：**每个调用方各自一条订阅**。
   *
   * 这正是"每个 JS 上下文一个订阅入口"在传输层的形状：两个上下文各调一次 `listen`
   * 就是两条记录，广播时逐条投递，彼此之间没有任何共享内存。
   */
  listen(channel: string, handler: RawHandler): Promise<() => void>;
  /** 广播一条通知（每个当前订阅各收到一次）。 */
  broadcast(envelope: unknown): Delivery;
  /** 注入 (a)：下一条广播**谁都收不到**（末次事件丢失）。 */
  dropNext(): void;
  /** 注入 (c)：把下一条广播**扣住**，等 {@link EventTransport.releaseHeld} 再投。 */
  holdNext(): void;
  /** 把扣住的按**扣住的顺序**投出去；返回真的投了几条。 */
  releaseHeld(): number;
  /** 当前订阅数（每个上下文一条；`stop()` 之后应回到 0）。 */
  subscribers(): number;
  /** 诊断计数：投递/丢弃/扣住各几次。 */
  readonly stats: { delivered: number; dropped: number; held: number };
  /** 用例之间复位（清订阅、清注入、清计数）。 */
  reset(): void;
}

/** 建一个事件通道替身。生产路径不会调用它。 */
export function createTransport(): EventTransport {
  const handlers = new Map<number, RawHandler>();
  /** 扣住的通知（按扣住顺序），放行时按这个顺序投。 */
  const held: unknown[] = [];
  let nextId = 1;
  /** 下一条广播的处置。**只影响一条**，投完就回到 `deliver`。 */
  let plan: "deliver" | "drop" | "hold" = "deliver";
  const stats = { delivered: 0, dropped: 0, held: 0 };

  function deliver(envelope: unknown): number {
    let count = 0;
    for (const handler of [...handlers.values()]) {
      handler({ payload: envelope });
      count += 1;
    }
    stats.delivered += count;
    return count;
  }

  return {
    async listen(_channel, handler) {
      const id = nextId++;
      handlers.set(id, handler);
      return async () => {
        handlers.delete(id);
      };
    },

    broadcast(envelope) {
      if (plan === "drop") {
        plan = "deliver";
        stats.dropped += 1;
        return "dropped";
      }
      if (plan === "hold") {
        plan = "deliver";
        stats.held += 1;
        held.push(envelope);
        return "held";
      }
      deliver(envelope);
      return "delivered";
    },

    dropNext() {
      plan = "drop";
    },

    holdNext() {
      plan = "hold";
    },

    releaseHeld() {
      const queued = held.splice(0);
      for (const envelope of queued) deliver(envelope);
      return queued.length;
    },

    subscribers: () => handlers.size,

    stats,

    reset() {
      handlers.clear();
      held.length = 0;
      plan = "deliver";
      stats.delivered = 0;
      stats.dropped = 0;
      stats.held = 0;
    },
  };
}

/** 用例文件里那一条 `vi.mock("@tauri-apps/api/event")` 拿的就是它。 */
export const transport = createTransport();
