# P1 · 领域模型与持久化基座实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `cargo test` 全绿地固定住 V0.1 的数据库形状、领域规则与事务边界，成为后续所有计划的基座。

**Architecture:** `commands → services → {storage, domain, platform}`。**服务拥有事务**：仓储写入函数一律接受 `&Transaction`，不自行 `begin`/`commit`、不自行加 `revision`；一次业务操作由服务提交并**恰好加一次** `revision`。`AppError` 放在共享的 `src/error.rs`，`storage` 不得反向引用 `commands`。时间只经 `platform::clock::Clock` 注入。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（`bundled`，不依赖系统 SQLite）· uuid · thiserror · tempfile（dev）· 无前端改动

**Spec:** `../specs/2026-10-02-worktrace-architecture/02-data-model.zh.md`（§2 表与索引、§3 命令与计时、§5 Task 状态机、§6 约束、§9 明细与恢复）· `00-architecture.zh.md` §4/§5 · `01-module-breakdown.zh.md` §2

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。

状态：核心已实施并验收；P1/P2 原验收基线为 209 个测试，P4 后全仓基线为 339 个测试。当前兼容契约与待办见[总纲 §10](2026-10-03-v01-plan-index.md)。上游：[总纲](2026-10-03-v01-plan-index.md)、[数据模型](../specs/2026-10-02-worktrace-architecture/02-data-model.zh.md)。本计划交付存储/领域基础，不代表应用验收完成。

覆盖：F-001…F-005、F-008、F-014、F-015、F-017、F-019 的基础部分。不做 IPC、实时计时、HUD 或后续版本实体。

## Task 1：运行环境与共享错误

- [x] 在 src-tauri 下记录实际 Rust/Cargo 版本，验证 SQLite bundled、UUID、错误派生、临时文件测试依赖，选定可编译版本并更新 Cargo.toml/Cargo.lock；不依据文档中的预设版本推断已经可用。
- [x] 新建 domain/mod.rs、storage/mod.rs、platform/mod.rs、commands/mod.rs、error.rs；在 lib.rs 导出，保留 greet。
- [x] error::AppError 提供稳定 code/message/脱敏 detail；storage 不依赖 commands。领域错误分别映射非法状态、占用冲突、待恢复、版本冲突，不把所有错误压成 DOMAIN_ERROR。
- [x] crate::envelope::WriteEnvelope（P4 已迁移到 src/envelope.rs） 保存 expected_data_epoch 与 expected_row_version；新建只需 epoch，修改既有对象必须版本。关系增删的幂等操作单列，不伪造实体版本。
- [x] tests/error_contract.rs：未知记录、非法状态、epoch/version 冲突均可区分；错误不包含数据库路径、SQL、业务正文。

## Task 2：数据库执行边界与迁移

文件：storage/db.rs、storage/migrations.rs、storage/schema_v1.rs、platform/paths.rs、tests/migrations.rs。

- [x] 小实验比较串行数据库工作线程与受控阻塞边界，记录选择；不得将 Connection 跨 await 持锁或直接放进 UI 回调执行长操作。
- [x] Db::open(path)/open_in_memory() 配置 foreign_keys、busy_timeout；磁盘数据库验证 WAL，内存测试不要求 WAL。
- [x] migrate 在一个事务内执行 DDL 和 user_version；失败回滚全部表和版本。现有库迁移前由初始化流程做一致备份，备份失败拒绝迁移；未来版本拒绝写入。
- [x] 完整 DDL 以附录为起点，补全状态/质量组合、非负版本、允许 mode/kind、外键 RESTRICT 和服务校验。V0.1 不接受 Scheduled、BACKGROUND/PASSIVE/WAITING 写入，不建 Goal/Milestone/番茄钟表。
- [x] tests/migrations.rs：空库初始化、重复启动、未来版本拒绝、故障中断后无半套表、备份失败不迁移。尚无 UI 时用临时文件验证。

## Task 3：领域不变量与时钟接缝

文件：domain/task.rs、domain/session.rs、domain/interval.rs、domain/error.rs、platform/clock.rs。

- [x] TaskStatus/TransitionCause 实现 02 §5 的 V0.1 跃迁；Scheduled 拒绝，终态只显式 reopen。
- [x] SessionState/SessionMode/TimerKind、IntervalFacts/IntervalRange：running 恰一开放可信区间，paused/finished/discarded 无开放有效区间，recovering 停止正常计时。记录损坏与普通待确认分开。
- [x] ClosedIntervalFacts { ended_at, duration_ms, sampled_end_wall_at, needs_review }：协调器已验证的闭合事实，供 Task 4 的 close_interval 落库。仓储**不得**由 wall-now 自行推算其中任何一项。
- [x] Clock::sample() 返回 ClockSample { wall_ms, monotonic_ms }；SystemClock 的 Instant 仅当前 run 有效。FakeClock 可独立推进/回拨两个数值并注入采样失败。
- [x] tests/domain_invariants.rs 覆盖允许/拒绝跃迁、0 长度半开区间、重叠、待确认和作废排除；无需一仓储函数一个机械测试。

