/**
 * 外壳冒烟用例（P7 Task 1b）：应用能挂载并渲染外壳，且**不调用任何命令**。
 *
 * 「不调用任何命令」是这条用例真正要钉的东西：模板页会调用一条示例命令，而那条命令
 * 在 Rust 侧已经删掉（`src-tauri/src/lib.rs` 只注册真实 handler），留着它就是一开窗
 * 就报 command not found。用 `mockIPC` 记账，任何命令被调用都会留下痕迹。
 */

import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";

import App from "../App";

afterEach(() => {
  clearMocks();
  cleanup();
});

describe("应用外壳", () => {
  it("挂载后渲染标题与三个区域占位，且一个命令都不调用", () => {
    const called: string[] = [];
    mockIPC((command) => {
      called.push(command);
      throw new Error(`外壳不该调用任何命令，却调用了 ${command}`);
    });

    const { container } = render(<App />);

    expect(container.querySelector(".app-title")?.textContent).toBe("Worktrace");
    for (const region of ["nav", "page", "status"]) {
      expect(
        container.querySelector(`[data-region="${region}"]`),
        `缺少区域占位 data-region="${region}"`,
      ).not.toBeNull();
    }

    expect(called).toEqual([]);
  });
});
