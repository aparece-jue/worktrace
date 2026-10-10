/**
 * 时间编辑的**唯一**文本形状与解析（P8 Task 2c）：`YYYY-MM-DD HH:MM`（本地时区）。
 *
 * 为什么需要它：恢复页与历史页都要让用户**手写起止**（`reconcile` 的 `ranges`、
 * `correct` 的 `retime`、`backfill`），而契约里这些字段是 Unix 毫秒。前端因此必须把
 * 「用户看到的本地时间」与「线上要发的毫秒」来回换一次，并且只换这一次——两页各写
 * 一份的话，「什么算合法输入」就有了两个答案。
 *
 * 三条口径：
 *
 * 1. **精度是分钟**：V0.1 的时间编辑不要求秒，展示也截到分钟。候选端点只当输入框的
 *    提示（placeholder），**不预填**成默认值（恢复页的口径）——所以"截断"不会悄悄
 *    改掉任何事实；
 * 2. **解析严格**：形状逐字匹配，并且要**往返一致**（`2026-02-31`、夏令时跳过的那个
 *    本地时刻都会因为往返落不回原值被拒）。不猜、不"就近取值"；
 * 3. **非法输入返回 `null`**：提示哪句话由调用方决定。R8 管的是**命令失败**的文案
 *    （只有 Rust 一个来源），这里连命令都还没发出去。
 */

/** `YYYY-MM-DD HH:MM`（本地时区）。 */
export function formatLocalMinute(ms: number): string {
  const at = new Date(ms);
  const pad = (value: number): string => String(value).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(
    at.getHours(),
  )}:${pad(at.getMinutes())}`;
}

/** `2026-10-03 09:00`（也接受 `T` 分隔）。 */
const MINUTE_SHAPE = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})$/;

/**
 * 解析 `YYYY-MM-DD HH:MM`（本地时区）⇒ Unix 毫秒；非法输入（含**不存在**的本地时刻）
 * 交回 `null`。
 */
export function parseLocalMinute(text: string): number | null {
  const found = MINUTE_SHAPE.exec(text.trim());
  if (found === null) return null;
  const year = Number(found[1]);
  const month = Number(found[2]);
  const day = Number(found[3]);
  const hour = Number(found[4]);
  const minute = Number(found[5]);
  const at = new Date(year, month - 1, day, hour, minute, 0, 0);
  // 往返一致才认：`Date` 对越界字段会**滚动**（2 月 31 日 ⇒ 3 月 3 日），夏令时跳过的
  // 本地时刻也会落到别的钟点——两者都不是"用户给的那个时间"。
  if (
    at.getFullYear() !== year ||
    at.getMonth() !== month - 1 ||
    at.getDate() !== day ||
    at.getHours() !== hour ||
    at.getMinutes() !== minute
  ) {
    return null;
  }
  return at.getTime();
}
