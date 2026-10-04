import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// 前端测试配置（P7 前置任务）。与 `vite.config.ts` 分开：那份带着 `tauri dev`
// 专用的固定端口与 HMR 设置，测试不需要它们。
//
// - `jsdom`：组件挂载与 DOM 断言要在浏览器环境语义下跑；
// - 复用 `@vitejs/plugin-react`（`vite.config.ts` 已在用它），JSX/React 转换只有一处配置；
// - 测试全局**不开**（不设 `globals: true`）：测试文件显式 `import { describe, it, expect }
//   from "vitest"`，这样 `tsc`（`pnpm build` 会类型检查 `src/**`）不需要改 `tsconfig.json`
//   的 `types`。
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
  },
});
