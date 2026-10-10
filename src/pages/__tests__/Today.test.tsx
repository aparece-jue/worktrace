/**
 * 今日页用例（P8 Task 1b）：F-010 的五项 + 今日选择列表的增删。
 *
 * 页面只做**展示与转发**——统计口径、日界换算、三类分列、"作废不算待确认"全在 Rust
 * （`services/stats.rs`），前端一个数字都不重算。这里逐条钉住的是：
 *
 * 1. **五项分别渲染、互不覆盖**：三组工时给互不相同的数字，各在各的卡片里；
 * 2. **人工与机器不合并**：机器（后台/被动）与等待分列可见，页面上不存在"人工 + 机器"的和；
 * 3. **待确认看 `intervals`**：一条候选都没有 ⇒ `—` / `0 条`，且**不按今日列表的条数**推；
 * 4. **口径标注可见**（R-03）：`date` / `timezone` / `range`（半开）与"按当前分类"；
 * 5. **失效即重拉**：`domain.changed` 之后数字换成新那一版；
 * 6. **空数据是 `0`**：不是空白、不是错误；
 * 7. **本视图水位**：迟到的旧响应不得覆盖已经上屏的新数字；
 * 8. **增删的请求逐字正确**：`date` / `timezone` 取**响应**里的（不是 JS 自己算的今天），
 *    成功后重拉一次；失败走 `reportCommandError`（展示 Rust 的 `message`）。
 *
 * 反向验证写在每条用例里（"改坏什么会让它红"）。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
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

import { Today } from "../Today";
import { domainState } from "../../state/domainState";
import { EVENT_DOMAIN_CHANGED, type TodayView } from "../../types/ipc";
import {
  AT,
  EPOCH,
  TODAY_DATE,
  TODAY_ZONE,
  createBackend,
  failure,
  measureGroup,
  task,
  todayView,
  type Backend,
} from "./fakeBackend";
import { installJsdomBridges } from "./jsdomBridges";

let backend: Backend;

/** 装上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。**页面没有 props**。 */
async function mountToday(): Promise<RenderResult> {
  const view = render(<Today />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("stats_today")).toBeGreaterThan(0));
  return view;
}

