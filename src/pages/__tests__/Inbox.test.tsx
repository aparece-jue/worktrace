/**
 * 收件箱页用例（P7 Task 3）：F-001 快速捕获 + F-002 任务理清。
 *
 * 断言口径（总纲 §5 第 8 条）：这里只断言**展示与转发**——状态合法性、事务边界都在 Rust。
 * 两条真正属于前端的判据也在这里钉住：① 空白输入不发命令；② 什么状态下**不出现**入口
 * （`start` 是一条命令，前端不自己拆两步）。P8 Task 2d 补的 F-003 跃迁入口照同一条口径：
 * 断的是"入口按 `allowed_targets` 开合"「请求逐字正确」「成功后重拉」「失败不乐观改列表」
 * 与"回执说出哪条会话被动了"，**不断**服务端会不会拒绝某个跃迁。
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
import { domainState } from "../../state/domainState";
import { EVENT_DOMAIN_CHANGED, type TaskQueryResult } from "../../types/ipc";
import {
  AT,
  EPOCH,
  SESSION,
  createBackend,
  failure,
  project,
  runningSnapshot,
  task,
  type Backend,
} from "./fakeBackend";

let backend: Backend;

/** 挂上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。**页面没有 props**。 */
async function mountInbox(): Promise<void> {
  render(<Inbox />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("list_tasks")).toBeGreaterThan(0));
}

/** 一条 `domain.changed`（同 epoch 的下一版 ⇒ 闸门判 `apply`，页面只当作失效）。 */
async function changed(revision: number): Promise<void> {
  await act(async () => {
    events.emit({
      data_epoch: EPOCH,
      event: EVENT_DOMAIN_CHANGED,
      revision,
      at: AT,
      payload: {},
    });
  });
}

/**
 * 一行上的**按钮文案**，按出现顺序。
 *
 * 去掉 antd 自动插在**两个汉字之间**的那个空格（`取消` 的 DOM 文本是 `取 消`，
 * `Button` 的 `autoInsertSpace`；P7 的「开始」也一样）。只去汉字之间的空格：`置为 Ready`
 * 里那个空格是文案本身的一部分，不能动——断的是"这一行出现了哪几个入口"，
 * 不是 antd 的排版细节。
 */
