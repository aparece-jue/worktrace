/**
 * 计时页用例（P7 Task 3）：四个动作各对应**一条**命令、展示值全部来自 P2 的 DTO、
 * 到点只提示不自动完成。
 *
 * 这里最要紧的两条：
 *
 * 1. **不做本地状态机**——点击之后按钮集合**不变**（状态要等 Rust 提交后推来的那一拍
 *    `timer.tick` 或重取的快照说它变了）；
 * 2. **展示值就是 DTO 的值**——`active_ms` / `pending_ms` / `remaining_ms` / `overtime_ms`
 *    原样格式化，前端不读时钟、不做减法。
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { clearMocks } from "@tauri-apps/api/mocks";

const events = vi.hoisted(() => {
  const listeners = new Map<number, (event: { payload: unknown }) => void>();
  let next = 1;
  return {
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
    reset() {
      listeners.clear();
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: events.listen }));

import {
  EVENT_TIMER_TICK,
  type CommandOutcome,
  type EventEnvelope,
  type TimerSnapshot,
} from "../../types/ipc";
import { Timer } from "../Timer";
import type { TaskIdentity } from "../../components/timerRequests";
import { domainState } from "../../state/domainState";
import { AT, EPOCH, SESSION, createBackend, runningSnapshot, type Backend } from "./fakeBackend";

/** 本上下文启动的那条会话属于哪个任务（收件箱页交给外壳，外壳再传进来）。 */
const TASK: TaskIdentity = { id: "t-9", title: "写周报", row_version: 4 };

let backend: Backend;

/** 一条 `timer.tick`（载荷就是 `TimerSnapshot`）。 */
function tick(snapshot: TimerSnapshot): EventEnvelope {
  return {
    data_epoch: snapshot.data_epoch,
    event: EVENT_TIMER_TICK,
    revision: snapshot.revision,
    at: AT,
    payload: snapshot,
  };
}

function deferred<T>(): { promise: Promise<T>; resolve(value: T): void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

/** 挂上页面并启动镜像。 */
async function mountTimer(
  currentTask: TaskIdentity | null = TASK,
  onSessionEnded: () => void = () => undefined,
): Promise<void> {
  render(<Timer currentTask={currentTask} onSessionEnded={onSessionEnded} />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("timer_snapshot")).toBeGreaterThan(0));
}

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
});

