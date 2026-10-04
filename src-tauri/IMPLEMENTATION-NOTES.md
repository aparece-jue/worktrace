# 实施记录（P1 起）

本文件只记计划明确要求"记录下来"的事实：实测工具链与依赖版本、需要选型的实验结论。
不是设计文档——设计在 `docs/superpowers/specs/`，计划在 `docs/superpowers/plans/`。

---

## 1. 工具链与依赖（P1 Task 1，实测）

计划要求「记录实际 Rust/Cargo 版本，验证依赖可用，**不依据文档中的预设版本推断**」。
以下为 2026-10-03 在 Windows 侧实测（`cargo --version` / `Cargo.lock` / `cargo tree`）：

| 项 | 实测值 | 说明 |
| --- | --- | --- |
| rustc | `1.98.1 (48a229cea 2026-09-01)` | Windows 与 WSL 两套均为 1.98.1 |
| cargo | `1.98.1 (797e8a9bc 2026-08-05)` | **同一次执行只用一套**，交替会让 `target/` 反复全量重编 |
| rusqlite | `0.40.2` | `bundled` 特性，不依赖系统 SQLite |
| libsqlite3-sys | `0.38.2` | 由 rusqlite 带入，编译期构建 SQLite |
| uuid | `1.26.1` | `v4` 特性 |
| thiserror | `2.0.21` | 注意：锁文件里另有 `1.0.69`，那是 **Tauri 的传递依赖**，不是本 crate 用的 |
| tempfile | `3.27.0` | dev-dependency |
| tauri | `2.12.0` | 原有依赖，未动 |

验证方式：`cargo test` 全绿（3 个单测 + 5 个集成测试），首次构建即成功，未出现版本不可用。

---

## 2. 数据库执行边界（P1 Task 2，已完成）

06 §4 要求做「DB 执行边界」实验并选线程方案。实验代码在
`src-tauri/tests/db_execution_boundary.rs`（可重复运行，`cargo test -- --nocapture`）。

**实测结论**（2026-10-03，Windows，rustc 1.98.1）：

| 观察 | 实测值 |
| --- | --- |
| 第二个写者遇到持锁时 | 等待 170–187ms 后成功（`busy_timeout=5s` 生效） |
| 对照组：`busy_timeout=0` | 立刻 `SQLITE_BUSY` 失败 |
| Mutex 边界下调用方线程 | 被挡 250ms（就是持锁者不放的时长） |
| 同工作量（1 次慢写 + 20 次小读） | Mutex 92.4ms vs 工作线程 87.9ms —— **同量级** |

**选定的方案**：`Db` 只持有连接，**本层不加 `Mutex`**，把「放到哪个线程」留给调用方
（P6/P7 的进程接线）。依据：

1. 两种边界**必然串行**——单连接 + SQLite 写锁的固有性质。上表最后一行说明吞吐没有差别，
   所以选择依据不是性能。
2. 真正的差别是**阻塞落在谁的线程上**。Mutex 边界让调用方线程卡在锁上；工作线程边界让
   调用方卡在 channel 上。两者都**不能**在 async 运行时的工作线程或 UI 回调里直接做——
   所以硬约束是「不得把 `Connection` 跨 `await` 持有，也不得在 UI 回调里跑长操作」，
   这条与边界选择无关。
3. `Connection` 是 `Send` 但 `!Sync`。加锁与否是**消费者的拓扑问题**；在库里塞 `Mutex`
   会让 `&mut self` 与事务生命周期纠缠，而 P1 Task 4 要求仓储接受 `&Transaction`、
   事务由服务层自由持有。

**`busy_timeout` 取 5 秒**：最长正常写事务是心跳（单行 upsert），备份走在线备份接口而非
长事务。5 秒足够覆盖一次 checkpoint 的抖动，又不至于让界面卡死。

### 附带发现：分层检查会误报

计划里那段分层检查是朴素 grep，**会把声明该规则的文档注释本身当成违规**——
`src/domain/mod.rs` 的 `//!` 注释里写了「不得引用 rusqlite / platform::」，
于是每次检查都报 LEAK。一个永远失败的检查会被忽略，比没有更糟。

已改为 `src-tauri/scripts/check-layers.ps1`：匹配前先剔除注释行（`//` 与块注释的 `*`），
并做过反向验证——故意往 `domain/mod.rs` 塞一行 `use rusqlite::Connection;`，
检查确实报 LEAK 并以退出码 1 失败；移除后恢复通过。

## 3. 平台实验（P2 起，待做）

06 §4 的「单调/墙钟映射」与「双窗口同步」需要真实机器行为与窗口，分别归 P2 的完成门槛
与 P7。**记录待补**，包括机器/系统版本、事件到达延迟与观察到的行为。

---

## 4. 串行执行边界（D6，P7 Task 0 定稿）

计划 Task 0 要求二选一：**单一 `Mutex<AppState{db, coordinator}>`**，或等价的
**专用工作线程 + channel**。

