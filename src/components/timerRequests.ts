/**
 * 计时命令的请求构造：**取值全部来自 P2 的 DTO**（`TimerSnapshot`）。
 *
 * 页面不许自己拼这些字段（也不许自己算会话版本）：暂停/继续/结束三者的 `session_id` 与
 * `session_version` 只有快照这一个来源，`expected_data_epoch` 同理。收在这里还有一个好处：
 * "哪些字段来自哪里"只有一处可读。
 *
 * ## 「继续」的判据：**这一拍的快照必须自洽**
 *
 * `resume_timer` 要 `task_id` + `task_expected_version`（`services/timer/coordinator.rs`
 * 的 `resume` 用它做任务的版本守卫，并可能 `Ready → Doing`），而「这条会话属于哪个任务」
 * **只有快照一个来源**：既有命令里没有「会话 → 任务」的读路径（`list_tasks` 只按
 * status / project / context 筛、`TaskRow` 不带会话、托盘只做 `pause`）。契约因此把
 * `task_id` / `task_row_version` / `task_title` 一起下发（`build()` 每次采样重读任务行）。
 *
 * 于是这里**没有**、也**不再需要**任何"本窗口记下的身份"：冷启动（重开窗口、托盘暂停之后，
 * F-009 的正常路径）与刚点完「开始」是**同一条路径**——都只读快照。
 *
 * 判据落在 [`buildResumeRequest`] 一处：`state === "paused"` 且 `task_id` /
 * `task_row_version` / `task_title` 三件齐备，才构造得出请求；任何一件缺失都返回 `null`，
 * 调用方据此让按钮**不出现**（不是"点了再失败"）。标题不参与请求构造，但它与另两件同级：
 * 界面拿不到标题就说不清在继续哪条任务，正因如此契约才把它一起下发。
 */

import type { ResumeRequest, SessionRequest, TimerSnapshot } from "../types/ipc";

/** 暂停/结束共用的请求：两份取值都来自快照。快照里没有会话（或还没基线）时返回 `null`。 */
export function sessionRequestOf(snapshot: TimerSnapshot | null): SessionRequest | null {
  if (snapshot === null) return null;
  const sessionId = snapshot.session_id;
  const sessionVersion = snapshot.session_version;
  if (sessionId === null || sessionVersion === null) return null;
  return {
    expected_data_epoch: snapshot.data_epoch,
    session_id: sessionId,
    session_expected_version: sessionVersion,
  };
}

/**
 * 「继续」的请求。**快照不自洽就返回 `null`**（调用方据此让按钮不出现）。
 *
 * 四个条件缺一不可，理由各自独立：
 * - `state === "paused"`：`resume` 只接受已暂停的会话（`coordinator.rs` 的同名守卫），
 *   别的状态下这个入口没有意义；
 * - `task_id` / `task_row_version`：请求本身的字段，缺了构造不出来；
 * - `task_title`：**界面唯一的标题来源**，缺了就没法告诉用户在继续哪条任务。
 */
export function buildResumeRequest(snapshot: TimerSnapshot | null): ResumeRequest | null {
  if (snapshot === null) return null;
  const session = sessionRequestOf(snapshot);
  const taskId = snapshot.task_id;
  const taskVersion = snapshot.task_row_version;
  const taskTitle = snapshot.task_title;
  if (
    session === null ||
    snapshot.state !== "paused" ||
    taskId === null ||
    taskVersion === null ||
    taskTitle === null
  ) {
    return null;
  }
  return {
    expected_data_epoch: session.expected_data_epoch,
    task_id: taskId,
    task_expected_version: taskVersion,
    session_id: session.session_id,
    session_expected_version: session.session_expected_version,
  };
}
