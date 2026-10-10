/**
 * 外壳用例（P7 Task 1b 建立，Task 3 / 接线轮更新）：应用能挂载、渲染外壳与页面，
 * 并**经镜像的唯一入口**启动事件会话。
 *
 * Task 1b 那条「一个命令都不调用」的断言到这里必然要改：Task 3 把页面接进外壳之后，
 * 挂载就会由 `domainState.start()` 发 `get_revision` + `timer_snapshot`，收件箱页再发两条
 * 读查询。所以现在钉的是**命令集合恰好是这四条**——多一条（模板页留下的 `greet`、
 * 或页面自己偷偷开第二条订阅/第二条查询）都会红。**不做去重**：每条命令恰好被调用一次
 * 也是这条断言的一部分。
 *
 * 外壳**不持有任何跨页面的业务状态**（接线轮删掉了过渡的「当前任务」）：计时页与状态栏的
 * 标题都读快照自己的 `task_title`。下面两条用例把这一点钉住：快照给了标题就显示，
 * 没给就整段不渲染——**任何状态（含结束后那一拍）都不出现占位文案**。
 *
 * Task 5 把项目页与任务页接进同一个挂载区：导航四项与「切过去就发它自己的读查询」
 * 由一条用例钉住（默认页仍是收件箱，所以上面那条"恰好四条命令"的断言不受影响）。
 * P8 Task 1b 加第 5 块页面「今日」：同一条用例扩到五项，并为 `stats_today` 补一条假响应
 * ——**默认页仍是收件箱**，所以"挂载时恰好四条命令"那条判据照旧（多一条就说明默认页被改了）。
 * P8 Task 2c 再加第 6、7 块「恢复」「历史」：导航与"切过去发自己的读查询"扩到七项；
 * 另加一条 M8 用例——`RECOVERY_REQUIRED` 要**把用户导到恢复页**（`requestPage` → 外壳的
 * `useState`），并把 Rust 的那句话一起带过去，而不是只弹一条通用提示。
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";

const events = vi.hoisted(() => {
  const listeners = new Map<number, () => void>();
  let next = 1;
  return {
    get open() {
      return listeners.size;
    },
    async listen() {
      const id = next++;
      listeners.set(id, () => undefined);
      return async () => {
        listeners.delete(id);
      };
    },
    reset() {
      listeners.clear();
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: events.listen }));

import App from "../App";
import { domainState } from "../state/domainState";

const EPOCH = "epoch-a";
const AT = 1_700_000_000_000;

/** 空闲快照（形状与 `TimerSnapshot::idle` 一致）。 */
const IDLE = {
  data_epoch: EPOCH,
  revision: 5,
  run_id: "run-1",
  session_id: null,
  session_version: null,
  task_id: null,
  task_row_version: null,
  task_title: null,
  tick_seq: 1,
  as_of: AT,
  active_ms: 0,
  pending_ms: null,
  state: null,
  timer_kind: null,
  remaining_ms: null,
  overtime_ms: null,
};

/** 一条正在计时的会话快照；任务三件由调用方逐条覆盖（默认：有标题）。 */
function activeSnapshot(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    ...IDLE,
    revision: 6,
    session_id: "session-1",
    session_version: 1,
    task_id: "task-1",
    task_row_version: 1,
    task_title: "写周报",
    state: "running",
    timer_kind: "stopwatch",
    ...overrides,
  };
}

/**
 * 外壳用到的命令的最小假后端（形状与 `src/types/ipc.ts` 一致）。
 *
 * `session` 为 `null`（默认）时 `timer_snapshot` 回空闲快照；给一份就回它。
 * `options.tasks` 给收件箱列表的夹具，`options.fail` 按命令名造一条失败响应
 * （M8 用例用 `start_timer` 的 `RECOVERY_REQUIRED`）。
 */
