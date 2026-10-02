# Worktrace 模块分解

| 项 | 值 |
| --- | --- |
| 文档状态 | 设计草案（待评审） |
| 日期 | 2026-10-02 |
| 上游 | [`00-architecture.zh.md`](00-architecture.zh.md) 总体架构 |
| 用途 | 本文是**后续每份 spec / plan 的派发源**：每个模块对应一份独立的 spec → plan → 实现 |
| 英文版 | [`01-module-breakdown.en.md`](01-module-breakdown.en.md) |

---

## 1. 总览

14 个模块，按层归位。依赖方向恒为 上 → 下，不允许反向。

| 组 | 模块 | 层 | 里程碑 |
| --- | --- | --- | --- |
| 基础设施 | M00 平台层 | L0 `platform/` | V0.1（单实例/托盘） |
| | M01 存储层 | L2 `storage/` | V0.1 |
| | M03 事件总线 | 横切 `events/` | V0.1 |
| 领域 | M02 领域模型 | L1 `domain/` | V0.1 |
| 服务 | M04 Timer Engine | L3 `services/timer.rs` | V0.1 |
| | M05 WorkSession | L3 `services/session.rs` | V0.1 |
| | M06 统计引擎 | L3 `services/statistics.rs` | V0.2 |
| | M07 报表与 KPA | L3 `services/report.rs` | V0.2 |
| | M08 Knowledge / 估时预测 | L3 `services/knowledge.rs` | V0.2 |
| | M09 Context Engine | L3 `services/context.rs` | V0.4 |
| | M10 AI Gateway | L3 `services/ai/` | V0.3 |
| | M13 搜索 | L3 `services/search.rs` | V0.2 |
| 外壳 | M11 窗口系统 | L0 + `features/hud`·`features/mini` | V0.1（托盘 + Basic HUD） |
| | M12 前端骨架 | React `app/` + `features/` | V0.1（Today + Inbox） |

---

## 2. 模块卡

### M00 平台层 `platform/`

| 项 | 内容 |
| --- | --- |
| **职责** | 封装全部 Win32 与操作系统交互：窗口扩展样式（置顶/透明/穿透/无焦点/无任务栏图标）、托盘、全局热键、休眠与锁屏检测、单实例 |
| **不负责** | 任何业务判断；任何领域类型 |
| **输入** | 窗口句柄、用户点击托盘项 |
| **输出** | 原生窗口状态变更、系统事件（`system.slept` / `system.woke` / `system.locked`） |
| **依赖** | 无 |
| **SPEC** | §38–42 |
| **硬规则** | 只有 `services/` 及以上可调用；`domain/` 与 `storage/` 不得 import |

### M01 存储层 `storage/`

| 项 | 内容 |
| --- | --- |
| **职责** | rusqlite 连接与事务、schema 与迁移（`PRAGMA user_version` 步进）、备份（`VACUUM INTO`）、各实体仓储实现、SQL 聚合统计查询 |
| **不负责** | 业务规则；事务边界以外的编排 |
| **输入** | 数据库文件路径（由组合根注入）、领域类型 |
| **输出** | 查询结果、持久化副作用 |
| **依赖** | M02（领域类型）、M03（事件） |
| **SPEC** | §8–13、§45、§46 |
| **关键约束** | 表内**不得**出现 `remaining` / `elapsed` 之类派生字段（见 02 数据模型） |

### M02 领域模型 `domain/`

| 项 | 内容 |
| --- | --- |
| **职责** | 纯类型与规则：Goal / Project / Milestone / Task / Dependency / Tag / Knowledge / WorkSession / ContextFact / Decision；Task 状态机；WorkSession 生命周期不变量 |
| **不负责** | 任何 IO；任何数据库或平台调用 |
| **输入** | 值对象 |
| **输出** | 校验结果、状态跃迁结果、领域事件 |
| **依赖** | M03 |
| **SPEC** | §8–13、§16–22、§24 |
| **硬规则** | 必须能在无数据库、无 Windows 的环境下跑单元测试 |

### M03 事件总线 `events/`

