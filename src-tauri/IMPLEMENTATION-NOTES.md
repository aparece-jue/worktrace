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
