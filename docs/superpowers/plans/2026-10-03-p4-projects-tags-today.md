# P4 · 项目、标签与今日计划 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 V0.1 的三组"辅助实体"做成可用仓储：项目（建/重命名/归档 + 归档后从新建任务的选择列表消失）、四类标签与任务打标、以及按用户时区记录的今日选择列表。

**Architecture:** 只碰 `domain/` 与 `storage/` 两层，不引入 `services/`，也不加 IPC。三个仓储各自独立，共用 P1 建好的 `bump_revision` 与 `AppError` 信封；每个业务写都在一个事务里完成"改实体 + 加版本 + 加 revision"，被拒的调用不留半条记录——沿用 P1 立下的断言风格。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· 无新依赖

**Spec:**
- `docs/superpowers/specs/2026-10-02-worktrace-architecture/02-data-model.zh.md` §2（`project`/`tag`/`task_tag`/`daily_plan` 表与索引）、§9（明细历史与今日计划）
- `.../04-functional-spec.zh.md` F-004（项目管理）、F-005（基础标签）、F-010（Today 的今日选择部分）
- `.../05-roadmap.zh.md` §1（V0.1 范围）

**依赖的前置计划：** P1（`2026-10-03-worktrace-v01-foundation.md`）。直接引用 P1 的
`storage::db::{Db, StorageError}`、`storage::meta::{read_meta, bump_revision}`、
`commands::envelope::{AppError, guard_row_version}`、`storage::task_repo::{TaskRow, get_task}`、
`domain::error::DomainError`、`domain::task::TaskStatus`。**签名不得改动**；需要改时先更新计划总纲。

## Global Constraints

继承计划总纲 §5 的 7 条，另加本计划特有的：

- **V0.1 的四类标签是 Domain / Activity / Context / Report**（F-005 原文）。`Knowledge` 属 **V0.2**（F-107），本计划的 `TagKind` **枚举里不得出现它**。
- **本计划不实现标签权重规则**。F-108（非空权重 0..1、同 kind 总和 >1 拒绝、<1 显示未分配、不自动归一化）是 **V0.2**。`task_tag.weight` 列在库里存在，但本计划写入时**一律留 NULL**，也不提供设置接口。
- **本计划不暴露标签层级**。`tag.parent_id` 列与 `uq_tag_root`/`uq_tag_child` 两条部分唯一索引存在（P1 已建），但 V0.1 创建的标签 `parent_id` 恒为 NULL——此时 `uq_tag_root` 恰好等价于"同 kind 内标签不得重名"。层级界面属 V0.2 的 F-107。
- **归档是状态，不是删除**（F-004）。`project.status` 取 `active`/`archived`；归档项目仍可被既有任务引用，只是不出现在**新建**任务的选择列表里。
- **今日计划按用户时区存储**（02 §9 原话：「`daily_plan` 存今日选择日期及时区，**不用任务 `updated_at` 推断今天安排**」）。本计划只负责按 `(task_id, local_date, timezone)` 存取与校验，不做跨时区换算。
- **每次业务写让 `revision` 恰好 +1**；被拒的调用不改 revision、不写审计、不留半条记录。

---

## 文件结构

```
src-tauri/src/
  domain/
    tag.rs                    建：TagKind 四类枚举（不含 Knowledge）
    localdate.rs              建：LocalDate 校验（YYYY-MM-DD 且真实日历日）
    mod.rs                    改：声明新模块
  storage/
    project_repo.rs           建：项目的建/取/重命名/归档/可选列表
    tag_repo.rs               建：标签的建/取/列举 + 任务打标
    daily_plan_repo.rs        建：今日选择的增删查
    mod.rs                    改：声明新模块
  tests/
    auxiliary_entities.rs     建：跨仓储集成（P4 的 tests/ 目录）
```

三个仓储互不依赖，`tag_repo` 引用 `task_repo` 只为校验任务存在，`daily_plan_repo` 同理。

---

### Task 1: LocalDate 校验

**Files:**
- Create: `src-tauri/src/domain/localdate.rs`
- Modify: `src-tauri/src/domain/mod.rs`

