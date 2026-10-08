# P6 · 平台硬化：备份恢复、维护态隔离与故障路径 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 P7 已经跑起来的进程级接线**从「能跑」补到「能交付」**：故障路径、维护态隔离，以及可回滚的 WAL 一致备份与恢复（生成全新 `data_epoch`）。

**与 P7 的分工**：单实例、最小 bootstrap、`application_run`、关窗仍运行的周期采样驱动、事件信封与去重协议、显式退出**都已由 P7 建立并有测试**（P7 排在执行顺序第 4，本计划第 7）。本计划**沿用同一批文件与入口**补齐硬化，**不重新实现第二套驱动或启动流程**。

**Architecture:** 扩展 P7 建立的 `platform/single_instance.rs`、`platform/scheduler.rs`、`services/bootstrap.rs`、`services/events.rs`，新增 `services/backup.rs`。前两者是 `platform/` 的叶子（只做 OS 适配，不含业务规则）；`services/` 的两个拥有事务与协议判断。**沿用 P7 建立的启动顺序并接入正式恢复扫描**，P3 的恢复扫描与 P4/P2 的服务都由它按序调用。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· Tauri 2 的窗口/事件 API（仅用于唤起主窗与广播）· 无新依赖（单实例用文件锁而非额外 crate）

**Spec:**
- `../specs/2026-10-02-worktrace-architecture/00-architecture.zh.md` §4（IPC 与错误）、§5（数据代次、revision 与计时协议）
- `.../02-data-model.zh.md` §4（崩溃恢复扫描时机）、§9（恢复实现）
- `.../08-implementation-contracts.zh.md` §1（约每 30 秒的检查点）
- `.../01-module-breakdown.zh.md` §2（"M00 的单实例在持久化初始化前生效"）
- `.../04-functional-spec.zh.md` F-009、F-014、F-016、F-019、F-020

**依赖的前置计划：**
- **P1**：`storage::{db, migrations, meta}`、`platform::paths`、`error::AppError`
- **P7**：单实例、bootstrap/run、周期采样、广播与显式退出入口及基础测试
- **P3**：恢复扫描入口（本计划只负责**按序调用它**，不重实现四类判定）

**边界（不要越界）：**
- 四类判定与 `reconcile` 归 **P3**；本计划只决定"什么时候调它"。
- 计时采样与异常分割归 **P2**；P7 建立驱动，本计划硬化故障和维护态路径。
- 界面、托盘、窗口生命周期归 **P7**；本计划扩展 P7 既有启动与广播原语，不倒置依赖。
- 备份的**自动化调度**属 V1.0（F-501+）；本计划做的是可手动触发的、可回滚的一次备份/恢复。

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。

---

## 开工前修订记录（2026-10-04）

> **开工前必读。** 下面每条都是「照原措辞写会写出编译不过或语义错的代码」或「引用与今天的树不符」。
> 所有行号与符号都是 2026-10-04 在 `worktrace-src/`（= 仓库 `src-tauri/`）里 `grep` 实测的。
> 只改措辞与引用，**不改 P6 的交付边界**；每条改动在正文对应位置都留了「（2026-10-04 修订：因 …）」。

| # | 级别 | 原措辞（位置） | 新措辞 / 落地口径 | 依据（实测 file:line） | 为什么必须改 |
| --- | --- | --- | --- | --- | --- |
| **C1** | **Critical** | 「维护态期间**必须暂停采样**」（Task 2 首条） | **不改 `Scheduler`**：维护态标志进 `AppState`，采样每拍**取锁之后**判 `sampling_allowed()`，维护态**整拍跳过**（不写 checkpoint、不 tick、不广播）。真实接缝的名字/签名/可见性/归属见 Task 2「维护态与采样：真实接缝」 | `platform/scheduler.rs` 只有 `spawn:71` / `ticks:112` / `stop:126` / `Drop:147`（**无 pause/resume**，`stop` 不可逆）；`RunningApp.sampling` 是私有字段 `services/bootstrap.rs:197`，外部只有只读访问器 `:230`/`:235`，且只拿得到 `&RunningApp` | 照原文实现要么给 `Scheduler` 加一个不存在的 pause，要么去动一个私有字段；而"pause 并等当前拍结束"会与同一把锁互锁（采样拍要 `lock_app`） |
| **C2** | **Critical** | 「取消尚未执行的采样并给**旧队列**标记 epoch/运行代次」（Task 4） | **没有队列**。D6 是单一 `Mutex<AppState>`：命令与采样走同一把锁；"等待中的采样"就是**堵在 `lock_app` 上的那一拍**，它**取锁之后**按维护态丢弃该拍 | `services/bootstrap.rs:341`（`AppState`）、`:361`（`AppBoundary`）、`:399`（`lock_app`）、`:749`（`sampling_action`）；`commands/mod.rs:127`（`run_command`）→ `:140`（`lock_app`）；`grep -rn "VecDeque\|mpsc\|queue" src/` = **0 命中** | "旧队列"这个对象全仓不存在；照原文写会去造一个不存在的结构 |
| **M1** | Minor | 「启动顺序固定为…**六步**」（Task 1） | 6 步**顺序不变**；但注入探针今天有 **9 条**记录，断言按 `StartupStep::ALL` 写 | `services/bootstrap.rs:83`（`pub enum StartupStep`）、`:105`（`pub const ALL: [StartupStep; 9]`）：②拆成 `DatabaseOpened`/`Migrated`、⑤拆成 `CoordinatorStarted`/`SamplingStarted`、①拆成 `SingleInstanceChecked`/`ExistingInstanceNotified` | 写"六步"会让断言漏掉三条真实副作用 |
| **M2** | Minor | 「P8 在 Tauri 的 setup 里**只调它一个**」（下游接口） | `setup` 里只有一个**启动入口**（`bootstrap::startup`），但启动成功后还挂托管、托盘与唤醒接收——**P6 的新编排只能加在 `startup` 内部**，不得在 `lib.rs` 里另排顺序 | `src/lib.rs:192`（`fn setup`）、`:214`（调 `bootstrap::startup`）、`:224`（`app.manage`）、`:228`（`tray::build`）、`:231`（`window::spawn_activation_watcher`） | "只调它一个"过窄，会被读成"lib.rs 里没有别的接线"，从而把编排放错层 |
| **M3** | Minor | 「执行 P1 的**分层检查脚本**」（Task 5） | 该脚本今天有**六条**规则，新增模块六条都要过（`platform/` 与 `services/` 两条最相关） | `src-tauri/scripts/check-layers.ps1:156`（commands）、`:157`（domain）、`:158`（storage）、`:167`（services：`std::time\|SystemTime\|Instant::now\|\bcommands\b`）、`:171`（platform：`\b(services\|storage\|commands)\b`）、`:178`（入口点：`Db::open\|migrate(\|run_repo::`） | 只写"分层检查"会让人按旧的三条自查，新模块（`platform/system_events.rs`、`services/backup.rs`）最容易踩的就是后三条 |
| **M4** | Minor | 依赖清单只点名了少数入口 | 补全**未点名但必用**的既有入口，见文末「依赖清单」一节（逐条 file:line） | 见该节 | 少写一个入口，实施者就会新造一个（`AppBoundary`/`AppGuard`/`holds_app_lock`、`ExitReport`、托盘与实验窗口接线最容易漏） |
| **M5** | Minor | Task 4 直接使用 `DATA_RESTORE_IN_PROGRESS` | 这个码**今天不存在**：本计划要**新增**它，并把四处联动点写进文末「产出接口」 | `src/error.rs:11`（`enum AppError` 五个变体）、`:39`（`code()`）、`:109`（码表用例）、`:161`（中文用例）；`worktrace-web/src/types/ipc.ts:87`（`ERROR_CODES` 五个）、`:94`（`ErrorCode`）；`src/commands/mod.rs:20`（"五个码"注释） | 引用一个不存在的码就是新的漂移源；新增码要四处同步，漏一处前端就分不出维护态 |
| **M6** | Minor | （原计划未登记）采样线程 **panic 会静默死亡** | 登记并纳入 Task 2 的故障路径：`on_tick` panic 时线程直接展开退出，`ticks` 停涨而 `sampling_errors` **不计** panic | `platform/scheduler.rs:96`/`:97`（`on_tick(); ticks.fetch_add(…)`）；`services/bootstrap.rs:758`-`:761`（只有 `Err(_)` 分支 `errors.fetch_add`）；来源 `docs/validation/p7-acceptance.md` §6.7 第 37 条（已判归 **P6**） | 计划写"单次失败不会让线程死掉"，但 panic 不在那句话的覆盖范围内，且现象是**没有任何一处会红** |
| **M7** | Minor（**已闭合**） | 源码注释 `bootstrap.rs:78`-`:80` 原写"六步在这里落成**八条**记录" | **2026-10-04 已由控制器修正**：现为 `services/bootstrap.rs:78`-`:81`，写"落成**九条**记录"并把三条拆分的来由写全（镜像与仓库逐字节一致，实测）。本计划的断言一律以 `StartupStep::ALL`（`:105`）为准 | `services/bootstrap.rs:78`-`:81` vs `:105` | 这条注释曾是漂移源（少一条）；**它的修正使该文件 `:79` 之后的所有行号 +1**——本计划里的 `bootstrap.rs` 行号已按修正后的树全部重核（见下方「fix round 2」） |

**控制器裁决回填（2026-10-04；本计划审前留白的一处）**：

- **迁移前备份的原语 = `VACUUM INTO`；失败即拒绝迁移；只在真的要迁移时执行**（零新增依赖）。
  **二次裁决（2026-10-04）**：原裁决是"**无条件**每次启动都备份"，现改为"**只有 `user_version < SCHEMA_VERSION` 时才备份**"——
  即 `user_version == SCHEMA_VERSION`（无需迁移）时**跳过备份并记一条诊断**；库文件**不存在**（首启）同样跳过。
  **代价说明**：这样避免"无迁移时也要复制一份库"的**启动开销与磁盘占用**（大库尤其明显，保留策略还会长期占 5 份）；
  P1 计划 `:33`/`:35` 的原文是「**现有库迁移前**由初始化流程做一致备份，**备份失败拒绝迁移**」——它保护的是**迁移这一步**，
  不迁移就没有可保护的动作。**不变的部分**：`VACUUM INTO` 主路径、同一 `Connection`、顺序
  「拿到锁 → `Db::open` → 读 `user_version` → **（需迁移时）备份** → `migrate`」、失败复用 `STORAGE_ERROR` + `detail` 前缀、
  命名与**保留最近 5 份**、`"backup"` feature 暂不引入。
- 依据与实测：`rusqlite 0.40.2` / `libsqlite3-sys 0.38.2` 的 bundled SQLite 是 **3.53.2**（本机 cargo 缓存里 `sqlite3/sqlite3.h` 的 `SQLITE_VERSION`），`VACUUM INTO` 要 3.27+ ⇒ 离线可用；而开 `rusqlite` 的 `"backup"` feature 要动 `Cargo.toml`/`Cargo.lock`，本机是离线环境 ⇒ **暂不引入**。
- 落地位置：Task 1 该条的五个子项（主路径 / **按需执行** / 失败口径与错误码 / 命名与保留 / 暂不引入 `"backup"`）、Task 1 主条的**验收判据**、Task 4 的首条、文末「产出接口」第 3 条。
- **`mode` 与本计划无关**（那是 P8 的条目，见 P8 计划的裁决回填）。

## fix round 2（2026-10-04 终审：3 处 Critical + 5 处 Important）

> 终审实测出的都是"照原文写写不出来"。下面记**原 → 新 → 依据 → 为什么**；正文对应位置已按新措辞改。
> **另有一处全局影响**：`services/bootstrap.rs` 的 M7 注释修正（`:78`-`:81`，控制器 2026-10-04 提交）让该文件 `:79` 之后的行号**整体 +1**；
> 本计划里所有 `bootstrap.rs` 引用已按修正后的树**重核**（例：`AppState` 字段 `:342`-`:344`、`sampling_action:749`、`lock_app:399`、`Db::open:664`、`migrate:667`）。

