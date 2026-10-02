# Worktrace 总体架构

| 项 | 值 |
| --- | --- |
| 文档状态 | 设计草案（待评审） |
| 日期 | 2026-10-02 |
| 上游文档 | [`../../PROJECT_SPEC.md`](../../PROJECT_SPEC.md) 产品设计草案 |
| 适用版本 | V0.1 起 |
| 英文版 | [`00-architecture.en.md`](00-architecture.en.md) |

> 本文描述**怎么做**。**做什么、为什么**以 `PROJECT_SPEC.md` 为准；两者冲突时以本文为准并在 ADR 中记录理由。

---

## 1. 定位与非目标

Worktrace 是一款面向个人长期使用的 **AI 辅助工作管理、时间管理与工作分析桌面工具**。

两条贯穿全局的约束（来自 SPEC §5.1、§5.2）：

- **Local First** —— 核心数据只存本地 SQLite，无网络也能完整使用。
- **AI Optional** —— AI 是增强层。Task / Project / Timer / WorkSession / Tag / Report / Review / Search / Export 在没有 AI、没有网络时必须全部可用。

**明确不做**（SPEC §50）：插件市场、通用工作流引擎、通用 Agent 平台、IDE 替代、Office 替代、CAD 自动化。这些边界在架构上体现为一件事：**不引入插件系统**（见方案对比结论，ADR-002）。

---

## 2. 技术栈（锁定）

| 层 | 选型 | 说明 |
| --- | --- | --- |
| 应用壳 | Tauri 2 | 单进程，多 Webview 窗口 |
| 前端 | React 19 + TypeScript 6 + Vite 8 | |
| 组件库 | **Ant Design 6（唯一）** | MUI / emotion 移除，见 ADR-001 |
| 面板分屏 | dockview 8 | **待评估**，见 ADR-004 |
| 包管理 | pnpm | |
| 后端 | Rust，edition 2021 | |
| 数据库 | SQLite，`rusqlite` + `bundled` | 见 ADR-003 |
| 平台 | Windows 优先 | 平台调用集中在 `platform/`，见 ADR-005 |

---

## 3. 分层与依赖规则

### 3.1 六个层

| 层 | 目录 | 职责 | 允许依赖 |
| --- | --- | --- | --- |
| L4 边界 | `commands/` | Tauri IPC：参数校验、DTO 转换、编排调用 | `services`, `domain`, `events` |
| L3 服务 | `services/` | timer / session / statistics / report / knowledge / context / search / ai | `domain`, `storage`, `platform`, `events` |
| L2 持久化 | `storage/` | 连接、事务、schema、迁移、备份、仓储实现 | `domain`, `events` |
| L1 领域 | `domain/` | 纯类型与规则，**无 IO** | `events` |
| L0 平台 | `platform/` | Win32：窗口样式、托盘、热键、休眠检测、单实例 | 无 |
| 横切 | `events/` | 事件类型 + 总线 | 无 |

依赖方向恒为 L4 → L3 → L2 → L1，`platform/` 与 `events/` 是叶子。

### 3.2 三条硬规则

1. **`domain/` 不得 import `storage/` 或 `platform/`。**
   领域规则必须能脱离数据库与操作系统单独跑测试。此条一破，`domain/` 就退化为"数据库的附属结构体"，SPEC §8–13 的对象模型随之失守。
2. **`storage/` 不得 import `platform/`。**
   数据库文件路径由组合根注入；存储层不应知道 Windows 的存在。
3. **`commands/` 不得直接 import `storage/`。**
   一切数据访问必须经过 `services/`。否则业务规则会从服务层漏进 IPC 边界，散落成"顺手在命令里写死"的逻辑。

### 3.3 组合根

`src-tauri/src/lib.rs` 是**唯一装配处**：建 storage → 建 services → 注册 commands → 建窗口与托盘。

分层的成败取决于"是否只有这一个文件 import 所有层"。任何第二个 import 跨全部层的地方，都应先怀疑设计。

---

## 4. 运行时拓扑

```
Rust 主进程（唯一真相源）
├── 事件总线 ──── 内部领域事件 + 广播到所有窗口
├── SQLite ────── 单文件，WAL
├── 窗口（每个 = 独立 Webview = 独立 JS 上下文，不共享内存）
│     main   主窗，1100×760（min 800×600）
│     hud    置顶 / 透明 / 鼠标穿透 / 无焦点 / 无任务栏图标
│     mini   可交互小窗：暂停 / 完成 / 切换 / 快速捕获
└── 托盘 ──────── 原生菜单，不依赖任何窗口存活
```

### 4.1 窗口只是订阅者

**状态只存在于 Rust 一处。** 因此：

- 关掉 HUD、关掉主窗、最小化到托盘 —— 计时与统计照跑。
- 主窗 / HUD / Mini 读同一份状态，不可能出现"HUD 显示 01:17:34、主窗显示 01:17:29"。
- 窗口重开时不需要重建状态，只需重新拉一次快照。

### 4.2 多入口构建

HUD 与 Mini 使用**独立的 Vite HTML 入口**（`hud.html` / `mini.html`），而不是主入口加路由参数。

理由：HUD 要求秒开、常驻、极简。走主入口意味着透明小窗要加载主窗整套页面代码，启动慢、内存高、透明窗口渲染更易出问题。代价仅为 Vite 中配置 `build.rollupOptions.input` 多入口。

### 4.3 托盘独立于窗口

托盘项（当前任务 / 暂停 / 完成 / 快速捕获 / 显示 HUD / 退出）直接读写 Rust 状态并发事件。SPEC §42 要求"最小化后隐藏任务栏图标、保留 Tray、后台继续运行"——这只有托盘不依赖窗口存活才能成立。

