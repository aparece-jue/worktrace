/**
 * 计时页用例（P7 Task 3）：四个动作各对应**一条**命令、展示值全部来自 P2 的 DTO、
 * 到点只提示不自动完成。
 *
 * 这里最要紧的三条：
 *
 * 1. **不做本地状态机**——点击之后按钮集合**不变**（状态要等 Rust 提交后推来的那一拍
 *    `timer.tick` 或重取的快照说它变了）；
 * 2. **展示值就是 DTO 的值**——`active_ms` / `pending_ms` / `remaining_ms` / `overtime_ms`
 *    原样格式化，前端不读时钟、不做减法；
 * 3. **任务身份与标题只有快照一个来源**——页面不再有任何 props：挂载时手上只有快照
 *    （冷启动与本窗口刚 `start` 完是同一条路径），「继续」的请求五个字段全部来自它。
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  type RenderResult,
} from "@testing-library/react";
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
import { domainState } from "../../state/domainState";
import { AT, EPOCH, SESSION, createBackend, runningSnapshot, type Backend } from "./fakeBackend";

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

/** 挂上页面并启动镜像。**页面没有 props**：任务身份与标题都只能从快照来。 */
async function mountTimer(): Promise<RenderResult> {
  const view = render(<Timer />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("timer_snapshot")).toBeGreaterThan(0));
  return view;
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
    const { container } = await mountTimer();

    expect(screen.getByText(/当前没有正在计时的会话/)).not.toBeNull();
    expect(screen.queryByTestId("pause-button")).toBeNull();
    expect(screen.queryByTestId("resume-button")).toBeNull();
    expect(screen.queryByTestId("finish-button")).toBeNull();
    // M1：没有会话就没有标题，也不留任何占位文案。
    expect(screen.queryByTestId("timer-task-title")).toBeNull();
    expect(container.textContent).not.toContain("本窗口不知道");
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

  it("冷启动（只有快照）：已暂停 ⇒「继续」可用，请求五个字段逐个等于快照值", async () => {
    // 反向验证：把 `buildResumeRequest` 的取值换成硬编码常量（或让页面自己拼一份身份）
    // ⇒ 下面那条 `toEqual` 红。「本窗口刚点完开始」现在走的是**同一条**路径。
    backend = createBackend();
    backend.snapshot = runningSnapshot({
      state: "paused",
      session_version: 2,
      task_id: "task-42",
      task_row_version: 9,
      task_title: "写季报",
    });
    await mountTimer();

    fireEvent.click(screen.getByTestId("resume-button"));

    await waitFor(() => expect(backend.count("resume_timer")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "resume_timer")?.request).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "task-42",
      task_expected_version: 9,
      session_id: SESSION,
      session_expected_version: 2,
    });
  });

  it("结束：一条 finish_timer，请求仍只取会话身份", async () => {
    backend = createBackend();
    backend.snapshot = runningSnapshot();
    await mountTimer();

    fireEvent.click(screen.getByTestId("finish-button"));

    await waitFor(() => expect(backend.count("finish_timer")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "finish_timer")?.request).toEqual({
      expected_data_epoch: EPOCH,
      session_id: SESSION,
      session_expected_version: 1,
    });
  });

  it("快照不自洽（缺标题）：「继续」不出现，界面也没有占位文案；暂停/结束不受影响", async () => {
    // 一致性判据（I2 的裁决）：只有 `state == Paused` 且任务三件齐备才给入口。
    // 反向验证：把标题改回占位常量（`?? "（本窗口不知道的任务）"`）⇒ 下面那两条红。
    backend = createBackend();
    backend.snapshot = runningSnapshot({ state: "paused", session_version: 2, task_title: null });
    const { container } = await mountTimer();

    expect(screen.queryByTestId("resume-button")).toBeNull();
    expect(screen.queryByTestId("timer-task-title")).toBeNull();
    expect(screen.getByTestId("finish-button")).not.toBeNull();
    expect(container.textContent).not.toContain("本窗口不知道");
  });
});

describe("计时页：展示值全部来自 DTO", () => {
  it("标题只有快照一个来源：有标题就原样显示，没有就整段不渲染（不编占位文案）", async () => {
    backend = createBackend();
    backend.snapshot = runningSnapshot({ task_title: "写季报" });
    const { container } = await mountTimer();

    expect(screen.getByTestId("timer-task-title").textContent).toBe("写季报");

    const untitled = runningSnapshot({ task_title: null, tick_seq: 2, revision: 6 });
    backend.snapshot = untitled;
    await act(async () => {
      events.emit(tick(untitled));
    });

    expect(screen.queryByTestId("timer-task-title")).toBeNull();
    expect(screen.getByText("运行中")).not.toBeNull();
    expect(container.textContent).not.toContain("本窗口不知道");
  });

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
