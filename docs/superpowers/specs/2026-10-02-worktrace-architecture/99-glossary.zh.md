# Worktrace 术语表

| 项 | 值 |
| --- | --- |
| 文档状态 | 设计草案（待评审） |
| 日期 | 2026-10-03 |
| 地位 | 本表统一术语；业务规则以修订后的 00/02/04 为准，术语冲突需同步修订 |
| 英文版 | [`99-glossary.en.md`](99-glossary.en.md) |

> 「禁止混用」一列是本文的主要价值 —— 它记录的是**这个项目里最容易混淆的术语对**。

---

## 1. 时间与工时（最易混淆的一组）

| 中文 | English | 定义 | 禁止混用 |
| --- | --- | --- | --- |
| 流逝时长 | Elapsed Time | 墙钟上经过的时间。09:00 到 10:00 就是 1 小时，无论中间在做什么 | **不得单独说「工时」**。说「工时」必须指明是哪一种 |
| **人工工时** | **Human Effort** | 用户本人实际投入的时间。只有 FOREGROUND 模式的 session 计入 | 与「实际时长」「流逝时长」**都不是**同义词 |
| 机器时长 | Machine / AI Process Time | 后台 AI 生成、仿真运行等非人工过程占用的时间。BACKGROUND / PASSIVE 计入此列 | 不得计入人工工时 |
| 关联时长 | Associated Duration | 统计口径一：任务带某标签，该标签就计入任务全部工时 | 与「加权工时」是**两种口径**，不可互相替代 |
| 加权工时 | Weighted Duration | 统计口径二：按标签权重分配工时（ADC 50% × 2h = 1h） | 同上 |
| 估时 | Estimated Duration | 任务开始前对耗时的估计。可以是 AI 给的（`source = ai`）或用户填的 | 与「计划时长」不是一回事 |
| 计划时长 | Planned Duration | 用户实际排进日程的时长 | 与「估时」不是一回事 |
| 实际时长 | Actual Duration | 事后统计出的真实耗时（由 session 聚合） | **不等于人工工时**（若含后台任务） |

> SPEC §12 的硬要求：`1h 设计（前台）+ 1h AI 生成（后台）` **必须**统计成 1h 人工工时，不是 2h。

---

## 2. 核心对象

| 中文 | English | 定义 | 禁止混用 |
| --- | --- | --- | --- |
| 目标 | Goal | 长期目标，如"完成工业 IO 控制板开发" | |
| 项目 | Project | 长期上下文容器（目标 + 约束 + 决策 + 任务 + 文档 + 知识） | 不只是"任务分组" |
| 里程碑 | Milestone | 项目阶段结果，如"原理图设计冻结" | |
| 任务 | Task | 主要管理对象。WBS 树上从 Task 到叶子的所有节点都是 Task | |
| 行动 | Action | **就是叶子 Task**，不单独建表（见 02-data-model §7） | 不是独立实体类型 |
| 会话 | WorkSession | 一次开始到结束的工作会话，可含暂停；工时由有效区间聚合 | 与「分段」不是同一层 |
| 分段 | SessionSegment | 未来的活动细分；与 V0.1 计时用 work_interval 有效区间不同 | |
| 依赖 | Dependency | 任务间关系：`blocks` / `depends_on` / `related` / `parallel` | |

---

## 3. 任务状态

| 中文 | English | 定义 | 禁止混用 |
| --- | --- | --- | --- |
| 收件箱 | Inbox | 已捕获未理清 | |
| 理清中 | Clarifying | 正在明确"下一步动作" | |
| 就绪 | Ready | 可执行未排期 | |
| 已排期 | Scheduled | 已进时间块 | |
| 进行中 | Doing | 正在做 | |
| **受阻** | **Blocked** | **我做不了**（缺技能、缺决策、前置未完） | **与 Waiting 是不同状态，不得合并** |
| **等待** | **Waiting** | **等外部**（同事回复、器件、测试、审批） | 同上 |
| 待检查 | Review | 做完待验 | |
| 完成 | Done | 需显式 reopen 才可重新就绪 | |
| 已取消 | Cancelled | 需显式 reopen 才可重新就绪 | |

