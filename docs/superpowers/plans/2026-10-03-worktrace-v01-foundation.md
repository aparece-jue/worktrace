# Worktrace V0.1 基座（M02 领域 + M01 持久化）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `cargo test` 全绿地固定住 V0.1 的数据库形状与领域规则——02 §2 的 12 张表、`data_epoch`/`revision` 信封、Task 与 Session/Interval 的跃迁表和不变量——成为后续 M04/M05/M06 计划可以直接依赖的基座。

**Architecture:** 按仓库既有的六层约定落地：`domain/` 放纯逻辑（无 IO、不引用 storage），`storage/` 放 SQLite 与仓储（不引用 platform），`platform/` 放 OS 接缝（数据目录、时钟），`commands/` 放命令信封与错误码。依赖方向严格自上而下（commands → storage → domain → platform），`events/` 只有纯类型。领域层不认识 SQLite，仓储层不认识 `std::time`——时间一律经 `platform::clock::Clock` 注入，测试用假时钟。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（`bundled`，不依赖系统 SQLite）· serde · thiserror 2 · uuid 1 · tempfile 3（dev）

**Spec:**
- `docs/superpowers/specs/2026-10-02-worktrace-architecture/02-data-model.zh.md`（§2 表结构、§3 会话与计时、§4 崩溃恢复、§5 Task 状态机）
- `.../00-architecture.zh.md` §4 IPC 与错误、§5 数据代次与 revision
- `.../01-module-breakdown.zh.md` §1 M00/M01/M02 职责、§2 边界
- `.../04-functional-spec.zh.md` F-001…F-008（本计划只覆盖其中的存储与领域部分）

## Global Constraints

- **分层与依赖方向**（01 §2、仓库 `AGENTS.md`）：`commands/ → storage/ → domain/ → platform/`；`domain/` **禁止 IO**、只能引用 `events/` 的纯类型；`storage/` **禁止调用 `platform/`**；`commands/` **禁止直连 SQL**。违反此方向的 import 一律驳回。
- **表结构以 02 §2 为准**，但 V0.1 只建 12 张表：`app_meta`、`application_run`、`project`、`task`、`work_session`、`work_interval`、`interval_checkpoint`、`time_edit`、`task_change`、`daily_plan`、`tag`、`task_tag`。**`goal` 与 `milestone` 是 V0.2，不建**；因此 `project.goal_id` 与 `task.milestone_id` 这两列在 V0.1 也**不加**（02 §6 原话：「goal_id 与 milestone_id 到 V0.2 迁移时才加」）。
- **时间一律为 Unix 毫秒整数**；`started_at`/`ended_at`/`duration_ms` 均为非负整数毫秒（02 §3）。
- **revision 语义**（00 §5）：同一 `data_epoch` 内**业务写事务 revision 恰好 +1**；心跳、tick、纯读**不加** revision；恢复/替换库要生成**全新** `data_epoch`，旧 epoch 的修改请求返回 `DATA_EPOCH_MISMATCH`。
- **错误契约**（00 §4）：预期失败返回 `Result<T, AppError>`，`AppError` 必带 `code`、`message`、脱敏 `detail`。禁止对可预期的用户/IO 错误用 `unwrap()`/`expect()`；测试代码除外。
- **外键默认 `RESTRICT`**（02 §6），因此连接必须开 `PRAGMA foreign_keys = ON`——SQLite 默认是关的，漏开会让所有外键测试假通过。
- **命令执行位置**：所有 `cargo` 命令在 `src-tauri/` 目录下运行。仓库位于 Windows 盘（`D:\ProJect\worktrace`），Windows PowerShell 与 WSL 两套工具链都是 1.98.1；**同一次执行里只用一套**，不要在两侧交替构建，否则 `target/` 会反复全量重编。
- **改动纪律**：只碰本计划列出的文件。`src/components/DockviewDemo.tsx` 等既有前端文件、`greet` 命令都保留不动（R-04 的 demo 排除打包是后续计划的事）。

---

## 文件结构

```
src-tauri/
  Cargo.toml                     改：加 rusqlite / uuid / thiserror / tempfile
  src/
    lib.rs                       改：声明模块、导出 Tauri 命令入口
    domain/
      mod.rs                     建：领域层出口
      error.rs                   建：DomainError（无 IO、无字符串化的 SQL 错误）
      ids.rs                     建：TaskId / SessionId / IntervalId / RunId 新类型
      task.rs                    建：TaskStatus 与 02 §5 跃迁表
      session.rs                 建：SessionState / SessionMode / TimerKind 与跃迁表
      interval.rs                建：区间范围校验、重叠判定、session 不变量
    platform/
      mod.rs                     建：平台层出口
      paths.rs                   建：应用数据目录解析（可注入根目录，便于测试）
      clock.rs                   建：Clock trait（挂钟 + 单调）与 FakeClock
    storage/
      mod.rs                     建：存储层出口
      db.rs                      建：打开连接、PRAGMA、内存库
      migrations.rs              建：基于 PRAGMA user_version 的迁移运行器
      schema_v1.rs               建：V0.1 建表 SQL 常量（迁移 1 的内容）
      meta.rs                    建：app_meta 读写与 bump_revision
      task_repo.rs               建：Task 的建/取/跃迁（含 task_change 审计）
      session_repo.rs            建：Session/Interval 的建/取/跃迁
    commands/
      mod.rs                     建：命令层出口
      envelope.rs                建：AppError 与 WriteEnvelope 守卫
  tests/
    foundation.rs                建：跨层集成测试（迁移→信封→跃迁→不变量）
```

`domain/` 与 `storage/` 之间只通过纯类型与仓储函数签名耦合；`storage/` 接收 `&Connection`，不认识 `platform::Clock`。

---

### Task 1: 依赖、模块骨架与测试基建

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/src/domain/mod.rs`, `platform/mod.rs`, `storage/mod.rs`, `commands/mod.rs`

**Interfaces:**
- Consumes: 无（起点）
- Produces: 四个空模块的声明，供后续任务填充

- [ ] **Step 1: 加入依赖**

编辑 `src-tauri/Cargo.toml`，在 `[dependencies]` 段加入：

```toml
rusqlite = { version = "0.40", features = ["bundled"] }
uuid = { version = "1", features = ["v4"] }
thiserror = "2"
```

并在文件末尾加：

```toml
[dev-dependencies]
tempfile = "3"
```

`bundled` 会用 `cc` 编译一份 SQLite 进二进制——本机 `gcc 11.4` 已在，不需要系统 `libsqlite3-dev`。

- [ ] **Step 2: 声明模块骨架**

把 `src-tauri/src/lib.rs` 改成（保留原有 `greet` 与 `run`，只加模块声明）：

```rust
pub mod commands;
pub mod domain;
pub mod platform;
pub mod storage;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("你好，{}！Worktrace 的 Rust 后端已连接。", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running Worktrace");
}
```

四个 `mod.rs` 先各写一行文档注释占位，例如 `src-tauri/src/domain/mod.rs`：

```rust
//! 领域层：实体、跃迁表与校验。禁止 IO。
```

`platform/mod.rs` 写 `//! 平台层：OS 适配的叶子模块。`，`storage/mod.rs` 写 `//! 存储层：SQLite、迁移与仓储。不得调用 platform。`，`commands/mod.rs` 写 `//! 命令层：IPC 边界与错误信封。不得直连 SQL。`

- [ ] **Step 3: 加一个能跑的冒烟测试**

在 `src-tauri/src/lib.rs` 末尾加：

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds_and_tests_run() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 4: 验证能编译并跑通**

Run（在 `src-tauri/`）: `cargo test --lib`
Expected: `test tests::crate_builds_and_tests_run ... ok`，`test result: ok. 1 passed`。首次会编译 bundled SQLite，耗时数分钟属正常。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/lib.rs \
        src-tauri/src/domain/mod.rs src-tauri/src/platform/mod.rs \
        src-tauri/src/storage/mod.rs src-tauri/src/commands/mod.rs
git commit -m "chore(m01): 加入 rusqlite 与分层模块骨架"
```

---

### Task 2: 平台层接缝——数据目录与时钟

**Files:**
- Create: `src-tauri/src/platform/paths.rs`
- Create: `src-tauri/src/platform/clock.rs`
- Modify: `src-tauri/src/platform/mod.rs`

**Interfaces:**
- Consumes: Task 1 的 `platform` 模块
- Produces:
  - `platform::paths::db_path(root: &Path) -> PathBuf` — 返回 `<root>/worktrace.db`
  - `platform::clock::Clock` trait：`fn wall_ms(&self) -> i64`、`fn monotonic_ms(&self) -> i64`
  - `platform::clock::SystemClock`（真实实现）、`platform::clock::FakeClock`（测试用，可推进、可跳变）

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/platform/clock.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_keeps_wall_and_monotonic_independent() {
        let clock = FakeClock::new(1_700_000_000_000, 5_000);
        // 模拟"系统改时"：挂钟后退 1 秒，单调时钟不动
        clock.set_wall_ms(1_699_999_999_000);
        assert_eq!(clock.wall_ms(), 1_699_999_999_000);
        assert_eq!(clock.monotonic_ms(), 5_000);

        // 推进单调时钟，挂钟不动（模拟检测到累计偏差前的采样）
        clock.advance_monotonic_ms(30_000);
        assert_eq!(clock.monotonic_ms(), 35_000);
        assert_eq!(clock.wall_ms(), 1_699_999_999_000);
    }
}
```

在 `src-tauri/src/platform/paths.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn db_path_is_under_the_given_root() {
        assert_eq!(db_path(Path::new("/tmp/wt")), Path::new("/tmp/wt/worktrace.db"));
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib platform`
Expected: 编译失败，`cannot find type FakeClock` / `cannot find function db_path`。

- [ ] **Step 3: 实现平台层**

`src-tauri/src/platform/paths.rs`：

```rust
//! 应用数据目录与数据库文件定位。
//!
//! 根目录由调用方注入，便于测试用临时目录；生产环境由 Tauri 的
//! app_data_dir 解析后传入，本层不认识 Tauri。

use std::path::{Path, PathBuf};

pub const DB_FILE_NAME: &str = "worktrace.db";

pub fn db_path(root: &Path) -> PathBuf {
    root.join(DB_FILE_NAME)
}
```

`src-tauri/src/platform/clock.rs`：