| # | 级别 | 原措辞（位置） | 新措辞 / 落地口径 | 依据（实测 file:line） | 为什么必须改 |
| --- | --- | --- | --- | --- | --- |
| **C-A** | **Critical** | Task 4「恢复完成必须生成**全新 `data_epoch`**」——没说用什么写 | 登记 `storage::meta::rotate_epoch(tx: &Transaction<'_>) -> Result<String, AppError>`：`UPDATE app_meta SET data_epoch = ?1 WHERE singleton = 1`，返回新值；**只在恢复/替换库成功后调用一次**，与 `run_repo::start_run` **同一事务**；**回滚路径不调**（原库的 epoch 原样保留） | `meta.rs` 只有 `read_meta:21` / `require_meta:37` / `init_meta:47`（**INSERT**）/ `bump_revision:64`（只动 revision）；`app_meta.singleton` 是 PK（`schema_v1.rs:22`-`:26`）；全仓唯一改过 `data_epoch` 的地方是**测试**里的裸 SQL（`tests/handshake.rs:25`） | 没有这个函数，"生成新 epoch"写不出来：`init_meta` 会撞 PK，`bump_revision` 不动 epoch |
| **C-B** | **Critical** | Task 4「关闭连接…同目录可回滚切换…再创建协调器并恢复驱动」——没有可调用的装入口，也没说旧 `Scheduler` 怎么收尾 | 三个具名接缝 + **三段流程**（见下「C-B：运行态装入口与三段流程」） | `AppState.{db,coordinator,recovery}` 私有（`bootstrap.rs:342`-`:344`）；`RunningApp.sampling` 私有（`:197`）且只有只读访问器（`:230`/`:235`）；`Db::open` 是**关联函数**（`db.rs:42`）、`Db` 没有 reopen/close；`Scheduler::spawn` 要新闭包（`scheduler.rs:71`）而 `stop` 不可逆（`:126`） | 原文既没有装入口、也没回答"Scheduler 停不停"；照原文写只能新造第二套驱动或去动私有字段 |
| **C-C** | **Critical** | Task 2「事件到达后的处理与周期采样走**同一条** `sampling_action` 路径」＋「时钟规则一律按 P2 的 `system_pause`」 | 两句互相矛盾 ⇒ 改成**一条具名路径**（见下「C-C：OS 事件到 `system_pause` 的具名路径」） | `sampling_action`（`:749`）→ `AppState::sample_tick`（`:528`）＝**heartbeat + tick**，**永不调 `system_pause`**；`Coordinator::system_pause`（`coordinator.rs:1293`）**生产零调用**（只有定义 + `tests/timer_regressions.rs` 13 处） | 按原文实现，锁屏/休眠会被当成一次普通心跳 ⇒ **R-02 静默落空**（不 pause、不 recovering） |
| **I1** | Important | 「封锁用户/系统写入入口…`guard_writable()`」——**有签名没有调用点** | 写死调用点（`run_command`）+ 白名单（见下「I1：`guard_writable` 的调用点与白名单」） | 唯一公共入口是 `run_command`（`commands/mod.rs:127`-`:141`；`lock_app` 在 `:140`） | 没有调用点 ⇒ 维护态形同虚设；白名单不写死 ⇒ 恢复流程会挡住自己 |
| **I2** | Important | 「按新库创建新 run、调用 P3 扫描」——没说先后 | 顺序在 C-B 的 ③-a/③-b 里写死：**`Db::open` → （同事务）`start_run` + `rotate_epoch` → `scan_recovery(conn, &new_run_id)` → 建协调器 → `install_runtime`**；`scan_recovery` 用**既有那个函数**（`bootstrap.rs:314`），与 P3 的 `rescan_recovery`（`2026-10-03-p3-recovery-and-history.md:76`）**共用同一实现、不另写查询** | `scan_recovery(conn, current_run_id)`（`:314`）按 `run_id <> 当前 run` 判，**必须先有新 run** | 顺序颠倒 ⇒ 扫描把**本 run** 的会话也当成历史（或反过来漏掉），恢复门禁判错 |
| **I4** | Important | 依赖清单的 storage 行没有 `current_version` | 补进清单：按需备份读的就是它 | `migrations.rs:29 pub fn current_version(conn) -> Result<i64, AppError>`（`migrate` 内部也用它，`:39`） | 清单缺一条，"按需"判据的读法就要现找 |
| **I5** | Important | 新增第六个错误码只列了"四处联动" | 补**前端两处**：`src/__tests__/ipc.test.ts:117` 的用例名写着"五个码"、`src/types/ipc.ts:86` 的注释写着"五个稳定码" | `ERROR_CODES`（`types/ipc.ts:87`-`:93`）被 `snapshot-contract.test.ts:291` 当**取值域**用——加第六项**不破坏它**（它只检查快照里出现过的值 ∈ 域），但注释与用例名要跟事实走 | 改码不改测试名/注释 = 又一处"文档说的与树不一样" |

### C-B：运行态装入口与三段流程

**先回答"Scheduler 停不停"：不停、也不重启——复用同一个 `Scheduler`。**
`Scheduler` 的闭包只捕获 `Arc<SharedApp>` / `Arc<Broadcaster>` / `Arc<AtomicU64>`（`bootstrap.rs:718`-`:723` 的 `Scheduler::spawn`），每一拍从 `AppState` 里读**当前**运行态 ⇒ 换库、换协调器之后，**同一个调度器自然对新运行态工作**。`stop()`（不可逆，`scheduler.rs:126`）仍然只在显式退出路径上调用。⇒ `RunningApp.sampling`（`:197`）**不改类型、不加访问器**，C-B 里"Scheduler 是私有且只有只读访问器"这个障碍就此消掉。

**新增接缝（`services/bootstrap.rs`，`impl AppState`）**：

```rust
/// 运行态：库 + 协调器。维护态期间它可以**不在手**（`take_runtime` 取出、`install_runtime` 装回）。
pub struct Runtime { pub db: Db, pub coordinator: Coordinator }

impl AppState {
    /// 取出运行态。只在维护态下可调；再调返回 Err(DATA_RESTORE_IN_PROGRESS)。
    pub fn take_runtime(&mut self) -> Result<Runtime, AppError>;
    /// 装回运行态（成功 = 新库那一套；失败 = 重开原库后**重建**的那一套）。
    pub fn install_runtime(&mut self, runtime: Runtime, recovery: RecoveryScan) -> Result<(), AppError>;
    /// 运行态是否在手（命令门禁与诊断用）。
    pub fn runtime_present(&self) -> bool;
    /// 平台可信边界（C-C）：通往 `Coordinator::system_pause` 的**唯一**生产入口。
    pub fn system_boundary(&mut self, boundary: Option<ClockSample>) -> Result<TimerSnapshot, AppError>;
}
```

- **实现口径**：`db`/`coordinator`/`recovery` 三个字段变 `Option<...>`；**访问器改成可失败**（`db(&self) -> Result<&Db, AppError>`、`db_mut(&mut self) -> Result<&mut Db, AppError>`、`coordinator(&self) -> Result<&Coordinator, AppError>`、`recovery(&self) -> Result<&RecoveryScan, AppError>`），缺运行态一律 `Err(AppError::DataRestoreInProgress)`。**不 `unwrap`、不 panic**。
- **这个改动的代价要如实认下来（2026-10-04 fix round 4：因 I-1，改成实测实数与分档）**：**共 52 处触碰点**：明细见「fix round 4」的 I-1 表——`commands/mod.rs` **19**（`db()` 8 + `db_mut()` 11）、`commands/dev.rs` **1**、`services/bootstrap.rs` 内部 `let AppState { … }` 解构 **9**、`tests/` **23**（6 个文件）。**清单以符号 `grep` 为准，别按计划里的数字估**（上一轮写的 13 处少算了约 3 倍）。`run_command` 的错误路径在"运行态不在手"时**不能再读库**，直接回 `authority: None` 的 `DATA_RESTORE_IN_PROGRESS`。
- **失败回滚装回去的是什么**：**不是**切换前那个 `Coordinator`——它的 `Db` 已经 drop（连接关了），它的 `Instant` 基线也不再可用（计划原文"不得沿用切换前 Instant"）。⇒ 回滚 = **重开原库 + 新 run + `scan_recovery` + 新协调器**（计划原文"以原 epoch 创建新 run 并扫描重建运行态"），旧 `Runtime` 只提供**路径与身份**（db 路径、`data_epoch`、`run_id`）。

**三段流程（恢复的唯一路径；①③ 短临界区，② 不持锁）**：

| 段 | 持锁 | 做什么 |
| --- | --- | --- |
| ① 进入 | `lock_app` 内 | `begin_maintenance(Restore, now)` → `take_runtime()` → **释放锁**。此后到达的命令一律 `DATA_RESTORE_IN_PROGRESS`（I1），采样拍整拍丢弃（Task 2） |
| ② 换库 | **不持锁** | 旧 `Db` 随 `Runtime` 一起 drop（**这时才关连接**）→ 临时路径验证（完整性 / 外键 / schema 版本）→ 同目录**可回滚切换**（原库留成回滚副本）。失败 ⇒ 直接走 ③-b |
| ③-a 提交 | `lock_app` 内 | `Db::open(新路径)` → **同一事务**：`run_repo::start_run(new_run_id)` + `meta::rotate_epoch(&tx)` → `scan_recovery(conn, &new_run_id)` → `Coordinator::new(clock, new_run_id)` + `establish_anchor(sample)` → `install_runtime(runtime, scan)` → `end_maintenance()` → 广播一条 `domain.changed`（**新 epoch**） |
| ③-b 回滚 | `lock_app` 内 | `Db::open(原路径)` → 新 run（**不 rotate**）→ `scan_recovery` → 新协调器 → `install_runtime(runtime, scan)` → `end_maintenance()` → 广播（**原 epoch**：客户端据此知道恢复没发生） |

- ①与③之间不持锁，**这正是"维护态 + 快速失败"能成立的前提**：长活（换库/验证）不占锁，别的命令取到锁后立刻拿到 `DATA_RESTORE_IN_PROGRESS` 而不是挂住。
- 客户端怎么知道恢复结束：③ 广播的那条 `domain.changed` 带**新 epoch** ⇒ 各窗口闸门规则①（`events.rs:393`：未知 epoch ⇒ `Rehandshake`）自动触发重新握手。**不新造事件名**（沿用既有 `domain.changed`）。
- ③ 只调 `rotate_epoch` 一次、且**只在 ③-a**：回滚路径保留原 epoch，否则"旧 epoch 的请求被拒"这条判据会把**没被替换的库**也一起拒掉。

### C-C：OS 事件到 `system_pause` 的具名路径

```
platform::system_events::spawn(clock, alive, on_event)      // 平台叶子：只做 OS 适配，不含业务规则
      ↓  on_event(SystemEvent { kind, boundary: Option<ClockSample> })
组合根 lib.rs 的 setup（startup 成功之后，与 tray::build:228 / spawn_activation_watcher:231 同一段）
      ↓  lock_app(app) → AppState::system_boundary(boundary) → Coordinator::system_pause(db, boundary)
      ↓  返回的 TimerSnapshot 经 running.broadcaster() 广播
```

- **新增（`platform/system_events.rs`，登记进 `platform/mod.rs`）**：
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum SystemEventKind { Locked, Unlocked, Suspending, Resumed, TimeChanged }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct SystemEvent { pub kind: SystemEventKind, pub boundary: Option<ClockSample> }

  /// 订阅 OS 事件；每个事件自带**事件时刻的样本**（拿不到就是 None ⇒ 调用方按"边界未知"处理）。
  pub fn spawn<F>(clock: Arc<SystemClock>, alive: Arc<AtomicBool>, on_event: F) -> std::io::Result<()>
  where F: FnMut(SystemEvent) + Send + 'static;
  ```
  回调是**闭包**而不是 trait 实现：`platform` 不得引用 `services|storage|commands`（`check-layers.ps1:171`），所以"谁来处理"必须由组合根注入——与 `Scheduler::spawn` 同一手法。
- **注入点与理由**：装在 **`lib.rs` 的 `setup`**（用 `RunningApp::app()`（`services/bootstrap.rs:208`）与 `RunningApp::broadcaster()`（`services/bootstrap.rs:212`）），**不给 `startup` 加参数**——`bootstrap::startup` 今天有 **12 个真实调用点**（`lib.rs:214` + `tests/` 11 处：`startup_order.rs` 6、`dev_injections.rs`/`exit.rs`/`ipc_commands.rs`/`periodic_sampling.rs`/`shell_lifecycle.rs` 各 1；注意 raw `grep` 会数成 **13**，因为 `tests/startup_order.rs:214` 只是**注释里**提到 `startup()`），加参数要改 12 处并动 P7 已交付的测试面；而 `spawn_activation_watcher`（`window.rs:156`，`lib.rs:231` 挂）已经确立了"组合根挂平台线程 + `alive` 标志收尾"的先例。
- **时钟必须同源（这条不写会静默降级）**：边界样本的 `monotonic_ms` 必须与协调器**同一个 `SystemClock` 原点**，否则 `system_pause` 的边界校验（`coordinator.rs:1304`-`:1316`：`b.monotonic_ms` 必须落在 `(previous, sample)` 内）**必然拒绝**，现象是"每次锁屏都掉进 `recovering`"（R-02 静默落空）。⇒ **前置改动**：`SystemClock` 加 `#[derive(Clone)]`（`platform/clock.rs:37`；`Instant` 是 `Copy` ⇒ 克隆共享同一 `origin`），`lib.rs` 建一个、克隆一个给事件源、原件交给 `startup`。（替代：`Arc<SystemClock>` + 在 `clock.rs` 加 `impl<T: Clock + ?Sized> Clock for Arc<T>`；两条都行，实施时选一条并登记。）
- **与 `sampling_action` 的分工（写死，别混）**：`sampling_action`（`:749`）= **周期**心跳/tick（1 秒一拍、30 秒检查点、空闲零写）；`system_boundary` = **平台可信边界**（锁屏/休眠事件到达时刻，事件驱动）。两者共用的是**同一把锁与同一份 `AppState`**，不是同一个函数——这正是原措辞把两件事说成一件的地方。
- **判据（照 P2 原文，不另写）**：`Locked`/`Suspending` 且边界可信 ⇒ `system_pause(db, Some(b))` ⇒ 会话 `paused`（`coordinator.rs:1346`-`:1358`）；边界未知/越界/晚到 ⇒ `system_pause(db, None)` ⇒ `recovering`；**唤醒不自动继续**（`Unlocked`/`Resumed` 只观察边界，不调 `start`/`resume`）。
- **失败路径**：事件源起不来只记诊断、保持周期采样可用（与 `window.rs:181`-`:185` 同一姿势），**不 panic、不假装成功**；维护态期间事件与采样走**同一个** `sampling_allowed()` 判据。
- **可断言的那半边**：可注入的事件源 + `FakeClock`（不依赖真实 OS 通知、不依赖真实 30 秒）——可信边界 ⇒ `paused`；边界 `None` ⇒ `recovering`；`Unlocked` ⇒ **没有**自动继续；事件与采样交错 ⇒ 不产生双重分割。