## Task 4：事务接口、元数据与仓储

文件：storage/meta.rs、storage/guards.rs、storage/task_repo.rs、storage/session_repo.rs、storage/checkpoint_repo.rs、tests/transaction_boundary.rs。

- [x] Meta/read_meta/init_meta/bump_revision；epoch 初始化 UUID，业务服务成功提交时 bump_revision 恰一次，无操作/拒绝/心跳不增加。
- [x] guard_epoch(tx, expected)/guard_row_version(actual, expected) 在调用方写事务内检查；禁止将读到的 epoch 当请求 expected 值比较自身。
- [x] 写入接口统一接受 &Transaction；仓储不得 begin/commit 或自行 bump_revision。单独服务包装拥有事务；组合服务可复用同一事务。
- [x] task_repo 提供 create_task(tx, title, project_id, now)、get_task/list_tasks_filtered（P4 替代并删除原 list_tasks）、transition_task(tx, id, expected_version, target, cause, now)，任务变化与 task_change 同事务；已归档项目禁止新归属。
- [x] session_repo::create_session(tx, task_id, run_id, mode, timer_kind, target_duration_ms, attributed_start) 持久化模式、预算与初始区间；调用方同事务保存 elapsed=0 检查点。mode 必填（DDL 是 NOT NULL；V0.1 只写 FOREGROUND，其余取值由服务拒绝）。倒计时预算必须正数，正计时必须 null。
- [x] close_interval(tx, id, ClosedIntervalFacts)/open_interval(tx, session_id, attributed_start)/update_session_state(tx, id, expected_version, target) 接收协调器已验证事实；不得用 wall-now 在仓储计算工时。
- [x] checkpoint_repo::write(tx, Checkpoint)/latest(conn, interval_id)；检查 run/interval 对应、归属和 elapsed 一致。心跳服务拥有独立短事务，不加 revision。
- [x] tests/transaction_boundary.rs：同一事务理清任务+建立会话+初始检查点+审计，只加一次 revision；末步骤故障全部回滚；预算重新打开库后仍存在；并发前台 start 仅一次成功。

## 执行与完成门槛

- [x] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。

在 src-tauri 运行 cargo fmt --check、cargo test、cargo clippy --all-targets。P1 通过只表示基础库正确，F-ID 的应用/UI 验收仍归后续计划。提交文件范围以上述文件及 Cargo 配置为准，不提交无关改动。

分层检查（违规明确失败）：

```powershell
# 在 src-tauri/ 下运行
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/check-layers.ps1
```

脚本在 `src-tauri/scripts/check-layers.ps1`，**不要在计划或对话里手抄这段检查**。

> 为什么必须是脚本而不是内联 grep：朴素 grep 会把**声明这条规则的文档注释本身**当成
> 违规——`src/domain/mod.rs` 的 `//!` 里写着「不得引用 `rusqlite` / `platform::`」，
> 于是每次检查都报 LEAK。一个永远失败的检查会被忽略，比没有更糟。脚本在匹配前剔除
> 注释行，并做过反向验证（故意塞一行越层 `use` 必须让检查以退出码 1 失败）。

## 附录：V0.1 DDL 起点

下面保留开工时的历史 DDL 起点，不是当前可执行迁移；当前权威定义为 src-tauri/src/storage/schema_v1.rs，不能据此附录重建或改写已发布迁移。所有发布后的迁移不可原地改写。跨表状态和历史重叠还需服务事务校验。