```rust
//! 时钟接缝。
//!
//! 领域与存储都不直接取时间：挂钟用于时间归属，单调时钟用于测量时长，
//! 两者必须能分别伪造，否则 08 §1 的"墙钟与单调增量之差"无法测试。

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// 挂钟毫秒（Unix epoch）。可能被系统改时影响。
    fn wall_ms(&self) -> i64;
    /// 单调毫秒，仅在同一进程内可比；跨重启不复用。
    fn monotonic_ms(&self) -> i64;
}

pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self { origin: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn monotonic_ms(&self) -> i64 {
        self.origin.elapsed().as_millis() as i64
    }
}

/// 测试用时钟：挂钟与单调时钟可分别设定与推进。
pub struct FakeClock {
    wall_ms: AtomicI64,
    monotonic_ms: AtomicI64,
}

impl FakeClock {
    pub fn new(wall_ms: i64, monotonic_ms: i64) -> Self {
        Self {
            wall_ms: AtomicI64::new(wall_ms),
            monotonic_ms: AtomicI64::new(monotonic_ms),
        }
    }

    pub fn set_wall_ms(&self, v: i64) {
        self.wall_ms.store(v, Ordering::SeqCst);
    }

    pub fn advance_monotonic_ms(&self, delta: i64) {
        self.monotonic_ms.fetch_add(delta, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn wall_ms(&self) -> i64 {
        self.wall_ms.load(Ordering::SeqCst)
    }

    fn monotonic_ms(&self) -> i64 {
        self.monotonic_ms.load(Ordering::SeqCst)
    }
}
```

`src-tauri/src/platform/mod.rs`：

```rust
//! 平台层：OS 适配的叶子模块。

pub mod clock;
pub mod paths;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib platform`
Expected: `test result: ok. 2 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/platform/
git commit -m "feat(m00): 平台层加数据目录与时钟接缝"
```

---

### Task 3: 打开数据库与迁移运行器

**Files:**
- Create: `src-tauri/src/storage/db.rs`
- Create: `src-tauri/src/storage/migrations.rs`
- Create: `src-tauri/src/storage/schema_v1.rs`
- Modify: `src-tauri/src/storage/mod.rs`

**Interfaces:**
- Consumes: Task 2 的 `platform::paths`
- Produces:
  - `storage::db::Db`：`open(path: &Path) -> Result<Db, StorageError>`、`open_in_memory() -> Result<Db, StorageError>`、`conn(&self) -> &Connection`
  - `storage::db::StorageError`（`thiserror`）
  - `storage::migrations::SCHEMA_VERSION: i64 = 1`、`migrate(conn: &Connection) -> Result<(), StorageError>`、`current_version(conn: &Connection) -> Result<i64, StorageError>`
  - `storage::schema_v1::SCHEMA_V1_SQL: &str`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/storage/migrations.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use crate::storage::db::Db;
    use super::*;

    #[test]
    fn migrate_is_idempotent_and_records_version() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(current_version(db.conn()).unwrap(), 0);

        migrate(db.conn()).unwrap();
        assert_eq!(current_version(db.conn()).unwrap(), SCHEMA_VERSION);

        // 再跑一次不得报错，也不得重复建表
        migrate(db.conn()).unwrap();
        assert_eq!(current_version(db.conn()).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn foreign_keys_are_enabled() {
        let db = Db::open_in_memory().unwrap();
        let on: i64 = db
            .conn()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on, 1, "SQLite 默认关闭外键，必须显式打开，否则外键测试会假通过");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib storage`
Expected: 编译失败，`cannot find type Db` / `cannot find function migrate`。

- [ ] **Step 3: 实现 db 与迁移运行器**

`src-tauri/src/storage/db.rs`：

```rust
//! SQLite 连接的打开与 PRAGMA 设置。

use rusqlite::Connection;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> Result<(), StorageError> {
        // 外键默认是关的（02 §6 要求外键生效）
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // journal_mode 会返回一行结果，必须用 query_row 而不是 pragma_update；
        // 内存库不支持 WAL，失败可忽略（文件库上真正的失败会在首次写入时暴露）
        let _ = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0));
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}
```

`src-tauri/src/storage/schema_v1.rs`：把 02 §2 的 V0.1 子集写成一个常量。**注意 V0.1 不含 `goal`/`milestone`，因此 `project` 没有 `goal_id`、`task` 没有 `milestone_id`。**

```rust
//! V0.1 建表 SQL（02 §2 的 V0.1 子集）。

pub const SCHEMA_V1_SQL: &str = r#"
CREATE TABLE app_meta(
  singleton   INTEGER PRIMARY KEY CHECK (singleton = 1),
  data_epoch  TEXT    NOT NULL,
  revision    INTEGER NOT NULL
);

CREATE TABLE application_run(
  id            TEXT PRIMARY KEY,
  started_at    INTEGER NOT NULL,
  clean_exit_at INTEGER
);

CREATE TABLE project(
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
  description TEXT,
  status      TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE TABLE task(
  id                     TEXT PRIMARY KEY,
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
  id                 TEXT PRIMARY KEY,
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
  row_version        INTEGER NOT NULL
);

CREATE TABLE work_interval(
  id                  TEXT PRIMARY KEY,
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
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES work_session(id),
  before_json TEXT NOT NULL,
  after_json  TEXT NOT NULL,
  reason      TEXT,
  created_at  INTEGER NOT NULL
);

CREATE TABLE task_change(
  id          TEXT PRIMARY KEY,
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
  id         TEXT PRIMARY KEY,
  kind       TEXT NOT NULL,
  name       TEXT NOT NULL CHECK (length(trim(name)) > 0),
  parent_id  TEXT REFERENCES tag(id),
  created_at INTEGER NOT NULL
);

CREATE TABLE task_tag(
  task_id TEXT NOT NULL REFERENCES task(id),
  tag_id  TEXT NOT NULL REFERENCES tag(id),
  weight  REAL,
  PRIMARY KEY (task_id, tag_id)
);

-- 02 §6：前台至多一条 running
CREATE UNIQUE INDEX uq_running_foreground ON work_session(mode)
  WHERE mode = 'FOREGROUND' AND state = 'running';

-- 02 §6：每个 session 至多一个开放区间
CREATE UNIQUE INDEX uq_open_interval_per_session ON work_interval(session_id)
  WHERE ended_at IS NULL AND voided_at IS NULL;

CREATE INDEX ix_task_status ON task(status);
CREATE INDEX ix_interval_session ON work_interval(session_id, started_at);
CREATE INDEX ix_session_state ON work_session(state);
"#;
```

`src-tauri/src/storage/migrations.rs`：

```rust
//! 基于 `PRAGMA user_version` 的迁移运行器。
//!
//! V0.1 只有一个版本。后续版本在此追加分支，不要修改 `SCHEMA_V1_SQL`
//! ——已发布的迁移必须保持字节稳定。

use crate::storage::db::StorageError;
use crate::storage::schema_v1::SCHEMA_V1_SQL;
use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 1;

pub fn current_version(conn: &Connection) -> Result<i64, StorageError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

pub fn migrate(conn: &Connection) -> Result<(), StorageError> {
    let found = current_version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(StorageError::SchemaTooNew { found, supported: SCHEMA_VERSION });
    }
    if found == SCHEMA_VERSION {
        return Ok(());
    }
    if found < 1 {
        conn.execute_batch(SCHEMA_V1_SQL)?;
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}
```

`src-tauri/src/storage/mod.rs`：

```rust
//! 存储层：SQLite、迁移与仓储。不得调用 platform。

pub mod db;
pub mod migrations;
pub mod schema_v1;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib storage`
Expected: `test result: ok. 2 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/
git commit -m "feat(m01): 数据库打开与 user_version 迁移运行器"
```

---

### Task 4: V0.1 表结构与索引真在库里

**Files:**
- Modify: `src-tauri/src/storage/schema_v1.rs`（仅当测试暴露缺项）
- Create: `src-tauri/tests/schema.rs`

**Interfaces:**
- Consumes: Task 3 的 `Db`、`migrate`、`SCHEMA_VERSION`
- Produces: 无新 API；本任务的价值是**证明**表结构、部分唯一索引与 CHECK 真的生效

- [ ] **Step 1: 写失败的测试**

创建 `src-tauri/tests/schema.rs`：

```rust
//! 迁移后的库必须与 02 §2 的 V0.1 形状一致。

use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::migrations::migrate;

fn fresh() -> Db {
    let db = Db::open_in_memory().unwrap();
    migrate(db.conn()).unwrap();
    db
}

const V01_TABLES: &[&str] = &[
    "app_meta", "application_run", "daily_plan", "interval_checkpoint", "project",
    "tag", "task", "task_change", "task_tag", "time_edit", "work_interval", "work_session",
];

#[test]
fn all_v01_tables_exist_and_v02_tables_do_not() {
    let db = fresh();
    let mut names: Vec<String> = db
        .conn()
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    names.sort();

    let mut expected: Vec<String> = V01_TABLES.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(names, expected);

    for v02 in ["goal", "milestone"] {
        assert!(
            !names.contains(&v02.to_string()),
            "{v02} 属于 V0.2，V0.1 不得建表"
        );
    }
}

#[test]
fn project_and_task_have_no_v02_columns() {
    let db = fresh();
    let cols = |t: &str| -> Vec<String> {
        db.conn()
            .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(!cols("project").contains(&"goal_id".to_string()));
    assert!(!cols("task").contains(&"milestone_id".to_string()));
    // 但 V0.1 该有的列必须在
    assert!(cols("task").contains(&"baseline_estimate_json".to_string()));
    assert!(cols("task").contains(&"row_version".to_string()));
    assert!(cols("work_interval").contains(&"sampled_end_wall_at".to_string()));
}

#[test]
fn only_one_running_foreground_session_is_allowed() {
    let db = fresh();
    let c = db.conn();
    c.execute_batch(
        "INSERT INTO application_run(id, started_at) VALUES ('run-1', 0);
         INSERT INTO task(id, title, status, row_version, created_at, updated_at)
           VALUES ('t1', 'T', 'Doing', 0, 0, 0);",
    )
    .unwrap();
    let ins = |id: &str, mode: &str, state: &str| {
        c.execute(
            "INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind,
                                      started_at, row_version)
             VALUES (?1, 't1', 'run-1', ?2, ?3, 'stopwatch', 0, 0)",
            rusqlite::params![id, mode, state],
        )
    };
    ins("s1", "FOREGROUND", "running").unwrap();
    let dup = ins("s2", "FOREGROUND", "running");
    assert!(dup.is_err(), "前台不得有第二条 running（uq_running_foreground）");

    // 不同 mode 或不同 state 不受限
    ins("s3", "BACKGROUND", "running").unwrap();
    ins("s4", "FOREGROUND", "paused").unwrap();
}

#[test]
fn a_session_has_at_most_one_open_interval() {
    let db = fresh();
    let c = db.conn();
    c.execute_batch(
        "INSERT INTO application_run(id, started_at) VALUES ('run-1', 0);
         INSERT INTO task(id, title, status, row_version, created_at, updated_at)
           VALUES ('t1', 'T', 'Doing', 0, 0, 0);
         INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind,
                                  started_at, row_version)
           VALUES ('s1', 't1', 'run-1', 'FOREGROUND', 'running', 'stopwatch', 0, 0);",
    )
    .unwrap();
    c.execute(
        "INSERT INTO work_interval(id, session_id, started_at) VALUES ('i1', 's1', 0)",
        [],
    )
    .unwrap();
    let dup = c.execute(
        "INSERT INTO work_interval(id, session_id, started_at) VALUES ('i2', 's1', 10)",
        [],
    );
    assert!(dup.is_err(), "同一 session 不得有两个开放区间（uq_open_interval_per_session）");

    // 闭合上一个之后可以再开
    c.execute("UPDATE work_interval SET ended_at = 100, duration_ms = 100 WHERE id = 'i1'", [])
        .unwrap();
    c.execute(
        "INSERT INTO work_interval(id, session_id, started_at) VALUES ('i3', 's1', 200)",
        [],
    )
    .unwrap();
}