### I1：`guard_writable` 的调用点与白名单

- **调用点（唯一**一处**）**：`commands/mod.rs` 的 `run_command`（`:139`-`:141`）——`let mut guard = lock_app(&app);` 之后、`body(&mut guard)` 之前加 `guard.guard_writable()?`（错误同样经 `capture_error_response` 之外的**维护态专用响应**返回，因为它不该去读库）。
- **白名单：维护态期间 24 条命令**全部**拒绝**（含 `get_revision` / `timer_snapshot` / `timer_tick` 与全部列表族）。理由：维护态里**运行态不在手**（C-B 的 `take_runtime`），任何读都会撞到"没有库、没有协调器"；放行只读命令只会让它们报出与"正在恢复"无关的错误，或者读到占位状态。
- **唯一的例外不是命令**：恢复流程自己的 ②③ 两段在同一次后台阻塞调用里**连续调服务原语**（`begin_restore` / `prepare_and_swap` / `commit_restore` / `abort_restore`），**不重新进 `run_command`**。⇒ **白名单里没有任何 IPC 命令名**，这条要原样写进实现注释。
- **被拒响应的形状**：`code = "DATA_RESTORE_IN_PROGRESS"`、`message` = 新增文案、`authority = None`（**不读库**）、`requires_handshake = false`（维护态里没有可握手的目标；恢复结束后的重新握手由"新 epoch 的 `domain.changed`"触发，见 C-B）。
- **P8 新增的 9 条命令**（见 P8 计划「P8 新增的 IPC 命令」）自动经过 `run_command` ⇒ **不需要各自再判**维护态。

## fix round 3（2026-10-04：跨文件对齐）

| # | 级别 | 原措辞 | 新措辞 / 落地 | 依据（实测） | 为什么必须改 |
| --- | --- | --- | --- | --- | --- |
| **①** | Critical（跨文件） | **P6 里 `retry_recovery` 0 命中**——P3 把"采集与看门狗"划给 P6，P6 却只登记了采样线程 panic（§6.7 第 37 条），没有故障态（`faulted`）的检测半边 | 新增 Task 2 条目「**协调器故障态（`faulted`）的检测半边**」（`:265` 起）：12 处置真点按成因分四组、**两条不同的故障路径（`Err` vs panic）的信号与处置对照表**、P6 只做三件检测（按跃迁记一次 / 启动路径点名 / 正式诊断日志记进入与清除）、故障态的可见通道（`RECOVERY_REQUIRED` + 诊断日志 + 新增只读投影）、四份职责的交叉引用 | `coordinator.rs:170`（字段）/`:186`（初值）/`:315`（`refuse_if_faulted`）/`:322`（`is_faulted`，**今天只有测试调用**）/`:1265`（`retry_recovery`）/12 处置真点 `:624/636/641/645/653/663/953/1067/1090/1104/1154/1243`/清除点 `:667`、`:1288`；P3 计划 §0.3 的 **S12**（实现 P2 / 出口 P3 / 触发 P8 / 看门狗 P6） | 缺检测 ⇒ 故障后"计时可用"不闭环：用户只看到命令全红，没人知道是**内存故障态**还是**线程死了** |
| **②** | Minor（跨文件） | P6 引用 P3 的行号（`p3:76`） | 改成**按符号/小节定位**：P3 计划 §0.3 的 **S1**（`AppState::rescan_recovery`） | P3 计划本轮 511 → **667 行**，行号已漂 | 行号会因并行改动漂移；符号与小节名不会 |

**新增的产出/依赖（① 的连带）**：产出接口新增第 6 条 `AppState::timer_faulted(&self) -> bool`（口径：运行态不在手 ⇒ `false`；**只是观察口，不是出口**）；依赖清单新增 `Coordinator::{faulted:170, refuse_if_faulted:315, is_faulted:322, retry_recovery:1265}` 一行。**P6 不调 `retry_recovery`**（否则就是自动重试，08 §1 禁止）。

## fix round 4（2026-10-04 定向复评：2 处新 Critical + 4 处 Important）

> 复评针对的是 **fix round 2/3 新引入的**问题。仍然只改这两份计划 + 报告。

| # | 级别 | 原措辞（我上一轮写的） | 新措辞 / 落地口径 | 依据（实测 file:line） | 为什么必须改 |
| --- | --- | --- | --- | --- | --- |
| **C-1** | **Critical** | fix round 2 的恢复骨架（③-a/③-b）只写 `scan_recovery(conn, &new_run_id)`，把"**P3 扫描**"替换成了**门禁查询** | ③-a/③-b 在 `scan_recovery` **之前**加 `services::recovery::scan_at_startup(&mut db, &new_run_id, now)`（P3 §0.3 的接缝），口径统一为「**先 `scan_at_startup` 做四类归一 → 再 `scan_recovery` 算门禁**」 | `scan_recovery`（`bootstrap.rs:314`）只读三条查询（`session_repo::{unfinished_sessions_of_other_runs, pending_intervals_of_other_runs, invariant_faults_of_other_runs}`），**不做四类归一**；启动路径今天就是"④ 步扫描"（调用点 `:699`），P3 计划 §0.3 明写第 ④ 步要从 `scan_recovery` 改成 `scan_at_startup(db, current_run_id, now)`；`uq_running_foreground` 在 `schema_v1.rs:183`，**不带 run 过滤** | 恢复用的备份**必然可能**带旧 run 的 `state='running'` 会话（备份取自计时中的库）⇒ 缺归一 ⇒ 该会话永远停在 `running` 且**占着全局前台唯一索引** ⇒ `start` 被 `require_no_running_foreground` 挡住，"恢复后计时可用"只能靠作废整次达成 |
| **C-2** | **Critical** | ① 我在依赖清单里写"维护态下托盘动作同样走命令体…因此自动被 `guard_writable` 挡住"；② 没写 `RunningApp::shutdown()` 的维护态分支 | ① 订正那句话（**托盘不走 `run_command`**）；② 把维护态分支写进计划：**在 `sampling.stop()` 之前**判维护态/退出意图，拒绝时`stop()` **不得**被调用；③ 给出两条托盘路径在维护态下的具体行为与判据 | `spawn_tray_pause`（`commands/mod.rs:1282`）**直接** `lock_app`（`:1289`）→ `tray_pause_impl`（`:1234`）；`spawn_tray_quit`（`:1311`）**连锁都不取**，直接 `tray_quit_impl`（`:1265`）= `running.shutdown()`（`bootstrap.rs:257`-`:283`）；`shutdown` 的顺序是 `holds_app_lock` 检查（`:263`）→ **`sampling.stop()`（`:270`，不可逆）** → `lock_app(...).snapshot(db)?` → `explicit_exit` | 维护态（运行态不在手 ⇒ 访问器返 `Err`）时照原样执行：**采样线程已经永久停了**、退出事务没跑、进程还活着 ⇒ **只能强杀**（而且维护结束后计时也不再采样） |
| **I-1** | Important | "这个改动的代价…（**约 13 个调用点**）" | 改成**实测实数与分档**：`commands/mod.rs` **19**（`db()` 8 + `db_mut()` 11）、`commands/dev.rs` **1**、`services/bootstrap.rs` 内部 `let AppState {` 解构 **9**、`tests/` **23**（6 个文件：`shell_lifecycle` 9 / `exit` 5 / `dev_injections` 3 / `startup_order` 3 / `ipc_commands` 2 / `periodic_sampling` 1）——共 **52** 处触碰点，并注明"**清单以符号 `grep` 为准**，别按计划里的数字估" | `grep -c` 实测（2026-10-04 21:3x） | 少算 3 倍会让"破坏性变更"被低估，实施时才发现 |
| **I-2** | Important | OS 事件块里 3 条子条仍写"与 `sampling_action` **同一条**路径 / **同一入口，而非第二条路径**" | 逐条改成 C-C 的新路径口径（保留"原记"痕迹） | `sampling_action`（`:749`）→ `sample_tick`（`:528`）**永不调 `system_pause`**；`Coordinator::system_pause`（`coordinator.rs:1293`）生产零调用 | 与新路径**正面冲突**：照验收口径写，实施者会去把事件塞进 `sampling_action`，R-02 再次静默落空 |
| **I-4** | Important | "四个只读访问器改成可失败：`db()`/`db_mut()`/`coordinator()`/`recovery()`" | **改成三个**：`db()` / `db_mut()` / `coordinator()` 变 `Result`；**`recovery()` 签名不变**（`-> &RecoveryScan`，字段也**保持非 `Option`**：`take_runtime` 只取走 `db`+`coordinator`） | `AppState::recovery()` 今天**生产零调用**（`grep -rn "\.recovery()" src/` = 0）；P3 已交付的测试用 `recovery().requires_recovery()`（`tests/startup_order.rs:155`/`:526`-`:529` 走的是 `RunningApp::recovery()`，同形的 `AppState::recovery()` 调用在 P3 侧已写） | 为一个零调用的访问器改签名，会**改坏 P3 已交付的测试**（跨计划冲突），收益为零 |

#### C-1 的落点（三处口径统一）

1. **恢复骨架 ③-a / ③-b**（Task 4 的执行骨架条目，`:334`）：`Db::open` → 同事务 `start_run` + `rotate_epoch` → **`scan_at_startup(&mut db, &new_run_id, now)`** → `scan_recovery(conn, &new_run_id)` → 新协调器 → `install_runtime`；回滚路径同形（**不 rotate**）。
2. **fix round 2 的 I2 行**（`:79`）：把"调用 P3 扫描"明确成**两个函数的先后**，不再含糊。
3. **fix round 2 的 C-B 表**（`:116`-`:117`）：③-a/③-b 两格都补上 `scan_at_startup`。
   **分工一句话**：`scan_at_startup`（P3，**唯一**做四类归一：第 2 类归一 / 第 4 类 run 重绑 / 第 3 类保持 / 第 1 类只诊断，返回 `StartupScanReport`）；`scan_recovery`（P7 既有，**只**算门禁 `RecoveryScan`）。**顺序不能倒**：门禁要基于归一之后的事实。

#### C-2 的落点（退出路径与托盘）

1. **`RunningApp::shutdown()` 的维护态分支（写进计划，产出接口第 8 条）**：顺序固定为
   `holds_app_lock` 检查（既有，`:263`）→ **`AppState::begin_exit()`**（新增，锁内、短）→ `sampling.stop()`（`:270`）→ `explicit_exit`（`:546`）。
   `begin_exit` 的判据：**维护态（运行态不在手）⇒ 拒绝**（返回 `DATA_RESTORE_IN_PROGRESS`，**不碰采样线程**）；否则置 `shutting_down = true` 并返回成功。
   **为什么不能只做"看一眼前再 stop"**：检查与 `stop()` 之间有一个窗口，若恢复流程恰好在这个窗口里 `begin_maintenance`，采样线程照样被永久停掉、退出事务照样跑不成 ⇒ 同一个死路。`begin_exit` 与 `begin_maintenance` **互斥**（各自拒绝对方已置位的状态），窗口因此关闭。
2. **依赖清单里那句错话**（`:379` 一带）改成：「托盘动作**不经 `run_command`**：`spawn_tray_pause`（`commands/mod.rs:1282`）自己 `lock_app`（`:1289`）后调 `tray_pause_impl`（`:1234`），`spawn_tray_quit`（`:1311`）直接调 `tray_quit_impl`（`:1265`）＝ `RunningApp::shutdown()`。⇒ **`guard_writable` 挡不住它们**，两条路径各自必须在取锁后先判维护态。」
3. **维护态下两条托盘路径的具体行为（判据写死）**：
   - **暂停**：`tray_pause_impl` 在 `lock_app` 之后先判维护态 ⇒ **拒绝**（`DATA_RESTORE_IN_PROGRESS`），不写任何东西；判据：维护态期间点暂停 ⇒ 库零写入（`revision`/`interval_checkpoint` 不变）、诊断里出现拒绝原因。
   - **退出**：`shutdown()` 走上面的 `begin_exit` ⇒ **拒绝**（`DATA_RESTORE_IN_PROGRESS`），**采样线程仍在跑**（判据：拒绝后 `sampling_ticks` 仍在涨）；用户**不能**在恢复中途退出进程（恢复本身很短，且强杀会让库停在中间态）。**允许的例外**：恢复期间用户仍可用操作系统的强杀——这不是"支持"，是不阻止。
   - **可见性**：这两条拒绝今天只落到 `eprintln!`/`println!`（`commands/mod.rs:1296`/`:1320` 一带的 `diagnostic(&error)`），而 release 的 Windows 子系统**没有控制台** ⇒ 必须有落盘诊断（P6 的"正式诊断日志"）与界面提示（`p7-acceptance` §6.3 第 9 条「退出事务失败的用户可见提示」，归 P6/P8；P8 侧在其 Task 5 与本计划登记表里已列）。**本轮只写清口径，不新造事件名**。
4. **新增状态与互斥（产出接口第 7/8 条）**：`AppState::begin_exit(&mut self) -> Result<(), AppError>` 与私有字段 `shutting_down: bool`；`begin_maintenance`（Task 2 已有签名）在 `shutting_down` 为真时同样拒绝。"退出是终态"这条既有语义不变。

#### I-1 的准确清单（以符号 `grep` 为准，别按数字估）

