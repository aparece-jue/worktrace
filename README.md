# Worktrace

AI-assisted personal work management, time tracking and work analytics.

## 开发
需要 Node.js、pnpm、Rust，以及 Windows 的 MSVC C++ 构建工具和 WebView2。

```sh
pnpm install
pnpm tauri dev
```

- `pnpm dev`：浏览器预览。
- `pnpm build`：检查 TypeScript 并构建前端。
- `pnpm tauri build`：打包桌面应用。
- `cargo check --manifest-path src-tauri/Cargo.toml`：检查 Rust。

## 架构
参考同级 test-tauri-project，使用 Tauri 2、React 19、TypeScript、Vite 和 pnpm，保留 Ant Design、MUI、Dockview 依赖。

- `src/App.tsx`：初始页面与 Rust 通信示例。
- `src/components/`：共享组件目录。
- `src/theme.ts`：Ant Design 主题。
- `src-tauri/src/`：Rust 主入口、应用初始化与命令。
- `src-tauri/capabilities/`：窗口权限。
- `src-tauri/tauri.conf.json`：窗口、开发服务和打包配置。

初始页面使用 Ant Design；桌面端可点击“测试 Rust 连接”调用 greet 命令。未复制参考项目的串口业务。
