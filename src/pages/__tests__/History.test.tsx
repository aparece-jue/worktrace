/**
 * 历史页用例（P8 Task 2c）：F-017 修正与补录。
 *
 * 页面只做**展示与转发**——哪些会话进常规历史、哪些区间能改、重叠怎么判、软删除写什么
 * 全在 Rust（`services/history.rs`），前端一个数字都不重算。这里逐条钉住的是：
 *
 * 1. **数据源是 `history_view`**（不是恢复列表）：读查询带半开窗口 + 分页窗口，
 *    不指名时**不带** `session_id`；
 * 2. **翻页只能按"取满一页"判**（响应里没有 `total`：形状由计划钉死）；
 * 3. **`correct` 只对 `finished` 开放**，而且只对已确认、未作废的区间给入口；
 * 4. **起止与 `row_version` 都是用户/详情给的**：`expected_row_version` 取自**详情**
 *    （夹具让列表与详情给出**不同**的版本，这条断言才有鉴别力）；
 * 5. **删除误记是软删除**：说明文案可见、二次确认之前不发命令、请求里不带起止；
 * 6. **`backfill` 是独立命令**：不发 `start_timer`；
 * 7. **`VERSION_CONFLICT`**：上屏 Rust 的可操作提示、**不**自动重试、按既有口径冲突刷新；
 * 8. **本视图水位**：迟到的旧响应不得覆盖已经上屏的那一页。
 *
 * 反向验证写在每条用例里（"改坏什么会让它红"）。
 */

import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
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

import { History } from "../History";
import { domainState } from "../../state/domainState";
import { EVENT_DOMAIN_CHANGED, type HistoryView } from "../../types/ipc";
import {
  AT,
  EPOCH,
  createBackend,
  failure,
  historyDetail,
  historyView,
  interval,
  session,
  task,
  timeEdit,
  type Backend,
} from "./fakeBackend";
import { installJsdomBridges } from "./jsdomBridges";

let backend: Backend;

/** 夹具里的三条会话 / 三条区间：一条可修正、一条已作废、一条待确认。 */
const FINISHED = "session-done";
const OPEN = "session-open";
const DISCARDED = "session-discarded";
const I_OK = "interval-ok";
const I_VOIDED = "interval-voided";
const I_PENDING = "interval-pending";
const PAGE_SIZE = 50;

/** 装上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。**页面没有 props**。 */
async function mountHistory(): Promise<RenderResult> {
  const view = render(<History />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("history_view")).toBeGreaterThan(0));
  return view;
}

/** 最近一条某命令的入参。 */
function lastRequest(command: string): Record<string, unknown> {
  const found = [...backend.requests].reverse().find((entry) => entry.command === command);
  return (found?.request ?? {}) as Record<string, unknown>;
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

/** 用例侧**独立**把本地时间文本算成毫秒（不走生产代码的解析器）。 */
function localMs(text: string): number {
  const [date, time] = text.split(" ");
  const [year, month, day] = date.split("-").map(Number);
  const [hour, minute] = time.split(":").map(Number);
  return new Date(year, month - 1, day, hour, minute, 0, 0).getTime();
}

/** 一个 antd Select 的**根**元素（`.ant-select`）；透传属性铺在 selector 那一层。 */
function selectRoot(testId: string): Element {
  const found = screen.getByTestId(testId);
  return found.closest(".ant-select") ?? found;
}

/** 打开一个 antd Select（下拉渲染在 body 的 portal 里）。 */
function openSelect(testId: string): void {
  const root = selectRoot(testId);
  fireEvent.mouseDown(root.querySelector(".ant-select-selector") ?? root);
}

/** 打开一个 antd Select 并点中一个选项。 */
async function pick(testId: string, label: string): Promise<void> {
  openSelect(testId);
  fireEvent.click(await screen.findByTitle(label));
  await act(async () => undefined);
}

/**
 * 一条已结束会话的详情。
 *
 * ⚠️ 列表那一行给的是 `row_version: 4`，详情给的是 `9`：**刻意不一致**，这样
 * "请求里带的是详情给的版本"才有鉴别力（两边相等时，接错来源也照样绿）。
 */
function finishedDetail(): HistoryView {
  return historyView({
    sessions: [
      session({ id: FINISHED, task_id: "task-1", state: "finished", row_version: 4 }),
    ],
    selected: historyDetail({
      session: session({ id: FINISHED, task_id: "task-1", state: "finished", row_version: 9 }),
      intervals: [
        interval({ id: I_OK, session_id: FINISHED }),
        interval({
          id: I_VOIDED,
          session_id: FINISHED,
          voided_at: AT + 3_600_000,
          duration_ms: null,
        }),
        interval({
          id: I_PENDING,
          session_id: FINISHED,
          needs_review: true,
          duration_ms: null,
        }),
      ],
      edits: [timeEdit({ id: "edit-1", session_id: FINISHED })],
    }),
  });
}

beforeAll(installJsdomBridges);

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
});