| 面 | 实测条数 | 说明 |
| --- | --- | --- |
| `commands/mod.rs` 的 `db()` | **8** | `:141`（`capture_error_response(guard.db(), …)`）、`:364`（`handshake::get_revision(app.db())`）等 |
| `commands/mod.rs` 的 `db_mut()` | **11** | 全部是写命令体（`catalog`/`daily_plan`） |
| `commands/dev.rs` | **1** | debug-only |
| `services/bootstrap.rs` 内部 `let AppState { … } = …` 解构 | **9** | 字段变 `Option` 后每处都要处理 |
| `tests/` | **23**（`shell_lifecycle` 9 / `exit` 5 / `dev_injections` 3 / `startup_order` 3 / `ipc_commands` 2 / `periodic_sampling` 1） | 多数是 `h.db()`/`.db_mut()` |
| **合计** | **52** | **`recovery()` 不在其中**（I-4：签名不变） |

---

## Task 1：单实例与启动顺序的硬化

文件：platform/single_instance.rs、services/bootstrap.rs、tests/startup_order.rs。

- [ ] **P7 已建立这套顺序并有测试**（P7 排执行顺序第 4，本计划第 7）。本任务不重写它，而是补两件事：① 把 P7 的「开发验证库」门禁升级为**正式恢复扫描接入**（调 P3 的扫描入口）；② 补故障路径——锁的异常释放、启动中途失败的状态回退、第二次启动的各种竞态。

- [ ] **启动顺序固定为**（01 §2 与 02 §4 的合并结论，不得调整）：
  1. **单实例检查**（必须**先于**持久化初始化，否则第二个进程可能先建库/迁移）；
  2. 打开库并迁移；
  3. 新建 `application_run`；
  4. 调用 **P3** 的恢复扫描；
  5. 启动协调器（P2）与周期采样驱动（本计划 Task 2）；
  6. 开窗口（P7）。
  - **（2026-10-04 修订：因 M1/M7）这 6 步在探针里是 9 条记录**：`pub enum StartupStep`（`src-tauri/src/services/bootstrap.rs:83`）与 `pub const ALL: [StartupStep; 9]`（`:105`）——①拆成 `SingleInstanceChecked`/`ExistingInstanceNotified`、②拆成 `DatabaseOpened`/`Migrated`、⑤拆成 `CoordinatorStarted`/`SamplingStarted`。**顺序断言按 `StartupStep::ALL` 写 9 条**，不要按"六步"写。**（2026-10-04 重核）**源码那条注释已由控制器修正为"九条"（`bootstrap.rs:78`-`:81`），与 `ALL` 一致；它的修正让该文件 `:79` 之后的行号整体 +1，本计划的 `bootstrap.rs` 行号**已按修正后的树重核**。
- [ ] 单实例用**文件锁**实现（`platform/` 叶子，不含业务规则）：拿不到锁的进程**通知既有实例后退出**，不得打开数据库、不得初始化计时（F-016 原文）。
- [ ] 通知机制要与锁分离：锁只保证"只有一个"；"唤起既有主窗"是通知，由 P7 消费一个"第二实例请求唤起"的信号。
- [ ] `application_run` 生命周期：每次成功启动建一行；**显式退出**在一个事务里结束 `running`/`paused` 会话并存 `clean_exit_at`；`recovering` 记录**保留**不清（02 §4、02 §9）。
- [ ] 锁文件与数据库同在 `platform::paths` 的应用数据目录下；锁的持有者崩溃后必须能自动释放（用 OS 级文件锁，不要用"写 pid 文件 + 手动清理"）。
- [ ] 测试：第二次启动不打开库、不迁移、不建 `application_run`；持有者被强杀后新进程能拿到锁；显式退出写 `clean_exit_at` 且结束 running/paused；启动顺序的可观测副作用顺序正确（用注入的探针记录调用次序）；启动失败时锁被释放。

- [ ] **初始化编排补一致备份（P1 计划 `:33`/`:35` 已勾选但全仓无实现的那条；`src-tauri/src/storage/migrations.rs:9-11` 的现行注释正指向本计划）**：把启动顺序的第二步固定拆成三步——「**单实例 → （需迁移时）迁移前一致备份 → 迁移**」。拿到单实例锁之后、调用 `storage::migrations::migrate` **之前**，**先读 `user_version`；只有当 `user_version < SCHEMA_VERSION`（真的要迁移）时**才由本编排先对**既有库**做一次一致备份（**二次裁决 2026-10-04**：原措辞是"无条件每次都备份"，改为按需——理由与代价说明见上方「控制器裁决回填」）；**备份失败就拒绝迁移**（不降级、不跳过、不先迁移后补），本次启动以可诊断的失败结束：不建 `application_run`、不启动协调器与周期采样驱动、不开窗口。备份原语复用本计划 Task 4 的 WAL 一致备份（`services/backup.rs`），本任务只负责**编排与顺序**（实现落在 `services/bootstrap.rs` 的启动入口，探针断言在 `tests/startup_order.rs`），不得自建第二套拷贝逻辑，也不得把备份塞进 `migrate` 内部——备份要停计时、关连接，是进程级动作（`src-tauri/src/storage/migrations.rs:9-11` 原文）。验收：**在"需要迁移"的库上**注入探针断言顺序为「拿到锁 → 备份完成 → `migrate` 开始」；备份失败时 `migrate` **零调用**且库停在迁移前版本、不建 `application_run`、不开窗口；备份产物可独立打开并通过完整性与版本校验；**在 `user_version == SCHEMA_VERSION` 的库上**断言"**零备份产物**（`backups/` 不新增文件）+ `migrate` 被调用且是空操作"，首启（库文件不存在）同样零备份产物——**这两条与"备份失败 ⇒ 拒绝迁移"是同一条按需判据的三个分支，缺一条就会出现"我以为它在保护迁移"的假绿**（**三条必须成对出现、且按原因断言，见下方子项 6**）。
  - **（2026-10-04 修订：因 C1 同族的"接缝写不出来"，M4/M5 的接口核对；三条前置已由控制器裁决定稿——见下）**
    1. **主路径 `VACUUM INTO '<备份路径>'`**（**裁决 2026-10-04**：零新增依赖）。今天的顺序是 `Db::open(&config.db_path)?`（`services/bootstrap.rs:664`）→ `migrate(db.connection())?`（`:667`），中间**没有关连接的时机**（第②步之后立刻要读 `meta::read_meta` 取 `data_epoch`，`:668`），所以备份原语必须是"**库已经打开时可用**"的那一种：在**同一个 `Connection`** 上执行 `VACUUM INTO`。顺序写死为「**拿到单实例锁 → `Db::open` → 读 `user_version` → `user_version < SCHEMA_VERSION` 时 `VACUUM INTO` 备份（否则跳过并记诊断）→ `migrate`**」——`SCHEMA_VERSION` 是 `storage/migrations.rs:21` 的 `pub const`，读版本的口径与 `migrate` 自己那条检查同源（`:30`）。**离线可用性有实测背书**：本机缓存里 `rusqlite 0.40.2` / `libsqlite3-sys 0.38.2` 的 bundled SQLite 是 **3.53.2**（`sqlite3/sqlite3.h` 的 `SQLITE_VERSION`），远高于 `VACUUM INTO` 要求的 3.27。
    2. **按需执行（二次裁决 2026-10-04）**：只有 `user_version < SCHEMA_VERSION` 时才备份；`user_version == SCHEMA_VERSION`（`migrate` 是幂等空操作，`storage/migrations.rs:3-7`）时**跳过备份并记一条诊断**。理由：P1 计划 `:33`/`:35` 保护的是**迁移这一步**，不迁移就没有可保护的动作；代价是"无迁移时每次启动复制一份库"的**启动开销与磁盘占用**（大库尤其明显，保留策略还会长期占 5 份）。**首启例外照旧**：库文件**不存在** ⇒ 没有可备份的事实，跳过并记诊断（"无事可做"，不是降级）。
    3. **备份失败 ⇒ 拒绝迁移**（这条路径只在**需要迁移**时才会走到）：本次启动以可诊断的失败结束——不建 `application_run`、不启动协调器与周期采样驱动、不开窗口。**错误码复用既有的 `STORAGE_ERROR`**（`AppError::Storage`，`src/error.rs:33`；`startup` 今天对 `Db::open`/`migrate` 的失败走的就是同一条路），**不为它新增第六个码**（第六个码只留给维护态的 `DATA_RESTORE_IN_PROGRESS`，见文末产出接口）；`detail` 必须带阶段标记（如 `"pre-migration backup: …"`）以便诊断，用户文案仍走 `message()`（`src/error.rs:50`）。验收：备份失败时 `migrate` **零调用**且 `code()` 断言为 `"STORAGE_ERROR"`。
    4. **文件名与保留策略**（写死，别留白）：备份放 `platform::paths::app_data_dir()`（`platform/paths.rs:15`）下的 `backups/` 子目录；文件名 `worktrace-f<格式版号>-s<数据库版号>-v<应用版本>-<Unix 毫秒>.db`（Task 4 末条要求三个版号"分别记录"，这里是它们唯一的落点）；**保留最近 5 份**（按需执行下它只在真迁移时触发，所以 5 份是"跨版本升级"的历史，不是"最近 5 次启动"），超出时按文件名里的时间戳从旧到新删；**清理失败只记诊断，不影响本次启动**（它发生在备份成功之后，不能把一次成功启动变成失败）。
    5. **`rusqlite` 的 `"backup"` feature 暂不引入**（**裁决 2026-10-04**）：`src-tauri/Cargo.toml:34` 今天只有 `["bundled"]`，开 `"backup"` 要动 `Cargo.toml`/`Cargo.lock`，而这是**离线**环境（crate 只能来自既有缓存）；`VACUUM INTO` 已经够用。将来若真需要**进度回调 / 可取消 / 增量**能力（超大库）再单独评估，**不在本计划范围内**。
    6. **测试必须成对，且按原因断言（2026-10-04 追加；防的正是"假绿"）**：
       - **成对**：「**需迁移 ⇒ 确实写出一份备份**」（`backups/` 新增一份、且该产物可独立打开并通过完整性/版本校验）与「**无需迁移 ⇒ 零备份产物**」（`backups/` 不新增文件）这两条断言**必须出现在同一个测试文件里**——本任务的编排断言落在 `tests/startup_order.rs`，与探针断言同文件。**只写其中一条的提交视为未完成，评审直接打回**：一个"备份函数恒返回 `Ok` 且什么都不做"的实现会让只写第二条的那份用例**全绿**。
       - **反向验证（变异）**：把按需判据改成恒 `false`（永不备份）⇒ 第一条**必须红**；改成恒 `true`（总备份）⇒ 第二条**必须红**。两条都要留下实跑记录——**改坏哪一条而用例不红，就说明那条判据没被钉住**。
       - **按原因断言**：三种情形都会表现为"这次没有备份发生"，但**原因不同，必须分开断言、不得合并成一条"都没备份"的用例**：① **库文件不存在（首启）**⇒ 断言走的是"**没有可备份的事实**"这条分支（注意新库的 `user_version == 0` 属于"**需要迁移**"，所以它**不是**被"版本相等"挡下的）；② **`user_version == SCHEMA_VERSION`** ⇒ 断言走的是"**无需迁移**"这条分支；③ **需迁移但备份失败** ⇒ 断言 `migrate` **零调用** + `code() == "STORAGE_ERROR"`。
       - **判据读 `user_version`，不是 `meta::read_meta`**：读法照 `storage/migrations.rs:30`（`PRAGMA user_version`），与 `SCHEMA_VERSION`（`:21`）比较。`meta::read_meta` 读的是 `app_meta` 表，**新库在 `migrate` 之前根本没有那张表**——两者混用会把新库误判成"无需迁移"，从而**静默跳过迁移**（这是本条要防的第二个假绿）。

## Task 2：周期采样驱动的故障路径与维护态隔离

文件：platform/scheduler.rs、services/bootstrap.rs、tests/periodic_sampling.rs。

- [ ] **驱动它的定时器归 P7**（P7 排在前面，已建立并有测试）。本任务不重建第二套驱动，只补**故障路径**：连续失败的可观测性与重试、数据库忙时的行为、维护态期间不得写入（**2026-10-04 修订：因 C1**，原措辞是"必须**暂停采样**"——`Scheduler` 没有 pause，落点见下方「维护态与采样：真实接缝」）、以及定时器线程不能因为单次失败而死掉。
- [ ] 背景（为什么这条必须存在）：08 §1 要求约每 30 秒同事务写 `interval_checkpoint`，F-009 要求关掉全部窗口后核心继续运行。没有它，关窗期间不会有任何检查点，异常时只能落到「没有可信检查点 → 整个当前区间待确认」那条最差分支。
- [ ] 定时器与窗口生命周期**解耦**：窗口全关不停止；只有显式退出才停止。实现上不得把定时器挂在任何窗口对象上。
- [ ] 每次触发走与用户命令**同一条串行边界**（同一协调器入口），不得另开一条路径绕过检测——否则会出现"定时采样没检测、用户 tick 检测了"的不一致。
- [ ] 触发失败（数据库忙、协调器报告故障）**只记诊断并重试**，不得让定时器线程死掉；连续失败要能被上层观察到。
- [ ] 空闲时不写检查点：没有活动会话时定时触发应立刻返回，不产生任何写入（避免空转制造 revision 或 I/O）。
- [ ] 测试：定时器在无窗口引用时仍被驱动（用可注入的时钟/调度器，不依赖真实 30 秒）；触发确实进入同一协调器入口；没有活动会话时不产生任何写入；单次触发失败后仍能继续下一次；显式退出后定时器停止。

