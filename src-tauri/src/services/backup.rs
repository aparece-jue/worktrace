//! 备份原语（P6 Task 1 落成、Task 4a 从 `services/bootstrap.rs` 整体搬到这里）。
//!
//! **为什么单独一个模块**：备份不是"启动"的一部分——恢复/替换库（Task 4b）也要用同一套
//! 命名、保留策略与失败口径。Task 1 当时只被允许改启动编排，原语先落在 `bootstrap.rs`；
//! 现在整体搬过来，**不留第二套拷贝**。
//!
//! ## 三件写死的事（原样继承，别"顺手改"）
//!
//! 1. **同一连接**。`Db::open` 之后没有关连接的时机（第②步一结束就要读 `app_meta` 取
//!    `data_epoch`），所以原语必须是「库已经打开时可用」的那种；而磁盘库是 WAL
//!    （`storage/db.rs` 打开时强制校验），**只拷主库文件必然撕裂**。`VACUUM INTO`
//!    产出的是一份事务一致的单文件快照（WAL 里尚未 checkpoint 的内容也在内），
//!    且**零新增依赖**——不开 `rusqlite` 的 `"backup"` feature（那要动
//!    `Cargo.toml`/`Cargo.lock`，本机是离线环境）。
//! 2. **只在需要迁移时调用**（调用点是 `bootstrap::startup` 的第②步，判据是
//!    `user_version < SCHEMA_VERSION`）。首启（库文件不存在）与「版本相等」两条路径
//!    **不产生备份产物**，也**不碰**应用数据目录——目录解析放在本模块，但只在被调用时发生。
//! 3. **失败即拒绝迁移**：返回 `AppError::Storage`，`detail` 带 `pre-migration backup:`
//!    阶段标记。调用方据此让本次启动失败——不降级、不跳过、不先迁移后补。
//!
//! ## 命名与保留策略
//!
//! 文件名 `worktrace-f<格式版号>-s<数据库版号>-v<应用版本>-<Unix 毫秒>.db`，
//! **三个版号分别记录**（02 §9）：
//!
//! - `f` = [`BACKUP_FORMAT_VERSION`]：这份产物长什么样（形状变了才 +1）；
//! - `s` = **被备份库的 `PRAGMA user_version`（迁移前）**，不是本次构建的 `SCHEMA_VERSION`
//!   ——文件名描述的是"这份产物是什么"，而恢复前要判的正是它；
//! - `v` = 应用版本；毫秒段是保留策略的排序键。
//!
//! 保留最近 [`BACKUP_KEEP`] 份，**但永不删掉刚写出的那一份**——纯按文件名挂钟毫秒排序时，
//! 系统时间被回拨且已有 ≥5 份会让"本次产物"成为名义上最老的那份，现象是"启动成功、
//! 日志说已备份、产物却没了"。清理失败只记诊断：它发生在备份成功之后，不能把一次成功
//! 启动变成失败。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::AppError;
use crate::platform::clock::Clock;
use crate::platform::paths;
use crate::storage::db::map_sqlite;

/// 备份产物的**格式版号**：与库 schema 版号、应用版本并列的第三个版号。
///
/// 它描述「这份备份文件长什么样」——今天是「一个 `VACUUM INTO` 出来的 SQLite 单文件
/// 快照」。形状变了才 +1（02 §9 要求三个版号分别记录，落地就是文件名）。
pub const BACKUP_FORMAT_VERSION: u32 = 1;

/// 备份目录名（生产缺省：`app_data_dir()/backups`）。与库、锁同在一个应用数据目录下。
const BACKUP_DIR_NAME: &str = "backups";

/// 保留的备份份数。按需执行下它只在**真迁移**时触发，所以这 5 份是「跨版本升级」的
/// 历史，不是「最近 5 次启动」。
const BACKUP_KEEP: usize = 5;

/// 备份文件名的固定前后缀。保留策略只认自己写出来的名字。
const BACKUP_PREFIX: &str = "worktrace-f";
const BACKUP_SUFFIX: &str = ".db";

/// 备份阶段的失败：`detail` 统一带阶段标记，便于把「备份失败」与「迁移失败」分开；
/// 用户文案仍走 `AppError::message()`（`Storage` 的 detail 不进用户可见文案）。
fn backup_stage(e: AppError) -> AppError {
    AppError::Storage {
        detail: format!("pre-migration backup: {}", e.detail().unwrap_or(e.code())),
    }
}

/// 备份目录相关的 IO 失败：`detail` 与阶段标记同前缀，便于读日志时一眼归位。
fn dir_error(e: std::io::Error) -> AppError {
    AppError::Storage {
        detail: format!("pre-migration backup dir: {e}"),
    }
}

