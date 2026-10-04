/**
 * 项目页用例（P7 Task 5）：F-004 的创建、改名、归档与项目详情。
 *
 * 页面只做**展示与转发**——重名规则、版本守卫、事务边界都在 Rust。这里逐条钉住的是：
 *
 * 1. **写完当场重拉**：创建/改名/归档之后列表立刻反映服务端事实（不赌 `domain.changed`
 *    到得比响应早）——「确认归档后更新列表」就是这一条；
 * 2. **改既有对象带项目版本**：改名/归档的 `expected_row_version` 取自列表里那一行；
 * 3. **归档保留历史**：归档后项目仍在列表里（已归档）、它的任务仍在详情里读得到；
 * 4. **R8**：`VERSION_CONFLICT` 只决定行为（冲突刷新 + 展示 Rust 的 `message`），
 *    前端没有第二份「码 → 文案」表；
 * 5. **项目详情的旧响应不得覆盖新选中项目的列表**。
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
    reset() {
      listeners.clear();
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: events.listen }));

import { Projects } from "../Projects";
import { domainState } from "../../state/domainState";
import type { TaskQueryResult } from "../../types/ipc";
import { EPOCH, createBackend, failure, project, task, type Backend } from "./fakeBackend";
import { installJsdomBridges } from "./jsdomBridges";

let backend: Backend;

/** 挂上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。**页面没有 props**。 */
async function mountProjects(): Promise<RenderResult> {
  const view = render(<Projects />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("list_projects")).toBeGreaterThan(0));
  return view;
}

/** 最近一条某命令的入参。 */
function lastRequest(command: string): unknown {
  const found = [...backend.requests].reverse().find((entry) => entry.command === command);
  return found?.request;
}

/** 在某一行里点一个按钮（项目行都是 `project-<id>`）。 */
function clickIn(testId: string, buttonTestId: string): void {
  fireEvent.click(within(screen.getByTestId(testId)).getByTestId(buttonTestId));
}