**Interfaces:**
- Consumes: `domain::error::DomainError`（P1）
- Produces:
  - `domain::localdate::LocalDate(String)`，方法 `parse(&str) -> Result<LocalDate, DomainError>`、`as_str(&self) -> &str`
  - `domain::error::DomainError::InvalidLocalDate { value: String }`（本任务新增）

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/domain/localdate.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_calendar_dates() {
        for s in ["2026-10-03", "2024-02-29", "2000-01-01", "1999-12-31"] {
            assert_eq!(LocalDate::parse(s).unwrap().as_str(), s, "{s} 应被接受");
        }
    }

    #[test]
    fn rejects_impossible_calendar_dates() {
        // 2026 不是闰年
        assert!(LocalDate::parse("2026-02-29").is_err());
        assert!(LocalDate::parse("2026-04-31").is_err());
        assert!(LocalDate::parse("2026-13-01").is_err());
        assert!(LocalDate::parse("2026-00-10").is_err());
        assert!(LocalDate::parse("2026-01-00").is_err());
    }

    #[test]
    fn rejects_anything_that_is_not_the_exact_shape() {
        for s in ["2026-1-3", "26-01-03", "2026/01/03", "2026-01-03T00:00", "", " 2026-01-03", "2026-01-03 "] {
            assert!(LocalDate::parse(s).is_err(), "{s:?} 应被拒绝");
        }
    }

    #[test]
    fn the_error_names_the_offending_value() {
        let e = LocalDate::parse("2026-02-30").unwrap_err();
        assert!(format!("{e}").contains("2026-02-30"), "错误应指出是哪个值：{e}");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib localdate`
Expected: 编译失败，`cannot find type LocalDate`。

- [ ] **Step 3: 实现 LocalDate**

在 `src-tauri/src/domain/error.rs` 加一个变体：

```rust
    #[error("not a valid local date (expected YYYY-MM-DD): {value}")]
    InvalidLocalDate { value: String },
```

`src-tauri/src/domain/localdate.rs`：

```rust
//! 用户本地日期（02 §9）。
//!
//! 今日计划按 `(task_id, local_date, timezone)` 存储，其中 `local_date` 是
//! **用户所在时区的日历日**，不是 UTC 日期、也不是从任务 `updated_at` 推出来的。
//! 本类型只保证形态与日历合法性；跨时区换算不在这里做。

use crate::domain::error::DomainError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalDate(String);

impl LocalDate {
    pub fn parse(s: &str) -> Result<Self, DomainError> {
        let invalid = || DomainError::InvalidLocalDate { value: s.to_string() };

        let bytes = s.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(invalid());
        }
        if !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
        {
            return Err(invalid());
        }

        let year: i32 = s[0..4].parse().map_err(|_| invalid())?;
        let month: u32 = s[5..7].parse().map_err(|_| invalid())?;
        let day: u32 = s[8..10].parse().map_err(|_| invalid())?;

        if !(1..=12).contains(&month) {
            return Err(invalid());
        }
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let max_day = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => unreachable!(),
        };
        if !(1..=max_day).contains(&day) {
            return Err(invalid());
        }

        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
```

`domain/mod.rs` 加 `pub mod localdate;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib localdate`
Expected: `test result: ok. 4 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/domain/localdate.rs src-tauri/src/domain/error.rs src-tauri/src/domain/mod.rs
git commit -m "feat(m02): 用户本地日期校验"
```

---

### Task 2: Project 仓储——建、重命名、归档

**Files:**
- Create: `src-tauri/src/storage/project_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`

**Interfaces:**
- Consumes: P1 的 `Db`/`AppError`/`bump_revision`/`guard_row_version`
- Produces:
  - `storage::project_repo::ProjectStatus`（`Active`/`Archived`，含 `as_str`/`from_str`）
  - `storage::project_repo::Project { id: String, name: String, description: Option<String>, status: ProjectStatus, created_at: i64, updated_at: i64 }`
  - `create_project(conn, name: &str, description: Option<&str>, now_ms) -> Result<Project, AppError>`
  - `get_project(conn, id: &str) -> Result<Option<Project>, AppError>`
  - `rename_project(conn, id: &str, new_name: &str, now_ms) -> Result<Project, AppError>`
  - `archive_project(conn, id: &str, now_ms) -> Result<Project, AppError>`
  - `list_selectable_projects(conn) -> Result<Vec<Project>, AppError>`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/storage/project_repo.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
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
    fn create_starts_active_and_bumps_revision() {
        let db = db();
        let p = create_project(db.conn(), "  桥接项目  ", Some("说明"), 1_000).unwrap();
        assert_eq!(p.name, "桥接项目", "名称应被 trim");
        assert_eq!(p.status, ProjectStatus::Active);
        assert_eq!(p.created_at, 1_000);
        assert_eq!(read_meta(db.conn()).unwrap().revision, 1);
    }

    #[test]
    fn blank_name_is_rejected_without_a_trace() {
        let db = db();
        assert!(create_project(db.conn(), "   ", None, 1_000).is_err());
        let n: i64 = db.conn().query_row("SELECT count(*) FROM project", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert_eq!(read_meta(db.conn()).unwrap().revision, 0);
    }

    #[test]
    fn rename_updates_timestamp_and_revision() {
        let db = db();
        let p = create_project(db.conn(), "旧名", None, 1_000).unwrap();
        let p2 = rename_project(db.conn(), &p.id, "新名", 2_000).unwrap();
        assert_eq!(p2.name, "新名");
        assert_eq!(p2.updated_at, 2_000);
        assert_eq!(p2.created_at, 1_000, "created_at 不得被改写");
        assert_eq!(read_meta(db.conn()).unwrap().revision, 2);
    }

    #[test]
    fn archiving_is_a_status_not_a_delete() {
        // F-004：归档是状态；既有任务仍可引用它
        let db = db();
        let p = create_project(db.conn(), "P", None, 1_000).unwrap();
        let archived = archive_project(db.conn(), &p.id, 2_000).unwrap();
        assert_eq!(archived.status, ProjectStatus::Archived);

        let still_there = get_project(db.conn(), &p.id).unwrap().expect("归档不等于删除");
        assert_eq!(still_there.status, ProjectStatus::Archived);
    }

    #[test]
    fn archiving_twice_is_idempotent_but_still_bumps_once_each_time() {
        let db = db();
        let p = create_project(db.conn(), "P", None, 1_000).unwrap();
        archive_project(db.conn(), &p.id, 2_000).unwrap();
        archive_project(db.conn(), &p.id, 3_000).unwrap();
        // 每次都是显式业务写，revision 各加一次
        assert_eq!(read_meta(db.conn()).unwrap().revision, 3);
        assert_eq!(get_project(db.conn(), &p.id).unwrap().unwrap().updated_at, 3_000);
    }

    #[test]
    fn unknown_project_is_rejected() {
        let db = db();
        assert!(rename_project(db.conn(), "nope", "x", 1_000).is_err());
        assert!(archive_project(db.conn(), "nope", 1_000).is_err());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib project_repo`
Expected: 编译失败，`cannot find function create_project`。

- [ ] **Step 3: 实现 project_repo**

先在 `src-tauri/src/domain/error.rs` 加一个变体——本任务与后续任务都要用它表示"找不到实体"：

```rust
    #[error("no such entity: {kind} {id}")]
    NotFound { kind: &'static str, id: String },
```

然后写 `src-tauri/src/storage/project_repo.rs`：

```rust
//! Project 仓储（F-004）。
//!
//! 归档是**状态**不是删除：`status` 取 active/archived，归档后仍可被既有任务
//! 引用，只是从"新建任务的选择列表"里消失（见 `list_selectable_projects`）。

use crate::commands::envelope::AppError;
use crate::domain::error::DomainError;
use crate::storage::meta::bump_revision;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatus {
    Active,
    Archived,
}

impl ProjectStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Active => "active",
            ProjectStatus::Archived => "archived",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, DomainError> {
        match s {
            "active" => Ok(ProjectStatus::Active),
            "archived" => Ok(ProjectStatus::Archived),
            other => Err(DomainError::UnknownTaskStatus(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: ProjectStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

const SELECT: &str =
    "SELECT id, name, description, status, created_at, updated_at FROM project";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let status: String = r.get("status")?;
    Ok(Project {
        id: r.get("id")?,
        name: r.get("name")?,
        description: r.get("description")?,
        status: ProjectStatus::from_str(&status).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, status.clone())),
            )
        })?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

pub fn get_project(conn: &Connection, id: &str) -> Result<Option<Project>, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    Ok(conn.query_row(&sql, [id], read_row).optional()?)
}

pub fn create_project(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
    now_ms: i64,
) -> Result<Project, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DomainError::EmptyTitle.into());
    }
    let id = Uuid::new_v4().to_string();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO project(id, name, description, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'active', ?4, ?4)",
        rusqlite::params![id, name, description, now_ms],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(Project {
        id,
        name: name.to_string(),
        description: description.map(str::to_string),
        status: ProjectStatus::Active,
        created_at: now_ms,
        updated_at: now_ms,
    })
}

pub fn rename_project(
    conn: &Connection,
    id: &str,
    new_name: &str,
    now_ms: i64,
) -> Result<Project, AppError> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err(DomainError::EmptyTitle.into());
    }
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE project SET name = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![new_name, now_ms, id],
    )?;
    if n == 0 {
        return Err(DomainError::NotFound { kind: "project", id: id.to_string() }.into());
    }
    bump_revision(&tx)?;
    tx.commit()?;

    fetch(conn, id)
}

pub fn archive_project(conn: &Connection, id: &str, now_ms: i64) -> Result<Project, AppError> {
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE project SET status = 'archived', updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now_ms, id],
    )?;
    if n == 0 {
        return Err(DomainError::NotFound { kind: "project", id: id.to_string() }.into());
    }
    bump_revision(&tx)?;
    tx.commit()?;

    fetch(conn, id)
}

/// F-004：归档项目不出现在**新建任务**的选择列表里。
/// 既有任务仍可引用归档项目，所以这里只是列表查询，不是唯一入口。
pub fn list_selectable_projects(conn: &Connection) -> Result<Vec<Project>, AppError> {
    let sql = format!("{SELECT} WHERE status = 'active' ORDER BY name, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], read_row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn fetch(conn: &Connection, id: &str) -> Result<Project, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], read_row)
        .optional()?
        .ok_or_else(|| DomainError::NotFound { kind: "project", id: id.to_string() }.into())
}
```

`storage/mod.rs` 加 `pub mod project_repo;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib project_repo`
Expected: `test result: ok. 6 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/project_repo.rs src-tauri/src/storage/mod.rs src-tauri/src/domain/error.rs
git commit -m "feat(m01): Project 仓储含归档状态"
```

---

### Task 3: 项目选择列表与任务归属

**Files:**
- Modify: `src-tauri/src/storage/task_repo.rs`
- Create: `src-tauri/tests/project_selection.rs`

**Interfaces:**
- Consumes: Task 2 的 `project_repo`
- Produces:
  - `storage::task_repo::assign_project(conn, task_id: &str, project_id: Option<&str>, expected_row_version: i64, now_ms: i64) -> Result<TaskRow, AppError>`
  - `storage::task_repo::list_tasks_in_project(conn, project_id: &str) -> Result<Vec<TaskRow>, AppError>`

- [ ] **Step 1: 写测试**

创建 `src-tauri/tests/project_selection.rs`：

```rust
//! F-004：归档项目从"新建任务的选择列表"消失，但既有任务不受影响。

use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::project_repo::{
    archive_project, create_project, list_selectable_projects,
};
use worktrace_lib::storage::task_repo::{assign_project, create_task, list_tasks_in_project};

fn seeded() -> Db {
    let db = Db::open_in_memory().unwrap();
    migrate(db.conn()).unwrap();
    init_meta(db.conn(), "epoch-a").unwrap();
    db
}

#[test]
fn archived_projects_leave_the_new_task_picker_but_keep_their_tasks() {
    let db = seeded();
    let active = create_project(db.conn(), "在做的", None, 1_000).unwrap();
    let done = create_project(db.conn(), "收尾的", None, 1_000).unwrap();

    let t = create_task(db.conn(), "任务 A", Some(&done.id), 2_000).unwrap();
    assert_eq!(t.project_id.as_deref(), Some(done.id.as_str()));

    assert_eq!(list_selectable_projects(db.conn()).unwrap().len(), 2);
    archive_project(db.conn(), &done.id, 3_000).unwrap();

    // 选择列表只剩在做的那个
    let picker = list_selectable_projects(db.conn()).unwrap();
    assert_eq!(picker.len(), 1);
    assert_eq!(picker[0].id, active.id);

    // 但既有任务仍在归档项目下，且能被查出来
    let tasks = list_tasks_in_project(db.conn(), &done.id).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, t.id);
}

#[test]
fn assigning_a_project_bumps_task_version_and_revision() {
    let db = seeded();
    let p = create_project(db.conn(), "P", None, 1_000).unwrap();
    let t = create_task(db.conn(), "T", None, 2_000).unwrap();

    let updated = assign_project(db.conn(), &t.id, Some(&p.id), 0, 3_000).unwrap();
    assert_eq!(updated.project_id.as_deref(), Some(p.id.as_str()));
    assert_eq!(updated.row_version, 1);
    assert_eq!(updated.updated_at, 3_000);
}

#[test]
fn assigning_with_a_stale_version_conflicts() {
    let db = seeded();
    let p = create_project(db.conn(), "P", None, 1_000).unwrap();
    let t = create_task(db.conn(), "T", None, 2_000).unwrap();
    assign_project(db.conn(), &t.id, Some(&p.id), 0, 3_000).unwrap();

    let err = assign_project(db.conn(), &t.id, None, 0, 4_000).unwrap_err();
    assert_eq!(err.code(), "VERSION_CONFLICT");
}

#[test]
fn clearing_the_project_is_allowed() {
    let db = seeded();
    let p = create_project(db.conn(), "P", None, 1_000).unwrap();
    let t = create_task(db.conn(), "T", Some(&p.id), 2_000).unwrap();
    let cleared = assign_project(db.conn(), &t.id, None, t.row_version, 3_000).unwrap();
    assert_eq!(cleared.project_id, None);
}

#[test]
fn assigning_to_a_missing_project_is_rejected() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 2_000).unwrap();
    let err = assign_project(db.conn(), &t.id, Some("nope"), 0, 3_000).unwrap_err();
    assert_eq!(err.code(), "STORAGE_ERROR", "外键必须拦住不存在的项目");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --test project_selection`
Expected: 编译失败，`cannot find function assign_project`。

- [ ] **Step 3: 实现 assign_project 与 list_tasks_in_project**

在 `src-tauri/src/storage/task_repo.rs` 追加：

```rust
/// 把任务归到某个项目（或清空归属）。F-004 要求任务可归属项目。
pub fn assign_project(
    conn: &Connection,
    task_id: &str,
    project_id: Option<&str>,
    expected_row_version: i64,
    now_ms: i64,
) -> Result<TaskRow, AppError> {
    let tx = conn.unchecked_transaction()?;
    let current = {
        let sql = format!("{SELECT} WHERE id = ?1");
        tx.query_row(&sql, [task_id], read_row)
            .optional()?
            .ok_or_else(|| AppError::Domain(DomainError::NotFound {
                kind: "task",
                id: task_id.to_string(),
            }))?
    };
    crate::commands::envelope::guard_row_version(current.row_version, expected_row_version)?;

    tx.execute(
        "UPDATE task SET project_id = ?1, row_version = row_version + 1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![project_id, now_ms, task_id],
    )?;
    crate::storage::meta::bump_revision(&tx)?;
    tx.commit()?;

    Ok(TaskRow {
        project_id: project_id.map(str::to_string),
        row_version: current.row_version + 1,
        updated_at: now_ms,
        ..current
    })
}

/// 某项目下的全部任务。**不过滤归档**——归档项目的既有任务仍要能看到（F-004）。
pub fn list_tasks_in_project(conn: &Connection, project_id: &str) -> Result<Vec<TaskRow>, AppError> {
    let sql = format!("{SELECT} WHERE project_id = ?1 ORDER BY created_at, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([project_id], read_row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --test project_selection`
Expected: `test result: ok. 5 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/task_repo.rs src-tauri/tests/project_selection.rs
git commit -m "feat(m01,m02): 任务归属项目与项目选择列表"
```

---

### Task 4: Tag 的四类创建与列举

**Files:**
- Create: `src-tauri/src/domain/tag.rs`
- Create: `src-tauri/src/storage/tag_repo.rs`
- Modify: `src-tauri/src/domain/mod.rs`, `src-tauri/src/storage/mod.rs`

**Interfaces:**
- Consumes: P1 的 `AppError`/`bump_revision`/`DomainError`
- Produces:
  - `domain::tag::TagKind`（`Domain`/`Activity`/`Context`/`Report`），`as_str`/`from_str`/`ALL`
  - `storage::tag_repo::Tag { id: String, kind: TagKind, name: String, parent_id: Option<String>, created_at: i64 }`
  - `create_tag(conn, kind: TagKind, name: &str, now_ms) -> Result<Tag, AppError>`
  - `get_tag(conn, id: &str) -> Result<Option<Tag>, AppError>`
  - `list_tags(conn, kind: TagKind) -> Result<Vec<Tag>, AppError>`

- [ ] **Step 1: 写失败的测试**

在 `src-tauri/src/storage/tag_repo.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
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
    fn v01_has_exactly_four_kinds_and_no_knowledge() {
        // F-005 原文是 Domain / Activity / Context / Report；
        // Knowledge 属 V0.2 的 F-107，枚举里不得出现。
        assert_eq!(TagKind::ALL.len(), 4);
        for k in TagKind::ALL {
            assert_ne!(k.as_str(), "Knowledge");
            assert_eq!(TagKind::from_str(k.as_str()).unwrap(), *k);
        }
        assert!(TagKind::from_str("Knowledge").is_err(), "V0.1 不接受 Knowledge");
    }

    #[test]
    fn create_round_trips_and_bumps_revision() {
        let db = db();
        let t = create_tag(db.conn(), TagKind::Domain, "  桥接  ", 1_000).unwrap();
        assert_eq!(t.name, "桥接", "名称应被 trim");
        assert_eq!(t.kind, TagKind::Domain);
        assert_eq!(t.parent_id, None, "V0.1 的标签一律是根标签");
        assert_eq!(get_tag(db.conn(), &t.id).unwrap(), Some(t));
        assert_eq!(read_meta(db.conn()).unwrap().revision, 1);
    }

    #[test]
    fn same_name_is_rejected_within_a_kind_but_allowed_across_kinds() {
        // uq_tag_root ON tag(kind,name) WHERE parent_id IS NULL
        let db = db();
        create_tag(db.conn(), TagKind::Domain, "桥接", 1_000).unwrap();
        assert!(
            create_tag(db.conn(), TagKind::Domain, "桥接", 2_000).is_err(),
            "同 kind 内不得重名（uq_tag_root）"
        );
        create_tag(db.conn(), TagKind::Activity, "桥接", 2_000).unwrap();
    }

    #[test]
    fn list_returns_only_the_requested_kind_sorted_by_name() {
        let db = db();
        create_tag(db.conn(), TagKind::Context, "实验室", 1_000).unwrap();
        create_tag(db.conn(), TagKind::Context, "会议室", 1_000).unwrap();
        create_tag(db.conn(), TagKind::Report, "周报", 1_000).unwrap();

        let ctx = list_tags(db.conn(), TagKind::Context).unwrap();
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx[0].name, "会议室");
        assert_eq!(ctx[1].name, "实验室");
        assert_eq!(list_tags(db.conn(), TagKind::Report).unwrap().len(), 1);
        assert_eq!(list_tags(db.conn(), TagKind::Activity).unwrap().len(), 0);
    }

    #[test]
    fn blank_name_is_rejected_without_a_trace() {
        let db = db();
        assert!(create_tag(db.conn(), TagKind::Domain, "  ", 1_000).is_err());
        let n: i64 = db.conn().query_row("SELECT count(*) FROM tag", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert_eq!(read_meta(db.conn()).unwrap().revision, 0);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib tag_repo`
Expected: 编译失败，`cannot find type TagKind`。

- [ ] **Step 3: 实现 TagKind 与 tag_repo**

`src-tauri/src/domain/tag.rs`：

```rust
//! 标签类别（F-005）。
//!
//! V0.1 只有四类。**`Knowledge` 不在其中**——它是 V0.2 的 F-107，
//! 连同标签层级一起。枚举里出现 Knowledge 就是范围越界。

use crate::domain::error::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    Domain,
    Activity,
    Context,
    Report,
}

impl TagKind {
    pub const ALL: &'static [TagKind] = &[
        TagKind::Domain,
        TagKind::Activity,
        TagKind::Context,
        TagKind::Report,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            TagKind::Domain => "Domain",
            TagKind::Activity => "Activity",
            TagKind::Context => "Context",
            TagKind::Report => "Report",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, DomainError> {
        Self::ALL
            .iter()
            .copied()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| DomainError::UnknownTagKind(s.to_string()))
    }
}
```

在 `domain/error.rs` 加：

```rust
    #[error("unknown tag kind: {0}")]
    UnknownTagKind(String),
```

`src-tauri/src/storage/tag_repo.rs`：

```rust
//! Tag 仓储（F-005）。
//!
//! V0.1 只建**根标签**（`parent_id` 恒为 NULL），此时 `uq_tag_root`
//! 等价于"同 kind 内标签不得重名"。层级与 Knowledge 属 V0.2。
//! 本计划**不写 `task_tag.weight`**（权重规则是 V0.2 的 F-108）。

use crate::commands::envelope::AppError;
use crate::domain::error::DomainError;
use crate::domain::tag::TagKind;
use crate::storage::meta::bump_revision;
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: String,
    pub kind: TagKind,
    pub name: String,
    pub parent_id: Option<String>,
    pub created_at: i64,
}

const SELECT: &str = "SELECT id, kind, name, parent_id, created_at FROM tag";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Tag> {
    let kind: String = r.get("kind")?;
    Ok(Tag {
        id: r.get("id")?,
        kind: TagKind::from_str(&kind).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, kind.clone())),
            )
        })?,
        name: r.get("name")?,
        parent_id: r.get("parent_id")?,
        created_at: r.get("created_at")?,
    })
}

pub fn get_tag(conn: &Connection, id: &str) -> Result<Option<Tag>, AppError> {
    let sql = format!("{SELECT} WHERE id = ?1");
    Ok(conn.query_row(&sql, [id], read_row).optional()?)
}

pub fn create_tag(
    conn: &Connection,
    kind: TagKind,
    name: &str,
    now_ms: i64,
) -> Result<Tag, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DomainError::EmptyTitle.into());
    }
    let id = Uuid::new_v4().to_string();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO tag(id, kind, name, parent_id, created_at) VALUES (?1, ?2, ?3, NULL, ?4)",
        rusqlite::params![id, kind.as_str(), name, now_ms],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(Tag { id, kind, name: name.to_string(), parent_id: None, created_at: now_ms })
}

pub fn list_tags(conn: &Connection, kind: TagKind) -> Result<Vec<Tag>, AppError> {
    let sql = format!("{SELECT} WHERE kind = ?1 ORDER BY name, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([kind.as_str()], read_row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
```

两个 `mod.rs` 各加一行声明。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib tag_repo`
Expected: `test result: ok. 5 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/domain/tag.rs src-tauri/src/domain/mod.rs src-tauri/src/domain/error.rs \
        src-tauri/src/storage/tag_repo.rs src-tauri/src/storage/mod.rs
git commit -m "feat(m02,m01): 四类标签的创建与列举"
```

---

### Task 5: 打标、去标与任务的标签查询

**Files:**
- Modify: `src-tauri/src/storage/tag_repo.rs`
- Create: `src-tauri/tests/tagging.rs`

**Interfaces:**
- Consumes: Task 4 的 `tag_repo`、P1 的 `task_repo`
- Produces:
  - `tag_task(conn, task_id: &str, tag_id: &str) -> Result<(), AppError>` —— 幂等
  - `untag_task(conn, task_id: &str, tag_id: &str) -> Result<bool, AppError>` —— 返回是否真的删掉了
  - `tags_of_task(conn, task_id: &str) -> Result<Vec<Tag>, AppError>`

- [ ] **Step 1: 写测试**

创建 `src-tauri/tests/tagging.rs`：

```rust
//! F-005：同一任务可有多个标签。

use worktrace_lib::domain::tag::TagKind;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, read_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::tag_repo::{create_tag, tag_task, tags_of_task, untag_task};
use worktrace_lib::storage::task_repo::create_task;

fn seeded() -> Db {
    let db = Db::open_in_memory().unwrap();
    migrate(db.conn()).unwrap();
    init_meta(db.conn(), "epoch-a").unwrap();
    db
}

#[test]
fn a_task_can_carry_tags_from_several_kinds() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let d = create_tag(db.conn(), TagKind::Domain, "桥接", 1_000).unwrap();
    let a = create_tag(db.conn(), TagKind::Activity, "调试", 1_000).unwrap();
    let c = create_tag(db.conn(), TagKind::Context, "实验室", 1_000).unwrap();

    for tag in [&d, &a, &c] {
        tag_task(db.conn(), &t.id, &tag.id).unwrap();
    }

    let tags = tags_of_task(db.conn(), &t.id).unwrap();
    assert_eq!(tags.len(), 3, "同一任务可有多个标签");
    let names: Vec<&str> = tags.iter().map(|x| x.name.as_str()).collect();
    assert!(names.contains(&"桥接") && names.contains(&"调试") && names.contains(&"实验室"));
}

