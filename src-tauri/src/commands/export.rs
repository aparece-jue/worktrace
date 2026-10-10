//! 导出与备份 / 恢复（P8 Task 3a：命令 9/10/11）的请求与响应 DTO、落盘辅助与命令体；`#[tauri::command]` 包装留在 `super`。

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::domain::error::DomainError;
use crate::envelope::WriteEnvelope;
use crate::error::AppError;
use crate::platform::clock::Clock;
use crate::services::bootstrap::{holds_app_lock, lock_app, AppState, SharedApp};
use crate::services::events::Broadcaster;
use crate::services::{backup, export, handshake, stats};

use super::unknown_enum_value;

// ─────────────────────────────────────────────────────────────────────────────
// 导出与备份 / 恢复的请求与响应 DTO（P8 Task 3a：命令 9/10/11）
// ─────────────────────────────────────────────────────────────────────────────

/// 导出请求（命令 9）：按 `format` 判别该带哪些标量。
///
/// **判别式，不是"都能传"**：`json` 要 `from`/`to`（半开范围）；`markdown` 只要可选的
/// `anchor`（这一周里的**任一刻**，周界由服务算）。不适用却传了 ⇒ 命令体**显式拒绝**
/// （不是"忽略它"）：忽略会让调用方以为自己指定了范围，而服务用的是另一套口径
/// （计划「2026-10-08 第三轮契约收口」的导出 DTO 条：不接受同时传不适用字段的含糊请求）。
///
/// `from` / `to` / `anchor` 都是 `Option`：必填与否**取决于判别式**，而 serde 表达不了
/// "当 format=json 时必填"——那条规则落在 [`parse_export_request`] 上（拒绝原因可读，
/// 且是 `DOMAIN_ERROR` 而不是传输层的反序列化错误）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ExportRequest {
    /// `json`（范围明细）或 `markdown`（自然周回顾）。
    pub format: String,
    /// 用户时区（原始输入，归一在服务那一条唯一入口上）。
    pub timezone: String,
    pub expected_data_epoch: String,
    /// `json` 必填：范围起点（含）。
    pub from: Option<i64>,
    /// `json` 必填：范围终点（**不含**）。
    pub to: Option<i64>,
    /// `markdown` 可选：这一周里的任一刻；省略 = 同一次样本的归属终点（"本周"）。
    pub anchor: Option<i64>,
}

/// 备份请求（命令 10）。只带库身份：它不修改任何业务事实，没有可校验的实体版本。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BackupRequest {
    pub expected_data_epoch: String,
}

/// 恢复请求（命令 11）：全项目唯一的危险操作。
///
/// `confirmed` 是**命令层再校验一次**的二次确认位——界面上的二次确认是硬前置，但
/// 不能只靠界面。缺它（反序列化失败）或为 `false` 都拒绝，且**不产生任何副作用**。
/// `expected_data_epoch` 是**进维护态之前**的身份守卫：拿旧展示来点恢复 ⇒
/// `DATA_EPOCH_MISMATCH`，此时进程状态与磁盘一个字节都没变。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RestoreRequest {
    /// 待恢复的备份产物路径（用户从备份目录里挑的那一份）。
    pub backup_path: String,
    pub expected_data_epoch: String,
    pub confirmed: bool,
}

/// 一次导出的结果（命令 9）：**真实存在的绝对路径** + 与内容同一份数据的版本信封。
///
/// 来由（P8 Task 3a）：P5 只产出内容（`ExportJson` / `ExportMarkdown` 是服务层类型、
/// 没有 serde），**落盘与 IPC 归 P8**（P5 计划把落盘订正给 P8）。`data_epoch` /
/// `revision` **直接取自这一次生成结果**——不在写文件之后重新查询（计划第三轮契约
/// 收口：那样报的是"响应时刻"的版本，与文件里的数字就不再同源）。
///
/// 落盘本身**不推进业务 `revision`**、不广播 `domain.changed`：写文件不是业务事实。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportResult {
    /// 落盘后的**绝对**路径（界面据此 `revealItemInDir`，也可复制给用户）。
    pub path: String,
    /// 写进文件的字节数（UTF-8 长度，与文件大小逐字节相符）。
    pub bytes: u64,
    pub data_epoch: String,
    pub revision: i64,
}

