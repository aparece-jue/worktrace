//! 应用数据目录。**平台叶子**：只做 OS 适配，不含业务规则。
//!
//! 不依赖 Tauri 的路径 API：迁移脚本、备份校验与测试都需要在没有运行时的
//! 情况下算路径。用环境变量推导，纯 std。

use std::path::PathBuf;

/// 应用标识。目录名与 Tauri 的 `identifier` 保持一致。
pub const APP_ID: &str = "com.worktrace.desktop";

/// 应用数据目录：Windows 用 `%APPDATA%`，类 Unix 用 `$XDG_DATA_HOME` 或 `~/.local/share`。
///
/// 不做创建——调用方按需 `create_dir_all`，这样只读场景（诊断、备份校验）
/// 不会意外建目录。
pub fn app_data_dir() -> Result<PathBuf, std::io::Error> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };

    base.map(|b| b.join(APP_ID)).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot determine the app data directory: APPDATA / XDG_DATA_HOME / HOME are all unset",
        )
    })
}

/// 数据库文件路径。
pub fn database_file() -> Result<PathBuf, std::io::Error> {
    Ok(app_data_dir()?.join("worktrace.db"))
}

/// 单实例锁文件路径（P6 用）。
///
/// 与数据库同目录：锁必须覆盖「打开库」这一步，放别处可能在锁生效前就建了库。
pub fn instance_lock_file() -> Result<PathBuf, std::io::Error> {
    Ok(app_data_dir()?.join("instance.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_is_under_the_app_id() {
        let dir = app_data_dir().expect("应有 APPDATA / XDG_DATA_HOME / HOME");
        assert!(dir.ends_with(APP_ID), "{dir:?} 应以 {APP_ID} 结尾");
    }

    #[test]
    fn db_and_lock_live_in_the_same_directory() {
        let db = database_file().unwrap();
        let lock = instance_lock_file().unwrap();
        assert_eq!(db.parent(), lock.parent(), "锁与库必须同目录");
    }
}