| 项 | 内容 |
| --- | --- |
| **职责** | 领域事件类型定义、内部订阅分发、全局单调 `revision`、向所有窗口广播、`get_snapshot()` 支撑 |
| **不负责** | 业务逻辑；事件的持久化（V0.1 不做事件溯源） |
| **输入** | 各层发布的事件 |
| **输出** | 订阅者回调、IPC 广播 |
| **依赖** | 无 |
| **SPEC** | §5.4 |
| **关键约束** | 只承载**业务状态流转**，不得演化成工作流引擎（SPEC §50） |

### M04 Timer Engine `services/timer.rs`

| 项 | 内容 |
| --- | --- |
| **职责** | 统一计时：Stopwatch / Countdown / Pomodoro / Time Block；权威时钟；约 1 Hz 的 `timer.tick` 广播；系统休眠后的时间校正 |
| **不负责** | 工时归属与统计（属 M05/M06） |
| **输入** | 目标任务、计时模式、目标时长 |
| **输出** | `timer.tick` 事件、计时状态查询 |
| **依赖** | M02、M03、M00（休眠检测） |
| **SPEC** | §14、§15 |
| **硬规则** | **不得**用 `remaining -= 1` 式累减。只存 `started_at` / `target_end` / `paused_total_ms`，显示值一律现算 |

### M05 WorkSession `services/session.rs`

| 项 | 内容 |
| --- | --- |
| **职责** | WorkSession 生命周期（开始/暂停/恢复/结束）；并发任务与四种执行模式（FOREGROUND / BACKGROUND / PASSIVE / WAITING）；打断处理；完成质量记录；崩溃后的 `needs_review` 标记 |
| **不负责** | 计时推进（属 M04）；工时聚合（属 M06） |
| **输入** | 任务 ID、执行模式、打断来源 |
| **输出** | `session.started` / `session.finished` / `session.interrupted` 事件 |
| **依赖** | M01、M02、M04、M03 |
| **SPEC** | §11–13、§23、§24 |
| **不变量** | 至多一个 FOREGROUND session（在服务层强制，不加 schema 唯一约束） |

### M06 统计引擎 `services/statistics.rs`

| 项 | 内容 |
| --- | --- |
| **职责** | 两种工时口径：关联时长（标签全额计入）与加权工时（按权重分配）；按 Project / Domain / Activity / Knowledge / Report 标签的分布；估时误差统计 |
| **不负责** | 报表排版与导出（属 M07） |
| **输入** | 时间范围、筛选条件 |
| **输出** | 聚合结果 DTO |
| **依赖** | M01（SQL 聚合）、M02 |
| **SPEC** | §17、§18、§22 |
| **关键约束** | 聚合下推到 SQL，禁止把整表拉进内存计算 |

### M07 报表与 KPA `services/report.rs`

| 项 | 内容 |
| --- | --- |
| **职责** | Daily / Weekly / Monthly / Quarterly / Custom 报表；KPA 汇报资料（时间投入 + 完成任务 + 里程碑 + 问题 + 改进）；导出 JSON / CSV / Markdown |
| **不负责** | 底层统计口径（属 M06） |
| **输入** | 时间范围、筛选条件、导出格式 |
| **输出** | `report.generated` 事件、导出文件 |
| **依赖** | M06、M02 |
| **SPEC** | §43、§44、§45 |

### M08 Knowledge / 估时预测 `services/knowledge.rs`

| 项 | 内容 |
| --- | --- |
| **职责** | 知识标签的熟练度与置信度模型；TaskKnowledge 关系；基于历史数据的估时误差分析与预测 |
| **不负责** | AI 调用（属 M10，本模块只产出可喂给 AI 的结构化特征） |
| **输入** | 历史 session、任务难度、返工率 |
| **输出** | `skill_level` / `confidence` / 估时建议 |
| **依赖** | M01、M02、M06 |
| **SPEC** | §19–22 |

### M09 Context Engine `services/context.rs`

| 项 | 内容 |
| --- | --- |
| **职责** | Project Context 汇总；ContextFact（带 `superseded_by` 的版本化事实）；Decision Log；文件与资料关联；保密等级（PUBLIC / INTERNAL / CONFIDENTIAL / STRICT_LOCAL）；Context Completeness 评估 |
| **不负责** | 实际 AI 调用（属 M10） |
| **输入** | 任务/项目 ID、关联文件 |
| **输出** | Context Bundle |
| **依赖** | M01、M02 |
| **SPEC** | §25–31 |
| **关键约束** | `STRICT_LOCAL` 的数据**不得**进入任何外发路径（与 M10 的接口契约） |

