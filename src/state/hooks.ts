/**
 * 页面读状态镜像的 hooks（P7 Task 2，00 §6）。
 *
 * 全部走 `useSyncExternalStore`：**订阅的就是 `domainState` 那一个入口**，
 * 组件卸载时 React 调用订阅函数返回的退订函数——不额外挂任何监听，
 * 所以不会有卸载泄漏。这里**不再开第二个订阅入口**（比如自己 `listen` 事件）。
 *
 * 选择器的返回值必须是**原始值或引用稳定的对象**：`useSyncExternalStore` 每次渲染
 * 都会调用它并比较引用，现算一个新对象会让 React 认为一直在变。
 * `view` 只在内容真的变了才换对象，`view.timer` 也是原来那个快照对象。
 */

import { useSyncExternalStore } from "react";

import { domainState, type DomainView } from "./domainState";
import type { TimerSnapshot } from "../types/ipc";

// 单例上的方法不依赖 `this`（都是闭包），所以可以当值传出去；
// 引用稳定是 `useSyncExternalStore` 的要求（每次渲染换函数会反复重订阅）。
const subscribe = domainState.subscribe;
const getView = domainState.getView;
const selectTimer = (): TimerSnapshot | null => domainState.getView().timer;
const selectInvalidated = (): number => domainState.getView().invalidated;

/** 整个镜像快照（epoch / 水位 / 失效计数 / 计时展示值 / 握手状态）。 */
export function useDomainView(): DomainView {
  return useSyncExternalStore(subscribe, getView);
}

/** 计时展示值（`timer.tick` 或计时快照）；没有活动会话时为 `null`。 */
export function useTimerSnapshot(): TimerSnapshot | null {
  return useSyncExternalStore(subscribe, selectTimer);
}

/**
 * 缓存失效计数：每接纳一条 `domain.changed`（或整体失效）加一。
 *
 * 页面把它放进自己那次「重拉数据」的 `useEffect` 依赖里即可——事件只作失效，
 * 数据从命令拉（00 §6）。
 */
export function useInvalidation(): number {
  return useSyncExternalStore(subscribe, selectInvalidated);
}
