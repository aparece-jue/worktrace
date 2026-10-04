/**
 * 计时命令的请求构造：**取值全部来自 P2 的 DTO**（`TimerSnapshot`）。
 *
 * 页面不许自己拼这些字段（也不许自己算会话版本）：暂停/继续/结束三者的 `session_id` 与
 * `session_version` 只有快照这一个来源，`expected_data_epoch` 同理。收在这里还有一个好处：
 * "哪些字段来自哪里"只有一处可读。
 *
 * ## ⚠️ 待接线：`resume` 的任务身份（P7 的过渡实现）
 *
 * `resume_timer` 要 `task_id` + `task_expected_version`（`services/timer/coordinator.rs`
 * 的 `resume` 用它做任务的版本守卫，并可能 `Ready → Doing`），但**当前**的 `TimerSnapshot`
 * 里没有任务字段，24 条命令里也没有「会话 → 任务」的读路径（`list_tasks` 的筛选只有
 * status / project / context，`TaskRow` 不带会话），托盘也只做 `pause`。
 *
 * 所以本轮由**发起 `start_timer` 的那一方**（收件箱页）把任务身份经外壳交给计时页——
 * **这是过渡，不是长期机制**。契约侧正在给 `TimerSnapshot` 补 `task_id` /
 * `task_row_version`；字段一到，接线就是：
 *
 * 1. 删掉这里第二个入参（`taskIdentity`）与 `TaskIdentity` 这个类型；
 * 2. `buildResumeRequest(snapshot)` 的三行任务取值改成读 `snapshot.task_id` /
 *    `snapshot.task_row_version`；
 * 3. `src/pages/Timer.tsx` 的调用点与「身份不可得 ⇒ 按钮不出现」那条判断**语义不变**
 *    （那时它退化成"快照里没有任务 ⇒ 没有可继续的会话"），`src/App.tsx` 的过渡状态与
 *    收件箱页的 `onSessionStarted` 一并删掉。
 *
 * 在字段落地之前，「身份不可得 ⇒ 继续**不出现**」（不是"点了再失败"）：冷启动
 * （重开窗口、或托盘暂停之后）这条会话不是本窗口开始的，本窗口没有可用的任务版本。
 */

import type { ResumeRequest, SessionRequest, TimerSnapshot } from "../types/ipc";

/**
 * 过渡期：本上下文启动会话时记下的任务身份。
 *
 * ⚠️ **待替换**——契约补上 `TimerSnapshot.task_id` / `task_row_version` 之后这个类型就没有
 * 存在理由了（见模块头）。`title` 只用于展示（计时页与状态栏的"当前任务"）。
 */
export interface TaskIdentity {
  id: string;
  title: string;
  /** 任务行的并发版本（`row_version`）：`resume` 的 `task_expected_version`。 */
  row_version: number;
}

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
 * 「继续」的请求。**任务身份拿不到就返回 `null`**（调用方据此让按钮不出现）。
 *
 * ⚠️ 待接线：见模块头——`taskIdentity` 这个入参会被 `snapshot.task_id` /
 * `snapshot.task_row_version` 取代，**这里就是本轮唯一的落点**。
 */
export function buildResumeRequest(
  snapshot: TimerSnapshot | null,
  taskIdentity: TaskIdentity | null,
): ResumeRequest | null {
  const session = sessionRequestOf(snapshot);
  if (session === null || snapshot === null || taskIdentity === null) return null;
  return {
    expected_data_epoch: session.expected_data_epoch,
    task_id: taskIdentity.id,
    task_expected_version: taskIdentity.row_version,
    session_id: session.session_id,
    session_expected_version: session.session_expected_version,
  };
}
