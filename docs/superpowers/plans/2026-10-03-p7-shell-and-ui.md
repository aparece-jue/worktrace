# P7 · 桌面外壳与业务界面 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把已经做好的服务接到用户手上：Tauri 命令接线、前端状态镜像、捕获→理清→计时→Today→修正/恢复→导出的页面流，以及托盘与窗口生命周期——并保证发布产物里没有不该有的东西。

**Architecture:** 前端**不含业务规则**：它只做订阅、渲染与转发命令。每个 JS 上下文只有**一个** `domainState` 订阅入口，页面通过 hooks 读取；事件只作缓存失效，计时 tick 只更新展示值。所有判断（状态合法性、统计口径、恢复分流）都留在 Rust。启动顺序只调 P6 的 `bootstrap` 一个入口。

**Tech Stack:** Tauri 2 · React 19 · TypeScript 6 · Vite 8 · Ant Design（ADR-001 统一 UI 库）· `useSyncExternalStore`（先不上状态库）

**Spec:**
- `../specs/2026-10-02-worktrace-architecture/00-architecture.zh.md` §4（IPC 与 DTO 生成）、§5（快照、去重、计时协议）、§6（前端状态边界）
- `.../04-functional-spec.zh.md` F-001、F-002、F-003、F-009、F-010、F-011、F-015、F-017、F-018、F-019、F-020
- `.../03-adr.zh.md` ADR-001（统一 Ant Design）、ADR-012 的 R-04（`DockviewDemo` 不进发布产物）

**依赖的前置计划：** P1–P6 全部。本计划**只消费**它们的服务入口与 DTO，不重写任何业务判断。

**边界（不要越界）：**
- **HUD 与全局捕获热键属 V0.1b**（F-012/F-013），本计划不做。
- **dockview 布局与布局保存**属 R-04 之后（"已明确需要同时看多面板时再接"），本计划用**固定布局**；`src/components/DockviewDemo.tsx` 保留在仓库但**必须排除在打包产物之外**。
- Mini 小窗待评估，不做。
- 本计划不新增任何统计/计时/恢复逻辑；发现缺什么，回到对应计划补，不在前端兜。

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。前端测试只断言**展示与转发**，业务断言留在 Rust 侧。

---

## Task 1：Tauri 命令接线与 DTO 生成

文件：src-tauri/src/commands/mod.rs、src-tauri/src/lib.rs、src/types/generated.ts（生成物）、src/ipc.ts、tests（Rust 侧的命令层测试）。

- [ ] `commands/` 层**不接受 `Connection`**（总纲 §9）：它只做参数反序列化、调用 `services::*`、把 `Result<T, AppError>` 映射成前端可用的形状。事务与 epoch/version 校验由服务与 `storage::guards` 负责。
- [ ] 启动只调 **P6 的 `bootstrap`** 一个入口；`lib.rs` 不得自己重排"单实例 → 库 → run → 扫描 → 协调器 → 窗口"的顺序。
- [ ] DTO 类型**从 Rust 生成**（00 §4），不手写两份。生成物提交进仓库，并在 CI/本地有"生成物与源一致"的检查（改了 Rust 类型忘了重新生成要能被发现）。
- [ ] 命令**按意图命名、一次完成业务事务**（00 §4）；Today 等聚合视图一次返回，列表避免 N+1。
- [ ] 所有修改类命令都带 `expected_data_epoch`，更新既有对象带 `expected_row_version`；`RECOVERY_REQUIRED` / `DATA_EPOCH_MISMATCH` / `VERSION_CONFLICT` / `DATA_RESTORE_IN_PROGRESS` 四个码要能原样透传到前端（载荷形态见总纲 §8 待确认项 1）。
- [ ] 测试：命令层不直接引用 `rusqlite`（用 grep 自查）；未知/非法参数返回稳定错误码而不是 panic；DTO 生成物与 Rust 定义一致。

## Task 2：前端状态镜像

