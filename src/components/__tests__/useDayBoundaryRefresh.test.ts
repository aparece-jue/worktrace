import { act, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { useDayBoundaryRefresh } from "../useDayBoundaryRefresh";

afterEach(() => { vi.useRealTimers(); });

it("后端日界到达时重读，提前不重读，失败后的重试不忙循环", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
  const refresh = vi.fn(async () => {});
  const { unmount } = renderHook(() => useDayBoundaryRefresh(90_000, refresh));
  await act(async () => { vi.advanceTimersByTime(89_999); });
  expect(refresh).not.toHaveBeenCalled();
  await act(async () => { vi.advanceTimersByTime(1); });
  expect(refresh).toHaveBeenCalledTimes(1);
  await act(async () => { vi.advanceTimersByTime(59_999); });
  expect(refresh).toHaveBeenCalledTimes(1);
  unmount();
  await act(async () => { vi.advanceTimersByTime(120_000); });
  expect(refresh).toHaveBeenCalledTimes(1);
});

it("新日界替代旧日界，清空样本后不继续重读", async () => {
  vi.useFakeTimers();
  vi.setSystemTime(0);
  const refresh = vi.fn(async () => {});
  const { rerender, unmount } = renderHook(({ end }: { end: number | undefined }) =>
    useDayBoundaryRefresh(end, refresh), { initialProps: { end: 1_000 as number | undefined } });
  rerender({ end: 10_000 });
  await act(async () => { vi.advanceTimersByTime(1_000); });
  expect(refresh).not.toHaveBeenCalled();
  rerender({ end: undefined });
  await act(async () => { vi.advanceTimersByTime(20_000); });
  expect(refresh).not.toHaveBeenCalled();
  unmount();
});