function scriptShell(
  session: Record<string, unknown> | null = null,
  options: {
    tasks?: Record<string, unknown>[];
    fail?: Record<string, Record<string, unknown>>;
  } = {},
): string[] {
  const called: string[] = [];
  mockIPC((command: string) => {
    called.push(command);
    const refused = options.fail?.[command];
    if (refused !== undefined) throw refused;
    switch (command) {
      case "get_revision":
        return { data_epoch: EPOCH, revision: 5 };
      case "timer_snapshot":
        return session ?? IDLE;
      case "finish_timer":
        return { snapshot: IDLE, revision: 9, task_version: 2 };
      case "list_tasks":
        return {
          tasks: options.tasks ?? [],
          total: options.tasks?.length ?? 0,
          data_epoch: EPOCH,
          revision: 5,
        };
      case "list_selectable_projects":
        return { items: [], data_epoch: EPOCH, revision: 5 };
      case "list_projects":
        return { items: [], data_epoch: EPOCH, revision: 5 };
      case "list_tags":
        return { items: [], data_epoch: EPOCH, revision: 5 };
      case "stats_today":
        // 今日页的五项（本用例只钉「切过去会发这条读命令」，数字不是它的事）。
        return {
          tasks: [],
          current: null,
          confirmed: [],
          live: [],
          pending: [],
          date: "2026-10-03",
          timezone: "Asia/Shanghai",
          range: { from: AT, to: AT + 86_400_000 },
          as_of: AT,
          data_epoch: EPOCH,
          revision: 5,
        };
      case "attention_overview":
        // 恢复页的概览（同样只钉"切过去会发这条读命令"）。
        return {
          items: [],
          pending_intervals: 0,
          pending_sessions: 0,
          fault_sessions: 0,
          data_epoch: EPOCH,
          revision: 5,
        };
      case "history_view":
        // 历史页的一页（同上：形状对就行）。
        return { sessions: [], selected: null, data_epoch: EPOCH, revision: 5 };
      default:
        throw new Error(`外壳不该调用 ${command}`);
    }
  });
  return called;
}

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
});

