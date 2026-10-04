/**
 * 收件箱页用例（P7 Task 3）：F-001 快速捕获 + F-002 任务理清。
 *
 * 断言口径（总纲 §5 第 8 条）：这里只断言**展示与转发**——状态合法性、事务边界都在 Rust。
 * 两条真正属于前端的判据也在这里钉住：① 空白输入不发命令；② 什么状态下**不出现**入口
 * （`start` 是一条命令，前端不自己拆两步）。
 *
 * 假后端（`fakeBackend.ts`）用官方的 `mockIPC` 挡住 IPC 边界，事件走本地替身；
 * 页面读的是**真实的** `domainState` 单例，所以"页面自己不开订阅"这件事也顺带被钉住。
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

import { Inbox } from "../Inbox";
import type { TaskIdentity } from "../../components/timerRequests";
import { domainState } from "../../state/domainState";
import { EPOCH, createBackend, failure, project, task, type Backend } from "./fakeBackend";

let backend: Backend;

/** 挂上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。 */
async function mountInbox(
  onSessionStarted: (started: TaskIdentity) => void = () => undefined,
): Promise<void> {
  render(<Inbox onSessionStarted={onSessionStarted} />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("list_tasks")).toBeGreaterThan(0));
}

/** 在捕获输入框里敲一句话并回车。 */
function capture(text: string): void {
  const input = screen.getByPlaceholderText(/输入一句话/);
  fireEvent.change(input, { target: { value: text } });
  fireEvent.keyDown(input, { key: "Enter" });
  fireEvent.keyUp(input, { key: "Enter" });
}

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
});

describe("收件箱：F-001 快速捕获", () => {
  it("空标题被拒：一条命令都不发，提示留在界面上", async () => {
    // 反向验证：把 `title === ""` 那道前置判据去掉（或改成不 return），
    // `create_task` 会被发出去 ⇒ 下面 `toBe(0)` 与那句提示都会红。
    backend = createBackend();
    await mountInbox();
    const before = backend.count("create_task");

    capture("   ");

    expect((await screen.findByRole("alert")).textContent).toContain("请输入任务标题");
    expect(backend.count("create_task")).toBe(before);
    expect(backend.commands).not.toContain("create_task");
  });

  it("回车创建后列表立即可见（不等任何事件），请求带库身份与可选项目", async () => {
    // 反向验证：把成功后的 `afterWrite()`（重拉）删掉，且用例不投递 `domain.changed`
    // ⇒ 新标题不会出现在列表里，`findByText("写周报")` 超时红。
    backend = createBackend();
    backend.projects = [project({ id: "p-1", name: "项目甲" })];
    await mountInbox();
    expect(await screen.findByText(/还没有任务/)).not.toBeNull();

    capture("写周报");

    expect(await screen.findByText("写周报")).not.toBeNull();
    expect(backend.requests.find((entry) => entry.command === "create_task")?.request).toEqual({
      expected_data_epoch: EPOCH,
      title: "写周报",
      project_id: null,
    });
  });

  it("服务拒绝时提示正确：上屏的就是 Rust 的 message（DOMAIN_ERROR 只决定行为）", async () => {
    // 反向验证：把 `reportCommandError` 的返回值换成 "未知错误"（或自己拼一句话），
    // 这句断言就红——文案只有 Rust 一个来源。
    backend = createBackend();
    backend.fail.create_task = failure({
      code: "DOMAIN_ERROR",
      message: "任务标题不能为空。",
    });
    await mountInbox();

    capture("写周报");

    expect((await screen.findByRole("alert")).textContent).toBe("任务标题不能为空。");
  });

  it("未知 code 也直接展示 message，而不是「未知错误」", async () => {
    // 反向验证：给 `commandErrorAction` 加一条"未知 code ⇒ 兜底文案"的分支，
    // 下面这句就会变成兜底文案而红。
    backend = createBackend();
    backend.fail.create_task = failure({
      code: "SOMETHING_NEW",
      message: "库里出了点怪事。",
    });
    await mountInbox();

    capture("写周报");

    expect((await screen.findByRole("alert")).textContent).toBe("库里出了点怪事。");
  });

  it("requires_handshake ⇒ 先重新握手，且不自动重试那条命令", async () => {
    // 反向验证：把 `requires_handshake` 那一支删掉 ⇒ `get_revision` 不会再来一次；
    // 若改成"重新握手 + 自动重发"，`create_task` 就会变成 2 次。
    backend = createBackend();
    backend.fail.create_task = failure({
      code: "DATA_EPOCH_MISMATCH",
      message: "数据已换库，请重新连接。",
      requires_handshake: true,
    });
    await mountInbox();
    const before = backend.count("get_revision");

    capture("写周报");

    expect((await screen.findByRole("alert")).textContent).toBe("数据已换库，请重新连接。");
    await waitFor(() => expect(backend.count("get_revision")).toBe(before + 1));
    expect(backend.count("create_task")).toBe(1);
  });

  it("项目选择列表只用 list_selectable_projects（「只含 active」是服务端的保证，前端不自己过滤）", async () => {
    // 反向验证：把这条命令换成 `list_projects`（会带回归档项目）⇒ 计数断言红。
    backend = createBackend();
    backend.projects = [project({ id: "p-active", name: "在办项目" })];
    await mountInbox();

    expect(backend.count("list_selectable_projects")).toBeGreaterThan(0);
    expect(backend.count("list_projects")).toBe(0);
  });
});

