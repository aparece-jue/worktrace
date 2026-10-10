/**
 * 托盘视图跳转的接收侧用例（P8 Task 7）。
 *
 * 钉的是 Rust 那条定向事件到「切页」之间的这一段：**载荷 → `requestPage` 参数**，
 * 以及认不出载荷时的姿势（什么都不做 + 诊断，不 panic）。真实托盘点击到不了这里
 * （集成测试里没有事件循环与托盘），所以上游那张表在
 * `src-tauri/tests/shell_lifecycle.rs`，页面真的切没切、输入框真的有没有焦点在
 * `src/__tests__/App.test.tsx`。
 *
 * `listen` 走本地替身（与 `Inbox.test.tsx` / `App.test.tsx` 同一个做法）：
 * 生产代码本来就只通过这个函数订阅，改它等于测试替身，不是改协议。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const bus = vi.hoisted(() => {
  const listeners = new Set<(event: { payload: unknown }) => void>();
  /**
   * 订阅过的频道名（按调用顺序）。
   *
   * 收集它是**判据的一部分**（fix round 1，评审 Important-1）：`installTrayViewRequests`
   * 里的 `listen(TRAY_VIEW_EVENT, …)` 若写成别的名字，事件就永远收不到——而"发一次事件看看"
   * 这类用例在**替身**上照样绿（旧替身把频道参数丢了）。所以照 `ipc.test.ts` 的写法把它留下来断言。
   * 反向验证：把那个 `listen` 的第一个实参改成 `"worktrace:event"` ⇒
   * `expect(bus.channels).toEqual([TRAY_VIEW_EVENT])` 红。
   */
  const channels: string[] = [];
  return {
    channels,
    get count() {
      return listeners.size;
    },
    async listen(channel: string, handler: (event: { payload: unknown }) => void) {
      channels.push(channel);
      listeners.add(handler);
      return async () => {
        listeners.delete(handler);
      };
    },
    emit(payload: unknown) {
      for (const handler of [...listeners]) handler({ payload });
    },
    reset() {
      listeners.clear();
      channels.length = 0;
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: bus.listen }));

import { installTrayViewRequests, trayViewRequest, TRAY_VIEW_EVENT } from "../trayViewRequests";
import { onPageRequest, requestPage, type PageKey } from "../state/pageRequest";

/** 一次跳转的记录（`requestPage` 的三个参数）。 */
type Seen = { page: PageKey; notice: string | undefined; focus: string | undefined };

/** 订阅 `requestPage`，把每次跳转记下来；返回记录与退订函数。 */
function recordPages(): { seen: Seen[]; stop: () => void } {
  const seen: Seen[] = [];
  const stop = onPageRequest((page, notice, intent) => {
    seen.push({ page, notice, focus: intent?.focus });
  });
  return { seen, stop };
}

let warn: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  bus.reset();
  warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  warn.mockRestore();
  bus.reset();
});

describe("托盘跳转载荷的解释（纯函数）", () => {
  it("快速捕获 ⇒ 收件箱 + 聚焦意图", () => {
    expect(trayViewRequest({ page: "inbox", focus: true })).toEqual({
      page: "inbox",
      intent: { focus: "capture" },
    });
  });

  it("当前任务 ⇒ 计时视图，不带聚焦意图", () => {
    expect(trayViewRequest({ page: "timer", focus: false })).toEqual({
      page: "timer",
      intent: undefined,
    });
  });

  it("认不出的页名/形状一律 null（不猜、不抛）", () => {
    // 页名不在托盘能要求的那两页里：**不切页**——猜一个页面出来就是把用户带走。
    expect(trayViewRequest({ page: "recovery", focus: false })).toBeNull();
    expect(trayViewRequest({ page: "", focus: false })).toBeNull();
    expect(trayViewRequest({ focus: true })).toBeNull();
    expect(trayViewRequest({ page: 7, focus: false })).toBeNull();
    expect(trayViewRequest(null)).toBeNull();
    expect(trayViewRequest(undefined)).toBeNull();
    expect(trayViewRequest("inbox")).toBeNull();
  });

  it("`focus` 缺字段或给了别的值时只当「不聚焦」，仍然切页", () => {
    expect(trayViewRequest({ page: "inbox" })).toEqual({ page: "inbox", intent: undefined });
    expect(trayViewRequest({ page: "inbox", focus: "capture" })).toEqual({
      page: "inbox",
      intent: undefined,
    });
  });
});

describe("托盘跳转的订阅", () => {
  it("订阅的是托盘那条事件名（频道名逐字断言），收到载荷就切到对应的页", async () => {
    const { seen, stop } = recordPages();
    const off = await installTrayViewRequests();
    expect(bus.count).toBe(1);
    // 频道名是判据：写成 `worktrace:event` 之类的别的名字时，本文件其余用例（靠 `emit` 直接
    // 投递）**照样绿**，只有这一句能红（fix round 1 / Important-1）。
    expect(bus.channels).toEqual([TRAY_VIEW_EVENT]);

    bus.emit({ page: "inbox", focus: true });
    bus.emit({ page: "timer", focus: false });

    expect(seen).toEqual([
      { page: "inbox", notice: undefined, focus: "capture" },
      { page: "timer", notice: undefined, focus: undefined },
    ]);

    off();
    stop();
  });

  it("认不出的载荷：什么都不做 + 一条诊断（不 panic）", async () => {
    const { seen, stop } = recordPages();
    const off = await installTrayViewRequests();

    expect(() => bus.emit({ page: "nope", focus: true })).not.toThrow();
    expect(seen).toEqual([]);
    expect(warn).toHaveBeenCalledTimes(1);

    off();
    stop();
  });

  it("退订之后不再有任何反应（不留悬挂监听）", async () => {
    const { seen, stop } = recordPages();
    const off = await installTrayViewRequests();
    await off();
    expect(bus.count).toBe(0);

    bus.emit({ page: "inbox", focus: true });
    expect(seen).toEqual([]);

    stop();
  });

  it("事件名与 Rust 常量同名（名字写错就是点了没反应）", () => {
    // 另一侧（读 Rust 源码核对同一个字面量）在
    // `src/types/__tests__/event-constants.test.ts`——那边只在仓库侧跑得起来。
    expect(TRAY_VIEW_EVENT).toBe("worktrace:tray-view");
  });
});

describe("切页口的附加意图", () => {
  it("`requestPage` 把意图原样交给处置者（第三个可选参数）", () => {
    const { seen, stop } = recordPages();
    requestPage("inbox", undefined, { focus: "capture" });
    requestPage("recovery", "存在待确认的计时记录，请先处理恢复再继续。");
    expect(seen).toEqual([
      { page: "inbox", notice: undefined, focus: "capture" },
      {
        page: "recovery",
        notice: "存在待确认的计时记录，请先处理恢复再继续。",
        focus: undefined,
      },
    ]);
    stop();
  });

  it("处置者只声明两个参数也能用（M8 那条路径不受影响）", () => {
    const seen: PageKey[] = [];
    const stop = onPageRequest((page) => seen.push(page));
    requestPage("recovery", "x", { focus: "capture" });
    expect(seen).toEqual(["recovery"]);
    stop();
  });
});