#[test]
fn trusted_closed_interval_must_have_matching_duration() {
    let db = fresh();
    let c = db.conn();
    c.execute_batch(
        "INSERT INTO application_run(id, started_at) VALUES ('run-1', 0);
         INSERT INTO task(id, title, status, row_version, created_at, updated_at)
           VALUES ('t1', 'T', 'Doing', 0, 0, 0);
         INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind,
                                  started_at, row_version)
           VALUES ('s1', 't1', 'run-1', 'FOREGROUND', 'running', 'stopwatch', 0, 0);",
    )
    .unwrap();

    // 可信闭合（needs_review=0）但没有 duration_ms → 拒绝
    let missing = c.execute(
        "INSERT INTO work_interval(id, session_id, started_at, ended_at, needs_review)
         VALUES ('bad1', 's1', 0, 500, 0)",
        [],
    );
    assert!(missing.is_err(), "可信闭合区间 duration_ms 不得为空");

    // duration_ms 与 ended-started 不一致 → 拒绝
    let wrong = c.execute(
        "INSERT INTO work_interval(id, session_id, started_at, ended_at, duration_ms, needs_review)
         VALUES ('bad2', 's1', 0, 500, 400, 0)",
        [],
    );
    assert!(wrong.is_err(), "duration_ms 必须等于 ended_at - started_at");

    // 待确认区间允许 duration_ms 为空
    c.execute(
        "INSERT INTO work_interval(id, session_id, started_at, ended_at, needs_review)
         VALUES ('ok1', 's1', 0, 500, 1)",
        [],
    )
    .unwrap();
}

#[test]
fn foreign_keys_are_enforced() {
    let db = fresh();
    let orphan = db.conn().execute(
        "INSERT INTO task(id, title, status, row_version, created_at, updated_at)
         VALUES ('t1', 'T', 'Inbox', 0, 0, 0)",
        [],
    );
    // task 不引用外部，应成功
    orphan.unwrap();
    // work_session 引用不存在的 task 必须失败
    let bad = db.conn().execute(
        "INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind, started_at, row_version)
         VALUES ('s1', 'nope', 'nope', 'FOREGROUND', 'running', 'stopwatch', 0, 0)",
        [],
    );
    assert!(bad.is_err(), "外键必须生效");
}
```

- [ ] **Step 2: 运行测试，记录哪些失败**

Run: `cargo test --test schema`
Expected: **允许失败**。这一跑的目的不是通过，而是拿到"库里到底有什么"的差集。把失败断言原文记下来——它给出的缺失列表就是 Step 3 要补的 SQL 清单。若五个用例一次全过，说明 Task 3 的 `SCHEMA_V1_SQL` 已经正确，直接跳到 Step 4。

- [ ] **Step 3: 按差集补 `SCHEMA_V1_SQL` 直到全绿**

只改 `src-tauri/src/storage/schema_v1.rs`，**不要改测试里的期望清单**——期望清单抄自 02 §2，它是规格。

两个已知的坑：

1. `trusted_closed_interval_must_have_matching_duration` 失败 → 检查 `ck_interval_duration` 是否写成了 `CASE` 形式。SQLite 把 CHECK 求值为 NULL 当作**通过**，而 `duration_ms = ended_at - started_at` 在 `duration_ms IS NULL` 时是 NULL，用 `OR` 连接会让"可信闭合却没有 duration_ms"整条溜过去。
2. `only_one_running_foreground_session_is_allowed` 失败 → 确认 `uq_running_foreground` 的 `WHERE` 子句写全了 `mode='FOREGROUND' AND state='running'`；漏掉任一项都会把不该拦的行拦掉。

- [ ] **Step 4: 重跑确认全绿**

Run: `cargo test --test schema`
Expected: `test result: ok. 5 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/tests/schema.rs src-tauri/src/storage/schema_v1.rs
git commit -m "test(m01): 固定 V0.1 表结构、部分唯一索引与区间 CHECK"
```

---

### Task 5: app_meta、data_epoch 与 revision 信封

**Files:**
- Create: `src-tauri/src/storage/meta.rs`
- Create: `src-tauri/src/commands/envelope.rs`
- Modify: `src-tauri/src/storage/mod.rs`, `src-tauri/src/commands/mod.rs`

**Interfaces:**
- Consumes: Task 3 的 `Db`/`StorageError`
- Produces:
  - `storage::meta::Meta { data_epoch: String, revision: i64 }`
  - `storage::meta::read_meta(conn: &Connection) -> Result<Meta, StorageError>`
  - `storage::meta::init_meta(conn: &Connection, epoch: &str) -> Result<(), StorageError>` —— 迁移后的库由它写入唯一一行
  - `commands::envelope::AppError`（`thiserror`，含 `code()` 方法返回 `&'static str`）
  - `commands::envelope::WriteEnvelope { expected_data_epoch: String, expected_row_version: Option<i64> }`
  - `commands::envelope::guard_epoch(conn, &str) -> Result<(), AppError>`
  - `commands::envelope::guard_row_version(actual: i64, expected: i64) -> Result<(), AppError>`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/commands/envelope.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::db::Db;
    use crate::storage::migrations::migrate;
    use crate::storage::meta::{bump_revision, init_meta, read_meta, Meta};

    fn db_with_meta(epoch: &str) -> Db {
        let db = Db::open_in_memory().unwrap();
        migrate(db.conn()).unwrap();
        init_meta(db.conn(), epoch).unwrap();
        db
    }

    #[test]
    fn new_database_starts_at_revision_zero() {
        let db = db_with_meta("epoch-a");
        assert_eq!(read_meta(db.conn()).unwrap(), Meta { data_epoch: "epoch-a".into(), revision: 0 });
    }

    #[test]
    fn write_transaction_bumps_revision_by_exactly_one() {
        let db = db_with_meta("epoch-a");
        assert_eq!(bump_revision(db.conn()).unwrap(), 1);
        assert_eq!(bump_revision(db.conn()).unwrap(), 2);
        assert_eq!(read_meta(db.conn()).unwrap().revision, 2);
    }

    #[test]
    fn stale_epoch_is_rejected_with_the_contract_code() {
        let db = db_with_meta("epoch-a");
        let err = guard_epoch(db.conn(), "epoch-old").unwrap_err();
        assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
        guard_epoch(db.conn(), "epoch-a").unwrap();
    }

    #[test]
    fn stale_row_version_is_rejected_with_the_contract_code() {
        let err = guard_row_version(7, 3).unwrap_err();
        assert_eq!(err.code(), "VERSION_CONFLICT");
        assert!(guard_row_version(3, 3).is_ok());
    }

    #[test]
    fn error_carries_no_database_identity() {
        // 00 §4 要求 detail 脱敏。做法是让变体根本不持有 epoch 字符串——
        // 这条断言的作用是：以后谁往 DataEpochMismatch 里塞字段，测试立刻红。
        let db = db_with_meta("epoch-secret-value");
        let err = guard_epoch(db.conn(), "wrong").unwrap_err();

        let rendered = format!("{err:?} {err}");
        assert!(
            !rendered.contains("epoch-secret-value"),
            "错误文本不得带库身份：{rendered}"
        );
        assert!(!err.message().contains("epoch-secret-value"));
        assert_eq!(err.message(), "数据已被恢复或替换，请刷新后重试。");
    }
}
```

需要 `Meta` 实现 `PartialEq` 与 `Debug`；把这两条写进 `Meta` 的 derive。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib commands`
Expected: 编译失败，`cannot find function init_meta` 等。

- [ ] **Step 3: 实现 meta 与信封**

`src-tauri/src/storage/meta.rs`：

```rust
//! app_meta：库身份（data_epoch）与业务 revision。
//!
//! 00 §5：同一 epoch 内业务写事务 revision 恰好 +1；心跳/tick/纯读不加。

use crate::storage::db::StorageError;
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub data_epoch: String,
    pub revision: i64,
}

/// 迁移后写入唯一一行。重复调用不覆盖已有 epoch。
pub fn init_meta(conn: &Connection, epoch: &str) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO app_meta(singleton, data_epoch, revision)
         VALUES (1, ?1, 0)
         ON CONFLICT(singleton) DO NOTHING",
        [epoch],
    )?;
    Ok(())
}

pub fn read_meta(conn: &Connection) -> Result<Meta, StorageError> {
    let row = conn
        .query_row(
            "SELECT data_epoch, revision FROM app_meta WHERE singleton = 1",
            [],
            |r| Ok(Meta { data_epoch: r.get(0)?, revision: r.get(1)? }),
        )
        .optional()?;
    row.ok_or(StorageError::MetaMissing)
}

/// 业务写事务调用一次；返回新 revision。
pub fn bump_revision(conn: &Connection) -> Result<i64, StorageError> {
    conn.execute("UPDATE app_meta SET revision = revision + 1 WHERE singleton = 1", [])?;
    Ok(read_meta(conn)?.revision)
}
```

在 `StorageError` 上补一个变体：

```rust
    #[error("app_meta has no singleton row; was migrate() called?")]
    MetaMissing,
```

