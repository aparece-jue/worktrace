/**
 * 任务列表页用例（P7 Task 5）：F-002 的轻量 GTD 三个列表 + F-005 的情境筛选。
 *
 * 断言口径（总纲 §5 第 8 条）：页面只做**展示与转发**——条件的交集、总数、分页窗口的
 * 合法性都在服务端。真正属于前端、这里逐条钉住的是：
 *
 * 1. **Waiting 与 Blocked 不合并**（一次只查一个状态，且两个选项各是各的）；
 * 2. **三个条件进同一条查询**（交集在服务端算，不是前端拼结果）;
 * 3. **计数与分页都用服务端的 `total`**（前端不数行数）；**改条件重置分页**；
 * 4. **旧响应不得覆盖新结果**，两条判据各有用例：
 *    ① 版本（`data_epoch`/`revision`，同一条件下的迟到响应）；
 *    ② 问题身份（筛选切换后，旧筛选的迟到响应）；
 * 5. **多标签任务只出现一次**：不做客户端标签连接、不为每个标签各发一条查询。
 *
 * 假后端（`fakeBackend.ts`）用官方的 `mockIPC` 挡住 IPC 边界，事件走本地替身；
 * 页面读的是**真实的** `domainState` 单例，所以"页面自己不开订阅"这件事也顺带被钉住。
 */

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
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

import { Tasks } from "../Tasks";
import { domainState } from "../../state/domainState";
import { EVENT_DOMAIN_CHANGED, type TagRow, type TaskQueryResult } from "../../types/ipc";
import { AT, EPOCH, createBackend, failure, project, tag, task, type Backend } from "./fakeBackend";
import { installJsdomBridges } from "./jsdomBridges";

let backend: Backend;

/** 挂上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。**页面没有 props**。 */
async function mountTasks(): Promise<RenderResult> {
  const view = render(<Tasks />);
  await act(async () => {
    await domainState.start();
  });
  // 挂起的调用也算"发过了"（mockIPC 在等响应之前就记了账），所以这条等待对
  // "查询还在飞"的用例同样成立。
  await waitFor(() => expect(backend.count("list_tasks")).toBeGreaterThan(0));
  return view;
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

/** 最近一条某命令的入参。 */
function lastRequest(command: string): unknown {
  const found = [...backend.requests].reverse().find((entry) => entry.command === command);
  return found?.request;
}

/** 列表里当前渲染了多少行。 */
function rowCount(): number {
  return document.querySelectorAll('[data-testid="task-list"] .task-item').length;
}

/**
 * 一个下拉的**根**元素（`.ant-select`）。
 *
 * antd 把透传属性铺在 `.ant-select-selector` 那一层（`@rc-component/select` 的
 * `SelectInput` 把 `restProps` 展开在 selector div 上），不是最外层；所以先 `closest`
 * 回到根，再在根上找 selector——两种落点都成立。
 */
function selectRoot(testId: string): Element {
  const found = screen.getByTestId(testId);
  return found.closest(".ant-select") ?? found;
}

/** 打开一个 antd Select 并点中一个选项（下拉渲染在 body 的 portal 里）。 */
async function pick(testId: string, label: string): Promise<void> {
  const root = selectRoot(testId);
  const selector = root.querySelector(".ant-select-selector") ?? root;
  fireEvent.mouseDown(selector);
  fireEvent.click(await screen.findByTitle(label));
  await act(async () => undefined);
}

beforeAll(installJsdomBridges);

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
});