### M10 AI Gateway `services/ai/`

| 项 | 内容 |
| --- | --- |
| **职责** | 统一 AI 接口（`clarify_task` / `decompose_task` / `estimate_duration` / `suggest_tags` / `suggest_priority` / `summarize_report` / `analyze_context` / `review_week`）；AI 输出元数据（value / source / confidence / confirmed）；用户反馈收集；AI 质量统计 |
| **不负责** | 具体厂商 SDK 泄漏到上层；不得成为 Core 的硬依赖 |
| **输入** | Context Bundle |
| **输出** | 带元数据的建议 |
| **依赖** | M09、M02 |
| **SPEC** | §5.2、§5.3、§32–35 |
| **硬规则** | 接口与实现分离；AI 不可用时所有调用方必须能降级；AI 建议**不得**覆盖 `source = user` 的字段 |

### M11 窗口系统

| 项 | 内容 |
| --- | --- |
| **职责** | 主窗 / HUD / Mini 三窗口的创建与生命周期；HUD 的 Locked 与 Edit 模式；托盘；单实例唤起 |
| **不负责** | 窗口内展示什么（属 M12） |
| **输入** | 用户操作、系统事件 |
| **输出** | 原生窗口状态、托盘菜单动作 |
| **依赖** | M00、M03 |
| **SPEC** | §38–42 |
| **关键约束** | HUD / Mini 使用独立 Vite 入口；窗口不得持有业务状态 |

### M12 前端骨架

| 项 | 内容 |
| --- | --- |
| **职责** | 应用外壳（主题、路由、布局）；`features/` 各业务页；`services/domainState.ts` 状态镜像层；`components/` 跨 feature 通用组件；`types/` 自动生成 |
| **不负责** | 任何业务规则；任何本地持久化 |
| **输入** | command 响应、event 推送 |
| **输出** | UI、command 调用 |
| **依赖** | Rust 侧全部（经 IPC） |
| **SPEC** | §6、§36、§37 |
| **硬规则** | `features/` 之间禁止互相 import；禁止页面自行 `listen()` |

### M13 搜索 `services/search.rs`

| 项 | 内容 |
| --- | --- |
| **职责** | 跨 Task / Project / Document / Decision / Knowledge / Context / Session 的统一检索 |
| **不负责** | 语义检索（V0.1–V1.0 均为 SQL 全文/前缀匹配） |
| **输入** | 查询串、范围过滤 |
| **输出** | 分类结果集 |
| **依赖** | M01、M02 |
| **SPEC** | §47 |
| **待定** | 是否启用 SQLite FTS5，在 M13 的 spec 中决定 |

---

## 3. 依赖图与实现顺序

```
M00 platform ─┐
M03 events   ─┼─→ M02 domain ─→ M01 storage ─→ M04 timer ─→ M05 session
              │                                      │           │
              │                                      └─────┬─────┘
              │                                            ▼
              │                            M06 statistics ─→ M07 report
              │                                   │      └─→ M08 knowledge
              │                                   └─→ M13 search
              └─→ M11 window system                     M09 context ─→ M10 ai
                                                         │
M12 frontend skeleton ←──────────────────────────────────┘（经 IPC，与全部相关）
```

### 建议实现顺序（V0.1 打通）

1. **M00**（仅单实例 + 托盘最小集）
2. **M03** 事件总线骨架（类型 + revision + 广播）
3. **M02** 领域模型（Task / Project / Tag / WorkSession）
4. **M01** 存储层（schema + 迁移 + 仓储）
5. **M04** Timer Engine
6. **M05** WorkSession
7. **M12** 前端骨架（Today + Inbox）
8. **M11** 窗口系统（托盘 + Basic HUD）

### 可并行项

- **M09 → M10**（Context Engine 与 AI Gateway）只依赖 M01/M02，可在 V0.2 期间先行
- **M13 搜索** 依赖 M01/M02，与 M06/M07/M08 互不依赖，可并行
- **M12 前端骨架** 的页面外壳（主题/布局/路由）可在 M01 完成前先搭，用 mock 数据

### 严格串行的关键路径

`M02 → M01 → M04 → M05 → M06 → M07` —— 这条链上任一环滑期，下游全部顺延。排计划时优先保它。