describe("应用外壳", () => {
  it("挂载：渲染标题与三个区域、导航七项、收件箱页，且命令集合恰好是那四条", async () => {
    const called = scriptShell();

    const { container } = render(<App />);

    expect(container.querySelector(".app-title")?.textContent).toBe("Worktrace");
    for (const region of ["nav", "page", "status"]) {
      expect(
        container.querySelector(`[data-region="${region}"]`),
        `缺少区域占位 data-region="${region}"`,
      ).not.toBeNull();
    }
    const nav = container.querySelector('[data-region="nav"]');
    expect(nav?.textContent).toContain("收件箱");
    expect(nav?.textContent).toContain("计时");
    expect(nav?.textContent).toContain("恢复");
    expect(nav?.textContent).toContain("历史");

    await waitFor(() => expect(called).toContain("list_tasks"));
    // 不去重（M4）：这条断言同时钉住「恰好四条」与「每条恰好一次」。
    // **默认页仍是收件箱**：多一条就说明默认页被改了（或页面在挂载时偷偷发了别的查询）。
    expect([...called].sort()).toEqual([
      "get_revision",
      "list_selectable_projects",
      "list_tasks",
      "timer_snapshot",
    ]);
    // 状态栏读的是镜像的握手状态（不是自己再发一次握手）
    await waitFor(() =>
      expect(container.querySelector('[data-region="status"]')?.textContent).toContain("已连接"),
    );
    expect(container.querySelector('[data-region="page"]')?.textContent).toContain("收件箱");
  });

  it("导航里七项都在；「项目」「任务」「今日」「恢复」「历史」各自发自己的读查询", async () => {
    // 反向验证：把 `PageView` 的某个分支接错页（或忘了把新页加进 `PAGES`）⇒
    // 导航文案或挂载区里那句断言红。
    const called = scriptShell();
    const { container } = render(<App />);
    await waitFor(() => expect(called).toContain("list_tasks"));

    const nav = container.querySelector('[data-region="nav"]');
    for (const label of ["收件箱", "项目", "任务", "计时", "今日", "恢复", "历史"]) {
      expect(nav?.textContent, `导航里缺少「${label}」`).toContain(label);
    }

    fireEvent.click(screen.getByRole("menuitem", { name: "项目" }));
    await waitFor(() => expect(called).toContain("list_projects"));
    expect(container.querySelector('[data-region="page"]')?.textContent).toContain("项目");

    fireEvent.click(screen.getByRole("menuitem", { name: "任务" }));
    await waitFor(() => expect(called).toContain("list_tags"));
    expect(container.querySelector('[data-region="page"]')?.textContent).toContain("任务");

    fireEvent.click(screen.getByRole("menuitem", { name: "今日" }));
    await waitFor(() => expect(called).toContain("stats_today"));
    expect(container.querySelector('[data-region="page"]')?.textContent).toContain("今日");

    fireEvent.click(screen.getByRole("menuitem", { name: "恢复" }));
    await waitFor(() => expect(called).toContain("attention_overview"));
    expect(screen.getByTestId("recovery-page")).not.toBeNull();

    fireEvent.click(screen.getByRole("menuitem", { name: "历史" }));
    await waitFor(() => expect(called).toContain("history_view"));
    expect(screen.getByTestId("history-page")).not.toBeNull();
  });

  it("M8：`RECOVERY_REQUIRED` 把用户导到恢复页，并把 Rust 那句话一起带过去（说明可关闭）", async () => {
    // 反向验证：把"按 code 无条件跳转"那一行删掉（只留分档）⇒ 下面 §孪生用例 红，本用例仍绿
    // （本夹具 `requires_handshake: false`）；把 `requestPage` 的第二参去掉 ⇒ 那句
    // `getByRole("alert")` 红；把外壳的 `closable`/`onClose` 去掉 ⇒ ④ 红。
    const called = scriptShell(null, {
      tasks: [
        {
          id: "task-1",
          project_id: null,
          title: "写周报",
          status: "Inbox",
          quality: null,
          row_version: 1,
          created_at: AT,
          updated_at: AT,
        },
      ],
      fail: {
        start_timer: {
          code: "RECOVERY_REQUIRED",
          message: "存在待确认的计时记录，请先处理恢复再继续。",
          authority: null,
          requires_handshake: false,
        },
      },
    });
    const { container } = render(<App />);
    await waitFor(() => expect(called).toContain("list_tasks"));

    // 在收件箱上点「开始」——被恢复门禁拒绝（这条命令今天正是 `RECOVERY_REQUIRED` 的来源）
    fireEvent.click(await screen.findByTestId("start-task-1"));

    // ① **切页**：恢复页挂载，并发它自己的读查询
    expect(await screen.findByTestId("recovery-page")).not.toBeNull();
    await waitFor(() => expect(called).toContain("attention_overview"));
    expect(container.querySelector('[data-region="page"]')?.textContent).toContain("恢复");
    // ② 那句说明跟着过来了（R8：用户文案只有 Rust 一个来源；发起命令的页面已经卸载）
    expect(screen.getByRole("alert").textContent).toBe("存在待确认的计时记录，请先处理恢复再继续。");
    // ③ 不是"只弹一条通用提示"：收件箱页已经不在了，屏上只剩恢复页 + 那一句原文
    expect(screen.queryByTestId("inbox-list")).toBeNull();
    // ④ 那句说明**有消失路径**（fix round 1 / Minor-4）：关掉它，恢复页仍在
    const close = container.querySelector(".error-notice .ant-alert-close-icon");
    expect(close, "跳转说明上没有关闭按钮").not.toBeNull();
    fireEvent.click(close!);
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(screen.getByTestId("recovery-page")).not.toBeNull();
  });

  it("M8（孪生）：`requires_handshake` 同时为真时**也**切页，且重新握手照旧执行", async () => {
    // 反向验证：把 `reportCommandError` 里"按 code 无条件 requestPage"那一行删掉 ⇒
    // 单值分档命中 `rehandshake` 就再也走不到 `recovery`，本用例红（原用例仍绿）。
    // 这条路径真实可达：`services/error_response.rs` 的 `requires_handshake =
    // authority.is_none() || DataEpochMismatch` ⇒ 任何 code 只要权威上下文读失败就带真。
    const called = scriptShell(null, {
      tasks: [
        {
          id: "task-1",
          project_id: null,
          title: "写周报",
          status: "Inbox",
          quality: null,
          row_version: 1,
          created_at: AT,
          updated_at: AT,
        },
      ],
      fail: {
        start_timer: {
          code: "RECOVERY_REQUIRED",
          message: "存在待确认的计时记录，请先处理恢复再继续。",
          authority: null,
          requires_handshake: true,
        },
      },
    });
    render(<App />);
    await waitFor(() => expect(called).toContain("list_tasks"));
    const greetings = called.filter((name) => name === "get_revision").length;

    fireEvent.click(await screen.findByTestId("start-task-1"));

    // ① 跳转不因为"同时要重新握手"而消失
    expect(await screen.findByTestId("recovery-page")).not.toBeNull();
    await waitFor(() => expect(called).toContain("attention_overview"));
    expect(screen.getByRole("alert").textContent).toBe("存在待确认的计时记录，请先处理恢复再继续。");
    // ② 重新握手那一档也照旧执行（两条动作互不吞并）
    await waitFor(() =>
      expect(called.filter((name) => name === "get_revision").length).toBeGreaterThan(greetings),
    );
  });

  it("导航切到计时页；卸载时把事件会话撤掉（不留监听）", async () => {
    scriptShell();
    const { container, unmount } = render(<App />);
    await waitFor(() => expect(events.open).toBe(1));

    fireEvent.click(screen.getByRole("menuitem", { name: "计时" }));

    expect(await screen.findByText(/当前没有正在计时的会话/)).not.toBeNull();
    expect(container.querySelector('[data-region="page"]')?.textContent).toContain("计时");

    unmount();
    await waitFor(() => expect(events.open).toBe(0));
  });

  it("「当前任务」只有快照一个来源：标题来自 `task_title`，计时页与状态栏同一份", async () => {
    scriptShell(activeSnapshot({ task_title: "写季报" }));
    const { container } = render(<App />);

    fireEvent.click(screen.getByRole("menuitem", { name: "计时" }));

    await waitFor(() => expect(screen.getByTestId("timer-task-title").textContent).toBe("写季报"));
    const status = container.querySelector('[data-region="status"]');
    await waitFor(() => expect(status?.textContent).toContain("写季报"));
    expect(status?.textContent).toContain("运行中");
  });

  it("M1：快照没给标题就不显示、也不占位——结束后那一拍同样不闪占位文案", async () => {
    const called = scriptShell(activeSnapshot({ task_title: null }));
    const { container } = render(<App />);
    fireEvent.click(screen.getByRole("menuitem", { name: "计时" }));
    const status = (): string =>
      container.querySelector('[data-region="status"]')?.textContent ?? "";

    // 会话在、标题缺：状态栏少一段、页面整段不渲染，都不编文案。
    await waitFor(() => expect(status()).toContain("运行中"));
    expect(status()).not.toContain("本窗口不知道");
    expect(screen.queryByTestId("timer-task-title")).toBeNull();

    // 结束：命令响应里已经没有会话了，而展示值仍由快照决定 ⇒ 这一拍**不会**掉进占位文案
    // （旧实现正是在这里清掉过渡身份、而快照还没换，于是闪一下「（本窗口不知道的任务）」）。
    fireEvent.click(screen.getByTestId("finish-button"));
    await waitFor(() => expect(called).toContain("finish_timer"));
    expect(status()).not.toContain("本窗口不知道");
    expect(screen.queryByTestId("timer-task-title")).toBeNull();
  });

  it("M1（补）：非空标题在「结束」的响应回来那一拍仍然显示", async () => {
    // 前一条用的是 `task_title: null` 的夹具，所以它只钉住「不出现占位」，钉不住
    // 「标题在这一拍会不会**消失**」。这里用非空标题补上另一半。
    //
    // 反向验证：把 `finish_timer` 的响应当成新状态用（例如成功后
    // `setTimer(response.snapshot)`，而响应里的空闲快照没有标题）⇒ 下面两句红。
    const called = scriptShell(activeSnapshot({ task_title: "写季报" }));
    const { container } = render(<App />);
    fireEvent.click(screen.getByRole("menuitem", { name: "计时" }));
    await waitFor(() => expect(screen.getByTestId("timer-task-title").textContent).toBe("写季报"));

    fireEvent.click(screen.getByTestId("finish-button"));
    await waitFor(() => expect(called).toContain("finish_timer"));
    // 等这条命令的响应真的回来（页面在这一拍清 busy；展示值**不**由响应决定）
    await act(async () => undefined);

    expect(screen.getByTestId("timer-task-title").textContent).toBe("写季报");
    expect(container.querySelector('[data-region="status"]')?.textContent).toContain("写季报");
  });
});