`src-tauri/src/commands/envelope.rs`：

```rust
//! 命令信封：每次业务写入都要过的两道闸。
//!
//! 00 §4/§5：修改命令带 expected_data_epoch 与 expected_row_version；
//! epoch 不匹配返回 DATA_EPOCH_MISMATCH 且不写入；版本不匹配返回 VERSION_CONFLICT。

use crate::storage::db::StorageError;
use crate::storage::meta::read_meta;
use rusqlite::Connection;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // 刻意不持有 expected/actual：epoch 是库身份，00 §4 要求错误 detail 脱敏。
    // 前端只需要 code 就能分支，排查靠诊断日志而非错误文本。
    #[error("the database identity changed; a fresh snapshot is required")]
    DataEpochMismatch,

    #[error("the record was modified by someone else; reload and retry")]
    VersionConflict { expected: i64, actual: i64 },

    #[error(transparent)]
    Domain(#[from] crate::domain::error::DomainError),

    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl AppError {
    /// 契约错误码，前端按它分支（00 §4）。
    pub fn code(&self) -> &'static str {
        match self {
            AppError::DataEpochMismatch => "DATA_EPOCH_MISMATCH",
            AppError::VersionConflict { .. } => "VERSION_CONFLICT",
            AppError::Domain(_) => "DOMAIN_ERROR",
            AppError::Storage(_) => "STORAGE_ERROR",
        }
    }

    /// 面向用户的消息。不得含 epoch 字面量等库身份信息。
    pub fn message(&self) -> String {
        match self {
            AppError::DataEpochMismatch =>
                "数据已被恢复或替换，请刷新后重试。".to_string(),
            AppError::VersionConflict { .. } =>
                "这条记录已被修改，请刷新后重试。".to_string(),
            AppError::Domain(e) => format!("操作不被允许：{e}"),
            AppError::Storage(e) => format!("存储错误：{e}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteEnvelope {
    pub expected_data_epoch: String,
    /// 新建实体时为 None；更新既有实体时必须给出。
    pub expected_row_version: Option<i64>,
}

pub fn guard_epoch(conn: &Connection, expected: &str) -> Result<(), AppError> {
    let actual = read_meta(conn)?.data_epoch;
    if actual != expected {
        return Err(AppError::DataEpochMismatch);
    }
    Ok(())
}

pub fn guard_row_version(actual: i64, expected: i64) -> Result<(), AppError> {
    if actual != expected {
        return Err(AppError::VersionConflict { expected, actual });
    }
    Ok(())
}
```

`src-tauri/src/commands/mod.rs` 与 `storage/mod.rs` 各补一行 `pub mod`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib`
Expected: 全过，其中 `error_detail_never_leaks_the_epoch_string` 必须绿——它把 00 §4 的脱敏要求钉住了。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/meta.rs src-tauri/src/commands/
git commit -m "feat(m01): app_meta 与 data_epoch/revision 命令信封"
```

---

### Task 6: 领域层 Task 状态机（02 §5）

**Files:**
- Create: `src-tauri/src/domain/error.rs`
- Create: `src-tauri/src/domain/task.rs`
- Modify: `src-tauri/src/domain/mod.rs`

**Interfaces:**
- Consumes: 无（纯逻辑）
- Produces:
  - `domain::error::DomainError`
  - `domain::task::TaskStatus`（`Inbox`/`Clarifying`/`Ready`/`Scheduled`/`Doing`/`Blocked`/`Waiting`/`Review`/`Done`/`Cancelled`，实现 `as_str`/`from_str`）
  - `domain::task::TransitionCause`（`Normal`/`ExplicitReopen`）
  - `domain::task::TaskStatus::transition(self, to, cause) -> Result<TaskStatus, DomainError>`
  - `domain::task::TaskStatus::allowed_targets(self) -> &'static [TaskStatus]`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/domain/task.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use TransitionCause::{ExplicitReopen, Normal};

    #[test]
    fn main_chain_advances_and_rolls_back() {
        assert!(TaskStatus::Inbox.transition(TaskStatus::Clarifying, Normal).is_ok());
        assert!(TaskStatus::Clarifying.transition(TaskStatus::Ready, Normal).is_ok());
        assert!(TaskStatus::Ready.transition(TaskStatus::Doing, Normal).is_ok());
        assert!(TaskStatus::Doing.transition(TaskStatus::Review, Normal).is_ok());
        assert!(TaskStatus::Review.transition(TaskStatus::Done, Normal).is_ok());
        // 02 §5：评审中检查失败回退到 Ready，不回退到 Doing
        assert!(TaskStatus::Review.transition(TaskStatus::Ready, Normal).is_ok());
        assert!(TaskStatus::Review.transition(TaskStatus::Doing, Normal).is_err());
        assert!(TaskStatus::Doing.transition(TaskStatus::Ready, Normal).is_ok());
    }

    #[test]
    fn inbox_can_reach_ready_directly() {
        // 02 §5：从 Inbox 直接 start 可在同一命令内先理清为 Ready
        assert!(TaskStatus::Inbox.transition(TaskStatus::Ready, Normal).is_ok());
    }

    #[test]
    fn every_status_can_be_cancelled_except_terminal_ones() {
        for s in [
            TaskStatus::Inbox, TaskStatus::Clarifying, TaskStatus::Ready, TaskStatus::Doing,
            TaskStatus::Blocked, TaskStatus::Waiting, TaskStatus::Review,
        ] {
            assert!(s.transition(TaskStatus::Cancelled, Normal).is_ok(), "{s:?} 应可取消");
        }
        assert!(TaskStatus::Done.transition(TaskStatus::Cancelled, Normal).is_err());
        assert!(TaskStatus::Cancelled.transition(TaskStatus::Done, Normal).is_err());
    }

    #[test]
    fn terminal_statuses_only_leave_via_explicit_reopen() {
        for t in [TaskStatus::Done, TaskStatus::Cancelled] {
            assert!(t.transition(TaskStatus::Ready, Normal).is_err(), "{t:?} 普通路径不得 reopen");
            assert_eq!(t.transition(TaskStatus::Ready, ExplicitReopen).unwrap(), TaskStatus::Ready);
            // 终态之间不能互跳
            assert!(TaskStatus::Done.transition(TaskStatus::Cancelled, ExplicitReopen).is_err());
        }
        // 非终态用 reopen 语义也应被拒（reopen 只对终态有意义）
        assert!(TaskStatus::Ready.transition(TaskStatus::Ready, ExplicitReopen).is_err());
    }

    #[test]
    fn blocked_and_waiting_only_return_to_ready() {
        for s in [TaskStatus::Blocked, TaskStatus::Waiting] {
            assert!(s.transition(TaskStatus::Ready, Normal).is_ok());
            assert!(s.transition(TaskStatus::Doing, Normal).is_err());
            assert!(s.transition(TaskStatus::Done, Normal).is_err());
        }
    }

    #[test]
    fn scheduled_is_not_open_in_v01() {
        // 02 §5：V0.1 不开放 Scheduled
        assert!(TaskStatus::Ready.transition(TaskStatus::Scheduled, Normal).is_err());
        assert!(TaskStatus::Scheduled.transition(TaskStatus::Doing, Normal).is_err());
    }

    #[test]
    fn self_transition_is_always_illegal() {
        for s in TaskStatus::ALL {
            assert!(s.transition(*s, Normal).is_err(), "{s:?} 不应能跃迁到自己");
        }
    }

    #[test]
    fn string_round_trip_is_stable() {
        for s in TaskStatus::ALL {
            assert_eq!(TaskStatus::from_str(s.as_str()).unwrap(), *s);
        }
        assert!(TaskStatus::from_str("nonsense").is_err());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib domain`
Expected: 编译失败，`cannot find type TaskStatus`。

- [ ] **Step 3: 实现领域层 Task**

`src-tauri/src/domain/error.rs`：

```rust
//! 领域错误。只描述规则被违反，不携带 IO 细节。

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("task cannot move from {from} to {to}")]
    IllegalTaskTransition { from: &'static str, to: &'static str },

    #[error("{status} is not open in V0.1")]
    StatusNotInVersion { status: &'static str },

    #[error("unknown task status: {0}")]
    UnknownTaskStatus(String),

    #[error("unknown session state: {0}")]
    UnknownSessionState(String),

    #[error("interval range is invalid: start={start} end={end}")]
    InvalidIntervalRange { start: i64, end: i64 },

    #[error("interval overlaps an existing interval in the same session")]
    IntervalOverlap,

    #[error("a running session must have exactly one open interval and no pending intervals")]
    RunningSessionInvariant,

    #[error("a recovering session must have a pending interval or an explicit fault flag")]
    RecoveringSessionInvariant,

    #[error("a paused session must not have an open interval")]
    PausedSessionInvariant,

    #[error("title must not be blank")]
    EmptyTitle,
}
```

`src-tauri/src/domain/task.rs`：

```rust
//! Task 状态机（02 §5）。纯逻辑，无 IO。

use crate::domain::error::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TaskStatus {
    Inbox,
    Clarifying,
    Ready,
    Scheduled,
    Doing,
    Blocked,
    Waiting,
    Review,
    Done,
    Cancelled,
}

/// 为什么发生跃迁。终态只能用显式 reopen 离开（02 §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionCause {
    Normal,
    ExplicitReopen,
}

