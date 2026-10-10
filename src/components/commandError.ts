/**
 * 命令失败的统一处置（R8 裁决）。
 *
 * 两条硬规则：
 *
 * 1. **用户文案只有 Rust 一个来源**——`ErrorResponse.message`（`src-tauri/src/error.rs`，
 *    已全中文）。这里**没有**「码 → 文案」表，也不给未知 `code` 兜底成"未知错误"：
 *    `message` 原样上屏。前端唯一会收到的失败类型是 {@link IpcError}（Task 1b 规范化过，
 *    连 Tauri 自己的反序列化失败都被兜成 `TRANSPORT_ERROR` + 原始 message）。
 * 2. **`code` / `requires_handshake` 只决定行为**：
 *    - `requires_handshake` ⇒ 先重新握手（`domainState.rehandshake()`），**不自动重试**
 *      非幂等命令（重发 `start` 会开出第二个会话）；
 *    - `VERSION_CONFLICT` ⇒ 冲突刷新（手上的行/会话版本旧了：推一次缓存失效并重取）；
 *    - `RECOVERY_REQUIRED` ⇒ **切到恢复页**，并把 Rust 的那句话一起带过去（M8）：
 *      门禁不是"一条错"，是一条要先走完的动线——只弹一句提示等于把用户留在原地；
 *    - 其余（含未知 `code`）⇒ 只提示。
 *
 * ⚠️ **「导到恢复页」不参与分档**（它和别的档**可以同时为真**）：`requires_handshake` 不是
 * `DATA_EPOCH_MISMATCH` 的专利——`services/error_response.rs` 的判据是
 * `authority.is_none() || matches!(error, DataEpochMismatch)`，任何 code 只要**权威上下文
 * 读失败**就带 `requires_handshake: true`（异常/恢复前后正是高发期）。分档是**单值**的，
 * 把跳转挂在档位上，这条路径就会被 `rehandshake` 吞掉——那恰好是 M8 要防的"只弹一条提示"。
 * 所以跳转按 `code` **无条件**执行一次，分档只管它自己那一档。
 *
 * `STORAGE_ERROR` / `DOMAIN_ERROR` / `DATA_RESTORE_IN_PROGRESS` 落在"只提示"：服务已经把
 * 面向用户的中文写在 `message` 里了（维护态那一句就是"正在恢复数据…"，所以维护态不需要
 * 前端另编一档文案，只需要页面按 `code` 把写入入口禁掉）。
 */

import { toIpcError, type IpcError } from "../ipc";
import { domainState } from "../state/domainState";
import { requestPage } from "../state/pageRequest";

/** 一次失败要做的动作（文案不在这里，见模块头）。 */
export type CommandErrorAction = "notice" | "rehandshake" | "refresh" | "recovery";

/**
 * 这一次失败该做什么（纯函数，便于单独断言）。
 *
 * ⚠️ 判据里**没有**「文案」这一维：不管走哪条分支，上屏的都是 `error.message`。
 */
export function commandErrorAction(error: IpcError): CommandErrorAction {
  if (error.requires_handshake) return "rehandshake";
  if (error.code === "VERSION_CONFLICT") return "refresh";
  if (error.code === "RECOVERY_REQUIRED") return "recovery";
  return "notice";
}

/**
 * 报告一次命令失败：执行该做的动作，并交回**要上屏的那句话**（恒为 `error.message`）。
 *
 * 刷新走的是镜像自己那条原语（`domainState.refresh()`：推一次缓存失效 + 取新快照），
 * 所以页面不需要各自传一个"重拉入口"进来——失效计数一变，页面自己的 `useEffect` 就重拉了。
 * 与闸门规则④共用同一份实现，不新增第二条分支。
 *
 * 恢复那一档同理走**一个**共享入口（`requestPage`），页面不各自去够外壳的 `useState`：
 * 触发方只表达"该去恢复页了"，怎么切页是 `src/App.tsx` 的事。
 *
 * 跳转**在分档之前无条件做**（理由见模块头）：它带上的那句是 Rust 的原文——发起命令的
 * 页面马上就要卸载，它自己的提示条留不下来。
 */
export function reportCommandError(cause: unknown): string {
  const error = toIpcError(cause);
  // 与分档无关的那一步（`RECOVERY_REQUIRED` 可以与 `requires_handshake` 同时为真）。
  if (error.code === "RECOVERY_REQUIRED") requestPage("recovery", error.message);
  switch (commandErrorAction(error)) {
    case "rehandshake":
      void domainState.rehandshake();
      break;
    case "refresh":
      void domainState.refresh();
      break;
    case "recovery":
      // 上面已经跳过了。这一档仍然保留：它是**纯函数**的断言对象（"这个 code 的动作就是
      // 去恢复页"），也是分档与动作之间的唯一对照点。
      break;
    case "notice":
      break;
  }
  return error.message;
}