describe("任务页：三个列表与筛选", () => {
  it("三个列表各查各的状态：Waiting 与 Blocked 不合并成一条查询", async () => {
    // 反向验证：把 Waiting/Blocked 合成一个选项（`statuses: ["Waiting","Blocked"]`），
    // 或把状态写死成 "Ready"，下面三次请求断言里的任意一条立刻红。
    backend = createBackend();
    backend.tasks = [task({ id: "t-wait", title: "等别人", status: "Waiting" })];
    await mountTasks();

    // 默认就是「下一步行动」= Ready
    expect(lastRequest("list_tasks")).toEqual({
      statuses: ["Ready"],
      project: "any",
      context_tag_id: null,
      limit: 20,
      offset: 0,
      expected_data_epoch: EPOCH,
    });

    fireEvent.click(screen.getByText("等待中"));
    await waitFor(() => expect(backend.count("list_tasks")).toBe(2));
    expect(lastRequest("list_tasks")).toMatchObject({ statuses: ["Waiting"] });

    fireEvent.click(screen.getByText("阻塞"));
    await waitFor(() => expect(backend.count("list_tasks")).toBe(3));
    expect(lastRequest("list_tasks")).toMatchObject({ statuses: ["Blocked"] });
  });

  it("筛选交集：状态 + 项目 + 情境进**同一条** list_tasks（交集在服务端算）", async () => {
    // 反向验证：把项目/情境做成前端过滤（查询仍发 `project: "any"`）⇒ 那条
    // 逐字段 `toEqual` 立刻红；拆成两条查询再合并结果 ⇒ 计数断言也红。
    backend = createBackend();
    backend.projects = [project({ id: "p-1", name: "项目甲" })];
    backend.tags = [tag({ id: "ctx-1", kind: "Context", name: "办公室" })];
    backend.tasks = [task({ id: "t-1", title: "交集里的那条", status: "Ready", project_id: "p-1" })];
    await mountTasks();

    await pick("project-select", "项目甲");
    await waitFor(() => expect(lastRequest("list_tasks")).toMatchObject({ project: { id: "p-1" } }));
    await pick("context-select", "办公室");
    await waitFor(() =>
      expect(lastRequest("list_tasks")).toMatchObject({ context_tag_id: "ctx-1" }),
    );

    expect(lastRequest("list_tasks")).toEqual({
      statuses: ["Ready"],
      project: { id: "p-1" },
      context_tag_id: "ctx-1",
      limit: 20,
      offset: 0,
      expected_data_epoch: EPOCH,
    });
    // 显示的就是这条查询的响应
    expect(screen.getAllByTestId("task-t-1")).toHaveLength(1);
  });

  it("无情境、无项目：下拉里没有可筛的东西，查询照发（不编选项）", async () => {
    backend = createBackend();
    await mountTasks();

    expect(selectRoot("context-select").className).toContain("ant-select-disabled");

    // 项目下拉只有两个哨兵选项（不限 / 无项目），没有任何真实项目
    await pick("project-select", "（无项目）");
    await waitFor(() =>
      expect(lastRequest("list_tasks")).toMatchObject({ project: "none" }),
    );
  });
});

describe("任务页：分页与计数", () => {
  it("计数与分页都用服务端的 total；改变条件把分页重置回第一页", async () => {
    // 反向验证①（本用例）：把分页器或「共 N 条」改成读 `tasks.length` ⇒ 下面
    // `共 45 条` 与 20 行两条断言红（服务端只回了这一页的 20 条）。
    // 反向验证②（本用例）：条件变化时不 `setPage(1)` ⇒ 切成「等待中」之后
    // 请求的 `offset` 会是 40 而不是 0，第三条断言红。
    backend = createBackend();
    backend.tasks = Array.from({ length: 45 }, (_, index) =>
      task({
        id: `t-${index + 1}`,
        title: `任务${index + 1}`,
        status: "Ready",
        created_at: AT + index,
      }),
    );
    await mountTasks();

    expect(screen.getByTestId("task-total").textContent).toBe("共 45 条");
    expect(rowCount()).toBe(20);
    expect(lastRequest("list_tasks")).toMatchObject({ offset: 0, limit: 20 });

    // 翻到第 3 页：窗口是 40..60，服务端只回剩下的 5 条
    fireEvent.click(screen.getByTitle("3"));
    await waitFor(() => expect(backend.count("list_tasks")).toBe(2));
    expect(lastRequest("list_tasks")).toMatchObject({ offset: 40 });
    expect(rowCount()).toBe(5);

    // 改条件 ⇒ 从第一页重新开始
    fireEvent.click(screen.getByText("等待中"));
    await waitFor(() => expect(backend.count("list_tasks")).toBe(3));
    expect(lastRequest("list_tasks")).toMatchObject({ statuses: ["Waiting"], offset: 0 });
    expect(document.querySelector(".ant-pagination-item-active")?.getAttribute("title")).toBe("1");
  });

  it("空列表：明确的空态（不是错误、也不是永远加载）", async () => {
    backend = createBackend();
    await mountTasks();

    expect(await screen.findByText(/这个条件下没有任务/)).not.toBeNull();
    expect(screen.getByTestId("task-total").textContent).toBe("共 0 条");
    expect(screen.queryByTestId("task-list")).toBeNull();
  });

  it("加载中：查询还在飞时给出加载态，不是空列表", async () => {
    backend = createBackend();
    backend.tasks = [task({ id: "t-1", title: "慢查询", status: "Ready" })];
    const first = backend.holdNext<TaskQueryResult>("list_tasks");
    await mountTasks();

    expect(screen.getByTestId("tasks-loading")).not.toBeNull();
    expect(screen.queryByText(/这个条件下没有任务/)).toBeNull();

    await act(async () => {
      first.resolve({
        tasks: backend.tasks,
        total: 1,
        data_epoch: EPOCH,
        revision: backend.revision,
      });
    });
    expect(await screen.findByText("慢查询")).not.toBeNull();
    expect(screen.queryByTestId("tasks-loading")).toBeNull();
  });

  it("查询失败只提示、不自动重试：上屏的就是 Rust 的 message", async () => {
    // 反向验证：把 `load()` 的 catch 改成调 `domainState.refresh()` ⇒ 失效会自触发
    // 重拉，`count("list_tasks")` 与 `count("get_revision")` 两条断言都红。
    backend = createBackend();
    backend.fail.list_tasks = failure({
      code: "DOMAIN_ERROR",
      message: "「每页条数」只能是 1 到 100，收到的是 200。",
    });
    await mountTasks();

    expect((await screen.findByRole("alert")).textContent).toBe(
      "「每页条数」只能是 1 到 100，收到的是 200。",
    );
    expect(backend.count("list_tasks")).toBe(1);
    expect(backend.count("get_revision")).toBe(1);
  });
});

