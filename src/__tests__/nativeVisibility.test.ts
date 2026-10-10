import { afterEach, expect, it, vi } from "vitest";

const native = vi.hoisted(() => ({
  minimized: false,
  resized: () => {},
  off: vi.fn(),
  visible: vi.fn(),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isMinimized: async () => native.minimized,
    onResized: async (handler: () => void) => { native.resized = handler; return native.off; },
  }),
}));
vi.mock("../state/domainState", () => ({ domainState: { setWindowVisible: native.visible } }));
import { installNativeVisibility } from "../nativeVisibility";

afterEach(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  native.minimized = false;
  vi.clearAllMocks();
});

it("原生最小化与还原通知镜像，卸载后迟到通知不再生效", async () => {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  const dispose = await installNativeVisibility();
  expect(native.visible).toHaveBeenLastCalledWith(true);
  native.minimized = true;
  native.resized();
  await vi.waitFor(() => expect(native.visible).toHaveBeenLastCalledWith(false));
  native.minimized = false;
  native.resized();
  await vi.waitFor(() => expect(native.visible).toHaveBeenLastCalledWith(true));
  dispose();
  expect(native.off).toHaveBeenCalledOnce();
  const before = native.visible.mock.calls.length;
  native.minimized = true;
  native.resized();
  await Promise.resolve();
  expect(native.visible.mock.calls.length).toBe(before);
});

it("浏览器环境无需原生窗口权限", async () => {
  const dispose = await installNativeVisibility();
  dispose();
  expect(native.visible).not.toHaveBeenCalled();
});
