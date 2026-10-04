# 自定义组件

组件与对应 CSS 原样复制自 `test-tauri-project/src/components/`。

当前这些复制组件均未接入生产页面，下面示例仅说明接口。`DockviewDemo` 是演示资产，V0.1 发布入口不得导入；P8 发布门禁验证它不进入产物。保留文件不表示产品已经使用其布局。

- `FloatingInput.tsx`：浮动标签输入框；同时导出 `FormFloatingInput`，支持 Ant Design 表单校验状态。
- `FloatingSelect.tsx`：浮动标签单选下拉框。
- `DockviewDemo.tsx`：可拖动标签、分屏和调整大小的工作区演示（默认导出）。

输入框和下拉框通过 `theme.useToken()` 使用 `src/theme.ts` 配置的 Ant Design 主题。各组件自动导入对应 CSS；DockviewDemo 同时导入 Dockview 基础样式。

调用文件位于 `src/` 时：

```tsx
import { FloatingInput, FormFloatingInput } from "./components/FloatingInput";
import { FloatingSelect } from "./components/FloatingSelect";
import DockviewDemo from "./components/DockviewDemo";
```

FloatingInput 必填 `label`；FloatingSelect 必填 `label`、`onChange` 和 `options`。DockviewDemo 可直接使用 `<DockviewDemo />`。

组件已复制，尚未接入 Worktrace 首页。参考项目的组件对比页面和串口业务页面未包含在此目录。