describe("任务页：旧响应与重复行", () => {
  it("I1-A：页面查询响应到达**不推全局水位**——同 revision 的通知仍必须让它重拉", async () => {
    // 反向验证（评审 I1 的探针 A）：把 `load()` 改回 `domainState.markApplied(result)`
    // ⇒ 全局水位被这条**过滤 + 分页**的响应推到第 6 版 ⇒ 下面那条同 revision(6) 的
    // `domain.changed` 被判"快照已包含"而 drop ⇒ 三句断言全红（失效计数不动、不再重拉、
    // 视图水位被推走）。
    backend = createBackend();
    backend.tasks = [task({ id: "t-1", title: "第一版", status: "Ready" })];
    const first = backend.holdNext<TaskQueryResult>("list_tasks");
    await mountTasks();

    // 页面读到第 6 版（写命令刚提交、它那条通知还在路上）
    backend.revision = 6;
    await act(async () => {
      first.resolve({
        tasks: [task({ id: "t-1", title: "第一版", status: "Ready" })],
        total: 1,
        data_epoch: EPOCH,
        revision: 6,
      });
    });
    expect(screen.getByText("第一版")).not.toBeNull();
    // 镜像那把水位是**权威快照**的水位：仍是握手时的第 5 版，页面查询推不动它
    expect(domainState.getView().revision).toBe(5);

    const before = backend.count("list_tasks");
    const invalidated = domainState.getView().invalidated;
    // 那条写命令对应的通知现在才到，与页面刚上屏的响应**同版本**
    await changed(6);

    expect(domainState.getView().invalidated).toBe(invalidated + 1);
    await waitFor(() => expect(backend.count("list_tasks")).toBe(before + 1));
  });

  it("I1-C：辅助查询（list_tags）上屏也不推全局水位——同 revision 的通知仍必须生效", async () => {
    // 反向验证（评审 I1 的探针 C）：把辅助查询改回全局 `isStaleResponse` + `markApplied`
    // ⇒ 第 6 版的 `list_tags` 响应把全局水位推到 6 ⇒ 紧随的第 6 版通知被 drop ⇒
    // `list_tags` 不再重发、"办公室"永远出不来（后两句红）。
    backend = createBackend();
    const firstTags = backend.holdNext<{
      items: TagRow[];
      data_epoch: string;
      revision: number;
    }>("list_tags");
    await mountTasks();

    // 一条写命令提交（第 6 版），辅助查询这次拿到了情境标签——但它迟到了，先扣住
    backend.revision = 6;
    backend.tags = [tag({ id: "ctx-1", name: "办公室" })];
    await act(async () => {
      firstTags.resolve({ items: backend.tags, data_epoch: EPOCH, revision: 6 });
    });
    await waitFor(() =>
      expect(selectRoot("context-select").className).not.toContain("ant-select-disabled"),
    );

    const before = backend.count("list_tags");
    // 与上面那条响应**同版本**的通知：它必须仍然生效（重发一次辅助查询）
    await changed(6);

    await waitFor(() => expect(backend.count("list_tags")).toBe(before + 1));
    expect(selectRoot("context-select").className).not.toContain("ant-select-disabled");
  });

  it("M5③：辅助查询的旧响应被**本视图**水位丢弃，不覆盖已经拿到的新选项", async () => {
    // 反向验证：去掉 `loadOptions` 里那两句 `optionsWatermark.isStale` ⇒ 迟到的第 5 版
    // 响应会把选项覆盖回只有"办公室"一份 ⇒ 最后那次 `pick("电脑")` 找不到选项，红。
    backend = createBackend();
    backend.tags = [tag({ id: "ctx-1", name: "办公室" })];
    const firstOptions = backend.holdNext<{
      items: TagRow[];
      data_epoch: string;
      revision: number;
    }>("list_tags");
    await mountTasks();

    // 第 6 版：多了一个情境标签"电脑"；这条辅助查询被扣住
    backend.revision = 6;
    backend.tags = [
      tag({ id: "ctx-1", name: "办公室" }),
      tag({ id: "ctx-2", name: "电脑" }),
    ];
    await changed(6);
    // 第 7 版：再来一个标签；这一次的辅助查询正常返回并上屏
    backend.revision = 7;
    backend.tags = [...backend.tags, tag({ id: "ctx-3", name: "电话" })];
    await changed(7);
    await waitFor(() => expect(backend.count("list_tags")).toBe(3));

    // 扣住的第 5 版（只有"办公室"）现在才回来：本视图水位已经是 7 ⇒ 丢弃
    await act(async () => {
      firstOptions.resolve({ items: [tag({ id: "ctx-1", name: "办公室" })], data_epoch: EPOCH, revision: 5 });
    });

    // 第 7 版那份还在：能选中只存在于新响应里的"电脑"
    await pick("context-select", "电脑");
    await waitFor(() =>
      expect(lastRequest("list_tasks")).toMatchObject({ context_tag_id: "ctx-2" }),
    );
  });

  it("同一筛选下旧响应晚到：按 data_epoch/revision 丢弃，不覆盖新结果", async () => {
    // 反向验证①：把 `load()` 里那句 `isStaleResponse` 去掉 ⇒ 旧响应（revision 5）
    // 会被上屏，下面「旧结果不出现」的断言立刻红。
    //
    // 这条用例的两个响应**问题身份完全相同**（同一筛选、同一页），所以只有版本判据
    // 能区分它们——这正是「比 data_epoch/revision，不比到达顺序」。
    backend = createBackend();
    backend.tasks = [task({ id: "t-old", title: "旧结果", status: "Ready" })];
    const first = backend.holdNext<TaskQueryResult>("list_tasks");
    await mountTasks();

    // 一条通知 ⇒ 缓存失效 ⇒ 同一条件重拉；这一次服务端已经到了第 6 版
    backend.revision = 6;
    backend.tasks = [task({ id: "t-new", title: "新结果", status: "Ready" })];
    await changed(6);
    expect(await screen.findByText("新结果")).not.toBeNull();

    // 旧响应现在才回来
    await act(async () => {
      first.resolve({
        tasks: [task({ id: "t-old", title: "旧结果", status: "Ready" })],
        total: 1,
        data_epoch: EPOCH,
        revision: 5,
      });
    });

    expect(screen.queryByText("旧结果")).toBeNull();
    expect(screen.getByText("新结果")).not.toBeNull();
  });

  it("切换筛选后，旧筛选的迟到响应不覆盖新结果（问题身份判据）", async () => {
    // 反向验证：把 `load()` 里那句「回答的是不是现在这个问题」去掉 ⇒ 迟到的 Ready
    // 响应会把「等待中」的结果顶掉，下面第二句（等别人还在）红。
    //
    // 两个响应的 revision **相同**（同一次读），所以版本判据区分不了它们：这条用例
    // 钉的是另一半——响应必须回答当前这个问题。
    backend = createBackend();
    backend.tasks = [task({ id: "t-ready", title: "旧的下一步", status: "Ready" })];
    const first = backend.holdNext<TaskQueryResult>("list_tasks");
    await mountTasks();

    backend.tasks = [task({ id: "t-wait", title: "等别人", status: "Waiting" })];
    fireEvent.click(screen.getByText("等待中"));
    expect(await screen.findByText("等别人")).not.toBeNull();

    await act(async () => {
      first.resolve({
        tasks: [task({ id: "t-ready", title: "旧的下一步", status: "Ready" })],
        total: 1,
        data_epoch: EPOCH,
        revision: backend.revision,
      });
    });

    expect(screen.queryByText("旧的下一步")).toBeNull();
    expect(screen.getByText("等别人")).not.toBeNull();
  });

  it("多标签任务只出现一次：不做客户端标签连接、也不按标签各查一次", async () => {
    // 反向验证：把筛选实现成「对每个 Context 标签各发一条 list_tasks 再拼接结果」
    // ⇒ `count("list_tasks")` 与行数两条断言红；改成客户端用 `tags_of_task` 连表
    // ⇒ 那条 `count("tags_of_task") === 0` 红（本页根本不调那条命令）。
    backend = createBackend();
    backend.tags = [tag({ id: "ctx-1", name: "办公室" }), tag({ id: "ctx-2", name: "电脑" })];
    backend.tasks = [task({ id: "t-1", title: "挂了两个情境的任务", status: "Ready" })];
    await mountTasks();

    expect(screen.getAllByTestId("task-t-1")).toHaveLength(1);
    expect(backend.count("tags_of_task")).toBe(0);

    await pick("context-select", "办公室");
    await waitFor(() => expect(backend.count("list_tasks")).toBe(2));
    expect(lastRequest("list_tasks")).toMatchObject({ context_tag_id: "ctx-1" });
    expect(screen.getAllByTestId("task-t-1")).toHaveLength(1);
  });
});