#[test]
fn tagging_twice_is_idempotent_and_does_not_bump_revision_twice() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let tag = create_tag(db.conn(), TagKind::Domain, "桥接", 1_000).unwrap();
    tag_task(db.conn(), &t.id, &tag.id).unwrap();
    let rev = read_meta(db.conn()).unwrap().revision;

    tag_task(db.conn(), &t.id, &tag.id).unwrap();
    assert_eq!(tags_of_task(db.conn(), &t.id).unwrap().len(), 1);
    assert_eq!(read_meta(db.conn()).unwrap().revision, rev, "重复打标不算业务变化");
}

#[test]
fn untagging_reports_whether_anything_was_removed() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let tag = create_tag(db.conn(), TagKind::Domain, "桥接", 1_000).unwrap();
    tag_task(db.conn(), &t.id, &tag.id).unwrap();

    assert!(untag_task(db.conn(), &t.id, &tag.id).unwrap());
    assert!(tags_of_task(db.conn(), &t.id).unwrap().is_empty());
    assert!(!untag_task(db.conn(), &t.id, &tag.id).unwrap(), "第二次应报告没删到");
}

#[test]
fn tagging_an_unknown_task_or_tag_is_rejected() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let tag = create_tag(db.conn(), TagKind::Domain, "桥接", 1_000).unwrap();

    assert_eq!(tag_task(db.conn(), "nope", &tag.id).unwrap_err().code(), "STORAGE_ERROR");
    assert_eq!(tag_task(db.conn(), &t.id, "nope").unwrap_err().code(), "STORAGE_ERROR");
}