/// 一次备份的结果（命令 10）。
///
/// 来由：备份产生一份库副本、**不修改业务事实** ⇒ **不加 `revision`**、不广播
/// `domain.changed`（计划第 10 条）。信封里的两个值就是这次备份时刻的权威身份与版本，
/// 与写命令的"提交后读回"同一口径——只是这里没有提交，所以它们是**产物落地之后**
/// 读回来的同一个值（判据：备份命令里没有 `bump_revision`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupResult {
    /// 备份产物的**绝对**路径（`VACUUM INTO` 出来的单文件快照）。
    pub path: String,
    /// 产物大小（字节）。
    pub bytes: u64,
    pub data_epoch: String,
    pub revision: i64,
}

/// 一次恢复的结果（命令 11）。
///
/// 来由：`revision` **必须在恢复的锁内冻结**（勘察 §1-B6 / 决策 3）。`RestoreOutcome`
/// 原先不带它，P8 扩了返回材料：`services::backup::Identity` 在**两步重扫之后、
/// `install_runtime` 之前**、仍持同一把锁时读回 `meta.revision`，提交与回滚两条路径
/// 各自把它放进 outcome。**不得**换库放锁之后再读一次拼上去——那样 `data_epoch` 与
/// `revision` 就不是同一个时刻的两个字段了。
///
/// `applied` 在成功时恒为 `true`：服务把"没换成"当 `Err` 交回（回滚成功也报原始
/// `Err(cause)`），所以**没有可达的正常 `false`**。这里照抄 `outcome.committed` 而不是
/// 写死 `true`——写死会把"服务将来改形状"的风险藏起来。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoreResult {
    /// 恢复之后的 `data_epoch`（提交路径 = 全新值）：前端据此**重新握手**，旧展示必须丢。
    pub data_epoch: String,
    /// 与 [`RestoreResult::data_epoch`] 同一时刻（锁内同一次读）的权威版本。
    pub revision: i64,
    pub applied: bool,
}

/// 读一次权威身份，并且**只接受请求带来的期望值**（命令 9/10/11 共用）。
///
/// 与 `storage::guards::guard_epoch` **同一判据**（都读同一行 `app_meta` 再比期望值）；
/// 区别只是**命令层不得引用 `storage::`**（`scripts/check-layers.ps1` 第 1 条），所以走
/// 公开的握手入口。判据没有被削弱：比的是**请求带来的**期望值，不是"读出来再跟自己比"。
///
/// 调用方必须在同一个临界区里用它：拿到锁之后读到的身份，才配得上"这次操作的身份"。
fn require_epoch(
    app: &AppState,
    expected_data_epoch: &str,
) -> Result<handshake::RevisionSnapshot, AppError> {
    let authority = handshake::get_revision(app.db()?)?;
    if authority.data_epoch != expected_data_epoch {
        return Err(AppError::DataEpochMismatch);
    }
    Ok(authority)
}

/// 一次导出请求**解析之后**的形状：判别式 + 该形状真正要用的标量。
///
/// 把"判别式"与"标量"绑在一个枚举里，命令体就不必再 `unwrap` 一次可选字段——
/// 校验与取值只有一处（`parse_export_request`），也不会有"校验过了但取错分支"的缝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportRequestShape {
    /// 范围明细（`[from, to)`）。
    Json { from: i64, to: i64 },
    /// 自然周回顾（`anchor` = 这一周里的任一刻；`None` = 本周）。
    Markdown { anchor: Option<i64> },
}

/// 解析导出的 `format` 与它该带的标量（字符串枚举显式解析，见模块头）。
///
/// 不适用却传了的字段**拒绝**而不是忽略（计划第三轮契约收口）。
fn parse_export_request(request: &ExportRequest) -> Result<ExportRequestShape, AppError> {
    let shape = match request.format.trim() {
        "json" => {
            let (Some(from), Some(to)) = (request.from, request.to) else {
                return Err(AppError::Domain {
                    detail: "JSON 导出必须同时给出 from 与 to（半开范围）。".into(),
                });
            };
            if request.anchor.is_some() {
                return Err(AppError::Domain {
                    detail: "JSON 导出不接受 anchor（那是 Markdown 周回顾的参数）。".into(),
                });
            }
            ExportRequestShape::Json { from, to }
        }
        "markdown" => {
            if request.from.is_some() || request.to.is_some() {
                return Err(AppError::Domain {
                    detail: "Markdown 周回顾不接受 from/to：周界由服务按 anchor 算，\
                             不能用任意范围冒充自然周。"
                        .into(),
                });
            }
            ExportRequestShape::Markdown {
                anchor: request.anchor,
            }
        }
        "" => {
            return Err(DomainError::EmptyText {
                field: "导出格式"
            }
            .into())
        }
        other => return Err(unknown_enum_value("导出格式", other)),
    };
    Ok(shape)
}

