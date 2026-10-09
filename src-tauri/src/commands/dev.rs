//! P7 Task 6a 的 dev 注入开关（**只在 debug 构建存在**）与实验窗口的开窗入口。
//!
//! 真实双窗口实验（`tests/manual-sync.md`）要确定性地造出三种竞态，靠的就是这里的
//! 四条 dev 命令。它们**不是产品命令**：不进错误契约、不接业务写路径、不改事务行为，
//! 只影响**前端侧的事件投递与响应时序**。
//!
//! | 命令 | 语义（`manual-sync.md` §1） | 落点 |
//! | --- | --- | --- |
//! | `__p7_drop_next_event(kind)` | 丢掉下一条**指定事件名**的通知 | [`crate::services::events::Broadcaster::arm_drop_next`] |
//! | `__p7_delay_next_query_ms(command, ms)` | 下一次指定命令的**响应**延迟返回 | [`delay_response_if_armed`]（由命令包装在放锁之后调） |
//! | `__p7_replay_event(revision)` | 用一条**旧 `revision`** 重播 `domain.changed` | [`replay_event_impl`] |
//! | `__p7_open_sync_lab()` | 开实验窗口 `sync-lab` | [`crate::platform::sync_lab::open_sync_lab`] |
//!
//! ## 怎么保证「只在 debug 构建」
//!
//! **两道编译期守卫，缺一不可**（发布构建里不是「关掉」，是**没有**）：
//!
//! 1. 本模块的声明带 `#[cfg(debug_assertions)]`（`commands/mod.rs`）⇒ 发布构建里
//!    命令体、延迟开关、这个文件的一切都不参与编译；
//! 2. `lib.rs` 的 `invoke_handler` 里，这四条**逐条**带 `#[cfg(debug_assertions)]`
//!    ⇒ 发布构建的 handler 列表里没有它们（`tauri::generate_handler!` 把每条命令前的
//!    属性原样交给生成的 match 臂，见 tauri-macros 的 `command/handler.rs`）。
//!
//! 两道守卫由 `tests/dev_injections.rs` 读源码逐条核对：dev 命令名只允许出现在
//! `src/lib.rs`（受守卫的注册臂）与 `src/commands/dev.rs` 两个文件里。
//!
//! ## 从 DevTools 控制台怎么调
//!
//! 页面的 `window` 上没有 `__TAURI__`（`tauri.conf.json` 没开 `withGlobalTauri`），
//! 但 Tauri 注入的 `__TAURI_INTERNALS__` 一直都在：
//!
//! ```text
//! await __TAURI_INTERNALS__.invoke("__p7_drop_next_event", { kind: "domain.changed" })
//! await __TAURI_INTERNALS__.invoke("__p7_delay_next_query_ms", { command: "list_tasks", ms: 4000 })
//! await __TAURI_INTERNALS__.invoke("__p7_replay_event", { revision: 12 })
//! await __TAURI_INTERNALS__.invoke("__p7_open_sync_lab")
//! ```
//!
//! 四条都是 `async` 命令（Windows 上建窗必须在异步命令里做，否则 WebView2 死锁），
//! 返回值就是「这次装上了什么」，控制台当场可见。
//!
//! ⚠️ **(b) 按「调用方窗口 + 命令名」匹配**：在哪个窗口装开关，就延迟哪个窗口的
//! 下一次响应。§2.2 在 B 上装，被延迟的就是 B 的重拉——而不是先跑到的那次 A 的重拉。
//!
//! ## 本模块不做
//!
//! 不新增业务逻辑、不碰事务：`__p7_replay_event` 只**读**一次库身份再广播一条旧信封；
//! 另外两条改的是内存里的开关；开窗那条连库都不碰。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, State};

use crate::error::{AppError, ErrorResponse};
use crate::services::bootstrap::{AppState, RunningApp};
use crate::services::events::{Broadcaster, EventEnvelope};
use crate::services::handshake;

// ─────────────────────────────────────────────────────────────────────────────
// 注入 (b)：下一次指定命令的响应延迟返回
// ─────────────────────────────────────────────────────────────────────────────

/// 延迟开关的键：**哪个窗口** + **哪条命令**。
struct DelayKey {
    window: String,
    command: String,
}