impl TaskStatus {
    pub const ALL: &'static [TaskStatus] = &[
        TaskStatus::Inbox,
        TaskStatus::Clarifying,
        TaskStatus::Ready,
        TaskStatus::Scheduled,
        TaskStatus::Doing,
        TaskStatus::Blocked,
        TaskStatus::Waiting,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Cancelled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Inbox => "Inbox",
            TaskStatus::Clarifying => "Clarifying",
            TaskStatus::Ready => "Ready",
            TaskStatus::Scheduled => "Scheduled",
            TaskStatus::Doing => "Doing",
            TaskStatus::Blocked => "Blocked",
            TaskStatus::Waiting => "Waiting",
            TaskStatus::Review => "Review",
            TaskStatus::Done => "Done",
            TaskStatus::Cancelled => "Cancelled",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, DomainError> {
        Self::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| DomainError::UnknownTaskStatus(s.to_string()))
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Done | TaskStatus::Cancelled)
    }

    /// Scheduled 属 V0.2；V0.1 既不接受它作为目标，也不接受它作为起点。
    pub const fn is_open_in_v01(self) -> bool {
        !matches!(self, TaskStatus::Scheduled)
    }

    /// 02 §5 的跃迁表的可读形式。
    pub const fn allowed_targets(self) -> &'static [TaskStatus] {
        use TaskStatus::*;
        match self {
            Inbox => &[Clarifying, Ready, Cancelled],
            Clarifying => &[Inbox, Ready, Cancelled],
            Ready => &[Doing, Scheduled, Blocked, Waiting, Review, Done, Cancelled],
            Scheduled => &[Ready, Doing, Blocked, Waiting, Review, Done, Cancelled],
            Doing => &[Ready, Blocked, Waiting, Review, Done, Cancelled],
            Blocked | Waiting => &[Ready, Cancelled],
            Review => &[Ready, Done, Cancelled],
            Done | Cancelled => &[Ready],
        }
    }

    pub fn transition(self, to: TaskStatus, cause: TransitionCause) -> Result<TaskStatus, DomainError> {
        let illegal = || DomainError::IllegalTaskTransition { from: self.as_str(), to: to.as_str() };

        if !self.is_open_in_v01() || !to.is_open_in_v01() {
            return Err(DomainError::StatusNotInVersion {
                status: if !self.is_open_in_v01() { self.as_str() } else { to.as_str() },
            });
        }
        if self == to {
            return Err(illegal());
        }

        if self.is_terminal() {
            // 终态只有显式 reopen 一条出边：→ Ready
            return match (cause, to) {
                (TransitionCause::ExplicitReopen, TaskStatus::Ready) => Ok(TaskStatus::Ready),
                _ => Err(illegal()),
            };
        }

        if cause == TransitionCause::ExplicitReopen {
            // reopen 只对终态有意义
            return Err(illegal());
        }

        if self.allowed_targets().contains(&to) {
            Ok(to)
        } else {
            Err(illegal())
        }
    }
}
```

`src-tauri/src/domain/mod.rs`：

```rust
//! 领域层：实体、跃迁表与校验。禁止 IO。

pub mod error;
pub mod task;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib domain`
Expected: `test result: ok. 7 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/domain/
git commit -m "feat(m02): Task 状态机按 02 §5 落地"
```

---

### Task 7: 领域层 Session/Interval 与不变量

**Files:**
- Create: `src-tauri/src/domain/session.rs`
- Create: `src-tauri/src/domain/interval.rs`
- Modify: `src-tauri/src/domain/mod.rs`

**Interfaces:**
- Consumes: Task 6 的 `DomainError`
- Produces:
  - `domain::session::SessionState`（`Running`/`Paused`/`Recovering`/`Finished`/`Discarded`）
  - `domain::session::SessionMode`（`Foreground`/`Background`/`Passive`/`Waiting`）
  - `domain::session::TimerKind`（`Stopwatch`/`Countdown`）
  - `domain::session::SessionTransition`、`SessionState::transition(self, t) -> Result<SessionState, DomainError>`
  - `domain::session::SessionState::check_invariants(self, iv: &IntervalFacts) -> Result<(), DomainError>`
  - `domain::interval::IntervalFacts { open_count: usize, pending_count: usize, closed_trusted_count: usize }`
  - `domain::interval::IntervalRange { started_at: i64, ended_at: Option<i64> }` 与 `validate()`、`overlaps()`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/domain/session.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::interval::{IntervalFacts, IntervalRange};

    fn facts(open: usize, pending: usize, closed: usize) -> IntervalFacts {
        IntervalFacts { open_count: open, pending_count: pending, closed_trusted_count: closed }
    }

    #[test]
    fn pause_resume_stay_in_the_same_session() {
        // 02 §3：暂停后恢复仍是同一 session
        assert_eq!(SessionState::Running.transition(SessionTransition::Pause).unwrap(), SessionState::Paused);
        assert_eq!(SessionState::Paused.transition(SessionTransition::Resume).unwrap(), SessionState::Running);
    }

    #[test]
    fn finish_is_reachable_from_running_and_paused() {
        // 02 §3：paused 也可直接 finish
        for s in [SessionState::Running, SessionState::Paused] {
            assert_eq!(s.transition(SessionTransition::Finish).unwrap(), SessionState::Finished);
        }
    }

    #[test]
    fn recovering_confirms_to_finished_or_paused() {
        assert_eq!(
            SessionState::Recovering.transition(SessionTransition::ConfirmToFinished).unwrap(),
            SessionState::Finished
        );
        assert_eq!(
            SessionState::Recovering.transition(SessionTransition::ConfirmToPaused).unwrap(),
            SessionState::Paused
        );
    }

    #[test]
    fn discard_session_is_reachable_from_every_non_terminal_state() {
        for s in [SessionState::Running, SessionState::Paused, SessionState::Recovering] {
            assert_eq!(s.transition(SessionTransition::DiscardSession).unwrap(), SessionState::Discarded);
        }
        assert!(SessionState::Finished.transition(SessionTransition::DiscardSession).is_err());
    }

    #[test]
    fn resume_is_not_available_to_recovering() {
        // 02 §4：recovering 必须由用户确认，不能自己回到运行
        assert!(SessionState::Recovering.transition(SessionTransition::Resume).is_err());
        assert!(SessionState::Recovering.transition(SessionTransition::Pause).is_err());
    }

    #[test]
    fn running_requires_one_open_interval_and_no_pending() {
        assert!(SessionState::Running.check_invariants(&facts(1, 0, 3)).is_ok());
        assert!(SessionState::Running.check_invariants(&facts(0, 0, 3)).is_err());
        assert!(SessionState::Running.check_invariants(&facts(1, 1, 3)).is_err());
        assert!(SessionState::Running.check_invariants(&facts(2, 0, 3)).is_err());
    }

    #[test]
    fn paused_requires_no_open_interval() {
        assert!(SessionState::Paused.check_invariants(&facts(0, 0, 3)).is_ok());
        assert!(SessionState::Paused.check_invariants(&facts(1, 0, 3)).is_err());
    }

    #[test]
    fn recovering_requires_a_pending_interval() {
        assert!(SessionState::Recovering.check_invariants(&facts(1, 1, 2)).is_ok());
        assert!(SessionState::Recovering.check_invariants(&facts(0, 0, 2)).is_err());
    }

    #[test]
    fn interval_range_validates_and_detects_overlap() {
        let a = IntervalRange { started_at: 100, ended_at: Some(200) };
        let b = IntervalRange { started_at: 150, ended_at: Some(250) }; // 与 a 相交
        let c = IntervalRange { started_at: 200, ended_at: Some(300) }; // 端点相接
        let d = IntervalRange { started_at: 300, ended_at: None };      // 开放区间
        let e = IntervalRange { started_at: 250, ended_at: None };      // 与 c 相交的开放区间

        a.validate().unwrap();
        assert!(a.overlaps(&b));
        assert!(!a.overlaps(&c), "半开区间 [100,200) 与 [200,300) 不重叠");
        assert!(!a.overlaps(&d), "[100,200) 与 [300,∞) 不重叠");
        assert!(!c.overlaps(&d), "[200,300) 与 [300,∞) 端点相接，不算重叠");
        assert!(c.overlaps(&e), "[200,300) 与 [250,∞) 重叠");

        let bad = IntervalRange { started_at: 300, ended_at: Some(200) };
        assert!(bad.validate().is_err());
        let negative = IntervalRange { started_at: -1, ended_at: Some(10) };
        assert!(negative.validate().is_err());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib domain`
Expected: 编译失败，`cannot find type SessionState`。

- [ ] **Step 3: 实现 session 与 interval**

`src-tauri/src/domain/interval.rs`：

```rust
//! 区间的范围校验与重叠判定（02 §3、§6）。
//!
//! 统计范围是**半开区间** [start, end)：端点相接不算重叠。

use crate::domain::error::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalFacts {
    pub open_count: usize,
    pub pending_count: usize,
    pub closed_trusted_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalRange {
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

impl IntervalRange {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.started_at < 0 {
            return Err(DomainError::InvalidIntervalRange {
                start: self.started_at,
                end: self.ended_at.unwrap_or(-1),
            });
        }
        if let Some(end) = self.ended_at {
            if end < self.started_at || end < 0 {
                return Err(DomainError::InvalidIntervalRange { start: self.started_at, end });
            }
        }
        Ok(())
    }

    /// 半开区间相交判定；开放区间（ended_at = None）视为延伸到正无穷。
    pub fn overlaps(&self, other: &Self) -> bool {
        const INF: i64 = i64::MAX;
        let a_start = self.started_at;
        let a_end = self.ended_at.unwrap_or(INF);
        let b_start = other.started_at;
        let b_end = other.ended_at.unwrap_or(INF);
        a_start < b_end && b_start < a_end
    }
}
```

`src-tauri/src/domain/session.rs`：

```rust
//! 会话状态与区间不变量（02 §3、§4）。纯逻辑，无 IO。

use crate::domain::error::DomainError;
use crate::domain::interval::IntervalFacts;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionState {
    Running,
    Paused,
    Recovering,
    Finished,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Foreground,
    Background,
    Passive,
    Waiting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    Stopwatch,
    Countdown,
    // V0.2 增 Pomodoro
}

impl SessionState {
    pub const ALL: &'static [SessionState] = &[
        SessionState::Running,
        SessionState::Paused,
        SessionState::Recovering,
        SessionState::Finished,
        SessionState::Discarded,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            SessionState::Running => "running",
            SessionState::Paused => "paused",
            SessionState::Recovering => "recovering",
            SessionState::Finished => "finished",
            SessionState::Discarded => "discarded",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, DomainError> {
        Self::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| DomainError::UnknownSessionState(s.to_string()))
    }

    pub fn transition(self, t: SessionTransition) -> Result<SessionState, DomainError> {
        use SessionState::*;
        use SessionTransition::*;
        let illegal = || DomainError::IllegalSessionTransition {
            from: self.as_str(),
            to: "?",
        };
        match (self, t) {
            (Running, Pause) => Ok(Paused),
            (Paused, Resume) => Ok(Running),
            (Running, Finish) | (Paused, Finish) => Ok(Finished),
            (Recovering, ConfirmToFinished) => Ok(Finished),
            (Recovering, ConfirmToPaused) => Ok(Paused),
            (Recovering, DiscardUncertain) => Ok(Finished),
            (Running, DiscardSession) | (Paused, DiscardSession) | (Recovering, DiscardSession) => {
                Ok(Discarded)
            }
            _ => Err(illegal()),
        }
    }

    /// 02 §4 的不变量：状态与区间事实必须互相一致。
    pub fn check_invariants(self, f: &IntervalFacts) -> Result<(), DomainError> {
        match self {
            SessionState::Running => {
                if f.open_count != 1 || f.pending_count != 0 {
                    return Err(DomainError::RunningSessionInvariant);
                }
            }
            SessionState::Paused => {
                if f.open_count != 0 {
                    return Err(DomainError::PausedSessionInvariant);
                }
            }
            SessionState::Recovering => {
                if f.pending_count == 0 && f.open_count == 0 {
                    return Err(DomainError::RecoveringSessionInvariant);
                }
            }
            SessionState::Finished | SessionState::Discarded => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTransition {
    Pause,
    Resume,
    Finish,
    ConfirmToFinished,
    ConfirmToPaused,
    DiscardUncertain,
    DiscardSession,
}
```

