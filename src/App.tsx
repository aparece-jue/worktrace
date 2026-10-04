import { Layout, Typography } from "antd";
import "./App.css";

const { Header, Sider, Content, Footer } = Layout;

/**
 * 应用外壳（P7 Task 1b）：**固定布局 + 区域占位**。
 *
 * 这里**不放页面本体**：捕获/理清/计时（Task 3）与 Projects/Tasks（Task 5）
 * 各自往 `page` 区挂载。外壳只负责：
 *
 * - 一块常驻的导航区（`data-region="nav"`）；
 * - 一块页面挂载区（`data-region="page"`），页面换进换出时它不动；
 * - 一条状态栏（`data-region="status"`），计时/握手状态由 Task 2/3 填。
 *
 * 之前这里是模板自带的连接页：它会调用一条示例命令。那条命令在 P7 已经随真实接线
 * 一起从 Rust 侧删掉（`src-tauri/src/lib.rs` 只注册真实 handler），留着只会一开窗就报
 * command not found。
 *
 * 不做的事：不引路由（离线取不到 `react-router`），不做多面板 `dockview`
 * 布局（R-04 之后再说，`src/components/DockviewDemo.tsx` 仍在仓库里但不进发布
 * 产物），不做业务判断。
 */
export default function App() {
  return (
    <Layout className="app-shell">
      <Header className="app-header">
        <Typography.Title level={4} className="app-title">
          Worktrace
        </Typography.Title>
        <Typography.Text className="app-subtitle" type="secondary">
          个人工作管理 · 时间记录 · 工作分析
        </Typography.Text>
      </Header>
      <Layout className="app-body">
        <Sider className="app-nav" width={200} theme="light" data-region="nav">
          <Typography.Text type="secondary">导航区（Task 3 / Task 5 接入）</Typography.Text>
        </Sider>
        <Content className="app-page" data-region="page">
          <Typography.Title level={5}>工作台</Typography.Title>
          <Typography.Paragraph type="secondary">
            页面本体由后续任务接入：捕获、理清与计时（Task 3），Projects 与 Tasks 列表（Task 5）。
          </Typography.Paragraph>
        </Content>
      </Layout>
      <Footer className="app-status" data-region="status">
        <Typography.Text type="secondary">状态栏（计时与握手状态由 Task 2 / Task 3 接入）</Typography.Text>
      </Footer>
    </Layout>
  );
}
