/**
 * 应用外壳（P7 Task 1b 建立，Task 3 把页面接进来）：**固定布局 + 两块页面 + 状态栏**。
 *
 * 外壳只负责四件事，别的一概不做（00 §6：前端不含业务规则）：
 *
 * 1. `data-region="nav"`：常驻导航区——**不引路由**（离线取不到 `react-router`），
 *    两块页面用一次 `useState` 切换；
 * 2. `data-region="page"`：页面挂载区（收件箱 / 计时）；
 * 3. `data-region="status"`：状态栏，读镜像的握手状态与计时展示值；
 * 4. **本上下文唯一那个订阅入口的启动接线**：挂载时 `domainState.start()`
 *    （Task 2 的 `startEventSession` 语义：先订阅并暂存 → 握手 → 取计时快照 → 按序交付），
 *    卸载时 `domainState.stop()`。**页面自己不开会话、也不各自订阅事件**——
 *    它们只通过 `src/state/hooks.ts` 读同一个入口。
 *
 * 「当前任务」不在外壳里存：计时页与状态栏的标题都读**快照自己的** `task_title`
 * （契约随 `task_id` / `task_row_version` 一起下发，见 `src/components/timerRequests.ts`
 * 的模块头）。外壳因此不持有任何跨页面的业务状态。
 *
 * 不做的事：不引路由与状态库；不做多面板 `dockview` 布局（R-04 之后再说，
 * `src/components/DockviewDemo.tsx` 仍在仓库里但不进发布产物）；不做业务判断。
 */

import { useEffect, useState } from "react";
import { Layout, Menu, Typography } from "antd";

import "./App.css";
import { formatDuration } from "./components/duration";
import { Inbox } from "./pages/Inbox";
import { SESSION_STATE_TEXT, Timer } from "./pages/Timer";
import { domainState } from "./state/domainState";
import { useHandshakePhase, useTimerSnapshot } from "./state/hooks";

const { Header, Sider, Content, Footer } = Layout;

type PageKey = "inbox" | "timer";

const PAGES: Array<{ key: PageKey; label: string }> = [
  { key: "inbox", label: "收件箱" },
  { key: "timer", label: "计时" },
];

/** 握手状态的中文。这是**镜像自己**的状态（不是错误码），与 Rust 的失败文案无关。 */
const PHASE_TEXT: Record<string, string> = {
  idle: "未连接",
  connecting: "连接中",
  ready: "已连接",
  failed: "连接失败",
};

/**
 * 状态栏：握手状态 + 当前计时。
 *
 * 单独一个组件是有意的——它每拍 `timer.tick` 都要重渲染，而外壳与页面不必跟着重渲染。
 */
function StatusBar() {
  const phase = useHandshakePhase();
  const timer = useTimerSnapshot();

  const parts = [`连接：${PHASE_TEXT[phase] ?? phase}`];
  if (timer !== null && timer.session_id !== null && timer.state !== null) {
    // 「当前任务」只有快照一个来源。快照没给标题（理论上只有任务行读不到）时
    // **少显示一段**，不编一句"不知道的任务"出来——占位文案会在会话切换的那一拍
    // 露出来，而它不是任何权威值。
    if (timer.task_title !== null) parts.push(timer.task_title);
    parts.push(`${SESSION_STATE_TEXT[timer.state]} ${formatDuration(timer.active_ms)}`);
  }
  return <Typography.Text type="secondary">{parts.join(" · ")}</Typography.Text>;
}

export default function App() {
  const [page, setPage] = useState<PageKey>("inbox");

  useEffect(() => {
    // 启动：失败不改本地状态（镜像自己会置 `phase: "failed"`，状态栏照实显示）。
    domainState.start().catch(() => undefined);
    return () => {
      // StrictMode 的 mount→cleanup→mount 与真实卸载都走这里：`stop()` 幂等，
      // 之后再来一次 `start()` 会重新起一代（Task 2 fix round 2）。
      void domainState.stop();
    };
  }, []);

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
          <Menu
            mode="inline"
            selectedKeys={[page]}
            onClick={({ key }) => setPage(key as PageKey)}
            items={PAGES.map(({ key, label }) => ({ key, label }))}
          />
        </Sider>
        <Content className="app-page" data-region="page">
          {page === "inbox" ? <Inbox /> : <Timer />}
        </Content>
      </Layout>
      <Footer className="app-status" data-region="status">
        <StatusBar />
      </Footer>
    </Layout>
  );
}
