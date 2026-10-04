/**
 * `src/state/hooks.ts` 的用例：页面通过 hooks 读**同一个** `domainState` 入口，
 * 事件到了会重新渲染，卸载之后订阅清零（不泄漏）。
 *
 * 这里跑的是**真实的单例**（`domainState` 用 `src/ipc.ts` 的真实依赖建出来的），
 * 所以 IPC 边界用官方 `mockIPC` 与一个 `listen` 替身挡住——它同时证明了
 * "hooks 自己一条命令都不发"（读的只是镜像）。
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";

const events = vi.hoisted(() => {
  const listeners = new Map<number, (event: { payload: unknown }) => void>();
  let next = 1;
  return {
    get open() {
      return listeners.size;
    },
    async listen(_channel: string, handler: (event: { payload: unknown }) => void) {
      const id = next++;
      listeners.set(id, handler);
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

import {
  EVENT_DOMAIN_CHANGED,
  EVENT_TIMER_TICK,
  type EventEnvelope,
  type TimerSnapshot,
} from "../../types/ipc";
import { domainState } from "../domainState";
import { useInvalidation, useTimerSnapshot } from "../hooks";

const EPOCH = "epoch-a";
const RUN = "run-1";
const AT = 1_700_000_000_000;

function sample(tickSeq: number): TimerSnapshot {
  return {
    data_epoch: EPOCH,
    revision: 5,
    run_id: RUN,
    session_id: "session-1",
    session_version: 1,
    task_id: "task-1",
    task_row_version: 1,
    tick_seq: tickSeq,
    as_of: AT,
    active_ms: tickSeq * 1_000,
    pending_ms: null,
    state: "running",
    timer_kind: "stopwatch",
    remaining_ms: null,
    overtime_ms: null,
  };
}

function envelope(overrides: Partial<EventEnvelope>): EventEnvelope {
  return {
    data_epoch: EPOCH,
    event: EVENT_DOMAIN_CHANGED,
    revision: 5,
    at: AT,
    payload: null,
    ...overrides,
  };
}

/** 一条 `timer.tick`：只更新展示值。 */
function tick(tickSeq: number, revision = 5): EventEnvelope {
  return envelope({ event: EVENT_TIMER_TICK, revision, payload: sample(tickSeq) });
}

let renders = 0;
let commands: string[] = [];

/** 两个 hook 各订阅一次；`renders` 用来证明卸载之后确实不再重渲染。 */
function Probe() {
  const timer = useTimerSnapshot();
  const invalidated = useInvalidation();
  renders += 1;
  return (
    <div>
      <span data-testid="seq">{timer?.tick_seq ?? "none"}</span>
      <span data-testid="invalidated">{invalidated}</span>
    </div>
  );
}

beforeEach(async () => {
  renders = 0;
  commands = [];
  mockIPC((command) => {
    commands.push(command);
    if (command === "get_revision") return { data_epoch: EPOCH, revision: 5 };
    if (command === "timer_snapshot") return sample(1);
    throw new Error(`这条用例没有脚本化命令 ${command}`);
  });
  await domainState.start();
});

afterEach(async () => {
  cleanup();
  await domainState.stop();
  clearMocks();
});

describe("hooks", () => {
  it("挂载与卸载都不发命令：读的是镜像，不是 IPC", () => {
    const { unmount } = render(<Probe />);
    unmount();
    expect(commands).toEqual(["get_revision", "timer_snapshot"]);
  });

  it("事件到达后重新渲染（同一个订阅入口），卸载后订阅清零", async () => {
    const { unmount } = render(<Probe />);
    expect(screen.getByTestId("seq").textContent).toBe("1");
    expect(domainState.subscriberCount()).toBe(2);

    await act(async () => {
      events.emit(tick(2));
    });
    expect(screen.getByTestId("seq").textContent).toBe("2");

    unmount();
    expect(domainState.subscriberCount()).toBe(0);
  });

  it("卸载后不再有监听泄漏：后续事件不会再把组件渲染一遍", async () => {
    const { unmount } = render(<Probe />);
    const before = renders;
    unmount();
    expect(domainState.subscriberCount()).toBe(0);

    await act(async () => {
      events.emit(envelope({ revision: 6 }));
      events.emit(tick(3));
    });
    expect(renders).toBe(before);
  });

  it("useInvalidation 反映事件带来的缓存失效（事件只作失效，不搬数据）", async () => {
    render(<Probe />);
    expect(screen.getByTestId("invalidated").textContent).toBe("0");

    await act(async () => {
      events.emit(envelope({ revision: 6 }));
    });
    expect(screen.getByTestId("invalidated").textContent).toBe("1");
  });
});