- [ ] **维护态与采样：真实接缝（2026-10-04 修订：因 C1；替换原"维护态期间必须暂停采样"）**

  **现状（为什么照原文写不出来）**：`Scheduler` 今天只有 `spawn(interval_ms, on_tick)`（`src-tauri/src/platform/scheduler.rs:71`）、只读的 `ticks()`（`:112`）、`stop()`（`:126`）与 `Drop`（`:147`）；**没有 `pause()`/`resume()`**，而 `stop()` 是**不可逆**的（置 `Arc<AtomicBool>` 停止位 + `join` + 完成位，线程退出后不会再来一拍）。`RunningApp.sampling` 是**私有字段**（`services/bootstrap.rs:197`），外部（Tauri 托管状态、托盘、命令层）只拿得到 `&RunningApp`，两个访问器都是只读（`sampling_ticks` `:230`、`sampling_errors` `:235`）。**更关键**：采样每一拍要取的就是那把 `Mutex<AppState>`（`sampling_action` → `lock_app`，`:749`/`:750`），所以"在 `Scheduler` 上加 pause 并等当前拍结束"会与**正持锁的调用方**互锁（与 `RunningApp::shutdown` 的自我持锁防线 `:263` 是同一个环）。

  **决定：不改 `Scheduler`，也不新增 pause/resume。** 维护态标志放进**串行边界之内**的 `AppState`，采样每拍在**取锁之后**判断、整拍跳过。三条理由：① 只有"取锁之后"这个位置才能覆盖**已经在途**的那一拍（它可能正堵在锁上，线程在不在跑都不影响判定）；② 不需要访问私有字段、不需要 `&mut`，`RunningApp` 对外仍是"不可被外部摆布"；③ 与 `stop()` 的关系保持单一：**维护态不调 `stop()`**（调了就再也回不来），`stop()` 仍是唯一停机入口，语义不变（不可逆、幂等、返回 ⇒ 线程已退出），`Drop`（`:147`）行为不变。

  **新增符号（P6 产出；归属文件 `src-tauri/src/services/bootstrap.rs`，与 `AppState`/`guard_business_timing` 同处）**：

  ```rust
  /// 维护态阶段（V0.1 只有恢复需要维护态；备份走在线备份，不停写入）。
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum MaintenancePhase { Restore }

  /// 进程内唯一的维护态记录。字段私有，读走 AppState 的方法。
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct MaintenanceState { pub phase: MaintenancePhase, pub entered_at_ms: i64 }

  impl AppState {
      /// 只读投影（诊断与 P8 的状态展示）。
      pub fn maintenance(&self) -> Option<&MaintenanceState>;
      /// 进入维护态。已在维护态 ⇒ 拒绝（不覆盖进入时刻，不重入）。
      pub fn begin_maintenance(&mut self, phase: MaintenancePhase, entered_at_ms: i64)
          -> Result<(), AppError>;
      /// 退出维护态，交回被清掉的那份记录（幂等：已清则 None）。
      pub fn end_maintenance(&mut self) -> Option<MaintenanceState>;
      /// 采样判据：无维护态才允许采样。**采样拍在 lock_app 之后第一句就问它。**
      pub fn sampling_allowed(&self) -> bool;
      /// 写入门禁：维护态 ⇒ Err(AppError::DataRestoreInProgress)（P6 新增码，见产出接口）。
      pub fn guard_writable(&self) -> Result<(), AppError>;
  }
  ```

  `AppState`（`:341`，字段私有）新增一个字段：`maintenance: Option<MaintenanceState>`（与 `db`/`coordinator`/`recovery` 并列；**不新增第二把锁**——判定必须与 `db` 在同一临界区里，这正是 D6 的"同一串行边界"）。
  **（2026-10-04 fix round 2）另四个接缝不在这里重复**：恢复态的取出/装回（`take_runtime` / `install_runtime` / `runtime_present`）与平台可信边界（`system_boundary` → `Coordinator::system_pause`）的签名、归属、三段流程见本节上方「fix round 2」的 **C-B / C-C**；仅 `db()`/`db_mut()`/`coordinator()` 三个访问器改为可失败（`Result<_, AppError>`，缺运行态 ⇒ `DataRestoreInProgress`）；`recovery()` 保持 `&RecoveryScan`，该字段不被取走。以实际调用点搜索与编译为准，旧调用数量只是历史证据（见 I-4）。

  **采样侧的落点（一处，别加第二处）**：`sampling_action`（`:749`）在 `let mut state = lock_app(app);`（`:750`）之后**第一句**加 `if !state.sampling_allowed() { return; }`，其后逻辑一字不动（`state.sample_tick()` → 有会话才 `broadcaster.emit(EventTimerTick)`，`:751`-`:763`）。

  **维护态期间的口径：不采样（整拍跳过，含读）**，不是"继续读但不写 checkpoint"。三条理由：① 恢复的换库窗口里 `AppState.db` 这个句柄正被关闭/替换（`Db` 就在 `AppState` 里，`:342`），读它要么报错（平白污染 `sampling_errors`）要么读到已经作废的那个世界；② 采样读出的快照会经 `broadcaster.emit` 广播给**所有窗口**，在"停止受理写入"的同时广播一份旧世界的 tick 自相矛盾；③ 只有真的停止取样，才能让 P2 **既有**的长间隔规则在维护结束后的第一拍成立（见下条补偿）。附带口径：维护态**不是错误**，`sampling_errors`（`:235`）不得因此增长；`Scheduler::ticks`（`:112`）照涨（它只是"触发了几次"的诊断计数）。

  **恢复时的补偿：维护窗口跨过的墙钟时间如何不污染 `duration_ms`**（三层，全部复用既有机制，**不新增时钟规则、不改 P2 的判据**）：

  1. **不采样本身就是第一层补偿**：维护后第一拍的 `d_mono` 是"维护窗口 + 一个节拍"，P2 的既有规则⑤（`services/timer/anchor.rs:236`：`d_mono > expected_interval_ms * 3` ⇒ `SampleVerdict::Suspended`）会把它判成**不可信的长间隔**，走 `Coordinator::handle_anomaly`（`coordinator.rs:1055`）→ 分割：可信前缀闭合、余段标 `needs_review`、会话置 `recovering`、`revision + 1`、写 `time_edit` 审计（`:1177`-`:1222`）。这是"维护时长不算工时"的现成分支，**不要另写一套**。
  2. **别依赖第 1 层的阈值**：`expected_interval_ms` 来自采样节拍（`DEFAULT_SAMPLING_INTERVAL_MS = 1_000`，`bootstrap.rs:70`），阈值是 3 秒 ⇒ **短维护（< 3 秒）不会触发规则⑤**。所以计划的硬要求是：**恢复流程不得复用维护前的运行态**——成功切换后按新库建新 run、新协调器（计划原文），失败重开原库时也**必须新 run + P3 扫描**（计划原文"以原 epoch 创建新 run 并扫描重建运行态"）。于是维护前那条 `running` 会话在新 run 里是**外来事实**：`scan_recovery`（`:314`）把它列进 `unfinished_sessions`、`guard_business_timing`（`:466`）挡住新计时，必须走 F-015 用户确认才成事实。因此维护跨度不会被静默计入任何数字：`live_ms` 只算**当前**开放区间（`A(M) − started_at`，`coordinator.rs:427`-`:436`），而 `duration_ms = attributed_end − started_at`（`primitives.rs:113`）里的 `attributed_end` 由确认/平台边界给出。
  3. **只有在实施者违反第 2 层（复用同一 coordinator 跨维护窗口）时才需要第三层**（**不推荐，且与计划原文冲突**）：把维护窗口显式喂给**既有**的不可信间隙分支——`Coordinator::handle_anomaly(db, sample, SampleVerdict::Suspended { gap_ms })`（`:1055` 是 `pub`；`SampleVerdict` 在 `anchor.rs:90` 也是 `pub`），`gap_ms` 取"进入维护/退出维护各一次 `ClockSample` 的单调差"（`services` 不得自取时间，仍走协调器那条时钟接缝 `AppState::now_ms` `:444` / `Coordinator::wall_ms` `:209`；需要单调值时在 `Coordinator` 上补一个只读采样入口，**不要**在服务层 `use std::time`——`scripts/check-layers.ps1:167` 会红）。`live` 为空时它是空操作（`coordinator.rs:1087`-`:1093`）。**不得**为此降低 `anchor.rs:236` 的阈值。

  **测试（可断言的那半边）**：维护态置位期间的采样拍不产生任何写入（`interval_checkpoint` 行数/列值不变、`revision` 不变、无 `timer.tick` 广播）；`ticks` 照涨而 `sampling_errors` 不涨；`guard_writable` 在维护态返回新码、退出后恢复；置位/清位都发生在同一把锁内（用 `FakeClock`，不依赖真实时间）。

- [ ] **采样线程 panic 的可观察性（2026-10-04 修订：因 M6；来源 `docs/validation/p7-acceptance.md` §6.7 第 37 条，P7 已判归 P6）**：本任务上面写着"不能让定时器线程死掉"，但**panic 不在那句话的覆盖范围内**——`platform/scheduler.rs:96`/`:97` 是 `on_tick(); ticks.fetch_add(1, …)`，`on_tick` 一旦 panic，线程直接展开退出、`ticks` 停涨，而诊断计数只在 `sampling_action` 的 `Err(_)` 分支累加（`services/bootstrap.rs:758`-`:761`），**panic 不计入**。⇒ 现象是"界面秒数照走（它从 `started_at` 算，不靠采样）、`interval_checkpoint` 不再前进、进程不报错、托盘还在"，**没有任何一处会红**。二选一（实施时定并登记）：① 在采样循环里给 `on_tick` 包 `catch_unwind`，把 panic 计进 `sampling_errors` 并继续下一拍；② 不加 `catch_unwind`，但给一个"`ticks` 不再增长"的看门狗/诊断出口，让 §2A 的实机判据（读 `interval_checkpoint` 列值）之外还有一处能红。**不论选哪条，都要有一条可失败的用例**（注入一个必 panic 的 `on_tick`，断言可观察出口变化）。

- [ ] **协调器故障态（`faulted`）的检测半边（2026-10-04 fix round 3：因 ①；P6 里此前 0 命中，而 P3 把"采集与看门狗"划给 P6）**

  **背景（分工写死，三份计划各有其责）**：`Coordinator::faulted`（`services/timer/coordinator.rs:170`，初值 `:186`）一旦置真，`refuse_if_faulted`（`:315`）会拒绝 `snapshot`/`tick`/`start`/`pause`/`resume`/`finish`/`heartbeat`/`system_pause` 等 **10 个入口** ⇒ 计时在故障后**没有出口**。三份计划的职责是：**实现 P2**（`Coordinator::retry_recovery`，`:1265`）／**生产出口 P3**（`AppState::retry_recovery`，P3 计划 §0.3 的 **S12**）／**触发 P8**（用户显式点"重试对账"，P8 计划「P8 新增的 IPC 命令」第 7 条）／**检测与看门狗 P6**（本条目；P3 计划 §0.3 S12 的末条也明写"P6 负责采样/启动路径的看门狗"，并援引 `docs/validation/p7-acceptance.md` §6.7 第 37 条）。**四者缺一，计时在故障后不可恢复**——P6 缺的是"故障是怎么被发现的、被谁看见了"。

  **① 谁把 `faulted` 置真（12 处置真点，实测；按成因分四组）**：

  | 成因 | 置真点（`coordinator.rs`） | 触发场景 |
  | --- | --- | --- |
  | 提交后重建失败 | `:624`/`:636`/`:641`/`:645`/`:653`/`:663` | 业务命令已提交、重建内存/响应时失败 |
  | 无基线可采样 | `:953` | 采样失败且没有可信基线 |
  | 异常事务失败 | `:1067` | `handle_anomaly` 的独立系统事务失败（`try_handle_anomaly` 返回 `Err`） |
  | 单调钟硬故障 | `:1090`/`:1104`/`:1154`/`:1243` | `SampleVerdict::MonotonicBackwards`（本 run 的单调读数已失去意义） |

  清除点只有两处：`retry_recovery` 成功（`:1288`）与 `:667`（重试路径内部）。**P6 不做任何自动清除**——08 §1 的立场是"故障不能自己把证据擦掉"，定时自动重试会把"用户没处理"变成"悄悄恢复"（P3 计划 S12 末条同口径）。

  **② 两条**不同**的故障路径，信号与处置都不同（别混成一条）**：

  | | 路 A：`Err`（含 `faulted`） | 路 B：采样线程 **panic**（§6.7 第 37 条） |
  | --- | --- | --- |
  | 线程 | **活着**，下一拍还会来 | **展开退出**，不再有下一拍（`scheduler.rs:96`-`:97`） |
  | `ticks`（`:230`） | 照涨 | **停涨** |
  | `sampling_errors`（`:235`） | 涨（`sampling_action` 的 `Err(_)` 分支，`:761`） | **不涨**（panic 不走那个分支） |
  | `interval_checkpoint` | 停更（`heartbeat` 被 `refuse_if_faulted` 挡住） | 停更（没有下一拍） |
  | 命令 | `RECOVERY_REQUIRED`（`refuse_if_faulted` 的返回值） | 在踩到故障前**可能照常成功**（线程死了不等于协调器故障） |
  | 会不会置 `faulted` | **会** | **不会**（panic 不经过协调器内部路径） |
  | 出口 | 用户显式 `retry_recovery`（P8 → P3 S12 → P2 实现） | §6.7 第 37 条那条（`catch_unwind` 计入 `sampling_errors`，或"`ticks` 不涨"的看门狗） |

  **③ P6 要做的检测（只做这三件，别扩张）**：
  1. **采样路径**：故障态下每一拍都会拿到 `Err(RecoveryRequired)` ⇒ 不能让它 1 秒一条刷爆诊断。要求**按"跃迁"计数/记录一次**（进入故障态时记一条，故障态持续期间不再重复刷），实现上用一个 `AtomicBool`/上一拍状态比较即可；`sampling_errors` 保留（它是"采样失败"的既有口径，不改成"故障次数"）。
  2. **启动路径**：启动后第一次采样若立刻处于故障态，要在启动诊断里点名（否则用户看到的是"刚打开就不能计时"却不知道原因）。
  3. **正式诊断日志**（P6 既有产出，见 `p7-acceptance` §6.3 第 10 条）：故障态的**进入/清除**各记一条（含 `run_id`、置真点分组名与 `wall_ms`）；**release 的 Windows 子系统没有控制台**，所以这条日志的落点必须与"正式诊断日志"一起交付，不能只 `println!`。
  4. **不做**：不自动重试、不自动清故障、不新增事件名、不改 `refuse_if_faulted` 的拒绝集合。

  **④ 故障态如何被看见（只用真实存在的通道）**：① 既有主通道 = **每个命令返回 `RECOVERY_REQUIRED`**（`AppError::RecoveryRequired`，`src/error.rs:26`；前端 `RECOVERY_REQUIRED` ⇒ 恢复页，P8 计划 Task 2 已登记路由改造）；② P6 的诊断日志（上条）；③ **新增一个只读观察口**（见产出接口）：`AppState::timer_faulted(&self) -> bool`——`Coordinator::is_faulted()`（`:322`）**今天已经存在但只有测试调用**，而 `AppState.coordinator` 私有（`:342`-`:344`）⇒ 服务层/命令层够不着；补这一个投影才能在诊断与将来的状态展示里读到它。**注意不要把 `faulted` 与 P3 的 `attention_overview` 混为一谈**：后者是**持久化事实**的概览，`faulted` 是**内存态**，两者不是一回事。

  **⑤ 交叉引用（写给三份计划的读者）**：本条目只补"检测与看门狗"；**清除**走 `AppState::retry_recovery`（P3 S12；它内部先调 P2 的 `Coordinator::retry_recovery`、再无条件重扫门禁 S1），**触发**是 P8 的第 7 条命令（用户显式重试）。**P6 不得自己调 `retry_recovery`**（否则等于自动重试）。

  **⑥ 测试**：注入一次"提交后重建失败"（P2 已有的注入点）⇒ 断言：进入故障态被**记一次**（不是每拍一条）、`sampling_errors` 口径不变、命令返回 `RECOVERY_REQUIRED`、`timer_faulted()` 为真；调 P3 的 `retry_recovery` 成功后断言故障态清除且只记一条"清除"；panic 注入（§6.7 第 37 条那条）⇒ 断言 `ticks` 停涨这一信号被观察到。

