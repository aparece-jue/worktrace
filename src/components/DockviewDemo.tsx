import { useRef, useState } from "react";
import { Button, Input, Space, Table, Tag } from "antd";
import { DockviewReact, themeLight, type DockviewApi, type DockviewReadyEvent, type IDockviewPanelProps } from "dockview-react";
import "dockview/dist/styles/dockview.css";
import "./DockviewDemo.css";

function NotesPanel({ api }: IDockviewPanelProps) {
  const [text, setText] = useState("");
  return <div className="dock-panel"><h3>{api.title}</h3><p>输入一些文字，再拖动标签或调整分屏，观察面板状态。</p><Input.TextArea aria-label={`${api.title}内容`} value={text} onChange={(e) => setText(e.target.value)} placeholder="临时笔记，关闭面板或刷新后不保存" autoSize={{minRows:5,maxRows:12}} /></div>;
}
function WelcomePanel() {
  return <div className="dock-panel"><Tag color="blue">Dockview + Ant Design</Tag><h2>可自由布局的工作区</h2><ol><li>拖动顶部标签，改变顺序。</li><li>拖到面板边缘的提示区域，创建分屏。</li><li>拖到另一组的标签栏，合并面板。</li><li>拖动分隔线，调整面板大小。</li><li>点击标签关闭按钮，关闭面板。</li></ol><p>所有面板都关闭后，仍可用上方按钮新增或重置。这里不启用独立弹出窗口。</p></div>;
}
function TasksPanel() {
  return <div className="dock-panel"><h3>任务列表</h3><Table size="small" pagination={false} rowKey="name" dataSource={[{name:"调整主题",status:"已完成"},{name:"测试拖动",status:"进行中"},{name:"体验分屏",status:"待处理"}]} columns={[{title:"任务",dataIndex:"name"},{title:"状态",dataIndex:"status",render:(status:string) => <Tag color={status === "已完成" ? "success" : "blue"}>{status}</Tag>}]} /></div>;
}
const components = { welcome: WelcomePanel, notes: NotesPanel, tasks: TasksPanel };
function initialLayout(api: DockviewApi) {
  api.clear();
  api.addPanel({id:"welcome",component:"welcome",title:"使用指南"});
  api.addPanel({id:"notes",component:"notes",title:"笔记",position:{referencePanel:"welcome",direction:"within"}});
  api.addPanel({id:"tasks",component:"tasks",title:"任务",position:{referencePanel:"welcome",direction:"right"}});
  api.getPanel("welcome")?.api.setActive();
}
export default function DockviewDemo() {
  const apiRef = useRef<DockviewApi | null>(null);
  const counter = useRef(0);
  const [ready, setReady] = useState(false);
  function addPanel(split: boolean) {
    const api = apiRef.current;
    if (!api) return;
    const id = `note-${++counter.current}`;
    api.addPanel({id,component:"notes",title:`笔记 ${counter.current}`, ...(api.activePanel ? {position:{referencePanel:api.activePanel.id,direction:split ? "right" as const : "within" as const}} : {})});
  }
  function onReady({api}: DockviewReadyEvent) {
    apiRef.current = api;
    initialLayout(api);
    setReady(true);
  }
  return <section><h2>Dockview 工作区演示</h2><p className="section-note">拖动标签进行排序或分屏，拖动分隔线调整大小。面板里的内容仍然是 React 和 Ant Design。</p><Space wrap className="dock-toolbar"><Button type="primary" disabled={!ready} onClick={() => addPanel(false)}>新增标签</Button><Button disabled={!ready} onClick={() => addPanel(true)}>右侧分屏</Button><Button disabled={!ready} onClick={() => apiRef.current && initialLayout(apiRef.current)}>重置布局与笔记</Button></Space><div className="dock-workspace"><DockviewReact components={components} onReady={onReady} theme={themeLight} /></div><p className="section-note">演示数据仅存在内存中；关闭面板、重置或刷新会清除笔记。</p></section>;
}