需要在 `domain/error.rs` 补一个变体：

```rust
    #[error("session cannot apply {from} -> {to}")]
    IllegalSessionTransition { from: &'static str, to: &'static str },
```

`domain/mod.rs` 补 `pub mod interval;` 与 `pub mod session;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib domain`
Expected: `test result: ok. 16 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/domain/
git commit -m "feat(m02): Session/Interval 状态机与不变量"
```

---

### Task 8: 仓储——Task 的建、取与跃迁

**Files:**
- Create: `src-tauri/src/storage/task_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`

**Interfaces:**
- Consumes: Task 3 的 `Db`/`StorageError`、Task 5 的 `bump_revision`/`AppError`、Task 6 的 `TaskStatus`/`TransitionCause`
- Produces:
  - `storage::task_repo::TaskRow { id: String, title: String, status: TaskStatus, project_id: Option<String>, row_version: i64, created_at: i64, updated_at: i64 }`
  - `storage::task_repo::create_task(conn: &Connection, title: &str, project_id: Option<&str>, now_ms: i64) -> Result<TaskRow, AppError>` —— id 由内部生成；标题 trim 后为空则拒绝
  - `storage::task_repo::get_task(conn, id: &str) -> Result<Option<TaskRow>, AppError>`
  - `storage::task_repo::transition_task(conn, id: &str, to: TaskStatus, cause: TransitionCause, now_ms: i64) -> Result<TaskRow, AppError>` —— 先读当前版本再委托给下面的 `_expecting` 版本
  - `storage::task_repo::transition_task_expecting(conn, id: &str, expected_row_version: i64, to: TaskStatus, cause: TransitionCause, now_ms: i64) -> Result<TaskRow, AppError>` —— 内部**同一个事务**里：校验版本 → 改状态 → 写 `task_change` → `row_version + 1` → `bump_revision`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/storage/task_repo.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::task::TransitionCause;
    use crate::storage::db::Db;
    use crate::storage::meta::{init_meta, read_meta};
    use crate::storage::migrations::migrate;

    fn db() -> Db {
        let db = Db::open_in_memory().unwrap();
        migrate(db.conn()).unwrap();
        init_meta(db.conn(), "epoch-a").unwrap();
        db
    }

    #[test]
    fn create_task_starts_in_inbox_at_version_zero() {
        let db = db();
        let t = create_task(db.conn(), "  写周报  ", None, 1_000).unwrap();
        assert_eq!(t.status, TaskStatus::Inbox);
        assert_eq!(t.row_version, 0);
        assert_eq!(t.title, "写周报", "标题应被 trim");
        assert_eq!(read_meta(db.conn()).unwrap().revision, 1, "建实体是一次业务写");
    }

    #[test]
    fn blank_title_is_rejected_before_touching_the_database() {
        let db = db();
        let err = create_task(db.conn(), "   ", None, 1_000).unwrap_err();
        assert_eq!(err.code(), "DOMAIN_ERROR");
        let n: i64 = db.conn().query_row("SELECT count(*) FROM task", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "拒绝时不得留下半条记录");
        assert_eq!(read_meta(db.conn()).unwrap().revision, 0, "失败不得增加 revision");
    }

    #[test]
    fn legal_transition_bumps_row_version_revision_and_writes_audit() {
        let db = db();
        let t = create_task(db.conn(), "T", None, 1_000).unwrap();

        let t = transition_task(db.conn(), &t.id, TaskStatus::Ready, TransitionCause::Normal, 2_000).unwrap();
        assert_eq!(t.status, TaskStatus::Ready);
        assert_eq!(t.row_version, 1);
        assert_eq!(t.updated_at, 2_000);
        assert_eq!(read_meta(db.conn()).unwrap().revision, 2);

        let (before, after): (String, String) = db
            .conn()
            .query_row("SELECT before_json, after_json FROM task_change WHERE task_id = ?1",
                       [&t.id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert!(before.contains("\"Inbox\""), "审计要留下跃迁前状态：{before}");
        assert!(after.contains("\"Ready\""), "审计要留下跃迁后状态：{after}");
    }

    #[test]
    fn illegal_transition_writes_nothing_at_all() {
        let db = db();
        let t = create_task(db.conn(), "T", None, 1_000).unwrap();
        transition_task(db.conn(), &t.id, TaskStatus::Ready, TransitionCause::Normal, 2_000).unwrap();

        // Ready → Clarifying 不在 02 §5 的允许集里
        let err = transition_task(db.conn(), &t.id, TaskStatus::Clarifying, TransitionCause::Normal, 3_000).unwrap_err();
        assert_eq!(err.code(), "DOMAIN_ERROR");

        let after = get_task(db.conn(), &t.id).unwrap().unwrap();
        assert_eq!(after.status, TaskStatus::Ready, "非法跃迁不得改状态");
        assert_eq!(after.row_version, 1, "非法跃迁不得动 row_version");
        assert_eq!(read_meta(db.conn()).unwrap().revision, 2, "非法跃迁不得增加 revision");
        let n: i64 = db.conn().query_row("SELECT count(*) FROM task_change", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "非法跃迁不得写审计");
    }

    #[test]
    fn stale_row_version_is_rejected() {
        let db = db();
        let t = create_task(db.conn(), "T", None, 1_000).unwrap();
        transition_task(db.conn(), &t.id, TaskStatus::Ready, TransitionCause::Normal, 2_000).unwrap();
        // 调用方手上还是 row_version=0 的旧副本
        let err = transition_task_expecting(db.conn(), &t.id, 0, TaskStatus::Doing, TransitionCause::Normal, 3_000)
            .unwrap_err();
        assert_eq!(err.code(), "VERSION_CONFLICT");
    }

    #[test]
    fn transition_to_scheduled_is_rejected_in_v01() {
        let db = db();
        let t = create_task(db.conn(), "T", None, 1_000).unwrap();
        transition_task(db.conn(), &t.id, TaskStatus::Ready, TransitionCause::Normal, 2_000).unwrap();
        let err = transition_task(db.conn(), &t.id, TaskStatus::Scheduled, TransitionCause::Normal, 3_000)
            .unwrap_err();
        assert_eq!(err.code(), "DOMAIN_ERROR");
    }
}
```

注意测试里用到一个带期望版本的变体，Step 3 要一并实现（签名见上面的 Interfaces）。

`transition_task` 就是它的薄包装（先读当前版本再调用它）。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib task_repo`
Expected: 编译失败，`cannot find function create_task`。

- [ ] **Step 3: 实现 task_repo**

**实现要点：** `AppError` 的 `Domain` 变体与 `DOMAIN_ERROR` 码在 Task 5 已经建好，本任务直接 `?` 即可——领域层返回 `Err` 时事务在 `drop` 时回滚，天然满足"非法跃迁不写库"。

`src-tauri/src/storage/task_repo.rs`：

```rust
//! Task 仓储。每次业务写都在一个事务里完成：改实体 + 写审计 + 加版本 + 加 revision。

use crate::commands::envelope::{guard_row_version, AppError, WriteEnvelope};
use crate::domain::error::DomainError;
use crate::domain::task::{TaskStatus, TransitionCause};
use crate::storage::meta::{bump_revision, read_meta};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub project_id: Option<String>,
    pub row_version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    let status: String = r.get("status")?;
    Ok(TaskRow {
        id: r.get("id")?,
        title: r.get("title")?,
        // 库里出现未知状态说明数据被外部改过；这里退化成 Inbox 会在别处造成误判，
        // 因此 map_err 成 FromSqlConversionFailure 让读取整体失败。
        status: TaskStatus::from_str(&status).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, status.clone())),
            )
        })?,
        project_id: r.get("project_id")?,
        row_version: r.get("row_version")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

const SELECT: &str =
    "SELECT id, title, status, project_id, row_version, created_at, updated_at FROM task";

pub fn get_task(conn: &Connection, id: &str) -> Result<Option<TaskRow>, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    Ok(conn.query_row(&sql, [id], read_row).optional()?)
}

pub fn create_task(
    conn: &Connection,
    title: &str,
    project_id: Option<&str>,
    now_ms: i64,
) -> Result<TaskRow, AppError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(DomainError::EmptyTitle.into());
    }
    let id = Uuid::new_v4().to_string();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO task(id, project_id, title, status, row_version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'Inbox', 0, ?4, ?4)",
        rusqlite::params![id, project_id, title, now_ms],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(TaskRow {
        id,
        title: title.to_string(),
        status: TaskStatus::Inbox,
        project_id: project_id.map(str::to_string),
        row_version: 0,
        created_at: now_ms,
        updated_at: now_ms,
    })
}

pub fn transition_task(
    conn: &Connection,
    id: &str,
    to: TaskStatus,
    cause: TransitionCause,
    now_ms: i64,
) -> Result<TaskRow, AppError> {
    let current = get_task(conn, id)?
        .ok_or_else(|| AppError::Domain(DomainError::UnknownTaskStatus(id.to_string())))?;
    transition_task_expecting(conn, &current.id, current.row_version, to, cause, now_ms)
}

pub fn transition_task_expecting(
    conn: &Connection,
    id: &str,
    expected_row_version: i64,
    to: TaskStatus,
    cause: TransitionCause,
    now_ms: i64,
) -> Result<TaskRow, AppError> {
    let tx = conn.unchecked_transaction()?;
    let current = {
        let sql = format!("{SELECT} WHERE id = ?1");
        tx.query_row(&sql, [id], read_row).optional()?
            .ok_or_else(|| AppError::Domain(DomainError::UnknownTaskStatus(id.to_string())))?
    };

    guard_row_version(current.row_version, expected_row_version)?;
    // 领域层说了算；它返回 Err 时事务在 drop 时回滚，一条审计都不会留
    let next = current.status.transition(to, cause)?;

    let before = format!("{{\"status\":\"{}\"}}", current.status.as_str());
    let after = format!("{{\"status\":\"{}\"}}", next.as_str());
    tx.execute(
        "INSERT INTO task_change(id, task_id, before_json, after_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![Uuid::new_v4().to_string(), id, before, after, now_ms],
    )?;
    tx.execute(
        "UPDATE task SET status = ?1, row_version = row_version + 1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![next.as_str(), now_ms, id],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;

    Ok(TaskRow { status: next, row_version: current.row_version + 1, updated_at: now_ms, ..current })
}
```

`WriteEnvelope` 在本任务只作为类型存在；`storage/mod.rs` 补 `pub mod task_repo;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib task_repo`
Expected: `test result: ok. 6 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/task_repo.rs src-tauri/src/storage/mod.rs src-tauri/src/commands/envelope.rs
git commit -m "feat(m01,m02): Task 仓储含审计、版本与 revision"
```

---

### Task 9: 仓储——Session 与 Interval

**Files:**
- Create: `src-tauri/src/storage/session_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`

**Interfaces:**
- Consumes: Task 2 的 `Clock`、Task 3/5/7 的 `Db`/`StorageError`/`bump_revision`/`SessionState`/`IntervalFacts`
- Produces:
  - `storage::session_repo::create_session(conn, task_id, run_id, mode, timer_kind, now_ms) -> Result<String, AppError>` —— 同事务建 session 与第一条开放区间
  - `storage::session_repo::pause_session(conn, session_id, now_ms) -> Result<(), AppError>` —— 闭合开放区间并置 paused
  - `storage::session_repo::resume_session(conn, session_id, now_ms) -> Result<(), AppError>` —— 开新区间并置 running
  - `storage::session_repo::interval_facts(conn, session_id) -> Result<IntervalFacts, AppError>`
  - `storage::session_repo::assert_invariants(conn, session_id) -> Result<(), AppError>`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/storage/session_repo.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session::{SessionMode, SessionState, TimerKind};
    use crate::storage::db::Db;
    use crate::storage::meta::{init_meta, read_meta};
    use crate::storage::migrations::migrate;

    fn seeded() -> Db {
        let db = Db::open_in_memory().unwrap();
        migrate(db.conn()).unwrap();
        init_meta(db.conn(), "epoch-a").unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO application_run(id, started_at) VALUES ('run-1', 0);
                 INSERT INTO task(id, title, status, row_version, created_at, updated_at)
                   VALUES ('t1', 'T', 'Doing', 0, 0, 0);",
            )
            .unwrap();
        db
    }

    fn state_of(conn: &rusqlite::Connection, id: &str) -> SessionState {
        let s: String = conn
            .query_row("SELECT state FROM work_session WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        SessionState::from_str(&s).unwrap()
    }

    #[test]
    fn start_creates_one_open_interval_and_passes_invariants() {
        let db = seeded();
        let sid = create_session(db.conn(), "t1", "run-1", SessionMode::Foreground, TimerKind::Stopwatch, 1_000).unwrap();
        assert_eq!(state_of(db.conn(), &sid), SessionState::Running);

        let f = interval_facts(db.conn(), &sid).unwrap();
        assert_eq!((f.open_count, f.pending_count, f.closed_trusted_count), (1, 0, 0));
        assert_invariants(db.conn(), &sid).unwrap();
        assert_eq!(read_meta(db.conn()).unwrap().revision, 1);
    }

    #[test]
    fn pause_closes_the_interval_and_freezes_duration() {
        let db = seeded();
        let sid = create_session(db.conn(), "t1", "run-1", SessionMode::Foreground, TimerKind::Stopwatch, 1_000).unwrap();
        pause_session(db.conn(), &sid, 61_000).unwrap();

        assert_eq!(state_of(db.conn(), &sid), SessionState::Paused);
        let (ended, dur, review): (i64, i64, i64) = db
            .conn()
            .query_row(
                "SELECT ended_at, duration_ms, needs_review FROM work_interval WHERE session_id = ?1",
                [&sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(ended, 61_000);
        assert_eq!(dur, 60_000, "duration_ms 必须等于 ended_at - started_at");
        assert_eq!(review, 0);
        assert_invariants(db.conn(), &sid).unwrap();
    }

    #[test]
    fn resume_opens_a_new_interval_and_preserves_the_first() {
        let db = seeded();
        let sid = create_session(db.conn(), "t1", "run-1", SessionMode::Foreground, TimerKind::Stopwatch, 1_000).unwrap();
        pause_session(db.conn(), &sid, 61_000).unwrap();
        resume_session(db.conn(), &sid, 121_000).unwrap();

        assert_eq!(state_of(db.conn(), &sid), SessionState::Running);
        let f = interval_facts(db.conn(), &sid).unwrap();
        assert_eq!((f.open_count, f.pending_count, f.closed_trusted_count), (1, 0, 1));
        assert_invariants(db.conn(), &sid).unwrap();
    }

    #[test]
    fn pausing_twice_is_rejected_without_touching_state() {
        let db = seeded();
        let sid = create_session(db.conn(), "t1", "run-1", SessionMode::Foreground, TimerKind::Stopwatch, 1_000).unwrap();
        pause_session(db.conn(), &sid, 61_000).unwrap();
        let rev_after_first = read_meta(db.conn()).unwrap().revision;

        let err = pause_session(db.conn(), &sid, 62_000).unwrap_err();
        assert_eq!(err.code(), "DOMAIN_ERROR");
        assert_eq!(read_meta(db.conn()).unwrap().revision, rev_after_first);
        let n: i64 = db.conn().query_row("SELECT count(*) FROM work_interval", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_second_foreground_session_cannot_start() {
        // 02 §6 的部分唯一索引；这条在领域层没有对应规则，必须由库兜住
        let db = seeded();
        create_session(db.conn(), "t1", "run-1", SessionMode::Foreground, TimerKind::Stopwatch, 1_000).unwrap();
        let err = create_session(db.conn(), "t1", "run-1", SessionMode::Foreground, TimerKind::Stopwatch, 2_000).unwrap_err();
        assert_eq!(err.code(), "STORAGE_ERROR");
        let n: i64 = db.conn().query_row("SELECT count(*) FROM work_session", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "失败不得留下半个 session");
    }

    #[test]
    fn invariants_catch_a_hand_corrupted_row() {
        // 模拟外部改库：running 却把区间闭合掉
        let db = seeded();
        let sid = create_session(db.conn(), "t1", "run-1", SessionMode::Foreground, TimerKind::Stopwatch, 1_000).unwrap();
        db.conn()
            .execute("UPDATE work_interval SET ended_at = 5000, duration_ms = 4000 WHERE session_id = ?1", [&sid])
            .unwrap();
        let err = assert_invariants(db.conn(), &sid).unwrap_err();
        assert_eq!(err.code(), "DOMAIN_ERROR");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib session_repo`
Expected: 编译失败，`cannot find function create_session`。

- [ ] **Step 3: 实现 session_repo**

```rust
//! Session 与 Interval 仓储（02 §3、§4、§6）。
//!
//! 每个命令在**一个事务**里完成：区间事实 + 会话状态 + revision。
//! 提交失败不留下半个状态——这是 02 §6「操作失败不得出现半个审计记录」的前半条。

use crate::commands::envelope::AppError;
use crate::domain::error::DomainError;
use crate::domain::interval::IntervalFacts;
use crate::domain::session::{SessionMode, SessionState, SessionTransition, TimerKind};
use crate::storage::meta::bump_revision;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

fn session_state(conn: &Connection, id: &str) -> Result<SessionState, AppError> {
    let s: String = conn
        .query_row("SELECT state FROM work_session WHERE id = ?1", [id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| AppError::Domain(DomainError::UnknownSessionState(id.to_string())))?;
    SessionState::from_str(&s).map_err(AppError::from)
}

pub fn interval_facts(conn: &Connection, session_id: &str) -> Result<IntervalFacts, AppError> {
    let (open, pending, closed): (i64, i64, i64) = conn.query_row(
        "SELECT
           COALESCE(SUM(CASE WHEN ended_at IS NULL AND voided_at IS NULL THEN 1 ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN needs_review = 1 THEN 1 ELSE 0 END), 0),
           COALESCE(SUM(CASE WHEN ended_at IS NOT NULL AND needs_review = 0
                              AND voided_at IS NULL THEN 1 ELSE 0 END), 0)
         FROM work_interval WHERE session_id = ?1",
        [session_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(IntervalFacts {
        open_count: open as usize,
        pending_count: pending as usize,
        closed_trusted_count: closed as usize,
    })
}

/// 跑当前会话的不变量；用于命令提交前自检与恢复扫描。
pub fn assert_invariants(conn: &Connection, session_id: &str) -> Result<(), AppError> {
    let state = session_state(conn, session_id)?;
    let facts = interval_facts(conn, session_id)?;
    state.check_invariants(&facts).map_err(AppError::from)
}

pub fn create_session(
    conn: &Connection,
    task_id: &str,
    run_id: &str,
    mode: SessionMode,
    timer_kind: TimerKind,
    now_ms: i64,
) -> Result<String, AppError> {
    let sid = Uuid::new_v4().to_string();
    let mode_str = match mode {
        SessionMode::Foreground => "FOREGROUND",
        SessionMode::Background => "BACKGROUND",
        SessionMode::Passive => "PASSIVE",
        SessionMode::Waiting => "WAITING",
    };
    let kind_str = match timer_kind {
        TimerKind::Stopwatch => "stopwatch",
        TimerKind::Countdown => "countdown",
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO work_session(id, task_id, run_id, mode, state, timer_kind,
                                  started_at, last_heartbeat_at, row_version)
         VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?6, 0)",
        rusqlite::params![sid, task_id, run_id, mode_str, kind_str, now_ms],
    )?;
    tx.execute(
        "INSERT INTO work_interval(id, session_id, started_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![Uuid::new_v4().to_string(), sid, now_ms],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(sid)
}

pub fn pause_session(conn: &Connection, session_id: &str, now_ms: i64) -> Result<(), AppError> {
    let tx = conn.unchecked_transaction()?;
    let state = session_state(&tx, session_id)?;
    let next = state.transition(SessionTransition::Pause)?;

    let open_id: Option<String> = tx
        .query_row(
            "SELECT id FROM work_interval
              WHERE session_id = ?1 AND ended_at IS NULL AND voided_at IS NULL",
            [session_id],
            |r| r.get(0),
        )
        .optional()?;
    let open_id = open_id.ok_or(AppError::Domain(DomainError::RunningSessionInvariant))?;

    let started_at: i64 = tx.query_row(
        "SELECT started_at FROM work_interval WHERE id = ?1",
        [&open_id],
        |r| r.get(0),
    )?;
    // 02 §3：可信闭合满足 duration_ms = ended_at - started_at
    tx.execute(
        "UPDATE work_interval
            SET ended_at = ?1, duration_ms = ?1 - started_at, needs_review = 0
          WHERE id = ?2",
        rusqlite::params![now_ms, open_id],
    )?;
    tx.execute(
        "UPDATE work_session SET state = ?1, row_version = row_version + 1 WHERE id = ?2",
        rusqlite::params![next.as_str(), session_id],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(())
}

pub fn resume_session(conn: &Connection, session_id: &str, now_ms: i64) -> Result<(), AppError> {
    let tx = conn.unchecked_transaction()?;
    let state = session_state(&tx, session_id)?;
    let next = state.transition(SessionTransition::Resume)?;

    tx.execute(
        "INSERT INTO work_interval(id, session_id, started_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![Uuid::new_v4().to_string(), session_id, now_ms],
    )?;
    tx.execute(
        "UPDATE work_session SET state = ?1, last_heartbeat_at = ?2, row_version = row_version + 1
          WHERE id = ?3",
        rusqlite::params![next.as_str(), now_ms, session_id],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(())
}
```

`storage/mod.rs` 补 `pub mod session_repo;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib session_repo`
Expected: `test result: ok. 6 passed`。其中 `a_second_foreground_session_cannot_start` 证明部分唯一索引真的在拦人。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/session_repo.rs src-tauri/src/storage/mod.rs
git commit -m "feat(m01,m02): Session/Interval 仓储含不变量自检"
```

---

### Task 10: 跨层集成测试——一次完整的业务写

**Files:**
- Create: `src-tauri/tests/foundation.rs`

**Interfaces:**
- Consumes: 前九个任务的全部公开 API
- Produces: 无新 API；把「迁移 → epoch 守卫 → 领域跃迁 → 仓储写入 → revision 前进 → 失败零残留」串成一条可回归的链

- [ ] **Step 1: 写测试**

创建 `src-tauri/tests/foundation.rs`：

```rust
//! 跨层集成：一个真实文件库上跑完整链路。
//!
//! 用临时文件而非内存库，顺带覆盖"打开既有库不重建 schema"这条路径。

use worktrace_lib::commands::envelope::{guard_epoch, AppError};
use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, read_meta};
use worktrace_lib::storage::migrations::{migrate, current_version, SCHEMA_VERSION};
use worktrace_lib::storage::session_repo::{create_session, interval_facts, pause_session};
use worktrace_lib::storage::task_repo::{create_task, get_task, transition_task};

fn open_fresh(path: &std::path::Path) -> Db {
    let db = Db::open(path).unwrap();
    migrate(db.conn()).unwrap();
    init_meta(db.conn(), "epoch-a").unwrap();
    db
}

#[test]
fn full_v01_write_path_is_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worktrace.db");
    let clock = FakeClock::new(1_700_000_000_000, 0);

    let db = open_fresh(&path);
    let epoch = read_meta(db.conn()).unwrap().data_epoch.clone();
    assert_eq!(epoch, "epoch-a");
    assert_eq!(read_meta(db.conn()).unwrap().revision, 0);

    // 计时与业务写共用同一个假时钟，逐段推进
    clock.advance_monotonic_ms(1_000);
    let t = create_task(db.conn(), "写周报", None, clock.wall_ms()).unwrap();
    assert_eq!(read_meta(db.conn()).unwrap().revision, 1);

    let t = transition_task(db.conn(), &t.id, TaskStatus::Ready, TransitionCause::Normal, clock.wall_ms()).unwrap();
    assert_eq!(t.row_version, 1);

    db.conn()
        .execute(
            "INSERT INTO application_run(id, started_at) VALUES ('run-1', ?1)",
            [clock.wall_ms()],
        )
        .unwrap();

    let sid = create_session(db.conn(), &t.id, "run-1", SessionMode::Foreground, TimerKind::Stopwatch, clock.wall_ms()).unwrap();
    let rev_before_failures = read_meta(db.conn()).unwrap().revision;

    // 一系列必须整体失败的调用：revision 一个字都不能动
    let failures: Vec<AppError> = vec![
        guard_epoch(db.conn(), "epoch-stale").unwrap_err(),
        create_task(db.conn(), "   ", None, clock.wall_ms()).unwrap_err(),
        transition_task(db.conn(), &t.id, TaskStatus::Clarifying, TransitionCause::Normal, clock.wall_ms()).unwrap_err(),
        pause_session(db.conn(), "no-such-session", clock.wall_ms()).unwrap_err(),
    ];
    for e in &failures {
        assert!(!e.message().is_empty());
    }
    assert_eq!(read_meta(db.conn()).unwrap().revision, rev_before_failures, "失败链不得改变 revision");

    // 正确暂停后，工时事实与不变量都对得上
    clock.advance_monotonic_ms(60_000);
    pause_session(db.conn(), &sid, clock.wall_ms()).unwrap();
    let f = interval_facts(db.conn(), &sid).unwrap();
    assert_eq!((f.open_count, f.pending_count, f.closed_trusted_count), (0, 0, 1));

    // 关掉再打开：不重建 schema，数据与 revision 都在
    let rev = read_meta(db.conn()).unwrap().revision;
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(current_version(db.conn()).unwrap(), SCHEMA_VERSION);
    migrate(db.conn()).unwrap();          // 幂等
    assert_eq!(read_meta(db.conn()).unwrap().revision, rev);
    assert_eq!(get_task(db.conn(), &t.id).unwrap().unwrap().status, TaskStatus::Ready);
}

#[test]
fn a_new_database_gets_its_own_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_fresh(&dir.path().join("a.db"));
    let b = open_fresh(&dir.path().join("b.db"));
    assert_ne!(
        read_meta(a.conn()).unwrap().data_epoch,
        read_meta(b.conn()).unwrap().data_epoch,
        "每个新库必须有独立身份"
    );
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test --test foundation`
Expected: `test result: ok. 2 passed`。

若 `full_v01_write_path_is_consistent` 在"失败链不得改变 revision"那条断言上红，说明某个本该整体失败的调用在中途提交了——回去查对应的仓储函数是不是漏了 `unchecked_transaction`，或者把 `?` 写在了 `tx.commit()` 之后。

- [ ] **Step 3: 跑一遍全量并确认没有回归**

Run: `cargo test`
Expected: 全部通过；`--lib` 与两个集成测试（`schema`、`foundation`）都绿。

- [ ] **Step 4: 跑分层自查**

Run:

```bash
cd src-tauri \
  && (grep -rn "rusqlite\|std::fs\|std::time" src/domain/ && echo "违反分层：domain 不得有 IO" && exit 1 || echo "domain 无 IO ✓") \
  && (grep -rn "platform::" src/storage/ && echo "违反分层：storage 不得调用 platform" && exit 1 || echo "storage 未越层 ✓")
```

Expected: 两行 ✓。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/tests/foundation.rs
git commit -m "test(m01,m02): 跨层集成覆盖完整 V0.1 写入链路"
```

---

## 验收（跑完本计划后必须成立）

1. `cargo test` 全绿，且 `cargo test --lib` 与 `cargo test --test schema`、`cargo test --test foundation` 分别可独立运行。
2. 迁移后的库恰有 V0.1 的 12 张表，**没有** `goal`/`milestone`，`project` 无 `goal_id`、`task` 无 `milestone_id`。
3. 三条库级约束真在拦人：前台唯一 running、每 session 唯一开放区间、可信闭合区间的 `duration_ms = ended_at - started_at`。
4. `data_epoch` 是新库独有身份；`DATA_EPOCH_MISMATCH` 与 `VERSION_CONFLICT` 两个错误码可被断言，且错误文本不含 epoch 字面量。
5. 一次业务写让 revision **恰好 +1**；任何被拒的调用（非法跃迁、空白标题、陈旧版本、未知会话）**revision 不变、审计不写、无半条记录**。
6. `domain/` 不含任何 `rusqlite` / `std::fs` / `std::time` 引用；`storage/` 不引用 `platform::`。

第 6 条用一条命令自查：

```bash
cd src-tauri && grep -rn "rusqlite\|std::fs\|std::time" src/domain/ && echo "违反分层：domain 不得有 IO" && exit 1 || echo "domain 无 IO ✓"
grep -rn "platform::" src/storage/ && echo "违反分层：storage 不得调用 platform" && exit 1 || echo "storage 未越层 ✓"
```

## 不在本计划范围内（后续计划的接口）

本计划**建表但不写行为**的部分，明确列在这里，避免执行者以为漏了：

- **`time_edit` 无写入函数**：只有 `correct`（M05，仅 finished）与 `reconcile`（M05，仅 recovering）会写它。本计划只保证表与外键在。
- **`interval_checkpoint` 无写入函数**：写入者是 M04 的计时协调器（08 §1：约每 30 秒同事务写 `interval_id/run_id/wall_at/attribution_at/elapsed_ms`）。
- **`application_run` 只有测试里的裸 INSERT**：真实的"新 run + 崩溃扫描"属 M00/M05 启动流程（02 §4）。
- **`daily_plan`、`tag`、`task_tag` 无仓储**：F-004/F-005 的项目与标签命令是后续计划。
- **无 IPC 命令**：`commands/` 本计划只有错误信封，没有 `#[tauri::command]`。计时与任务的命令入口在 M04/M05 计划里接。

后续计划的接口：

- **M04/M05 计时协调器**：`start/pause/resume/finish` 的 IPC 命令、归属基线 `A(M)`、心跳与 `interval_checkpoint` 写入、崩溃恢复扫描与 `reconcile`、`discard_session`/`backfill`。本计划已把表、不变量与 `pause_session`/`resume_session` 原语备好。
- **M03 事件广播**与 revision 通知协议（00 §5 的信封、去重与握手规则）。
- **M00 单实例**、锁屏/休眠事件、托盘与窗口。
- **M12 前端**：状态镜像、Today 页、命令 DTO 生成。
- **M06/M07** 统计与导出。
- V0.2 起的 `goal`/`milestone`/`time_block`/`task_knowledge`/`phase_checkpoint`/`pomodoro_cycle` 表，以及 `task.milestone_id`、`project.goal_id`、`work_interval.cycle_index` 列。
