/**
 * 应用外壳（P7 Task 1b 建立，Task 3 把收件箱/计时接进来，Task 5 再接入项目/任务，
 * P8 Task 1b 接入第 5 块页面「今日」，P8 Task 2c 接入第 6、7 块「恢复」「历史」，
 * P8 Task 3b 接入第 8 块「数据」）：**固定布局 + 八块页面 + 状态栏**。
 *
 * 外壳只负责四件事，别的一概不做（00 §6：前端不含业务规则）：
 *
 * 1. `data-region="nav"`：常驻导航区——**不引路由**（离线取不到 `react-router`），
 *    八块页面用一次 `useState` 切换；
 * 2. `data-region="page"`：页面挂载区（收件箱 / 项目 / 任务 / 计时 / 今日 / 恢复 / 历史 / 数据）；
 * 3. `data-region="status"`：状态栏，读镜像的握手状态与计时展示值；
 * 4. **本上下文唯一那个订阅入口的启动接线**：挂载时 `domainState.start()`
 *    （Task 2 的 `startEventSession` 语义：先订阅并暂存 → 握手 → 取计时快照 → 按序交付），
 *    卸载时 `domainState.stop()`。**页面自己不开会话、也不各自订阅事件**——
 *    它们只通过 `src/state/hooks.ts` 读同一个入口。
 *
 * ## 「切到某页」是外壳的事，触发口在外面（P8 Task 2c，M8）
 *
 * 页面顺序照 GTD 的动线排：捕获（收件箱）→ 项目 → 任务 → 计时；「今日」（F-010 的五项）
 * 收在末尾——它是**日报视图**，不是动线的起点，**默认页仍是收件箱**；恢复与历史挂在
 * 最后，它们是**事后动线**（先处理待确认，再修正/补录）；「数据」（F-018 导出 / F-019
 * 备份与恢复）是**出口**，收在最后——它只往外给东西，不参与动线。
 *
 * 「切页」除了导航点击，还有一个**页面外部**的触发口：`src/state/pageRequest.ts`。
 * 今天的消费者是命令失败的降级路径——`RECOVERY_REQUIRED` 要把用户导到恢复页而不是只弹
 * 一句提示（计划 Task 2 的 M8）。外壳订阅它，并把随行的那句话（Rust 的 `message`）
 * 显示在页面区上方：发起那条命令的页面此时已经卸载，它自己的提示条留不下来。
 * 那句说明**可手动关闭**（`ErrorNotice` 的 `closable`）：它讲的是"为什么被扔到这一页"，
 * 用户处理完之后得有个消失路径；但**不做"到达即清"**——M8 要的正是"看得见原因"。
 * 用户自己点导航时它同样作废（新的一页有自己的上下文）。
 *
 * 「当前任务」不在外壳里存：计时页与状态栏的标题都读**快照自己的** `task_title`
 * （契约随 `task_id` / `task_row_version` 一起下发，见 `src/components/timerRequests.ts`
 * 的模块头）。外壳因此不持有任何跨页面的业务状态——`pageRequest` 带过来的那一句是
 * **一次跳转的说明**，不是业务状态。
 *
 * 不做的事：不引路由与状态库；不做多面板 `dockview` 布局（R-04 之后再说，
 * `src/components/DockviewDemo.tsx` 仍在仓库里但不进发布产物）；不做业务判断。
 */

import { useEffect, useState } from "react";
import { Layout, Menu, Typography } from "antd";

import "./App.css";
import { ErrorNotice } from "./components/ErrorNotice";
import { formatDuration } from "./components/duration";
import { Data } from "./pages/Data";
import { History } from "./pages/History";
import { Inbox } from "./pages/Inbox";
import { Projects } from "./pages/Projects";
import { Recovery } from "./pages/Recovery";
import { Tasks } from "./pages/Tasks";
import { SESSION_STATE_TEXT, Timer } from "./pages/Timer";
import { Today } from "./pages/Today";
import { domainState } from "./state/domainState";
import { useHandshakePhase, useTimerSnapshot } from "./state/hooks";
import { onPageRequest, type PageKey } from "./state/pageRequest";

const { Header, Sider, Content, Footer } = Layout;

const PAGES: Array<{ key: PageKey; label: string }> = [
  { key: "inbox", label: "收件箱" },
  { key: "projects", label: "项目" },
  { key: "tasks", label: "任务" },
  { key: "timer", label: "计时" },
  { key: "today", label: "今日" },
  { key: "recovery", label: "恢复" },
  { key: "history", label: "历史" },
  { key: "data", label: "数据" },
];

/**
 * 页面挂载区：固定布局 + 一次 `useState` 切换，不引路由（R-04）。
 *
 * 用 `switch` 而不是嵌套三元：八块页面写成一串三元已经读不出分支了。
 */
function PageView({ page }: { page: PageKey }) {
  switch (page) {
    case "inbox":
      return <Inbox />;
    case "projects":
      return <Projects />;
    case "tasks":
      return <Tasks />;
    case "timer":
      return <Timer />;
    case "today":
      return <Today />;
    case "recovery":
      return <Recovery />;
    case "history":
      return <History />;
    case "data":
      return <Data />;
  }
}

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
  /**
   * 随一次**跳转**过来的说明（Rust 的 `message`，见模块头）。用户自己点导航时清掉：
   * 那句说明属于"为什么把我扔到这一页"，不属于这一页本身。
   */
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    // 启动：失败不改本地状态（镜像自己会置 `phase: "failed"`，状态栏照实显示）。
    domainState.start().catch(() => undefined);
    return () => {
      // StrictMode 的 mount→cleanup→mount 与真实卸载都走这里：`stop()` 幂等，
      // 之后再来一次 `start()` 会重新起一代（Task 2 fix round 2）。
      void domainState.stop();
    };
  }, []);

  useEffect(() => {
    // 「切到某页」的外部触发口（M8）：订阅 -> 返回退订函数。退订与订阅同源，
    // 所以 StrictMode 的双挂载也不会留下第二个处置者。
    return onPageRequest((next, message) => {
      setPage(next);
      setNotice(message ?? null);
    });
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
            onClick={({ key }) => {
              // 手动切页：上一次跳转的说明作废（新的一页有自己的上下文）。
              setNotice(null);
              setPage(key as PageKey);
            }}
            items={PAGES.map(({ key, label }) => ({ key, label }))}
          />
        </Sider>
        <Content className="app-page" data-region="page">
          {/*
            随跳转过来的那句说明：**可关闭**（fix round 1 / Minor-4）——它描述的是"你为什么
            被扔到这一页"，恢复页处理完之后没有别的消失路径。注意**不是"到达即清"**：
            那样 M8 的"看得见原因"就没了。
          */}
          <ErrorNotice message={notice} closable onClose={() => setNotice(null)} />
          <PageView page={page} />
        </Content>
      </Layout>
      <Footer className="app-status" data-region="status">
        <StatusBar />
      </Footer>
    </Layout>
  );
}
