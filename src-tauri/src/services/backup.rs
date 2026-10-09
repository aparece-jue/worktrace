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
//! 日志说已备份、产物却没了"。恢复流程那条"旧版本先备份再迁移"的分支还多护一份：
//! **用户正在恢复的那份产物**（它就在同一个目录里，A1）。清理失败只记诊断：它发生在
//! 备份成功之后，不能把一次成功启动变成失败；落点是**注入的诊断出口**（[`Diagnostics`]），
//! 不是控制台——release 的 Windows 子系统没有控制台（P6 终审 M-3）。
//!
//! ## 恢复的三段流程（P6 Task 4b；顺序写死，别按别处的措辞猜）
//!
//! 02 §9 的顺序——「停计时、暂停写入、**关闭连接** → 在临时路径验证待恢复库完整性/
//! 外键/schema（未来版本拒绝，旧版本先备份再迁移）→ **同目录可回滚切换** → 重开校验；
//! 失败还原原路径并重新打开原库」——在代码里就是三段，边界与锁的关系是**硬约束**：
//!
//! | 段 | 持锁 | 做什么 |
//! | --- | --- | --- |
//! | ① [`begin_restore`] | **锁内、短** | `begin_maintenance(Restore)` → `take_runtime()` → 立即放开锁 |
//! | ② [`prepare_and_swap`] | **不持锁** | 旧 `Db` 随 `Runtime` drop（**这时才关连接**）→ 候选库拷到同目录暂存 → 完整性/外键/版本验证（旧版本先备份再迁移）→ 同目录改名切换（原库留成回滚副本） |
//! | ③ [`commit_restore`] / [`abort_restore`] | **锁内** | `Db::open` → 同一事务 `start_run` + `rotate_epoch`（**只在提交路径**）→ `scan_at_startup` 归一 → `scan_recovery` 门禁 → 新协调器 + 锚点 → `install_runtime` → `end_maintenance` → 广播 |
//!
//! 为什么 ① 与 ③ 必须短、② 必须不持锁：维护态的意义就是"长活不占锁"——换库与验证
//! 期间别的调用取到锁之后**立刻**拿到 [`AppError::DataRestoreInProgress`]，而不是挂住。
//!
//! **唯一入口是 [`restore_from_backup`]**：它在**同一次后台阻塞调用**里连续调上面那几个
//! 服务原语，**不重新进 `commands::run_command`**（维护态里没有任何 IPC 命令名在白名单上；
//! 见 `services::bootstrap` 模块头与 `commands::run_command` 的注释）。P8 接线时也只在
//! 一个 `#[tauri::command]` 里调它——**不要**把三段拆到三个命令里去。
//!
//! **两条路径都不得复用维护前的协调器**（"维护窗口不计入工时"的第二层补偿）：提交路径
//! 装的是新库上的新协调器；回滚路径重开原库、建**新 run**、再建**新协调器**
//! （旧 `Instant` 基线随旧协调器一起作废）。维护前那条 `running` 会话在新 run 里是
//! **外来事实**，由 P3 的扫描归一 + 门禁挡住新计时，必须走 F-015 的用户确认才成事实。
//!
//! **`Scheduler` 全程不 stop、不重启**（`stop` 不可逆）：[`Runtime`] 里**没有**调度器，
//! 它在 `RunningApp` 上活着，闭包每拍从 `AppState` 读**当前**运行态 ⇒ 换库后自然对新
//! 运行态工作。反过来，若把调度器塞进运行态，①③ 两段里任何一次丢弃都会在持锁状态下
//! `join` 一个正在等这把锁的线程（G11 的自死锁）。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::AppError;
use crate::platform::clock::{Clock, ClockSample};
use crate::platform::diagnostics::Diagnostics;
use crate::platform::paths;
use crate::services::bootstrap::{
    holds_app_lock, lock_app, scan_recovery, MaintenancePhase, RecoveryScan, Runtime, SharedApp,
};
use crate::services::events::{Broadcaster, EventEnvelope};
use crate::services::timer::coordinator::Coordinator;
use crate::storage::db::{map_sqlite, Db};
use crate::storage::meta;
use crate::storage::migrations::{current_version, migrate, SCHEMA_VERSION};
use crate::storage::run_repo;

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

/// 迁移前备份的阶段标记：`detail` 里用它把「备份失败」与「迁移失败」分开。
const PRE_MIGRATION_STAGE: &str = "pre-migration backup";

/// 恢复候选库（旧版本 schema）迁移前那次备份的阶段标记。
const PRE_RESTORE_STAGE: &str = "pre-restore backup";

/// 保留策略清理失败的**正式诊断记录名**（P6 终审 M-3）。
///
/// 为什么要有它：清理失败原先只打 `eprintln!`，而 release 的 Windows 子系统没有控制台
/// ⇒「备份目录里的旧产物删不掉」在真机上永久不可见（磁盘被慢慢占满，日志里一条线索都没有）。
/// 清理失败**不影响这次备份的成功**，所以它只能走诊断：`event=backup.prune_failed`。
const BACKUP_PRUNE_FAILED: &str = "backup.prune_failed";

/// 恢复完成的**正式诊断记录名**（提交与回滚各写一条，含库身份、原身份与回滚副本路径）。
///
/// 与维护态/故障态跃迁写的是同一个落点（`AppState.diagnostics` ⇒ `StartupConfig.diagnostic_log`）：
/// release 的 Windows 子系统没有控制台，这条记录是"那天到底恢没恢复、副本在哪"的唯一线索。
const RESTORE_DIAGNOSTIC: &str = "restore.finished";

/// 恢复**失败**的正式诊断记录名（P6 终审 I-2）：两条失败臂各写一条。
///
/// 为什么必须有：原先只有成功路径写 `restore.finished` ⇒「恢复失败**且回滚也失败**」
/// （原库没重建回来、进程永久停在维护态、托盘拒绝退出）在日志里与「正在恢复」不可区分。
/// 记录带 `stage=`（① 还是 ② ③ 段失败）、`rolled_back=`（原库有没有重建回来）与
/// `rollback=`（回滚副本路径）。失败时**也**会把原因交回调用方，这一条只是落盘。
const RESTORE_FAILED: &str = "restore.failed";

/// 备份阶段失败的统一形状：`detail` 带阶段标记，便于把「备份失败」与后续动作分开；
/// 用户文案仍走 `AppError::message()`（`Storage` 的 detail 不进用户可见文案）。
fn stage_error(stage: &str, e: AppError) -> AppError {
    AppError::Storage {
        detail: format!("{stage}: {}", e.detail().unwrap_or(e.code())),
    }
}

