/**
 * 数据页用例（P8 Task 3b）：F-018 导出与 F-019 备份 / 恢复。
 *
 * 页面只做**展示与转发**——导出什么内容、周界怎么算、文件叫什么、落在哪、恢复怎么换库全在
 * Rust（`services/export.rs` / `services/backup.rs`）。这里逐条钉住的是：
 *
 * 1. **导出的请求逐字正确**：`format` / `timezone` / 范围都来自 `stats_today` 的**响应**
 *    （页面不自己算今天、也不算周界）；成功之后显示**真实路径**、给出大小，并调
 *    `revealItemInDir`（参数就是那条路径）；
 * 2. **导出失败上屏 Rust 的 message**，且屏幕上**不**留成功路径、也不打开位置；
 * 3. **能力不可用要明确**：不在 Tauri 运行时里 ⇒ "打开所在位置"**先就禁用** + 一句说明，
 *    点它也不会去打插件（不是"点了才报错"）；在 Tauri 里但 ACL 没登记 ⇒ 插件给的原因上屏
 *    允许重试（不静默）；
 * 4. **备份成功给路径与大小**，且**版本没变不是失败**（备份不改业务事实）；
 * 5. **恢复的二次确认是硬前置**：点「恢复」只打开确认框（零命令），确认后才带
 *    `confirmed: true` 与逐字的 `backup_path`；取消 ⇒ 零命令、零错误提示；
 * 6. **恢复成功立即采用新 `data_epoch`**（不等待在途握手）并按新身份重读，
 *    旧库的产物从屏上消失；
 * 7. **恢复失败上屏 Rust 的 message**，不走"成功 ⇒ 重新握手"那条路（不伪装成功）；
 * 8. **维护态**：`DATA_RESTORE_IN_PROGRESS` 的文案上屏 + 三个入口按 `code` 禁用；
 * 9. **查询与写失败是两条通道**：查询失败只提示、不重拉；写失败走 `reportCommandError`
 *    （`VERSION_CONFLICT` ⇒ 冲突刷新 ⇒ 本页重拉一次）。
 *
 * 插件替身的选择：**不 `vi.mock` `@tauri-apps/plugin-opener`**，而是让 `fakeBackend` 的
 * `mockIPC` 处理 `plugin:opener|reveal_item_in_dir`（它直接发 `{ paths }`，不走 `{ request }`
 * 信封）并把路径记进 `backend.revealed`。这样断的是**真实转发链**（页面 → 插件 → IPC 载荷）
 * 而不是"某个替身函数被调用过"；`isTauri` 用 `vi.stubGlobal` 控制（它读的就是 `globalThis`）。
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
    reset() {
      listeners.clear();
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: events.listen }));

import { Data } from "../Data";
import { domainState } from "../../state/domainState";
import {
  BACKUP_PATH,
  EPOCH,
  EXPORT_PATH,
  RESTORE_EPOCH,
  TODAY_ZONE,
  createBackend,
  failure,
  measureGroup,
  todayView,
  type Backend,
} from "./fakeBackend";
import { installJsdomBridges } from "./jsdomBridges";

let backend: Backend;

/** 装上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。**页面没有 props**。 */
async function mountData(): Promise<RenderResult> {
  const view = render(<Data />);
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

/** 本机时区（页面把它作为 `stats_today` 的原始输入；归一在服务端）。 */
function localTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone;
}