---

## 4. 执行模式

| 中文 | English | 计入人工工时 | 定义 |
| --- | --- | --- | --- |
| 前台 | FOREGROUND | ✅ | 用户当前工作；同时至多一个 running，允许多个 paused |
| 后台 | BACKGROUND | ❌ | 并行进行、不由用户推进（如 AI 生成文档） |
| 被动 | PASSIVE | ❌ | 机器过程（如 LTspice 仿真跑着） |
| 等待 | WAITING | ❌ | 等外部条件 |

---

## 5. 标签体系（五类）

| 中文 | English | 定义 |
| --- | --- | --- |
| 领域 | Domain | 任务属于什么领域：Hardware / Firmware / Software / Documentation / Management |
| 活动 | Activity | 实际在做什么：Design / Research / Calculation / Coding / Debug / Review / Testing |
| 知识 | Knowledge | 需要什么知识。**唯一有层级的类别**（Electronics → Analog → ADC） |
| 上下文 | Context | 需要什么条件：PC / Internet / OrCAD / Lab / High Focus |
| 汇报 | Report | 专用于周报与 KPA：产品研发 / 技术预研 / 问题分析 / 验证测试 |

> 「上下文」一词有两义：**标签类别**（本表）与 **Context Engine 的上下文**（下节）。写作时须限定，如「Context 类标签」vs「上下文包」。

---

## 6. 能力模型与质量

| 中文 | English | 定义 | 禁止混用 |
| --- | --- | --- | --- |
| 熟练度 | Skill Level | 实验性能力估计；未启用/样本不足时显示未知，默认先显示使用事实 | **必须与置信度并存**，单独给出会得出错误结论 |
| 置信度 | Confidence | 该估计有多少样本支撑 | 同上 |
| 返工 | Rework | 完成后被推翻重做 | |
| 完成质量 | Completion Quality | `normal` / `reworked` / `review_failed` / `partially_done` / `abandoned` | |
| 打断 | Interruption | 计时中插入另一任务，原会话被暂停 | |

> 原愿景的数值示例不作为本轮评分验收；先展示事实与样本，评分启用方式见 R-06。

---

## 7. 上下文引擎

| 中文 | English | 定义 |
| --- | --- | --- |
| 上下文包 | Context Bundle | 本次用户选择的任务/简要背景/知识/历史/决策，不含文件正文 |
| 上下文事实 | ContextFact | 项目级的事实条目（如 `Pt1000 current = 0.2mA`） |
| 已取代 | Superseded | 事实被新值替代后的状态。**旧值保留，不覆盖**（版本化） |
| 决策日志 | Decision Log | 记录决策、理由、日期，供日后追问"当时为什么这么选" |
| 上下文完整度 | Context Completeness | 具体动作的必要输入缺项提示，不计算整体百分比 |

---

## 8. 界面部件

| 中文 | English | 定义 | 禁止混用 |
| --- | --- | --- | --- |
| 主窗 | Main Window | 常规主界面 | |
| 抬头显示 | HUD / OSD | 置顶、透明、鼠标穿透、不抢焦点、无任务栏图标的实时状态窗 | |
| 迷你控制器 | Mini Controller | **可交互**小窗：暂停 / 完成 / 切换 / 快速捕获 | **与 HUD 是两回事**：HUD 穿透不可点，Mini 可点 |
| 托盘 | System Tray | 原生托盘菜单，不依赖任何窗口存活 | |
| 锁定模式 | Locked | HUD 状态：鼠标穿透、不可选、不可拖 | |
| 编辑模式 | Edit | HUD 状态：可移动、可调整大小、可配置 | |

---

## 9. 架构与实现