function buttonLabels(row: HTMLElement): string[] {
  return within(row)
    .getAllByRole("button")
    .map((button) => (button.textContent ?? "").replace(/(?<=[\u4e00-\u9fff])\s+(?=[\u4e00-\u9fff])/g, ""));
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
    await mountInbox();

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
    // 提交后的任务版本**不必在这里记**：会话一开起来，快照就带上任务的 id / 版本 / 标题
    // （`build()` 每次采样重读任务行），计时页与状态栏都从那里取。
  });

  it("正在计时的那条任务：「开始」被禁掉（再点必败）；别的任务不受影响", async () => {
    // M3：`require_no_running_foreground` 与唯一索引 `uq_running_foreground` 都只认
    // `state='running'` 的前台会话 ⇒ 那条任务上再点「开始」必失败，所以不给入口。
    backend = createBackend();
    backend.tasks = [
      task({ id: "t-9", title: "正在计时", status: "Doing", row_version: 4 }),
      task({ id: "t-8", title: "另一条", status: "Ready", row_version: 0 }),
    ];
    backend.snapshot = runningSnapshot({ task_id: "t-9", state: "running" });
    await mountInbox();

    const timed = await screen.findByTestId("task-t-9");
    expect((within(timed).getByTestId("start-t-9") as HTMLButtonElement).disabled).toBe(true);
    const other = screen.getByTestId("task-t-8");
    expect((within(other).getByTestId("start-t-8") as HTMLButtonElement).disabled).toBe(false);
  });

  it("暂停的会话不挡「开始」：Rust 只对 running 占用前台槽位，前端不多拦", async () => {
    // 与上一条合起来就是 M3 的边界：判据只看 `state === "running"`——多拦会把一个
    // Rust 允许的动作变成灰色的。
    backend = createBackend();
    backend.tasks = [task({ id: "t-9", title: "已暂停", status: "Doing", row_version: 4 })];
    backend.snapshot = runningSnapshot({ task_id: "t-9", state: "paused", session_version: 2 });
    await mountInbox();

    const row = await screen.findByTestId("task-t-9");
    expect((within(row).getByTestId("start-t-9") as HTMLButtonElement).disabled).toBe(false);
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

  it("入口按跃迁表开合：Blocked 只剩「取消」、Done 只剩「重开」，点行本身不发命令", async () => {
    // 反向验证：把 `canCancel` 改成 `return true`（Done 行多出「取消」）或把 `canFinish`
    // 改成 `return true`（Blocked 行多出「完成」）⇒ 两条按钮清单断言红；若实现成
    // "点了直接发命令"，最后那条 `toEqual([])` 也红。
    backend = createBackend();
    backend.tasks = [
      task({ id: "t-blocked", title: "被阻塞", status: "Blocked" }),
      task({ id: "t-done", title: "已完成", status: "Done" }),
    ];
    await mountInbox();

    const blocked = await screen.findByTestId("task-t-blocked");
    const done = screen.getByTestId("task-t-done");
    // `allowed_targets`：`Blocked | Waiting => [Ready, Cancelled]`、`Done | Cancelled => [Ready]`。
    expect(buttonLabels(blocked)).toEqual(["取消"]);
    expect(buttonLabels(done)).toEqual(["重开"]);
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

describe("收件箱：F-003 状态跃迁（P8 Task 2d）", () => {
  it("完成：请求逐字正确、回执说出被结束的会话、当场重拉列表（不投递任何事件）", async () => {
    // 反向验证：`expected_row_version` 换成别的值、`cause` 换成 "reopen" ⇒ 逐字断言红；
    // 删掉成功后的 `afterWrite()`（且本用例不投递 `domain.changed`）⇒ 状态标签停在 Doing，
    // 那条 `getByText("Done")` 超时红；把 `transitionNotice` 改成不含会话 id 的文案 ⇒
    // 回执断言红——"这次动作碰了哪条会话"就没人说了。
    backend = createBackend();
    backend.tasks = [task({ id: "t-9", title: "写周报", status: "Doing", row_version: 7 })];
    backend.snapshot = runningSnapshot({ task_id: "t-9" });
    await mountInbox();

    const row = await screen.findByTestId("task-t-9");
    const before = backend.count("list_tasks");
    fireEvent.click(within(row).getByTestId("finish-t-9"));

    await waitFor(() => expect(backend.count("transition_task")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "transition_task")?.request).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "t-9",
      expected_row_version: 7,
      target: "Done",
      cause: "user",
    });
    // 回执：这条任务正在计时 ⇒ 完成在同一个事务里结束了它那条会话（名单来自**响应**）
    const notice = await screen.findByTestId("transition-notice");
    expect(notice.textContent).toContain("置为 Done");
    expect(notice.textContent).toContain(`结束会话 ${SESSION}`);
    // 重拉：这里一条事件都没投递，列表上必须已经是服务端的新事实
    await waitFor(() => expect(backend.count("list_tasks")).toBe(before + 1));
    await waitFor(() =>
      expect(within(screen.getByTestId("task-t-9")).getByText("Done")).not.toBeNull(),
    );
  });

  it("阻塞/等待：Ready 行两个入口都在，点了只发一条命令，回执说会话被暂停", async () => {
    // 反向验证：把「等待中」按钮的 `target` 写成 "Blocked" ⇒ 逐字断言红；回执里没有
    // `paused_sessions` 也会红（暂停与结束是两个不同的联动事实，不能混着说）。
    backend = createBackend();
    backend.tasks = [task({ id: "t-5", title: "被卡住", status: "Ready", row_version: 2 })];
    backend.snapshot = runningSnapshot({ task_id: "t-5" });
    await mountInbox();

    const row = await screen.findByTestId("task-t-5");
    expect(within(row).getByTestId("wait-t-5")).not.toBeNull();
    fireEvent.click(within(row).getByTestId("block-t-5"));

    await waitFor(() => expect(backend.count("transition_task")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "transition_task")?.request).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "t-5",
      expected_row_version: 2,
      target: "Blocked",
      cause: "user",
    });
    expect((await screen.findByTestId("transition-notice")).textContent).toContain(
      `暂停会话 ${SESSION}`,
    );
  });

  it("重开：终结态回 Ready 带的是 cause \"reopen\"（不是 \"user\"），且不谎报会话联动", async () => {
    // 反向验证：把 reopen 那个按钮的 `cause` 改成 "user" ⇒ 逐字断言红——同一个请求在服务层
    // 会被 `ReopenMustBeExplicit` 拒绝（`domain/task.rs`），界面必须给唯一合法的那个原因。
    backend = createBackend();
    backend.tasks = [task({ id: "t-3", title: "已完成的任务", status: "Done", row_version: 4 })];
    await mountInbox();

    const row = await screen.findByTestId("task-t-3");
    fireEvent.click(within(row).getByTestId("reopen-t-3"));

    await waitFor(() => expect(backend.count("transition_task")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "transition_task")?.request).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "t-3",
      expected_row_version: 4,
      target: "Ready",
      cause: "reopen",
    });
    expect((await screen.findByTestId("transition-notice")).textContent).toContain(
      "没有会话被结束或暂停",
    );
  });

  it("五个入口逐状态照 `allowed_targets` 开合（同一行换个 status，入口就不同）", async () => {
    // 反向验证：`canFinish` 改成 `return true` ⇒ Inbox 行多出「完成」而红；`canCancel` 改成
    // `status !== "Cancelled"` ⇒ Done/Cancelled 行多出「取消」而红；`canBlock` 去掉
    // `Scheduled` 之外的判断（改成 `return true`）⇒ Waiting 行多出两个而红。
    backend = createBackend();
    backend.tasks = [
      task({ id: "t-inbox", title: "刚捕获", status: "Inbox" }),
      task({ id: "t-doing", title: "在做", status: "Doing" }),
      task({ id: "t-waiting", title: "等人", status: "Waiting" }),
      task({ id: "t-cancelled", title: "已取消", status: "Cancelled" }),
    ];
    await mountInbox();
    await screen.findByTestId("task-t-inbox");

    const labels = (id: string): string[] => buttonLabels(screen.getByTestId(`task-${id}`));

    // Inbox：`allowed_targets` = [Clarifying, Ready, Cancelled] ⇒ 跃迁入口只有「取消」
    // （另外两个是 P7 就有的「置为 Ready」与「开始」）。
    expect(labels("t-inbox")).toEqual(["置为 Ready", "开始", "取消"]);
    // Doing：完成 / 取消 / 阻塞 / 等待中（`Ready` 只是"已经在做"的另一种说法，不给入口）
    expect(labels("t-doing")).toEqual(["开始", "完成", "取消", "阻塞", "等待中"]);
    // Waiting：出口只有 Ready 与 Cancelled ⇒ 跃迁入口只剩「取消」
    expect(labels("t-waiting")).toEqual(["取消"]);
    // Cancelled：只能 reopen
    expect(labels("t-cancelled")).toEqual(["重开"]);
  });

  it("失败：上屏的就是 Rust 的 message，且**不**乐观改列表", async () => {
    // 反向验证：catch 里换成自己拼的文案 ⇒ 那句断言红；在 `await` 之前先本地把状态改成
    // Done（乐观更新）⇒ 状态标签断言红——屏幕上必须仍是服务端给的最后一份事实。
    backend = createBackend();
    backend.tasks = [task({ id: "t-9", title: "写周报", status: "Doing", row_version: 7 })];
    backend.fail.transition_task = failure({
      code: "DOMAIN_ERROR",
      message: "这条任务已经被完成了。",
    });
    await mountInbox();

    const row = await screen.findByTestId("task-t-9");
    const before = backend.count("list_tasks");
    fireEvent.click(within(row).getByTestId("finish-t-9"));

    expect((await screen.findByRole("alert")).textContent).toBe("这条任务已经被完成了。");
    // 失败不重发、也不重拉（重拉是**成功**之后的动作；冲突刷新走 `reportCommandError`
    // 的 VERSION_CONFLICT 那一支，那条在 F-002 的用例里已钉）。
    expect(backend.count("transition_task")).toBe(1);
    expect(backend.count("list_tasks")).toBe(before);
    expect(within(screen.getByTestId("task-t-9")).getByText("Doing")).not.toBeNull();
  });

  it("上一条回执不留到下一次动作：跃迁后捕获成功，回执从屏幕上消失", async () => {
    // 反向验证（fix round 1，评审 Minor-1）：删掉 `clearNotices()` ⇒ 捕获之后上一条回执仍
    // 挂在屏幕上 ⇒ 最后那条 `queryByTestId(...)` 为 null 的断言红。
    backend = createBackend();
    backend.tasks = [task({ id: "t-9", title: "写周报", status: "Doing", row_version: 7 })];
    backend.snapshot = runningSnapshot({ task_id: "t-9" });
    await mountInbox();

    const row = await screen.findByTestId("task-t-9");
    fireEvent.click(within(row).getByTestId("finish-t-9"));
    expect((await screen.findByTestId("transition-notice")).textContent).toContain("置为 Done");

    capture("另一条");
    await waitFor(() => expect(backend.count("create_task")).toBe(1));
    await waitFor(() => expect(screen.queryByTestId("transition-notice")).toBeNull());
  });

  it("跃迁之后那条命令**失败**：屏幕上只有失败提示，不叠着上一条回执", async () => {
    // 反向验证：把清回执只放在 `afterWrite()` 里（评审给的那一行）⇒ **失败**路径根本不会
    // 调用它，这里会数到两条 alert（成功回执 + 失败提示）而红——所以清在命令开头
    // （理由见 `clearNotices` 的注释）。
    backend = createBackend();
    backend.tasks = [task({ id: "t-9", title: "写周报", status: "Doing", row_version: 7 })];
    backend.snapshot = runningSnapshot({ task_id: "t-9" });
    await mountInbox();

    const row = await screen.findByTestId("task-t-9");
    fireEvent.click(within(row).getByTestId("finish-t-9"));
    expect((await screen.findByTestId("transition-notice")).textContent).toContain("结束会话");

    backend.fail.create_task = failure({ code: "DOMAIN_ERROR", message: "标题太长。" });
    capture("另一条");

    await waitFor(() => {
      const alerts = screen.queryAllByRole("alert");
      expect(alerts.length).toBe(1);
      expect(alerts[0].textContent).toBe("标题太长。");
    });
    expect(screen.queryByTestId("transition-notice")).toBeNull();
  });
});