describe("计时页：四个动作各一条命令", () => {
  it("没有活动会话：只给指引，一个动作按钮都没有，也不发命令", async () => {
    backend = createBackend(); // 默认就是 idle 快照
    await mountTimer();

    expect(screen.getByText(/当前没有正在计时的会话/)).not.toBeNull();
    expect(screen.queryByTestId("pause-button")).toBeNull();
    expect(screen.queryByTestId("resume-button")).toBeNull();
    expect(screen.queryByTestId("finish-button")).toBeNull();
    expect(backend.commands).toEqual(["get_revision", "timer_snapshot"]);
  });

  it("运行中：暂停一条命令；点击本身不改按钮（状态由 DTO 决定，不做本地状态机）", async () => {
    // 反向验证：在 `pause()` 成功之后本地把状态改成 paused（或让按钮读本地状态）
    // ⇒ "命令响应之后按钮仍然是暂停" 那两句红。
    backend = createBackend();
    backend.snapshot = runningSnapshot({ active_ms: 61_000 });
    await mountTimer();

    expect(screen.getByText("01:01")).not.toBeNull();
    expect(screen.getByTestId("pause-button")).not.toBeNull();
    expect(screen.queryByTestId("resume-button")).toBeNull();

    const gate = deferred<CommandOutcome>();
    backend.hold.pause_timer = gate.promise;
    backend.commands.length = 0;
    backend.requests.length = 0;
    fireEvent.click(screen.getByTestId("pause-button"));

    await waitFor(() => expect(backend.count("pause_timer")).toBe(1));
    expect(backend.requests[0]).toEqual({
      command: "pause_timer",
      request: {
        expected_data_epoch: EPOCH,
        session_id: SESSION,
        session_expected_version: 1,
      },
    });
    // 命令还在飞：按钮集合没有变。
    expect(screen.queryByTestId("resume-button")).toBeNull();

    // Rust 提交之后的响应（快照已经是 paused）——它**不进展示**：页面不维护第二份状态。
    const paused = runningSnapshot({
      active_ms: 61_000,
      state: "paused",
      session_version: 2,
      tick_seq: 2,
      revision: 6,
    });
    backend.snapshot = paused;
    await act(async () => {
      gate.resolve({ snapshot: paused, revision: 6, task_version: 4 });
    });
    expect(screen.queryByTestId("resume-button")).toBeNull();

    // 状态真的变了（Rust 推来的那一拍）才换按钮。
    await act(async () => {
      events.emit(tick(paused));
    });
    expect(await screen.findByTestId("resume-button")).not.toBeNull();
    expect(screen.queryByTestId("pause-button")).toBeNull();
  });

  it("已暂停：继续一条命令，请求带任务与会话**两份**版本", async () => {
    backend = createBackend();
    backend.snapshot = runningSnapshot({ state: "paused", session_version: 2 });
    await mountTimer();

    fireEvent.click(screen.getByTestId("resume-button"));

    await waitFor(() => expect(backend.count("resume_timer")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "resume_timer")?.request).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "t-9",
      task_expected_version: 4,
      session_id: SESSION,
      session_expected_version: 2,
    });
  });

  it("结束：一条 finish_timer；响应里没有会话了 ⇒ 通知外壳放下任务身份", async () => {
    backend = createBackend();
    backend.snapshot = runningSnapshot();
    const ended: number[] = [];
    await mountTimer(TASK, () => ended.push(1));

    fireEvent.click(screen.getByTestId("finish-button"));

    await waitFor(() => expect(backend.count("finish_timer")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "finish_timer")?.request).toEqual({
      expected_data_epoch: EPOCH,
      session_id: SESSION,
      session_expected_version: 1,
    });
    await waitFor(() => expect(ended).toEqual([1]));
  });

  it("冷启动（本窗口不知道任务）：暂停态下不出现「继续」，暂停/结束不受影响", async () => {
    // 契约里没有「会话 → 任务」的读路径（见 `components/timerRequests.ts`）：
    // `resume_timer` 的请求构造不出来时，按钮**不出现**——而不是点了再失败。
    backend = createBackend();
    backend.snapshot = runningSnapshot({ state: "paused", session_version: 2 });
    await mountTimer(null);

    expect(screen.queryByTestId("resume-button")).toBeNull();
    expect(screen.getByText(/不知道它属于哪个任务/)).not.toBeNull();
    expect(screen.getByTestId("finish-button")).not.toBeNull();
  });
});

describe("计时页：展示值全部来自 DTO", () => {
  it("暂停值冻结：状态变了、数值仍是 Rust 冻结的那个（前端不读时钟、不做减法）", async () => {
    backend = createBackend();
    backend.snapshot = runningSnapshot({ active_ms: 61_000 });
    await mountTimer();
    expect(screen.getByText("01:01")).not.toBeNull();

    const paused = runningSnapshot({
      active_ms: 61_000,
      pending_ms: 30_000,
      state: "paused",
      session_version: 2,
      tick_seq: 2,
      revision: 6,
    });
    backend.snapshot = paused;
    await act(async () => {
      events.emit(tick(paused));
    });

    expect(await screen.findByText("已暂停")).not.toBeNull();
    // 冻结值与待确认段都原样展示（待确认与已计时**分列**，不混在一起）
    expect(screen.getByText("01:01")).not.toBeNull();
    expect(screen.getByText("00:30")).not.toBeNull();
    expect(screen.getByText(/待确认/)).not.toBeNull();
  });

  it("到点只提示、不自动完成：倒计时超时只弹提示，一条命令都不发", async () => {
    // 反向验证：让页面在超时时自动发 `finish_timer`（或自动改任务状态）
    // ⇒ 最后那两条"命令清单"断言红。
    backend = createBackend();
    backend.snapshot = runningSnapshot({
      timer_kind: "countdown",
      active_ms: 300_000,
      remaining_ms: 0,
      overtime_ms: 12_000,
      revision: 6,
    });
    await mountTimer();

    expect(await screen.findByText(/已到目标时长/)).not.toBeNull();
    expect(screen.getByText("05:00")).not.toBeNull();
    expect(screen.getByText("00:12")).not.toBeNull();

    // 到点**什么写命令都不发**（会话不自动结束、任务也不自动完成）。
    expect(backend.commands).toEqual(["get_revision", "timer_snapshot"]);
    expect(backend.commands).not.toContain("finish_timer");
  });
});
