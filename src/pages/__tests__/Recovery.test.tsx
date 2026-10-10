/**
 * 恢复页用例（P8 Task 2c）：F-015 恢复确认。
 *
 * 页面只做**展示与转发**——什么算待确认、哪些状态能对账、重叠怎么判、作废写哪些行全在
 * Rust（`services/recovery.rs`），前端一个数字都不重算。这里逐条钉住的是：
 *
 * 1. **可信与待确认视觉可分**：可信那一段是范围说明、候选在另一块里，两块不是同一个元素；
 * 2. **候选只作候选**：候选端点/跨度只上屏、只当输入框的 placeholder，**不预填**成默认值；
 * 3. **确认的 `ranges` 与输入逐字一致**（按本地时间独立算一遍毫秒），且带**响应给的**
 *    会话版本；缺输入就地拦住、一条命令都不发；
 * 4. **重叠由服务判**：上屏的就是 Rust 那句具体冲突（前端不自己写第二份重叠规则）；
 * 5. **丢弃与作废是两个动作、两条命令**：`reconcile(discard_uncertain)` 与
 *    `discard_session` 各自数命令名；作废有**二次确认**（点开确认框不发命令）；
 * 6. **重试与时钟校正都由显式点击触发**：挂载时一条都不发；`accepted: false` 是正常路径；
 * 7. **维护态**：`DATA_RESTORE_IN_PROGRESS` 上屏"正在恢复"并把写入入口禁掉；
 * 8. **本视图水位**：迟到的旧响应不得覆盖已经上屏的那一份。
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

import { Recovery } from "../Recovery";
import { domainState } from "../../state/domainState";
import { EVENT_DOMAIN_CHANGED, type AttentionOverview } from "../../types/ipc";
import {
  AT,
  EPOCH,
  attentionItem,
  attentionOverview,
  createBackend,
  failure,
  pendingInterval,
  type Backend,
} from "./fakeBackend";
import { installJsdomBridges } from "./jsdomBridges";

let backend: Backend;

/** 一条 `recovering` 会话 + 两条候选：`P1` 候选终点已知、`P2` 终点未知。 */
const S1 = "session-1";
const P1 = "pending-1";
const P2 = "pending-2";

