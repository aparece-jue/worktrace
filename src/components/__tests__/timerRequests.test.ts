/**
 * 「继续」的请求构造（P7 Task 3 接线的落点）：**取值全部来自快照**。
 *
 * 这一条与页面用例（`src/pages/__tests__/Timer.test.tsx`）分工不同：页面用例钉的是
 * "请求真的发出去了、字段就是快照里那几个"，这里钉的是**判据本身**——`state === "paused"`
 * 且 `task_id` / `task_row_version` / `task_title` 三件齐备才构造得出请求，缺任何一件都
 * 返回 `null`（调用方据此让按钮不出现，而不是"点了再失败"）。
 *
 * 为什么值得单独一条：这是**冷启动**（重开窗口、托盘暂停之后的 F-009 正常路径）唯一的
 * 请求构造路径——本窗口没有那次 `start_timer` 的上下文，除快照之外没有任何来源。
 */

import { describe, expect, it } from "vitest";

import { buildResumeRequest, sessionRequestOf } from "../timerRequests";
import type { TimerSnapshot } from "../../types/ipc";

/**
 * 一份**自洽**的「已暂停」快照。
 *
 * 字段刻意取与假后端默认值不同的数：用例里比对的是**字面量**，所以"把请求换成硬编码常量"
 * （或把某个字段换成快照以外的来源）会当场红。
 */
function pausedSnapshot(overrides: Partial<TimerSnapshot> = {}): TimerSnapshot {
  return {
    data_epoch: "epoch-7",
    revision: 11,
    run_id: "run-7",
    session_id: "session-42",
    session_version: 6,
    task_id: "task-42",
    task_row_version: 9,
    task_title: "写季报",
    tick_seq: 3,
    as_of: 1_700_000_000_000,
    active_ms: 5_000,
    pending_ms: null,
    state: "paused",
    timer_kind: "stopwatch",
    remaining_ms: null,
    overtime_ms: null,
    ...overrides,
  };
}

describe("继续：请求取值全部来自快照", () => {
  it("冷启动（只有这一份快照）：五个字段逐个等于快照值", () => {
    const snapshot = pausedSnapshot();

    expect(buildResumeRequest(snapshot)).toEqual({
      expected_data_epoch: "epoch-7",
      task_id: "task-42",
      task_expected_version: 9,
      session_id: "session-42",
      session_expected_version: 6,
    });
  });

  it("快照不自洽就没有请求：状态 / 任务三件 / 会话身份，缺任何一件都返回 null", () => {
    const cases: Array<[string, TimerSnapshot | null]> = [
      ["快照还没到（null）", null],
      ["会话在跑，不是暂停", pausedSnapshot({ state: "running" })],
      ["会话已结束", pausedSnapshot({ state: "finished" })],
      ["待恢复：归属还没对账", pausedSnapshot({ state: "recovering" })],
      ["没有会话", pausedSnapshot({ session_id: null, session_version: null })],
      ["没有会话版本", pausedSnapshot({ session_version: null })],
      ["没有任务 id", pausedSnapshot({ task_id: null })],
      ["没有任务版本", pausedSnapshot({ task_row_version: null })],
      ["没有任务标题（界面说不清在继续哪条任务）", pausedSnapshot({ task_title: null })],
    ];

    for (const [label, snapshot] of cases) {
      expect(buildResumeRequest(snapshot), label).toBeNull();
    }
  });

  it("暂停/结束的请求不受任务字段影响（那两条只要会话身份）", () => {
    const snapshot = pausedSnapshot({ task_id: null, task_row_version: null, task_title: null });
    expect(sessionRequestOf(snapshot)).toEqual({
      expected_data_epoch: "epoch-7",
      session_id: "session-42",
      session_expected_version: 6,
    });
  });
});
