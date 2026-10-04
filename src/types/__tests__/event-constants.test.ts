import { describe, expect, it } from "vitest";
import rustEntry from "../../../src-tauri/src/lib.rs?raw";
import rustEvents from "../../../src-tauri/src/services/events.rs?raw";
import { EVENT_CHANNEL } from "../../ipc";
import { EVENT_DOMAIN_CHANGED, EVENT_TIMER_TICK } from "../ipc";

// 源码绑定两侧真实常量；不用第三份期望值掩盖单侧改名。
function rustConstant(source: string, name: string): string {
  const match = source.match(new RegExp(`(?:pub\\s+)?const\\s+${name}\\s*:\\s*&str\\s*=\\s*"([^"]+)"`));
  if (!match) throw new Error(`找不到 Rust 常量 ${name}，请同步契约检查`);
  return match[1];
}

describe("跨语言事件常量", () => {
  it("监听与广播使用同一个频道", () => {
    expect(EVENT_CHANNEL).toBe(rustConstant(rustEntry, "EVENT_CHANNEL"));
  });
  it("缓存失效事件名一致", () => {
    expect(EVENT_DOMAIN_CHANGED).toBe(rustConstant(rustEvents, "EVENT_DOMAIN_CHANGED"));
  });
  it("计时展示事件名一致", () => {
    expect(EVENT_TIMER_TICK).toBe(rustConstant(rustEvents, "EVENT_TIMER_TICK"));
  });
});