/** 最近一条某命令的入参。 */
function lastRequest(command: string): unknown {
  const found = [...backend.requests].reverse().find((entry) => entry.command === command);
  return found?.request;
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
 * 本地时间的 `YYYY-MM-DD HH:MM`——用例侧**独立算一遍**页面上那句区间标注。
 *
 * 为什么不写死字符串：`range` 的两个端点按本机时区渲染，写死就只能在某一个时区的机器上绿。
 * 独立算的这一遍仍然能判红：页面要是拿 `date` 凑、或者写死常量，这里的期望值就对不上。
 */
function localStamp(ms: number): string {
  const at = new Date(ms);
  const pad = (value: number): string => String(value).padStart(2, "0");
  const date = `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
  return `${date} ${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/**
 * 一份"五项齐全、数字互不相同"的响应。
 *
 * 三组给的数字刻意交错：`01:01` / `02:02` / `03:20` / `05:10` / `06:40` / `08:20`，
 * 而"人工 + 机器"的两种和是 `03:03` 与 `06:23`——页面上出现其中任何一个都是相加出来的。
 */
function populatedView(overrides: Partial<TodayView> = {}): TodayView {
  return todayView({
    tasks: [task({ id: "t-plan", title: "今日已选", status: "Ready" })],
    confirmed: measureGroup(
      "confirmed",
      { human: 61_000, machine_background: 122_000, machine_passive: 200_000, waiting: 310_000 },
      { human: 2, machine_background: 2, machine_passive: 1, waiting: 1 },
    ),
    live: measureGroup("live", { human: 400_000 }, { human: 1 }),
    pending: measureGroup("pending", { human: 500_000 }, { human: 3 }),
    ...overrides,
  });
}

beforeAll(installJsdomBridges);
// Keep the wall clock inside the backend fixture day; boundary expiry has its own tests.
beforeEach(() => {
  vi.spyOn(Date, "now").mockReturnValue(todayView().range.from + 3_600_000);
});

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
  vi.restoreAllMocks();
});

describe("今日页：F-010 五项", () => {
  it("五项各占一块，数字取自各自的列（三组数字互不相同、互不覆盖）", async () => {
    // 反向验证：把任何一项接到别的列上（或漏掉一个渲染分支）⇒ 下面那句逐字断言红。
    backend = createBackend();
    backend.view = populatedView();
    await mountToday();

    for (const testId of [
      "today-plan",
      "today-current",
      "today-confirmed",
      "today-live",
      "today-pending",
    ]) {
      expect(screen.getByTestId(testId), `缺少 ${testId}`).not.toBeNull();
    }

    // ① 今日选择列表：逐行（标题 + 状态标签）
    const row = screen.getByTestId("today-task-t-plan");
    expect(within(row).getByText("今日已选")).not.toBeNull();
    expect(within(row).getByText("Ready")).not.toBeNull();

    // ② 当前任务（本夹具没有装载过任何会话）
    expect(screen.getByTestId("today-current-none").textContent).toBe("当前没有会话");

    // ③④⑤ 三个数字分别来自 confirmed / live / pending 的 human 列
    expect(screen.getByTestId("today-confirmed-human").textContent).toBe("01:01");
    expect(screen.getByTestId("today-live-human").textContent).toBe("06:40");
    expect(screen.getByTestId("today-pending-human").textContent).toBe("08:20");

    // 互不覆盖：每张卡片里没有别的那两个数字
    expect(within(screen.getByTestId("today-confirmed")).queryByText("06:40")).toBeNull();
    expect(within(screen.getByTestId("today-confirmed")).queryByText("08:20")).toBeNull();
    expect(within(screen.getByTestId("today-live")).queryByText("01:01")).toBeNull();
    expect(within(screen.getByTestId("today-live")).queryByText("08:20")).toBeNull();
    expect(within(screen.getByTestId("today-pending")).queryByText("01:01")).toBeNull();
    expect(within(screen.getByTestId("today-pending")).queryByText("06:40")).toBeNull();

    // 读命令的形状：`stats_today` 只带时区与 epoch（日期由服务算）；候选查询与收件箱同口径
    expect(lastRequest("stats_today")).toEqual({
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
      expected_data_epoch: EPOCH,
    });
    expect(lastRequest("list_tasks")).toEqual({
      statuses: ["Inbox", "Clarifying", "Ready", "Doing"],
      project: "any",
      limit: 50,
      offset: 0,
      expected_data_epoch: EPOCH,
    });
    // 不使用 `plan_for`：`TodayView.tasks` 已经是同一个今日选择列表（再发一条会造出第二个水位）
    expect(backend.commands).not.toContain("plan_for");
  });

  it("人工与机器不合并：三个机器 measure 分列可见，页面上没有人工+机器的和", async () => {
    // 反向验证：把机器时长并进人工那一格（或渲染一个"总计"）⇒ 下面两句 `not.toContain` 红。
    backend = createBackend();
    backend.view = populatedView();
    const { container } = await mountToday();

    expect(screen.getByTestId("today-confirmed-machine_background").textContent).toBe("02:02");
    expect(screen.getByTestId("today-confirmed-machine_passive").textContent).toBe("03:20");
    expect(screen.getByTestId("today-confirmed-waiting").textContent).toBe("05:10");
    // 人工那一格就是 confirmed.human 自己——不是"把机器并进来"的和
    expect(screen.getByTestId("today-confirmed-human").textContent).toBe("01:01");
    // 分列是**可见的口径**，不只是三个数字丢了标签
    const confirmed = screen.getByTestId("today-confirmed");
    expect(within(confirmed).getByText("机器（后台）")).not.toBeNull();
    expect(within(confirmed).getByText("机器（被动）")).not.toBeNull();
    expect(within(confirmed).getByText("等待")).not.toBeNull();

    const page = container.textContent ?? "";
    expect(page).not.toContain("03:03"); // 人工 01:01 + 机器（后台）02:02
    expect(page).not.toContain("06:23"); // 人工 + 机器（后台 + 被动）
    expect(page).not.toContain("11:33"); // 四个 measure 全加起来
  });

  it("作废不进待确认：intervals 为 0 ⇒ 0 条 / ms 未给数 ⇒ —，页面不按今日条数推", async () => {
    // 反向验证：把 `today-pending-human-count` 改成数今日列表的条数（`tasks.length`）
    // 或自己拿 live/confirmed 拼 ⇒ 下面两句红（夹具里今日有两条任务，而待确认是 0 条）。
    backend = createBackend();
    backend.view = populatedView({
      tasks: [
        task({ id: "t-1", title: "甲", status: "Ready" }),
        task({ id: "t-2", title: "乙", status: "Ready" }),
      ],
      pending: measureGroup("pending", { human: null }, { human: 0 }),
    });
    await mountToday();

    // 一条已知端点的候选都没有 ⇒ 毫秒是"未给数"（不推算），不是 0
    expect(screen.getByTestId("today-pending-human").textContent).toBe("—");
    // 有没有待确认看 `intervals`（Ruling P5-12），不是看 `ms === null`
    expect(screen.getByTestId("today-pending-human-count").textContent).toBe("0 条");
  });

  it("口径标注可见：date / timezone / range（半开）与「按当前分类」都在页面上", async () => {
    // 反向验证：把区间标注换成本地"现在"（或拿 `date` 凑两个端点）⇒ 那句逐字断言红。
    backend = createBackend();
    backend.view = populatedView();
    await mountToday();

    const scope = screen.getByTestId("today-scope");
    expect(screen.getByTestId("today-date").textContent).toBe(TODAY_DATE);
    expect(screen.getByTestId("today-timezone").textContent).toBe(TODAY_ZONE);
    expect(screen.getByTestId("today-range").textContent).toBe(
      `[${localStamp(backend.view.range.from)}, ${localStamp(backend.view.range.to)})`,
    );
    expect(scope.textContent).toContain("按当前分类");
    expect(scope.textContent).toContain("半开");
  });

  it("失效即重拉：`domain.changed` 之后数字换成新那一版", async () => {
    // 反向验证：把 `useInvalidation()` 从 `useEffect` 依赖里删掉 ⇒ 失效不再触发重拉，
    // 下面那句新数字与计数断言一起红。
    backend = createBackend();
    backend.view = populatedView();
    await mountToday();
    expect(screen.getByTestId("today-confirmed-human").textContent).toBe("01:01");

    backend.revision = 6;
    backend.view = populatedView({ confirmed: measureGroup("confirmed", { human: 90_000 }) });
    await changed(6);

    await waitFor(() =>
      expect(screen.getByTestId("today-confirmed-human").textContent).toBe("01:30"),
    );
    expect(backend.count("stats_today")).toBe(2);
  });

  it("空数据：三个数字都是 0 时显示 0（不是空白），也不报错", async () => {
    // 反向验证：把 `formatMs` 改成返回空串（或让 0 走 `—`）⇒ 下面三句全红。
    backend = createBackend();
    backend.view = todayView({ pending: measureGroup("pending", { human: 0 }) });
    await mountToday();

    expect(screen.getByTestId("today-confirmed-human").textContent).toBe("0");
    expect(screen.getByTestId("today-live-human").textContent).toBe("0");
    expect(screen.getByTestId("today-pending-human").textContent).toBe("0");
    expect(screen.getByTestId("today-pending-human-count").textContent).toBe("0 条");
    expect(await screen.findByText(/今天还没有选择任务/)).not.toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("本视图水位：迟到的旧响应不得覆盖已经上屏的新数字", async () => {
    // 反向验证：删掉 `watermark.isStale(today, epoch)` 那一句 ⇒ 旧响应把新数字顶回去，
    // 最后一句红（这正是 Inbox/Projects 用同一把水位防的那件事）。
    backend = createBackend();
    backend.view = populatedView();
    const slow = backend.holdNext<TodayView>("stats_today");
    await mountToday();

    backend.revision = 6;
    backend.view = populatedView({ confirmed: measureGroup("confirmed", { human: 90_000 }) });
    await changed(6);
    await waitFor(() =>
      expect(screen.getByTestId("today-confirmed-human").textContent).toBe("01:30"),
    );

    // 第一次那条请求现在才回来（revision 5，旧数字）
    await act(async () => {
      slow.resolve(populatedView({ revision: 5 }));
    });
    expect(screen.getByTestId("today-confirmed-human").textContent).toBe("01:30");
  });
});

describe("今日页：候选下拉与五项互不牵连", () => {
  it("候选查询失败不拖累五项：主数据照常上屏，候选区降级为空且提示失败", async () => {
    // 反向验证（fix round 1 / Important-1）：把候选查询放回与 `statsToday` 同一个
    // `Promise.all` + 同一个错误态 ⇒ 这里 `list_tasks` 一失败 `setView` 就永不执行，
    // 五项一个都不渲染（页面只剩 `today-loading`），下面五句一起红。
    backend = createBackend();
    backend.view = populatedView();
    backend.tasks = [task({ id: "t-free", title: "候选中", status: "Inbox" })];
    backend.fail.list_tasks = failure({ code: "DOMAIN_ERROR", message: "候选读失败。" });
    await mountToday();

    // ①③④⑤ 照常：今日列表、三个数字，且不是加载态
    expect(screen.getByTestId("today-task-t-plan")).not.toBeNull();
    expect(screen.getByTestId("today-confirmed-human").textContent).toBe("01:01");
    expect(screen.getByTestId("today-live-human").textContent).toBe("06:40");
    expect(screen.getByTestId("today-pending-human").textContent).toBe("08:20");
    expect(screen.queryByTestId("today-loading")).toBeNull();
    // ② 当前任务也在
    expect(screen.getByTestId("today-current-none").textContent).toBe("当前没有会话");

    // 候选区降级：下拉里没有可选项；错误提示**不**属于这一次失败（它只跟五项那条查询走）
    openSelect("today-candidate");
    expect(screen.queryByTitle("候选中")).toBeNull();
    expect(screen.getByRole("alert").textContent).toContain("候选读失败");
  });
});

describe("今日页：当前任务与运行状态", () => {
  it("current 非空不等于正在计时：已结束的会话只报状态，running + 开放区间才报正在计时", async () => {
    // 反向验证：把"是否正在计时"改成按 `current !== null` 判（Ruling P6-21 禁止）⇒ 下面
    // 那句「未在计时」立刻红。
    backend = createBackend();
    backend.view = populatedView({
      current: { session_id: "s-1", task_id: "t-1", task_title: "写周报", state: "finished" },
      live: measureGroup("live", { human: 0 }, { human: 0 }),
    });
    await mountToday();

    expect(screen.getByTestId("today-current-title").textContent).toBe("写周报");
    expect(screen.getByTestId("today-current-state").textContent).toBe("已结束");
    expect(screen.getByTestId("today-running").textContent).toBe("未在计时");

    // 会话真的在跑、live 列也有开放区间 ⇒ 才报「正在计时」
    backend.revision = 6;
    backend.view = populatedView({
      current: { session_id: "s-1", task_id: "t-1", task_title: "写周报", state: "running" },
      live: measureGroup("live", { human: 12_000 }, { human: 1 }),
    });
    await changed(6);

    await waitFor(() => expect(screen.getByTestId("today-running").textContent).toBe("正在计时"));
    expect(screen.getByTestId("today-current-state").textContent).toBe("运行中");
    expect(screen.getByTestId("today-live-human").textContent).toBe("00:12");
  });
});

describe("今日页：今日选择列表的增删", () => {
  it("加入今日：候选排除已在列表里的，请求的 date/timezone 取响应，成功后重拉", async () => {
    // 反向验证：把请求里的 `date`/`timezone` 换成 JS 自己算的（或漏掉 `expected_data_epoch`）
    // ⇒ 那句逐字段断言红；成功后的重拉删掉 ⇒ 新行与计数断言红。
    backend = createBackend();
    backend.view = populatedView();
    backend.tasks = [
      task({ id: "t-plan", title: "今日已选", status: "Ready" }),
      task({ id: "t-free", title: "候选中", status: "Inbox" }),
    ];
    await mountToday();

    openSelect("today-candidate");
    expect(await screen.findByTitle("候选中")).not.toBeNull();
    // 已经在今日列表里的那条**不**出现在候选里（按 id 排除）
    expect(screen.queryByTitle("今日已选")).toBeNull();
    fireEvent.click(screen.getByTitle("候选中"));
    await act(async () => undefined);

    fireEvent.click(screen.getByTestId("today-add"));
    await waitFor(() => expect(backend.count("add_to_plan")).toBe(1));
    expect(lastRequest("add_to_plan")).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "t-free",
      date: TODAY_DATE,
      timezone: TODAY_ZONE,
    });

    // 成功后重拉一次：新任务出现在今日列表里（假后端把 add 的效果记在 `view.tasks` 上）
    await waitFor(() => expect(backend.count("stats_today")).toBe(2));
    expect(await screen.findByTestId("today-task-t-free")).not.toBeNull();
  });

  it("移除：请求的 task_id 是那一行，成功后重拉（那一行消失、别的行留着）", async () => {
    // 反向验证：把 `remove` 的 `task_id` 写死 / 成功后不重拉 ⇒ 逐字段断言或下面那句红。
    backend = createBackend();
    backend.view = populatedView({
      tasks: [
        task({ id: "t-plan", title: "今日已选", status: "Ready" }),
        task({ id: "t-keep", title: "留着", status: "Doing" }),
      ],
    });
    backend.tasks = backend.view.tasks;
    await mountToday();

    fireEvent.click(screen.getByTestId("remove-t-plan"));
    await waitFor(() => expect(backend.count("remove_from_plan")).toBe(1));
    expect(lastRequest("remove_from_plan")).toEqual({
      expected_data_epoch: EPOCH,
      task_id: "t-plan",
      date: TODAY_DATE,
      timezone: TODAY_ZONE,
    });

    await waitFor(() => expect(backend.count("stats_today")).toBe(2));
    await waitFor(() => expect(screen.queryByTestId("today-task-t-plan")).toBeNull());
    expect(screen.getByTestId("today-task-t-keep")).not.toBeNull();
  });

  it("写失败：展示 Rust 的 `message`，且不把失败的当成功（不重拉、不乐观改列表）", async () => {
    // 反向验证：把失败那支也调一次 `afterWrite()`（乐观刷新）⇒ 计数断言红；
    // 换成自己编一句文案 ⇒ 那句逐字断言红（R8：用户文案只有 Rust 一个来源）。
    backend = createBackend();
    backend.view = populatedView();
    backend.tasks = [task({ id: "t-free", title: "候选中", status: "Inbox" })];
    backend.fail.add_to_plan = failure({ code: "DOMAIN_ERROR", message: "这一天已经加过了。" });
    await mountToday();

    await pick("today-candidate", "候选中");
    fireEvent.click(screen.getByTestId("today-add"));

    expect((await screen.findByRole("alert")).textContent).toBe("这一天已经加过了。");
    // 失败 ⇒ 不重拉（只有挂载那一次读），候选也不会"乐观地"变成今日列表里的一行
    expect(backend.count("stats_today")).toBe(1);
    expect(screen.queryByTestId("today-task-t-free")).toBeNull();
  });
});
