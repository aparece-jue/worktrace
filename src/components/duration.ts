/**
 * 毫秒 → `mm:ss` / `h:mm:ss`。
 *
 * **只做展示格式化**：数值一律来自 P2 的 DTO（`active_ms` / `pending_ms` /
 * `remaining_ms` / `overtime_ms`），这一层不读时钟、不做减法、不算工时。
 */

/** 负数按 0 处理（异常值不该在界面上变成 `-1:59`）。 */
export function formatDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1_000));
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  const mm = String(minutes).padStart(2, "0");
  const ss = String(seconds).padStart(2, "0");
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}