- [ ] **正式 OS 事件接线（锁屏 / 休眠 / 唤醒 / 改时的事件源本身；本计划拥有，不只登记实机步骤）**（**2026-10-04 fix round 2：因 C-C**——原措辞"事件到达后的处理与周期采样走**同一条** `sampling_action` 路径"与"时钟规则按 `system_pause`"**互相矛盾**：`sampling_action`（`:749`）走 `sample_tick`，**永不调 `system_pause`**。真实可行的只有一条：`platform::system_events::spawn` → 组合根 `lib.rs`（`startup` 成功之后）→ `lock_app` → `AppState::system_boundary(Option<ClockSample>)` → `Coordinator::system_pause(db, boundary)`；签名、注入点、**时钟必须同源（`SystemClock` 需 `Clone`）**与判据见「fix round 2」的 C-C）。P7 计划 `:49` 明确「V0.1 里**没有任何平台事件源**（锁屏/休眠/唤醒/改时的监听器不存在）」，P2 计划 `:41`/`:93` 把「正式 OS 事件接线与实机验证」推给 P7/P8，`docs/validation/p7-acceptance.md` 的 **§5.2「只有实机才能验」表末行** 与 **「仍未达成 / 存疑」表**两处，**原**记为「仍无归属」，已于 2026-10-04 订正为「**归属已闭环：实现归 P6、实机结论归 P8**」——本计划接手实现，实机结论仍归 P8 复核（见下）。
  - **依赖 P7 已建立的入口（不新建第二套驱动或启动流程）**：周期触发沿用 `src-tauri/src/platform/scheduler.rs` 的 `Scheduler`（`:40`；`spawn` 在 `:71`，`stop` 在 `:126`）；**（2026-10-04 fix round 4：因 I-2 改写**——原记「事件到达后的处理与周期采样走**同一条** `sampling_action` 路径」**是错的**，见 C-C）事件的归属路径是 `AppState::system_boundary`（→ `Coordinator::system_pause`），**与周期采样共用的是同一把锁、同一份 `AppState`**（`lock_app`，`:399`；`AppState`，`:341`；**单一 `Mutex` 串行边界**），**不是同一个函数**；广播出口沿用 `src-tauri/src/services/events.rs` 的 `Broadcaster`（`:168`；`emit` 在 `:199`；信封 `EventEnvelope` 在 `:52`）。
  - **输入 → 处理 → 输出（2026-10-04 fix round 4：因 I-2 改写**——原记「处理是把事件翻译成一次**同一串行边界内**的采样/异常判定调用（与用户命令、周期采样共用 `sampling_action` 那条路）」**与 C-C 冲突且写不出来**）**：输入是 OS 的锁屏/解锁、休眠/唤醒、系统改时通知——新增一个 `platform/` 叶子模块做 OS 适配（如 `platform/system_events.rs`，名字实施时定并登记 `platform/mod.rs`），**只做适配、不含业务规则**；处理是**组合根注入的回调**在 `lock_app` 之后调 `AppState::system_boundary(Option<ClockSample>)`，由它调 `Coordinator::system_pause`（**这是与 `sampling_action` 并列的第二条路径，不是同一条**：`sampling_action` 管周期心跳/tick，本路径管平台可信边界），**时钟规则一律按 P2 已定义的处理**（`system_pause` 只接受平台已验证、位于上次可信观察与当前采样之间的边界，否则走 `recovering`；唤醒不自动继续）——本计划**不另写判断、不改 P2 的规则**；输出是 `Broadcaster` 广播的 `domain.changed`/`timer.tick` 与数据库里已提交的事实。
  - **失败路径**：平台不支持或监听注册失败时不得 panic、不得静默假装成功；按本 Task 既有口径只记诊断并保持周期采样可用，连续失败要能被上层观察到（`RunningApp::sampling_errors`，`src-tauri/src/services/bootstrap.rs:235`）。
  - **自动化那半边（可断言）（2026-10-04 fix round 4：因 I-2 改写**——原验收口径写「与 `sampling_action` 同一入口，而非第二条路径」，**方向正好相反**）**：用可注入的事件源与时钟断言——事件到达进入**同一把锁的串行边界**（`lock_app` → `AppState::system_boundary`；**入口与 `sampling_action` 不同**，但两者不可能并发进入协调器）；事件与周期采样交错时不产生双重分割；事件源故障后周期采样仍继续；不依赖真实 30 秒、也不依赖真实 OS 通知。
  - **实机验收（不能用单元测试代替，08 §6）归 P8 复核**：锁屏 30 分钟、休眠/唤醒、正反改时的到达延迟与行为，按 `src-tauri/tests/manual-sync.md`（§0 环境表登记机器/系统版本、提交号、验收人/日期；「每一步记**观察到的现象**（数字、时刻、截图路径、SQL 结果），不要只写『通过』」）与 `src-tauri/tests/manual-shell.md`（§0 同样的环境表与读库方法）的写法登记步骤与结论，并对上 `docs/validation/p2-clock-mapping.md` §6/§7 声明的未验证项。
  - **（2026-10-04 修订：因 M3/M4）两条容易漏的接线约束**：① 新叶子模块（如 `platform/system_events.rs`）必须登记进 `platform/mod.rs`（今天 7 个模块，`:11`-`:17`），并满足 `scripts/check-layers.ps1:171` 的 platform 规则（不得出现 `services\|storage\|commands`）——OS 事件只在 `platform/` 适配、翻译与落库全在 `services/`；② 事件的处理**必须与周期采样走同一个维护态判据**（`AppState::sampling_allowed()`，见上条）：否则维护态期间一次锁屏/唤醒通知会绕过维护态直接写库，把"封锁写入"戳出一个洞。事件到达时"取不到锁"是正常的（说明有别的操作在临界区里），**排队等锁后按当时的状态重新判定**，不要用"取锁失败就丢弃/就 panic"这种写法。

## Task 3：事件去重协议的故障路径

文件：services/events.rs、tests/event_protocol.rs。

- [ ] **信封格式与四条去重规则由 P7 建立**（多窗口同步是 P7 的实验目标）。本任务只补广播失败、订阅者异常、重连与 epoch 切换下的边界，不改协议本身。

- [ ] 事件信封字段固定为 `data_epoch`、`event`、`revision`、`at`（Unix 毫秒）、`payload`（00 §5）。**广播按提交顺序**；广播失败**只记诊断，不回滚已提交业务**，也不得返回"事务失败"让用户重复操作。
- [ ] 实现 00 §5 的去重规则。**（2026-10-04 修订：因 C2 同批审计）断言口径改成分层，不再要求"四条各一例"**：
  - **①②③④ 里可达的那三条各要有可失败用例**（改坏实现必须变红）：①「新 `data_epoch` 的权威快照使**全部**业务/计时缓存失效；未知 epoch 的通知**只触发重新握手**，不直接接纳（防迟到通知把缓存切回旧库）」；③「查询响应比已应用/所需版本旧时**不得覆盖**」；④「序号跳号或乱序无法证明一致时，**取新快照**而不是猜」。
  - **②以性质断言（不可达），不要求"能杀掉它的用例"**：②「应用快照后丢弃**同 epoch 且 `revision <=` 快照版本**的通知」在通知路径上被③吞掉——`RevisionGate::on_notification`（`src-tauri/src/services/events.rs:390`）先查②（`:396`）再查③（`:399`），而 `apply_snapshot`（`:350`-`:370`）恒保证 `seen_revision >= applied_revision`（`:357`/`:364`-`:365`），所以②能挡的③一定也挡得住。**这是它的性质，不是覆盖缺口**——P7 已冻结此结论：计划 `2026-10-03-p7-shell-and-ui.md` 的「Task 6a fix round 1」②条（`:711`-`:714`，**以该小节②为准**；"只删闸门②六条全绿"，实测）与 `docs/validation/p7-acceptance.md` §8.5。⇒ 本计划要求一条**性质用例**：驱动一串合法的 `apply_snapshot`/`on_notification` 之后断言不变量 `applied_revision() <= seen_revision()`（两者都有公开读取器，`:341`/`:345`）恒成立，并在注释里写明"因此②不可被黑盒杀掉"。**不要**为了"四条各一例"去构造一个只有②能挡的输入——那要么造不出来，要么就得先破坏 `apply_snapshot` 的不变量。
  - 向量文件 `src/types/__vectors__/revision-gate.json` 里那条「规则②」用例（`:36`-`:44`）**保留**：它仍然是一条有效的行为回归（`<=` 的分寸：等于也丢），只是不充当②的判别力证明。
- [ ] 事件只作缓存失效信号，**不承载业务规则**；纯事件类型与运行时广播分离（01 §2：领域层只能引用前者）。
- [ ] `timer.tick` 与计时查询带 `data_epoch`、`run_id`、`session_id`、`session_version`、`tick_seq`、`as_of` 等字段（P2 已产出）；本计划只负责**广播与去重**，不重算这些值。
- [ ] 测试：**可达的**三条规则各一条可失败用例（①③④，含"新 epoch 使缓存失效"与"未知 epoch 只触发握手"）；② 那条**性质断言**（`applied <= seen` 恒成立，见上）；乱序通知被丢弃；旧查询响应不覆盖新状态；末次通知丢失后靠"至多 30 秒校验 `get_revision`"收敛（F-020）；广播失败不回滚业务。

## Task 4：WAL 一致备份与恢复（新 `data_epoch`）

文件：services/backup.rs、tests/backup_restore.rs。