describe("收件箱：旧响应与水位（fix round 2）", () => {
  it("旧响应（同 epoch、revision 更旧）不得覆盖已经上屏的收件箱列表", async () => {
    // 反向验证：把 `load()` 改回全局 `domainState.isStaleResponse` ⇒ 全局水位没人推，
    // 那条第 5 版的迟到响应**判不出旧**、直接覆盖屏幕上的第 6 版 ⇒ 后两句红。
    //
    // 两个响应的"问题"完全相同（同一组条件、同一页），所以只有版本判据能区分它们——
    // 这正是本页要用**本视图**水位的原因（收件箱是过滤视图，不能拿全局快照的水位比）。
    backend = createBackend();
    backend.tasks = [task({ id: "t-old", title: "旧列表", status: "Inbox" })];
    const first = backend.holdNext<TaskQueryResult>("list_tasks");
    await mountInbox();

    // 一条通知 ⇒ 缓存失效 ⇒ 同一条件重拉；这一次服务端已经到了第 6 版
    backend.revision = 6;
    backend.tasks = [task({ id: "t-new", title: "新列表", status: "Inbox" })];
    await changed(6);
    expect(await screen.findByText("新列表")).not.toBeNull();

    // 旧响应现在才回来（第 5 版、旧内容）
    await act(async () => {
      first.resolve({
        tasks: [task({ id: "t-old", title: "旧列表", status: "Inbox" })],
        total: 1,
        data_epoch: EPOCH,
        revision: 5,
      });
    });

    expect(screen.queryByText("旧列表")).toBeNull();
    expect(screen.getByText("新列表")).not.toBeNull();
  });

  it("收件箱的响应**不推全局水位**——同 revision 的通知仍必须让它重拉", async () => {
    // 反向验证（与 Task 5 的 I1-A 同型）：把 `load()` 改成"全局判旧 + `markApplied`"
    // ⇒ 全局水位被这条**过滤视图**推到第 6 版 ⇒ 下面那条同 revision(6) 的通知被判
    // "快照已包含"而 drop ⇒ 三句断言全红（水位被推走、失效计数不动、不再重拉）。
    backend = createBackend();
    backend.tasks = [task({ id: "t-1", title: "第一版", status: "Inbox" })];
    const first = backend.holdNext<TaskQueryResult>("list_tasks");
    await mountInbox();

    backend.revision = 6;
    await act(async () => {
      first.resolve({
        tasks: [task({ id: "t-1", title: "第一版", status: "Inbox" })],
        total: 1,
        data_epoch: EPOCH,
        revision: 6,
      });
    });
    expect(await screen.findByText("第一版")).not.toBeNull();
    // 镜像那把水位是**权威快照**的水位：仍是握手时的第 5 版，收件箱的读推不动它
    expect(domainState.getView().revision).toBe(5);

    const before = backend.count("list_tasks");
    const invalidated = domainState.getView().invalidated;
    await changed(6);

    expect(domainState.getView().invalidated).toBe(invalidated + 1);
    await waitFor(() => expect(backend.count("list_tasks")).toBe(before + 1));
  });
});