describe("历史页：一页常规历史", () => {
  it("读查询：半开窗口 + 分页窗口，不指名时不带 session_id；列表与口径说明都上屏", async () => {
    // 反向验证：把 `session_id: null` 也塞进请求（而不是省略）⇒ 那句 `in` 断言红；
    // 把 `from` 换成"最近 N 天"、或把 `limit` 写错 ⇒ 逐字段断言红。
    backend = createBackend();
    backend.history = historyView({
      sessions: [
        session({ id: FINISHED, task_id: "task-1", state: "finished" }),
        session({ id: "session-discarded", task_id: "task-2", state: "discarded" }),
      ],
    });
    await mountHistory();

    const request = lastRequest("history_view");
    expect(request.expected_data_epoch).toBe(EPOCH);
    // 半开窗口 `[0, 此刻)`：下界是 Unix 毫秒起点（不是"最近 N 天"这种任意窗口）
    expect(request.from).toBe(0);
    expect(typeof request.to).toBe("number");
    expect(request.to as number).toBeGreaterThan(0);
    expect(request.limit).toBe(PAGE_SIZE);
    expect(request.offset).toBe(0);
    // 省略（而不是传 null）= 只要列表
    expect("session_id" in request).toBe(false);

    expect(screen.getByTestId("history-scope").textContent).toContain("到此刻为止的全部历史");
    expect(within(screen.getByTestId(`history-row-${FINISHED}`)).getByText("已结束")).not.toBeNull();
    expect(
      within(screen.getByTestId("history-row-session-discarded")).getByText("已丢弃"),
    ).not.toBeNull();
  });

  it("翻页：取满一页才给「下一页」，翻过去改成 offset；上一页回头", async () => {
    // 反向验证：把 `canNext` 改成恒真（或恒假）⇒ 50 条夹具下那句 `disabled` 断言红；
    // 把 `setOffset(offset + PAGE_SIZE)` 写成 `setOffset(offset + 1)` ⇒ 请求里的 offset 红。
    backend = createBackend();
    backend.history = historyView({
      sessions: Array.from({ length: PAGE_SIZE }, (_, index) =>
        session({ id: `session-${index}`, task_id: `task-${index}` }),
      ),
    });
    await mountHistory();

    expect((screen.getByTestId("history-prev") as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByTestId("history-next") as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(screen.getByTestId("history-next"));
    await waitFor(() => expect(lastRequest("history_view").offset).toBe(PAGE_SIZE));
    expect(screen.getByTestId("history-page-info").textContent).toBe("第 2 页 · 本页 0 条");
    // 第二页空了（夹具只有一页）：取不满 ⇒ 不给下一页；但可以回去
    await waitFor(() =>
      expect((screen.getByTestId("history-next") as HTMLButtonElement).disabled).toBe(true),
    );
    expect((screen.getByTestId("history-prev") as HTMLButtonElement).disabled).toBe(false);
  });
});

describe("历史页：详情、修正与补录", () => {
  it("打开详情：请求带那条会话 id，区间/审计/软删除说明都上屏", async () => {
    // 反向验证：把 `session_id` 从请求里删掉 ⇒ 详情永远为空、下面三句 `getByTestId` 红。
    backend = createBackend();
    backend.history = finishedDetail();
    await mountHistory();

    fireEvent.click(screen.getByTestId(`history-open-${FINISHED}`));
    await waitFor(() => expect(lastRequest("history_view").session_id).toBe(FINISHED));

    expect(screen.getByTestId("history-detail-session-id").textContent).toBe(FINISHED);
    expect(screen.getByTestId("history-detail-state").textContent).toBe("已结束");
    expect(screen.getByTestId("history-detail-version").textContent).toBe("版本 9");
    expect(screen.getByTestId(`history-interval-${I_OK}`)).not.toBeNull();
    expect(screen.getByTestId("history-edits").textContent).toContain("reconcile_confirm");
    // 软删除的说明可见（不是只藏在确认框里）
    expect(screen.getByTestId("history-soft-delete-note").textContent).toContain("软删除");
    expect(screen.getByTestId("history-soft-delete-note").textContent).toContain("审计");
  });

  it("correct 入口按状态开：只有已结束会话的已确认区间能改", async () => {
    // 反向验证：把 `correctable` 里的 `session.state === "finished"` 去掉 ⇒ 待确认会话
    // 也会拿到"重定时"按钮，下面 `queryByTestId(...).toBeNull()` 红；
    // 去掉 `voided_at === null` / `!needs_review` ⇒ 另两句红。
    backend = createBackend();
    backend.history = finishedDetail();
    await mountHistory();
    fireEvent.click(screen.getByTestId(`history-open-${FINISHED}`));
    await waitFor(() => expect(screen.getByTestId(`history-interval-${I_OK}`)).not.toBeNull());

    // 已确认、未作废 ⇒ 有入口
    expect(screen.getByTestId(`history-retime-${I_OK}`)).not.toBeNull();
    expect(screen.getByTestId(`history-delete-${I_OK}`)).not.toBeNull();
    // 已作废 / 待确认 ⇒ 都没有入口，并各给一句原因
    expect(screen.queryByTestId(`history-retime-${I_VOIDED}`)).toBeNull();
    expect(screen.queryByTestId(`history-delete-${I_VOIDED}`)).toBeNull();
    expect(within(screen.getByTestId(`history-interval-${I_VOIDED}`)).getByText(/已作废的区间/)).not.toBeNull();
    expect(screen.queryByTestId(`history-retime-${I_PENDING}`)).toBeNull();
    expect(within(screen.getByTestId(`history-interval-${I_PENDING}`)).getByText(/归恢复页处理/)).not.toBeNull();
  });

  it("非 finished 的会话：correct 入口整条禁用，并说明该去哪一页", async () => {
    // 反向验证：把 `correctable` 里的 `session.state === "finished"` 去掉 ⇒ 这条会话里
    // **已确认**的那条区间也会拿到"重定时"按钮，下面两句 `queryByTestId(...).toBeNull()` 红
    // （夹具刻意给一条已确认区间：只给待确认区间的话，`!needs_review` 那一半就会替状态门禁兜底，
    // 断言就抓不到状态那一半了）。
    const I_PREFIX = "interval-prefix";
    backend = createBackend();
    backend.history = historyView({
      sessions: [session({ id: OPEN, state: "recovering", needs_review: true })],
      selected: historyDetail({
        session: session({ id: OPEN, state: "recovering", needs_review: true }),
        // `recovering` 的正常形状：可信前缀（已确认闭合）+ 待确认余段，两条都在详情里。
        intervals: [
          interval({ id: I_PREFIX, session_id: OPEN }),
          interval({ id: I_PENDING, session_id: OPEN, needs_review: true, duration_ms: null }),
        ],
        edits: [],
      }),
    });
    await mountHistory();

    fireEvent.click(screen.getByTestId(`history-open-${OPEN}`));
    await waitFor(() => expect(screen.getByTestId("history-detail-state").textContent).toBe("待恢复"));

    expect(screen.queryByTestId(`history-retime-${I_PREFIX}`)).toBeNull();
    expect(screen.queryByTestId(`history-delete-${I_PREFIX}`)).toBeNull();
    expect(screen.queryByTestId(`history-retime-${I_PENDING}`)).toBeNull();
    expect(screen.queryByTestId(`history-delete-${I_PENDING}`)).toBeNull();
    expect(screen.getByTestId("history-correct-disabled-note").textContent).toContain(
      "只有已结束的会话能修正起止或删除误记",
    );
    expect(screen.getByTestId("history-correct-disabled-note").textContent).toContain(
      "请先在恢复页对账",
    );
  });

  it("补录任务下拉不是一堵墙：列了多少条可见、能加载下一页，加载到的任务能直接补录", async () => {
    // 反向验证：把 `loadTasks(tasks.length)` 写回 `loadTasks(0)`、或把"追加"改回"覆盖"⇒
    // 第 51 条永远进不了候选，`offset: 50` 与 `task_id: "task-50"` 两处断言红；
    // 把「已列出 N / 共 M 条」那一句删掉 ⇒ "没列全"又变回静默截断，第一句断言红。
    backend = createBackend();
    backend.history = historyView();
    // 51 条：一页装不下（`list_tasks` 的排序是 `ORDER BY created_at, id` **升序** ⇒
    // 只取第一页时被截掉的正是最后建的那一条）。
    backend.tasks = Array.from({ length: PAGE_SIZE + 1 }, (_, index) =>
      task({ id: `task-${index}`, title: `任务 ${index}`, status: "Ready" }),
    );
    await mountHistory();

    openSelect("history-backfill-task");
    expect(screen.getByTestId("history-backfill-tasks-info").textContent).toBe("已列出 50 / 共 51 条");
    // 第 51 条还没进候选（还在没加载的那一页里）
    expect(screen.queryByTitle("任务 50")).toBeNull();

    fireEvent.click(screen.getByTestId("history-backfill-load-more"));
    await waitFor(() => expect(backend.count("list_tasks")).toBe(2));
    expect(lastRequest("list_tasks")).toEqual({
      statuses: [],
      project: "any",
      limit: PAGE_SIZE,
      offset: PAGE_SIZE,
      expected_data_epoch: EPOCH,
    });
    await waitFor(() =>
      expect(screen.getByTestId("history-backfill-tasks-info").textContent).toBe(
        "已列出 51 / 共 51 条",
      ),
    );
    // 取满了就不给入口（判据是服务端给的 `total`）
    expect((screen.getByTestId("history-backfill-load-more") as HTMLButtonElement).disabled).toBe(
      true,
    );

    // 搜索是**本地过滤**已加载的候选（契约没有关键词字段：`TaskFilter` 只有
    // statuses / project / context_tag_id，"后端搜索"今天不存在）
    const search = selectRoot("history-backfill-task").querySelector(
      "input.ant-select-input",
    ) as HTMLInputElement;
    fireEvent.change(search, { target: { value: "任务 50" } });
    await waitFor(() => expect(screen.queryByTitle("任务 49")).toBeNull());
    expect(screen.getByTitle("任务 50")).not.toBeNull();

    // 「列出来」要能落到 `backfill` 的 `task_id` 上
    fireEvent.click(screen.getByTitle("任务 50"));
    await act(async () => undefined);
    fireEvent.change(screen.getByTestId("history-backfill-start"), {
      target: { value: "2026-10-03 09:00" },
    });
    fireEvent.change(screen.getByTestId("history-backfill-end"), {
      target: { value: "2026-10-03 10:00" },
    });
    fireEvent.click(screen.getByTestId("history-backfill-submit"));

    await waitFor(() => expect(backend.count("backfill")).toBe(1));
    expect(lastRequest("backfill").task_id).toBe("task-50");
  });

  it("已作废的会话：文案说的是它自己的原因，不是「请先结束这次会话」", async () => {
    // 反向验证：把 `correctionHint` 的 `discarded` 分支删掉（落回"请先结束这次会话"）⇒
    // 下面两句红。本页能打开的详情只有 `finished` 与 `discarded` 两种状态，所以对已作废的
    // 会话说"请先结束"是一条**永远做不到**的指引。
    backend = createBackend();
    backend.history = historyView({
      sessions: [session({ id: DISCARDED, state: "discarded", needs_review: false })],
      selected: historyDetail({
        session: session({ id: DISCARDED, state: "discarded", needs_review: false }),
        intervals: [
          interval({
            id: I_VOIDED,
            session_id: DISCARDED,
            voided_at: AT + 3_600_000,
            duration_ms: null,
          }),
        ],
        edits: [],
      }),
    });
    await mountHistory();

    fireEvent.click(screen.getByTestId(`history-open-${DISCARDED}`));
    await waitFor(() =>
      expect(screen.getByTestId("history-detail-state").textContent).toBe("已丢弃"),
    );

    const note = screen.getByTestId("history-correct-disabled-note");
    expect(note.textContent).toContain("已作废的会话不能修正起止或删除误记");
    expect(note.textContent).not.toContain("请先结束这次会话");
    // 已作废的区间也没有修正入口（"修正只接受已确认且未作废的区间"那一半）
    expect(screen.queryByTestId(`history-retime-${I_VOIDED}`)).toBeNull();
    expect(screen.queryByTestId(`history-delete-${I_VOIDED}`)).toBeNull();
    // 软删除那句只属于"能改的会话"：已作废的会话上不该出现
    expect(screen.queryByTestId("history-soft-delete-note")).toBeNull();
  });

  it("重定时：请求的起止与输入逐字一致，版本取自**详情**，成功后重拉", async () => {
    // 反向验证：把 `expected_row_version` 换成列表里那一行的版本（4）⇒ 逐字段断言红；
    // 删掉成功后的 `afterWrite()` ⇒ 重拉计数红。
    backend = createBackend();
    backend.history = finishedDetail();
    await mountHistory();
    fireEvent.click(screen.getByTestId(`history-open-${FINISHED}`));
    await waitFor(() => expect(screen.getByTestId(`history-retime-${I_OK}`)).not.toBeNull());
    // 打开详情本身是一次读（`session_id` 变了），所以基线要在它之后再取
    const before = backend.count("history_view");

    fireEvent.change(screen.getByTestId(`history-retime-start-${I_OK}`), {
      target: { value: "2026-10-03 09:00" },
    });
    fireEvent.change(screen.getByTestId(`history-retime-end-${I_OK}`), {
      target: { value: "2026-10-03 09:45" },
    });
    fireEvent.click(screen.getByTestId(`history-retime-${I_OK}`));

    await waitFor(() => expect(backend.count("correct")).toBe(1));
    expect(lastRequest("correct")).toEqual({
      expected_data_epoch: EPOCH,
      session_id: FINISHED,
      expected_row_version: 9,
      interval_id: I_OK,
      action: "retime",
      started_at: localMs("2026-10-03 09:00"),
      ended_at: localMs("2026-10-03 09:45"),
    });
    await waitFor(() => expect(backend.count("history_view")).toBe(before + 1));
  });

  it("删除误记：软删除说明可见、二次确认之前不发命令、请求不带起止", async () => {
    // 反向验证：把 Popconfirm 拿掉（点一下直接发）⇒ 第一次的 `correct` 计数红；
    // 给 `delete` 也带上 `started_at`/`ended_at`（`retime` 的键）⇒ `toEqual` 的形状断言红。
    backend = createBackend();
    backend.history = finishedDetail();
    await mountHistory();
    fireEvent.click(screen.getByTestId(`history-open-${FINISHED}`));
    await waitFor(() => expect(screen.getByTestId(`history-delete-${I_OK}`)).not.toBeNull());

    fireEvent.click(screen.getByTestId(`history-delete-${I_OK}`));
    expect(backend.count("correct")).toBe(0);
    // 确认框开了（确认按钮出现）——"软删除"那句说明在详情卡里另有一份，见上一条用例
    expect(await screen.findByText("确认删除")).not.toBeNull();

    fireEvent.click(await screen.findByText("确认删除"));
    await waitFor(() => expect(backend.count("correct")).toBe(1));
    expect(lastRequest("correct")).toEqual({
      expected_data_epoch: EPOCH,
      session_id: FINISHED,
      expected_row_version: 9,
      interval_id: I_OK,
      action: "delete",
    });
  });

  it("补录：独立命令（不夹带 start_timer），请求三段取自表单", async () => {
    // 反向验证：把补录接成"先 `start_timer` 再结束"两步 ⇒ `commands` 里出现 `start_timer`，
    // 那句 `not.toContain` 红（F-017：补录不启动计时、不伪造完成事件）。
    backend = createBackend();
    backend.history = historyView();
    backend.tasks = [task({ id: "task-7", title: "写季报", status: "Ready" })];
    await mountHistory();

    await pick("history-backfill-task", "写季报");
    fireEvent.change(screen.getByTestId("history-backfill-start"), {
      target: { value: "2026-10-03 09:00" },
    });
    fireEvent.change(screen.getByTestId("history-backfill-end"), {
      target: { value: "2026-10-03 10:30" },
    });
    fireEvent.click(screen.getByTestId("history-backfill-submit"));

    await waitFor(() => expect(backend.count("backfill")).toBe(1));
    expect(lastRequest("backfill")).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "task-7",
      started_at: localMs("2026-10-03 09:00"),
      ended_at: localMs("2026-10-03 10:30"),
    });
    expect(backend.commands).toContain("backfill");
    expect(backend.commands).not.toContain("start_timer");
    // 补录的说明也写着这两句（不是只靠按钮文案暗示）
    expect(screen.getByTestId("history-backfill-note").textContent).toContain("不启动计时");
    expect(screen.getByTestId("history-backfill-note").textContent).toContain("不伪造完成事件");
    await waitFor(() => expect(backend.count("history_view")).toBe(2));
  });
});