/// 延迟开关：`(键, 毫秒)`。**一次一发**（命中即清空）。
///
/// 键里带窗口是必须的（`manual-sync.md` §2.2）：一次 `domain.changed` 之后，
/// **每个**开着任务页的窗口都会重拉 `list_tasks`——A 还会因为 `afterWrite()` 当场再拉一次。
/// 只按命令名匹配的话，装在 B 上的开关会被 A 的重拉先吃掉，B 那次根本没被延迟，
/// 实验看起来正好是「注入没生效」。所以：**在哪个窗口装的，就延迟哪个窗口的下一次**。
static DELAY: Mutex<Option<(DelayKey, u64)>> = Mutex::new(None);

/// 装开关：让 `window` 里**下一次 `command`** 的响应延迟 `ms` 毫秒返回。
///
/// `command` 是 IPC 命令名（例如 `"list_tasks"`），与 `commands/mod.rs` 里
/// `run_command("list_tasks", …)` 的第一个参数逐字相同——名字对不上就是「装了没反应」。
/// 重复装 = 后一次覆盖前一次（语义始终是「下一次」）。
pub fn arm_delay(window: &str, command: &str, ms: u64) {
    *lock_delay() = Some((
        DelayKey {
            window: window.to_string(),
            command: command.to_string(),
        },
        ms,
    ));
}

/// 消费式取走：窗口与命令名**都**相同才命中，命中即清空。
fn take_delay(window: &str, command: &str) -> Option<u64> {
    let mut pending = lock_delay();
    match pending.as_ref() {
        Some((key, ms)) if key.window == window && key.command == command => {
            let ms = *ms;
            *pending = None;
            Some(ms)
        }
        _ => None,
    }
}

/// 命令包装在**取完数据、放开锁之后**调它：把**响应**推迟，不推迟读。
///
/// 位置就是这条开关的全部要点（`manual-sync.md` §1）：命令体已经在
/// `spawn_blocking` 里跑完（那里持有 `AppState` 的锁与 `Connection`），这一句在
/// `.await` 回来之后才执行——于是「先取数据再 sleep」成立，**没有任何 `Connection`
/// 跨 `await` 被持有**，也没有把串行边界拉长（别的命令、别的窗口都不会被这次延迟挡住）。
///
/// 睡觉走 `spawn_blocking`：这是阻塞线程上的事，不该占住异步运行时的 worker。
pub async fn delay_response_if_armed(window: &str, command: &str) {
    let Some(ms) = take_delay(window, command) else {
        return;
    };
    let _ = tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(ms));
    })
    .await;
}

fn lock_delay() -> std::sync::MutexGuard<'static, Option<(DelayKey, u64)>> {
    // 中毒不该让注入开关变成 panic：这本来就是实验器材。
    DELAY.lock().unwrap_or_else(|error| error.into_inner())
}

// ─────────────────────────────────────────────────────────────────────────────
// 注入 (a)：丢掉下一条指定事件名的通知
// ─────────────────────────────────────────────────────────────────────────────

/// 丢掉下一条**指定事件名**的通知。返回装上的事件名（控制台当场可见）。
///
/// 判定与消费在 [`Broadcaster::arm_drop_next`] / `take_drop`：事件名不同**不吃掉**
/// 开关——这正是它带 `kind` 参数的理由（有活动会话时每秒一条 `timer.tick`）。
#[tauri::command]
pub async fn __p7_drop_next_event(
    state: State<'_, RunningApp>,
    kind: String,
) -> Result<String, String> {
    let broadcaster = Arc::clone(state.broadcaster());
    broadcaster.arm_drop_next(&kind);
    println!("[worktrace] dev: 已装开关——下一条 `{kind}` 通知将被丢弃");
    Ok(kind)
}

// ─────────────────────────────────────────────────────────────────────────────
// 注入 (b) 的命令面
// ─────────────────────────────────────────────────────────────────────────────

/// 让**调用方窗口**里下一次指定命令的响应延迟 `ms` 毫秒返回。返回命令名。
#[tauri::command]
pub async fn __p7_delay_next_query_ms(
    window: tauri::WebviewWindow,
    command: String,
    ms: u64,
) -> Result<String, String> {
    let label = window.label().to_string();
    arm_delay(&label, &command, ms);
    println!("[worktrace] dev: 已装开关——`{label}` 窗口下一次 `{command}` 的响应晚 {ms} ms 返回");
    Ok(command)
}

