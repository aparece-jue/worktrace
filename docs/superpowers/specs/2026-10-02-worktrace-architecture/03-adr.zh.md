# Worktrace 技术决策记录（ADR）

| 项 | 值 |
| --- | --- |
| 文档状态 | 设计草案（待评审） |
| 日期 | 2026-10-02 |
| 上游 | [`00-architecture.zh.md`](00-architecture.zh.md) |
| 英文版 | [`03-adr.en.md`](03-adr.en.md) |

格式：**背景 → 决策 → 理由 → 后果 → 被否决的备选**。状态取值 `已定` / `待评估` / `已否决`。

| 编号 | 决策 | 状态 |
| --- | --- | --- |
| ADR-001 | 组件库只保留 Ant Design，移除 MUI / emotion | 已定，待执行 |
| ADR-002 | 分层单体 + 内部事件总线 | 已定 |
| ADR-003 | SQLite 访问用 `rusqlite` + `bundled` | 已定 |
| ADR-004 | dockview 的必要性 | **待评估** |
| ADR-005 | Windows 优先 + 单一平台边界 `platform/` | 已定 |
| ADR-006 | Rust 为唯一真相源，React 只持 UI 临时状态 | 已定 |
| ADR-007 | 不存 `task.actual_duration` | 已定 |
| ADR-008 | 时间戳统一用 Unix 毫秒整数 | 已定 |
| ADR-009 | HUD / Mini 使用独立 Vite 入口 | 已定 |
| ADR-010 | 事件信封携带单调 `revision` | 已定 |

---

## ADR-001 组件库只保留 Ant Design

**状态**：已定，待执行（执行时机：本设计通过后的第一步）

**背景**：脚手架依赖里同时存在 `antd@6`、`@mui/material@9`、`@emotion/react`、`@emotion/styled`。实测全仓库 `src/` 与 `src-tauri/src/` 中 **MUI / emotion 的实际 import 数为 0**；6 个文件使用 antd。

**决策**：从 `package.json` 移除 `@mui/material`、`@emotion/react`、`@emotion/styled`。MUI 仅作为设计/API 参考（查文档即可，不占依赖）。

**理由**：
- 两套组件库同装 = 双倍打包体积 + 双倍主题系统 + 双份心智负担
- 实际使用量为零，移除是零风险动作
- 已有的自研组件（`FloatingInput` / `FloatingSelect`）只依赖 antd 与 `theme.useToken()`，不依赖 MUI

**后果**：`pnpm-lock.yaml` 需重生成；必须跑一次 `pnpm build` 与 `pnpm tauri dev` 确认无残留引用。antd 成为唯一组件库，主题统一走 `src/theme.ts`。

**被否决的备选**：保留 MUI 作为"以后可能用得上"的依赖 —— 那正是 SPEC §57 反对的投机性储备。

---

## ADR-002 分层单体 + 内部事件总线

**状态**：已定

**背景**：13 个模块需要装进一个应用。可选：分层单体、微内核插件化、Cargo workspace 多 crate。

**决策**：分层单体。单人项目、单机部署，模块间通过内部事件总线解耦，不引入插件系统，暂不拆 crate。

**理由**：
- SPEC §4 的架构图本身就是分层形态
- SPEC §5.4 的事件驱动明确写着"不需要做成复杂工作流引擎"
- SPEC §50 把 `Plugin Marketplace` 与 `Generic Workflow Engine` 列为 Non-Goals —— 微内核插件化正是朝那个方向迈的第一步
- 多 crate 拆分对单人项目产生大量样板（各自的 `Cargo.toml`、feature 开关、循环依赖处理）

**后果**：模块边界靠**纪律与评审**维持，编译器不帮忙。因此 `00-architecture.zh.md` §3.2 的三条硬规则必须进 CI（至少进 code review 清单）。

**被否决的备选**：
- 微内核 + 插件：为不存在的需求付费，且与 Non-Goals 冲突
- 多 crate：不是永久否决。当某个模块确实需要隔离时（最可能是 `ai` —— 若将来要能整体剔除网络依赖），从单 crate 切出一个 crate 是局部改动，不返工

---

## ADR-003 SQLite 访问用 `rusqlite` + `bundled`

**状态**：已定（含一条待实测项）

**背景**：候选 `rusqlite` / `sqlx` / Diesel / `tauri-plugin-sql`。

**决策**：`rusqlite`，开启 `bundled` 特性。数据库只在 Rust 侧访问。