### 4.4 单实例

第二次启动必须唤起已有实例的主窗，不得起第二个进程（否则两个进程写同一个 SQLite 文件）。实现归 `platform/single_instance.rs`。

### 4.5 HUD 的 Locked / Edit

HUD 的两种模式（SPEC §40）由 **Rust 侧持有状态**，因为切换需修改 Win32 窗口扩展样式；前端只发一条命令。

---

## 5. IPC 契约

| 通道 | 方向 | 用途 | 机制 |
| --- | --- | --- | --- |
| **command** | 前端 → Rust | 所有**意图**（开始任务、完成任务、查询视图） | `invoke()` |
| **event** | Rust → 前端 | 所有**状态变化通知**，投递给全部窗口 | `emit()` |

### 5.1 command 粒度原则

| 原则 | 正例 | 反例 |
| --- | --- | --- |
| 一次调用完成一个完整业务事务 | `complete_task(task_id, quality)` 内部依次结束 session → 写工时 → 更新统计 → 发事件 | `end_session()` + `update_task_status()` + `refresh_stats()` 三次 IPC |
| 视图聚合一次取回 | `get_today_view()` 返回 Today 页所需全部数据 | `list_tasks()` + `list_sessions()` + `get_stats()` |
| 按意图命名 | `start_task` / `pause_session` / `quick_capture` | `insert_session` / `update_row` |
| 禁止 N+1 | 列表自带所需字段 | 列表每行再调 `get_task_detail` |

**DTO 策略**：领域类型直接 `#[derive(Serialize)]` 复用；**视图类 command 返回专门的聚合 DTO**（`TodayView`、`ReportView`）。不做"每个领域类型配一个镜像 DTO"的双层结构。

### 5.2 event 信封

命名沿用 SPEC §5.4 的 `实体.动作` 点分式：`task.completed`、`session.started`、`session.finished`、`task.updated`、`project.updated`、`report.generated`。

```jsonc
{
  "event": "task.updated",
  "revision": 1042,                          // 单调递增全局版本号
  "at": "2026-10-02T21:30:00+08:00",
  "payload": { }
}
```

`revision` 是**本文新增的**（SPEC 未提），理由：窗口可能重开、刚 reload、或错过事件。窗口挂载时调 `get_snapshot()` 取得当前 revision，之后若收到的事件 revision 跳号，即知漏事件并重新拉全量。没有它，错过事件的窗口会永久显示过期数据。

### 5.3 错误契约

所有 command 返回 `Result<T, AppError>`，`AppError` 序列化为：

```jsonc
{ "code": "TASK_NOT_FOUND", "message": "...", "detail": { } }
```

**Rust panic 不允许穿透 IPC 边界**，一律转为 `AppError`。

---

## 6. 前端状态镜像

Q3=A（Rust 单一真相源）能否落地，取决于这一层。

**统一入口**：`src/services/domainState.ts` 提供唯一的订阅与缓存。**禁止任何页面自行 `listen()`** —— 否则重复订阅、内存泄漏、状态不一致会同时出现。

| 事件类型 | 前端行为 | 理由 |
| --- | --- | --- |
| 领域事件（`task.created` / `updated` / `completed`、`project.updated` …） | **只当缓存失效信号** → 重新拉取受影响视图 | 前端绝不复刻业务规则。若用事件"打补丁"，规则就有两份，必然漂移 |
| 高频计时（`timer.tick`，约 1 Hz） | 直接替换展示值 `{ session_id, elapsed_ms, remaining_ms }` | 每秒拉全量太重；且这是纯展示数据，前端不对它做业务判断 |

**技术形态**：极薄 store（`useSyncExternalStore` + 一个 `Map`）。**不引入 Redux / Zustand / Jotai** —— 真相源在 Rust，前端只是缓存；复杂状态管理库解决的是"前端持有真相源"的问题，而该问题已被 Q3=A 消除。

**类型同步**：Rust DTO → TS 类型不得手写。候选 `ts-rs`、`tauri-specta`，二选一，在实现阶段第一步确认维护状态与 Tauri v2 兼容性（见 §8）。

---

## 7. 待实测假设

以下三条**尚未验证**，不得在实现中当作既成事实：

1. **Windows 鼠标穿透的动态切换** —— `WS_EX_LAYERED | WS_EX_TRANSPARENT` 的组合，以及切换后是否需要 `SetWindowPos(..., SWP_FRAMECHANGED)` 才生效。
2. **Tauri v2 多窗口 × Vite 多入口** —— `WebviewWindowBuilder` 的 url 与多入口产物的路径对应关系。
3. **同步 `rusqlite` 与 Tauri command 的配合** —— 同步 command 的实际执行线程，以及重查询是否统一走 `spawn_blocking`。

验证结论应回填本文与 ADR-003 / ADR-005。

---

## 8. ADR 索引

| 编号 | 决策 | 状态 |
| --- | --- | --- |
| ADR-001 | 组件库只保留 Ant Design，移除 MUI / emotion | 已定，待执行 |
| ADR-002 | 分层单体 + 内部事件总线（否决微内核插件化与多 crate） | 已定 |
| ADR-003 | SQLite 访问用 `rusqlite` + `bundled` | 已定 |
| ADR-004 | dockview 的必要性评估 | **待评估** |
| ADR-005 | Windows 优先 + 单一平台边界 `platform/` | 已定 |
| ADR-006 | Rust 为唯一真相源，React 只持 UI 临时状态 | 已定 |

详见 [`03-adr.md`](03-adr.md)。
