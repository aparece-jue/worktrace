/**
 * 托盘视图跳转的**接收侧**（P8 Task 7；计划 §6.4-11）。
 *
 * Rust 侧的托盘菜单只有「当前任务」与「快速捕获」两个窗口动作，`lib.rs` 的
 * `on_tray_action` 把它们变成一条**窗口作用域的定向事件**
 * （`worktrace:tray-view`，`emit_to("main", …)`，载荷 `{ page, focus }`）之后照旧抬窗；
 * 本模块把那条事件翻成一次 {@link requestPage}——切页仍然是外壳那一套（`pageRequest` +
 * `App.tsx` 的订阅，M8 建立的唯一入口），这里**不新造第二套**，也不碰 DOM。
 *
 * ## 为什么不是 `worktrace:event` 那条广播
 *
 * P6 的"不新造第三个事件名"管的是 `services/events.rs` 那对**业务广播**
 * （`domain.changed` / `timer.tick`）：它们的词表是封闭的，而"用户点了托盘的哪一项"
 * 不是业务事实、没有 `revision`，混进广播只会让每个窗口每次点托盘都白重拉一轮数据。
 * 所以跳转走定向事件，只发给主窗（签名两处：Rust 的
 * `platform::tray::TRAY_VIEW_EVENT` 与下面的 {@link TRAY_VIEW_EVENT}，由
 * `src/types/__tests__/event-constants.test.ts` 读 Rust 源码核对）。
 *
 * ## 三个"不做"
 *
 * 1. **不抬窗**：抬窗是 Rust 的事（`window::raise_or_rebuild_main`），前端只切页。
 * 2. **不排队**：外壳还没挂载（或这一页根本没在听）时请求被 `requestPage` 丢弃——
 *    与 M8 同一条口径：一次点不动的跳转不值得留一个待办队列。
 * 3. **不猜**：认不出的载荷（页名不认识、载荷不是对象）**什么都不做**，只留一条
 *    `console.warn` 诊断。托盘的这条通道没有回执，失败时既不能 panic，也不能编一个
 *    画面出来（"别 panic、别造假画面"）。
 */

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { requestPage, type PageRequestIntent, type PageKey } from "./state/pageRequest";

/**
 * 托盘视图跳转的事件名（`src-tauri/src/platform/tray.rs` 的 `TRAY_VIEW_EVENT`）。
 *
 * 它不属于 `worktrace:event` 那条广播（见模块头），所以住在这一层。
 */
export const TRAY_VIEW_EVENT = "worktrace:tray-view";

/**
 * 托盘能要求跳到的页。
 *
 * **只有这两页**：Rust 的 `tray::tray_view_for` 只会给出这两个字符串。收窄在这里是有意的
 * ——载荷来自窗口事件（运行期数据，不是类型），认不出就不该切页。
 */
const TRAY_VIEW_PAGES: readonly PageKey[] = ["inbox", "timer"];

/** 一次托盘跳转载荷（Rust 的 `platform::tray::TrayView` 的序列化形状）。 */
interface TrayViewRequest {
  page: PageKey;
  focus: boolean;
}

/**
 * 把一个载荷翻成"切到某页 +（可能）聚焦捕获输入框"；认不出就返回 `null`。
 *
 * 纯函数，便于单独断言（`src/__tests__/trayViewRequests.test.ts`）。**不抛异常**：
 * 事件载荷是外部数据，一个坏载荷不能把外壳的订阅打掉（那之后所有跳转都失灵）。
 */
export function trayViewRequest(payload: unknown): {
  page: PageKey;
  intent: PageRequestIntent | undefined;
} | null {
  if (typeof payload !== "object" || payload === null) return null;
  const candidate = payload as Partial<TrayViewRequest>;
  if (typeof candidate.page !== "string") return null;
  if (!TRAY_VIEW_PAGES.includes(candidate.page as PageKey)) return null;
  return {
    page: candidate.page as PageKey,
    // 只有**明确**要求聚焦时才带这个意图：缺字段/给了别的值都当"不聚焦"，
    // 不去猜（猜错就是把光标从用户手里抢走）。
    intent: candidate.focus === true ? { focus: "capture" } : undefined,
  };
}

/**
 * 订阅托盘跳转；返回退订函数（与 `onPageRequest` 同形，外壳在 `useEffect` 里一并收尾）。
 *
 * ⚠️ 这是**这个 JS 上下文里的第二条 Tauri 订阅**（第一条是 `src/ipc.ts` 的
 * `worktrace:event`）：它订阅的是另一个事件名，不是"第二个状态入口"——
 * `state/hooks.ts` 那句"不再开第二个订阅入口"说的是**业务状态**只能有一个来源，
 * 而本模块不持有任何状态、不读镜像，只把一次跳转转交给 `requestPage`。
 */
export async function installTrayViewRequests(): Promise<UnlistenFn> {
  return listen<unknown>(TRAY_VIEW_EVENT, (event) => {
    const request = trayViewRequest(event.payload);
    if (request === null) {
      console.warn("[worktrace] 认不出的托盘跳转载荷，已忽略", event.payload);
      return;
    }
    requestPage(request.page, undefined, request.intent);
  });
}
