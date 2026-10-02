import { useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { Alert, Button, Card, Input, Space, Typography } from "antd";
import "./App.css";

export default function App() {
  const [name, setName] = useState("Worktrace");
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  async function connect() {
    setLoading(true);
    setMessage("");
    setError("");
    try { setMessage(await invoke<string>("greet", { name })); }
    catch (cause) { setError(String(cause)); }
    finally { setLoading(false); }
  }
  return (
    <main className="app-shell">
      <header>
        <Typography.Text type="secondary">个人工作管理 · 时间记录 · 工作分析</Typography.Text>
        <Typography.Title>Worktrace</Typography.Title>
        <Typography.Paragraph type="secondary">记录工作过程，让每一份投入清晰可见。</Typography.Paragraph>
      </header>
      <Card title="项目已就绪">
        <Typography.Paragraph>基于 Tauri 2、React 和 TypeScript，开始构建你的工作管理应用。</Typography.Paragraph>
        <Space.Compact className="connection-form">
          <Input aria-label="名称" value={name} onChange={(event) => setName(event.target.value)} />
          <Button type="primary" loading={loading} disabled={!isTauri()} onClick={connect}>测试 Rust 连接</Button>
        </Space.Compact>
        {!isTauri() && <Typography.Paragraph className="status" type="secondary">当前为浏览器预览，请通过 pnpm tauri dev 启动桌面应用以测试后端。</Typography.Paragraph>}
        {message && <Alert className="status" type="success" title={message} showIcon />}
        {error && <Alert className="status" type="error" title={error} showIcon />}
      </Card>
    </main>
  );
}