**理由**：
- ADR-006 已定 Rust 为唯一真相源，`tauri-plugin-sql` 天生面向"前端直连 SQL"，与其直接冲突，首先排除
- `bundled` 把 SQLite 源码编进二进制 → 不依赖用户机器上的 SQLite，桌面分发零额外依赖
- 本地单用户、单文件数据库，没有连接池与多后端的压力，同步 API 反而让事务与心智负担更低
- `sqlx` 的编译期 SQL 校验需要 `DATABASE_URL` 或离线 `.sqlx` 缓存，构建与 CI 复杂度上升，收益在这个规模体现不出来
- Diesel 需 `print-schema` 生成 `schema.rs`，宏多，对"多条件筛选 + 聚合"这类动态查询不灵活

**后果**：SQL 是字符串，**没有编译期校验** → 用测试补偿：M01 的每个仓储函数配一个测试。这是本决策的主要代价，必须接受而不是回避。

**待实测**：同步 `rusqlite` 与 Tauri command 的配合方式 —— 同步 command 的实际执行线程，以及重查询是否统一走 `spawn_blocking`。见 `00-architecture.zh.md` §7 第 3 条。

**被否决的备选**：`sqlx`（构建复杂度换来的编译期校验在本规模不划算）、Diesel（对动态查询不灵活）、`tauri-plugin-sql`（与 ADR-006 冲突）。

---

## ADR-004 dockview 的必要性

**状态**：**待评估**（唯一未决项）

**背景**：脚手架带 `dockview@8` + `dockview-react@8`。实测全仓库 dockview 的消费者**只有一个**：`src/components/DockviewDemo.tsx`（41 行的演示组件）。两个真正在用的自定义组件（`FloatingInput` / `FloatingSelect`）都不依赖它。

**待回答的问题**：主界面到底需不需要**多面板可拖拽分屏**？

| 分支 | 判据 | 后果 |
| --- | --- | --- |
| **需要** | Today / Inbox / Timer / Reports 等页面存在"同屏并列观察多个视图"的真实需求，且用户会主动调整布局 | 保留 dockview，并为它写一份独立 spec：面板注册表、布局持久化、与 `features/` 的关系 |
| **不需要** | 各页面单视图即可满足；并列观察可用固定分栏或标签页解决 | 移除 dockview 与 `DockviewDemo`，`src/components` 只留真正在用的组件 |

**倾向**：暂倾向前者的证据不足。SPEC §36 列的核心页面（Inbox / Today / Projects / Calendar / Review / Reports / Knowledge / Settings）都是**单页单视图**形态，§37 的 Today 是纵向列表，§38–42 的 HUD / Mini / Tray 与 dockview 无关。**但这是 UI 使用习惯问题，应由产品判断而非架构判断决定**，故不在本文档下结论。

**评估时机**：M12 前端骨架构思 Today 与 Reports 页面时。在此之前 dockview **保留依赖、不接入**，不写任何基于它的代码。

**注意**：`DockviewDemo.tsx` 是复制过来的演示文件，**不属于产品代码**。无论评估结果如何，它都不应出现在 V0.1 的发布产物中。

---

## ADR-005 Windows 优先 + 单一平台边界

**状态**：已定（含一条待实测项）

**背景**：Tauri 天然跨平台，但 SPEC §38–42 要的 HUD 是"置顶 + 透明 + 鼠标穿透 + 无任务栏图标 + 不抢焦点"，三平台实现差异极大（Windows 用 `WS_EX_LAYERED` / `WS_EX_TRANSPARENT`，macOS 用 `NSPanel` + `ignoresMouseEvents`，Linux 各 WM 各行其是）。

**决策**：Windows 单平台优先。全部平台相关调用集中在 `src-tauri/src/platform/`，**不建 trait 抽象、不预做其他平台**。

**理由**：
- 只是目录纪律，成本几乎为零
- 但把移植成本从"翻遍整个 `src-tauri` 找散落的 `#[cfg(windows)]`"降为"改写一个目录"
- 不建抽象层：为一个实现建 trait 是 SPEC §57 反对的投机性抽象

**后果**：`platform/` 是叶子层，只有 `services/` 及以上可调用（见 `00-architecture.zh.md` §3.1）。这条是三条硬规则之外的第 4 条边界纪律。

**待实测**：Win32 鼠标穿透的动态切换方式，以及是否需要 `SetWindowPos(..., SWP_FRAMECHANGED)`。见 `00-architecture.zh.md` §7 第 1 条。

**被否决的备选**：现在就做多平台 —— 成本高，且 HUD / 托盘 / 全局热键都需各平台真机验证。