/// 导出产物的目录名（生产缺省：`<app_data_dir>/exports`，与备份同在一个应用数据目录下）。
const EXPORT_DIR_NAME: &str = "exports";

/// 备份阶段标记：`backup_consistent` 的失败 `detail` 前缀。用户手动备份与"迁移前备份"
/// 分开，日志里一眼看得出是哪一次失败。
const USER_BACKUP_STAGE: &str = "user backup";

/// 导出产物的落盘目录（**只算路径**，不创建）。
///
/// `dir_override` 与 `services::backup::backup_consistent` 的同名参数同一姿势：生产传
/// `None`（走 [`crate::platform::paths::app_data_dir`]），测试注入临时目录——**绝不写
/// 真实的 `%APPDATA%`**（`tests/startup_order.rs` 与 `tests/backup_restore.rs` 都立过
/// 这条规矩，环境变量那条路还要求串行执行、会把并行测试拖成互相干扰）。
pub(super) fn export_dir(dir_override: Option<&Path>) -> Result<PathBuf, AppError> {
    match dir_override {
        Some(dir) => Ok(dir.to_path_buf()),
        None => Ok(crate::platform::paths::app_data_dir()
            .map_err(|error| export_io("resolve the app data directory", error))?
            .join(EXPORT_DIR_NAME)),
    }
}

/// 导出产物的文件名：`worktrace-export-<形状>-<Unix 毫秒>.<扩展名>`。
///
/// 时间戳来自**同一条时钟接缝**（`AppState::now_ms`），与备份产物命名同一口径。名字的
/// 粒度是**毫秒**，所以同一毫秒内的两次同形状导出会撞成同一个名字——**这是"重名"，
/// 不是"同一份产物"**：两次 JSON 导出可以问的是**不同的范围**（内容就不同），
/// 两次周回顾也可以问不同的周。所以撞名时**拒绝覆盖**（见 [`write_export`]），
/// 与 `services::backup::backup_consistent` 的"同名即拒绝、不覆盖"同一口径。
///
/// 取舍（fix round 1，评审 Minor-4）：走 IPC 时每次导出都是一次独立往返，
/// 落进同一毫秒**实际不可达**；而"静默覆盖"的代价是把用户可能正开着的那份文件换成
/// 内容不同的另一份。真要支持批量导出，正确的改法是名字里加序号/UUID，
/// **不是**退回静默覆盖。
fn export_file_name(shape: ExportRequestShape, at_ms: i64) -> String {
    match shape {
        ExportRequestShape::Json { .. } => format!("worktrace-export-json-{at_ms}.json"),
        ExportRequestShape::Markdown { .. } => {
            format!("worktrace-export-weekly-{at_ms}.md")
        }
    }
}

/// 把内容写进导出目录，返回**绝对**路径与写出的字节数。
///
/// 两条不变量（fix round 1 的 Minor-3 / Minor-4）：
///
/// 1. **不留半份产物**：先写同目录的 `<名字>.partial`、写成功后再改名为正式名
///    （同卷改名）。磁盘满 / 写一半失败 ⇒ `exports/` 里**不会**出现一个名字正常、
///    内容截断的文件（用户不会以为它是一份完整导出）；失败时顺手清掉临时文件
///    （best-effort：删不掉也不改变"这次导出失败"这个结论）。
/// 2. **不覆盖已有产物**：同名产物已经存在 ⇒ 拒绝（与 `backup_consistent` 同一口径），
///    而不是把内容换成另一份（见 [`export_file_name`] 的取舍说明）。
fn write_export(dir: &Path, file_name: &str, text: &str) -> Result<(PathBuf, u64), AppError> {
    std::fs::create_dir_all(dir)
        .map_err(|error| export_io("create the export directory", error))?;
    let path = dir.join(file_name);
    if path.exists() {
        return Err(AppError::Storage {
            detail: format!("export: artifact already exists: {}", path.display()),
        });
    }
    let staged = dir.join(format!("{file_name}.partial"));
    let bytes = text.as_bytes();
    // Only clean up a temporary file owned by this attempt.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .map_err(|error| export_io("create the staged artifact", error))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(export_io("write the artifact", error));
    }
    if let Err(error) = std::fs::rename(&staged, &path) {
        let _ = std::fs::remove_file(&staged);
        return Err(export_io("publish the artifact", error));
    }
    Ok((absolute_path(path)?, bytes.len() as u64))
}