/** 装上页面并启动镜像（页面要等握手拿到 epoch 才会发业务查询）。**页面没有 props**。 */
async function mountRecovery(): Promise<RenderResult> {
  const view = render(<Recovery />);
  await act(async () => {
    await domainState.start();
  });
  await waitFor(() => expect(backend.count("attention_overview")).toBeGreaterThan(0));
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

/** 本地时间的 `YYYY-MM-DD HH:MM`——用例侧**独立算一遍**页面上的 placeholder。 */
function localStamp(ms: number): string {
  const at = new Date(ms);
  const pad = (value: number): string => String(value).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(
    at.getHours(),
  )}:${pad(at.getMinutes())}`;
}

/**
 * 用例侧**独立**把本地时间文本算成毫秒（不走生产代码的解析器）。
 *
 * 反向验证的支点：页面要是把用户输入当 UTC 解析、或者自己加减了时区，这里的期望值就对不上。
 */
function localMs(text: string): number {
  const [date, time] = text.split(" ");
  const [year, month, day] = date.split("-").map(Number);
  const [hour, minute] = time.split(":").map(Number);
  return new Date(year, month - 1, day, hour, minute, 0, 0).getTime();
}

/** 一份"一条 recovering 会话 + 两条候选"的概览；计数刻意**大于**列表条数（证明页面读字段）。 */
function withCandidates(overrides: Partial<AttentionOverview> = {}): AttentionOverview {
  return attentionOverview({
    items: [
      attentionItem({
        session_id: S1,
        task_id: "task-9",
        state: "recovering",
        attention: "needs_review",
        session_row_version: 7,
        intervals: [
          pendingInterval({ id: P1, started_at: AT, ended_at: AT + 120_000 }),
          pendingInterval({ id: P2, started_at: AT + 600_000, ended_at: null }),
        ],
      }),
    ],
    pending_intervals: 7,
    pending_sessions: 3,
    ...overrides,
  });
}

/** 逐条把四个输入框填满（`confirm` 必须覆盖全部待确认区间）。 */
function fillAll(start: string, end: string): void {
  fireEvent.change(screen.getByTestId(`recovery-start-${P1}`), { target: { value: start } });
  fireEvent.change(screen.getByTestId(`recovery-end-${P1}`), { target: { value: end } });
  fireEvent.change(screen.getByTestId(`recovery-start-${P2}`), { target: { value: start } });
  fireEvent.change(screen.getByTestId(`recovery-end-${P2}`), { target: { value: end } });
}

beforeAll(installJsdomBridges);

afterEach(async () => {
  cleanup();
  await domainState.stop();
  events.reset();
  clearMocks();
});

describe("恢复页：看清哪一段可信、哪一段待确认", () => {
  it("可信范围与待确认候选分成两块（不是同一个元素）；候选端点只作提示、不预填", async () => {
    // 反向验证：把可信那句范围说明与候选列表塞进同一个容器 ⇒ 前两句红；
    // 把候选值预填进输入框（`value={formatLocalMinute(...)}`）⇒ value 那两句红。
    backend = createBackend();
    backend.attention = withCandidates();
    await mountRecovery();

    const trusted = screen.getByTestId(`recovery-trusted-${S1}`);
    const pending = screen.getByTestId(`recovery-pending-${S1}`);
    expect(trusted.textContent).toContain("不在本次确认范围");
    expect(trusted.textContent).toContain("照旧计入统计");
    expect(trusted.contains(pending)).toBe(false);
    expect(pending.contains(trusted)).toBe(false);

    // 两条候选各自可辨：一条有候选端点、一条终点未知（"终点未知不推算"）
    expect(within(screen.getByTestId(`recovery-candidate-${P1}`)).getByText("待确认")).not.toBeNull();
    expect(screen.getByTestId(`recovery-candidate-span-${P1}`).textContent).toBe(
      "候选跨度 02:00（未确认）",
    );
    expect(screen.getByTestId(`recovery-candidate-span-${P2}`).textContent).toBe(
      "候选起点已知、终点未知（不推算时长）",
    );

    // 候选值只进 placeholder：输入框是空的（"已知单调时长只作候选，不得作为默认值强迫接受"）
    const start = screen.getByTestId(`recovery-start-${P1}`) as HTMLInputElement;
    const end = screen.getByTestId(`recovery-end-${P1}`) as HTMLInputElement;
    expect(start.value).toBe("");
    expect(end.value).toBe("");
    expect(start.placeholder).toBe(localStamp(AT));
    expect(end.placeholder).toBe(localStamp(AT + 120_000));
    expect((screen.getByTestId(`recovery-end-${P2}`) as HTMLInputElement).placeholder).toBe(
      "终点未知（请填）",
    );

    // 顶部计数来自**响应**（不是一个跟列表条数对得上的数字）：页面不自己数列表
    expect(screen.getByTestId("recovery-pending-intervals").textContent).toBe("待确认区间 7 条");
    expect(screen.getByTestId("recovery-pending-sessions").textContent).toBe("待确认会话 3 个");
    expect(screen.getByTestId("recovery-fault-sessions").textContent).toBe("记录损坏 0 个");
  });
});

describe("恢复页：确认、丢弃、作废是三条不同的路", () => {
  it("确认：ranges 与输入逐字一致（含响应给的会话版本），成功后重拉一次", async () => {
    // 反向验证：把 `ranges` 里的起止换成候选端点、或把 `expected_row_version` 换成列表里
    // 别的数字 ⇒ 下面那条逐字段断言红；把成功后的 `afterWrite()` 删掉 ⇒ 重拉计数红。
    backend = createBackend();
    backend.attention = withCandidates();
    await mountRecovery();

    fillAll("2026-10-03 09:00", "2026-10-03 09:30");
    fireEvent.click(screen.getByTestId(`recovery-confirm-${S1}`));

    await waitFor(() => expect(backend.count("reconcile")).toBe(1));
    expect(lastRequest("reconcile")).toEqual({
      expected_data_epoch: EPOCH,
      session_id: S1,
      expected_row_version: 7,
      action: "confirm",
      target_state: "finished",
      ranges: [
        {
          interval_id: P1,
          started_at: localMs("2026-10-03 09:00"),
          ended_at: localMs("2026-10-03 09:30"),
        },
        {
          interval_id: P2,
          started_at: localMs("2026-10-03 09:00"),
          ended_at: localMs("2026-10-03 09:30"),
        },
      ],
    });
    await waitFor(() => expect(backend.count("attention_overview")).toBe(2));
  });

  it("缺一条起止就地拦住；重叠由服务拒绝，上屏的是那句具体冲突", async () => {
    // 反向验证：把缺输入那一支删掉（直接发命令）⇒ `reconcile` 计数红；
    // 把失败文案换成"操作失败"这种笼统句（或自己在页面里判重叠）⇒ 那句逐字断言红。
    backend = createBackend();
    backend.attention = withCandidates();
    await mountRecovery();

    // 只填了一条候选的两个端点 ⇒ 整批不发（`confirm` 必须恰好覆盖全部待确认区间）
    fireEvent.change(screen.getByTestId(`recovery-start-${P1}`), {
      target: { value: "2026-10-03 09:00" },
    });
    fireEvent.change(screen.getByTestId(`recovery-end-${P1}`), {
      target: { value: "2026-10-03 09:30" },
    });
    fireEvent.click(screen.getByTestId(`recovery-confirm-${S1}`));
    expect((await screen.findByRole("alert")).textContent).toContain("YYYY-MM-DD HH:MM");
    expect(backend.count("reconcile")).toBe(0);

    // 填全之后由服务判重叠：页面只是把 Rust 那句原文上屏
    const conflict =
      "操作不被允许：这段区间与已有区间重叠（与 2026-10-03 08:00–09:10 冲突）。";
    backend.fail.reconcile = failure({ code: "DOMAIN_ERROR", message: conflict });
    fillAll("2026-10-03 09:00", "2026-10-03 09:30");
    fireEvent.click(screen.getByTestId(`recovery-confirm-${S1}`));

    await waitFor(() => expect(screen.getByRole("alert").textContent).toBe(conflict));
    // 写失败 ⇒ 不重拉（不把失败当成功）
    expect(backend.count("attention_overview")).toBe(1);
  });

  it("丢弃不确定区间：走 reconcile 且 ranges 为空，绝不发 discard_session", async () => {
    // 反向验证：把"丢弃"接到 `discard_session` 上（两个动作合并成一个按钮）⇒
    // 下面 `not.toContain("discard_session")` 与逐字段断言一起红。
    backend = createBackend();
    backend.attention = withCandidates();
    await mountRecovery();

    // 目标状态与确认共用一套控件：改成"暂停"之后应当原样进请求
    const target = screen.getByTestId(`recovery-target-${S1}`);
    fireEvent.click(within(target).getAllByRole("radio")[1]);
    fireEvent.click(screen.getByTestId(`recovery-discard-uncertain-${S1}`));

    await waitFor(() => expect(backend.count("reconcile")).toBe(1));
    expect(lastRequest("reconcile")).toEqual({
      expected_data_epoch: EPOCH,
      session_id: S1,
      expected_row_version: 7,
      action: "discard_uncertain",
      target_state: "paused",
      ranges: [],
    });
    expect(backend.commands).not.toContain("discard_session");
    // 影响范围写在按钮旁边（"只丢不确定区间、保留此前闭合工时"）
    expect(screen.getByTestId(`recovery-discard-uncertain-note-${S1}`).textContent).toContain(
      "此前已闭合的可信工时保留",
    );
  });

  it("作废整次：二次确认之前一条命令都不发，确认后走 discard_session", async () => {
    // 反向验证：把 Popconfirm 拿掉（点一下直接发）⇒ 第一次的 `discard_session` 计数红；
    // 把 `onConfirm` 接到 `reconcile` 上 ⇒ 第二条命令名断言红。
    backend = createBackend();
    backend.attention = withCandidates();
    await mountRecovery();

    fireEvent.click(screen.getByTestId(`recovery-discard-session-${S1}`));
    // 只是打开了确认框：写入命令一条都不该发
    expect(backend.count("discard_session")).toBe(0);
    expect(backend.count("reconcile")).toBe(0);
    // 确认框里写的是**影响范围**，不是笼统的"确定吗"
    expect(await screen.findByText(/全部计时区间都会被软作废/)).not.toBeNull();

    fireEvent.click(await screen.findByText("确认作废"));
    await waitFor(() => expect(backend.count("discard_session")).toBe(1));
    expect(lastRequest("discard_session")).toEqual({
      expected_data_epoch: EPOCH,
      session_id: S1,
      expected_row_version: 7,
    });
    expect(backend.count("reconcile")).toBe(0);
    await waitFor(() => expect(backend.count("attention_overview")).toBe(2));
  });
});

describe("恢复页：显式触发的两个动作", () => {
  it("重试恢复：挂载时一条都不发，点一下才发；文案按 P6-3 写", async () => {
    // 反向验证：把 `retry_recovery` 挪进挂载时的 `useEffect`（自动重试）⇒
    // 挂载后的 `not.toContain` 断言红。
    backend = createBackend();
    backend.attention = attentionOverview();
    await mountRecovery();

    expect(screen.getByTestId("recovery-retry-note").textContent).toContain(
      "计时不可用（故障态或提交后待刷新）",
    );
    expect(backend.commands).not.toContain("retry_recovery");

    fireEvent.click(screen.getByTestId("recovery-retry"));
    await waitFor(() => expect(backend.count("retry_recovery")).toBe(1));
    expect(lastRequest("retry_recovery")).toEqual({ expected_data_epoch: EPOCH });
    expect(screen.getByTestId("recovery-info").textContent).toBe("已重试恢复，计时状态已刷新。");
  });

  it("时钟校正：显式接受；accepted=false 是正常路径、不是错误", async () => {
    // 反向验证：把挂载时自动接受那一步加回去 ⇒ 第一条 `not.toContain` 红；
    // 把 `accepted: false` 当失败上屏（走 `ErrorNotice`）⇒ 那句 `queryByRole("alert")` 红。
    backend = createBackend();
    backend.attention = attentionOverview();
    await mountRecovery();

    expect(backend.commands).not.toContain("accept_detected_clock_correction");
    fireEvent.click(screen.getByTestId("recovery-accept-clock"));
    await waitFor(() => expect(backend.count("accept_detected_clock_correction")).toBe(1));
    expect(lastRequest("accept_detected_clock_correction")).toEqual({ expected_data_epoch: EPOCH });
    expect(screen.getByTestId("recovery-info").textContent).toContain("当前没有待接受的时钟校正");
    expect(screen.queryByRole("alert")).toBeNull();

    // 真的有校正待接受：`accepted: true`
    backend.clockAccepted = true;
    fireEvent.click(screen.getByTestId("recovery-accept-clock"));
    await waitFor(() =>
      expect(screen.getByTestId("recovery-info").textContent).toBe("已接受这次时钟校正。"),
    );
    expect(backend.count("accept_detected_clock_correction")).toBe(2);
  });
});

describe("恢复页：损坏档与维护态", () => {
  it("记录损坏只作诊断：给诊断原因、不给确认/丢弃入口，作废整次仍在", async () => {
    // 反向验证：把 `invariant_broken` 也当成可对账的一类（照常给"确认"按钮）⇒
    // 下面两句 `queryByTestId(...).toBeNull()` 红（服务端同样会拒，界面不该先给入口）。
    backend = createBackend();
    backend.attention = attentionOverview({
      items: [
        attentionItem({
          session_id: "session-broken",
          task_id: "task-broken",
          state: "running",
          attention: "invariant_broken",
          intervals: [],
          fault_reason: "running_with_pending_interval",
        }),
        attentionItem({
          session_id: "session-paused",
          task_id: "task-paused",
          state: "paused",
          attention: "none",
          intervals: [],
        }),
      ],
      fault_sessions: 1,
    });
    await mountRecovery();

    const broken = screen.getByTestId("recovery-fault-session-broken");
    expect(broken.textContent).toContain("只作诊断、不自动修复");
    expect(broken.textContent).toContain("running_with_pending_interval");
    expect(screen.queryByTestId("recovery-confirm-session-broken")).toBeNull();
    expect(screen.queryByTestId("recovery-discard-uncertain-session-broken")).toBeNull();
    expect(
      screen.getByTestId("recovery-not-reconcilable-session-broken").textContent,
    ).toContain("损坏的记录不能通过确认修复");
    // 作废整次是**用户**的显式动作（不是自动修复），损坏档也留着这个出口
    expect(screen.getByTestId("recovery-discard-session-session-broken")).not.toBeNull();

    // 非 recovering 的会话同理：确认/丢弃只对 recovering 开放
    expect(screen.queryByTestId("recovery-confirm-session-paused")).toBeNull();
    expect(screen.getByTestId("recovery-not-reconcilable-session-paused").textContent).toContain(
      "只对 recovering 会话开放",
    );
  });

  it("维护态：DATA_RESTORE_IN_PROGRESS 上屏「正在恢复」，并把写入入口禁掉", async () => {
    // 反向验证：只在页面里写一句自己的"维护中"文案（不展示 Rust 的 message）⇒
    // 那句逐字断言红；把 `disabled` 里的 `maintenance` 去掉 ⇒ 禁用断言红。
    backend = createBackend();
    backend.attention = withCandidates();
    backend.fail.reconcile = failure({
      code: "DATA_RESTORE_IN_PROGRESS",
      message: "正在恢复数据，请稍候重试。",
    });
    await mountRecovery();

    fireEvent.click(screen.getByTestId(`recovery-discard-uncertain-${S1}`));
    expect((await screen.findByRole("alert")).textContent).toBe("正在恢复数据，请稍候重试。");

    for (const testId of [
      `recovery-confirm-${S1}`,
      `recovery-discard-uncertain-${S1}`,
      `recovery-discard-session-${S1}`,
      "recovery-retry",
      "recovery-accept-clock",
    ]) {
      expect((screen.getByTestId(testId) as HTMLButtonElement).disabled, testId).toBe(true);
    }
  });
});

describe("恢复页：判旧与查询失败", () => {
  it("本视图水位：迟到的旧响应不得覆盖已经上屏的那一份概览", async () => {
    // 反向验证：删掉 `watermark.isStale(found, epoch)` 那一句 ⇒ 旧响应把"0 条"顶回
    // "7 条"，最后那句断言红。
    backend = createBackend();
    backend.attention = withCandidates();
    const slow = backend.holdNext<AttentionOverview>("attention_overview");
    await mountRecovery();

    backend.revision = 6;
    backend.attention = attentionOverview({ revision: 6 });
    await changed(6);
    await waitFor(() =>
      expect(screen.getByTestId("recovery-pending-intervals").textContent).toBe("待确认区间 0 条"),
    );

    // 第一次那条请求现在才回来（revision 5，旧数字）
    await act(async () => {
      slow.resolve(withCandidates({ revision: 5 }));
    });
    expect(screen.getByTestId("recovery-pending-intervals").textContent).toBe("待确认区间 0 条");
  });

  it("查询失败只提示、不重拉：上屏的是 Rust 的 message", async () => {
    // 反向验证：把查询失败那支也改成 `reportCommandError` ⇒ 它内部会 `refresh()`，
    // 下面"只读了一次"的计数断言红（查询路径自触发重拉）。
    backend = createBackend();
    backend.fail.attention_overview = failure({
      code: "STORAGE_ERROR",
      message: "存储暂时不可用，请稍后重试。",
    });
    render(<Recovery />);
    await act(async () => {
      await domainState.start();
    });

    expect((await screen.findByRole("alert")).textContent).toBe("存储暂时不可用，请稍后重试。");
    expect(backend.count("attention_overview")).toBe(1);
  });
});