**选定：单一 `Mutex`。** 落点在 `src/services/bootstrap.rs`：
`AppState { db, coordinator, recovery }` + `AppBoundary { state: Mutex<AppState>, holder }`
+ `SharedApp = Arc<AppBoundary>` + `lock_app()`（返回 `AppGuard`）。
周期采样驱动（`platform/scheduler.rs`）与用户命令取的是**同一把锁**——
「每次触发走与用户命令同一条串行边界」不是口头约定，而是同一个 `Mutex`。

> **2026-10-04 Task 4 fix round 1 订正**：`SharedApp` 早期就是 `Arc<Mutex<AppState>>`，
> 现在多包了一层——`AppBoundary` 额外记着**当前持锁线程 id**（`holder`），
> 用来在「持锁调显式退出」时明确失败而不是与采样线程互锁（评审 I1）。
> 串行语义没变：能拿到 `db`/`coordinator` 的路径仍然只有 `lock_app` 一条。

### 为什么不是工作线程 + channel

| 判据 | Mutex 边界（选定） | 工作线程 + channel |
| --- | --- | --- |
| 吞吐 | 与工作线程**同量级**（§2 实测：92.4ms vs 87.9ms） | 同量级 |
| 阻塞落在谁身上 | 调用方线程卡在锁上，但它本来就跑在 `spawn_blocking` 的阻塞池线程上 | 调用方卡在 channel 上 |
| 代码量 | 一个 `Arc<Mutex<..>>` + 取锁 | 需要一个命令枚举、响应通道、以及「响应也要在库里读」的二次往返 |
| 事务生命周期 | 仓储的 `&Transaction` 天然活在临界区内 | 事务必须在工作线程里开、在同一个闭包里提交，命令语义要整体搬进枚举 |
| 串行证据 | `lock_app` 是**唯一**能拿到 `db`/`coordinator` 的路径（字段私有可达性由类型保证） | 靠「只有一个消费者」这一约定，漏一条 path 就多一条并发 |

决定性的一条是**第 5 行**：Mutex 的串行性由类型系统保证（拿不到锁就拿不到 `Db`），
工作线程方案靠纪律保证（只要有人直接摸到 `Db` 就破功）。P1 §2 已经说明两种边界的
**吞吐没有差别**，所以这里选择的不是性能，而是「哪种边界更难被绕过」。

### 与选型无关的三条硬约束（依据 §2 的实测）

1. **命令一律 `async`**；
2. **不得在 UI 回调里跑长事务**——命令体的阻塞段放在
   `tauri::async_runtime::spawn_blocking` 里（Tauri 2.12.0 有该入口，见
   `tauri-2.12.0/src/async_runtime.rs:311`），绝不在 `invoke_handler` 的调用线程上
   直接跑 SQLite；
3. **`Connection` 不得跨 `await` 持有**——锁与事务都活在那个阻塞闭包内，
   闭包结束即释放。`std::sync::MutexGuard` 不是 `Send`，这条在类型上也绕不过去。

**代价（诚实记下）**：临界区从「一次业务写」扩大到「一次命令的整个阻塞段」，
慢命令会让采样与其它命令排队；`busy_timeout` 仍是 5 秒上限。若 P6 的备份/维护态
需要「长事务期间允许查询」，那时再评估读写分离，而不是现在先加一层通道。

## 5. 单实例与周期采样（P7 Task 0）

- **单实例锁用 `std::fs::File::try_lock`**（Rust 1.89 起稳定，本机 1.98.1 实测可用；
  Windows 走 `LockFileEx`、类 Unix 走 `flock`）。因此**没有新增依赖**：不加
  `windows-sys`/`libc`，也不用 `tauri-plugin-single-instance`/`fs2`/`fd-lock`/`interprocess`
  （后四个在本机两侧缓存 0 命中）。锁由**内核**在进程被杀时释放，
  不需要「清理陈旧 PID 文件」那套启发式（`tests/startup_order.rs` 用子进程 + 强杀实测过）。
- **「唤起既有主窗」是通知，与锁分离**：拿锁失败的进程写一次同目录的
  `instance.notify`（`platform/single_instance.rs`）。**交付边界**（2026-10-04 订正）：
  Task 0 只交付**发送侧**（`request_activation`）与**接收原语**
  （`take_activation_request`）——把请求变成「抬起主窗」的消费侧需要窗口对象，归
  **Task 4**，所以 `take_activation_request` 现在还没有生产调用者：这是分割点，
  不是遗漏（与计划 Task 0 第 2 条同一口径）。通知失败**不改变**「退出」这个决定。
- **周期采样驱动是一个不挂在任何窗口上的线程**（`platform/scheduler.rs`，F-009）：
  窗口对象根本传不进它的签名。空闲（无活动会话）时它只读不写——由
  `tests/periodic_sampling.rs` 用三件事钉住：**App 自己那条连接上的**
  `SELECT total_changes()`、全表行数、`revision`。
  ⚠️ `total_changes()` 是**连接级**计数：在测试里新开一条连接取它恒为 0，等于没有断言
  （本轮评审抓到的注水；修法是 `Rig::app_total_changes()`——在锁内、在 App 的连接上取，
  并做过反向验证：临时让空闲路径写一行，该断言确实变红）。