#[test]
fn the_weight_column_stays_null_in_v01() {
    // F-108（权重 0..1、总和 ≤1、不自动归一化）是 V0.2。V0.1 只建关联。
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let tag = create_tag(db.conn(), TagKind::Domain, "桥接", 1_000).unwrap();
    tag_task(db.conn(), &t.id, &tag.id).unwrap();

    let w: Option<f64> = db
        .conn()
        .query_row("SELECT weight FROM task_tag", [], |r| r.get(0))
        .unwrap();
    assert_eq!(w, None, "V0.1 不得写入权重");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --test tagging`
Expected: 编译失败，`cannot find function tag_task`。

- [ ] **Step 3: 实现打标与去标**

追加到 `tag_repo.rs`：

```rust
/// 给任务打标。**幂等**：已经打过就不算业务变化，不增加 revision。
pub fn tag_task(conn: &Connection, task_id: &str, tag_id: &str) -> Result<(), AppError> {
    let tx = conn.unchecked_transaction()?;
    let existed: bool = tx
        .query_row(
            "SELECT 1 FROM task_tag WHERE task_id = ?1 AND tag_id = ?2",
            rusqlite::params![task_id, tag_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if existed {
        return Ok(());
    }
    // weight 刻意留 NULL：权重规则是 V0.2 的 F-108
    tx.execute(
        "INSERT INTO task_tag(task_id, tag_id, weight) VALUES (?1, ?2, NULL)",
        rusqlite::params![task_id, tag_id],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(())
}

/// 去标。返回**是否真的删掉了一行**——调用方据此决定要不要提示。
pub fn untag_task(conn: &Connection, task_id: &str, tag_id: &str) -> Result<bool, AppError> {
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "DELETE FROM task_tag WHERE task_id = ?1 AND tag_id = ?2",
        rusqlite::params![task_id, tag_id],
    )?;
    if n == 0 {
        return Ok(false);
    }
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(true)
}

/// 某任务身上的全部标签，按 kind 再按名称排序。
pub fn tags_of_task(conn: &Connection, task_id: &str) -> Result<Vec<Tag>, AppError> {
    let sql = "SELECT t.id, t.kind, t.name, t.parent_id, t.created_at
                 FROM tag t JOIN task_tag tt ON tt.tag_id = t.id
                WHERE tt.task_id = ?1
                ORDER BY t.kind, t.name, t.id";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([task_id], read_row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --test tagging`
Expected: `test result: ok. 5 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/tag_repo.rs src-tauri/tests/tagging.rs
git commit -m "feat(m01,m02): 任务打标与去标（权重留 NULL）"
```

---

### Task 6: 今日计划（按用户时区）

**Files:**
- Create: `src-tauri/src/storage/daily_plan_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Create: `src-tauri/tests/daily_plan.rs`

**Interfaces:**
- Consumes: Task 1 的 `LocalDate`、P1 的 `task_repo`
- Produces:
  - `add_to_plan(conn, task_id: &str, date: &LocalDate, timezone: &str, now_ms: i64) -> Result<(), AppError>` —— 幂等
  - `remove_from_plan(conn, task_id: &str, date: &LocalDate, timezone: &str) -> Result<bool, AppError>`
  - `plan_for(conn, date: &LocalDate, timezone: &str) -> Result<Vec<String>, AppError>` —— 返回 task_id 列表
  - `storage::daily_plan_repo::normalize_timezone(tz: &str) -> Result<String, AppError>`

- [ ] **Step 1: 写测试**

创建 `src-tauri/tests/daily_plan.rs`：

```rust
//! 02 §9：今日计划存"日期 + 时区"，不用任务 updated_at 推断今天安排。

use worktrace_lib::domain::localdate::LocalDate;
use worktrace_lib::storage::daily_plan_repo::{
    add_to_plan, normalize_timezone, plan_for, remove_from_plan,
};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, read_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::task_repo::create_task;

fn seeded() -> Db {
    let db = Db::open_in_memory().unwrap();
    migrate(db.conn()).unwrap();
    init_meta(db.conn(), "epoch-a").unwrap();
    db
}

#[test]
fn a_task_is_in_the_plan_only_for_its_own_date_and_zone() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let d3 = LocalDate::parse("2026-10-03").unwrap();
    let d4 = LocalDate::parse("2026-10-04").unwrap();

    add_to_plan(db.conn(), &t.id, &d3, "Asia/Shanghai", 1_000).unwrap();

    assert_eq!(plan_for(db.conn(), &d3, "Asia/Shanghai").unwrap(), vec![t.id.clone()]);
    assert!(plan_for(db.conn(), &d4, "Asia/Shanghai").unwrap().is_empty(), "换一天就不在计划里");
    assert!(
        plan_for(db.conn(), &d3, "America/New_York").unwrap().is_empty(),
        "同一日历日在另一个时区是另一行（PK 含 timezone）"
    );
}

#[test]
fn adding_twice_is_idempotent() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let d = LocalDate::parse("2026-10-03").unwrap();
    add_to_plan(db.conn(), &t.id, &d, "Asia/Shanghai", 1_000).unwrap();
    let rev = read_meta(db.conn()).unwrap().revision;

    add_to_plan(db.conn(), &t.id, &d, "Asia/Shanghai", 2_000).unwrap();
    assert_eq!(plan_for(db.conn(), &d, "Asia/Shanghai").unwrap().len(), 1);
    assert_eq!(read_meta(db.conn()).unwrap().revision, rev, "重复加入不算业务变化");
}

#[test]
fn removal_reports_whether_a_row_went_away() {
    let db = seeded();
    let t = create_task(db.conn(), "T", None, 1_000).unwrap();
    let d = LocalDate::parse("2026-10-03").unwrap();
    add_to_plan(db.conn(), &t.id, &d, "Asia/Shanghai", 1_000).unwrap();

    assert!(remove_from_plan(db.conn(), &t.id, &d, "Asia/Shanghai").unwrap());
    assert!(!remove_from_plan(db.conn(), &t.id, &d, "Asia/Shanghai").unwrap());
}

#[test]
fn the_plan_order_is_stable_for_the_same_input() {
    // 今日选择的展示顺序必须稳定，否则前端每次刷新都跳动
    let db = seeded();
    let a = create_task(db.conn(), "A", None, 1_000).unwrap();
    let b = create_task(db.conn(), "B", None, 2_000).unwrap();
    let c = create_task(db.conn(), "C", None, 3_000).unwrap();
    let d = LocalDate::parse("2026-10-03").unwrap();

    for t in [&c, &a, &b] {
        add_to_plan(db.conn(), &t.id, &d, "Asia/Shanghai", 1_000).unwrap();
    }
    let first = plan_for(db.conn(), &d, "Asia/Shanghai").unwrap();
    let second = plan_for(db.conn(), &d, "Asia/Shanghai").unwrap();
    assert_eq!(first, second);
    assert_eq!(first, vec![a.id.clone(), b.id.clone(), c.id.clone()], "按任务创建顺序");
}

#[test]
fn timezone_is_required_and_normalised() {
    assert_eq!(normalize_timezone(" Asia/Shanghai ").unwrap(), "Asia/Shanghai");
    assert_eq!(normalize_timezone("UTC").unwrap(), "UTC");
    assert!(normalize_timezone("   ").is_err(), "空时区必须被拒");
    assert!(normalize_timezone("Asia Shanghai").is_err(), "时区名不得含空格");
}

#[test]
fn adding_an_unknown_task_is_rejected() {
    let db = seeded();
    let d = LocalDate::parse("2026-10-03").unwrap();
    assert_eq!(
        add_to_plan(db.conn(), "nope", &d, "Asia/Shanghai", 1_000).unwrap_err().code(),
        "STORAGE_ERROR"
    );
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --test daily_plan`
Expected: 编译失败，`cannot find function add_to_plan`。

- [ ] **Step 3: 实现 daily_plan_repo**

先在 `src-tauri/src/domain/error.rs` 加：

```rust
    #[error("invalid timezone: {value}")]
    InvalidTimezone { value: String },
```

然后写 `src-tauri/src/storage/daily_plan_repo.rs`：

```rust
//! 今日计划（02 §9、F-010 的"今日选择列表"部分）。
//!
//! 存的是 `(task_id, local_date, timezone)` 三元组，**不是**从任务
//! `updated_at` 推断出来的"今天"。日期由调用方按用户时区给出并经
//! `LocalDate` 校验；本层不做时区换算。
//!
//! 本计划只负责列表本身。跨日裁剪与统计口径属 P5。

use crate::commands::envelope::AppError;
use crate::domain::error::DomainError;
use crate::domain::localdate::LocalDate;
use crate::storage::meta::bump_revision;
use rusqlite::{Connection, OptionalExtension};

/// 时区名归一化：去首尾空白，非空且不含内部空白。
///
/// V0.1 不引入时区数据库——IANA 名的合法性由上层（P7 的界面）保证，
/// 这里只挡住显然不合法的输入。
pub fn normalize_timezone(tz: &str) -> Result<String, AppError> {
    let t = tz.trim();
    if t.is_empty() || t.chars().any(char::is_whitespace) {
        return Err(DomainError::InvalidTimezone { value: tz.to_string() }.into());
    }
    Ok(t.to_string())
}

/// 把任务加入某日计划。**幂等**：已在计划里就不算业务变化。
pub fn add_to_plan(
    conn: &Connection,
    task_id: &str,
    date: &LocalDate,
    timezone: &str,
    _now_ms: i64,
) -> Result<(), AppError> {
    let tz = normalize_timezone(timezone)?;
    let tx = conn.unchecked_transaction()?;
    let existed: bool = tx
        .query_row(
            "SELECT 1 FROM daily_plan WHERE task_id = ?1 AND local_date = ?2 AND timezone = ?3",
            rusqlite::params![task_id, date.as_str(), tz],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if existed {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO daily_plan(task_id, local_date, timezone) VALUES (?1, ?2, ?3)",
        rusqlite::params![task_id, date.as_str(), tz],
    )?;
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(())
}

/// 从某日计划移除。返回是否真的删掉了一行。
pub fn remove_from_plan(
    conn: &Connection,
    task_id: &str,
    date: &LocalDate,
    timezone: &str,
) -> Result<bool, AppError> {
    let tz = normalize_timezone(timezone)?;
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "DELETE FROM daily_plan WHERE task_id = ?1 AND local_date = ?2 AND timezone = ?3",
        rusqlite::params![task_id, date.as_str(), tz],
    )?;
    if n == 0 {
        return Ok(false);
    }
    bump_revision(&tx)?;
    tx.commit()?;
    Ok(true)
}

/// 某日某时区的计划，按任务创建顺序稳定返回。
pub fn plan_for(
    conn: &Connection,
    date: &LocalDate,
    timezone: &str,
) -> Result<Vec<String>, AppError> {
    let tz = normalize_timezone(timezone)?;
    let mut stmt = conn.prepare(
        "SELECT d.task_id
           FROM daily_plan d JOIN task t ON t.id = d.task_id
          WHERE d.local_date = ?1 AND d.timezone = ?2
          ORDER BY t.created_at, t.id",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![date.as_str(), tz], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}
```

`storage/mod.rs` 加 `pub mod daily_plan_repo;`。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --test daily_plan`
Expected: `test result: ok. 6 passed`。

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/storage/daily_plan_repo.rs src-tauri/src/storage/mod.rs src-tauri/tests/daily_plan.rs
git commit -m "feat(m01,m02): 今日计划按日期与时区存取"
```

---

## 验收（跑完本计划后必须成立）

1. `cargo test` 全绿，P1 与 P2 的测试无回归。
2. **`TagKind` 恰好四类且不含 Knowledge**：`v01_has_exactly_four_kinds_and_no_knowledge` 通过。
3. **`task_tag.weight` 在 V0.1 恒为 NULL**：`the_weight_column_stays_null_in_v01` 通过。这条把 F-108 挡在 V0.2 之外。
4. **归档项目从选择列表消失、但既有任务仍可查**：`archived_projects_leave_the_new_task_picker_but_keep_their_tasks` 通过（F-004）。
5. **同 kind 内标签不得重名、跨 kind 可同名**：`same_name_is_rejected_within_a_kind_but_allowed_across_kinds` 通过（`uq_tag_root`）。
6. **今日计划按 `(task, date, timezone)` 三元组隔离**：`a_task_is_in_the_plan_only_for_its_own_date_and_zone` 通过。
7. **幂等写不重复加 revision**：打标与加入计划各有一条对应断言。
8. **分层自查**（沿用 P1 的两条）：

```bash
cd src-tauri \
  && (grep -rn "rusqlite\|std::fs\|std::time" src/domain/ && echo "违反：domain 不得有 IO" && exit 1 || echo "domain 无 IO ✓") \
  && (grep -rn "platform::" src/storage/ && echo "违反：storage 不得调用 platform" && exit 1 || echo "storage 未越层 ✓")
```

## 不在本计划范围内

- **标签权重规则**（F-108，V0.2）：非空权重 0..1、同 kind 总和 >1 拒绝、<1 显示未分配、不自动归一化。列已建，规则不落地。
- **Knowledge 标签与标签层级**（F-107，V0.2）：`parent_id` 列与两条部分唯一索引已建，本计划不暴露父子选择。
- **标签加权统计**（F-109，V0.2）：P5 只做**无权重**的关联口径。
- **`time_block` 排期**（F-105，V0.2）与 Goal/Milestone（V0.2）。
- **IPC 命令、前端选择控件、时区换算**：分别属 P7 与 P5。