文件：src/state/domainState.ts、src/state/hooks.ts、src/state/__tests__/。

- [ ] **每个 JS 上下文只有一个 `domainState` 订阅入口**（00 §6 原文）；页面通过 hooks 读取，卸载时清理监听。多窗口各自一个上下文，互不共享内存。
- [ ] 事件只作**缓存失效**，计时 tick 只更新**展示值**（00 §6）；两者都不触发业务判断。
- [ ] 窗口启动顺序：**先监听并暂存通知，再拉一致快照**（00 §5 规则 1），避免快照与通知之间丢事件。
- [ ] 收到快照后丢弃**同 epoch 且 `revision <=` 快照版本**的通知；未知 epoch 的通知**只触发重新握手**，不直接接纳（规则 2）。
- [ ] 可见窗口**至多每 30 秒**校验 `get_revision`（返回 epoch+revision）；隐藏窗口在显示前校验（规则 4）。末次通知丢失仍要能收敛。
- [ ] 计时状态的前端判断顺序：先查 `data_epoch`、`run_id`、`session_version`，再比 `tick_seq`；**旧状态生成的 tick 即使序号较新也不能覆盖**暂停/切换后的展示（00 §5）。较新 `session_version` 的未知 tick 先触发计时快照，**不自行推导状态跃迁**。
- [ ] 先用 `useSyncExternalStore` 等简单方案（00 §6）；**不得**把"不使用状态库"当成领域权威的必要条件——以后允许评估状态库，但业务规则不得因此搬到前端。
- [ ] 测试：乱序通知不改变展示；旧 tick 不覆盖新状态；快照版本旧于已应用时不覆盖；监听先于快照的时序正确；卸载后不再有监听泄漏。

## Task 3：捕获、理清与计时控制

文件：src/pages/Inbox.tsx、src/pages/Timer.tsx、src/components/（按需）、tests（组件测试）。

- [ ] **F-001 快速捕获**：主窗内输入一句话回车即创建并**立即可见**；不要求项目/标签/日期；空标题拒绝并给出可理解提示。全局快捷键属 V0.1b，不做。
- [ ] **F-002 任务理清**：Inbox 可直接置 Ready；项目/标签可选，**无强制表单**；`start` 可在同一命令内原子理清并启动（前端只发一次命令，不自己拆成两步）。
- [ ] 任务状态动作与托盘“完成”调用 P3 transition_task 服务，不能由前端先 finish 再改状态；历史 correct 发送 session.expected_row_version，不发送不存在的 interval 版本。
- [ ] **F-003 状态机**：界面只展示 02 §5 允许的跃迁入口；非法跃迁的按钮不出现，但**即使出现也要被 Rust 拒绝**（前端过滤只是体验，不是校验）。显式 reopen 要有明确入口并与普通推进区分。
- [ ] **计时控制**：开始/暂停/继续/结束四个动作各对应一条命令，不做本地状态机；暂停值冻结、到点只提示不自动完成，均由 P2 的 DTO 驱动展示。
- [ ] 归档项目不出现在新建任务的选择列表；**即使前端漏过滤，服务也会拒绝**（P4 的保证），前端要正确显示该错误。
- [ ] 测试：空标题被拒；回车创建后列表立即可见；start 只发一次 IPC；非法跃迁不产生请求（且服务拒绝时提示正确）；错误码到用户文案的映射完整（不出现"未知错误"）。

## Task 4：Today、修正/恢复与导出备份

文件：src/pages/Today.tsx、src/pages/Recovery.tsx、src/pages/History.tsx、src/pages/Data.tsx。

