/**
 * 「切页」的最小触发口（P8 Task 2c；M8 的第二处落点）。
 *
 * 外壳是**固定布局 + 一次 `useState` 切页**（R-04），**不引路由**——离线取不到
 * `react-router`（见 `src/App.tsx` 的模块头）。所以「把用户导到恢复页」这件事不能靠
 * 一个链接组件，只能靠一个**能从页面外部触发**的入口：命令失败的降级路径
 * （`reportCommandError` 的 `RECOVERY_REQUIRED` 那一档）今天用它；随后要复用的同类
 * 跳转（托盘视图跳转等）也走同一个口，因此这里不写死任何页面自己的知识。
 *
 * 它**不是状态**：不持有"当前页"（那是外壳的 `useState`），只把一次跳转请求交给正在
 * 监听的外壳。没有外壳在听（页面没挂载）时请求被丢弃——一次点不动的跳转不值得为它
 * 留一个待办队列，也没有第二个消费者会来取。
 */

/**
 * 七个页面的键。
 *
 * 它是**唯一出处**（外壳的 `PageKey` 直接引用它）：导航项、`PageView` 的 `switch` 与
 * 跳转请求共用同一个联合类型，少一处字面量就少一处能漂移的地方。
 */
export type PageKey =
  | "inbox"
  | "projects"
  | "tasks"
  | "timer"
  | "today"
  | "recovery"
  | "history";

/**
 * 一次切页请求的处置者。
 *
 * `notice` 是可选的一句说明，**原样来自 Rust 的 `ErrorResponse.message`**（R8：用户文案
 * 只有那一个来源）。带上它是为了别把门禁的原因丢掉：跳转之后发起那一条命令的页面已经
 * 卸载，它自己的提示条也就没了，外壳据此把它显示在页面区上方。
 */
export type PageRequestHandler = (page: PageKey, notice?: string) => void;

const handlers = new Set<PageRequestHandler>();

/** 请求切到某页（可带一句说明）。同步分发：调用方在失败处理的 `catch` 里直接调它。 */
export function requestPage(page: PageKey, notice?: string): void {
  // 复制一份再遍历：处置者可能在回调里退订（React 的卸载路径）。
  for (const handler of [...handlers]) handler(page, notice);
}

/** 订阅切页请求；返回退订函数（外壳在 `useEffect` 里用）。 */
export function onPageRequest(handler: PageRequestHandler): () => void {
  handlers.add(handler);
  return () => {
    handlers.delete(handler);
  };
}