beforeAll(installJsdomBridges);

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
});
describe("项目页：创建", () => {
  it("无项目时是明确的空态；创建之后列表当场出现新行", async () => {
    // 反向验证：把 `create()` 成功后的 `loadProjects()` 删掉（且用例不投递事件）⇒
    // 新项目不会出现，`findByText("项目乙")` 超时红。
    backend = createBackend();
    await mountProjects();
    expect(await screen.findByText(/还没有项目/)).not.toBeNull();

    const input = screen.getByPlaceholderText(/输入项目名称/);
    fireEvent.change(input, { target: { value: "项目乙" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(await screen.findByText("项目乙")).not.toBeNull();
    expect(backend.requests.find((entry) => entry.command === "create_project")?.request).toEqual({
      expected_data_epoch: EPOCH,
      name: "项目乙",
    });
  });

  it("空名称在界面层拦住：一条命令都不发", async () => {
    // 反向验证：去掉 `name === ""` 那道前置判据 ⇒ `create_project` 会被发出去，
    // 下面 `toBe(0)` 与那句提示都会红（Rust 对同一输入同样拒绝，那条 message 才是权威）。
    backend = createBackend();
    await mountProjects();

    const input = screen.getByPlaceholderText(/输入项目名称/);
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect((await screen.findByRole("alert")).textContent).toContain("请输入项目名称");
    expect(backend.count("create_project")).toBe(0);
    expect(backend.commands).not.toContain("create_project");
  });
});

describe("项目页：改名与归档", () => {
  it("改名：请求带列表里那一行的项目版本；成功后列表更新", async () => {
    // 反向验证：把 `expected_row_version` 写成常量（或漏掉）⇒ 逐字段 `toEqual` 红。
    backend = createBackend();
    backend.projects = [project({ id: "p-1", name: "项目甲", row_version: 3 })];
    await mountProjects();

    clickIn("project-p-1", "rename-p-1");
    const input = screen.getByTestId("rename-input-p-1");
    fireEvent.change(input, { target: { value: "项目甲（改）" } });
    fireEvent.click(screen.getByTestId("rename-ok-p-1"));

    expect(await screen.findByText("项目甲（改）")).not.toBeNull();
    expect(backend.requests.find((entry) => entry.command === "rename_project")?.request).toEqual({
      expected_data_epoch: EPOCH,
      project_id: "p-1",
      expected_row_version: 3,
      name: "项目甲（改）",
    });
  });

  it("归档：确认之后才发命令、提交带 epoch 与项目版本，列表当场更新且历史仍可读", async () => {
    // 反向验证③（计划点名的三条之一）：把归档成功后的重拉去掉（`run(..., async () => undefined)`）
    // ⇒ 列表不会更新，下面 `list_projects` 计数与「已归档」两条断言红。
    backend = createBackend();
    backend.projects = [project({ id: "p-1", name: "项目甲", row_version: 2 })];
    backend.tasks = [task({ id: "t-1", title: "项目里的行动", project_id: "p-1", status: "Ready" })];
    await mountProjects();

    const row = await screen.findByTestId("project-p-1");
    expect(within(row).getByText("在办")).not.toBeNull();
    const before = backend.count("list_projects");

    // 先确认（Popconfirm），确认之前一条命令都不该发
    clickIn("project-p-1", "archive-p-1");
    expect(backend.count("archive_project")).toBe(0);
    fireEvent.click(await screen.findByText("确定归档"));

    await waitFor(() => expect(backend.count("archive_project")).toBe(1));
    expect(backend.requests.find((entry) => entry.command === "archive_project")?.request).toEqual({
      expected_data_epoch: EPOCH,
      project_id: "p-1",
      expected_row_version: 2,
    });
    await waitFor(() => expect(backend.count("list_projects")).toBe(before + 1));

    // 归档保留历史：项目还在列表里（已归档）、也不再给「归档」入口
    const archived = await screen.findByTestId("project-p-1");
    expect(within(archived).getByText("已归档")).not.toBeNull();
    expect(within(archived).queryByTestId("archive-p-1")).toBeNull();

    // 它的任务在项目详情里照旧读得到
    clickIn("project-p-1", "open-p-1");
    expect(await screen.findByTestId("detail-task-t-1")).not.toBeNull();
  });

  it("版本冲突：按 R8 只决定行为——冲突刷新 + 展示 Rust 的 message，不自动重发命令", async () => {
    // 反向验证：把 `commandErrorAction` 的 VERSION_CONFLICT 那一支删掉 ⇒ 失效计数不动，
    // 本页不会重拉，`count("list_projects")` 那条 waitFor 超时红。
    backend = createBackend();
    backend.projects = [project({ id: "p-1", name: "项目甲", row_version: 2 })];
    backend.fail.archive_project = failure({
      code: "VERSION_CONFLICT",
      message: "项目已被别处改动，请刷新后重试。",
    });
    await mountProjects();
    const before = backend.count("list_projects");

    clickIn("project-p-1", "archive-p-1");
    fireEvent.click(await screen.findByText("确定归档"));

    expect((await screen.findByRole("alert")).textContent).toBe("项目已被别处改动，请刷新后重试。");
    await waitFor(() => expect(backend.count("list_projects")).toBe(before + 1));
    // 冲突刷新不是重试：那条命令仍然只发过一次
    expect(backend.count("archive_project")).toBe(1);
  });
});

describe("项目页：项目详情", () => {
  it("新增第一条行动：一条 create_task，项目在请求里定死；列表当场出现", async () => {
    // 反向验证：把 `project_id` 漏掉（或改成先建 Inbox 任务再改归属）⇒ 逐字段断言红，
    // 后者还会多发一条命令。
    backend = createBackend();
    backend.projects = [project({ id: "p-1", name: "项目甲" })];
    await mountProjects();

    clickIn("project-p-1", "open-p-1");
    expect(await screen.findByText(/这个项目还没有任务/)).not.toBeNull();

    const input = screen.getByPlaceholderText(/新增一条行动/);
    fireEvent.change(input, { target: { value: "第一步：访谈" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(await screen.findByText("第一步：访谈")).not.toBeNull();
    expect(backend.requests.find((entry) => entry.command === "create_task")?.request).toEqual({
      expected_data_epoch: EPOCH,
      title: "第一步：访谈",
      project_id: "p-1",
    });
    // 详情列表用的是「不限状态」的空集合（归档/完成的历史也要看得见）
    expect(lastRequest("list_tasks")).toEqual({
      statuses: [],
      project: { id: "p-1" },
      limit: 100,
      offset: 0,
      expected_data_epoch: EPOCH,
    });
  });

  it("切换项目之后，上一个项目的任务响应晚到不得覆盖新选中的列表", async () => {
    // 反向验证：把 `loadDetail()` 里那句「回答的是不是现在这个项目」去掉 ⇒ 迟到的
    // 甲项目响应会把乙项目的列表顶掉（显示回到加载态），第二句断言红。
    backend = createBackend();
    backend.projects = [
      project({ id: "p-1", name: "项目甲" }),
      project({ id: "p-2", name: "项目乙" }),
    ];
    backend.tasks = [task({ id: "t-1", title: "甲的任务", project_id: "p-1" })];
    const first = backend.holdNext<TaskQueryResult>("list_tasks");
    await mountProjects();

    clickIn("project-p-1", "open-p-1");
    await waitFor(() => expect(backend.count("list_tasks")).toBe(1));

    backend.tasks = [task({ id: "t-2", title: "乙的任务", project_id: "p-2" })];
    clickIn("project-p-2", "open-p-2");
    expect(await screen.findByText("乙的任务")).not.toBeNull();

    await act(async () => {
      first.resolve({
        tasks: [task({ id: "t-1", title: "甲的任务", project_id: "p-1" })],
        total: 1,
        data_epoch: EPOCH,
        revision: backend.revision,
      });
    });

    expect(screen.queryByText("甲的任务")).toBeNull();
    expect(screen.getByText("乙的任务")).not.toBeNull();
  });
});
