# Worktrace 数据模型

| 项 | 值 |
| --- | --- |
| 文档状态 | 设计草案（待评审） |
| 日期 | 2026-10-02 |
| 上游 | [`00-architecture.zh.md`](00-architecture.zh.md)、[`01-module-breakdown.zh.md`](01-module-breakdown.zh.md) |
| 落地模块 | M01 存储层、M02 领域模型 |
| 英文版 | [`02-data-model.en.md`](02-data-model.en.md) |

> 本文给出实体关系、表结构与状态机。**完整 DDL 与迁移脚本在 M01 存储层的 spec 中给出。**

---

## 1. 实体关系

```
Goal 1──n Project 1──n Milestone 1──n Task
                                        │
        Task ──self──→ Task              │  parent_task_id（WBS 层级）
        Task n──n Task  (Dependency)     │  blocks / depends_on / related / parallel
        Task 1──n WorkSession ───────────┘
        WorkSession 1──n SessionSegment  (V1 起)
        Task n──n Tag   (带 weight)
        Tag  ──self──→ Tag               (仅 kind = knowledge 有层级)
        Project 1──n ContextFact
        Project 1──n Decision
        Project 1──n Document ──n Task   (多对多)
        Tag(kind=knowledge) 1──1 KnowledgeStat
        Task n──n KnowledgeStat          (经 TaskKnowledge)
```

---

## 2. 表结构

### 2.0 表的版本归属

本设计覆盖产品全周期，但**不是所有表都在 V0.1 建**：

| 表 | 引入版本 |
| --- | --- |
| `goal` `project` `milestone` `task` `work_session` `tag` `task_tag` | V0.1 |
| `knowledge_stat` `task_knowledge` | V0.2 |
| `context_fact` `decision` `document` `task_document` | V0.4 |

**迁移策略**：V0.1 只创建 V0.1 的表，后续版本通过迁移**新增**表，**不一次性建全**。

理由：未使用的表会随需求演进而需要改动，提前建表等于提前冻结还没想清楚的结构 —— 那是 SPEC §57 反对的投机性设计。字段同理：`task` 表中服务于 V0.3 AI 元数据的 `priority_json` / `estimated_json`，允许在 V0.1 先建为普通列、到 V0.3 再迁移为信封结构。

### 2.1 目标与项目

```sql
goal(
  id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT,
  status TEXT NOT NULL,                    -- active / done / dropped
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)

project(
  id TEXT PRIMARY KEY,
  goal_id TEXT REFERENCES goal(id),
  name TEXT NOT NULL, description TEXT,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)

milestone(
  id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES project(id),
  title TEXT NOT NULL, status TEXT NOT NULL,
  due_at INTEGER, done_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)
```

### 2.2 Task

```sql
task(
  id TEXT PRIMARY KEY,
  project_id TEXT REFERENCES project(id),
  milestone_id TEXT REFERENCES milestone(id),
  parent_task_id TEXT REFERENCES task(id),   -- WBS 层级；Action 就是叶子 Task
  title TEXT NOT NULL, description TEXT,
  status TEXT NOT NULL,                      -- 见 §3 状态机

  priority_json TEXT,                        -- {"value":"P1","source":"user","confidence":null,"confirmed_at":...}
  estimated_json TEXT,                       -- 同上信封，承载 SPEC §22 的 estimated_duration

  importance INTEGER, urgency INTEGER,       -- 0..3
  deadline INTEGER,
  scheduled_start INTEGER, scheduled_end INTEGER,
  planned_duration INTEGER,                  -- 分钟；用户手动计划值

  energy_required INTEGER, difficulty INTEGER,
  completion_criteria TEXT,
  quality TEXT,                              -- 见 §5 完成质量
  needs_review INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
)
```

**关于 `actual_duration`**：SPEC §8.4 的字段表里列了它，**本设计刻意不存**。实际工时一律由 `work_session` 聚合得出：

```sql
SELECT SUM(ended_at - started_at - paused_total_ms) FROM work_session WHERE task_id = ?
```

理由：存一份就等于有了第二个真相源，必然出现"缓存与明细不一致"。`work_session(task_id)` 上有索引，个人规模（十年也就几十万行）下聚合开销可忽略。若将来报表成为瓶颈，再加**物化汇总表**并由 M01 在 session 结束时维护，而不是在 `task` 上挂一个语义模糊的列。此差异已记入 §7。

### 2.3 WorkSession

```sql
work_session(
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES task(id),
  mode TEXT NOT NULL,                        -- FOREGROUND / BACKGROUND / PASSIVE / WAITING
  started_at INTEGER NOT NULL,
  ended_at INTEGER,                          -- NULL = 进行中
  paused_total_ms INTEGER NOT NULL DEFAULT 0,
  target_end INTEGER,                        -- 倒计时/Pomodoro 的目标时刻；正计时为 NULL
  interruption_of TEXT REFERENCES work_session(id),
  quality TEXT,
  needs_review INTEGER NOT NULL DEFAULT 0,   -- 崩溃恢复后置 1，见 §6
  last_heartbeat_at INTEGER,                 -- 运行中每 30s 更新
  created_at INTEGER NOT NULL
)
```

