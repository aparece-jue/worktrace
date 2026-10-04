/**
 * jsdom 缺的两个浏览器 API（P7 Task 5）。
 *
 * 页面用例要跑真 antd 的浮层组件，而它们会用到这两个 API：
 *
 * - `matchMedia`：antd 的 `Pagination` 用它判响应式尺寸（`responsive` 默认开）；
 * - `ResizeObserver`：`Select` 的下拉与 `Popconfirm` 浮层
 *   （`@rc-component/resize-observer`、`rc-virtual-list`）用它测尺寸。
 *
 * jsdom 两个都没实现，不补的话**打开下拉/浮层那一下**会抛 `ReferenceError`
 * （表现是"点了没反应"，很容易被误读成组件 bug）。
 *
 * 只补到"能用"为止：替身不模拟任何行为，也不进生产代码（真实浏览器里它们是原生的）。
 * 放在用例目录而不是 `vitest.config.ts` 的 `setupFiles`：只有需要浮层的页面用例才付这份代价。
 */
export function installJsdomBridges(): void {
  if (typeof window === "undefined") return;

  // 判据用 "不是函数"，不是 "属性不存在"：jsdom 把 `matchMedia` 声明在 window 上但值不是
  // 可调用对象，`"matchMedia" in window` 因此为真而调用照样抛 `is not a function`。
  if (typeof window.matchMedia !== "function") {
    Object.defineProperty(window, "matchMedia", {
      writable: true,
      value: (query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addListener: () => undefined,
        removeListener: () => undefined,
        addEventListener: () => undefined,
        removeEventListener: () => undefined,
        dispatchEvent: () => false,
      }),
    });
  }

  if (typeof window.ResizeObserver !== "function") {
    class ResizeObserverStub {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    }
    Object.defineProperty(window, "ResizeObserver", {
      writable: true,
      value: ResizeObserverStub,
    });
  }
}
