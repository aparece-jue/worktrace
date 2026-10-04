/**
 * `src/ipc.ts` 的用例：命令转发的形状、错误兜底、迟到响应丢弃、事件订阅顺序。
 *
 * 只断言**展示与转发**（总纲 §5 第 8 条）：这里没有任何业务判断可测，
 * 业务断言留在 Rust 侧。
 *
 * `invoke` 走官方的 `@tauri-apps/api/mocks`；`listen` 走本文件里的替身——
 * 官方的 `mockIPC(..., { shouldMockEvents: true })` 会在自己的实现里消化掉
 * `plugin:event|*`，因而看不到"订阅了几次、退订了几次、按什么顺序交付"，
 * 而这三件事正是这个文件要验的。
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";

const events = vi.hoisted(() => {
  const listeners = new Map<number, (event: { payload: unknown }) => void>();
  const channels: string[] = [];
  let next = 1;
  return {
    channels,
    get open() {
      return listeners.size;
    },
    async listen(channel: string, handler: (event: { payload: unknown }) => void) {
      channels.push(channel);
      const id = next++;
      listeners.set(id, handler);
      return async () => {
        listeners.delete(id);
      };
    },
    emit(payload: unknown) {
      for (const handler of [...listeners.values()]) handler({ payload });
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({ listen: events.listen }));

import {
  EVENT_CHANNEL,
  IpcError,
  TRANSPORT_ERROR,
  createFreshnessGate,
  getRevision,
  listProjects,
  sendVersioned,
  startEventSession,
  timerSnapshot,
  timerTick,
  toIpcError,
} from "../ipc";
import type { EventEnvelope } from "../types/ipc";

/** 一次 `invoke` 的实参快照。 */
interface Call {
  command: string;
  args: unknown;
}

function recordCalls(handler?: (command: string) => unknown): Call[] {
  const calls: Call[] = [];
  mockIPC((command, args) => {
    calls.push({ command, args });
    return handler === undefined ? undefined : handler(command);
  });
  return calls;
}

const EPOCH = "0f8fad5b-d9cb-469f-a165-70867728950e";

/** 等一次必然失败的调用，并把它规范化后的错误交回来（不成功的调用是这条用例的失败）。 */
async function rejection(promise: Promise<unknown>): Promise<IpcError> {
  try {
    await promise;
  } catch (cause) {
    return toIpcError(cause);
  }
  throw new Error("这次调用本该失败，却成功了");
}

afterEach(() => {
  clearMocks();
  events.channels.length = 0;
});

describe("命令转发", () => {
  it("带 request 的命令发 { request: … }，字段名原样 snake_case", async () => {
    const calls = recordCalls();
    await listProjects({ expected_data_epoch: EPOCH, status: "archived" });
    expect(calls).toEqual([
      {
        command: "list_projects",
        args: { request: { expected_data_epoch: EPOCH, status: "archived" } },
      },
    ]);
  });

  it("没有入参的三条命令不发 request 键", async () => {
    const calls = recordCalls();
    await getRevision();
    await timerSnapshot();
    await timerTick();
    expect(calls.map((call) => call.command)).toEqual([
      "get_revision",
      "timer_snapshot",
      "timer_tick",
    ]);
    for (const call of calls) {
      expect(call.args).toEqual({});
    }
  });
});

describe("错误规范化", () => {
  it("命令体的 ErrorResponse 原样透传（五个码与 authority 都不动）", async () => {
    const response = {
      code: "VERSION_CONFLICT",
      message: "这条记录已被修改，请刷新后重试。",
      authority: {
        data_epoch: EPOCH,
        revision: 13,
        records: [{ kind: "task", id: "t1", row_version: 4 }],
      },
      requires_handshake: false,
    };
    mockIPC(() => {
      throw response;
    });

    const error = await rejection(listProjects({ expected_data_epoch: EPOCH }));
    expect(error).toBeInstanceOf(IpcError);
    expect(error.code).toBe("VERSION_CONFLICT");
    expect(error.message).toBe("这条记录已被修改，请刷新后重试。");
    expect(error.requires_handshake).toBe(false);
    expect(error.authority?.records).toEqual([{ kind: "task", id: "t1", row_version: 4 }]);
  });

  it("非法 project 形状那种失败拿不到 ErrorResponse，兜底成可判定对象且不吞 message", async () => {
    // Tauri 的参数反序列化失败是**字符串**，不是 ErrorResponse 形状。
    const raw = "invalid args `request` for command `list_tasks`: invalid value: map";
    mockIPC(() => {
      throw raw;
    });

    const error = await rejection(listProjects({ expected_data_epoch: EPOCH }));
    expect(error).toBeInstanceOf(IpcError);
    expect(error.code).toBe(TRANSPORT_ERROR);
    expect(error.message).toBe(raw);
    expect(error.authority).toBeNull();
    expect(error.requires_handshake).toBe(false);
  });

  it("已经是 IpcError 的不再包一层", () => {
    const original = new IpcError({
      code: "DOMAIN_ERROR",
      message: "操作不被允许：标题不能为空。",
      authority: null,
      requires_handshake: false,
    });
    expect(toIpcError(original)).toBe(original);
  });
});