- [ ] **F-010 Today**：今日选择列表、当前任务、确认人工工时、运行暂计、待确认时间**分别显示**，不预先相加；明确标注"按当前分类"（R-03）。
- [ ] **F-015 / F-017 恢复与修正**：recovering 记录要能看清"哪一段是可信的、哪一段待确认"；确认与丢弃是两个明确的动作，**不许合并成一个"丢弃"按钮**（02 §3 原文）；作废整次记录单独入口并二次确认。`correct` 只对 `finished` 开放，界面据此禁用在其它状态下的入口。
- [ ] **F-018 导出**：触发 P5 的生成函数并落盘（路径由本计划决定），导出后给出可打开的位置；界面上的数字与导出内容来自同一次查询（同一 `as_of`/`revision`）。
- [ ] **F-019 备份/恢复 UI**：恢复是危险操作，要有明确的维护态提示；恢复期间不接受用户命令（P6 的 `DATA_RESTORE_IN_PROGRESS` 要显示成"正在恢复"而不是通用错误）。
- [ ] 测试：五项数字分别渲染且不互相覆盖；恢复页对待确认与可信部分的标注正确；丢弃与作废是两个不同入口；恢复维护态下命令被禁用并有提示。

## Task 5：托盘与窗口生命周期

文件：src-tauri/src/platform/tray.rs（或等价）、src-tauri/tauri.conf.json、src-tauri/capabilities/。

- [ ] **F-011 托盘**：提供当前任务、暂停、完成、快速捕获、退出五项。**关闭所有窗口后托盘仍可用且计时继续**——这是验收的硬条件，要在真实环境手测。
- [ ] **F-009 窗口独立性**：关闭或隐藏所有窗口时核心继续运行；重开窗口**立即拉快照**而不是等下一次通知。托盘菜单的动作与界面动作走**同一批命令**，不另开路径。
- [ ] 显示 HUD 的托盘项属 V0.1b，本计划**不加**该菜单项。
- [ ] 退出流程：托盘"退出"走 P6 的显式退出（结束 running/paused、写 `clean_exit_at`、停定时器），不是直接杀进程。
- [ ] 测试（能自动化的部分）：托盘动作与界面动作调用同一命令；关窗不触发退出；重开窗口触发快照。**其余必须人工验收。**

## Task 6：发布产物门禁与端到端人工验收

文件：scripts/check-bundle.*（或 package.json 脚本）、tests/manual-v01.md（记录模板）。

- [ ] **R-04 门禁**：`DockviewDemo` 及其 CSS **不得出现在发布产物**。加一条构建后检查（在产物里搜 `DockviewDemo`/dockview 的标记），并把它列为发布前必须通过的一项。保留源码不动。
- [ ] 用**固定布局**（R-04：主界面先用固定布局）；`dockview` 依赖保留在 `package.json` 里但没有任何入口引用它。
- [ ] 端到端人工验收，逐条记录观察结果（模板落在 `tests/manual-v01.md`）：
  - F-001/F-002/F-003：捕获、理清、非法跃迁；
  - F-009/F-011：关掉全部窗口后托盘可用、计时继续；重开立即拉快照；
  - F-010：Today 五项数字与数据库明细一致；
  - F-015/F-017：强杀重启后四类判定各一条，确认/丢弃/作废各一次；
  - F-018：导出 JSON 用外部工具重算与界面一致；周回顾逐项核对；
  - F-019：备份→恢复→旧请求被拒；
  - F-020：多窗口同时打开，制造乱序/丢通知并确认收敛；
  - F-014：拔网线跑完整 V0.1 功能，无报错、无降级提示。
- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [ ] 完成门槛：`cargo fmt --check`、`cargo test`、`cargo clippy --all-targets` 全绿；前端 `tsc` 与构建通过；P1 分层检查通过（含 `services/` 不得直接取时间）；P1–P6 测试无回归。
- [ ] **不得把仓储/服务层测试标为"UI 已验收"**（P4 的约定）。人工验收记录要能对上具体版本与机器。

---

## 下游

本计划是 V0.1 的最后一份。完成后再对照 [总纲 §6「V0.1 的完成定义」](2026-10-03-v01-plan-index.md) 逐条核验，并按总纲 §4 的 F-ID 覆盖矩阵确认 18 项都有对应的验收记录。