// ─────────────────────────────────────────────────────────────────────────────
// 注入 (c)：用旧 revision 重播一条 domain.changed
// ─────────────────────────────────────────────────────────────────────────────

/// 用一条**旧 `revision`** 重播 `domain.changed`，返回广播出去的那条信封。
///
/// 参数是 `revision` 而**不是** `event_seq`：信封里没有 `event_seq`
/// （`manual-sync.md` §1 的订正、`EventEnvelope` 的五个字段）。
/// 旧版本的信封会被 [`Broadcaster::emit`] 记一次 `out_of_order` 诊断，但**照常投递**
/// ——这正是实验要的：客户端的闸门②/③据此丢弃它，展示不变。
///
/// **只读 + 广播**：读一次库身份（`data_epoch`）就够，不写库、不动 revision、
/// 不开写事务（`tests/dev_injections.rs` 断言这三点）。
#[tauri::command]
pub async fn __p7_replay_event(
    window: tauri::WebviewWindow,
    state: State<'_, RunningApp>,
    revision: i64,
) -> Result<EventEnvelope, ErrorResponse> {
    let broadcaster = Arc::clone(state.broadcaster());
    super::run_command(
        CMD_REPLAY_EVENT,
        window.label(),
        &state,
        Vec::new(),
        move |app| replay_event_impl(app, &broadcaster, revision),
    )
    .await
}

/// 命令名常量：命令包装按它把 dev 延迟开关（注入 (b)）对上号。
const CMD_REPLAY_EVENT: &str = "__p7_replay_event";

/// [`__p7_replay_event`] 的命令体（IPC 包装只做转发）。
pub fn replay_event_impl(
    app: &mut AppState,
    broadcaster: &Broadcaster,
    revision: i64,
) -> Result<EventEnvelope, AppError> {
    let identity = handshake::get_revision(app.db()?)?;
    let envelope = EventEnvelope::domain_changed(
        identity.data_epoch,
        revision,
        app.now_ms()?,
        // 载荷只是标记：`domain.changed` 的载荷不参与客户端的镜像合并
        // （事件只作缓存失效），实验要的是「一条旧版本的**通知**」本身。
        serde_json::json!({ "replayed_revision": revision }),
    );
    broadcaster.emit(envelope.clone());
    Ok(envelope)
}

// ─────────────────────────────────────────────────────────────────────────────
// 实验窗口：开 `sync-lab`
// ─────────────────────────────────────────────────────────────────────────────

/// 开实验窗口 `sync-lab`（已经开着就抬起来），返回窗口 label。
///
/// 为什么是命令而不是「启动时按环境变量开」：`manual-shell.md` 的既有验收步骤
/// （单窗口、托盘、关窗后继续计时）不该被实验器材影响，所以窗口**只在需要时**由
/// 人显式开；命令还能在实验中途重开被关掉的窗口，环境变量做不到。
///
/// 窗口本身是 `platform::sync_lab` 建的（配置与主窗同源、只改 label）。
#[tauri::command]
pub async fn __p7_open_sync_lab(app: AppHandle) -> Result<String, String> {
    crate::platform::sync_lab::open_sync_lab(&app)
        .map(|window| window.label().to_string())
        .map_err(|error| error.to_string())
}

/// Debug-only observation of the real command wrapper. No request payloads or SQL.
pub struct CommandProbe {
    id: u64,
    command: &'static str,
    window: String,
    started: std::time::Instant,
}

impl CommandProbe {
    pub fn start(command: &'static str, window: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let probe = Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            command,
            window: window.to_string(),
            started: std::time::Instant::now(),
        };
        probe.record("start", None);
        probe
    }

    pub fn record(&self, phase: &str, success: Option<bool>) {
        let wall_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        println!(
            "[worktrace] probe: {}",
            serde_json::json!({
                "id": self.id, "window": self.window, "command": self.command,
                "phase": phase, "wall_ms": wall_ms,
                "elapsed_ms": self.started.elapsed().as_millis(), "success": success,
            })
        );
    }
}