/// 落盘失败的统一形状：`code` 是 `STORAGE_ERROR`，`detail` 是**内部诊断**（任务 3 的
/// 「落盘的失败路径」条：目标目录不可写 / 磁盘满 / 路径过长都走这条，用户看到的是
/// `AppError::message()` 那句固定文案，不是这里拼的英文）。
fn export_io(stage: &str, error: std::io::Error) -> AppError {
    AppError::Storage {
        detail: format!("export: {stage}: {error}"),
    }
}

/// 响应里给的是**绝对**路径（`revealItemInDir` 与"复制路径"都要求它）。
///
/// 生产缺省路径本来就是绝对的（`app_data_dir()` 由 `%APPDATA%` / `$XDG_DATA_HOME` 推出）；
/// 这里只为**注入的相对目录**兜底：拼上当前目录，而不是报错。
fn absolute_path(path: PathBuf) -> Result<PathBuf, AppError> {
    if path.is_absolute() {
        return Ok(path);
    }
    let cwd =
        std::env::current_dir().map_err(|error| export_io("resolve an absolute path", error))?;
    Ok(cwd.join(path))
}

/// 路径 → 响应里的字符串。不是合法 UTF-8 就拒绝：界面拿它去开文件管理器，
/// 半个路径比一条明确的错误更糟。
fn path_to_string(path: &Path) -> Result<String, AppError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::Storage {
            detail: format!("path is not valid UTF-8: {}", path.display()),
        })
}

/// [`export_data`] 的命令体（IPC 包装只做转发）。
pub fn export_data_impl(
    app: &mut AppState,
    request: ExportRequest,
    dir_override: Option<&Path>,
) -> Result<ExportResult, AppError> {
    let shape = parse_export_request(&request)?;
    // 文件名里的时间戳：在**生成之前**取一次（失败就不该留下半份产物），
    // 走与业务写同一条时钟接缝（`AppState::now_ms` → 协调器 → `Clock`）。
    let at_ms = app.now_ms()?;
    let (text, data_epoch, revision) = match shape {
        ExportRequestShape::Json { from, to } => {
            let query = stats::StatsRangeQuery {
                from,
                to,
                timezone: request.timezone,
                expected_data_epoch: request.expected_data_epoch,
            };
            let export = app.export_json(&query)?;
            (export.text, export.data_epoch, export.revision)
        }
        ExportRequestShape::Markdown { anchor } => {
            let query = export::WeeklyQuery {
                timezone: request.timezone,
                anchor,
                expected_data_epoch: request.expected_data_epoch,
            };
            let export = app.export_weekly_markdown(&query)?;
            (export.text, export.data_epoch, export.revision)
        }
    };
    let (path, bytes) = write_export(
        &export_dir(dir_override)?,
        &export_file_name(shape, at_ms),
        &text,
    )?;
    Ok(ExportResult {
        path: path_to_string(&path)?,
        bytes,
        data_epoch,
        revision,
    })
}