- [ ] 备份必须是**WAL 一致**的：不能只拷主库文件。主路径是 **`VACUUM INTO '<备份路径>'`**（**裁决 2026-10-04**：同一连接、零新增依赖；bundled SQLite 实测 3.53.2 ⇒ 支持），或按 02 §9 的顺序——**停计时 → 暂停写入 → 关闭连接 → 备份**（恢复前的备份在维护态里做，那时连接本来就要关）。**（二次裁决 2026-10-04）Task 1 的"迁移前备份"是**按需**的**：`user_version < SCHEMA_VERSION` 才做，无需迁移时**不产生备份产物**——所以本任务**不得**依赖"每次启动都有一份新鲜备份"这个假设（恢复演练要的基线由演练自己触发的那次备份提供）。备份包含恢复所需的全部数据。**磁盘库今天是 WAL**（`storage/db.rs:48`-`:52` 打开时强制校验 `journal_mode=wal`），所以"只拷主库文件"必然是错的；`rusqlite` 的 `"backup"` feature **暂不引入**（见 Task 1 第 5 条）。
- [ ] 恢复维护态同时封锁用户/系统写入入口，按 Task 2 的接缝让维护态期间的采样拍不写入。**（2026-10-04 修订：因 C2）原措辞"取消尚未执行的采样并给旧队列标记 epoch/运行代次"里的"旧队列"这个对象不存在**——D6 裁决是单一 `Mutex<AppState>`（`services/bootstrap.rs:341`/`:361`），命令与采样走**同一把锁**（`commands/mod.rs:127` 的 `run_command` → `:140` `lock_app`；采样 `bootstrap.rs:749` → `:750` `lock_app`），全仓 `grep -rn "VecDeque\|mpsc\|queue" src/` = **0 命中**。真实的对象与判据是：**"等待中的采样"就是堵在 `lock_app`（`:399`）上的那一拍**；它在**取锁之后**先问 `AppState::sampling_allowed()`（Task 2 新增），维护态已置位就**丢弃这一拍**（不 heartbeat ⇒ 不写 `interval_checkpoint`、不 tick、不广播），退出维护态后的下一拍照常。所谓"等待正在执行的串行操作结束"就是**取锁成功的那一刻**；不需要给任何东西标代次，也不需要新增队列结构。关闭连接前不得有在途检查点或异常事务。
- [ ] 成功切换后丢弃旧协调器基线、开放区间单调起点与 tick 计数（`Coordinator::tick_seq`，进程内计数、新 run 才重置 —— **2026-10-04 修订：因 C2**，原文写的"tick 队列"不存在，全仓没有队列结构），按新库创建新 run、调用 P3 扫描，再创建协调器并恢复驱动。失败则重开原库，以原 epoch 创建新 run并扫描重建运行态；**两条路径都不得复用维护前的 `Coordinator` 实例**（这是"维护窗口不计入工时"的第二层补偿，理由见 Task 2「恢复时的补偿」第 2 条）；不得沿用切换前 Instant，也不得自动开始工作。失败重建可能将原 running 转为 recovering，UI 明示，原库工时事实不丢失。
- [ ] **（2026-10-04 fix round 2：因 C-A / C-B / I2）恢复的执行骨架与顺序（写死，别再按原措辞猜）**：① 进入维护态 + `take_runtime()`（锁内，短）；② 关连接（旧 `Db` 随 `Runtime` drop）+ 临时路径验证 + 同目录可回滚切换（**不持锁**）；③ 提交或回滚（锁内）：`Db::open` → **同一事务**里 `run_repo::start_run(new_run_id)` **+ `meta::rotate_epoch(&tx)`**（**只在提交路径**；回滚保留原 epoch）→ `scan_recovery(conn, &new_run_id)`（**必须在建好新 run 之后**——`scan_recovery` 按 `run_id <> 当前 run` 判；与 P3 的 **S1**（`AppState::rescan_recovery`，见 P3 计划 §0.3；**按符号定位，不锁行号**）**共用同一函数**，不另写查询）→ `Coordinator::new(clock, new_run_id)` + `establish_anchor(sample)` → `install_runtime(runtime, scan)` → `end_maintenance()` → 广播**新 epoch** 的 `domain.changed`；回滚路径同样重建（重开原库 + 新 run + 重扫 + 新协调器），**不装回原协调器**（旧 `Instant` 基线不可用，计划原文已禁止沿用）。**（2026-10-04 fix round 4：因 C-1）** 两个"重扫"是**两步**：先 `services::recovery::scan_at_startup(&mut db, &new_run_id, now)`（P3 的四类归一，返回 `StartupScanReport`），再 `scan_recovery(conn, &new_run_id)`（门禁）——`scan_recovery` 只是三条只读查询、**不归一**，缺前一步则备份里旧 run 的 `running` 会话会永远停在 `running` 并占着全局 `uq_running_foreground`（`schema_v1.rs:183`，不带 run 过滤）。**`Scheduler` 全程不 stop、不重启**：它的闭包每拍从 `AppState` 读当前运行态，换库后自然对新运行态工作（`scheduler.rs:126` 的 `stop` 不可逆，只在显式退出时调）。完整签名与三段表见「fix round 2」的 **C-B**；`rotate_epoch` 见文末产出接口。
- [ ] **不能覆盖仍打开的 WAL 数据库**（02 §9 原文）。
- [ ] 恢复到**临时路径**验证：完整性、外键、schema 版本（未来版本拒绝；旧版本先备份再迁移），验证通过后才做**同目录可回滚切换**；切换后重开校验。**失败还原原路径并重新打开原库**，且原库必须可用并能重新握手。
- [ ] 恢复完成必须生成**全新 `data_epoch`** 与**新 `run_id`**（F-019 原文），**不能沿用备份里的 epoch**；恢复后的 `revision` 可以低于原库，只在**新 epoch 内**比较（00 §5）。
- [ ] 恢复期间进入维护态：**暂停受理新写入**（`AppState::guard_writable()`，Task 2），被拒的写入返回 `DATA_RESTORE_IN_PROGRESS`（**P6 新增码**，见文末产出接口；2026-10-04 修订：因 M5——这个码今天全仓不存在）。**（2026-10-04 修订：因 C2）原文"取消旧队列/查询/AI 请求（V0.1 只有队列与查询）"里的对象同样不存在**：V0.1 没有队列（`grep` 实测 0 命中），也没有任何 AI 请求（`commands/` 只有 `mod.rs` 与 debug-only 的 `dev.rs`，共 24 条命令）。能"取消"的只有**堵在锁上等待的那些调用**：它们取锁之后按维护态立即失败（写命令）或丢弃（采样拍），因此不会在换库窗口里落进任何一次写入。完成后恢复握手（`data_epoch` 已变，见下条）。
- [ ] **旧 epoch 的所有修改请求返回 `DATA_EPOCH_MISMATCH` 且不写入**；"恢复较低 revision 的备份仍不接受旧响应"（F-019）——这条与 Task 3 的规则 1 是同一个机制，不要在两处各写一份判断。
- [ ] 测试：在途心跳/异常分割与恢复互斥；**维护态期间到达的写命令与采样拍都不能写新库**（各自在取锁后按维护态被拒/丢弃——2026-10-04 修订：因 C2，原文是"旧队列不能写新库"）；成功后驱动仅绑定新运行态；失败后原库重建且不继承旧计时基线；后台采样失败不能绕过维护态重试写入。
- [ ] 格式版号、数据库版号与应用版本**分别记录**（02 §9）。
- [ ] 测试：备份文件可独立打开并通过完整性/外键/版本校验；恢复后 `data_epoch` 与恢复前不同、`run_id` 是新的；旧 epoch 写入被拒；恢复失败时原库可打开、原工时事实保留且重建状态符合恢复规则；恢复期间写入返回 `DATA_RESTORE_IN_PROGRESS`（**新增码**，断言 `code()` 字符串本身，并按总纲 §5 第 8 条断言"被拒的用户命令四件事"：`revision` 不变、无新行、无审计、既有记录字段一致）；备份过程中有并发写时不产生撕裂（用真实的临时文件库，不用内存库）。

## Task 5：本地优先与端到端验收

文件：tests/offline_and_recovery.rs。

- [ ] F-014 的验收口径：**依赖预先安装后**测试与构建通过；**发布应用断网运行全部 V0.1 功能**，无任何报错或降级提示。本任务要在断网条件下跑一遍端到端（拔网线或等价隔离），不要求首次下载依赖也能离线。
- [ ] 端到端：启动 → 计时 → 关掉全部窗口（核心与定时采样继续）→ 重开窗口立即拉快照 → 崩溃 → 重启（走 P3 的四类判定）→ 备份 → 恢复（新 epoch、旧请求被拒）。整条链路的可观测状态都要断言。
- [ ] **人工验收（不能用单元测试代替，08 §6）**：真实拔网线跑一遍 V0.1 功能；手动触发一次备份与恢复，核对恢复后 `data_epoch` 变化、界面拿到的旧请求被拒、`revision` 在新 epoch 内重新开始；强杀后重启核对单实例未重复初始化。记录机器/系统版本与观察结果。
- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
- [ ] 完成门槛：在 `src-tauri` 运行 `cargo fmt --check`、`cargo test`、`cargo clippy --all-targets`，并执行 P1 的分层检查脚本；P1–P5 的测试无回归。
- [ ] 分层自查要覆盖新模块，且**六条规则全过**（2026-10-04 修订：因 M3——`src-tauri/scripts/check-layers.ps1` 今天是**六条**：`:156` commands / `:157` domain / `:158` storage / `:167` services / `:171` platform / `:178` 入口点）：`platform/single_instance.rs` 与 `platform/scheduler.rs`、以及新增的 `platform/system_events.rs` **不得**出现 `services`/`storage`/`commands`（`:171`）；`services/`（含新增的 `services/backup.rs`）不得直接取系统时间（`std::time`/`SystemTime`/`Instant::now`，走 `Clock`）也不得引用 `commands`（`:167`）；入口点规则（`:178`）不许 `lib.rs`/`main.rs` 自己 `Db::open`/`migrate(`/`run_repo::`——Task 1 的备份编排因此**只能**落在 `services/bootstrap.rs` 的 `startup` 里。

---

## 下游接口（供 P8 接入，扩展 P7 既有接线）

- `services/bootstrap.rs` 的启动入口（**P7 建立，本计划扩展**）：**（2026-10-04 修订：因 M2）**`lib.rs` 的 `setup`（`src/lib.rs:192`）里**只有一个启动入口**——`bootstrap::startup`（`:214`，六步顺序全在它内部）；"只调它一个"指的是**不要另排一套启动顺序**，不是说 `setup` 里只有这一件事：启动成功之后它还挂 `app.manage(*running)`（`:224`）、`tray::build`（`:228`）与 `window::spawn_activation_watcher`（`:231`）。⇒ P6 的备份编排、维护态置位/清位**只能加在 `startup` 内部**（`scripts/check-layers.ps1:178` 的入口点规则会机械拦住 `lib.rs` 里的 `Db::open`/`migrate(`/`run_repo::`）。
- 单实例的"第二实例请求唤起"信号；广播订阅接口（含 epoch/revision 去重）。
- 备份/恢复的服务入口与维护态信号；进入维护态时 P8 在既有外壳接入状态展示并停止接受用户命令。
  - **（2026-10-04 修订：因 C1/M5）维护态对 UI 的出口只有两条，别假定更多**：① 写命令被拒时返回的新码 `DATA_RESTORE_IN_PROGRESS`（P8 按 `code` 显示"正在恢复"并禁用入口——发起恢复的那个窗口自己还知道"这次调用还没返回"）；② 维护结束后的 `data_epoch` 变化由既有收敛路径带走（`DATA_EPOCH_MISMATCH` ⇒ `requires_handshake`，见 `src/services/error_response.rs:84`）。**本计划不新造第三个事件名**（`services/events.rs:46`/`:48` 只有 `domain.changed` 与 `timer.tick`，而事件名两侧各定义一份、今天没有机械检查——`p7-acceptance` §6.6 第 35 条）。若 P8 需要"另一个窗口立刻看到维护态"，那是 P6/P8 之间要新定的接口（第三个事件名或一条只读命令），**必须先登记再实现，不得假定它已经存在**。
- 周期采样驱动的启停：由 bootstrap 管理，**界面层（P7/P8）不直接操作定时器**；维护态下由本计划让它**不写入**（`AppState::sampling_allowed()`，Task 2），**不调 `Scheduler::stop()`**（不可逆，调了就不再采样）。

## 依赖清单（P7 已交付、本计划必用的既有入口；2026-10-04 修订：因 M4）

> 这些名字**都已经在今天的树里**（行号实测）。清单的作用是：实施时**不要**新造第二套。