/** 本地时间的 `YYYY-MM-DD HH:MM`——用例侧**独立**算一遍页面上的区间文本。 */
function localStamp(ms: number): string {
  const at = new Date(ms);
  const pad = (value: number): string => String(value).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(
    at.getHours(),
  )}:${pad(at.getMinutes())}`;
}

/** 让 `isTauri()` 说"在 Tauri 运行时里"（必须在 `render` 之前调，见文件头）。 */
function stubTauriRuntime(): void {
  vi.stubGlobal("isTauri", true);
}

/** 填好待恢复的备份路径并走完二次确认（返回时 `restore` 已经发出去）。 */
async function restoreWithConfirm(): Promise<void> {
  fireEvent.change(screen.getByTestId("data-restore-path"), { target: { value: BACKUP_PATH } });
  fireEvent.click(screen.getByTestId("data-restore-run"));
  fireEvent.click(await screen.findByText("确认恢复"));
}

beforeAll(installJsdomBridges);

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
  vi.unstubAllGlobals();
});

describe("数据页：导出（F-018）", () => {
  it("导出成功：请求按判别式只带该带的字段，给出真实路径并打开所在位置", async () => {
    // 反向验证：把请求里的 `from`/`to` 换成 JS 自己算的"今天"（或给 markdown 也塞上
    // `from`/`to`）⇒ 下面两条逐字段断言红；把成功后的 `reveal(...)` 删掉 ⇒
    // `backend.revealed` 断言红；把路径换成"产物已生成"这类文案 ⇒ 路径断言红。
    stubTauriRuntime();
    backend = createBackend();
    backend.view = todayView({ confirmed: measureGroup("confirmed", { human: 5_400_000 }) });
    await mountData();

    // 口径标注来自**响应**：区间文本就是响应那两个端点（页面不自己算日界）
    expect(screen.getByTestId("data-date").textContent).toBe(backend.view.date);
    expect(screen.getByTestId("data-timezone").textContent).toBe(TODAY_ZONE);
    expect(screen.getByTestId("data-range").textContent).toBe(
      `[${localStamp(backend.view.range.from)}, ${localStamp(backend.view.range.to)})`,
    );
    expect(screen.getByTestId("data-confirmed-human").textContent).toBe("已确认人工 1:30:00");
    expect(lastRequest("stats_today")).toEqual({
      timezone: localTimezone(),
      expected_data_epoch: EPOCH,
    });

    // ① JSON 范围明细：范围与时区都取响应
    fireEvent.click(screen.getByTestId("data-export-run"));
    await waitFor(() => expect(backend.count("export_data")).toBe(1));
    expect(lastRequest("export_data")).toEqual({
      format: "json",
      timezone: TODAY_ZONE,
      expected_data_epoch: EPOCH,
      from: backend.view.range.from,
      to: backend.view.range.to,
    });
    // 真实路径 + 大小上屏（路径就是命令交回的那一条，一个字都不改写）
    expect(screen.getByTestId("data-export-path").textContent).toBe(EXPORT_PATH);
    expect(screen.getByTestId("data-export-bytes").textContent).toBe("大小 2048 字节");
    // "可打开的位置"：导出成功即调一次，参数就是那条路径
    await waitFor(() => expect(backend.revealed).toEqual([EXPORT_PATH]));

    // ② Markdown 周回顾：**不**带范围（周界由服务按这一周算），也不带 anchor（= 本周）
    fireEvent.click(within(screen.getByTestId("data-format")).getAllByRole("radio")[1]);
    fireEvent.click(screen.getByTestId("data-export-run"));
    await waitFor(() => expect(backend.count("export_data")).toBe(2));
    expect(lastRequest("export_data")).toEqual({
      format: "markdown",
      timezone: TODAY_ZONE,
      expected_data_epoch: EPOCH,
    });
    expect(backend.revealed).toEqual([EXPORT_PATH, EXPORT_PATH]);
  });

  it("导出失败：上屏 Rust 的 message，不显示成功路径、也不打开位置", async () => {
    // 反向验证：把 `catch` 里换成自编文案 ⇒ 那句逐字断言红；把失败前的
    // `setExported(null)` 删掉（或失败后仍调 `reveal`）⇒ 路径/`revealed` 断言红。
    stubTauriRuntime();
    backend = createBackend();
    const message = "存储暂时不可用，请稍后重试。";
    backend.fail.export_data = failure({ code: "STORAGE_ERROR", message });
    await mountData();

    fireEvent.click(screen.getByTestId("data-export-run"));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toBe(message));
    expect(screen.queryByTestId("data-export-path")).toBeNull();
    expect(backend.revealed).toEqual([]);
  });

  it("能力不可用：先就禁用「打开所在位置」并给出说明，点击也不打插件", async () => {
    // 反向验证：把 `canReveal` 去掉（自动那一次照调、按钮照给）⇒
    // `revealed`/命令名那两句与禁用断言一起红——这条用例钉的正是"不是点了才报错"。
    backend = createBackend();
    await mountData(); // 默认 jsdom：`globalThis.isTauri` 不是真值

    fireEvent.click(screen.getByTestId("data-export-run"));
    await waitFor(() => expect(backend.count("export_data")).toBe(1));

    // 产物本身照常给出（导出不依赖这条能力）
    expect(screen.getByTestId("data-export-path").textContent).toBe(EXPORT_PATH);
    // 说明先就在，按钮先就是禁用的
    expect(screen.getByTestId("data-reveal-note").textContent).toContain(
      "没有「打开所在位置」的能力",
    );
    const reveal = screen.getByTestId("data-export-reveal") as HTMLButtonElement;
    expect(reveal.disabled).toBe(true);
    // 自动那一次没打插件，手动点也不打
    expect(backend.revealed).toEqual([]);
    expect(backend.commands).not.toContain("plugin:opener|reveal_item_in_dir");
    fireEvent.click(reveal);
    expect(backend.commands).not.toContain("plugin:opener|reveal_item_in_dir");
  });

  it("打开位置失败显示原因，允许修复后重试", async () => {
    // 反向验证：把 `catch` 里的 `setRevealError` 删掉 ⇒ 静默失败，那句 message 断言红。
    stubTauriRuntime();
    backend = createBackend();
    const denied =
      "opener.reveal_item_in_dir not allowed. Permissions associated with this command: opener:allow-reveal-item-in-dir";
    backend.fail["plugin:opener|reveal_item_in_dir"] = denied;
    await mountData();

    fireEvent.click(screen.getByTestId("data-export-run"));
    await waitFor(() => expect(backend.count("plugin:opener|reveal_item_in_dir")).toBe(1));

    await waitFor(() => expect(screen.getByTestId("data-reveal-note").textContent).toBe(denied));
    expect((screen.getByTestId("data-export-reveal") as HTMLButtonElement).disabled).toBe(false);
    delete backend.fail["plugin:opener|reveal_item_in_dir"];
    fireEvent.click(screen.getByTestId("data-export-reveal"));
    await waitFor(() => expect(backend.count("plugin:opener|reveal_item_in_dir")).toBe(2));
    await waitFor(() => expect(screen.queryByTestId("data-reveal-note")).toBeNull());
  });
});

describe("数据页：备份与恢复（F-019）", () => {
  it("备份成功：发 backup、显示路径与大小；版本没变不是失败", async () => {
    // 反向验证：把"版本没变"也当失败上屏（或成功后清掉产物）⇒ 那句 `queryByRole("alert")`
    // 或路径断言红。
    stubTauriRuntime();
    backend = createBackend();
    await mountData();
    const revisionBefore = backend.revision;

    fireEvent.click(screen.getByTestId("data-backup-run"));

    await waitFor(() => expect(backend.count("backup")).toBe(1));
    expect(lastRequest("backup")).toEqual({ expected_data_epoch: EPOCH });
    expect(screen.getByTestId("data-backup-path").textContent).toBe(BACKUP_PATH);
    expect(screen.getByTestId("data-backup-bytes").textContent).toBe("大小 40960 字节");
    await waitFor(() => expect(backend.revealed).toEqual([BACKUP_PATH]));
    // 备份不改业务事实：`revision` 原样（假后端也不推版本），页面不把这当成失败
    expect(backend.revision).toBe(revisionBefore);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("恢复：二次确认之前零命令，确认后 confirmed/backup_path 逐字；取消 ⇒ 零命令零报错", async () => {
    // 反向验证：把 Popconfirm 拿掉（点一下直接发）⇒ 第一次的 `restore` 计数红；把
    // `confirmed` 写死 false 或路径改成 trim 前的原文 ⇒ 逐字段断言红。
    stubTauriRuntime();
    backend = createBackend();
    await mountData();

    // 没有路径就不给入口（V0.1 的"手填/复制路径"口径：路径是用户给的）
    expect((screen.getByTestId("data-restore-run") as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(screen.getByTestId("data-restore-path"), { target: { value: BACKUP_PATH } });
    fireEvent.click(screen.getByTestId("data-restore-run"));
    // ① 只是打开了确认框：恢复命令一条都不发
    expect(backend.count("restore")).toBe(0);
    expect(await screen.findByText(/用这份备份覆盖当前数据？/)).not.toBeNull();

    // ② 取消 ⇒ 零命令、零错误提示（"取消 ≠ 失败"）
    // antd 的 Button 会在**恰好两个汉字**之间插一个空格（"取 消"），所以按正则找。
    fireEvent.click(await screen.findByRole("button", { name: /取\s*消/ }));
    expect(backend.count("restore")).toBe(0);
    expect(screen.queryByRole("alert")).toBeNull();

    // ③ 确认 ⇒ 带上逐字的路径与二次确认位
    await restoreWithConfirm();
    await waitFor(() => expect(backend.count("restore")).toBe(1));
    expect(lastRequest("restore")).toEqual({
      backup_path: BACKUP_PATH,
      expected_data_epoch: EPOCH,
      confirmed: true,
    });
  });

  it("恢复成功：立即采用新 data_epoch、旧库的产物不残留", async () => {
    // 反向验证：把 `domainState.rehandshake()` 删掉 ⇒ 下面 `get_revision` 计数与新身份
    // 那两句红；把 epoch 变化的清理 effect 删掉 ⇒ 旧路径残留那句红。
    stubTauriRuntime();
    backend = createBackend();
    await mountData();

    // 先造一份"旧库的展示"：一次成功的导出
    fireEvent.click(screen.getByTestId("data-export-run"));
    await waitFor(() => expect(screen.getByTestId("data-export-path")).not.toBeNull());
    const greetings = backend.count("get_revision");

    await restoreWithConfirm();
    await waitFor(() => expect(backend.count("restore")).toBe(1));

    // ① 立即采用恢复回执身份，不再发一次 get_revision
    await waitFor(() => expect(domainState.getView().dataEpoch).toBe(RESTORE_EPOCH));
    expect(backend.count("get_revision")).toBe(greetings);
    // ② 页面按新身份重读（不是拿旧展示继续显示）
    await waitFor(() =>
      expect(lastRequest("stats_today")).toEqual({
        timezone: localTimezone(),
        expected_data_epoch: RESTORE_EPOCH,
      }),
    );
    // ③ 旧库的产物从屏上消失；成功那句说明在
    expect(screen.queryByTestId("data-export-path")).toBeNull();
    expect(screen.getByTestId("data-info").textContent).toContain("恢复完成");
    // ④ 状态回到"这一页可以再用"：忙碌态解除、入口可用
    expect((screen.getByTestId("data-backup-run") as HTMLButtonElement).disabled).toBe(false);
  });

  it("恢复失败：上屏 Rust 的 message，不走成功那条路（不伪装成功）", async () => {
    // 反向验证：把失败也当成功（同样 `rehandshake()` 并写"恢复完成"）⇒
    // `get_revision` 计数与 `data-info` 那两句红。
    stubTauriRuntime();
    backend = createBackend();
    const message = "恢复失败，已回滚到恢复前的数据（原库未变）。";
    backend.fail.restore = failure({ code: "STORAGE_ERROR", message });
    await mountData();
    const greetings = backend.count("get_revision");

    await restoreWithConfirm();
    await waitFor(() => expect(backend.count("restore")).toBe(1));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toBe(message));
    // 不伪装成功：没有成功说明，也没有"成功后重新握手"那一步
    expect(screen.queryByTestId("data-info")).toBeNull();
    expect(backend.count("get_revision")).toBe(greetings);
    // 身份没变 ⇒ 旧展示照旧在屏上（失败不该把页面清成"换了库"的样子）
    expect(screen.getByTestId("data-scope")).not.toBeNull();
    expect((screen.getByTestId("data-backup-run") as HTMLButtonElement).disabled).toBe(false);
  });

  it("维护态：DATA_RESTORE_IN_PROGRESS 上屏「正在恢复」，并把三个入口禁掉", async () => {
    // 反向验证：不在页面里按 `code` 记维护态（只弹提示）⇒ 三条禁用断言红；
    // 自己编一句"维护中"文案 ⇒ 那句逐字断言红。
    backend = createBackend();
    backend.fail.backup = failure({
      code: "DATA_RESTORE_IN_PROGRESS",
      message: "正在恢复数据，请稍候重试。",
    });
    await mountData();
    // 先把恢复入口填成可点（否则它的禁用是"没路径"而不是"维护态"）
    fireEvent.change(screen.getByTestId("data-restore-path"), { target: { value: BACKUP_PATH } });
    expect((screen.getByTestId("data-restore-run") as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(screen.getByTestId("data-backup-run"));

    expect((await screen.findByRole("alert")).textContent).toBe("正在恢复数据，请稍候重试。");
    expect(screen.getByTestId("data-maintenance").textContent).toContain("正在恢复");
    for (const id of ["data-export-run", "data-backup-run", "data-restore-run"]) {
      expect((screen.getByTestId(id) as HTMLButtonElement).disabled, id).toBe(true);
    }
  });
});

describe("数据页：查询与写失败是两条通道", () => {
  it("查询失败只提示、不重拉：上屏的是 Rust 的 message", async () => {
    // 反向验证：把查询失败那支也改成 `reportCommandError` ⇒ 它内部会 `rehandshake()`，
    // 下面 `get_revision` 计数红（查询路径自触发重拉/重新握手）。
    // ⚠️ 夹具刻意带 `requires_handshake: true`：不带它的话两条通道的**可观测行为**一样
    // （`STORAGE_ERROR` 在那一边也落在"只提示"），这条用例就断不出通道分开了。
    // 这个形状是真实可达的：`services/error_response.rs` 的判据是
    // `authority.is_none() || matches!(error, DataEpochMismatch)`，异常/恢复前后正是高发期。
    backend = createBackend();
    backend.fail.stats_today = failure({
      code: "STORAGE_ERROR",
      message: "存储暂时不可用，请稍后重试。",
      requires_handshake: true,
    });
    render(<Data />);
    await act(async () => {
      await domainState.start();
    });

    expect((await screen.findByRole("alert")).textContent).toBe("存储暂时不可用，请稍后重试。");
    expect(backend.count("stats_today")).toBe(1);
    expect(backend.count("get_revision")).toBe(1);
  });

  it("写失败走 reportCommandError：VERSION_CONFLICT 触发冲突刷新（本页重拉一次）", async () => {
    // 反向验证：把写失败改成只 `setError(toIpcError(cause).message)` ⇒ 分档不再执行，
    // 失效计数不动 ⇒ 下面"重拉一次"与 `timer_snapshot` 两句红。
    stubTauriRuntime();
    backend = createBackend();
    await mountData();
    const reads = backend.count("stats_today");

    backend.fail.backup = failure({
      code: "VERSION_CONFLICT",
      message: "这条记录已被修改，请刷新后重试。",
    });
    fireEvent.click(screen.getByTestId("data-backup-run"));

    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toBe("这条记录已被修改，请刷新后重试。"),
    );
    // `reportCommandError` 的 `refresh` 那一档：推一次失效（本页据此重拉）+ 取新计时快照
    await waitFor(() => expect(backend.count("stats_today")).toBeGreaterThan(reads));
    expect(backend.commands).toContain("timer_snapshot");
    expect(screen.queryByTestId("data-backup-path")).toBeNull();
  });
});