| 中文 | English | 定义 |
| --- | --- | --- |
| 唯一真相源 | Single Source of Truth | 领域状态只在 Rust 侧存在一份（ADR-006） |
| 命令 | Command | 前端 → Rust 的请求/响应通道 |
| 事件 | Event | Rust → 前端的广播通道 |
| 信封 | Envelope | 事件的统一外层结构 `{event, data_epoch, revision, at, payload}（业务事件；at 为 Unix 毫秒）` |
| 修订号 | revision | 随业务事务提交的持久版本号；tick/心跳不增加（ADR-010） |
| 快照 | Snapshot | `get_snapshot()` 返回的当前状态全量，供窗口挂载时对齐 |
| 状态镜像 | State Mirror | 前端缓存层 `src/services/domainState.ts`，唯一订阅入口 |
| 失效信号 | Invalidation Signal | 领域事件在前端的角色 —— 触发重拉，而非打补丁 |
| 待确认 | needs_review | 不确定区间标记，session 镜像汇总；不占运行槽位，仅该区间确认后计入 |
| 平台层 | Platform Layer | `src-tauri/src/platform/`，Win32 调用的唯一边界（ADR-005） |
| 组合根 | Composition Root | `lib.rs`，唯一装配全部层的地方 |
| 领域层 | Domain Layer | `domain/`，纯类型与规则，无 IO |

---

## 10. 方法论

| 中文 | English | 定义 |
| --- | --- | --- |
| 捕获 | Capture | 把要做的事记下来 |
| 理清 | Clarify | 明确"这是什么、下一步做什么" |
| 计划 | Plan | 拆解、估时、排优先级 |
| 记录 | Track | 记录真实工时、切换、打断 |
| 回顾 | Review | 分析结果、生成报表、提高预测准确度 |
| WBS | Work Breakdown Structure | Goal → Project → Milestone → Task → Action |

## 11. 本轮补充

| 术语 | 定义 |
| --- | --- |
| work_interval 有效工作区间 | 开始/结束事实；暂停关闭区间，恢复新建区间，跨报表范围裁剪 |
| source 来源 | user/rule/ai，记录值从哪里来；采纳 AI 不改变来源 |
| confirmed_at 确认 | 用户确认权，任何已确认值不被后台覆盖 |
| recovering 恢复待确认 | 含不确定区间的恢复记录；可信闭合工时保留，不继续计时 |
| tick_seq | 单次应用 run 的计时展示序号，不是持久业务版本 |
| 暂计 | 正在运行区间截至快照时间的值，与已确认结束时间分列 |
| 未分配 | 同一 kind 权重总和不足 1 的剩余人工工时 |

来源/确认、休眠与历史分类默认值详见 02/06，R-01～R-08 已批准；本轮协议与恢复修订待审核，平台验证与实现尚未完成。

数据代次 `data_epoch`：数据库身份 UUID，创建/恢复时更新；revision 仅在同一代次内比较。`session_version`：计时会话状态版本，过滤暂停前晚到的 tick。`needs_review`：区间级不确定标记，session 汇总反映它；可信闭合区间不因此被排除。

当前产品边界（2026-10-03）：个人工作记录与任务管理，包含能力短板分析和 KPA 工作证据整理；文件读取、OCR、正文提取和正文搜索交给外部 Agent 与配套 skill。见 [范围与导入契约](07-scope-and-agent-import.zh.md)。

资料引用 Reference：标题、路径/URL 与来源定位，不含正文。Agent Import：外部 skill 输出 JSON，校验和预览后逐项采纳。能力画像 Capability Profile：证据、自评与可选模型推断，连接改善措施。KPA Evidence：可回溯的工作时间、成果与证据材料。

实施细节补充见 [08：计时、阶段、AI 与报告快照](08-implementation-contracts.zh.md)。

duration_ms：可信区间时长校验值，与统计起止差一致；归属终点 Attribution Endpoint：开始墙钟＋可信单调增量；检查点 Checkpoint：成功持久化的可信时钟映射；phase：番茄钟 work/break 阶段；report_snapshot：用户确认后不可变的报告内容和事实快照。