/// 备份目录相关的 IO 失败：`detail` 与阶段标记同前缀，便于读日志时一眼归位。
fn dir_error(stage: &str, e: std::io::Error) -> AppError {
    AppError::Storage {
        detail: format!("{stage} dir: {e}"),
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

/// 参与"不许删"比较的**规范路径**（A1 复审）。
///
/// `protected` 是调用方给的拼写，目录项是文件系统给的拼写：大小写、`.`/`..` 段、
/// 软链、Windows 的 `\\?\` 前缀都可能不同（P8 的文件选择器给什么拼写不由这里决定）。
/// 逐字节比字符串会把**同一份文件**判成两份 ⇒ 用户选中那份又变成可删候选。能
/// `canonicalize` 就用它（两侧此刻都存在），失败（刚被删、权限不足）就退回原路径——
/// 退回只会让比较更严格，不会多删。
fn keep_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 只保留最新的 [`BACKUP_KEEP`] 份备份，但**永不删掉 `protected` 里的任何一份**。
///
/// 为什么要有第二个参数（Task 1 评审留下的那条）：排序键是**文件名里的挂钟毫秒**，
/// 而挂钟可以被回拨（对时、跨时区、用户改表）。5 份既有产物都比本次更晚时，本次产物
/// 排在最前、会被当成"最老"删掉——**刚备份完就把它删了**，日志却写着"已备份"。
/// 时间戳排序仍然是保留策略的口径（不改成 mtime：那要读文件系统时间，而"哪份更新"
/// 在回拨下同样不可信），只是把本次产物排除在候选之外，多删一份次老的。
///
/// `protected` 有两类（A1）：① 本次刚写出的产物（上面那条）；② 恢复流程里
/// **用户正在恢复的那份产物**——"旧版本先备份再迁移"那条分支复用的是用户自己的备份
/// 目录，于是这次备份会顺带触发保留策略，把用户刚选中的那份删掉（staged 拷贝在先，
/// 恢复本身不受影响，但用户回头就找不到自己选的文件了）。所以调用方把被恢复的路径
/// 一并传进来，不让保留策略碰它。
///
/// 比较走 [`keep_key`]（规范路径）：**同一份文件的不同拼写**也算同一份
/// （`tests/backup_restore.rs::a_differently_spelled_pick_is_still_protected`）。
///
/// **不返回错误**：清理发生在备份成功之后，失败只记诊断，不能把一次成功启动变成失败。
///
/// `diagnostics` 是**注入的**落点（P6 终审 M-3）：这里原先是 `eprintln!`，而 release 的
/// Windows 子系统没有控制台 ⇒ 保留策略清理失败在真机上永久不可见。落点关闭时
/// （[`Diagnostics::disabled`]）什么都不写，这也是测试夹具的缺省。
fn prune_old_backups(dir: &Path, protected: &[&Path], diagnostics: &Diagnostics) {
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

    let keep: Vec<PathBuf> = protected.iter().map(|path| keep_key(path)).collect();
    backups.sort_by_key(|(at_ms, _)| *at_ms);
    let excess = backups.len() - BACKUP_KEEP;
    let removable = backups
        .into_iter()
        .filter(|(_, path)| !keep.contains(&keep_key(path)));
    for (_, path) in removable.take(excess) {
        if let Err(error) = std::fs::remove_file(&path) {
            // 只记诊断（正式诊断落点是 `platform::diagnostics`，见 Task 2a）：清理发生在
            // 备份**成功之后**，删不掉一份旧产物不该让这次备份（乃至这次启动）变成失败。
            diagnostics.record(
                BACKUP_PRUNE_FAILED,
                &format!("file={} error={error}", path.display()),
            );
        }
    }
}

/// 一致备份的**通用原语**：在**同一个已打开的连接**上 `VACUUM INTO` 一份快照，返回产物路径。
///
/// `dir_override` = 调用方注入的备份目录（测试与恢复流程用）；`None` ⇒
/// `app_data_dir()/backups`。**目录只在被调用时解析**，所以"无需迁移"的启动不碰数据目录。
///
/// `stage` 是失败 `detail` 的阶段标记：同一个动作在两个调用点含义不同
/// （迁移前 vs 恢复候选库迁移前），日志里必须分得开。
///
/// 顺序写死：解析目录 → `create_dir_all` → 取一次时钟样本给产物命名 → 同名即拒绝
/// （**不覆盖**：这比让 `VACUUM INTO` 自己撞出来可诊断得多）→ `VACUUM INTO` →
/// 保留策略（**永不删本次产物**，清理失败只记 `diagnostics` 里那一条）。
pub fn backup_consistent(
    dir_override: Option<&Path>,
    conn: &Connection,
    db_version: i64,
    clock: &(dyn Clock + Send),
    stage: &str,
    diagnostics: &Diagnostics,
) -> Result<PathBuf, AppError> {
    backup_consistent_keeping(
        dir_override,
        conn,
        db_version,
        clock,
        stage,
        diagnostics,
        None,
    )
}

/// [`backup_consistent`] 的实体，外加一位"保留策略**不许删**的路径"（A1）。
///
/// 为什么不让 `backup_consistent` 自己多收一位：它是公开原语，启动路径、恢复路径与
/// `tests/` 的夹具都按现有的六个参数调用（改公开签名会牵动每一条调用点），而
/// **只有恢复流程**需要"多留一份"——那条分支复用的是用户的备份目录，用户选中的产物
/// 就在同一个目录里。多出来的这一位因此只留在内层。
fn backup_consistent_keeping(
    dir_override: Option<&Path>,
    conn: &Connection,
    db_version: i64,
    clock: &(dyn Clock + Send),
    stage: &str,
    diagnostics: &Diagnostics,
    also_keep: Option<&Path>,
) -> Result<PathBuf, AppError> {
    let dir = match dir_override {
        Some(dir) => dir.to_path_buf(),
        None => paths::app_data_dir()
            .map_err(|e| dir_error(stage, e))?
            .join(BACKUP_DIR_NAME),
    };
    std::fs::create_dir_all(&dir).map_err(|e| dir_error(stage, e))?;

    // 时间戳走注入的时钟：`services` 不得自取系统时间（分层门禁那条规则的用意是
    // 「服务层的时间必须来自 `Clock`」）。时钟取不到就没法给产物命名 ⇒ 同样拒绝。
    let sample = clock.sample().map_err(|_| AppError::Storage {
        detail: format!("{stage}: clock sample unavailable for the artifact name"),
    })?;
    let target = dir.join(backup_file_name(db_version, sample.wall_ms));

    // 同名产物（同一毫秒）在这里就拒绝：**不覆盖**是刻意的。
    if target.exists() {
        return Err(AppError::Storage {
            detail: format!("{stage}: artifact already exists: {}", target.display()),
        });
    }

    let target_sql = target.to_str().ok_or_else(|| AppError::Storage {
        detail: format!("{stage}: artifact path is not valid UTF-8"),
    })?;
    conn.execute("VACUUM INTO ?1", [target_sql])
        .map_err(map_sqlite)
        .map_err(|e| stage_error(stage, e))?;

    // 备份已经落地，之后才是保留策略：它失败只记诊断，且**永不删刚写出的这一份**
    // （恢复分支还要再护住"用户正在恢复的那一份"）。
    let mut protected = vec![target.as_path()];
    if let Some(also_keep) = also_keep {
        protected.push(also_keep);
    }
    prune_old_backups(&dir, &protected, diagnostics);
    Ok(target)
}

/// 迁移前的按需一致备份：**在同一个已打开的连接上** `VACUUM INTO`，返回本次产物路径。
///
/// 判据（只在 `user_version < SCHEMA_VERSION` 时执行）、命名与保留策略见模块头；
/// 具体动作是 [`backup_consistent`]，本函数只把阶段标记钉死成「迁移前」。
pub fn backup_before_migration(
    dir_override: Option<&Path>,
    conn: &Connection,
    from_version: i64,
    clock: &(dyn Clock + Send),
    diagnostics: &Diagnostics,
) -> Result<PathBuf, AppError> {
    backup_consistent(
        dir_override,
        conn,
        from_version,
        clock,
        PRE_MIGRATION_STAGE,
        diagnostics,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 恢复：三段流程（P6 Task 4b）
// ─────────────────────────────────────────────────────────────────────────────

/// 恢复流程的**时钟来源**：每调用一次交出一个新的钟。
///
/// 为什么是"来源"而不是一个 `Box<dyn Clock + Send>`：③ 的提交与回滚**互斥、但都要建新
/// 协调器**，而 `Coordinator::new` 会拿走那只钟的所有权——提交失败之后回滚还得再要一只。
/// 生产侧闭包捕获**同一个 [`crate::platform::clock::SystemClock`] 并 `clone()` 它：
/// `SystemClock` 的克隆共享 `origin`，所以"时钟必须同源"仍然成立（各建一个新的
/// `SystemClock` 会得到两个原点，OS 边界样本会被 `system_pause` 全部拒绝——R-02 静默落空）。
pub type ClockSource = Box<dyn Fn() -> Box<dyn Clock + Send> + Send>;

/// 候选库在**同目录**里的暂存名后缀（验证在它上面做，切换就是它的改名）。
///
/// 为什么放在**同一个目录**而不是系统临时目录：最后一步必须是**同卷改名**才不会退化成
/// "先删后拷"（那正是"不能覆盖仍打开的 WAL 数据库"之外最危险的中间态）。计划原文的
/// "临时路径验证 + 同目录可回滚切换"落到代码里就是这两句。
const STAGED_SUFFIX: &str = ".restore-staged";

/// 原库在切换期间的回滚副本后缀（**同目录**，切换失败或 ③ 失败时改回来）。
const ROLLBACK_SUFFIX: &str = ".restore-rollback";

/// 一次恢复的结果（提交与回滚共用同一个形状——`committed` 是唯一的判据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// `true` = 换库成功（新库、**新 `data_epoch`**）；`false` = 回滚（**原库、原 epoch**）。
    pub committed: bool,
    /// 恢复之后库里的 `data_epoch`（提交路径 = 全新值；回滚路径 = 原值）。
    pub data_epoch: String,
    /// 恢复之后**当前 run** 的 id（两条路径都是**新的**）。
    pub run_id: String,
    /// 恢复之后的重扫门禁快照（`requires_recovery()` 就是"要不要先确认历史"）。
    pub recovery: RecoveryScan,
    /// 候选库 schema 偏旧时、迁移前那份一致备份的产物路径（`None` = 没走到那条分支）。
    pub migration_backup: Option<PathBuf>,
    /// **恢复之前那个世界**的完整副本（`<db>.restore-rollback`）；`None` = 没有副本留下。
    ///
    /// 为什么必须报出来（fix round 1 的 Minor 1）：提交成功后这份副本**刻意保留**，
    /// 而它是"误恢复"唯一能退回的地方；不报路径的话，`rollback_path()` 就是个零调用者，
    /// 用户只能靠翻库目录才知道它存在。产品口径（保留多久、怎么清）留给 P8。
    /// 回滚路径上是 `None`：那时副本已经被改名回主库路径（被消耗掉了）。
    pub rollback: Option<PathBuf>,
}

/// 切换前后的路径账：③ 的回滚路径要用它把原库改回来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSwap {
    /// 主库路径（切换后这里已经是**候选库**）。
    target: PathBuf,
    /// 原库的回滚副本（同目录；提交后**保留**，见 [`RestoreSwap::rollback_path`]）。
    rollback: PathBuf,
    /// 恢复**之前**的 `data_epoch`——回滚路径广播的就是它（客户端据此知道恢复没发生）。
    previous_data_epoch: String,
    /// 恢复**之前**的 `run_id`（只用于诊断与报告）。
    previous_run_id: String,
    /// 候选库偏旧时的迁移前备份产物。
    migration_backup: Option<PathBuf>,
    /// 文件是否**已经**切换过（② 段失败时会把切换撤销并把它改回 `false`）。
    swapped: bool,
}

/// ② 段失败时的返回值：**原因 + 还可以用来回滚的路径账**。
///
/// 为什么错误要带着 `swap` 一起回：失败之后必须走 ③-b 把运行态重建起来（否则进程留在
/// "没有库的维护态"里，任何命令都只能拿到 `DATA_RESTORE_IN_PROGRESS`），而重建需要
/// 主库路径与恢复前的身份——那两样只有 `swap` 有。
///
/// **装在 `Box` 里返回**（`Result<RestoreSwap, Box<RestoreFailure>>`）：它带着整份路径账，
/// 不装箱的话 `Err` 变体比 `Ok` 大得多，clippy 的 `result_large_err` 会红。
#[derive(Debug)]
pub struct RestoreFailure {
    /// 失败原因（原样交给调用方）。
    pub error: AppError,
    /// 路径账：`swapped == false`（② 段已经自己撤销过切换），所以 ③-b 不必再撤一次。
    pub swap: RestoreSwap,
}

impl RestoreSwap {
    /// 切换后主库路径（= 恢复完成之后 `Db::open` 的那个路径）。
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// 恢复之前的 `data_epoch`。
    pub fn previous_data_epoch(&self) -> &str {
        &self.previous_data_epoch
    }

    /// 恢复之前的 `run_id`。
    pub fn previous_run_id(&self) -> &str {
        &self.previous_run_id
    }

    /// 原库回滚副本的路径；切换是否已经发生（`false` = 主库路径上还是原库）。
    pub fn is_swapped(&self) -> bool {
        self.swapped
    }

    /// 原库回滚副本的路径。
    ///
    /// **提交成功后它仍然留在磁盘上**：它是"恢复之前那个世界"的唯一完整副本，而计划没有
    /// 授权任何一步悄悄删掉它（下一次恢复会先删掉它再用新的副本顶上）。
    /// 要手工回退：把主库文件挪开，再把这个文件改名成主库路径即可。
    pub fn rollback_path(&self) -> &Path {
        &self.rollback
    }

    /// 同目录可回滚切换：原库 → 回滚副本，候选库（`staged`）→ 主库路径。
    ///
    /// 第一步失败（原库改不出去）⇒ 什么都没变；第二步失败 ⇒ **把原库改回来**再报错。
    /// 副作用只在"两步都成功"之后才置 `swapped`。
    ///
    /// **还原那一步不许吞错误**（fix round 1 的 Important）：第二步失败之后主库路径上
    /// **已经没有原库了**（它刚被改名为回滚副本）。如果"改回来"也失败而我们照旧返回
    /// 一个"切换没发生"的错误，调用方会以为原库还在主库路径上——而 `Db::open` 会
    /// **创建一个空文件**，下一次启动就把这间空库当成了用户的数据（静默的数据丢失）。
    /// 所以这条路径返回的错误**必须点明原库在哪**，让用户/接线方找得到它。
    fn swap_files(&mut self, staged: &Path) -> Result<(), AppError> {
        let had_original = self.target.exists();
        if had_original {
            remove_db_files(&self.rollback)?;
            std::fs::rename(&self.target, &self.rollback)
                .map_err(|e| restore_io("swap original aside", e))?;
            // 边车（`-wal`/`-shm`）跟着原库走：留一份旧库的 WAL 在主库路径上，
            // 下一次打开会拿它去恢复一个**不相干**的库（正是"不能覆盖仍打开的
            // WAL 数据库"要防的那件事，只是方向相反）。
            move_sidecars(&self.target, &self.rollback)?;
        }
        match std::fs::rename(staged, &self.target) {
            Ok(()) => {
                self.swapped = true;
                Ok(())
            }
            Err(error) => {
                if had_original {
                    // 撤销：把原库改回主库路径。两步都可能失败（同一类 IO 故障），
                    // 任何一步失败都**原样报出去**，绝不吞。
                    let undone = move_sidecars(&self.rollback, &self.target).and_then(|()| {
                        std::fs::rename(&self.rollback, &self.target)
                            .map_err(|e| restore_io("swap original back", e))
                    });
                    if let Err(undo) = undone {
                        return Err(restore_io_at(
                            "swap candidate in (and the original could not be moved back)",
                            &self.rollback,
                            undo,
                        ));
                    }
                }
                Err(restore_io("swap candidate in", error))
            }
        }
    }

    /// **两条能走到 `Db::open` 的入口各自拒绝造库**（fix round 1/2 的 Important）。
    ///
    /// 为什么必须有：`Db::open` 底层是 `Connection::open`——文件不存在时它会
    /// **创建一个空文件**；而空文件的 `user_version = 0` 会被下一次启动当成
    /// "需要迁移的旧库"⇒ 备份它（空的）→ 迁移 → `init_meta` ⇒
    /// 用户的数据被静默换成一间空库。
    ///
    /// **口径（与实现相符，别写成"永不"）**：判据是**逐入口**的——本模块封不住既有的
    /// `Db::open`，所以 [`commit_restore`] 与 [`abort_restore`] 各判一次。
    /// 拒绝之后调用方**留在维护态**：所有命令被拒、没有任何写入落进错误的地方，
    /// 这比"以为库是好的、其实打开了空库"诚实得多。
    ///
    /// 空路径（内存库）单独给一条可读文案：那条防线分支在建 swap 时就没有主库路径，
    /// 不该报出"…is not there: ; the original library should be at "这种半句话。
    fn require_library_in_place(&self) -> Result<(), AppError> {
        if self.target.as_os_str().is_empty() {
            return Err(AppError::Storage {
                detail: "restore: in-memory database has no path to swap".to_string(),
            });
        }
        if !self.target.exists() {
            return Err(AppError::Storage {
                detail: format!(
                    "restore: refusing to open a database that is not there: {}; \
                     the original library should be at {} — move it back before retrying",
                    self.target.display(),
                    self.rollback.display()
                ),
            });
        }
        Ok(())
    }

    /// 把主库路径改回原库（③-b 的第一句）。
    ///
    /// 顺序：先把**候选库**从主库路径挪开（它已经不可信：可能只跑了一半的事务、
    /// 或者根本没有库身份），再把回滚副本改回主库路径。两份边车文件一起搬。
    ///
    /// 候选库文件**直接删掉**（不是留成第三份副本）：它只是用户挑的那份备份的拷贝，
    /// 原产物还在备份目录里；留一份"失败的候选库"在库目录里只会让人以为那是可用的库。
    fn rollback_files(&self) -> Result<(), AppError> {
        remove_db_files(&self.target)?;
        std::fs::rename(&self.rollback, &self.target)
            .map_err(|e| restore_io("rollback original", e))?;
        move_sidecars(&self.rollback, &self.target)?;
        Ok(())
    }
}

/// 三段流程的**唯一入口**：① 进入维护态 + 取走运行态（锁内，短）→ ② 关连接、临时路径
/// 验证、同目录可回滚切换（**不持锁**）→ ③ 提交或回滚（锁内）。
///
/// **必须在一次调用里走完**（P8 接线时也只在一个 `#[tauri::command]` 的阻塞段里调它）：
/// ②③ 之间没有任何 IPC 命令进得来（维护态里全部命令被 `guard_writable` 拒），
/// 而恢复自己**不重新进 `commands::run_command`**——这条约束的落点就是本函数
/// （另有一条**硬判据**钉住"不在持锁线程上开始"：`debug_assert!` 在 release 会被编译掉，
/// 接线缺陷必须在 release 也明确失败，见 P6 终审 M-2）。
///
/// - `backup`：待恢复的产物（`VACUUM INTO` 出来的单文件快照，或任何一份合法的库文件）。
/// - `backup_dir`：候选库 schema 偏旧时，迁移前那次备份的落盘目录（`None` = 生产缺省）。
/// - `clock`：**新**协调器的时钟来源。必须与组合根交给 `startup` 的那一份**同源**
///   （生产是同一个 `SystemClock` 实例的克隆）：OS 事件边界样本的 `monotonic_ms` 与
///   协调器落在同一个原点上，否则 `system_pause` 的边界校验必然拒绝（R-02 静默落空）。
///
/// 返回：**成功提交** ⇒ `Ok(RestoreOutcome { committed: true, .. })`；
/// **没换成** ⇒ 先走 ③-b 把原库重建起来，再把它那条原因作为 `Err` 交回
/// （"恢复没发生"对用户是一次失败，而回滚本身是**结果**不是错误——`abort_restore`
/// 单独调用时返回 `Ok(committed: false)`）。回滚自己再失败 ⇒ 透出回滚那条错误。
pub fn restore_from_backup(
    app: &SharedApp,
    broadcaster: &Broadcaster,
    backup: &Path,
    backup_dir: Option<&Path>,
    clock: &ClockSource,
) -> Result<RestoreOutcome, AppError> {
    // **不重新进 `run_command` 的硬判据**（P6 终审 M-2）：恢复不能在已持有串行边界的
    // 线程上开始（① 要取锁），也不该从命令体内部被调用。原先这里只有 `debug_assert!`，
    // 而 release 会把它整条编译掉 ⇒ P8 一旦把恢复放进 `run_command` 闭包（正持锁）调用，
    // 现象是**静默死锁**（① 段的 `lock_app` 永远等不到）。判据与 [`RunningApp::shutdown`]
    // 的自死锁防线同一条标准：明确失败，并说清正确姿势；**拒绝时不碰任何东西**。
    if holds_app_lock(app) {
        return Err(AppError::Storage {
            detail: "恢复流程必须在锁外开始（它自己按三段取锁），不要在持有串行边界的线程上调用"
                .to_string(),
        });
    }

    // 诊断落点**只取一次**：两条失败臂共用（`Diagnostics` 只是路径句柄，Clone 便宜）。
    let diagnostics = lock_app(app).diagnostics().clone();

    // ① 进入维护态 + 取走运行态（锁内，短）。
    let runtime = begin_restore(app)?;

    // ② 关连接 + 临时路径验证 + 同目录可回滚切换（**不持锁**）。
    let swap = match prepare_and_swap(runtime, backup, backup_dir, clock, &diagnostics) {
        Ok(swap) => swap,
        Err(failure) => {
            // 拆箱：路径账与原因都要用（`RestoreFailure` 只是为了让 Err 变体不撑大 Result）。
            let RestoreFailure { error: cause, swap } = *failure;
            let rollback = swap.rollback_path().to_path_buf();
            return match abort_restore(app, broadcaster, swap, clock) {
                // 原库重建成功：把"这次恢复没做成"的原因交回调用方。
                Ok(_) => {
                    record_restore_failure(
                        &diagnostics,
                        "prepare_and_swap",
                        true,
                        &cause,
                        &rollback,
                    );
                    Err(cause)
                }
                // 连原库都重建不起来：那条错误更严重，直接透出（同样落盘，且点明
                // `rolled_back=false`——这正是"与正在恢复不可区分"的那条路径）。
                Err(rollback_error) => {
                    record_restore_failure(
                        &diagnostics,
                        "prepare_and_swap",
                        false,
                        &rollback_error,
                        &rollback,
                    );
                    Err(rollback_error)
                }
            };
        }
    };

    // ③-a 提交（锁内）。
    match commit_restore(app, broadcaster, &swap, clock) {
        Ok(outcome) => Ok(outcome),
        Err(cause) => {
            // ③-a 失败 ⇒ ③-b 回滚：把原库改回来、重开、新 run + 两步重扫 + 新协调器。
            let rollback = swap.rollback_path().to_path_buf();
            match abort_restore(app, broadcaster, swap, clock) {
                Ok(_) => {
                    record_restore_failure(&diagnostics, "commit_restore", true, &cause, &rollback);
                    Err(cause)
                }
                Err(rollback_error) => {
                    record_restore_failure(
                        &diagnostics,
                        "commit_restore",
                        false,
                        &rollback_error,
                        &rollback,
                    );
                    Err(rollback_error)
                }
            }
        }
    }
}

/// 恢复失败的**唯一落盘出口**（P6 终审 I-2）。
///
/// 字段口径：`stage` = 哪一段失败（`prepare_and_swap` / `commit_restore`）、
/// `rolled_back` = 原库有没有被重建回来、`error` = **交回调用方的那条错误**
/// （回滚也失败时透出的是回滚那条，所以这里记的也是它）、`rollback` = 回滚副本路径
/// （`rolled_back=false` 时它就是"恢复之前那个世界"还在的地方）。
fn record_restore_failure(
    diagnostics: &Diagnostics,
    stage: &str,
    rolled_back: bool,
    error: &AppError,
    rollback: &Path,
) {
    diagnostics.record(
        RESTORE_FAILED,
        &format!(
            "stage={stage} rolled_back={rolled_back} code={} rollback={} detail={}",
            error.code(),
            rollback.display(),
            error.detail().unwrap_or_default()
        ),
    );
}

/// ① 段：进入维护态 + 取走运行态（**锁内，短**）。
///
/// 返回的 [`Runtime`] 由调用方在**锁外**持有/丢弃：旧 `Db` 随它 drop 时连接才真正关闭
/// （02 §9 的"关闭连接"），而它**不含** [`crate::platform::scheduler::Scheduler`]
/// （G11 的自死锁陷阱，见 `services::bootstrap` 模块头）。
///
/// 已处于维护态（或退出意图已置位）⇒ 拒绝：`begin_maintenance` 不重入；
/// 而进入时刻取自**同一条时钟接缝**（`AppState::now_ms`），运行态不在手时那一步就先拒了。
pub fn begin_restore(app: &SharedApp) -> Result<Runtime, AppError> {
    let mut state = lock_app(app);
    let entered_at_ms = state.now_ms()?;
    state.begin_maintenance(MaintenancePhase::Restore, entered_at_ms)?;
    match state.take_runtime() {
        Ok(runtime) => Ok(runtime),
        // **加固**（fix round 1 的 Minor 7）：`begin_maintenance` 已经置位而运行态取不出来时，
        // 不能把维护态**永久闩住**（那会让所有命令永远拿到 `DATA_RESTORE_IN_PROGRESS`、
        // 且没有任何回滚路径）。今天不可达（两者在同一把锁的同一个临界区里），
        // 但一行清位就能把这条"闩死"的可能性关掉。
        Err(error) => {
            state.end_maintenance();
            Err(error)
        }
    }
}

/// ② 段：关连接 → 候选库拷到同目录暂存 → 完整性/外键/版本验证（旧版本先备份再迁移）
/// → 同目录可回滚切换。**全程不持锁**。
pub fn prepare_and_swap(
    runtime: Runtime,
    backup: &Path,
    backup_dir: Option<&Path>,
    clock: &ClockSource,
    diagnostics: &Diagnostics,
) -> Result<RestoreSwap, Box<RestoreFailure>> {
    // 路径与身份必须在**关连接之前**读出来（`Runtime` 一 drop，库句柄就没了）。
    // 路径读不到（内存库）时连路径账都建不起来，只能报一个空 swap——生产上库一定在磁盘上
    // （`Db::open` 是唯一入口），所以那条分支只是防线。
    let Some(target) = runtime.db.path().map(Path::to_path_buf) else {
        return Err(Box::new(RestoreFailure {
            error: AppError::Storage {
                detail: "restore: in-memory database has no path to swap".to_string(),
            },
            swap: RestoreSwap {
                target: PathBuf::new(),
                rollback: PathBuf::new(),
                previous_data_epoch: String::new(),
                previous_run_id: String::new(),
                migration_backup: None,
                swapped: false,
            },
        }));
    };
    let mut swap = RestoreSwap {
        rollback: rollback_path(&target),
        target,
        previous_data_epoch: String::new(),
        previous_run_id: runtime.coordinator.run_id().to_string(),
        migration_backup: None,
        swapped: false,
    };

    // 当前库的身份：恢复**之前**那个 `data_epoch`（回滚路径广播的就是它）。
    // 读不到就不是一次可以开始的恢复——原库没有 `app_meta` 行时它是读不出来的。
    match runtime.db.connection().query_row(
        "SELECT data_epoch FROM app_meta WHERE singleton = 1",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(epoch) => swap.previous_data_epoch = epoch,
        Err(error) => {
            let error = AppError::Storage {
                detail: format!("restore: cannot read current data_epoch: {error}"),
            };
            return Err(Box::new(RestoreFailure { error, swap }));
        }
    }

    // **关闭连接**：旧 `Db` 随 `runtime` 一起 drop。WAL 在最后一次连接干净关闭时被
    // checkpoint 回主库文件并删除，之后才轮到"同目录改名"。
    drop(runtime);

    let staged = staged_path(&swap.target);
    // 候选库 → 同目录暂存（"临时路径验证"）。先清掉上一次失败留下的同名文件。
    let copied = remove_db_files(&staged)
        .and_then(|()| std::fs::copy(backup, &staged).map_err(|e| restore_io("stage candidate", e)))
        .and_then(|_| {
            validate_candidate(&staged, backup, backup_dir, clock, diagnostics)
                .map(|artifact| swap.migration_backup = artifact)
        });
    if let Err(error) = copied {
        let _ = remove_db_files(&staged);
        return Err(Box::new(RestoreFailure { error, swap }));
    }

    // 同目录可回滚切换（原库留成回滚副本）。
    if let Err(error) = swap.swap_files(&staged) {
        let _ = remove_db_files(&staged);
        return Err(Box::new(RestoreFailure { error, swap }));
    }
    Ok(swap)
}

/// 候选库的验证：完整性 → 外键 → schema 版本（未来版本**拒绝**；旧版本**先备份再迁移**）。
///
/// `Db::open` 顺带把"磁盘库必须是 WAL"这条既有校验也走一遍（`storage/db.rs`）。
///
/// **`app_meta` 的缺失不在这里判**：02 §9 给这道门的判据就是这三条。库身份由 ③-a 的
/// `rotate_epoch` 兜底——没有 `app_meta` 行时它拒绝（影响行数 ≠ 1），而那个事务与
/// `start_run` 同一个事务，所以**什么都不会落库**。
///
/// 返回旧版本分支里那份迁移前备份的产物路径（没走那条分支就是 `None`）。
///
/// `backup` 是**用户选中的那份产物**（不是 `staged` 副本）：旧版本分支的保留策略要护住它
/// （A1），所以这里必须拿到原路径。
fn validate_candidate(
    staged: &Path,
    backup: &Path,
    backup_dir: Option<&Path>,
    clock: &ClockSource,
    diagnostics: &Diagnostics,
) -> Result<Option<PathBuf>, AppError> {
    let db = Db::open(staged)?;
    let integrity: String = db
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(map_sqlite)?;
    if !integrity.eq_ignore_ascii_case("ok") {
        return Err(AppError::Storage {
            detail: format!("restore: integrity_check failed on candidate: {integrity}"),
        });
    }

    // 外键：`foreign_key_check` 有行就是违规（空集 = 通过）。
    let violations: i64 = {
        let mut statement = db
            .connection()
            .prepare("SELECT count(*) FROM pragma_foreign_key_check")
            .map_err(map_sqlite)?;
        statement
            .query_row([], |row| row.get(0))
            .map_err(map_sqlite)?
    };
    if violations != 0 {
        return Err(AppError::Storage {
            detail: format!("restore: foreign_key_check failed on candidate: {violations} rows"),
        });
    }

    let from_version = current_version(db.connection())?;
    if from_version > SCHEMA_VERSION {
        return Err(AppError::Storage {
            detail: format!(
                "restore: candidate schema v{from_version} is newer than this build (v{SCHEMA_VERSION})"
            ),
        });
    }
    if from_version == SCHEMA_VERSION {
        return Ok(None);
    }

    // 旧版本：**先备份再迁移**（02 §9 的括号原文）。用的是与迁移前备份同一个原语，
    // 阶段标记不同（`pre-restore backup`），产物路径随结果交回调用方。
    //
    // `also_keep = Some(backup)`（A1）：这次备份落在**用户自己的**备份目录里、与
    // "用户正在恢复的那一份"同目录，所以保留策略必须同时护住它——否则用户选中一份
    // 次老的产物来恢复，回头就发现它被这次"迁移前备份"的清理删掉了。
    let artifact = backup_consistent_keeping(
        backup_dir,
        db.connection(),
        from_version,
        &*clock(),
        PRE_RESTORE_STAGE,
        diagnostics,
        Some(backup),
    )?;
    migrate(db.connection())?;
    Ok(Some(artifact))
}

/// ③-a 段：提交（**锁内**）。顺序写死，别调换：
///
/// `Db::open(新路径)` → **同一事务**里 `run_repo::start_run(new_run_id)`
/// **+ `meta::rotate_epoch`**（**只有这条路径** rotate；回滚保留原 epoch）
/// → `services::recovery::scan_at_startup(&mut db, …)`（P3 的四类归一）
/// → [`scan_recovery`]（门禁；**必须在归一之后**，它只是三条只读查询）
/// → `Coordinator::new(clock, new_run_id)` + `establish_anchor(sample)`
/// → `install_runtime` → `end_maintenance` → 广播**新 epoch** 的 `domain.changed`。
///
/// **两步重扫的顺序不能倒**，也不能合成一步：`scan_recovery` 不归一，缺了前一步，
/// 备份里旧 run 的 `running` 会话会永远停在 `running` 并占着全局的
/// `uq_running_foreground`（那条索引不带 run 过滤）⇒ 恢复之后根本 start 不起来。
pub fn commit_restore(
    app: &SharedApp,
    broadcaster: &Broadcaster,
    swap: &RestoreSwap,
    clock: &ClockSource,
) -> Result<RestoreOutcome, AppError> {
    if !swap.swapped {
        return Err(AppError::Storage {
            detail: "restore: candidate is not swapped in yet".to_string(),
        });
    }

    let mut state = lock_app(app);
    // 顺序写死：**先判维护态、再判"库在不在"**（评审 Minor 2）。反过来的话，
    // "不在维护态 + 目标缺失"这条组合会从 `DATA_RESTORE_IN_PROGRESS` 变成 `STORAGE_ERROR`，
    // 把一次接线缺陷说成文件问题。
    require_maintenance_without_runtime(&state)?;
    swap.require_library_in_place()?;

    // 新 run 的起点与归属基线用**同一个样本**（与 `startup` 第③/⑤步同一口径）。
    // 钟**只在这里要一只**：它随后就归新协调器所有。
    let clock = clock();
    let sample = clock
        .sample()
        .map_err(|_| clock_unavailable("restore: clock sample unavailable at commit"))?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut db = Db::open(&swap.target)?;

    // 提交路径的**同一事务**：新 run + 新 epoch。`rotate_epoch` 只在这里调一次，
    // 它不 bump revision（00 §5：恢复后的 revision 可以低于原库，只在新 epoch 内比较）。
    //
    // 身份判据**不在这里**：它在 [`rebuild_and_install`] 里、`install_runtime` **之前**判
    // （fix round 2 的 Minor 1）——过了 `install_runtime` 这次恢复就已经上线了，
    // 那时再报错等于把一次已提交、已广播的恢复说成失败。
    {
        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        run_repo::start_run(&tx, &run_id, sample.wall_ms)?;
        meta::rotate_epoch(&tx)?;
        tx.commit().map_err(map_sqlite)?;
    }

    let identity = rebuild_and_install(
        &mut state,
        broadcaster,
        db,
        &run_id,
        sample,
        clock,
        true,
        &swap.previous_data_epoch,
    )?;
    // 身份判据已经在 `rebuild_and_install` 里、装卸之前判过（提交必须是全新身份）。
    state.diagnostics().record(
        RESTORE_DIAGNOSTIC,
        &format!(
            "outcome=committed run_id={run_id} data_epoch={} previous_data_epoch={} rollback={}",
            identity.data_epoch,
            swap.previous_data_epoch,
            swap.rollback.display()
        ),
    );
    Ok(RestoreOutcome {
        committed: true,
        data_epoch: identity.data_epoch,
        run_id,
        recovery: identity.recovery,
        migration_backup: swap.migration_backup.clone(),
        // 提交成功 ⇒ 原库留成回滚副本（**不删**），路径报给调用方。
        rollback: Some(swap.rollback.clone()),
    })
}

/// ③-b 段：回滚（**锁内**）。先把主库路径改回原库（若切换已经发生过），再**重开原库并
/// 重建运行态**——`Db::open(原路径)` → 新 run（**不 rotate**）→ 两步重扫 → 新协调器 →
/// `install_runtime` → `end_maintenance` → 广播（**原 epoch**：客户端据此知道恢复没发生）。
///
/// **不回装原来的协调器**：它的 `Instant` 基线随旧 `Db`/旧 run 一起作废，复用它就是
/// 把维护窗口算进工时（计划原文禁止）。重建可能把原来 `running` 的会话推成 `recovering`
/// ——那正是 P3 的恢复规则，原库的工时事实一条都不会丢。
pub fn abort_restore(
    app: &SharedApp,
    broadcaster: &Broadcaster,
    swap: RestoreSwap,
    clock: &ClockSource,
) -> Result<RestoreOutcome, AppError> {
    let mut state = lock_app(app);
    // 顺序写死（评审 Minor 2 + 修复轮 2 的 Important）：
    // **先判状态、再动文件、最后判库在不在**。
    //
    // 反过来（先 `rollback_files()`、后判维护态）会在"运行态还活着"的时候把主库文件
    // 换掉，最后返回一句 `DATA_RESTORE_IN_PROGRESS`——文件被动了、状态没动，
    // 是最难查的一类不一致。状态是这条路径的**前置条件**，先判它。
    require_maintenance_without_runtime(&state)?;
    if swap.swapped {
        swap.rollback_files()?;
    }
    // 还原之后（或从未切换过时）主库路径上必须**已经**有一份库：
    // 没有就拒绝打开它——恢复流程不造库（见 [`RestoreSwap::require_library_in_place`]）。
    swap.require_library_in_place()?;

    let clock = clock();
    let sample = clock
        .sample()
        .map_err(|_| clock_unavailable("restore: clock sample unavailable at rollback"))?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let mut db = Db::open(&swap.target)?;

    // **不 rotate**：原库的 epoch 原样保留——否则"旧 epoch 的请求被拒"这条判据
    // 会把一个**没被替换**的库也一起拒掉。身份判据在 [`rebuild_and_install`] 里
    // 装卸之前判（fix round 2 的 Minor 1）。
    {
        let tx = db
            .connection_mut()
            .unchecked_transaction()
            .map_err(map_sqlite)?;
        run_repo::start_run(&tx, &run_id, sample.wall_ms)?;
        tx.commit().map_err(map_sqlite)?;
    }

    let identity = rebuild_and_install(
        &mut state,
        broadcaster,
        db,
        &run_id,
        sample,
        clock,
        false,
        &swap.previous_data_epoch,
    )?;
    state.diagnostics().record(
        RESTORE_DIAGNOSTIC,
        &format!(
            "outcome=rolled_back run_id={run_id} data_epoch={} previous_data_epoch={}",
            identity.data_epoch, swap.previous_data_epoch
        ),
    );
    Ok(RestoreOutcome {
        committed: false,
        data_epoch: identity.data_epoch,
        run_id,
        recovery: identity.recovery,
        migration_backup: swap.migration_backup.clone(),
        // 回滚路径上那份副本**被消耗掉了**（改名回主库路径），所以没有可发现的副本。
        rollback: None,
    })
}

/// 两条 ③ 路径共用的后半段：**两步重扫 → 新协调器 + 锚点 → 装卸 → 广播**。
///
/// 广播用的 `data_epoch`/`revision` 在这里、**在两步重扫之后**才读：P3 的归一可能改事实，
/// 而那会**增加一次 revision**（02 §"启动扫描的版本与审计补充"）——广播必须报库**最终**
/// 的那个版本，否则客户端会拿着比库小一号的水位线（`domain.changed` 的 `revision` 就是
/// 客户端的水位线，差一格就要靠下一次 `get_revision` 兜）。
#[allow(clippy::too_many_arguments)]
fn rebuild_and_install(
    state: &mut crate::services::bootstrap::AppGuard<'_>,
    broadcaster: &Broadcaster,
    mut db: Db,
    run_id: &str,
    sample: ClockSample,
    clock: Box<dyn Clock + Send>,
    committed: bool,
    previous_data_epoch: &str,
) -> Result<Identity, AppError> {
    // 第一步：P3 的四类归一（**唯一**做归一的那一步）。备份必然可能带旧 run 的
    // `running` 会话（备份取自计时中的库），不归一它就会永远占着全局唯一索引。
    let _scan_report = crate::services::recovery::scan_at_startup(&mut db, run_id, sample.wall_ms)?;
    // 第二步：门禁快照（三条只读查询，**必须在归一之后**，否则判的是归一前的事实）。
    let recovery = scan_recovery(db.connection(), run_id)?;
    // **权威身份**：两步重扫之后的库身份与版本（归一可能刚推进过一次 revision）。
    let meta = meta::require_meta(db.connection())?;

    // **身份判据**（fix round 1 的真判据，fix round 2 挪到这里 = 装卸**之前**）：
    // 提交必须换新身份、回滚必须保留原身份。位置是契约的一部分——过了下面
    // `install_runtime` 那条线，这次恢复就已经"上线"（运行态装回、维护态清掉、广播已发），
    // 那时再报错只会把一个已提交的恢复说成失败，而错误路径还会去动活着的运行态。
    // 放在这里：失败 ⇒ 运行态仍未装回、维护态仍在 ⇒ 调用方走 ③-b 是安全的。
    match (
        committed,
        meta.data_epoch.as_str() == previous_data_epoch,
    ) {
        // **这一臂构造性不可达，所以不为它造注入**（4b 残余①）：提交路径在上面那个
        // 事务里先 `rotate_epoch`（写一个全新的 uuid），它失败（影响行数 ≠ 1）就走不到
        // 这里；而"新 uuid 恰好等于旧值"要撞上 2^-122 量级的巧合。要真驱动它，只能改
        // `rotate_epoch` 的产物形状（让它可以返回旧值）——那是为一条不可达路径改生产
        // 代码，代价与收益不成比例。回滚那一臂**有**真驱动的用例
        // （`abort_restore_refuses_a_library_whose_identity_changed`：改掉回滚副本的
        // epoch，③-b 必须拒绝上线）。
        (true, true) => {
            return Err(AppError::Storage {
                detail: "restore: commit did not rotate the data_epoch".to_string(),
            })
        }
        (false, false) => {
            return Err(AppError::Storage {
                detail: format!(
                    "restore: rollback kept the wrong library identity: expected {previous_data_epoch}, found {}",
                    meta.data_epoch
                ),
            })
        }
        _ => {}
    }

    // **新协调器**（两条路径都不复用维护前那一个）：时钟来自调用方，锚点用同一个样本。
    // `tick_seq` 随新协调器从 0 起算，旧 `Instant` 基线随旧协调器一起作废。
    let mut coordinator = Coordinator::new(clock, run_id.to_string());
    coordinator.establish_anchor(sample);

    // 装回运行态 → 结束维护态（顺序不能反：结束维护的时长要从**新**运行态那条时钟接缝读）。
    state.install_runtime(Runtime { db, coordinator }, recovery.clone())?;
    state.end_maintenance();

    // 恢复完成/回滚的**唯一**广播：`domain.changed` 带**恢复之后**的 epoch。
    // 客户端闸门规则①（未知 epoch ⇒ 重新握手）据此自动重新握手——**不新造事件名**。
    broadcaster.emit(EventEnvelope::domain_changed(
        meta.data_epoch.clone(),
        meta.revision,
        sample.wall_ms,
        serde_json::json!({
            "restore": if committed { "committed" } else { "rolled_back" },
            "run_id": run_id,
            "previous_data_epoch": previous_data_epoch,
        }),
    ));

    Ok(Identity {
        data_epoch: meta.data_epoch,
        recovery,
    })
}

/// [`rebuild_and_install`] 装完之后库里的**权威身份**（广播用的就是这一份）。
struct Identity {
    data_epoch: String,
    recovery: RecoveryScan,
}

/// ③ 两段的前置：**还在维护态、运行态不在手**。不满足就是不变量被破坏（不是用户错误），
/// 照实报 `DATA_RESTORE_IN_PROGRESS`——它正是"此刻没有可用运行态"那个码。
fn require_maintenance_without_runtime(
    state: &crate::services::bootstrap::AppGuard<'_>,
) -> Result<(), AppError> {
    if state.maintenance().is_none() || state.runtime_present() {
        return Err(AppError::DataRestoreInProgress);
    }
    Ok(())
}

fn clock_unavailable(detail: &str) -> AppError {
    AppError::Storage {
        detail: detail.to_string(),
    }
}

fn restore_io(stage: &str, e: std::io::Error) -> AppError {
    AppError::Storage {
        detail: format!("restore: {stage}: {e}"),
    }
}

/// 与 [`restore_io`] 相同，外加一句**原库在哪里**——只在"原库不在主库路径上"的失败里用。
///
/// 为什么单列一条：这类失败之后用户的数据**没有丢**，只是躺在一个可预期的路径上
/// （`<db>.restore-rollback`）。错误文案里必须写出那个路径，否则现象是
/// "应用起不来 + 库目录里多一个看不懂的文件"。
fn restore_io_at(stage: &str, stranded: &Path, cause: AppError) -> AppError {
    AppError::Storage {
        detail: format!(
            "restore: {stage}: {cause:?}; the original library is at {} (rename it back to the database path before restarting)",
            stranded.display()
        ),
    }
}

// ── 文件助手（都在同一个目录里改名字，所以不跨卷） ──────────────────────────────

/// SQLite 的边车文件是「主库路径 + 后缀」（不是换扩展名）：`worktrace.db-wal`。
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// 把 `from` 的两份边车文件搬到 `to`（两边同名后缀）。
fn move_sidecars(from: &Path, to: &Path) -> Result<(), AppError> {
    for suffix in ["-wal", "-shm"] {
        let source = sidecar(from, suffix);
        if !source.exists() {
            continue;
        }
        let destination = sidecar(to, suffix);
        if destination.exists() {
            std::fs::remove_file(&destination).map_err(|e| restore_io("replace sidecar", e))?;
        }
        std::fs::rename(&source, &destination).map_err(|e| restore_io("move sidecar", e))?;
    }
    Ok(())
}

/// 删掉一份库文件**及其边车**（不存在就当作成功）。
fn remove_db_files(path: &Path) -> Result<(), AppError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| restore_io("remove db file", e))?;
    }
    for suffix in ["-wal", "-shm"] {
        let extra = sidecar(path, suffix);
        if extra.exists() {
            std::fs::remove_file(&extra).map_err(|e| restore_io("remove sidecar", e))?;
        }
    }
    Ok(())
}