describe("历史页：版本冲突、判旧与查询失败", () => {
  it("VERSION_CONFLICT：上屏可操作提示、不自动重试，并按既有口径冲突刷新", async () => {
    // 反向验证：把失败那支改成"自动重发一次 `correct`"⇒ `correct` 计数红；
    // 把 `reportCommandError` 换成自己编一句文案 ⇒ 那句逐字断言红。
    backend = createBackend();
    backend.history = finishedDetail();
    backend.fail.correct = failure({
      code: "VERSION_CONFLICT",
      message: "这条记录已被修改，请刷新后重试。",
    });
    await mountHistory();
    fireEvent.click(screen.getByTestId(`history-open-${FINISHED}`));
    await waitFor(() => expect(screen.getByTestId(`history-retime-${I_OK}`)).not.toBeNull());
    const before = backend.count("history_view");

    fireEvent.change(screen.getByTestId(`history-retime-start-${I_OK}`), {
      target: { value: "2026-10-03 09:00" },
    });
    fireEvent.change(screen.getByTestId(`history-retime-end-${I_OK}`), {
      target: { value: "2026-10-03 09:45" },
    });
    fireEvent.click(screen.getByTestId(`history-retime-${I_OK}`));

    expect((await screen.findByRole("alert")).textContent).toBe("这条记录已被修改，请刷新后重试。");
    // **不**自动重试那条写命令
    expect(backend.count("correct")).toBe(1);
    // 但版本冲突要刷新（R8 的既有口径：推一次缓存失效 + 重取），页面据此重拉一次
    await waitFor(() => expect(backend.count("history_view")).toBe(before + 1));
  });

  it("本视图水位：迟到的旧响应不得覆盖已经上屏的那一页", async () => {
    // 反向验证：删掉 `watermark.isStale(found, epoch)` 那一句 ⇒ 旧响应把新列表顶回去，
    // 最后那句断言红。
    backend = createBackend();
    backend.history = historyView({
      sessions: [session({ id: "session-old", task_id: "task-old" })],
    });
    const slow = backend.holdNext<HistoryView>("history_view");
    await mountHistory();

    backend.revision = 6;
    backend.history = historyView({
      sessions: [session({ id: "session-new", task_id: "task-new" })],
      revision: 6,
    });
    await changed(6);
    await waitFor(() => expect(screen.getByTestId("history-row-session-new")).not.toBeNull());

    await act(async () => {
      slow.resolve(
        historyView({
          sessions: [session({ id: "session-old", task_id: "task-old" })],
          revision: 5,
        }),
      );
    });
    expect(screen.getByTestId("history-row-session-new")).not.toBeNull();
    expect(screen.queryByTestId("history-row-session-old")).toBeNull();
  });

  it("查询失败只提示、不重拉：上屏的是 Rust 的 message", async () => {
    // 反向验证：把查询失败那支也改成 `reportCommandError` ⇒ 它内部会 `refresh()`，
    // 下面"只读了一次"的计数断言红（查询路径自触发重拉）。
    backend = createBackend();
    backend.fail.history_view = failure({
      code: "DOMAIN_ERROR",
      message: "操作不被允许：窗口不合法。",
    });
    render(<History />);
    await act(async () => {
      await domainState.start();
    });

    expect((await screen.findByRole("alert")).textContent).toBe("操作不被允许：窗口不合法。");
    expect(backend.count("history_view")).toBe(1);
  });
});
