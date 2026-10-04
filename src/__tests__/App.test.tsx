/**
 * 外壳用例（P7 Task 1b 建立，Task 3 更新）：应用能挂载、渲染外壳与页面，
 * 并**经镜像的唯一入口**启动事件会话。
 *
 * Task 1b 那条「一个命令都不调用」的断言到这里必然要改：Task 3 把页面接进外壳之后，
 * 挂载就会由 `domainState.start()` 发 `get_revision` + `timer_snapshot`，收件箱页再发两条
 * 读查询。所以现在钉的是**命令集合恰好是这四条**——多一条（模板页留下的 `greet`、
 * 或页面自己偷偷开第二条订阅/第二条查询）都会红。
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

/** 四条命令的最小假后端（形状与 `src/types/ipc.ts` 一致）。 */
function scriptShell(): string[] {
  const called: string[] = [];
  mockIPC((command: string) => {
    called.push(command);
    switch (command) {
      case "get_revision":
        return { data_epoch: EPOCH, revision: 5 };
      case "timer_snapshot":
        return {
          data_epoch: EPOCH,
          revision: 5,
          run_id: "run-1",
          session_id: null,
          session_version: null,
          tick_seq: 1,
          as_of: AT,
          active_ms: 0,
          pending_ms: null,
          state: null,
          timer_kind: null,
          remaining_ms: null,
          overtime_ms: null,
        };
      case "list_tasks":
        return { tasks: [], total: 0, data_epoch: EPOCH, revision: 5 };
      case "list_selectable_projects":
        return { items: [], data_epoch: EPOCH, revision: 5 };
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
  it("挂载：渲染标题与三个区域、导航两项、收件箱页，且命令集合恰好是那四条", async () => {
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

    await waitFor(() => expect(called).toContain("list_tasks"));
    expect([...new Set(called)].sort()).toEqual([
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
});
