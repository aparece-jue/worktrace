/**
 * 失败提示（R8）：文案**就是** Rust 的 `ErrorResponse.message`，前端一字不改写。
 *
 * 为什么是行内 `Alert` 而不是 antd 的静态 `message.error(...)`：
 *
 * - 静态方法要在 React 树之外自己起一份渲染（antd 5 起推荐 `App.useApp()`），
 *   而这一层不该为了弹一句提示去改组合根；
 * - 行内提示是**可见、可断言**的 DOM（`role="alert"`），用例能直接核对"上屏的那句话
 *   与 Rust 给的一字不差"——这正是 R8 要钉的东西。
 *
 * ## 可关闭是可选的（P8 Task 2c fix round 1）
 *
 * 页面里的失败提示**默认不可关**：它由下一次失败/成功替换，用户不需要手动清。
 * 但**外壳**那条"随跳转过来的一句说明"（`RECOVERY_REQUIRED` ⇒ 恢复页）是另一回事：
 * 它描述的是"你为什么被扔到这一页"，处理完之后只会一直挂着、没有别的消失路径
 * ⇒ 外壳传 `closable`，用户自己关掉它。
 */

import { Alert } from "antd";

/**
 * `message` 为 `null` 时什么都不渲染（没有失败就没有提示条）。
 *
 * `closable` / `onClose` 默认关：只有"需要用户自己收起"的那一处（外壳的跳转说明）会开它。
 */
export function ErrorNotice({
  message,
  closable = false,
  onClose,
}: {
  message: string | null;
  /** 显示关闭按钮。 */
  closable?: boolean;
  /** 关闭回调（外壳用它把 state 清掉）。 */
  onClose?: () => void;
}) {
  if (message === null) return null;
  return (
    <Alert
      className="error-notice"
      // antd 6：`message` 已废弃，改用 `title`。
      title={message}
      type="error"
      showIcon
      // antd 6 的非弃用形状：关闭回调进 `closable` 对象（顶层 `onClose` 已弃用）。
      closable={closable ? { onClose } : false}
    />
  );
}
