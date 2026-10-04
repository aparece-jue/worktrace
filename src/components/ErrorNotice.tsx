/**
 * 失败提示（R8）：文案**就是** Rust 的 `ErrorResponse.message`，前端一字不改写。
 *
 * 为什么是行内 `Alert` 而不是 antd 的静态 `message.error(...)`：
 *
 * - 静态方法要在 React 树之外自己起一份渲染（antd 5 起推荐 `App.useApp()`），
 *   而这一层不该为了弹一句提示去改组合根；
 * - 行内提示是**可见、可断言**的 DOM（`role="alert"`），用例能直接核对"上屏的那句话
 *   与 Rust 给的一字不差"——这正是 R8 要钉的东西。
 */

import { Alert } from "antd";

/** `message` 为 `null` 时什么都不渲染（没有失败就没有提示条）。 */
export function ErrorNotice({ message }: { message: string | null }) {
  if (message === null) return null;
  return (
    <Alert
      className="error-notice"
      // antd 6：`message` 已废弃，改用 `title`。
      title={message}
      type="error"
      showIcon
    />
  );
}