---

## ADR-006 Rust 为唯一真相源

**状态**：已定

**背景**：领域状态（当前任务、计时、统计、标签）由谁持有。

**决策**：Rust 持有全部领域状态。React 只持 **UI 临时状态**（表单输入、选中项、展开态）。通信 = command（前端→Rust 请求/响应）+ event（Rust→前端广播）。

**理由**（三条均来自 SPEC 本身）：
- §7 原文："Rust 负责核心状态、计时、统计、数据库、Context 和 AI 调用"
- §15 要求计时不受 JS 卡顿、窗口隐藏、系统休眠影响 —— 只有以 Rust 侧时间为准才能满足
- §38–42 要求主窗 / HUD / Mini / 托盘四个入口同时存在。Tauri 中每个 Webview 是**独立 JS 上下文**，没有共享 store；状态若在 React，就要自己实现跨窗口同步

**后果**：
- 每个界面动作跨 IPC → command 粒度必须按"用户意图"设计，禁止 N+1（见 `00-architecture.zh.md` §5.1）
- 需要一层标准化的前端状态镜像（`src/services/domainState.ts`），**禁止页面自行 `listen()`**
- 前端可替换：将来更换 UI 技术栈不动核心

**被否决的备选**：
- React 主导状态：多窗口同步要自己写三遍；计时受 JS 卡顿影响（§15 明确反对）；托盘读不到 React 状态
- 双真相源 + 事件同步：一致性维护成本高，几乎必然出 bug

---

## ADR-007 不存 `task.actual_duration`

**状态**：已定

**背景**：SPEC §8.4 的 Task 字段表列了 `actual_duration`。

**决策**：**不存**。实际工时一律由 `work_session` 聚合得出。

**理由**：存一份就有两个真相源，缓存与明细必然漂移。`work_session(task_id)` 有索引，个人规模下聚合开销可忽略。

**后果**：任一需要"任务实际工时"的查询都要 JOIN + SUM。若报表成为瓶颈，加**物化汇总表**并由 M01 在 session 结束时维护，而不是在 `task` 上挂语义模糊的列。

**被否决的备选**：存字段 + 在 session 结束时更新 —— 缓存失效路径多（打断、崩溃恢复、人工修正工时），漏一条就不一致。

---

## ADR-008 时间戳统一用 Unix 毫秒整数

**状态**：已定

**决策**：所有时间字段为 `INTEGER`，存 Unix 毫秒，不带时区。展示层按本地时区渲染。

**理由**：避免夏令时与跨时区迁移的歧义；不让 SQLite 的日期函数参与业务计算（业务计算统一在 Rust 侧）。

**后果**：调试时看到的是数字而非可读时间，需要在日志与调试工具里统一格式化。区间边界（"今天"）由 Rust 侧按本地时区计算后传入，SQL 不做时区推理。

---

## ADR-009 HUD / Mini 使用独立 Vite 入口

**状态**：已定（含一条待实测项）

**背景**：多窗口可用"单一入口 + 路由参数"，或"每个窗口一个 HTML 入口"。

**决策**：`hud.html` 与 `mini.html` 作为独立入口。

**理由**：HUD 要求秒开、常驻、极简。走主入口意味着透明小窗要加载主窗整套页面代码 —— 启动慢、内存高、透明窗口渲染更易出问题。

**后果**：Vite 需配 `build.rollupOptions.input` 多入口；各入口共享 `theme.ts` 与 `components/`，但**不得** import `features/` 下的主窗页面。

**待实测**：`WebviewWindowBuilder` 的 url 与多入口产物的路径对应关系。见 `00-architecture.zh.md` §7 第 2 条。

---

## ADR-010 事件信封携带单调 `revision`

**状态**：已定

**背景**：SPEC 定义了事件名，但未定义信封。窗口可能重开、reload，或错过事件。

**决策**：每个 event 带 `{ event, revision, at, payload }`，`revision` 是 Rust 侧单调递增的全局版本号。

**理由**：窗口挂载时调 `get_snapshot()` 取得当前 revision；之后若收到的事件 revision **跳号**，即知漏事件并重新拉全量。没有它，错过事件的窗口会永久显示过期数据，且没有任何机制能发现。

**后果**：Rust 侧需维护一个全局计数器（含跨重启的持久化，避免重启后版本号回退导致前端误判）；`get_snapshot()` 成为必须实现的 command。

**被否决的备选**：定时轮询全量刷新 —— 延迟不可控、开销大，且高频计时场景下反而更贵。