```sql
CREATE TABLE app_meta(
  singleton   INTEGER PRIMARY KEY CHECK (singleton = 1),
  data_epoch  TEXT    NOT NULL,
  revision    INTEGER NOT NULL
);

CREATE TABLE application_run(
  id            TEXT PRIMARY KEY NOT NULL,
  started_at    INTEGER NOT NULL,
  clean_exit_at INTEGER
);

CREATE TABLE project(
  id          TEXT PRIMARY KEY NOT NULL,
  name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
  description TEXT,
  row_version INTEGER NOT NULL,
  status      TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE task(
  id                     TEXT PRIMARY KEY NOT NULL,
  project_id             TEXT REFERENCES project(id),
  parent_task_id         TEXT REFERENCES task(id),
  title                  TEXT NOT NULL CHECK (length(trim(title)) > 0),
  description            TEXT,
  status                 TEXT NOT NULL,
  priority_json          TEXT,
  estimated_json         TEXT,
  planned_duration_ms    INTEGER,
  deadline               INTEGER,
  importance             INTEGER,
  urgency                INTEGER,
  energy_required        INTEGER,
  difficulty             INTEGER,
  completion_criteria    TEXT,
  quality                TEXT,
  baseline_estimate_json TEXT,
  row_version            INTEGER NOT NULL,
  created_at             INTEGER NOT NULL,
  updated_at             INTEGER NOT NULL
);

CREATE TABLE work_session(
  id                 TEXT PRIMARY KEY NOT NULL,
  task_id            TEXT NOT NULL REFERENCES task(id),
  run_id             TEXT NOT NULL REFERENCES application_run(id),
  mode               TEXT NOT NULL,
  state              TEXT NOT NULL,
  timer_kind         TEXT NOT NULL,
  target_duration_ms INTEGER,
  started_at         INTEGER NOT NULL,
  ended_at           INTEGER,
  last_heartbeat_at  INTEGER,
  interruption_of    TEXT REFERENCES work_session(id),
  quality            TEXT,
  needs_review       INTEGER NOT NULL DEFAULT 0 CHECK (needs_review IN (0,1)),
  row_version        INTEGER NOT NULL,
  CONSTRAINT ck_timer_budget CHECK (
    (timer_kind='stopwatch' AND target_duration_ms IS NULL) OR
    (timer_kind='countdown' AND target_duration_ms IS NOT NULL AND target_duration_ms > 0)
  )
);

CREATE TABLE work_interval(
  id                  TEXT PRIMARY KEY NOT NULL,
  session_id          TEXT NOT NULL REFERENCES work_session(id),
  started_at          INTEGER NOT NULL,
  ended_at            INTEGER,
  voided_at           INTEGER,
  duration_ms         INTEGER,
  sampled_end_wall_at INTEGER,
  needs_review        INTEGER NOT NULL DEFAULT 0 CHECK (needs_review IN (0,1)),
  CONSTRAINT ck_interval_range CHECK (ended_at IS NULL OR ended_at >= started_at),
  -- 必须写成 CASE 而不是 OR：SQLite 把 CHECK 结果为 NULL 当作通过，
  -- 而 `duration_ms = ended_at - started_at` 在 duration_ms 为 NULL 时求值为 NULL，
  -- 用 OR 连接会让"可信闭合却没有 duration_ms"这条悄悄溜过去。
  CONSTRAINT ck_interval_duration CHECK (
    CASE
      WHEN duration_ms IS NOT NULL THEN
        ended_at IS NOT NULL AND duration_ms = ended_at - started_at AND duration_ms >= 0
      WHEN ended_at IS NOT NULL AND needs_review = 0 THEN
        0                                   -- 可信闭合必须有 duration_ms
      ELSE 1
    END
  )
);

CREATE TABLE interval_checkpoint(
  interval_id    TEXT PRIMARY KEY REFERENCES work_interval(id),
  run_id         TEXT NOT NULL REFERENCES application_run(id),
  wall_at        INTEGER NOT NULL,
  attribution_at INTEGER NOT NULL,
  elapsed_ms     INTEGER NOT NULL CHECK (elapsed_ms >= 0)
);

CREATE TABLE time_edit(
  id          TEXT PRIMARY KEY NOT NULL,
  session_id  TEXT NOT NULL REFERENCES work_session(id),
  before_json TEXT NOT NULL,
  after_json  TEXT NOT NULL,
  reason      TEXT,
  created_at  INTEGER NOT NULL
);

CREATE TABLE task_change(
  id          TEXT PRIMARY KEY NOT NULL,
  task_id     TEXT NOT NULL REFERENCES task(id),
  before_json TEXT NOT NULL,
  after_json  TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE daily_plan(
  task_id    TEXT NOT NULL REFERENCES task(id),
  local_date TEXT NOT NULL,
  timezone   TEXT NOT NULL,
  PRIMARY KEY (task_id, local_date, timezone)
);

CREATE TABLE tag(
  id         TEXT PRIMARY KEY NOT NULL,
  kind       TEXT NOT NULL,
  name       TEXT NOT NULL CHECK (length(trim(name)) > 0),
  parent_id  TEXT REFERENCES tag(id),
  row_version INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE task_tag(
  task_id TEXT NOT NULL REFERENCES task(id),
  tag_id  TEXT NOT NULL REFERENCES tag(id),
  weight  REAL,
  PRIMARY KEY (task_id, tag_id)
);

-- 索引名与定义逐条抄自 02 §2 的索引块，不得改名或改列：
-- 名字是下游（迁移对比、诊断、测试）的稳定引用点。
CREATE UNIQUE INDEX uq_running_foreground ON work_session(mode)
  WHERE mode='FOREGROUND' AND state='running';
CREATE UNIQUE INDEX uq_open_interval ON work_interval(session_id)
  WHERE ended_at IS NULL AND voided_at IS NULL;
CREATE UNIQUE INDEX uq_tag_root ON tag(kind,name) WHERE parent_id IS NULL;
CREATE UNIQUE INDEX uq_tag_child ON tag(kind,parent_id,name) WHERE parent_id IS NOT NULL;
CREATE INDEX idx_interval_session ON work_interval(session_id);
CREATE INDEX idx_task_project ON task(project_id);
CREATE INDEX idx_task_status ON task(status);
CREATE INDEX idx_session_task ON work_session(task_id);
```

## 当前兼容状态

跨阶段接口、错误载荷、启动归属及 P7 前待办统一见[总纲 §10](2026-10-03-v01-plan-index.md)。P1/P2/P4 核心已验收，P3 尚未实施；历史签名、测试数量和开工记录保留为当时证据，消费接口以当前源码及总纲为准。文档对齐不表示待办代码、IPC 或平台验证已经完成。