/// 备份文件名：`worktrace-f<格式版号>-s<数据库版号>-v<应用版本>-<Unix 毫秒>.db`。
///
/// `db_version` 记的是**被备份的那份库**的 `PRAGMA user_version`（迁移前），不是本次
/// 构建的 `SCHEMA_VERSION`：文件名描述的是「这份产物是什么」。写构建版本只会与
/// 应用版本重复，写被备份的版本才有信息量（恢复前要判的正是它）。
fn backup_file_name(db_version: i64, at_ms: i64) -> String {
    format!(
        "{BACKUP_PREFIX}{BACKUP_FORMAT_VERSION}-s{db_version}-v{}-{at_ms}{BACKUP_SUFFIX}",
        env!("CARGO_PKG_VERSION")
    )
}

/// 从文件名里取出 Unix 毫秒（保留策略据此从旧到新删）。
///
/// 名字不符合这个形状（或毫秒段不是数字）⇒ `None`：认不出的文件**不碰**，
/// 删别人的东西不是保留策略的事。
fn backup_timestamp(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_prefix(BACKUP_PREFIX)?
        .strip_suffix(BACKUP_SUFFIX)?;
    let (_, at_ms) = stem.rsplit_once('-')?;
    at_ms.parse::<i64>().ok()
}

/// 只保留最新的 [`BACKUP_KEEP`] 份备份，但**永不删掉 `just_written`**。
///
/// 为什么要有第二个参数（Task 1 评审留下的那条）：排序键是**文件名里的挂钟毫秒**，
/// 而挂钟可以被回拨（对时、跨时区、用户改表）。5 份既有产物都比本次更晚时，本次产物
/// 排在最前、会被当成"最老"删掉——**刚备份完就把它删了**，日志却写着"已备份"。
/// 时间戳排序仍然是保留策略的口径（不改成 mtime：那要读文件系统时间，而"哪份更新"
/// 在回拨下同样不可信），只是把本次产物排除在候选之外，多删一份次老的。
///
/// **不返回错误**：清理发生在备份成功之后，失败只记诊断，不能把一次成功启动变成失败。
fn prune_old_backups(dir: &Path, just_written: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<(i64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            backup_timestamp(&path).map(|at_ms| (at_ms, path))
        })
        .collect();
    if backups.len() <= BACKUP_KEEP {
        return;
    }

    backups.sort_by_key(|(at_ms, _)| *at_ms);
    let excess = backups.len() - BACKUP_KEEP;
    let removable = backups
        .into_iter()
        .filter(|(_, path)| path.as_path() != just_written);
    for (_, path) in removable.take(excess) {
        if let Err(error) = std::fs::remove_file(&path) {
            // 只记诊断（正式诊断落点是 `platform::diagnostics`，见 Task 2a）。
            eprintln!(
                "[worktrace] backup prune: cannot remove {}: {error} (ignored)",
                path.display()
            );
        }
    }
}

/// 迁移前的按需一致备份：**在同一个已打开的连接上** `VACUUM INTO`，返回本次产物路径。
///
/// `dir_override` = 调用方注入的备份目录（测试与将来的恢复流程用）；`None` ⇒
/// `app_data_dir()/backups`。**目录只在被调用时解析**，所以"无需迁移"的启动不碰数据目录。
///
/// 顺序写死：解析目录 → `create_dir_all` → 取一次时钟样本给产物命名 → 同名即拒绝
/// （**不覆盖**：这比让 `VACUUM INTO` 自己撞出来可诊断得多）→ `VACUUM INTO` →
/// 保留策略（**永不删本次产物**）。
pub fn backup_before_migration(
    dir_override: Option<&Path>,
    conn: &Connection,
    from_version: i64,
    clock: &(dyn Clock + Send),
) -> Result<PathBuf, AppError> {
    let dir = match dir_override {
        Some(dir) => dir.to_path_buf(),
        None => paths::app_data_dir()
            .map_err(dir_error)?
            .join(BACKUP_DIR_NAME),
    };
    std::fs::create_dir_all(&dir).map_err(dir_error)?;

    // 时间戳走注入的时钟：`services` 不得自取系统时间（分层门禁那条规则的用意是
    // 「服务层的时间必须来自 `Clock`」）。时钟取不到就没法给产物命名 ⇒ 同样拒绝迁移。
    let sample = clock.sample().map_err(|_| AppError::Storage {
        detail: "pre-migration backup: clock sample unavailable for the artifact name".to_string(),
    })?;
    let target = dir.join(backup_file_name(from_version, sample.wall_ms));

    // 同名产物（同一毫秒）在这里就拒绝：**不覆盖**是刻意的。
    if target.exists() {
        return Err(AppError::Storage {
            detail: format!(
                "pre-migration backup: artifact already exists: {}",
                target.display()
            ),
        });
    }

    let target_sql = target.to_str().ok_or_else(|| AppError::Storage {
        detail: "pre-migration backup: artifact path is not valid UTF-8".to_string(),
    })?;
    conn.execute("VACUUM INTO ?1", [target_sql])
        .map_err(map_sqlite)
        .map_err(backup_stage)?;

    // 备份已经落地，之后才是保留策略：它失败只记诊断，且**永不删刚写出的这一份**。
    prune_old_backups(&dir, &target);
    Ok(target)
}