**这张表里没有 `remaining`、没有 `elapsed`。** 这是 SPEC §15 的硬要求在 schema 层面的体现：只存时刻，显示值一律现算。

```rust
elapsed_ms   = now - started_at - paused_total_ms
remaining_ms = target_end - now          // target_end 为 NULL 时无意义
```

### 2.4 标签

```sql
tag(
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,        -- domain / activity / knowledge / context / report
  name TEXT NOT NULL,
  parent_id TEXT REFERENCES tag(id),         -- 仅 kind = knowledge 使用
  created_at INTEGER NOT NULL,
  UNIQUE(kind, parent_id, name)
)

task_tag(
  task_id TEXT NOT NULL REFERENCES task(id),
  tag_id  TEXT NOT NULL REFERENCES tag(id),
  weight  REAL,                              -- NULL = 关联时长语义；有值 = 加权工时语义
  PRIMARY KEY (task_id, tag_id)
)
```

`weight` 一列承载 SPEC §18 的两种统计口径，**不建两张表**：

| `weight` | 语义 | 统计方式 |
| --- | --- | --- |
| `NULL` | 关联时长 | 该标签计入任务全部工时 |
| 有值（如 0.6） | 加权工时 | 按比例分配工时 |

`weight` 之和**不强制**等于 1。若用户填的权重和不为 1，由 M06 统计引擎决定归一化策略（在 M06 的 spec 中确定）。

### 2.5 知识

```sql
knowledge_stat(
  tag_id TEXT PRIMARY KEY REFERENCES tag(id),   -- kind 必须为 knowledge
  experience_hours REAL NOT NULL DEFAULT 0,
  recent_hours REAL NOT NULL DEFAULT 0,
  application_count INTEGER NOT NULL DEFAULT 0,
  learning_count INTEGER NOT NULL DEFAULT 0,
  skill_level REAL, confidence REAL,            -- 两者并存，SPEC §20
  estimate_accuracy REAL,
  last_used_at INTEGER
)

task_knowledge(
  task_id TEXT NOT NULL REFERENCES task(id),
  knowledge_tag_id TEXT NOT NULL REFERENCES tag(id),
  weight REAL, required_level REAL,
  used INTEGER NOT NULL DEFAULT 0,
  learning_gain REAL, confidence REAL,
  source TEXT,                                  -- user / ai
  PRIMARY KEY (task_id, knowledge_tag_id)
)
```

`skill_level` 与 `confidence` **必须并存**（SPEC §20）：前者是能力估计，后者是样本量支撑。只给一个会得出"做了三个任务就封神"的错误结论。

### 2.6 上下文与决策

```sql
context_fact(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES project(id),
  key TEXT NOT NULL, value TEXT NOT NULL,
  source_type TEXT NOT NULL, source_id TEXT,
  confidence REAL,
  security_level TEXT NOT NULL DEFAULT 'INTERNAL',   -- PUBLIC/INTERNAL/CONFIDENTIAL/STRICT_LOCAL
  created_at INTEGER NOT NULL,
  superseded_by TEXT REFERENCES context_fact(id)     -- 版本化，不覆盖
)

decision(
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES project(id),
  title TEXT NOT NULL, reason TEXT,
  decided_at INTEGER, created_at INTEGER NOT NULL
)

document(
  id TEXT PRIMARY KEY,
  project_id TEXT REFERENCES project(id),
  path TEXT NOT NULL, kind TEXT, security_level TEXT NOT NULL DEFAULT 'INTERNAL',
  created_at INTEGER NOT NULL
)

task_document(task_id TEXT NOT NULL, document_id TEXT NOT NULL, PRIMARY KEY(task_id, document_id))
```

`context_fact` 用 `superseded_by` 做**版本化而非覆盖**（SPEC §28）：改一个参数值时旧值保留并标记被取代，AI 不会读到过期数据，同时保留历史。

---

## 3. Task 状态机

```
Inbox ──→ Clarifying ──→ Ready ──→ Scheduled ──→ Doing ──→ Review ──→ Done
                            │           │           │
                            └───────────┴───────────┴──→ Blocked
                            └───────────┴───────────┴──→ Waiting
                                                        └──→ Cancelled
```

| 状态 | 含义 |
| --- | --- |
| Inbox | 已捕获，未理清 |
| Clarifying | 正在明确"下一步动作是什么" |
| Ready | 可执行，但未排期 |
| Scheduled | 已排入时间块 |
| Doing | 进行中 |
| **Blocked** | **我做不了**（缺技能、缺决策、前置任务未完成） |
| **Waiting** | **等外部**（同事回复、器件到货、测试、审批） |
| Review | 已完成待检查 |
| Done / Cancelled | 终态 |

`Blocked` 与 `Waiting` 是两个状态，不可合并（SPEC §9）。跃迁规则集中在 `domain/task/state.rs`，**在领域层强制**，不得散落到 service 或 command。非法跃迁返回领域错误，不写库。

---

## 4. WorkSession 生命周期

```
created ──start──→ running ──pause──→ paused ──resume──→ running
                     │                                     │
                     ├──interrupt──→ running(新 session)    │
                     │               原 session → paused     │
                     └──finish───────────────────────────→ finished
```

