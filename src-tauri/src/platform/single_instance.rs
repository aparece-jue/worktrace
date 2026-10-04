//! 单实例（P7 Task 0）。**平台叶子**：只做 OS 适配，不含业务规则。
//!
//! ## 锁是 OS 级的
//!
//! 用 `std::fs::File::try_lock`（Rust 1.89 起稳定，本仓工具链 1.98.1 实测可用）：
//! Windows 走 `LockFileEx`，类 Unix 走 `flock`。两者都由**内核**在文件句柄关闭时
//! 释放——进程被强杀（TerminateProcess / SIGKILL）时句柄由内核关闭，锁照样释放。
//! 这正是单实例需要的语义：不需要「清理陈旧 PID 文件」那套启发式。
//!
//! 因此**零新增依赖**：不加 `windows-sys`、`libc`，也不用
//! `tauri-plugin-single-instance` / `fs2` / `fd-lock` / `interprocess`（两侧缓存 0 命中）。
//!
//! 锁文件用既有的 [`crate::platform::paths::instance_lock_file()`]（与库同目录）：
//! 锁必须覆盖「打开库」这一步，放别处可能在锁生效前就建了库。
//!
//! ## 「唤起既有主窗」是**通知**，与锁分离
//!
//! 拿锁失败只说明「已经有实例」。要让既有实例把主窗拉到前台，是**另一件事**：
//! 这里给出一条与锁无关的通道（同目录的 [`activation_request_file()`]），
//! 发送方（拿锁失败的进程）写一次请求，既有实例消费它。
//! **通知失败不影响退出决定**——拿不到锁的进程无论如何都要退出，不能因为
//! 「叫不醒前面那个」就变成第二个实例。
//!
//! 消费方（把请求变成「抬起主窗」）需要窗口对象，属 Task 4 的窗口接线；
//! 本模块只提供通道两端的原语。故障路径硬化（锁异常释放、通知丢失）归 P6 Task 1。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 唤醒请求文件的名字。与锁同目录。
const ACTIVATION_FILE: &str = "instance.notify";

/// 持有中的单实例锁。**Drop 即释放**；进程被杀时由内核释放。
#[derive(Debug)]
pub struct InstanceLock {
    file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// 尝试获取锁。
    ///
    /// - `Ok(Some(lock))`：本进程成为唯一实例，持有到 `lock` 被丢弃；
    /// - `Ok(None)`：**已有实例持有**——调用方必须走「通知既有实例后退出」，
    ///   **不得**继续打开库（F-016）。
    ///
    /// 锁文件所在目录不存在时会先建出来（正常路径上它由上一次运行建好）：
    /// 拿不到锁的进程也会走到这一步，但**建目录不是打开库**，仍在允许范围内。
    /// 锁文件本身**不删除**——删除锁文件是有竞态的（P6 Task 1 硬化时再看）。
    pub fn acquire(path: impl AsRef<Path>) -> std::io::Result<Option<Self>> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;

        match file.try_lock() {
            Ok(()) => {
                // 诊断用：写出持有者 PID，出问题时能一眼看出是谁拿着锁。
                // 写失败不影响持锁——锁已经拿到了。
                let pid = std::process::id();
                let _ = file.set_len(0);
                let _ = writeln!(file, "{pid}");
                let _ = file.flush();
                Ok(Some(Self { file, path }))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// 锁文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 显式释放（等价于 Drop，供需要在退出前主动让位的调用方使用）。
    pub fn release(self) {
        let _ = self.file.unlock();
    }
}

/// 唤醒请求文件：与锁同目录，**与锁分离**。
pub fn activation_request_file(lock_path: &Path) -> PathBuf {
    lock_path
        .parent()
        .map(|p| p.join(ACTIVATION_FILE))
        .unwrap_or_else(|| PathBuf::from(ACTIVATION_FILE))
}

/// 发送方：告诉既有实例「有人想再开一个，把主窗抬起来」。
///
/// 写入 PID 与当前挂钟毫秒，纯粹为诊断。**返回 `Err` 不改变调用方的决定**：
/// 通知失败也必须退出（见模块头「通知失败不影响退出决定」）。
///
/// 时间在这里直接取 `SystemTime` 是**平台层**的本分：分层规则禁止的是
/// `services` 自取时间，因为服务层的时间必须来自可注入的 `Clock`。
pub fn request_activation(lock_path: &Path) -> std::io::Result<PathBuf> {
    let path = activation_request_file(lock_path);
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;
    writeln!(file, "{} {at_ms}", std::process::id())?;
    file.flush()?;
    Ok(path)
}

/// 接收方：消费一次唤醒请求。返回「这次是否真的有请求」。
///
/// 消费语义是**取走**：读到就删掉，所以同一请求不会被反复处理。
/// 调用方（Task 4 的窗口接线）把它接到「抬起主窗」上。
pub fn take_activation_request(lock_path: &Path) -> std::io::Result<bool> {
    let path = activation_request_file(lock_path);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_acquire_of_the_same_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance.lock");

        let first = InstanceLock::acquire(&path).unwrap().expect("首次应拿到锁");
        assert!(
            InstanceLock::acquire(&path).unwrap().is_none(),
            "同一进程的第二次获取也必须被拒（锁在文件上，不在进程上）"
        );

        drop(first);
        assert!(
            InstanceLock::acquire(&path).unwrap().is_some(),
            "释放后应能重新获取"
        );
    }

    #[test]
    fn activation_request_is_taken_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("instance.lock");

        assert!(
            !take_activation_request(&lock).unwrap(),
            "没有请求时返回 false"
        );
        let written = request_activation(&lock).unwrap();
        assert_eq!(written, activation_request_file(&lock));
        assert!(take_activation_request(&lock).unwrap(), "第一次应取到");
        assert!(!take_activation_request(&lock).unwrap(), "取走后不应再有");
    }

    #[test]
    fn the_lock_file_sits_next_to_the_activation_request() {
        let lock = Path::new("/tmp/app/instance.lock");
        assert_eq!(
            activation_request_file(lock),
            Path::new("/tmp/app/instance.notify")
        );
    }
}