/// [`backup`] 的命令体（IPC 包装只做转发 + 凑一只同源钟）。
///
/// `dir_override` 与 [`export_data_impl`] 同一姿势：生产 `None`，用例注入临时目录。
pub fn backup_impl(
    app: &mut AppState,
    clock: &(dyn Clock + Send),
    request: BackupRequest,
    dir_override: Option<&Path>,
) -> Result<BackupResult, AppError> {
    // 维护态门禁：命令体**自己**也要挡一次。`run_command` 那一层也挡（取锁之后第一句），
    // 但用例直接调命令体，且"备份在维护态被拒"是计划第 10 行点名的判据。
    app.guard_writable()?;
    // 身份守卫：过期请求**在产出任何东西之前**被拒（不留下半份产物）。
    // 信封是写命令的共同形状，只是 `backup_consistent` 是六参原语、不收它——校验因此
    // 在这里用握手入口做，判据与 `guard_epoch` 同源（见 [`require_epoch`]）。
    let envelope = WriteEnvelope::for_create(request.expected_data_epoch);
    require_epoch(app, &envelope.expected_data_epoch)?;
    let artifact = backup::backup_consistent(
        dir_override,
        app.db()?.connection(),
        backup::current_version(app.db()?.connection())?,
        clock,
        USER_BACKUP_STAGE,
        app.diagnostics(),
    )?;
    // 响应里的身份与版本在**产物落地之后**读回：备份不改业务事实，所以它们与上面那次
    // 守卫读到的相同；这样写是为了让"响应里的值 = 响应时刻库里的值"成为结构性事实，
    // 而不是"因为没人写所以碰巧相等"。
    let authority = require_epoch(app, &envelope.expected_data_epoch)?;
    let bytes = std::fs::metadata(&artifact)
        .map_err(|error| AppError::Storage {
            detail: format!("{USER_BACKUP_STAGE}: stat the artifact: {error}"),
        })?
        .len();
    Ok(BackupResult {
        path: path_to_string(&artifact)?,
        bytes,
        data_epoch: authority.data_epoch,
        revision: authority.revision,
    })
}

/// [`restore`] 的命令体：二次确认 → 身份守卫 → **一次调用体内**走完三段。
///
/// 收 `&SharedApp` / `&Broadcaster` / `&ClockSource`（不是 `&mut AppState`）：三段各自
/// 取锁、中间那段不持锁，命令体自己不持有任何借用——这正是"不进 `run_command`"在类型
/// 上的样子。返回的 `revision` 来自 [`backup::RestoreOutcome::revision`]，即**锁内冻结**
/// 的那个值（见 [`RestoreResult`]）。
pub fn restore_impl(
    app: &SharedApp,
    broadcaster: &Broadcaster,
    clock: &backup::ClockSource,
    request: RestoreRequest,
) -> Result<RestoreResult, AppError> {
    // ⓪ **持锁调用 ⇒ 立刻拒绝**，判据与 `restore_from_backup` 第一句**同源**
    //    （`holds_app_lock`），但位置必须在这里：下面 ② 的身份守卫要 `lock_app`，
    //    而错误接线（把恢复塞回 `run_command` 的闭包）正持着那把非重入 `Mutex`——
    //    没有这一句，现象是**静默死锁**（服务入口那句要到 ① 段才跑得到，太晚）。
    //    有了它，"接线错误"是**红**：明确的 `STORAGE_ERROR`，什么都不碰。
    if holds_app_lock(app) {
        return Err(AppError::Storage {
            detail: "恢复流程必须在锁外开始（命令体自己按段取锁），不要在持有串行边界的线程上调用"
                .to_string(),
        });
    }
    // ① 二次确认：命令层**再校验一次**。拒绝时一个字都不写：不进维护态、不碰文件、
    //    连库都不读（`confirmed: false` 与"文件不存在"是两件事，先答前一件）。
    if !request.confirmed {
        return Err(AppError::Domain {
            detail: "恢复会替换当前数据库，需要明确的二次确认（confirmed 必须为 true）。".into(),
        });
    }
    // ② 身份守卫：在**进维护态之前**（失败时进程状态与磁盘一个字节都没变）。
    //    这一小段自己取锁、随即释放——不能把 guard 带进 ③：`restore_from_backup` 的
    //    第一句就是"持锁调用 ⇒ 拒绝"，带进去会**红**（这正是我们要的行为，不是要绕过的）。
    {
        let state = lock_app(app);
        require_epoch(&state, &request.expected_data_epoch)?;
    }
    // ③ 三段流程必须在**这一次调用体内**：服务入口自己把它们连起来，命令层不得拆成
    //    多次 IPC（P6 验收 §6 第一条）。
    let outcome = backup::restore_from_backup(
        app,
        broadcaster,
        Path::new(&request.backup_path),
        None,
        clock,
    )?;
    // `applied` 照抄 `outcome.committed`：服务把"没换成"当 `Err` 交回（回滚成功也报原始
    // 原因），所以能走到这里就恒为 `true`。
    Ok(RestoreResult {
        data_epoch: outcome.data_epoch,
        revision: outcome.revision,
        applied: outcome.committed,
    })
}
