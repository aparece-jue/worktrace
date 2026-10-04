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
 *    - 其余（含未知 `code`）⇒ 只提示。
 *
 * `RECOVERY_REQUIRED` 落在"只提示"：它的用户界面（恢复确认页）属 P3/P8，这里不替它做决定；
 * `STORAGE_ERROR` / `DOMAIN_ERROR` 同理——服务已经把面向用户的中文写在 `message` 里了。
 */

import { toIpcError, type IpcError } from "../ipc";
import { domainState } from "../state/domainState";

/** 一次失败要做的动作（文案不在这里，见模块头）。 */
export type CommandErrorAction = "notice" | "rehandshake" | "refresh";

/**
 * 这一次失败该做什么（纯函数，便于单独断言）。
 *
 * ⚠️ 判据里**没有**「文案」这一维：不管走哪条分支，上屏的都是 `error.message`。
 */
export function commandErrorAction(error: IpcError): CommandErrorAction {
  if (error.requires_handshake) return "rehandshake";
  if (error.code === "VERSION_CONFLICT") return "refresh";
  return "notice";
}

/**
 * 报告一次命令失败：执行该做的动作，并交回**要上屏的那句话**（恒为 `error.message`）。
 *
 * 刷新走的是镜像自己那条原语（`domainState.refresh()`：推一次缓存失效 + 取新快照），
 * 所以页面不需要各自传一个"重拉入口"进来——失效计数一变，页面自己的 `useEffect` 就重拉了。
 * 与闸门规则④共用同一份实现，不新增第二条分支。
 */
export function reportCommandError(cause: unknown): string {
  const error = toIpcError(cause);
  switch (commandErrorAction(error)) {
    case "rehandshake":
      void domainState.rehandshake();
      break;
    case "refresh":
      void domainState.refresh();
      break;
    case "notice":
      break;
  }
  return error.message;
}