| 状态 | 判据 |
| --- | --- |
| running | `ended_at IS NULL` 且不处于暂停 |
| paused | `ended_at IS NULL` 且 `paused_total_ms` 已含当前暂停区间 |
| finished | `ended_at IS NOT NULL` |

暂停**不写新行**，而是累加 `paused_total_ms` 并记录暂停起点（放在 `paused_total_ms` 的配套字段中，由 M05 决定具体承载方式）。这样一次任务连续工作 3 段仍是一条 session，符合 SPEC §11 的示例（09:00–09:35 / 14:20–15:10 / 16:30–17:00 是三条 session，不是一条）。

**执行模式影响统计，不影响存储**：

| mode | 统计含义 |
| --- | --- |
| FOREGROUND | 计入人工工时 |
| BACKGROUND | 后台并行；人工投入另计 |
| PASSIVE | 机器/被动过程（如仿真跑着）；不计人工工时 |
| WAITING | 等待外部；不计人工工时 |

SPEC §12 的"1h 设计 + 1h AI 生成 ≠ 2h 人工工时"由此保证。

---

## 5. 完成质量

`quality` 取值（SPEC §24）：`normal` / `reworked` / `review_failed` / `partially_done` / `abandoned`。

它存在的意义是防止系统推出错误结论："完成快 = 熟练度高"。有了它，M08 才能把"快但返工"从"快且干净"里分出来。存在 `task.quality`（任务级结论）与 `work_session.quality`（单次会话结论）两处，二者用途不同，不互相推导。

---

## 6. 需要服务层强制的不变量

schema 只能表达一部分约束，以下四条必须由 `services/` 保证并配测试：

| # | 不变量 | 为什么 schema 表达不了 |
| --- | --- | --- |
| 1 | **至多一个 FOREGROUND session** | 唯一索引表达不了"仅对 mode = FOREGROUND 且 ended_at IS NULL" |
| 2 | **`task.quality` 只在 status ⊆ {Done} 时有值** | 跨表/跨状态的条件约束 |
| 3 | **`knowledge_stat.tag_id` 必须指向 kind = knowledge 的 tag** | SQLite 不支持带条件的跨表外键 |
| 4 | **崩溃恢复：`ended_at IS NULL` 且心跳超时的 session 置 `needs_review = 1`** | 需要业务判断时间阈值 |

第 4 条的完整语义（SPEC §46 的崩溃恢复）：

- 进程启动时扫描 `ended_at IS NULL` 且 `last_heartbeat_at` 早于 2 分钟前的 session
- **不擅自补写 `ended_at`** —— 编造的工时比缺失的工时更有害，它会污染统计、估时预测与熟练度模型
- 置 `needs_review = 1`，在 UI 上请用户确认这段时长的归属
- 依据：SPEC §5.3 `User > Rule > AI`、§57「数据必须可解释」

---

## 7. 与 SPEC 的差异

| 差异 | SPEC | 本设计 | 理由 |
| --- | --- | --- | --- |
| 不存 `task.actual_duration` | §8.4 列为字段 | 由 `work_session` 聚合 | 避免第二个真相源；见 §2.2 |
| 不建 `Action` 表 | §3.2 的 WBS 树含 Action 层 | Task 自引用，Action = 叶子 Task | 两套几乎相同的表 = 两套仓储 + 两套状态机 |
| AI 元数据用 JSON 列 | §33 要求 value/source/confidence/confirmed | 内联 JSON 列，不建 EAV 表 | 字段数编译期已知且总整取整存；EAV 是为运行时未知属性集设计的 |
| 新增 `last_heartbeat_at` | 未提 | 有 | SPEC §46 要求崩溃恢复，无心跳无法判断崩溃区间 |
| 新增 `revision`（事件级） | 未提 | 有 | 见 `00-architecture.zh.md` §5.2 |

---

## 8. 索引

```sql
CREATE INDEX idx_task_status        ON task(status);
CREATE INDEX idx_task_project       ON task(project_id);
CREATE INDEX idx_task_parent        ON task(parent_task_id);
CREATE INDEX idx_task_deadline      ON task(deadline) WHERE deadline IS NOT NULL;
CREATE INDEX idx_session_task       ON work_session(task_id);
CREATE INDEX idx_session_started    ON work_session(started_at);
CREATE INDEX idx_session_open       ON work_session(ended_at) WHERE ended_at IS NULL;
CREATE INDEX idx_task_tag_tag       ON task_tag(tag_id);
CREATE INDEX idx_context_fact_lookup ON context_fact(project_id, key) WHERE superseded_by IS NULL;
CREATE INDEX idx_document_project   ON document(project_id);
```

`idx_session_open` 是**部分索引**，专门服务两类高频查询：找当前进行中的会话、崩溃恢复扫描。个人规模下它只有个位数行。

时间统一用 **Unix 毫秒（INTEGER）** 存储，不带时区；展示层按本地时区渲染。理由：避免夏令时与跨时区迁移带来的时间戳歧义，且 SQLite 的日期函数不参与业务计算。