fn staged_path(target: &Path) -> PathBuf {
    sidecar(target, STAGED_SUFFIX)
}

fn rollback_path(target: &Path) -> PathBuf {
    sidecar(target, ROLLBACK_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合规的产物名（保留策略只认自己写出来的名字）。
    fn artifact_name(at_ms: i64) -> String {
        format!("{BACKUP_PREFIX}{BACKUP_FORMAT_VERSION}-s1-v0.1.0-{at_ms}{BACKUP_SUFFIX}")
    }

    /// **保留策略清理失败要落诊断**（P6 终审 M-3）。
    ///
    /// 形态：目录里放 6 份产物 + 1 份**同名目录**（`remove_file` 对目录必然失败，
    /// 两个平台一致），它按时间戳是最老的一份 ⇒ 清理会删它、失败、记一条。
    /// 这条诊断是 release 里唯一的线索：那时没有控制台，`eprintln!` 没人看得见。
    #[test]
    fn a_failed_prune_records_a_diagnostic_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("worktrace.log");
        let diagnostics = Diagnostics::to_file(&log);

        let names: Vec<String> = (0..7).map(|i| artifact_name(1_000 + i)).collect();
        // 最老的那一份做成目录（删不掉），其余做成正常文件。
        std::fs::create_dir(dir.path().join(&names[0])).unwrap();
        for name in &names[1..] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        // `just_written` 不在这个目录里：本次产物不进候选，7 份 ⇒ 超出 2 份。
        let just_written = dir.path().join("elsewhere.db");

        prune_old_backups(dir.path(), &[just_written.as_path()], &diagnostics);

        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains(&format!("event={BACKUP_PRUNE_FAILED}")),
            "清理失败必须落诊断：{text}"
        );
        assert!(
            text.contains(&format!("file={}", dir.path().join(&names[0]).display())),
            "诊断里要点明删不掉的是哪一份：{text}"
        );
        assert!(
            dir.path().join(&names[0]).is_dir(),
            "删不掉的那一份（目录）还在"
        );
        assert!(
            !dir.path().join(&names[1]).exists(),
            "能删的那一份（次老）必须真的被删掉"
        );
    }

    /// 正控：全部删得掉时**一条诊断都不写**——否则上面那条就不是"失败才写"。
    #[test]
    fn a_successful_prune_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("worktrace.log");
        let diagnostics = Diagnostics::to_file(&log);

        for i in 0..7 {
            std::fs::write(dir.path().join(artifact_name(1_000 + i)), b"x").unwrap();
        }
        let elsewhere = dir.path().join("elsewhere.db");
        prune_old_backups(dir.path(), &[elsewhere.as_path()], &diagnostics);

        assert!(
            !log.exists(),
            "清理成功不该产生任何诊断行（`Diagnostics` 只在真的写时才建文件）"
        );
        assert!(!dir.path().join(artifact_name(1_000)).exists());
        assert!(!dir.path().join(artifact_name(1_001)).exists());
        assert!(dir.path().join(artifact_name(1_002)).exists());
    }
}
