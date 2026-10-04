/**
 * 计时页（P7 Task 3）：开始/暂停/继续/结束四个动作，以及由 P2 的 DTO 驱动的展示。
 *
 * ## 四个动作各对应**一条**命令，不做本地状态机
 *
 * `pause_timer` / `resume_timer` / `finish_timer` 的入参是**当前快照里那一条会话**
 * 的 id 与版本——取值只有快照一个来源（`src/components/timerRequests.ts`），点击之后也
 * **不改本地状态**：按钮集合完全由快照的 `state` 决定，新状态要等 Rust 提交后推来的那一拍
 * `timer.tick`（或它触发的重取）。「开始」在收件箱页（那里才有任务行与它的版本）。
 *
 * ## 展示值全部来自 DTO
 *
 * `active_ms` / `pending_ms` / `remaining_ms` / `overtime_ms` / `state` 原样展示，
 * 前端**不读时钟、不做减法**：暂停值冻结是 P2 的事（暂停区间不并入 `active_ms`）。
 * 「到点只提示、不自动完成」在这里就是一句 `Alert`——不发 `finish_timer`，
 * 也不把任务改成完成（那条联动属 P8）。
 *
 * ## 「继续」与标题都只有快照一个来源
 *
 * `resume_timer` 要 `task_id` + `task_expected_version`，`task_id` / `task_row_version` /
 * `task_title` 三件都由快照下发（`build()` 每次采样重读任务行），所以冷启动（重开窗口、
 * 托盘暂停之后的 F-009 正常路径）与本窗口刚点完「开始」是**同一条路径**；判据写在
 * [`buildResumeRequest`] 一处——**快照不自洽就不给这个入口**，不是"点了再失败"。
 * 标题同理：`task_title` 为空时**不渲染**（也不编占位文案），因为那不是任何权威值。
 */

import { useState } from "react";
import { Alert, Button, Empty, Flex, Space, Tag, Typography } from "antd";

import { finishTimer, pauseTimer, resumeTimer } from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { formatDuration } from "../components/duration";
import { buildResumeRequest, sessionRequestOf } from "../components/timerRequests";
import { useDataEpoch, useTimerSnapshot } from "../state/hooks";
import type { CommandOutcome, SessionState } from "../types/ipc";

/** 会话状态的中文（**展示用**，不是错误码表：它只是 `SessionState` 的界面措辞）。 */
export const SESSION_STATE_TEXT: Record<SessionState, string> = {
  running: "运行中",
  paused: "已暂停",
  recovering: "待恢复",
  finished: "已结束",
  discarded: "已丢弃",
};

export function Timer() {
  const epoch = useDataEpoch();
  const snapshot = useTimerSnapshot();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const state = snapshot?.state ?? null;
  const running = state === "running";
  const paused = state === "paused";
  /** 到点：只有倒计时会到点（正计时的 `remaining_ms`/`overtime_ms` 恒为 `null`）。 */
  const overdue = snapshot?.timer_kind === "countdown" && (snapshot.overtime_ms ?? 0) > 0;
  /** 这一拍能不能发「继续」：快照自洽（已暂停 + 任务三件齐备）。不可得 ⇒ 按钮不出现。 */
  const canResume = buildResumeRequest(snapshot) !== null;

  async function run(command: () => Promise<CommandOutcome>): Promise<void> {
    setBusy(true);
    try {
      // 响应**不进展示**：按钮集合与标题都仍由快照决定（不做本地状态机）。
      await command();
      setError(null);
    } catch (cause) {
      setError(reportCommandError(cause));
    } finally {
      setBusy(false);
    }
  }

  async function pause(): Promise<void> {
    const request = sessionRequestOf(snapshot);
    if (request === null) return;
    await run(() => pauseTimer(request));
  }

  async function resume(): Promise<void> {
    const request = buildResumeRequest(snapshot);
    if (request === null) return;
    await run(() => resumeTimer(request));
  }

  async function finish(): Promise<void> {
    const request = sessionRequestOf(snapshot);
    if (request === null) return;
    await run(() => finishTimer(request));
  }

  /** 还没有活动会话（`session_id` 为空）时只显示这一句。 */
  const idle = snapshot !== null && snapshot.session_id === null;

  return (
    <Flex vertical gap={16}>
      <Typography.Title level={5} style={{ margin: 0 }}>
        计时
      </Typography.Title>

      <ErrorNotice message={error} />

      {epoch === null ? (
        <Typography.Text type="secondary">正在连接…</Typography.Text>
      ) : snapshot === null ? (
        <Typography.Text type="secondary">正在读取计时状态…</Typography.Text>
      ) : idle ? (
        <Empty description="当前没有正在计时的会话：到「收件箱」里点「开始」。" />
      ) : (
        <Flex vertical gap={12}>
          <Space size={8} wrap>
            {/* 标题只有快照一个来源：没给就不渲染，也不编占位文案。 */}
            {snapshot.task_title !== null ? (
              <Typography.Text strong data-testid="timer-task-title">
                {snapshot.task_title}
              </Typography.Text>
            ) : null}
            {state !== null ? <Tag>{SESSION_STATE_TEXT[state]}</Tag> : null}
            <Typography.Text type="secondary">
              已计时 <span className="timer-active">{formatDuration(snapshot.active_ms)}</span>
            </Typography.Text>
            {snapshot.pending_ms !== null ? (
              <Typography.Text type="warning">
                待确认 <span className="timer-pending">{formatDuration(snapshot.pending_ms)}</span>
              </Typography.Text>
            ) : null}
            {snapshot.remaining_ms !== null ? (
              <Typography.Text type="secondary">
                剩余 <span className="timer-remaining">{formatDuration(snapshot.remaining_ms)}</span>
              </Typography.Text>
            ) : null}
            {snapshot.overtime_ms !== null && snapshot.overtime_ms > 0 ? (
              <Typography.Text type="danger">
                超时 <span className="timer-overtime">{formatDuration(snapshot.overtime_ms)}</span>
              </Typography.Text>
            ) : null}
          </Space>

          {overdue ? (
            // 到点**只提示**：不自动结束会话、也不自动完成任务（那条联动属 P8）。
            <Alert
              className="timer-overdue"
              type="warning"
              showIcon
              title="已到目标时长（不会自动结束，请自行结束或继续计时）。"
            />
          ) : null}

          <Space>
            {running ? (
              <Button
                type="primary"
                data-testid="pause-button"
                disabled={busy}
                onClick={() => void pause()}
              >
                暂停
              </Button>
            ) : null}
            {canResume ? (
              <Button
                type="primary"
                data-testid="resume-button"
                disabled={busy}
                onClick={() => void resume()}
              >
                继续
              </Button>
            ) : null}
            {running || paused ? (
              <Button danger data-testid="finish-button" disabled={busy} onClick={() => void finish()}>
                结束
              </Button>
            ) : null}
          </Space>
        </Flex>
      )}
    </Flex>
  );
}