describe("收件箱：F-002 任务理清", () => {
  it("从 Inbox 直接开始：只发一条 start_timer，不自己拆成 clarify_ready + start_timer", async () => {
    // 反向验证：把 `start()` 改成先 `clarifyReady` 再 `startTimer`（或额外补一次
    // `clarify_ready`），`count("start_timer")` 或 `count("clarify_ready")` 立刻红。
    backend = createBackend();
    backend.tasks = [task({ id: "t-9", title: "开始我", status: "Inbox", row_version: 2 })];
    const started: TaskIdentity[] = [];
    await mountInbox((current) => started.push(current));

    const row = await screen.findByTestId("task-t-9");
    backend.commands.length = 0;
    backend.requests.length = 0;
    fireEvent.click(within(row).getByTestId("start-t-9"));

    await waitFor(() => expect(backend.count("start_timer")).toBe(1));
    expect(backend.count("clarify_ready")).toBe(0);
    expect(backend.requests[0]).toEqual({
      command: "start_timer",
      request: {
        expected_data_epoch: EPOCH,
        task_id: "t-9",
        task_expected_version: 2,
        mode: "FOREGROUND",
        timer_kind: "stopwatch",
      },
    });
    // 任务身份（含**提交后**的版本，Inbox → Ready → Doing 两次跃迁）交给外壳：
    // 计时页的「继续」要用它，而快照里没有任务字段。
    expect(started).toEqual([{ id: "t-9", title: "开始我", row_version: 4 }]);
  });

  it("置为 Ready：一条 clarify_ready，请求带任务版本；成功后标签变 Ready", async () => {
    backend = createBackend();
    backend.tasks = [task({ id: "t-1", title: "理清我", status: "Inbox", row_version: 3 })];
    await mountInbox();

    const row = await screen.findByTestId("task-t-1");
    expect(within(row).getByText("Inbox")).not.toBeNull();
    fireEvent.click(within(row).getByTestId("clarify-t-1"));

    await waitFor(() => expect(backend.count("clarify_ready")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "clarify_ready")?.request).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "t-1",
      expected_row_version: 3,
    });
    // 重拉之后状态标签跟着服务端的事实变
    await waitFor(() => expect(within(screen.getByTestId("task-t-1")).getByText("Ready")).not.toBeNull());
  });

  it("非法跃迁不产生请求：Blocked/Done 上没有入口，也一条写命令都不发", async () => {
    // 反向验证：把 `canClarify`/`canStart` 改成 `return true`（前端过滤放行）⇒
    // 两个 queryAllByRole 断言红；若再实现成"点了直接发命令"，最后那条 `toEqual([])` 也红。
    backend = createBackend();
    backend.tasks = [
      task({ id: "t-blocked", title: "被阻塞", status: "Blocked" }),
      task({ id: "t-done", title: "已完成", status: "Done" }),
    ];
    await mountInbox();

    const blocked = await screen.findByTestId("task-t-blocked");
    const done = screen.getByTestId("task-t-done");
    expect(within(blocked).queryAllByRole("button")).toEqual([]);
    expect(within(done).queryAllByRole("button")).toEqual([]);
    // 状态照常展示（"捕获 / 理清 Ready / 开始计时"三处里的第一处）
    expect(within(blocked).getByText("Blocked")).not.toBeNull();

    backend.commands.length = 0;
    fireEvent.click(blocked);
    fireEvent.click(done);
    expect(backend.commands).toEqual([]);
  });

  it("服务拒绝时提示正确：非法跃迁即使漏过滤，Rust 的 message 也会原样上屏", async () => {
    backend = createBackend();
    backend.tasks = [task({ id: "t-1", title: "理清我", status: "Inbox", row_version: 3 })];
    backend.fail.clarify_ready = failure({
      code: "DOMAIN_ERROR",
      message: "正在计时的任务不能理清。",
    });
    await mountInbox();

    const row = await screen.findByTestId("task-t-1");
    fireEvent.click(within(row).getByTestId("clarify-t-1"));

    expect((await screen.findByRole("alert")).textContent).toBe("正在计时的任务不能理清。");
  });

  it("VERSION_CONFLICT 触发冲突刷新：重拉一次列表，文案仍是 Rust 的 message", async () => {
    // 反向验证：把 `commandErrorAction` 里的 VERSION_CONFLICT 那一支删掉 ⇒
    // 失效计数不动，本页不会重拉，`count("list_tasks")` 那条 waitFor 超时红。
    backend = createBackend();
    backend.tasks = [task({ id: "t-1", title: "理清我", status: "Inbox" })];
    backend.fail.clarify_ready = failure({
      code: "VERSION_CONFLICT",
      message: "任务已被别处改动，请刷新后重试。",
    });
    await mountInbox();

    const row = await screen.findByTestId("task-t-1");
    const before = backend.count("list_tasks");
    fireEvent.click(within(row).getByTestId("clarify-t-1"));

    expect((await screen.findByRole("alert")).textContent).toBe("任务已被别处改动，请刷新后重试。");
    await waitFor(() => expect(backend.count("list_tasks")).toBe(before + 1));
  });
});