describe("迟到响应丢弃", () => {
  it("规则③：旧于已应用水位的查询响应被丢弃，同版本不算旧", () => {
    const gate = createFreshnessGate();
    expect(gate.applied()).toBeNull();
    gate.markApplied({ data_epoch: EPOCH, revision: 5 });

    expect(gate.isStaleResponse({ data_epoch: EPOCH, revision: 4 }, EPOCH)).toBe(true);
    expect(gate.isStaleResponse({ data_epoch: EPOCH, revision: 5 }, EPOCH)).toBe(false);
    expect(gate.isStaleResponse({ data_epoch: EPOCH, revision: 6 }, EPOCH)).toBe(false);
    // 请求发出时的 epoch 与响应回来的不同 ⇒ 这次响应回答的不是那个世界
    expect(gate.isStaleResponse({ data_epoch: "other", revision: 6 }, EPOCH)).toBe(true);
    // 不带 epoch 的命令（get_revision）只按水位判
    expect(gate.isStaleResponse({ data_epoch: "other", revision: 1 }, null)).toBe(false);
  });

  it("规则①：未知 epoch 单独认出来（它要重新握手，不能走规则②'静默丢弃'那条路）", () => {
    const gate = createFreshnessGate();
    // 还没应用过任何快照：无从判断"未知"，启动顺序由 startEventSession 保证。
    expect(gate.isUnknownEpoch({ data_epoch: EPOCH, revision: 1 })).toBe(false);

    gate.markApplied({ data_epoch: EPOCH, revision: 5 });
    expect(gate.isUnknownEpoch({ data_epoch: EPOCH, revision: 6 })).toBe(false);
    expect(gate.isUnknownEpoch({ data_epoch: "other", revision: 1 })).toBe(true);
    // 规则②对未知 epoch 恒为 false ⇒ 调用方**必须**先问规则①，
    // 否则按字面实现会把"另一个库的数据"当新数据接纳。
    expect(gate.isStaleNotification({ data_epoch: "other", revision: 1 })).toBe(false);
  });

  it("规则②：同 epoch 且 revision <= 已应用水位的通知被丢弃", () => {
    const gate = createFreshnessGate();
    gate.markApplied({ data_epoch: EPOCH, revision: 5 });

    expect(gate.isStaleNotification({ data_epoch: EPOCH, revision: 5 })).toBe(true);
    expect(gate.isStaleNotification({ data_epoch: EPOCH, revision: 6 })).toBe(false);
    // 未知 epoch 的通知不在这里丢：它只触发重新握手（规则①）
    expect(gate.isStaleNotification({ data_epoch: "other", revision: 1 })).toBe(false);
  });

  it("水位只前进：更旧的标记不会把它拉回去", () => {
    const gate = createFreshnessGate();
    gate.markApplied({ data_epoch: EPOCH, revision: 5 });
    gate.markApplied({ data_epoch: EPOCH, revision: 3 });
    expect(gate.applied()).toEqual({ data_epoch: EPOCH, revision: 5 });
  });

  it("sendVersioned：迟到的响应交回 null，且**不**改水位", async () => {
    const gate = createFreshnessGate();
    gate.markApplied({ data_epoch: EPOCH, revision: 5 });

    const late = await sendVersioned(
      async () => ({ data_epoch: EPOCH, revision: 4 }),
      gate,
      EPOCH,
    );
    expect(late).toBeNull();
    expect(gate.applied()).toEqual({ data_epoch: EPOCH, revision: 5 });

    const fresh = await sendVersioned(
      async () => ({ data_epoch: EPOCH, revision: 6 }),
      gate,
      EPOCH,
    );
    expect(fresh).toEqual({ data_epoch: EPOCH, revision: 6 });
    // 收到 ≠ 用上：水位要调用方自己 markApplied
    expect(gate.applied()).toEqual({ data_epoch: EPOCH, revision: 5 });
  });
});

describe("事件订阅", () => {
  const envelope = (revision: number): EventEnvelope => ({
    data_epoch: EPOCH,
    event: "domain.changed",
    revision,
    at: 1_700_000_000_000,
    payload: null,
  });

  it("先订阅再拉快照：load 期间到达的通知按原顺序在快照之后交付", async () => {
    const seen: number[] = [];
    const order: string[] = [];

    const started = await startEventSession(
      (event) => {
        seen.push(event.revision);
        order.push(`event:${event.revision}`);
      },
      async () => {
        order.push("load");
        // load 期间（订阅已建立、快照还没有）到达两条通知：必须被暂存而不是丢掉
        events.emit(envelope(1));
        events.emit(envelope(2));
        return "snapshot";
      },
    );

    expect(started.value).toBe("snapshot");
    expect(events.channels).toEqual([EVENT_CHANNEL]);
    expect(order).toEqual(["load", "event:1", "event:2"]);
    expect(seen).toEqual([1, 2]);

    // 之后的通知直通
    events.emit(envelope(3));
    expect(seen).toEqual([1, 2, 3]);
    await started.stream.close();
  });

  it("close() 之后不再交付，且订阅真的被撤掉", async () => {
    const seen: number[] = [];
    const started = await startEventSession(
      (event) => seen.push(event.revision),
      async () => undefined,
    );
    expect(events.open).toBe(1);

    events.emit(envelope(1));
    await started.stream.close();
    expect(events.open).toBe(0);

    events.emit(envelope(2));
    expect(seen).toEqual([1]);
  });

  it("load 抛错时撤掉订阅并原样抛出，不留悬挂监听", async () => {
    const failure = new Error("握手失败");
    await expect(
      startEventSession(
        () => undefined,
        async () => {
          throw failure;
        },
      ),
    ).rejects.toBe(failure);
    expect(events.open).toBe(0);
  });
});