| 入口 | 位置 | 本计划怎么用 |
| --- | --- | --- |
| `AppBoundary` / `AppGuard<'a>` / `lock_app(&SharedApp) -> AppGuard<'_>` / `holds_app_lock(&SharedApp) -> bool` | `services/bootstrap.rs:361` / `:370` / `:399` / `:412` | 维护态的置位/清位、采样拍的判定、写命令门禁**都在 `lock_app` 之后**；`holds_app_lock` 是"持锁调用会互锁"的既有防线（`RunningApp::shutdown` 用它，`:263`） |
| `SharedApp = Arc<AppBoundary>`、`AppState`、`Db`/`Coordinator` 字段私有 | `:351` / `:341` / `:342`-`:344` | 新增 `maintenance` 字段与五个方法都在这一个类型上；**不新增第二把锁** |
| `RunningApp::{shutdown:257, sampling_ticks:230, sampling_errors:235, app:208, broadcaster:212, run_id:216, data_epoch:220, recovery:225}` + `AppState::explicit_exit:546` | `services/bootstrap.rs:194`-`:284`（`explicit_exit` 在 `AppState` 上，`:546`） | 维护态期间要**拒绝**的写入口就是这些；`sampling_ticks`/`sampling_errors` 是采样诊断的既有出口 |
| `ExitReport` | `:612` | P8 复用显式退出时读它（`p7-acceptance` §6.3 第 9 条：退出事务失败的用户可见提示归 P6/P8） |
| `StartupStep` / `StartupProbe` / `NoProbe` / `StartupConfig` / `Startup::{Running, AlreadyRunning}` | `services/bootstrap.rs:83` / `:134` / `:140` / `:152` / `:183` | 启动顺序与备份编排的探针断言（9 条，见 Task 1） |
| `Scheduler::{spawn:71, ticks:112, stop:126}` + `Drop:147` | `platform/scheduler.rs:40` | 只读 `ticks`、只在退出路径 `stop`；**不加 pause/resume** |
| `Coordinator::{faulted:170（初值 :186）, refuse_if_faulted:315, is_faulted:322, retry_recovery:1265}` | `services/timer/coordinator.rs` | 故障态的检测半边归 P6（fix round 3 的新条目）：`is_faulted` 今天**只有测试调用**，P6 补一个 `AppState` 投影（产出接口第 6 条）；`retry_recovery` 是**清除**入口，归 P2 实现 / P3 出口 / P8 触发，**P6 不调** |
| `Broadcaster` / `EventEnvelope` / `EventSink` | `services/events.rs:168` / `:52` / `:113` | 维护态期间不广播；恢复完成后照既有 `domain.changed` 走 |
| `platform/tray.rs`（`TRAY_ID`/`TrayAction`/`MENU_ITEMS`/`build`） | `platform/tray.rs:34`/`:43`/`:88`/`:131` | **（2026-10-04 fix round 4：因 C-2 订正**——原记「维护态下托盘动作同样走命令体…因此自动被 `guard_writable` 挡住」**是错的**）托盘动作**不经 `run_command`**：`spawn_tray_pause`（`commands/mod.rs:1282`）自己 `lock_app`（`:1289`）后调 `tray_pause_impl`（`:1234`），`spawn_tray_quit`（`:1311`）**连锁都不取**、直接调 `tray_quit_impl`（`:1265`）＝ `RunningApp::shutdown()`（`bootstrap.rs:257`-`:283`）。⇒ **`guard_writable` 挡不住这两条**；它们各自必须在取锁后先判维护态（暂停：拒绝；退出：`shutdown()` 里 `begin_exit` 拒绝，见 fix round 4 的 C-2） |
| `platform/sync_lab.rs`（`SYNC_LAB_WINDOW_LABEL`/`open_sync_lab`）+ `capabilities/default.json` 的 `"windows":["main","sync-lab"]` | `platform/sync_lab.rs:39`/`:56`；`capabilities/default.json:1` | 第二个窗口只用于 P7 的同步实验；新增任何窗口/能力都要同步改这份 capabilities（P8 的导出落盘能力同样落在这里，见 P8 Task 3） |
| `platform/window.rs`（`MAIN_WINDOW_LABEL:34`、`plan_activation:57`、`raise_or_rebuild_main:81`、`spawn_activation_watcher:156`） | `platform/window.rs` | 单实例唤起路径的既有实现，Task 1 只补故障路径，不重写 |
| `storage::{db::Db::{open:42, connection:73, connection_mut:78}}, migrations::{SCHEMA_VERSION:21, migrate:24, current_version:29, migrate_with_ddl:38}, meta::{read_meta:21, require_meta:37, init_meta:47, bump_revision:64, rotate_epoch（本计划新增）}}` | `storage/` | 备份编排插在 `Db::open` 与 `migrate` 之间（`bootstrap.rs:664`/`:667`）；**"按需备份"读的就是 `migrations::current_version`（`:29`）**（2026-10-04 fix round 2：因 I4）；`bump_revision` 的"一次业务写恰好一次"口径不变；恢复切库后换身份用 `meta::rotate_epoch`（C-A，本计划新增） |

## 产出接口（P6 新增，供 P8 与后续消费；2026-10-04 修订：因 M5）

1. **新错误码 `DATA_RESTORE_IN_PROGRESS`**，四处联动（缺一处就是新的漂移）：
   - `src/error.rs`：`enum AppError`（`:11`）加变体；`code()`（`:39`）返回该字符串；`message()`（`:50`）给一句面向用户的中文（如"正在恢复数据，请稍候重试。"）；`src/error.rs:109` 的码表用例与 `:161` 的中文用例各加一条（**六个**码互不相同）。
   - `worktrace-web/src/types/ipc.ts`：`ERROR_CODES`（`:87`-`:93`）加第六项、`ErrorCode`（`:94`）自动跟上；注释里"五个稳定码"改成六个。
   - `src-tauri/src/commands/mod.rs:20`/`:44`/`:162` 的三处"五个码"措辞改成六个（`internal_failure` 的语义不变：**没有**"内部错误"码）。
   - `src-tauri/src/services/error_response.rs:84` 的 `requires_handshake` 口径**不变**（维护态**不**要求重新握手：库身份没变）；前端 `commandErrorAction`（`worktrace-web/src/components/commandError.ts:31`）落到 `"notice"` 分支——文案由 Rust 给，UI 用 `code` 决定禁用与提示。
   - **（2026-10-04 fix round 2：因 I5）前端还有两处要跟事实走**：`worktrace-web/src/types/ipc.ts:86` 的注释"`AppError::code()` 的**五个**稳定码"改成六个；`worktrace-web/src/__tests__/ipc.test.ts:117` 的用例名"命令体的 ErrorResponse 原样透传（**五个码**与 authority 都不动）"改成六个（断言本身不用改——它只检查透传）。**`ERROR_CODES` 的第六项加进去不破坏既有门禁**：`types/__tests__/snapshot-contract.test.ts:291` 把它当**取值域**用，只检查"快照里出现过的 `code` ∈ 域"，而 `error_response.json` 快照里的 `VERSION_CONFLICT` 不变。
2. **`AppState` 的维护态方法**（`services/bootstrap.rs`，签名见 Task 2）：`maintenance` / `begin_maintenance` / `end_maintenance` / `sampling_allowed` / `guard_writable`；**外加退出意图（fix round 4 的 C-2）**：`begin_exit(&mut self) -> Result<(), AppError>` + 私有字段 `shutting_down: bool`——**与 `begin_maintenance` 互斥**（各自拒绝对方已置位的状态），`RunningApp::shutdown()`（`bootstrap.rs:257`）的顺序固定为 `holds_app_lock` 检查（`:263`）→ **`begin_exit`** → `sampling.stop()`（`:270`）→ `explicit_exit`（`:546`）。**为什么必须有它**：`stop()` 不可逆，维护态下先停采样再发现"访问器返 `Err`"⇒ 采样永久停、退出事务没跑、进程只能强杀。
   - **`guard_writable` 的调用点与白名单（2026-10-04 fix round 2：因 I1）**：唯一调用点在 `commands/mod.rs` 的 `run_command`（`:140` 取锁之后、`body` 之前）；维护态期间 **24 条命令全部拒绝**（含只读），因为运行态此时**不在手**；**白名单里没有任何 IPC 命令名**——恢复流程自己的 ②③ 两段在同一次后台阻塞调用里连续调服务原语，不重新进 `run_command`。被拒响应：`code = DATA_RESTORE_IN_PROGRESS`、`authority = None`（不读库）、`requires_handshake = false`。
3. **恢复/事件接缝**（`services/bootstrap.rs`，签名见「fix round 2」C-B/C-C）：
   - `pub struct Runtime { pub db: Db, pub coordinator: Coordinator }`；
   - `AppState::{take_runtime(&mut self) -> Result<Runtime, AppError>, install_runtime(&mut self, runtime: Runtime, recovery: RecoveryScan) -> Result<(), AppError>, runtime_present(&self) -> bool, system_boundary(&mut self, boundary: Option<ClockSample>) -> Result<TimerSnapshot, AppError>}`；
   - **三个**访问器改成可失败（**2026-10-04 fix round 4：因 I-4 从四个收到三个**）：`db(&self) -> Result<&Db, AppError>`、`db_mut(&mut self) -> Result<&mut Db, AppError>`、`coordinator(&self) -> Result<&Coordinator, AppError>`；**`recovery(&self) -> &RecoveryScan` 签名与 `recovery` 字段都不动**（它生产零调用，而 P3 已交付测试用 `recovery().requires_recovery()`）。**这是本次唯一的破坏性签名变更，实测 52 处触碰点**：`commands/mod.rs` 19（`db()` 8 + `db_mut()` 11）、`commands/dev.rs` 1、`services/bootstrap.rs` 内部解构 9、`tests/` 23（6 个文件）——**以符号 `grep` 为准**。
   - `system_boundary` 是 `Coordinator::system_pause`（`coordinator.rs:1293`）的**唯一生产入口**。
4. **`storage::meta::rotate_epoch`（新，2026-10-04 fix round 2：因 C-A）**：`pub fn rotate_epoch(tx: &Transaction<'_>) -> Result<String, AppError>`——`UPDATE app_meta SET data_epoch = ?1 WHERE singleton = 1`（新值仍用 `uuid::Uuid::new_v4()`，与 `init_meta:48` 同一形状），返回新值。**分工**：`init_meta`（`:47`）只在**建库**时 INSERT 一次（重复 INSERT 会撞 `app_meta.singleton` 的 PK，`schema_v1.rs:22`）；`bump_revision`（`:64`）只动 `revision`；`rotate_epoch` **只动 `data_epoch`、不动 revision**。**调用约定**：只由恢复/替换库的**提交路径**在**与 `run_repo::start_run` 同一个事务**里调一次；回滚路径不调。**不 bump revision**：新 epoch 内不存在"旧响应"，bump 只会给"一次业务写恰好 +1"多造一条无业务含义的例外。
5. **`services/backup.rs` 的入口**（实施后登记真实签名；原语已由控制器裁决，2026-10-04，**二次裁决同日**）：① **迁移前一致备份** = 在已打开的 `&Connection` 上 `VACUUM INTO '<app_data_dir>/backups/worktrace-f<格式版号>-s<数据库版号>-v<应用版本>-<Unix 毫秒>.db'`，**只在 `user_version < SCHEMA_VERSION` 时执行**（无需迁移 ⇒ 跳过 + 记诊断；首启无库 ⇒ 跳过），保留最近 5 份，失败 ⇒ 启动失败（`STORAGE_ERROR`）；② **恢复流程入口**（临时路径验证 → 可回滚切换 → 新 `data_epoch`/新 `run_id`）；③ **不引入新依赖**（`rusqlite` 的 `"backup"` feature 暂不开，`Cargo.toml` 不动）。
6. **`AppState::timer_faulted(&self) -> bool`（新，2026-10-04 fix round 3）**：把 `Coordinator::is_faulted()`（`coordinator.rs:322`，今天只有测试调用）投影到服务层——`AppState.coordinator` 私有（`:342`-`:344`）⇒ 不补这一个投影，诊断与（将来的）状态展示都读不到故障态。**口径**：运行态不在手（维护态）时返回 `false`；**它只是观察口，不是出口**——清除仍然只能走 P3 的 `AppState::retry_recovery`（S12），P6 不做自动重试。
7. **不新增事件名**（见下游接口那条）。

## 与提前外壳的责任衔接

单实例、最小 bootstrap/run、周期采样、提交广播与显式退出先在 P7 建立并测试。本计划沿用同一文件/入口，完善故障路径、备份恢复及维护态隔离，不重新实现第二套驱动或启动。P7 的开发验证门禁在 P3 恢复扫描接入后升级为正式扫描；最终由 P8 执行完整发布验收。


## 磁盘不足验收补充

- [ ] P2 已用临时文件库 max_page_count 实际触发 SQLITE_FULL，覆盖业务/检查点/恢复事务；本阶段另验证 OS 磁盘耗尽、WAL 写失败和释放空间后的恢复，不能以该核心测试替代平台验收。

## P3 前遗留的消费门槛（2026-10-04）

- [ ] 为 P4 catalog 四个读信封及 P2 提交后单一读事务补第二连接 WAL 并发快照证据；无并发的返回值相等断言不能替代同事务验证。
- [ ] 按 [pre-p3-closure](../../validation/pre-p3-closure.md) 关闭 #6–10/#19/#23/#37/#38：正式日志、退出失败提示、提交后重建故障、采样失活检测、维护/退出互斥及跨进程第二实例验证均为必做，不再仅“可选加固”。
- [ ] 安装新 Runtime 沿用 P3 S1 的 recovery_scan_failed 标记与扫描顺序；扫描失败不得启用业务计时，S12 成功清协调器故障后仍须重扫，不自动重试用户意图。
- [ ] 提交后重建失败的广播缺口用真实故障注入验收：已提交写保留、不重发，authority/重新握手与 get_revision 收敛可验证，不能把 Err 误当事务回滚；P8 同步显示与手册。
- [ ] 发布诊断不依赖控制台；采样 panic/失活应停止可信展示并给出可见故障与显式恢复出口，单调硬故障走新 run。实现后把正式 OS 事件与磁盘/WAL 故障证据交给 P8 实机复核。

## 2026-10-08 开工前收口

P1/P2/P3/P4/P5/P7 核心已交付；P5 异常采样入口回归已补齐，各入口都在独立夹具中首次触发异常。当前已确认问题已修复或明确由 P6/P8 承接，允许开始 P6，不表示平台实机验收完成。

- 访问器以 I-4 为准：db/db_mut/coordinator 改为 Result，recovery 不改；正文旧“四个访问器”已修正。P3/P5 新增调用都须适配，不依据历史调用数漏改。
- 维护态拒绝必须发生在所有 AppState 统计/导出入口采样之前：stats_snapshot/stats_today/export_json/export_weekly_markdown 也可能由采样提交 P2 异常事务，不能按“只读查询”绕过维护隔离。连同重试恢复、系统边界、托盘和周期采样逐条测试；维护期间不得读已取走的运行态或写入任一库。
- P6 Task 5 的服务级恢复/离线链路在本阶段完成；需要 P8 新增 IPC、Today/恢复/导出/备份页面的“全部 V0.1 功能”实机链路，由 P8 接线后最终验收。P6 记录该部分为待 P8，不能据服务测试标平台通过，也不能因 P8 尚未接线而反向把已有服务前置视为未完成。
- 历史行号、2026-10-04 的调用数量与未实施描述不作为当前状态。当前证据及交接见[跨阶段复审](../../validation/cross-stage-review-2026-10-08.md)，P8 接线按当前 13 条新增命令契约执行。

P6 承接维护态/恢复切换、WAL 一致备份、迁移前按需备份、正式 OS 事件源、采样线程故障与诊断、退出失败提示、跨进程单实例与并发证据；这些是阶段交付内容，不是开工前必须已经实现的前置。manual_platform_verified 继续为 false。
