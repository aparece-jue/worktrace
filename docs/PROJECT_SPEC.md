# AI 辅助时间与工作管理工具

> 2026-10-02 评审修订：本文保留原始愿景和版本草案。当前可评估的实现设计见 [修订摘要与待评估清单](superpowers/specs/2026-10-02-worktrace-architecture/06-review-notes.zh.md)、[路线图](superpowers/specs/2026-10-02-worktrace-architecture/05-roadmap.zh.md) 和 [数据模型](superpowers/specs/2026-10-02-worktrace-architecture/02-data-model.zh.md)。修订方案包含 V0.1b、有效工作区间、早期备份/修正/周回顾；尚未获用户批准，不表示功能已实现。

> 文档状态：项目设计草案，整理自“评估AI时间管理工具可行性”对话。
> 本文描述产品定位、建议架构与计划功能，不代表这些功能已经实现；技术选型、字段与版本安排仍需结合仓库现状确认。

## 目录

- [1. 项目简介](#1-项目简介)
- [2. 产品定位](#2-产品定位)
- [3. 核心方法论](#3-核心方法论)
- [4. 总体架构](#4-总体架构)
- [5. 技术原则](#5-技术原则)
- [6. 前端架构](#6-前端架构)
- [7. Rust 后端架构](#7-rust-后端架构)
- [8. 核心对象模型](#8-核心对象模型)
- [9. 任务状态](#9-任务状态)
- [10. Dependency](#10-dependency)
- [11. WorkSession](#11-worksession)
- [12. 并发任务](#12-并发任务)
- [13. SessionSegment](#13-sessionsegment)
- [14. Timer Engine](#14-timer-engine)
- [15. Timer 实现原则](#15-timer-实现原则)
- [16. 标签系统](#16-标签系统)
- [17. 标签比例](#17-标签比例)
- [18. 两种工时统计](#18-两种工时统计)
- [19. Knowledge / Skill Model](#19-knowledge--skill-model)
- [20. 熟练度与置信度](#20-熟练度与置信度)
- [21. TaskKnowledge](#21-taskknowledge)
- [22. 时间预测模型](#22-时间预测模型)
- [23. 打断管理](#23-打断管理)
- [24. 完成质量](#24-完成质量)
- [25. Context Engine](#25-context-engine)
- [26. Project Context](#26-project-context)
- [27. Context Completeness](#27-context-completeness)
- [28. ContextFact](#28-contextfact)
- [29. Decision Log](#29-decision-log)
- [30. 文件与资料](#30-文件与资料)
- [31. 保密数据](#31-保密数据)
- [32. AI Gateway](#32-ai-gateway)
- [33. AI 输出元数据](#33-ai-输出元数据)
- [34. AI 反馈系统](#34-ai-反馈系统)
- [35. AI 质量统计](#35-ai-质量统计)
- [36. 主界面](#36-主界面)
- [37. Today](#37-today)
- [38. HUD / OSD](#38-hud--osd)
- [39. HUD 示例](#39-hud-示例)
- [40. HUD 状态](#40-hud-状态)
- [41. Mini Controller](#41-mini-controller)
- [42. System Tray](#42-system-tray)
- [43. Reports](#43-reports)
- [44. KPA / 工作汇报](#44-kpa--工作汇报)
- [45. 数据导入导出](#45-数据导入导出)
- [46. 备份与恢复](#46-备份与恢复)
- [47. Search](#47-search)
- [48. External Agent](#48-external-agent)
- [49. CAD / 工程数据](#49-cad--工程数据)
- [50. Non-Goals](#50-non-goals)
- [51. V0.1](#51-v01)
- [52. V0.2](#52-v02)
- [53. V0.3](#53-v03)
- [54. V0.4](#54-v04)
- [55. V0.5](#55-v05)
- [56. V1.0](#56-v10)
- [57. 设计原则总结](#57-设计原则总结)
- [58. 最终愿景](#58-最终愿景)


## 1. 项目简介

本项目是一款面向个人长期使用的 **AI 辅助工作管理、时间管理与工作分析桌面工具**。

它不仅用于记录“有哪些任务”，还希望解决以下几个更深层的问题：

- 我现在应该做什么？
- 一个模糊任务应该如何拆解？
- 这个任务预计需要多久？
- 实际花了多久？
- 时间主要花在了哪些项目、工作类型和知识领域？
- 哪些任务经常低估工时？
- 哪些工作最容易被打断？
- 我的能力和熟练度是否正在提高？
- 哪些知识最近使用较多？
- 一周、一个月、一个季度到底完成了什么？
- 如何快速生成周报、月报和 KPA 汇报资料？
- AI 是否真的提高了工作效率？

项目的核心目标不是实现一个新的通用 Agent，而是构建一个：

> **长期理解个人工作方式，并辅助计划、执行、记录、分析和复盘的个人工作系统。**

AI 是增强层，而不是软件存在的前提。

即使没有网络和 AI，本项目依然应该能够作为完整的任务管理、时间记录与统计分析工具使用。

---

## 2. 产品定位

项目可以概括为：

> **AI Personal Work Analytics & Execution Manager**

核心关注五件事情：

1. Capture  
   捕获需要做的事情。

2. Understand  
   理解任务、项目、上下文和知识需求。

3. Plan  
   拆解任务、评估优先级、估算时间并安排执行。

4. Track  
   记录真实工作时间、任务切换、中断和后台任务。

5. Review  
   分析结果、生成报告，并让系统根据历史数据逐渐提高预测准确度。

本项目不追求：

- 替代 IDE
- 替代 OrCAD / Cadence
- 替代 Office
- 替代通用 Agent
- 自己实现完整 MCP 平台
- 自己实现 Zapier / n8n 类工作流平台
- 自动操作所有外部软件

外部执行和复杂自动化应尽量交给专门的 Agent 或自动化工具。

本项目主要负责：

> **状态、任务、时间、上下文、统计、分析和决策辅助。**

---

## 3. 核心方法论

系统不是简单套用某一种时间管理方法，而是组合多个成熟方法，各自负责不同问题。

### 3.1 GTD

GTD 作为整个任务管理系统的骨架。

用于处理：

- Inbox 捕获
- Clarify 理清
- 判断 Project / Task / Action
- 生成下一步行动
- Context 分类
- Waiting / Blocked
- Review

核心问题：

> 这是什么？下一步应该做什么？

---

### 3.2 WBS

WBS 用于复杂任务和项目拆解。

结构：

```text
Goal
└── Project
    └── Milestone
        └── Task
            └── Action
```

AI 可辅助：

- 任务拆解
- 识别遗漏步骤
- 找出依赖关系
- 识别并行工作
- 分析关键路径
- 判断任务粒度是否过大

---

### 3.3 四象限 / 优先级

系统综合：

- Importance
- Urgency
- Deadline
- Dependency
- Project importance
- Goal relevance
- Cost of delay

生成：

```text
P0
P1
P2
```

但原则是：

> AI 建议，人最终确认。

优先级不应只是一个永久静态字段，而可以根据当前情况动态重新评估。

---

### 3.4 OKR / SMART

用于回答：

> 为什么做？

以及：

> 做到什么程度算完成？

主要负责：

- Goal 对齐
- Completion Criteria
- Success Criteria
- Milestone
- Acceptance Criteria

避免任务出现：

```text
“研究一下”
“看看这个”
“处理一下”
```

这种没有明确完成标准的状态。

---

### 3.5 时间块与精力管理

任务不仅考虑时间，还考虑：

- Estimated Duration
- Energy Requirement
- Context
- Deadline
- Priority
- Dependencies
- Available Time

AI 可以辅助安排：

```text
高认知任务 → 高精力时间段
Review / 文档 → 中等精力
整理 / 归档 → 低精力时间
```

最终目标不是简单把日历填满，而是提高：

> **任务与可用时间、精力状态之间的匹配程度。**

---

### 3.6 Daily / Weekly Review

系统必须形成闭环。

每日回顾关注：

- 今天完成什么？
- 哪些任务延期？
- 哪些被打断？
- 哪些任务估时严重错误？
- 明天第一件事是什么？

每周回顾关注：

- 完成率
- 项目进展
- 阻塞任务
- 延期原因
- 工时分布
- 估时误差
- 中断情况
- AI 建议准确度
- 下周容量调整

---

## 4. 总体架构

系统采用：

> **Tauri + Rust + React + Ant Design + SQLite**

总体架构：

```text
                     Desktop Application
                              │
                 ┌────────────┴────────────┐
                 │                         │
             Frontend                  Rust Core
          React + AntD                   Tauri
                 │                         │
        ┌────────┼────────┐        ┌───────┼────────┐
        │        │        │        │       │        │
      Pages      HUD     Mini     Domain  Services Storage
                                  Logic
                                            │
                                         SQLite
                                            │
                                  ┌─────────┴─────────┐
                                  │                   │
                              AI Gateway          Files / Context
                                  │
                           Local / Cloud AI
```

---

## 5. 技术原则

### 5.1 Local First

核心数据默认存储在本地。

主要原因：

- 工作信息可能敏感
- 公司内部资料可能保密
- 时间记录属于长期个人数据
- 软件不能依赖网络才能正常使用
- 本地数据库访问速度稳定

推荐核心存储：

```text
SQLite
```

---

### 5.2 AI Optional

AI 是增强层。

以下功能即使没有 AI，也必须正常工作：

- Task
- Project
- Timer
- WorkSession
- Tag
- Calendar
- TimeBlock
- Reports
- Review
- Search
- Export

AI 不应该成为 Core 的硬依赖。

---

### 5.3 User > Rule > AI

所有信息来源明确区分。

优先级：

```text
User Confirmed
    ↓
System Rule
    ↓
AI Prediction
```

例如：

```text
priority:
  value: P1
  source: user
```

AI 后续重新分析时，不应覆盖用户确认的信息。

---

### 5.4 Event Driven

系统内部建议使用事件驱动方式连接模块。

例如：

```text
task.completed
session.started
session.finished
task.updated
report.generated
project.updated
```

事件产生后：

```text
task.completed
    ↓
结束 Session
    ↓
更新工时
    ↓
更新统计
    ↓
更新知识数据
    ↓
进入 Review
```

这属于业务状态流转，不需要做成复杂工作流引擎。

---

## 6. 前端架构

推荐目录：

```text
src/
├── app/
├── pages/
├── features/
│   ├── inbox/
│   ├── tasks/
│   ├── projects/
│   ├── timer/
│   ├── calendar/
│   ├── reports/
│   ├── review/
│   ├── knowledge/
│   └── settings/
├── components/
├── hooks/
├── services/
└── types/
```

重点采用：

> **按业务功能拆分，而不是过度抽象。**

避免形成：

```text
Controller
→ Service
→ Manager
→ Provider
→ Repository
→ Adapter
```

层层包装。

项目应优先保持：

- 简单
- 可读
- 可调试
- 易维护

---

## 7. Rust 后端架构

推荐：

```text
src-tauri/src/
├── main.rs
│
├── commands/
│
├── domain/
│   ├── task/
│   ├── project/
│   ├── session/
│   ├── tag/
│   ├── knowledge/
│   ├── review/
│   └── report/
│
├── services/
│   ├── timer.rs
│   ├── statistics.rs
│   ├── context.rs
│   ├── search.rs
│   └── ai.rs
│
├── storage/
├── events/
└── models/
```

React 主要负责：

> 展示和交互。

Rust 负责：

> 核心状态、计时、统计、数据库、Context 和 AI 调用。

---

## 8. 核心对象模型

### 8.1 Goal

代表长期目标。

例如：

```text
完成工业 IO 控制板开发
```

---

### 8.2 Project

Project 是长期上下文容器，而不仅是任务分组。

Project 可包含：

```text
目标
约束
关键决策
任务
里程碑
文档
知识
历史
问题
相关文件
```

例如：

```text
Project:
Industrial IO Board
```

---

### 8.3 Milestone

代表项目阶段结果。

例如：

```text
原理图设计冻结
PCB Release
样板调试完成
```

---

### 8.4 Task

Task 是主要管理对象。

示例字段：

```text
id
title
description

goal_id
project_id
milestone_id
parent_task_id

status
priority

importance
urgency

deadline

scheduled_start
scheduled_end

estimated_duration
planned_duration
actual_duration

energy_required
difficulty

completion_criteria

created_at
updated_at
```

---

## 9. 任务状态

推荐：

```text
Inbox
Clarifying
Ready
Scheduled
Doing
Blocked
Waiting
Review
Done
Cancelled
```

特别需要区别：

```text
Blocked
```

和：

```text
Waiting
```

因为：

> 没做

并不代表：

> 不想做。

可能是等待：

- 同事回复
- 器件
- 测试
- 审批
- 前置任务

---

## 10. Dependency

任务支持依赖关系。

例如：

```text
A → B
```

表示：

> A 完成后 B 才能开始。

还可支持：

```text
Blocks
Depends On
Related
Can Run In Parallel
```

未来可用于：

- Critical Path
- 延期影响分析
- 自动调度

---

## 11. WorkSession

WorkSession 是整个工时统计系统的核心。

一个 Task 可以存在多个：

```text
Task
└── WorkSession
    ├── Session 1
    ├── Session 2
    └── Session 3
```

例如：

```text
Task:
AD5422 Review

09:00–09:35
14:20–15:10
16:30–17:00
```

最终实际工时：

```text
115 min
```

而不是：

```text
17:00 - 09:00
```

---

## 12. 并发任务

系统允许同时存在多个运行任务。

例如：

```text
Foreground
原理图设计

Background
AI生成设计文档

Passive
LTspice 仿真
```

Execution Mode：

```text
FOREGROUND
BACKGROUND
PASSIVE
WAITING
```

必须区分：

```text
Elapsed Time
Human Effort
Machine / AI Process Time
```

否则：

```text
1h 设计
+
1h AI
```

不能错误统计成：

```text
2h 人工工时
```

---

## 13. SessionSegment

未来可以增加：

```text
WorkSession
└── SessionSegment
```

例如：

```text
09:00–09:40 Research
09:40–10:30 Design
10:30–10:50 Calculation
```

这样可以实现更准确的：

> Activity 工时统计。

V1 可以先不实现。

---

## 14. Timer Engine

统一 Timer Engine 支持：

```text
Stopwatch
Countdown
Pomodoro
Time Block
```

### Stopwatch

正计时。

### Countdown

按预计时间倒计时。

### Pomodoro

支持：

```text
25 / 5
50 / 10
90 / 20
Custom
```

### Time Block

例如：

```text
14:00–15:30
```

---

## 15. Timer 实现原则

不要依赖：

```js
remaining -= 1;
```

而应该保存：

```text
started_at
target_end
paused_duration
```

显示时计算：

```text
remaining =
target_end - now
```

避免：

- 系统休眠
- JS 卡顿
- 窗口暂停
- 页面隐藏

造成计时漂移。

---

## 16. 标签系统

标签不是简单的一套字符串。

推荐五类：

```text
Domain
Activity
Knowledge
Context
Report
```

---

### 16.1 Domain

任务属于什么领域。

例如：

```text
Hardware
Firmware
Software
Documentation
Management
```

---

### 16.2 Activity

实际在做什么。

例如：

```text
Design
Research
Calculation
Coding
Debug
Review
Testing
Documentation
Communication
```

---

### 16.3 Knowledge

任务需要什么知识。

例如：

```text
Analog
ADC
Filtering
Rust
React
Tauri
RS485
Power Electronics
```

支持层级：

```text
Electronics
├── Analog
│   ├── ADC
│   ├── OpAmp
│   └── Filtering
│
└── Power
    ├── Buck
    └── Boost
```

---

### 16.4 Context

任务需要哪些条件。

例如：

```text
PC
Internet
OrCAD
VS Code
Lab
High Focus
Low Energy
Phone
```

---

### 16.5 Report

专门用于周报 / KPA。

例如：

```text
产品研发
技术预研
问题分析
验证测试
项目支持
文档交付
```

这样内部详细标签不会直接污染正式汇报。

---

## 17. 标签比例

Task 可以对某些标签保存权重。

例如：

```text
Activity

Design       60%
Research     20%
Review       20%
```

或者：

```text
Knowledge

ADC                30%
Analog Frontend    25%
Filtering          20%
Protection         15%
Industrial AI      10%
```

工时可以按权重分配。

---

## 18. 两种工时统计

### 关联时长

只要任务带有标签，就计入整个任务时间。

例如：

```text
Task = 2h

ADC
ADS1118
Analog
```

则：

```text
ADC      2h
ADS1118  2h
Analog   2h
```

适合：

> 我参与了多少 ADC 相关工作？

---

### 加权工时

按照标签比例分配。

例如：

```text
Task = 2h

ADC        50%
Filter     30%
ESD        20%
```

得到：

```text
ADC      1.0h
Filter   0.6h
ESD      0.4h
```

适合：

> 真正投入 ADC 设计的时间是多少？

---

## 19. Knowledge / Skill Model

系统长期建立用户自己的能力模型。

例如：

```text
Knowledge:
Rust / Ownership

experience_hours
recent_hours
application_count
learning_count

skill_level
confidence

estimate_accuracy

last_used_at
```

熟练度不能只根据：

```text
累计时间
```

而应综合：

- 经验时间
- 最近使用
- 任务难度
- 独立完成情况
- 返工率
- Debug 时间
- 估时误差
- 学习型任务
- 使用频率

---

## 20. 熟练度与置信度

应同时保存：

```text
skill_level
confidence
```

例如：

```text
Vulkan

Skill:      0.72
Confidence: 0.21
```

说明：

> 当前表现不错，但样本不足。

避免做少数几个任务就给出过高评价。

---

## 21. TaskKnowledge

Task 和 Knowledge 的关系可以包含：

```text
knowledge_id

weight

required_level

used

learning_gain

confidence

source
```

这样可以区分：

```text
使用已有知识
```

和：

```text
真正学习新知识
```

---

## 22. 时间预测模型

每个 Task 建议区分：

```text
estimated_duration
planned_duration
actual_duration
```

例如：

```text
AI Estimate:  90 min
User Plan:    60 min
Actual:      112 min
```

长期以后可以分析：

```text
PCB任务平均低估 42%
Rust Debug平均误差 +18%
文档Review平均误差 +7%
```

AI 可利用这些历史数据提高后续估时。

---

## 23. 打断管理

执行过程中支持：

```text
Interrupt
```

例如：

```text
正在：
AIAO 原理图

临时任务：
回复供应商问题
```

操作：

```text
暂停当前 Session
→ 创建临时 Task
→ 开始新 Session
→ 记录 interruption
```

后续统计：

```text
本周被打断 14 次
损失 3h12m
```

---

## 24. 完成质量

任务不能只有：

```text
Done
```

建议增加结果质量。

例如：

```text
Normal
Reworked
Review Failed
Partially Done
Abandoned
```

否则系统可能产生错误结论：

> 完成快 = 熟练度高

但实际可能：

> 完成快 + 后续严重返工。

---

## 25. Context Engine

Context Engine 用于解决：

> 用户只说一句任务描述时，AI缺乏足够背景。

例如：

```text
“完成 AIAO 原理图”
```

AI不能只看到这一句话。

它应该获取：

```text
Task Context
+
Project Context
+
Related Documents
+
Knowledge
+
History
+
Decisions
```

形成 Context Bundle。

---

## 26. Project Context

Project 应保存：

```text
目标
设计约束
关键参数
架构
决策
文档
历史任务
未解决问题
术语
知识
```

例如：

```text
Project:
AIAO

Decision:
ADC = ADS1118

Decision:
DAC = AD5422

Constraint:
24V supply

Constraint:
2 AI + 2 AO
```

---

## 27. Context Completeness

AI 可以对任务上下文完整程度进行判断。

例如：

```text
Goal           ✓
Project        ✓
Input          ✓
Output         ✓
Constraints    ✓
Dependency     ✓
Acceptance     ?
Deadline       ✓
```

得到：

```text
Context Completeness: 88%
```

只有真正缺失重要信息时才询问用户。

避免每个任务都弹出大量表单。

---

## 28. ContextFact

建议增加：

```text
ContextFact
```

结构：

```text
id
project_id

key
value

source_type
source_id

confidence
security_level

created_at
superseded_by
```

例如：

```text
Pt1000 current = 0.2mA
```

后续修改：

```text
0.2mA → superseded
0.3mA → current
```

这样 AI 不会一直使用过期数据。

---

## 29. Decision Log

工程类任务尤其需要：

```text
Decision
```

例如：

```text
Decision:
AO 使用 AD5422

Reason:
减少外围电路复杂度

Date:
2026-xx-xx
```

未来回顾时能够知道：

> 为什么当时这么选。

---

## 30. 文件与资料

支持关联：

```text
PDF
Word
Excel
Markdown
Images
CSV
JSON
Netlist
Code
Log
```

核心目标是：

> 获取上下文。

不是：

> 自己实现一个 Office 或 CAD。

---

## 31. 保密数据

文件和 Context 建议支持：

```text
PUBLIC
INTERNAL
CONFIDENTIAL
STRICT_LOCAL
```

例如：

```text
STRICT_LOCAL
```

意味着：

- 不允许发送到云 AI
- 不允许自动外发
- 只允许本地处理

---

## 32. AI Gateway

Core 不直接调用具体厂商。

统一接口：

```text
AI Gateway
├── clarify_task()
├── decompose_task()
├── estimate_duration()
├── suggest_tags()
├── suggest_priority()
├── summarize_report()
├── analyze_context()
└── review_week()
```

底层以后可接：

```text
Cloud AI
Local AI
```

---

## 33. AI 输出元数据

AI 输出应包含：

```text
value
source
confidence
confirmed
created_at
```

例如：

```text
estimated_duration:
  value: 85
  source: ai
  confidence: 0.76
```

---

## 34. AI 反馈系统

用户可以快速反馈：

```text
太细
太粗
估时太长
估时太短
标签错误
项目分类错误
优先级不合理
```

用于改进后续预测。

---

## 35. AI 质量统计

需要真正衡量：

> AI 是否越来越准？

例如统计：

```text
估时误差
标签接受率
任务拆分接受率
优先级修改率
AI建议采纳率
```

例如：

```text
AI拆分：
72% 直接接受
18% 修改
10% 删除
```

---

## 36. 主界面

推荐核心页面：

```text
Inbox
Today
Projects
Calendar
Review
Reports
Knowledge
Settings
```

---

## 37. Today

Today 是核心执行页面。

显示：

```text
今天计划
当前任务
下一任务
时间块
并行后台任务
今日已工作
剩余任务
```

例如：

```text
09:30 ADS1118 Review
10:30 Pt1000 Calculation
13:30 Documentation
```

---

## 38. HUD / OSD

需要一个类似显卡性能监控的实时窗口。

特点：

```text
Always On Top
Transparent
Click Through
No Taskbar Icon
No Focus
Realtime
```

用途：

> 用户工作时无需打开主窗口，也能看到当前状态。

---

## 39. HUD 示例

```text
AIAO 原理图               P1

01:17:34 / 02:00:00
████████████░░░ 64%

Today
5h42m

AI Docs
72%

LTspice
Running
```

---

## 40. HUD 状态

### Locked

```text
鼠标穿透
不可选择
不可拖动
```

### Edit

```text
允许移动
允许调整大小
允许配置
```

---

## 41. Mini Controller

区别于 HUD。

Mini Window 可以交互：

```text
当前任务
Timer
Pause
Complete
Switch
Quick Capture
```

---

## 42. System Tray

程序最小化后：

```text
隐藏任务栏图标
保留 Tray
后台继续运行
```

Tray 提供：

```text
Current Task
Pause
Complete
Quick Capture
HUD
Open
Exit
```

---

## 43. Reports

Reports 用于：

- 工时
- Weekly Review
- Monthly Report
- Quarterly Report
- KPA

时间范围：

```text
Daily
Weekly
Monthly
Quarterly
Custom
```

筛选：

```text
Project
Goal
Domain
Activity
Knowledge
Report Tag
Task
Milestone
```

---

## 44. KPA / 工作汇报

不能只输出：

```text
本季度 510h
```

而应该组合：

```text
时间投入
+
完成任务
+
里程碑
+
项目成果
+
问题
+
改进
```

例如：

```text
工业IO项目

投入:
126h

完成:
- DIDO输入模块
- PWM输出模块
- AIAO前端
- RS232隔离模块

里程碑:
4 / 5
```

AI 可以基于这些真实数据生成：

> 周报、月报和 KPA 草稿。

---

## 45. 数据导入导出

长期个人数据必须保证可迁移。

至少支持：

```text
JSON
CSV
Markdown
```

以后可考虑：

```text
Excel
PDF Report
```

用户不能被锁死在数据库内部。

---

## 46. 备份与恢复

必须设计：

```text
Automatic Backup
Snapshot
Database Migration
Crash Recovery
```

避免长期积累的数据因为一次数据库损坏丢失。

---

## 47. Search

统一搜索：

```text
Task
Project
Document
Decision
Knowledge
Context
Session
```

例如：

```text
“所有和 RS485 隔离有关的内容”
```

返回：

```text
任务
文档
设计决策
历史记录
知识
```

---

## 48. External Agent

本项目不实现通用 Agent。

只需要提供未来可扩展接口：

```text
task.created
task.updated
task.completed
project.updated
report.generated
```

以及基础数据 API。

Agent 可自行处理：

```text
OrCAD
Word
VS Code
Email
Git
Browser
```

明确边界：

```text
This App:
Plan / Track / Analyze / Review

Agent:
Execute / Automate
```

---

## 49. CAD / 工程数据

软件本体不负责控制 OrCAD。

可以允许导入：

```text
Netlist
PDF
BOM
JSON
Images
```

如果未来需要高级集成，可额外提供：

```text
export_for_ai.tcl
```

作为独立辅助脚本。

不是核心功能。

---

## 50. Non-Goals

明确暂时不做：

```text
Plugin Marketplace
Generic Workflow Engine
General Agent Platform
Full CAD Automation
IDE Replacement
Office Replacement
Universal Automation Platform
```

防止产品边界不断膨胀。

---

## 51. V0.1

优先建立底座。

实现：

```text
Task
Project
Basic Tag
Timer
WorkSession
SQLite
Today
Inbox
Tray
Basic HUD
```

---

## 52. V0.2

增加：

```text
Knowledge Tag
Weighted Tag
Reports
TimeBlock
Pomodoro
Interruption
Multi Session
```

---

## 53. V0.3

加入：

```text
AI Task Clarification
AI Task Breakdown
AI Estimate
AI Tagging
AI Priority Suggestion
```

---

## 54. V0.4

加入：

```text
Context Engine
Project Context
Decision Log
ContextFact
Document Association
```

---

## 55. V0.5

加入：

```text
Weekly Review
KPA Reports
Skill Model
Knowledge Analytics
Estimate Accuracy
AI Feedback
```

---

## 56. V1.0

目标：

```text
稳定数据结构
稳定数据库迁移
稳定备份
成熟任务流程
可靠计时
成熟工时统计
可用AI辅助
完整Review
稳定HUD
可靠Report
```

达到：

> 可以真正作为日常工作主力工具长期使用。

---

## 57. 设计原则总结

整个项目始终遵守以下原则：

#### 1. 管理不能比工作本身更麻烦

用户应该尽量少填写信息。

AI 自动提取和建议。

---

#### 2. AI 建议，人决定

AI 不擅自：

```text
删除任务
修改重要决策
发送数据
改变截止日期
```

---

#### 3. 数据必须可解释

AI 的输出尽量有：

```text
来源
置信度
修改记录
```

---

#### 4. 数据必须属于用户

提供：

```text
本地存储
备份
导出
迁移
```

---

#### 5. 长期使用优先

不是追求“第一天看起来很聪明”。

而是：

> 使用一年后，比第一天更懂用户。

---

#### 6. 不重复制造 Agent

任务执行交给更专业的 Agent。

本项目关注：

```text
Capture
Understand
Plan
Track
Analyze
Review
```

---

## 58. 最终愿景

随着使用时间增加，系统逐渐能够回答：

```text
我今天应该做什么？

这个任务大概需要多久？

我为什么总是低估某类任务？

最近时间主要花在哪里？

哪些工作最容易打断我？

我的哪些知识使用最频繁？

我的哪些能力正在提高？

哪个项目消耗时间最多？

哪些工作投入很大但产出很低？

AI到底节省了多少时间？

这个季度我完成了什么？
```

最终希望达到：

> 用户只需要正常工作，系统自动形成完整的任务历史、工时记录、知识图谱、能力变化、项目进度和工作总结。

它不是简单的 Todo List，也不是 Agent 平台。

它更像一个：

> **持续记录、理解和优化个人工作过程的 AI 工作操作系统。**
