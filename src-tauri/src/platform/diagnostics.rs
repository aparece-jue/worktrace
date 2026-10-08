//! 正式诊断日志的落点（P6 Task 2a）。**平台叶子**：只做「把一行追加到某个文件」，
//! 不含任何业务规则——记什么、什么时候记，全由 `services` 那侧决定。
//!
//! ## 为什么必须有这个落点
//!
//! release 的 Windows 子系统**没有控制台**（`tauri.conf.json` 的
//! `windows_subsystem`）：`println!`/`eprintln!` 写出去的东西没人看得见。而 P6 有两类
//! 「用户看得见、控制台看不见」的事实只能靠它留下来：
//!
//! - **维护态的进入/退出**（本任务）——恢复期间写入被拒的原因、维护窗口有多长；
//! - **协调器故障态的进入/清除**（Task 2b）——故障是"内存态"，库里没有痕迹。
//!
//! 所以接口是通用的 `record(event, detail)`：**不是**只服务维护态的一次性函数。
//!
//! ## 三条口径
//!
//! - **一次记录一次追加**（打开 → 写 → 关）：记录只发生在**状态跃迁**上（不是每拍、
//!   不是每次命令），所以不需要长期持有的句柄，也**不引入第二把锁**——本模块不参与
//!   任何串行决策，`Diagnostics` 只是一个路径。
//! - **绝不打断业务**：写失败降级成 `eprintln!`（开发期仍可见），**不返回错误、不 panic**。
//!   诊断坏掉不该让一次状态跃迁失败。
//! - **行格式稳定**：`event=<名字> <detail>`（`detail` 由调用方按 `k=v` 拼、可为空）。
//!   测试与事后排查都按这个形状读。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 一条一行地追加诊断记录。`path = None` 表示**关闭**（`record` 是空操作）。
///
/// 关闭是显式路径构造的缺省（见 `services::bootstrap::StartupConfig::new`）：
/// 测试与打包实验不该因为没注入就意外在真实数据目录里留文件。
#[derive(Debug, Clone)]
pub struct Diagnostics {
    path: Option<PathBuf>,
}

impl Diagnostics {
    /// 关闭落盘。
    pub fn disabled() -> Self {
        Self { path: None }
    }

    /// 追加写到指定文件。父目录在**第一次真的写入时**才按需创建。
    pub fn to_file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
        }
    }

    /// 按可选路径构造：`None` = 关闭（与 [`Diagnostics::disabled`] 同一语义）。
    ///
    /// 用途：`StartupConfig.diagnostic_log` 是 `Option<PathBuf>`，而**启动探针**（组合根那条
    /// `StartupTrace`）与**状态跃迁**（维护态、故障态）必须写进**同一个文件**。把这段
    /// `match` 收在这里，两处就不会各写一份、日后漂移。
    pub fn from_optional_path(path: Option<PathBuf>) -> Self {
        match path {
            Some(path) => Self::to_file(path),
            None => Self::disabled(),
        }
    }

    /// 是否配置了落点。
    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    /// 落点路径（诊断与测试读回用）。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 记一条。`event` 是稳定的事件名（如 `maintenance.begin`），`detail` 是 `k=v` 串。
    ///
    /// **失败只降级**：写不进去时打一行 `eprintln!` 就结束，绝不返回错误（见模块头）。
    pub fn record(&self, event: &str, detail: &str) {
        let Some(path) = &self.path else {
            return;
        };
        let line = if detail.is_empty() {
            format!("event={event}\n")
        } else {
            format!("event={event} {detail}\n")
        };
        if let Err(error) = append(path, &line) {
            eprintln!(
                "[worktrace] diagnostics: 写不进 {}：{error}",
                path.display()
            );
        }
    }
}

/// 追加一行。父目录不存在时创建（`app_data_dir()` 自己**不建目录**，见 `paths`）。
fn append(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 关闭时一次系统调用都不该发生（路径都没有，无处可写）。
    #[test]
    fn a_disabled_sink_records_nothing() {
        let sink = Diagnostics::disabled();
        assert!(!sink.is_enabled());
        assert!(sink.path().is_none());
        sink.record("maintenance.begin", "phase=Restore");
    }

    /// `Option<PathBuf>` 的两条分支：`Some` = 落盘、`None` = 关闭（**不是**写到某个缺省）。
    #[test]
    fn an_optional_path_maps_to_the_same_two_shapes() {
        assert!(!Diagnostics::from_optional_path(None).is_enabled());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worktrace.log");
        let sink = Diagnostics::from_optional_path(Some(path.clone()));
        assert!(sink.is_enabled());
        assert_eq!(sink.path(), Some(path.as_path()));
        sink.record("startup.step", "step=single_instance_checked");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "event=startup.step step=single_instance_checked\n"
        );
    }

    /// 多次 `record` 是**追加**（不是覆盖），父目录按需创建。
    #[test]
    fn records_append_across_calls_and_create_the_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("diagnostics.log");
        let sink = Diagnostics::to_file(&path);

        sink.record("maintenance.begin", "phase=Restore entered_at_ms=1000");
        sink.record("maintenance.end", "duration_ms=500");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "event=maintenance.begin phase=Restore entered_at_ms=1000\n\
             event=maintenance.end duration_ms=500\n"
        );
    }
}
