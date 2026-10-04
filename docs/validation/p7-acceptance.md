# P7 验收记录（桌面外壳与核心交互）

日期：2026-10-04。核对对象：P7 计划（`docs/superpowers/plans/2026-10-03-p7-shell-and-ui.md`）
从 `ae9ec00` 到 **`34d84e5`** 的全部改动（Task 0 → Task 6b，**60 个提交**；核对时工作树干净）。
**门禁数字与接口行号都以 `34d84e5` 那棵树为准**；本记录自身的文档提交在它之后（见文末「提交链」）。

**校验基线（本记录自己跑出来的，命令与原始输出摘要见第三节）**：
`cargo test --offline` **464 passed / 0 failed / 1 ignored**（P7 开始时 357）；
前端 `pnpm test` **13 files / 133 tests passed**（P7 开始时 0——仓库当时没有 vitest）；
`pnpm build` EXIT 0。

**依据**：总纲 §5 第 9 条点名的三份权威清单（02 §8 M01/M05、04 §9、06 §4）中与 P7 相关的条目，
加上 §5 第 1–8 条横切约定、§5 第 6 条「人工验收单列」与 P7 计划自身的勾选项。
**方法**：与 P1/P2/P4 的记录相同——逐条把清单条目映射到**可指名的测试或可复现的人工步骤**，
或明确写出它归哪份后续计划。「测试全绿」不等于「清单已覆盖」，「自动化用例绿」更不等于
「UI 已验收」；本记录就是来分辨这三件事的。

---

## 一、结论（先说要紧的）

- **P7 的自动化半边：可以验收。** Rust 464 passed / 0 failed、`fmt --check` EXIT 0、
  `clippy --all-targets -D warnings` 0 条、`check-layers.ps1` 六条规则 PASSED；
  前端 13 files / 133 tests EXIT 0、`pnpm build`（`tsc && vite build`）EXIT 0。
  零新增依赖：`Cargo.lock` 在整轮里**逐字节未变**（实测 `git diff --stat ae9ec00..34d84e5 --
  src-tauri/Cargo.lock` 无输出）；`src-tauri/Cargo.toml` **只改了一行依赖声明**——
  `tauri` 的 feature 列表加 `tray-icon`（+5/−1，无增删依赖行，见 §3.1）；
  `package.json` 只加了 **3 个**钉版本的 devDependency（`vitest@5.0.1`、`jsdom@30.1.1`、
  `@testing-library/react@16.3.3`；`@vitejs/plugin-react` 本来就在）与一条 `test` 脚本（+5/−1）。
- **V0.1 的桌面外壳在单窗口下「能用」**：能捕获、理清、开始/暂停/继续/结束计时、管项目与标签、
  按 GTD 三列表筛任务；关掉全部窗口后进程与托盘留下、周期采样继续（**这一条的设计与决策函数
  有断言，实机结论为空**，见第五节）。
- **真实双窗口实验（06 §4）的器材已经齐了，但实验本身没跑。** 第二个窗口 `sync-lab`、
  三条 dev-only 注入开关、`manual-sync.md` 的七步步骤都已落地；**实机结论一栏是空的**。
  自动化那半边（`dualContextSync.test.ts`，6 条）证明的是「同一套规则在两个各自独立的 JS
  上下文里各自成立」，**证不了真实双 WebView 的广播时序**。
- **外壳人工验收（F-009 / F-011 / F-016）的实机结论同样为空。** 步骤与判据在
  `src-tauri/tests/manual-shell.md` §1–§4，由 P8 执行与复核。
  **本记录不把任何仓储/服务层测试标为「UI 已验收」**（P4 的约定，见第五节）。
- **权威清单里与 P7 相关的条目逐条核对完毕**（§5 第 9 条，见第七节），其余条目明确不属 P7。
- **诚实登记的缺口一处不少**：托盘「完成」项、视图跳转、动态菜单标签、项目详情分页、
  `useRunningTaskId` 的 `mode` 判别、`Drop for Scheduler` 的持锁边界、维护态、统计与导出、
  备份恢复——逐条列在第六节，每条都写明归属与「现在没做的后果」。
- **P7 整分支评审：可宣告完成、0 Critical**（2026-10-04）。评审对**本记录**做了 12 条抽查
  （**8 条一致、4 条记录错**）并指出第六节**漏登 6 条**已登记的遗留；**fix round 1 已逐条
  订正与补登**（4 条见 §3.1/§3.2 与文末「推送状态」，漏登的见 §6.6 第 29–36 条），
  另补写了 `manual-shell.md` §4 的 F-001/F-002 走查步骤。
- **三处「自报证据」在独立评审判定下站不住，已订正并重做**（Task 0 的两条注水证据、
  Task 6a 的三条反向验证声明、Task 1a 的枚举大小写措辞）。逐条见第八节——**这是本记录最该被
  读的一节**：它说明本计划的绿灯是怎么被反复敲过的。

---

## 二、交付总览（Task 0 → 6b，各自绑提交号）

> 逐条的细节、反向验证的原始输出与「本阶段不做」见
> `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task*-report.md`。
> 下面每节只写「交付了什么」与「门禁走到哪」。

### Task 0 平台接线与启动顺序

**提交**：`08be12b`（存储与平台原语、启动顺序、单实例、显式退出 + `IMPLEMENTATION-NOTES.md` §4/§5）、
`38094fc`（周期采样接线与事件协议）、`d64caa9`（fix round 1：C1/I1–I4 + M1/M3/M4）、
`4845d51`（计划措辞订正）。

**交付**：`services/bootstrap.rs`（**唯一**启动入口：单实例 → 开库迁移 → 建 `application_run`
→ 恢复扫描门禁 → 起协调器与周期采样 → 开窗）、`platform/single_instance.rs`（OS 级文件锁，
`std::fs::File::try_lock`，**零新增依赖**）、`storage/run_repo.rs`（`application_run` 的三个原语）、
`platform/scheduler.rs`（窗口全关仍在跑的采样驱动）、`services/events.rs`（事件信封 +
四条去重规则的规范实现 `RevisionGate`）、显式退出入口（先停定时器，再在同一事务里结束
`running`/`paused` 会话、写 `clean_exit_at`、保存 revision、清活动阶段；`recovering` 保留）。

**选型（D6 定稿）**：单一 `Mutex<AppState{db, coordinator}>`，串行性由**类型**保证。
**门禁**：393 passed（357 → 393，+36 条）→ fix round 1 后 **394**。

### Task 1 IPC 接线与 DTO 形状（Rust 半边，1a）

**提交**：`580b4b5`（serde 形状 + `TaskQueryRequest` + 15 份逐字节 IPC 快照）、
`12f0453`（**24 条 IPC 命令** + `lib.rs` 组合根 + `identifier` → `com.worktrace.desktop`）、
`3669b0e`（采样配对证据、分层脚本两处漏洞）、`602ed03`（计划）、
`695cd57`（命令体抽 `*_impl`，24 条逐条覆盖）、`874d9b2`（`WriteOutcome::into_parts()` +
`domain.changed` 发送侧）、`fc1f2f9`（分层门禁四条规则裸词化 + `-CaseSensitive`）、
`487f526`（计划订正）。

**交付**：`commands/mod.rs` 的 24 条命令（每条一对：`#[tauri::command]` 包装 + `*_impl` 命令体）、
请求 DTO（枚举一律**字符串**，非法取值走 `parse_*` 拿稳定错误码）、`announce`（**仅 `Changed`
才广播**，在释放锁之前、所以广播顺序 = 提交顺序）、错误透传（五个码 + `capture_error_response`）。
**门禁**：404 → **431** passed / 0 failed。

### Task 1 前端契约、IPC 客户端与外壳（1b）

**提交**：`2fc8a41`（vitest + jsdom 测试基建 + 手写 `src/types/ipc.ts`）、
`b5ac956`（`src/ipc.ts`：24 条转发 + 错误规范化 + 水位原语 + 事件会话）、
`7d8f3eb`（外壳替换、`greet` 退场）、`865b2f4`（`ipc_commands.rs` 注释订正）、
`8c7e228`（计划 + 三条遗留）、`428e639`（**18 个请求 DTO 各钉一份 JSON 样本**）、
`e0912b0`（快照集合精确比对 + `isUnknownEpoch`）、`a52463f`（计划）。

**交付**：`startEventSession`（**先订阅并暂存 → 握手+快照+应用 → 按序交付**）、
`createFreshnessGate`（`<=`）/`sendVersioned`（`<`）、`toIpcError`（Tauri 反序列化失败兜底成
本地码 `TRANSPORT_ERROR`）、`snapshot-contract.test.ts`（TS 声明 ↔ 15 份快照的键集合与字面量联合）。
**门禁**：Rust **437**、前端 **3 files / 17 → 18 tests** EXIT 0、`build` EXIT 0。

### Task 2 前端状态镜像

**提交**：`e02401a`（`RevisionGate` 的 TS 镜像 + **两侧共读的 41 步协议向量**）、
`32bb3e8`（镜像、hooks、25 条时序用例）、`aa1cb0e`（计划）、
`d72d600` + `9aabf26`（fix round 1：启动缝里的动作、`orderTimer` 前三级、暂存的区分性用例）、
`49beb41` + `0dbc761`（fix round 2：`starting` 带代次，`start→stop→start` 必须重新就绪）。

**交付**：`src/state/domainState.ts`（每个 JS 上下文**唯一**的订阅入口；事件只作缓存失效、
tick 只更新展示值；启动顺序先监听后快照；可见窗口至多每 30 秒 `get_revision` 校验）、
`src/state/hooks.ts`（6 个 hooks）。**门禁**：前端 **42 → 51 → 61 tests** EXIT 0；
Rust 未动（437）。

### Task 3 捕获、理清与计时控制

**提交**：`57d3fb8`（镜像的代次过滤、重新握手/冲突刷新、查询判旧）、
`7574f6c`（R8 错误处置 + 收件箱页 F-001/F-002）、`17b6835`（计时页四个动作）、
`d0abc6e`（接进外壳）、`26cb48f`（计划）；**契约补口**：`a0db688`（`TimerSnapshot` 补
`task_id`/`task_row_version`）、`faf1c78`（计划 + `p4-acceptance.md` §8）、
`1f15b6b`（再补 `task_title`）、`94aa875`（接线：「继续」与标题改用快照的权威字段，
删掉过渡身份）。

**交付**：收件箱页（回车一条 `create_task` 并当场重拉；`start` 只发**一条**命令，
`Inbox → Ready → Doing` 两步跃迁在 Rust 的同一个事务里）、计时页（暂停/继续/结束各一条命令，
**不做本地状态机**，按钮集合只由快照的 `state` 决定）、R8 错误口径（只按 `code` 决定**行为**，
文案只有 `ErrorResponse.message` 一个来源）。**门禁**：前端 **65/76/83/84** 逐提交 EXIT 0
→ 接线后 **92**；Rust 契约 **456**。

### Task 4 托盘与窗口生命周期

**提交**：`b0290c3`（托盘菜单装配 + 主窗生命周期的平台半边）、`dabb79e`（托盘动作接线、
关窗不退出、唤醒接收侧）、`27b8473`（计划）、`4050c8f` + `fcb4d93`（fix round 1：
自死锁防线 `AppBoundary`+`holds_app_lock`、托盘路由表 `tray_dispatch`、验收判据纠错）、
`86c75c0`（复评收尾：判据拆两趟 + 两处文档订正）。

**交付**：`platform/tray.rs`（四项 + 一个禁用预留项「完成（P8 启用）」，`TrayAction` 与
`MENU_ITEMS`）、`platform/window.rs`（`MAIN_WINDOW_LABEL`、`should_prevent_exit`、
`plan_activation`、`raise_or_rebuild_main`、`spawn_activation_watcher`）、
`tauri` 的 `tray-icon` feature、`RunEvent::ExitRequested{code: None} ⇒ prevent_exit()`。
**分层处理**：托盘只交出「点到了什么」，`platform` **不引用** services/storage/commands；
「动作 → 命令体」留在 `commands` 层由组合根接线。
**门禁**：**449 → 452** passed / 0 failed。

### Task 5 Projects 与轻量 GTD 列表

**提交**：`f7a423b`（镜像暴露 `markApplied`）、`1bb275f`（任务页）、`f574659`（项目页）、
`db60a7c`（接进导航）、`44e06b4`（计划）、`2cb637d`（外壳用例标题订正）；
**fix round 1**：`87e810b`（**I1**：页面查询不再推全局水位，改用本视图水位）+ `77deb3d`
（7 条 Minor + 登记）。

**交付**：任务页（Ready/Waiting/Blocked **三个列表各查各的状态**；项目三值选择器 + 情境筛选
进**同一条** `list_tasks`；计数与分页都用服务端的 `total`）、项目页（创建/改名/归档 +
项目详情；归档先确认再提交 epoch 与**列表里那一行的项目版本**）、`src/components/viewWatermark.ts`
（**一个视图一份水位**）。**门禁**：前端 92 → **112** → **133** tests EXIT 0；
`src-tauri/` 零改动。

### Task 6a 双窗口同步实验（自动化半边 + Rust 前置件）

**自动化半边提交**：`b7b9250`（假事件总线 + 假后端 + 三种竞态注入 + 6 条用例）、
`7111c8f`（`manual-sync.md` 步骤与记录模板）、`98f8217`（计划）、
`684d495`（fix round 1：替身跟上本视图水位契约 + 订正三处反向验证声明）、
`ec45527`（实机步骤订正）、`d4e042d`（计划）。
**Rust 前置件提交**：`c22eb8e`（第二个窗口 `sync-lab`，配置与主窗同源只改 label）、
`2660303`（**三条 dev-only 注入开关** + 开窗命令 + 守卫用例）、`34d84e5`（`manual-sync.md` §1
改成「已落地」+ 控制台入口命令）。

**交付**：`src/state/__tests__/{syncLabBus,syncLab,dualContextSync.test}.ts`（两个**各自独立**的
`domainState` 上下文 + 一个共用的假后端，6 条用例：跨上下文失效、末次事件丢失 30 秒内收敛、
旧响应按**本视图水位**丢弃、跳号取新快照、重复通知不再失效、A 暂停 ⇒ B 收敛）；
`platform/sync_lab.rs` + `capabilities/default.json` 的 `windows: ["main","sync-lab"]` +
`commands/dev.rs` 的四条 dev 命令（**两道编译期守卫**，发布构建里不存在）。
**门禁**：前端 112 → **118 → 133** EXIT 0、`build` EXIT 0、dist 里 lab 关键字 0 命中；
Rust **458 → 464** passed / 0 failed。

### Task 6b 外壳人工验收与完成门槛（本记录）

**提交**：见文末「提交链」。**交付**：本记录、计划 checkbox 的逐条勾选与归属、
`manual-sync.md` §2.2 的判据订正（+ §2.6 收尾关窗）、`manual-shell.md` §4 的 F-001/F-002
走查步骤、完成门槛的真跑数字，以及**整分支评审 fix round 1**（4 条记录错订正 + 8 条补登）。
**实机验收结论：空。** 步骤就位（`manual-shell.md` §1–§4、`manual-sync.md` §0–§5），
执行与复核归 P8。

---

## 三、证据与数字（本记录自己跑的，不是抄的）

### 3.1 Rust 完成门槛

命令（Windows，离线；脚本 `.dsh_tmp/p4-gate.ps1`，它就是计划里那条完成门槛的机器化）：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File '\\wsl.localhost\...\.dsh_tmp\p4-gate.ps1'
```

输出摘要（完整日志 `.dsh_tmp/p7t6b-gate-1.log`）：

```
=== 1. cargo fmt (auto-format) ===        fmt exit = 0
=== 2. pull formatted files back ===      0 file(s) pulled back
=== 3. cargo fmt --check ===              fmt-check EXIT = 0
=== 4. cargo test --offline ===           31 个 test result 行全 `ok`
  TOTAL 464 passed, 0 failed
  test EXIT = 0
=== 5. clippy --all-targets --offline -- -D warnings ===   no warnings/errors
  clippy EXIT = 0
=== 6. layer check ===
  commands clean / domain clean / storage clean / services clean / platform clean
  entry points clean
LAYER CHECK PASSED
=== 7. git status --porcelain ===         （空）
```

- **464 passed / 0 failed**（HEAD `34d84e5`）。这是 P7 完成门槛的权威数字。
- **`1 ignored`**：`tests/startup_order.rs:457` 的 `#[ignore = "helper process: 由
  a_killed_lock_holder_releases_the_lock 拉起"]`——它是**被另一个用例当子进程拉起的入口**，
  不是被跳过的覆盖。**订正一处**：Task 6a 前置件报告里写的「464 passed / 0 failed /
  **0 ignored**」不准确，真实是 **1 ignored**（上面那条，P1 起就在，与本轮无关）。
- **六条分层规则**全部 clean（`src-tauri/scripts/check-layers.ps1:156`–`:178`，逐条**裸词 + `-CaseSensitive`**）：
  commands 禁 `\b(storage|rusqlite|Connection)\b`；domain 禁
  `\b(rusqlite|platform|storage|commands|services)\b|std::fs`；storage 禁
  `\b(platform|commands|services)\b`；services 禁 `std::time|SystemTime|Instant::now|\bcommands\b`；
  **platform 禁 `\b(services|storage|commands)\b`**（P7 Task 0 加的第五条，托盘「复用同一批命令」
  正是这条反向边的入口）；**入口点规则**：`src/lib.rs`/`src/main.rs` 禁
  `Db::open|migrate\(|run_repo::`（把「唯一启动入口」从约定变成机器检查）。
  注释行被剔除（`//` 之后不算），四条旧规则在 Task 1 的 fix round 里从 `module::` 形式改成裸词式
  （原先 `use crate::{storage as db};` 能绕过去）。
- 测试数轨迹（**P7 起点 357 → 终点 464**，逐轮的原始输出见各任务报告与 `progress.md`）：
  357 → 393（Task 0）→ 394（Task 0 fix）→ 404（Task 1a）→ 431（Task 1a fix）→ 437（Task 1b）
  → 449（Task 4）→ 452（Task 4 fix）→ **456（`a0db688`，Task 3 契约：`task_id`/`task_row_version`）
  → 457（`1f15b6b`，再补 `task_title` + 一条 `the_snapshot_task_title_tracks_a_rename_and_is_not_cached`）**
  → 458（`c22eb8e`，Task 6a 前置 1/3）→ **464（`2660303`，Task 6a 前置 2/3，本记录复跑一致）**。
  ⚠️ 本记录早先写的「456 → 458」漏了 `1f15b6b` 那一格，把 `task_title` 的那条用例算到了前置件头上——
  **已按 `git show --stat 1f15b6b` 实测订正**。
- **零新增 crate**：`Cargo.lock` 在 `ae9ec00..34d84e5` **逐字节未变**（`git diff --stat` 无输出）。
  `src-tauri/Cargo.toml` **改了一行**：Task 4 启用 `tauri` 已有的 `tray-icon` feature
  （`tauri = { version = "2", features = ["tray-icon"] }`，+5/−1）——可选依赖本来就在解析图里，
  启用 feature 不引入新包，所以 lock 不动。**没有增删任何依赖行。**
  Task 6a 前置件报告另跑过一次性探针 `cargo check --offline --lib --release`（EXIT 0，93 s），
  证明发布档位下 dev 面没有悬空引用——**它不在共享门禁里**，建议归 P8 的发布门禁（R-04）。

### 3.2 前端完成门槛

命令（Windows 侧，仓库目录；**没有跑 `pnpm install`**）：

```powershell
cd 'D:\ProJect\worktrace'; pnpm test; pnpm build
```

输出摘要（完整日志 `.dsh_tmp/p7t6b-web-gate.log`）：

```
=== 1. pnpm test ===   $ vitest run
  src/state/__tests__/revision-protocol.test.ts    (9 tests)
  src/__tests__/ipc.test.ts                        (13 tests)
  src/state/__tests__/domainState.test.ts          (35 tests)
  src/types/__tests__/snapshot-contract.test.ts    (4 tests)
  src/state/__tests__/hooks.test.tsx               (4 tests)
  src/state/__tests__/dualContextSync.test.ts      (6 tests)
  src/components/__tests__/viewWatermark.test.ts   (4 tests)
  src/components/__tests__/timerRequests.test.ts   (3 tests)
  src/pages/__tests__/Timer.test.tsx               (8 tests)
  src/pages/__tests__/Projects.test.tsx            (10 tests)
  src/pages/__tests__/Inbox.test.tsx               (13 tests)
  src/__tests__/App.test.tsx                       (6 tests)
  src/pages/__tests__/Tasks.test.tsx               (18 tests)
   Test Files  13 passed (13)
        Tests  133 passed (133)
  test EXIT = 0
=== 2. pnpm build ===   $ tsc && vite build
  1495 modules transformed.  built in 508ms
  build EXIT = 0
=== 3. git status --porcelain ===   （空）
```

- **13 files / 133 tests**（9+13+35+4+4+6+4+3+8+10+13+6+18 = 133 ✓）。
- `pnpm build` = `tsc && vite build`，**`tsc` 会类型检查 `src/**`**（含 13 个测试文件），
  所以 build EXIT 0 同时是类型门禁。
- 一条既存的非阻断告警：`dist/assets/index-*.js 728.02 kB` 超过 vite 默认的 500 kB 阈值。
  **不是 P7 引入的**（antd 的体积），P7 未做代码分割；登记给 P8 的打包门禁。

### 3.3 各轮独立评审的判定（**结论不是自评的**）

| 任务 | 独立评审结论 | 遗留判定 |
| --- | --- | --- |
| Task 0 | 可进 Task 1，但 **3 条证据注水**（C1 恒真 + I1/I2 不可失败） | 修复轮全部 ADDRESSED；I1 的字面完成由 Task 1 首个提交收口 |
| Task 1a | 达成计划原文，无 Critical，**6 条 Important** | 修复轮 4 提交 + 定向复评「All findings addressed」 |
| Task 1b | 达成计划原文，无 Critical，**2 条 Important（覆盖缺口）** | 修复轮 3 提交 + 复评「全部 ADDRESSED」 |
| Task 2 | 达成 7/7；**26 处变异 / 20 杀 / 6 存活** | 3 条 Important 修复；fix round 2 后再复验通过 |
| Task 3 | 达成计划原文、无 Critical、可收；评审**真跑 vitest** 并自做 **12 处** sabotage | 接线轮两条 Important 复评收口 |
| Task 4 | 代码达成、无 Critical 代码缺陷；1 条 **Critical（文档）**会让 F-009 **假通过** | 修复轮 + 定向复评 C1/I1/I2/M1–M4 全 ADDRESSED |
| Task 5 | 达成计划原文，**1 条 Important（水位语义）** | 修复轮 `87e810b`+`77deb3d` 收口，**133** 条全绿 |
| Task 6a | 自动化半边达成、可以收；**3 条自报反向验证实跑证伪** | 修复轮 `684d495` 订正 + round 2 反向验证 8 处 |
| Task 6a 前置 | 本轮由 Task 6b 复核：数字对得上（464），**一处「0 ignored」需订正** | 见 §3.1 |
| **P7 整分支** | **可宣告完成、0 Critical**（2026-10-04）。评审对**本记录**做了 **12 条抽查：8 条一致、4 条记录错**，并指出第六节**漏登 6 条**已登记的遗留；另有 2 条「只登记不删」类 | **fix round 1 已逐条订正与补登**：4 条记录错见 §3.1 / §3.2 / 文末「推送状态」；6 条漏登 + 2 条顺带登记见 §6.6（第 29–36 条）；并补写了 `manual-shell.md` §4 的 F-001/F-002 走查步骤 |

---

## 四、下游接口登记（供 P8 / 后续消费）

**这一节是 P7 交付的对外面**。全部为真实签名/形状，附 `file:line`。

### 4.1 24 条 IPC 命令（注册表 `src-tauri/src/lib.rs:113`–`:159`）

命令层只做参数反序列化、调用 `services::*`、把 `Result<T, AppError>` 映射成 `ErrorResponse`
（`src-tauri/src/commands/mod.rs:127` 的 `run_command` 是唯一骨架）。**每条命令都只收一个
`request` 参数**（参数名不受 Tauri 的 camelCase 重命名影响，字段就是 Rust 里的 snake_case）。

| # | 命令 | 请求 DTO | 响应 |
| --- | --- | --- | --- |
| 1 | `get_revision` | 无 | `RevisionSnapshot{data_epoch, revision}` |
| 2 | `list_projects` | `ListProjectsRequest{expected_data_epoch, status: Option<String>}` | `ProjectList` |
| 3 | `list_selectable_projects` | `EpochRequest{expected_data_epoch}` | `ProjectList`（只含 active） |
| 4 | `create_project` | `CreateProjectRequest{expected_data_epoch, name}` | `ProjectChange` |
| 5 | `rename_project` | `RenameProjectRequest{+project_id, expected_row_version, name}` | `ProjectChange` |
| 6 | `archive_project` | `ArchiveProjectRequest{+project_id, expected_row_version}` | `ProjectChange` |
| 7 | `list_tags` | `ListTagsRequest{expected_data_epoch, kind: Option<String>}` | `TagList` |
| 8 | `create_tag` | `CreateTagRequest{+kind, name, parent_id: Option<String>}` | `TagChange` |
| 9 | `tags_of_task` | `TaskTagsRequest{expected_data_epoch, task_id}` | `TagList` |
| 10 | `tag_task` | `TaskTagRequest{+task_id, tag_id}` | `TaskTagsChange` |
| 11 | `untag_task` | 同名（`TaskTagRequest`） | `TaskTagsChange` |
| 12 | `list_tasks` | `TaskQueryRequest`（见 §4.2） | `TaskQueryResult` |
| 13 | `create_task` | `CreateTaskRequest{+title, project_id: Option<String>}` | `TaskChange` |
| 14 | `clarify_ready` | `ClarifyReadyRequest{+task_id, expected_row_version}` | `TaskChange` |
| 15 | `set_task_project` | `SetTaskProjectRequest{+task_id, expected_row_version, project}` | `TaskProjectChange` |
| 16 | `plan_for` | `DailyPlanQuery{expected_data_epoch, date, timezone}` | `DailyPlanView` |
| 17 | `add_to_plan` | `PlanMutationRequest{+task_id, date, timezone}` | `DailyPlanChange` |
| 18 | `remove_from_plan` | 同名（`PlanMutationRequest`） | `DailyPlanChange` |
| 19 | `timer_snapshot` | 无 | `TimerSnapshot` |
| 20 | `timer_tick` | 无 | `TimerSnapshot`（`tick_seq` 前进一格） |
| 21 | `start_timer` | `StartTimerRequest`（见 §4.2） | `CommandOutcome` |
| 22 | `pause_timer` | `SessionRequest` | `CommandOutcome` |
| 23 | `resume_timer` | `ResumeRequest` | `CommandOutcome` |
| 24 | `finish_timer` | `SessionRequest` | `CommandOutcome` |

**请求形状的三条硬口径**（照抄，别重新发明）：

1. **枚举一律是字符串**：`mode`（`stopwatch`/`countdown`）、`timer_kind`、`statuses`、
   `kind`（标签类别）、`project.status`。命令体显式过
   `services::timer::parse_session_mode`/`parse_timer_kind`、`TaskStatus::parse`
   （在 `TaskQueryRequest::try_from` 里）后构造服务请求。**不要依赖 serde 的枚举反序列化**：
   它的失败拿不到 `ErrorResponse.code`，会退化成 Tauri 的反序列化错误（`00 §4` 只认那五个码）。
2. **业务查询一律带 `expected_data_epoch`**（请求带来的期望值，服务在同一个读事务里
   `guard_epoch`）；**改既有对象另带 `expected_row_version`**。
3. **`project` 选择器有两个不同形状**：`SetTaskProjectRequest.project` 是**二值**
   `{"bind":"<project_id>"}` / `"clear"`（`services::catalog::ProjectTarget`）；
   `TaskQueryRequest.project` 是**三值** `"any"` / `"none"` / `{"id":"<project_id>"}`。

### 4.2 两个形状复杂的请求

`TaskQueryRequest`（`services::catalog`，`D3` 裁决的 IPC 友好 DTO；字段与校验见计划 Task 1）：

```
{ expected_data_epoch, statuses: string[], project: "any"|"none"|{"id":…},
  context_tag_id: string|null, limit: 1..=100, offset: >=0 }
```

`StartTimerRequest`（`src-tauri/src/commands/mod.rs:327`）：

```
{ expected_data_epoch, task_id, task_expected_version,
  mode: string, timer_kind: string,
  target_duration_ms: number|null,          // 倒计时必须有、正计时必须没有
  expected_interval_ms: number }            // 省略时按本进程采样节拍（只用于识别挂起）
```

`SessionRequest` / `ResumeRequest` 的服务侧形状在
`src-tauri/src/services/timer/coordinator.rs`（`ResumeRequest` **两份版本**：
`task_id`+`task_expected_version` 与 `session_id`+`session_expected_version`）。

### 4.3 事件频道与信封

- **频道名：`worktrace:event`**（`src/ipc.ts:66`；Tauri 侧 `listen<EventEnvelope>(EVENT_CHANNEL)`）。
- **信封五个字段，一个不多一个不少**（`src-tauri/src/services/events.rs:52`）：
  `data_epoch` / `event` / `revision` / `at`（Unix 毫秒）/ `payload`。
- **两个事件名**：`domain.changed`（`services/events.rs:46`，**一次成功业务写一条**）、
  `timer.tick`（`:48`，周期采样发出，**不加 revision**）。
- **`timer.tick` 的 `payload` 就是 `TimerSnapshot` 的 serde 形状**
  （`services/events.rs::timer_tick_payload` 直接序列化该类型，不手抄第二份字段表）；
  `domain.changed` 的 `payload` **就是该命令的响应 DTO**（`commands/mod.rs:186` 的 `announce`）。
- **前端消费口径**：`domain.changed` **只作缓存失效**——不把 `payload` 并进镜像
  （Task 6a 的反向验证 R4' 实测：并进去 ⇒ 场景 1 红）；`timer.tick` 走计时判据链单独处理。
- **收敛路径**：可见窗口至多每 30 秒 `get_revision` 校验一次（`VERIFY_INTERVAL_MS = 30_000`，
  `src/state/domainState.ts:68`）；隐藏窗口在显示前校验。

### 4.4 `TimerSnapshot` 的三个任务字段（P7 Task 3 的契约补口）

`src-tauri/src/services/timer/snapshot.rs`：`task_id: Option<String>`（`:41`）、
`task_row_version: Option<i64>`（`:47`）、`task_title: Option<String>`（`:57`）。

- **无会话时三者一起是 `null`**，且**键始终存在**（不加 `skip_serializing_if`）。
- 有会话时取自会话行 + **每次采样重读任务行**（不缓存：暂停期间改标题会 bump
  `task.row_version`，缓存会让「继续」拿旧版本撞 `VERSION_CONFLICT`）。
- **为什么必须进契约**：24 条命令里没有「按 id 取任务」的读路径 ⇒ 冷启动（重开窗口）
  或托盘暂停之后，前端**构造不出** `resume_timer` 的请求、也拿不到当前任务标题。
  这三个字段是那条回路的唯一来源。
- **前端落点**：`src/components/timerRequests.ts` 的 `buildResumeRequest(snapshot)` 直接读它们；
  可用性判据是「`state === Paused` 且三个字段齐备」。
- 契约同轮落地：三份快照重生成 + `src/types/ipc.ts` + `snapshot-contract.test.ts` 的键集合
  （**两者必须同一次提交**，否则 TS ↔ 快照的键集合断言红）。

### 4.5 `ErrorResponse` 透传口径

`src-tauri/src/error.rs`：`AppError::code()` 只有**五个**码（`:39`–`:45`）——
`RECOVERY_REQUIRED` / `DATA_EPOCH_MISMATCH` / `VERSION_CONFLICT` / `DOMAIN_ERROR` / `STORAGE_ERROR`；
失败响应统一走 `services::error_response::capture_error_response`（在**原事务结束之后、
同一串行边界内**捕获，**不另调 `timer.snapshot` 补版本**）。

载荷形状（`src-tauri/src/error.rs:189`–`:231`）：

```
ErrorResponse { code, message, authority: ErrorAuthority|null, requires_handshake: bool }
ErrorAuthority { data_epoch, revision, records: RecordVersion[] }
RecordVersion  { kind: "task"|"session"|"project"|"tag", id, row_version: number|null }
```

- `records` 与请求**一一对应**、顺序确定（`AuthorityKind` 白名单序 `task→session→project→tag`
  再同 kind 内的请求序）；**前端按 `kind`+`id` 匹配，不按下标**。
- `row_version: null` = **已显式确认不存在**（与「根本没请求」是两件事，字段必须留着）。
- **前端只按 `code` 决定行为**（提示 / 重新握手 / 冲突刷新），文案只有 `message` 一个来源，
  **没有第二份「码 → 文案」表**；未知 `code` 也原样展示（R8）。
- **兜底本地码 `TRANSPORT_ERROR`**（`src/ipc.ts:74`，`toIpcError` `:142`）：Tauri 反序列化
  失败时用，保留原始 message、`requires_handshake = false`。**它不是第六个业务码**。
- `DATA_RESTORE_IN_PROGRESS` 由 P6 引入后再登记为透传项（P7 未实现、未声称）。
- **已知例外（登记在案）**：计时族命令在 `tx.commit()` **之后**才 `rebuild_from_committed`，
  那一步失败会让命令返回 `Err` ⇒ **这一笔已提交的写没有对应的 `domain.changed`**，
  且 `requires_handshake` 为 `false`。收敛靠 30 秒 `get_revision`（它直读 `app_meta`，
  不经协调器）。归 P6 的故障路径范围。

### 4.6 `WriteEnvelope` 的构造点

类型在 **crate 根** `src-tauri/src/envelope.rs:60`（`storage`/`services` 都要用它，
而它们不得依赖 `commands`）：`for_create(expected_data_epoch)`（`:69`）、
`for_update(expected_data_epoch, expected_row_version)`（`:77`）。
**P7 的 IPC 层是第一个生产构造者**，11 个构造点全在 `commands/mod.rs`：
`:452`（create_project）、`:491`（rename_project）、`:531`（archive_project）、
`:602`（create_tag）、`:675`（tag_task）、`:713`（untag_task）、`:785`（create_task）、
`:831`（clarify_ready）、`:871`（set_task_project）、`:933`（add_to_plan）、`:979`（remove_from_plan）。
**集合操作**（`tag_task`/`untag_task`/`add_to_plan`/`remove_from_plan`）用 `for_create`，
语义是「关系操作没有可校验的实体版本」（裁决 R-T3-i / R-T4-e），不是「创建了实体」。
**计时命令不用它**：`start`/`resume` 各有自己的 `*_expected_version` 字段，一个
`Option<i64>` 装不下两个版本。

### 4.7 `viewWatermark`：视图水位契约（P7 Task 5 fix round 1 的 I1 结论）

`src/components/viewWatermark.ts:40`（接口）/`:47`（`createViewWatermark`）：

```ts
interface ViewWatermark {
  isStale(stamp: VersionStamp, requestEpoch: string | null): boolean;  // 响应回来先判旧
  applied(stamp: VersionStamp): void;                                  // 真的上屏之后才推进
}
```

- **一个视图一份水位**：`Tasks.tsx` 的主列表与筛选选项各一份、`Projects.tsx` 的项目列表与
  详情各一份。用法是 `useState(createViewWatermark)[0]`。
- **两半判据**：`epoch` 与发起请求时的 `requestEpoch` 不同 ⇒ 丢弃；同 epoch 内比**本视图
  已上屏过**的那一版，且水位**只前进**。「同版本」不算旧。
- **为什么不能推全局水位**（`domainState.markApplied` / `isStaleResponse` 只给**全量快照**）：
  `TaskQueryResult`/`ProjectList` 是**局部视图**，不满足「已应用的是权威快照」这个前提。
  推平全局水位有两个实测后果（评审探针 A/C）：① 同 `revision` 的 `domain.changed` 被判
  `drop` ⇒ 页面不再重拉；② 30 秒校验比 `seenRevision` 而 `seen` 已被推平 ⇒ **不再 `resync`**
  （状态栏永久停在旧状态，连 `rehandshake()` 也救不回来）。
- **全局水位仍由真正的快照推进**：`get_revision` 与 `timer_snapshot`。
- **辅助查询（标签 / 可选项目）不进水位**：让它推水位会把一条正在飞的主列表响应按「更旧」丢掉。

### 4.8 dev-only 注入命令与其作用域（P7 Task 6a 前置件）

四条，**只在 debug 构建编译与注册**（两道守卫：`commands/mod.rs:110` 的
`#[cfg(debug_assertions)] pub mod dev;` + `src/lib.rs:151`–`:158` 逐条带守卫的注册臂；
`tests/dev_injections.rs` 读源码核对，`cargo check --lib --release` 是发布档位的编译探针）：

| 命令 | 签名 | 作用域 |
| --- | --- | --- |
| `__p7_drop_next_event` | `(kind: String) -> Result<String,String>`（`dev.rs:138`） | 丢**那一次广播本身**（所有窗口都收不到）；**按事件名筛**——不筛的话会被每秒一条的 `timer.tick` 吃掉 |
| `__p7_delay_next_query_ms` | `(command: String, ms: u64) -> Result<String,String>`（`dev.rs:154`） | 只作用于**装开关的那个窗口**（键 = 调用方窗口 label + 命令名）；**先取数据再 sleep**，不跨 `await` 持 `Connection` |
| `__p7_replay_event` | `(revision: i64) -> Result<EventEnvelope,ErrorResponse>`（`dev.rs:179`） | 广播给**所有**窗口；参数是**旧 `revision`**（信封里没有 `event_seq`）；**只读 + 广播**，不写库 |
| `__p7_open_sync_lab` | `() -> Result<String,String>`（`dev.rs:229`） | 开/抬起实验窗口，返回 label |

- **控制台入口**：页面**没有** `window.__TAURI__`（`tauri.conf.json` 没开 `withGlobalTauri`），
  用 `__TAURI_INTERNALS__.invoke(...)`；窗口身份读
  `window.__TAURI_INTERNALS__.metadata.currentWindow.label`。
- **第二个窗口 `sync-lab`**：`platform/sync_lab.rs:39`（label 常量）/`:56`（`open_sync_lab`），
  配置**与主窗同源只改 label**；**不是** `tauri.conf.json` 的静态窗口；
  `capabilities/default.json` 的 `windows` 是 `["main","sync-lab"]`。
- **命令层唯一的侵入**：命令包装多一个 `window: tauri::WebviewWindow` 参数，`run_command`
  多收 `(command, window)` 两个键（发布构建里只用于对齐调用形状）。

### 4.9 前端对外面（P8 直接复用）

- `src/ipc.ts`：24 条转发（`:174`–`:289`）、`EVENT_CHANNEL`（`:66`）、`toIpcError`（`:142`）、
  `createFreshnessGate`（`:444`）、`sendVersioned`（`:536`）、`startEventSession`（`:577`）。
- `src/state/domainState.ts`：`DomainState` 接口（`:103`–`:166`）——
  `subscribe` / `getView` / `subscriberCount` / `start` / `stop` / `rehandshake` / `refresh` /
  `isStaleResponse` / `markApplied`；单例 `domainState`（`:623`）。
- `src/state/hooks.ts`：`useDomainView` / `useDataEpoch` / `useHandshakePhase` /
  `useTimerSnapshot` / `useRunningTaskId` / `useInvalidation`。
- **P8 加页面的姿势**：在 `src/App.tsx` 的挂载区加一个分支 + 一个 `src/pages/*.tsx`，
  用 hooks 读镜像、用 `ipc.ts` 发命令、用 `viewWatermark` 判旧——**不重做外壳**。

---

## 五、验收边界（必须诚实：哪些是自动测试证明的、哪些只有实机才能验）

### 5.1 自动测试**证明了**什么

| 面 | 断言在哪 | 钉住的是什么 |
| --- | --- | --- |
| 启动顺序、单实例、周期采样、显式退出 | `tests/{startup_order,periodic_sampling,exit}.rs` | 启动副作用的**次序**（注入探针）；第二个进程**不打开库、不迁移、不建 `application_run`**；锁持有者被强杀后新进程能拿到锁；**无窗口引用时采样仍被驱动**且空闲零写入；退出事务结束 `running`/`paused` 且写 `clean_exit_at`、`recovering` 行仍在 |
| 事件协议四条去重规则 | `tests/event_protocol.rs` + `src/types/__vectors__`（两侧共读的 41 步向量） | 新 epoch 的权威快照使全部缓存失效、未知 epoch 只触发重新握手；同 epoch 且 `revision <=` 快照版本的通知被丢弃；旧查询响应不覆盖；跳号 ⇒ 取新快照 |
| 24 条命令 | `tests/ipc_commands.rs`（逐条）、`tests/ipc_requests.rs`（18 个请求 DTO 的字段样本）、`tests/ipc_snapshots.rs`（15 份 JSON 快照逐字节） | 每条命令的转发与返回类型；请求字段一个不少；Rust 类型改了忘了改前端类型或快照 ⇒ 红灯 |
| 错误契约 | `tests/error_contract.rs`、`tests/ipc_snapshots.rs`、`tests/revision_gate_vectors.rs` | 五个码与 `message` 全中文、内部标识不外漏；`authority` 的逐条形状与顺序 |
| 前端镜像与外壳 | `src/state/__tests__/*`、`src/pages/__tests__/*`、`src/__tests__/App.test.tsx` | 乱序通知不改变展示、旧 tick 不覆盖新状态、监听先于快照、卸载不漏监听；页面只做展示与转发（业务断言全在 Rust 侧） |
| 双窗口的**规则侧** | `src/state/__tests__/dualContextSync.test.ts`（6 条） | **同一套规则在两个各自独立的 JS 上下文里各自成立**——跨上下文失效与重取、旧响应按本视图水位丢弃、末次事件丢失 30 秒内收敛、跳号取新快照 |
| 托盘与窗口的**决策函数** | `tests/shell_lifecycle.rs`（14 条） | 菜单四项 + 预留禁用项 + id↔动作一一对应；`should_prevent_exit` 两分支；`plan_activation` 四格真值表；主窗 label 与 `tauri.conf.json`/`capabilities` 一致；托盘暂停与 IPC 暂停**效果逐项相等**、没有会话时零写入零广播；托盘退出走显式退出入口 |
| dev 注入开关 | `tests/dev_injections.rs`（6 条） | 注册表里受守卫的恰好是那四条、发布那批一条都没有；开关按事件名筛 / 按窗口与命令名分 / 重播不写库 |

### 5.2 **只有实机才能验**的（当前结论：**空**）

**集成测试进程里没有事件循环、没有窗口对象、没有托盘、也没有第二个 WebView**
（`tauri::test` 的 mock 运行时本轮没有启用）。所以下面这些，`cargo test` 与 `vitest`
**结构上就钉不住**：

| 项 | 步骤在哪 | 现状 |
| --- | --- | --- |
| **F-009 关掉全部窗口后托盘仍可用、计时继续** | `manual-shell.md` §2A | **未跑**。§2A 的真判据是 `interval_checkpoint` 的 `wall_at`/`elapsed_ms` 在关窗 60 秒后**前进 ≥ 20 秒**（该表以 `interval_id` 为主键 upsert，**行数不会涨，必须读列值**）；界面秒数是从 `started_at` 算的，**采样死了也照样「继续走」**，所以秒数本身证明不了任何事 |
| **F-009 重开窗口第一眼就是对的** | `manual-shell.md` §2B | **未跑**。「秒数马上就是对的」没有判别力（每秒一条 tick 的实现在实机上看起来一样） |
| **F-011 真实托盘交互** | `manual-shell.md` §1 | **未跑**。可点项恰好四项 + 一个禁用项；暂停把会话置 `paused` 且 `revision` 恰好 +1；没计时时点暂停零写入 |
| **F-011 托盘「退出」的收尾** | `manual-shell.md` §2C | **未跑**。进程结束、`clean_exit_at` 写入、会话 `finished`、开放区间闭合 |
| **F-016 单实例唤起** | `manual-shell.md` §3 | **未跑**。第二个进程自己退出、`application_run` 不增加、既有实例**重建**或**抬起**主窗 |
| **双窗口竞态 (a)(b)(c)** | `manual-sync.md` §2.1–§2.3 | **未跑**。真实双 WebView 的广播时序、真实 IPC 下「旧响应晚到」长什么样 |
| **跨窗口 30 秒收敛 / 显示前校验** | `manual-sync.md` §2.4–§2.5 | **未跑**；§2.5 的两条还**没有可观察通道**，已标「不可观察 / 存疑」，不得凭感觉判通过 |
| **F-020 界面侧（多窗口一致性）** | `manual-sync.md` §2.0 / §4 | **未跑**。自动化那半边只证明「规则在两个上下文里各自成立」，**真实双 WebView 的展示是否一致仍须真机看** |
| **F-001 / F-002 与计时非法请求的实机走查** | **`manual-shell.md` §4**（Task 6b 补写的七步；判据对照在 `manual-sync.md` §4） | **未跑**。自动用例覆盖了展示与转发（`Inbox.test.tsx` 13 条、`Timer.test.tsx` 8 条），但**外壳人工验收不能用单元测试代替**（总纲 §5 第 6 条 / 08 §6） |
| **平台事件实机验收**（锁屏 / 休眠 / 唤醒 / 改时 / 关窗后采样 / 事件到达延迟） | 登记在计划文末「仍待与归属」 | **未跑、且仍无归属**：P7 只登记步骤，结论由实机跑出、P8 复核。`docs/validation/p2-clock-mapping.md` §6/§7 已声明这些**未验证、不得当成已验证** |
| **多入口开发/打包路径 + Windows 打包验证**（00 §7） | 登记在计划文末 | **未做**，归 P8，与 R-04 的发布产物门禁一起 |

### 5.3 「不得把仓储/服务层测试标为『UI 已验收』」——本记录的落法

P4 的约定在 P7 继续有效，并且**这一条正是 P7 最容易违反的地方**（P7 第一次有了真实界面）。
本记录的处理是**在每一处显式分开**：

1. **第五节的两张表就是那条分界**：上表（自动化证明了什么）里的每一条都指名到**测试文件**；
   下表（只有实机才能验）里的每一条都指名到**人工步骤的节号**，并统一标注「未跑」。
   **没有任何一条实机项被写成「已通过」**。
2. **Task 6a 的单测没有被当成实机结论**：`dualContextSync.test.ts` 的 6 条绿，
   证明的是「规则在两个上下文里各自成立」；文档（`manual-sync.md` 头部、
   `manual-shell.md` §5）与计划里都写明「**真实双 WebView 的广播时序仍须真机跑**」。
3. **决策函数的断言没有被当成行为验收**：`tests/shell_lifecycle.rs` 钉住的是菜单映射、
   `should_prevent_exit`、`plan_activation`、托盘暂停/退出的**库内证据**与 label 一致性——
   **钉不住「真实托盘图标/菜单交互」与「关窗后仍在计时」**。这句写在
   `manual-shell.md` 的头部与计划「仍待与归属」里。
4. **判据本身被审过两轮**：Task 4 的评审 C1 抓到 `manual-shell.md` 原先要求的
   「`revision` 只按心跳前进」是错的（**心跳不加 revision**），会让 F-009 **假通过**；
   复评 N1 又抓到 §2 同一趟塞了两条**互斥**判据（会让 F-009 **假不通过**）。两处都改了。
   **一条会假通过或假不通过的判据，与没有判据一样糟**——这是本计划在人工验收上留下的最实用的一条经验。

---

## 六、已登记但未做（P8 / 后续），逐条

> 这一节是「P7 到底没做什么」的权威入口。每条都写：现在没做的**后果**、归属、以及
> 有没有留下接线的接缝。

### 6.1 依赖 P3（恢复确认）的

| # | 项 | 后果 | 归属 |
| --- | --- | --- | --- |
| 1 | **托盘「完成」项** | 菜单里是一个**禁用项**「完成（P8 启用）」（id `tray.finish_reserved`，`action: None`）——看得见、点了没反应，且**不存在一条通往尚未存在服务的路径** | P8（P3 的 `transition_task` 接入后启用） |
| 2 | **F-003 的完整联动**（完成 / 取消 / Blocked / Waiting / reopen） | P7 只在三处展示状态（捕获、理清 Ready、开始计时）；`transition_task` 尚不存在，**P7 无对应入口可验**。「即使出现也要被 Rust 拒绝」这条要求保留，由 P8 执行 | P8 |
| 3 | **恢复确认页与待确认区间展示** | 不做。P7 的 Today 是两条命令的组合（`plan_for` + `snapshot`），不含确认工时聚合 | P8/P5 |

### 6.2 依赖 P5（统计）的

| # | 项 | 后果 | 归属 |
| --- | --- | --- | --- |
| 4 | **统计视图与导出**（F-010 统计半边、F-018） | 无入口 | P8（P5 接入） |
| 5 | **「确认人工工时 / 待确认时间」的展示** | 无入口；`task_change` 现有三种 JSON 形状，P5 取完成项**必须按形状过滤**（集中登记在 `storage/mod.rs` 的模块文档） | P8/P5 |

### 6.3 依赖 P6（平台收口）的

| # | 项 | 后果 | 归属 |
| --- | --- | --- | --- |
| 6 | **维护态**（隔离、错误码 `DATA_RESTORE_IN_PROGRESS`、托盘在维护态下禁用） | 错误码全仓 0 命中；P7 只登记透传项，**未实现未声称** | P6（引入）→ P8（展示） |
| 7 | **备份与恢复** | 无入口 | P6 Task 4 |
| 8 | **故障路径硬化**（单实例/启动的锁异常释放、通知丢失） | Task 0 只做了开发验证库门禁 | P6 Task 1 |
| 9 | **退出事务失败的用户可见提示** | 当前只记诊断并**以非零码退出**（事务已回滚、库一致；这一次 run 以「没有 `clean_exit_at`」结束正是恢复扫描的输入 F-015） | P6/P8 |
| 10 | **正式诊断日志** | 本轮所有诊断走 `println!`/`eprintln!`，而 release 的 Windows 子系统**没有控制台**（`src/main.rs` 的 `windows_subsystem`）⇒ 输出被丢弃。验收必须用 `pnpm tauri dev` 或重定向 | P6 |

### 6.4 P8 自己的

| # | 项 | 后果 | 归属 |
| --- | --- | --- | --- |
| 11 | **视图跳转**（托盘「快速捕获」跳到捕获输入框） | P7 只保证**抬窗** | P8（需要前端的视图/路由） |
| 12 | **动态菜单标签**（把当前任务标题做成菜单项） | 菜单项是静态文案 | P8 |
| 13 | **项目详情分页** | 详情**只列第一页**（服务端上限 100），条数超过一页时文案会说出来、**不静默截断**；两条判据已就位 | P8 候选 |
| 14 | **`src/state/hooks.ts` 的 `useRunningTaskId` 判别不了 `mode`** | Rust 的口径是 `FOREGROUND AND running`；V0.1 只有前台会话 ⇒ **当前不可达**，但语义不完整 | P3/P8 同步 |
| 15 | **「多入口开发/打包路径」与 Windows 打包验证**（00 §7） | 未做 | P8（与 R-04 发布产物门禁一起） |
| 16 | **发布档位编译探针进正式门禁** | `cargo check --offline --lib --release` 本轮是**手工一次性证据**，没塞进共享门禁（避免给其他人加 93 s） | P8 的 R-04 门禁 |
| 17 | **Inbox 页的标签入口**（`tag_task`/`untag_task` 的界面） | 无入口：任务页只有**查询侧**的情境筛选 | P8 |
| 18 | **归档/完成状态的批量操作** | V0.1 无此入口 | V0.1 之外 |

### 6.5 已知的工程边界（不影响本阶段验收，但要传给 P8）

| # | 项 | 事实 |
| --- | --- | --- |
| 19 | **`Drop for Scheduler` 不查 `holds_app_lock`**（`platform/scheduler.rs:147`–`:158`） | 「同一线程既持有串行边界的 guard、又丢弃自己拥有的 `Scheduler`」能绕过 `RunningApp::shutdown` 的自死锁防线。**当前生产路径不可达**（`Scheduler` 只被 Tauri 托管、按进程生命周期析构）。**P8 若新增「拥有并显式丢弃 `RunningApp`」的退出路径，必须先放掉那把锁** |
| 20 | **请求 DTO 的 TS→Rust 方向仍只能人工对齐** | Rust→TS 有机械联系（`tests/ipc_requests.rs` 的 18 份样本），但 `src/types/ipc.ts` 的**请求**接口没有东西核对它：把 `expected_row_version` 写成 `expected_revision` 不会红，要到运行期退化成 `TRANSPORT_ERROR` 才暴露 |
| 21 | **TS ↔ 快照检查盖不住的两件事** | ① **枚举取值域两个方向都盖不住**（快照里只有样例值）；② **键集合断言只比 `keyof`**，不含值类型与可选性（`pending_ms: number\|null` 写成 `string\|null` 不会红）。边界是「键与已出现的取值」，不是「完整类型等价」——这是「不上 DTO 生成器」（D1 裁决）的已知代价 |
| 22 | **包装层 `targets` 未覆盖** | `run_command` 的 `targets`（决定 `authority.records`）只活在 `#[tauri::command]` 包装里，要观测它得有 Tauri 运行时。`tests/error_contract.rs` 覆盖的是**机制**、`tests/ipc_snapshots.rs` 钉的是一份**样例**——两者都不是「逐条命令的 targets」的断言 |
| 23 | **计时族命令「提交后重建失败」那一笔不发 `domain.changed`** | 见 §4.5。收敛靠 30 秒 `get_revision`；协调器解锁要等下一次成功重建（P3/P6 范围） |
| 24 | **`manual-sync.md` §2.5 的两条不可观察** | 「隐藏期间不再每 30 秒轮询」「恢复后立刻有一次 `get_revision`」**没有计数出口** ⇒ 已标「不可观察 / 存疑」，不得凭感觉判通过。要给结论得先给注入开关/日志加校验计数 |
| 25 | **`@mui/material` 与 `@emotion/*` 是模板遗留死依赖** | `src/App.tsx` 未使用、`package.json` 里仍在。**只登记，不在 P7 删**（删依赖属清理任务且需用户确认） |
| 26 | **前端镜像 `worktrace-web/` 不是测试环境** | 镜像曾缺 15 份快照 fixture，直接跑 `pnpm test` 会**假 4 红**（控制器已补齐，现 0 缺失）。评审/实施一律用 `git archive` 完整导出到工作区外再装依赖；**绝不在仓库目录跑 `pnpm install`**（会把 Windows 侧装的 `node_modules` 重链成 linux 原生二进制） |
| 27 | **`dist` 的 728 kB 单 chunk 告警** | 既存（antd 体积），P7 未做代码分割 |
| 28 | **术语「上下文」vs「情境」** | 用户文案按 `99-glossary.zh.md` §5 用「上下文」，04 F-005 与计划写「情境」。**仍待用户拍**；若改，`zh_kind`、`error_contract` 的逐字断言、`tags.rs` 两条 needle 共 5 处一起改 |

### 6.6 整分支评审补登（2026-10-04 fix round 1，共 8 条）

> 前 5 条是**计划与 P4/Task 0 账本里已经登记、但本记录初版漏登**的；后 3 条是同一轮评审
> 顺手点出的「只登记、不删」类。全部**只登记**，本轮不动代码。

| # | 项 | 事实 | 归属 |
| --- | --- | --- | --- |
| 29 | **计时判据链的「同一条规则两处实现」仍是待定项** | 前端 `orderTimer`（`src/state/domainState.ts`）是**五级**判据（`data_epoch` → `run_id` → `session_id` → `session_version` → `tick_seq`）；Rust 的 `Coordinator::is_stale_tick`（`src-tauri/src/services/timer/coordinator.rs:368`）只判 `session_id` + `row_version`，**不是同一个函数、没有共享向量、Rust 侧也没有对应断言**，而且它在**生产路径上零调用**（`src/` 里只在 `events.rs:377` 的注释里被提到，唯一的调用方是 `tests/timer_snapshot.rs:212`–`:220`）。计划（「遗留与边界（Task 2 fix round 1 登记）」一节）明写「**要不要给计时判据链也造一份两侧共读的向量，是一个待定项**：要么把 `is_stale_tick` 扩成同一条链，要么承认它是展示侧独有、在 P8 的实机验收里覆盖」 | **待定**：计划明写是待定项，本记录不替它拍板。**P8 的双窗口实机验收正落在这一格** |
| 30 | **「`applied == None` 时的通知」两侧都没覆盖** | 协议向量（`src/types/__vectors__/revision-gate.json`）刻意不含这一格，而 **Rust 侧 `tests/event_protocol.rs` 也没覆盖**：该文件里 **3 个用例、共 12 处 `gate.on_notification`**（实测 `grep -c`；计划里写作「三处」，那指的是**用例数**，不是调用点数），**每一处都跟在 `apply_snapshot` 之后** ⇒ `epoch == None ⇒ Rehandshake` 这条分支目前**两侧都只有「前端的不判未知」这一半**有断言 | P8（或给两侧各补一例；本条与第 29 条同源，都是「规则镜像」的边界） |
| 31 | **`SessionAttention` 零调用** | `src-tauri/src/domain/session.rs:179` 的 `SessionAttention`（`InvariantBroken`/`NeedsReview`）**全仓零调用**（`grep -rn SessionAttention src/` 只命中定义处）。它是 P3 四类判定的候选类型，Task 0 的门禁**没有**用它（避免为尚未存在的服务造临时实现），Task 0 报告 §「遗留」第 7 条登记过。按纪律**只登记不删** | P3（四类判定接入时决定去留） |
| 32 | **`AuthorityTarget::Deserialize` 零调用，且注释的理由与事实相反** | `src-tauri/src/error.rs:246` 给 `AuthorityTarget` 派生了 `Deserialize`，其上方注释（`:244`–`:245`）说「`Deserialize` 是给 P7 的 IPC 用的：没有它，命令层只好再写一份『字符串 → 种类』的 match」——**而 P7 的 IPC 路径一次都没用它**：`commands/mod.rs:177`–`:178` 的 `target()` 只调 `AuthorityTarget::new`，`kind` 在命令层是**枚举字面量**，从来没有从字符串解析过。P4 账本记的是「`AuthorityTarget::Deserialize`/`new` 生产零调用（**P7 定了再收**）」——P7 只收掉了 `new` 那一半 | P8：要么删掉 `Deserialize` 与那段理由，要么等真的出现字符串入口再用（**注释该改，因为它现在说的是假的**） |
| 33 | **两个文件该拆了** | `src-tauri/src/services/catalog.rs` **771 行**、`src-tauri/src/commands/mod.rs` **1338 行**（实测 `wc -l`）。P4 账本与 Task 5 报告都记过「`catalog.rs` 已四类职责，**P7 再加任务命令时建议拆 `services/tasks.rs`**」——而 P7 真的加了 24 条 IPC 命令与全部请求 DTO ⇒ `commands/mod.rs` 现在是「命令骨架 + 请求 DTO + 24 条包装/命令体 + 托盘入口」一肩挑，**P8 还要往这里加命令** | P8（下一个加命令的人先拆；拆法见 Task 5 报告的建议） |
| 34 | **`BroadcastDiagnostics::dropped` 只有测试消费、实机没有出口** | `src-tauri/src/services/events.rs:130`–`:134` 的注释说这个计数是「为了**让实机实验**（`tests/manual-sync.md` §2.1/§2.3.1）能分辨『开关没生效』与『规则没成立』」——但 `diagnostics()`（`:237`）**没有任何生产消费者**，唯一读 `dropped` 的是 `tests/dev_injections.rs:338`/`:356`。⇒ **实机操作者读不到这个数**，注释里那句用途目前兑现不了；`manual-sync.md` §2.2 的「注入没生效就记『注入未生效』」同样只能靠现象判断 | P8（给 DevTools 加一条读 `diagnostics()` 的 dev 命令，或把注释改成事实） |
| 35 | **跨语言的三对常量没有任何机械检查** | 事件频道名与两个事件名在两侧**各定义一份**：`worktrace:event` 在 `src-tauri/src/lib.rs:74`（Rust `const`）与 `src/ipc.ts:66`（TS `export const`）；`domain.changed` / `timer.tick` 在 `src-tauri/src/services/events.rs:46`/`:48` 与 `src/types/ipc.ts:97`/`:98`。**单侧改名是静默的**：Rust 换了频道名 ⇒ 前端一个事件都收不到；TS 换了事件名 ⇒ 镜像永远不失效——两侧都不会红。现有的 TS↔快照检查（15 份）**不含事件信封**，向量文件 `revision-gate.json` 里也没有这些名字 | P8（**不要现在造生成器**：一条「读两侧源码比对这三个字面量」的用例就够，与 `snapshot-contract.test.ts` 同一种做法） |
| 36 | **死组件与残留文件（只登记，不删）** | ① `src/components/{FloatingInput,FloatingSelect,DockviewDemo}.tsx`（+同名 `.css`）**没有任何代码引用**：`grep -rn <名字> --include=*.ts --include=*.tsx src/` 排除自身后为 **0 命中**；文档/计划里提到它们的只有 `src/components/README.md`（仍在教怎么 import，**与现状不符**）与 `DockviewDemo` 在 P8 计划/ADR 里（作为「不进发布产物」的对象，见 ②）。② `DockviewDemo` 有特殊身份：ADR-012 的 R-04 要求它**不进发布产物**，P8 计划里已有一条构建后检查（含「故意 import 一次确认门禁会红」的反向验证）。③ 仓库根有两个被 `.gitignore` 的 `*.log` 忽略的 stray：`.p7t3-baseline-test.log`、`.p7t2-test-raw.log`（`git status --ignored` 实测）。按工作区纪律「发现无关文件**只报告不删**」 | P8（`DockviewDemo` 与 R-04 一起处理；其余是清理任务，**删文件/删依赖都需用户确认**） |

---

## 七、对照总纲 §5 第 9 条的权威清单（逐条：已核对 / 不适用）

### 7.1 02 §8 M01/M05 必测案例（14 条）

| # | 条目 | P7 侧结论 |
| --- | --- | --- |
| 10 | **运行/暂停时退出** | **已核对**。`tests/exit.rs` + `tests/shell_lifecycle.rs` 的托盘退出用例：先停定时器、同一事务里结束 `running`/`paused`、写 `clean_exit_at`、`recovering` 保留。**实机收尾步骤**在 `manual-shell.md` §2C（未跑） |
| 9 | **强杀后十秒内重启** | **已核对（P7 补的那一半）**。`tests/startup_order.rs::a_killed_lock_holder_releases_the_lock`（拉起的 helper 进程就是那条 `#[ignore]` 的入口）；第二实例的行为在 `manual-shell.md` §3（未跑） |
| 其余 12 条 | 暂停后重启 / 暂停直接结束 / 恢复前台冲突 / 并发 start / 跨午夜含暂停 / 空范围 / 同名根标签 / 历史工时重叠 / 前后改系统时间 / 旧版本编辑冲突 / 区间修正后报表重算 / 待确认排除与显式确认 | **不适用**：属 P1/P2/P4/P5 的计时、恢复与统计范畴（各自记录已逐条核对）。P7 未触碰这些链路，只把既有命令接到 IPC |

### 7.2 04 §9 必做集成用例（6 条）

| # | 条目 | P7 侧结论 |
| --- | --- | --- |
| 4 | **旧版本修改** | **已核对（IPC 半边）**。`VERSION_CONFLICT` 原样透传、带 `authority.records`；前端只按 `code` 决定**行为**（冲突刷新 + 展示 Rust 的 `message`），**不重发**那条命令（`Projects.test.tsx` 有断言） |
| 3 | **重复提交** | **已核对（幂等半边）**。幂等重复 ⇒ `Changed` 位为假 ⇒ **不广播 `domain.changed`**；`tests/ipc_commands.rs` 逐条覆盖 |
| 2 | **磁盘不足** | **不适用（P7）**。审计写失败整体回滚由 P4 覆盖；真正的 OS/磁盘故障归 P6 |
| 1/5/6 | 迁移失败 / 跨午夜暂停 / 统计修正 | **不适用**：归 P1/P2 与 P5 |

### 7.3 06 §4 实现前技术验证（4 项）

| # | 条目 | P7 侧结论 |
| --- | --- | --- |
| 3 | **双窗口同步** | **自动化半边已完成、实机未跑**。器材（`sync-lab` + 三条注入开关）已落地；6 条用例证明规则在两个上下文里各自成立；**真实双 WebView 的广播时序仍须真机跑**（`manual-sync.md` §2.1–§2.5，**结论为空**） |
| 4 | **HUD / 构建** | **不适用（P7）**。HUD 属 V0.1b（F-012/F-013，本计划不做）；「多入口开发/打包路径」与 Windows 打包验证归 P8 |
| 1/2 | DB 执行边界 / 单调与墙钟映射 | **不适用**：P1/P2 已完成并记录 |

### 7.4 横切约定 §5 第 1–8 条

| 条 | 结论 |
| --- | --- |
| 1 分层与依赖方向 | **已核对并加强**：`check-layers.ps1` 从四条扩到**六条**（P7 加了 platform 反向边与入口点规则），四条旧规则改成**裸词式 + `-CaseSensitive`**（原先 `use crate::{storage as db};` 能绕）。六条全 PASSED |
| 2 工具链固定 | **已遵守**：全部 `cargo` 命令只在 Windows 侧跑；同一轮只用一套工具链 |
| 3 错误契约 | **已核对**：五个码原样透传、`message` 全中文、`Domain.detail` 只用于生成用户文案、`Storage.detail` 只进诊断 |
| 4 写事务信封 | **已核对**：一次业务写恰好 `revision + 1`；心跳与 tick **不加**；被拒命令零变化（P1/P2/P4 的用例 + IPC 逐条覆盖） |
| 5 测试策略 | **已遵守**：时间经 `FakeClock` 注入；库用 `tempfile`；前端时间经 `vi.useFakeTimers` |
| 6 **人工验收单列** | **已遵守，且是本记录的重点**：F-009/F-011/F-016/F-020 与双窗口同步都给了可复现的手工步骤（`manual-shell.md` / `manual-sync.md`），**一处也没拿单测冒充**；**没有把仓储/服务层测试标为「UI 已验收」**（见 §5.3） |
| 7 改动纪律 | **已遵守**：`greet` 命令与模板页按计划在 P7 **明确处理**（`7d8f3eb`）；`@mui`/`@emotion` 死依赖只登记不删。**计时内核的语义整轮零改动**——`src-tauri/src/services/timer/**` 只动了**只读的契约面与入口**（实测 `git diff --stat ae9ec00..HEAD`：`coordinator.rs` +103、`snapshot.rs` +40）：`snapshot.rs` 加 `task_id`/`task_row_version`/`task_title` 三个字段（每次采样重读任务行）、`coordinator.rs` 加 `parse_session_mode`/`parse_timer_kind`（命令层不 import `domain` 的替代）与几处 serde derive、一个 `wall_ms()` 取时钟的入口。**状态机、事务边界、心跳与采样语义一行未改**（P2 的既有用例全部原样通过） |
| 8 断言口径 | **已核对，且抓到并修掉多类恒真/不可失败的断言**：Task 0 的 `total_changes()` 恒 0（C1）、顺序 emit 到 Vec（I1）、测试自己先 commit 的「不回滚」（I2）；Task 1b 的 `>= 15` 快照计数；Task 6a 的三条反向验证声明。**这些是真实缺陷，不是形式**（见第八节） |

---

## 八、独立评审与订正记录（含被证伪的项）

> 这一节写「哪些绿灯曾经是假的、怎么被敲掉的」。故意写全。

### 8.1 Task 0：3 条证据注水（C1 + I1 + I2）

| 项 | 原声明 | 实测 | 订正 |
| --- | --- | --- | --- |
| **C1（Critical）** | 「空闲不写库」由 `SELECT total_changes()` 证明 | **恒真**：用**新连接**取值 ⇒ 连接级计数器永远是 0（评审用 python sqlite3 实测） | 改成 App 连接上的写入探针；报告与 `IMPLEMENTATION-NOTES.md:125` 的引用一并订正 |
| **I1** | 「广播按提交顺序」 | **不可失败**：只是顺序 emit 到 `Vec`，任何实现都过 | 改成真实路径 + 双写者。**字面完成**：复评指出新用例仍有证伪力缺口（`any(committed_checkpoint >= 30_000)` 会被之后任意一拍满足），随 Task 1 首个提交收口 |
| **I2** | 「广播失败不回滚业务」 | **不可失败**：事务由测试自己先 `commit` | 合并到真实采样调用点做真用例 |

另有 I3（`AppState` 字段 `pub` ⇒ 恢复门禁可被绕过，改成私有 + 只读访问器）与
I4（分层门禁**没有 platform 规则**——而 Task 4 的托盘正是 platform→services 反向边的出现处）。

### 8.2 Task 1a：两处**计划自身的硬错误**

- 计划 `:71` 写「状态枚举在 JSON 里是**统一小写**」——**错的**：`TaskStatus`（`"Ready"`/`"Inbox"`）
  与 `TagKind`（`"Context"`）是 **PascalCase**，与 schema 的 CHECK 逐字一致；只有
  `ProjectStatus`/`SessionState`/`TimerKind` 是小写。**照计划写 TS 联合类型就是永远不命中的
  静默 bug**。已订正（`487f526`）。
- 计划 `:69` 要求给 `StartRequest` 加 `Deserialize`——实现**刻意没做**（那条路径拿不到
  `ErrorResponse.code`），计划改成实际口径。

### 8.3 Task 4：一条会让 F-009 **假通过**的判据（C1）

`manual-shell.md` 原先要求观察「`revision` 只按心跳前进」——**心跳不加 revision**，
操作者会看到 revision 不动而误记「通过」；而且**界面秒数是从 `started_at` 算出来的**，
采样线程死了也照样「继续走」。⇒ 存在 F-009 **假通过**的风险。
改为两条真判据：① `revision` **不变**（正确现象）；② `interval_checkpoint` 的
`wall_at`/`elapsed_ms` 在关窗 60 秒后**前进 ≥ 20 秒**（该表以 `interval_id` 为主键 upsert，
行数不会涨，**必须读列值**）。复评又抓到反向的一条（N1：§2 同一趟塞了两条**互斥**判据，
会让 F-009 **假不通过**）⇒ §2 拆成 2A/2B/2C 三趟。

### 8.4 Task 5：一条由本任务自己引入的 Important（I1）

`f7a423b` 把此前「无生产调用者」的 `markApplied` 接上 ⇒ 页面把**过滤+分页后的查询响应**
当权威快照推**全局**水位。评审探针实测两个后果（同 revision 的 `domain.changed` 被判 `drop`；
30 秒校验失去判据 ⇒ 不再 `resync`，连 `rehandshake()` 也救不回来）。
**修法**：新增 `viewWatermark.ts`（一个视图一份水位），两页不再调 `domainState.markApplied`。
`87e810b` + `77deb3d`，**133** 条全绿。

### 8.5 Task 6a：**3 条自报的反向验证被实跑证伪**（不是 2 条）

| 原声明 | 实测 | 订正后 |
| --- | --- | --- |
| 「`VERIFY_INTERVAL_MS` 改 60_000 ⇒ 场景 2 第 30 秒那条红」 | **假**：用例 advance 的是 import 进来的常量**本身**，纯相对计时（60_000 / 1_000 / 31_000 六条全绿） | 本文件只钉**路径**；常量的**数值**由 `domainState.test.ts` 的 `expect(VERIFY_INTERVAL_MS).toBe(30_000)` 钉住 |
| 「摘掉闸门② ⇒ 场景 4 后半段红」 | **假**：`applySnapshot` 恒有 `seen >= applied.revision` ⇒ ②在 `onNotification` 里**不可达**（只删② 六条全绿） | 写明「闸门②不可达」是它的**性质，不是覆盖缺口**；迟到的补号由**闸门③**挡下 |
| 「摘掉闸门③ ⇒ 场景 4 附加红」 | **假（旧契约下）**：那时页面把全局水位推到第 6 版 ⇒ ②先兜住了 | 改写用例让 `applied` 停在第 5 版，**只有③能挡**（只删③ ⇒ 红） |

订正后做了 **round 2 反向验证：8 处变异（6 红 + 2 预期绿 + 控制组）**，全部在
`git archive HEAD` 的导出副本上做、工作树全程干净。

> **给本记录读者的提醒**：上面那三条声明**当时是自报的**（作者按自己的理解写「把实现改坏成
> 什么样它会红」），独立评审逐条实跑才证伪。**三条全部不成立**，不是部分——订正之后由
> round 2 的 8 处变异重新背书（R5'/R7'/R8' 分别对应上面三行）。

### 8.6 本记录自己复核出的两处订正

1. **Task 6a 前置件报告的「0 ignored」不准确**：真实是 `464 passed / 0 failed / **1 ignored**`
   （`tests/startup_order.rs:457` 的 helper 进程入口，P1 起就在）。本记录 §3.1 已写明。
2. **`manual-shell.md` §5 与 `manual-sync.md` 的「前置尚未落地」已过期**：Task 6a 的两个前置件
   在 `c22eb8e`/`2660303`/`34d84e5` 落地后，这两处引用块仍写「尚未落地」。本轮一并订正
   （只改状态与指向，**步骤、判据、记录表一字未动**）。

### 8.7 整分支评审对本记录的抽查与 fix round 1（2026-10-04）

**评审结论：P7 可宣告完成、0 Critical。** 它对本记录做了 **12 条抽查**：**8 条一致、
4 条记录错**；另指出第六节**漏登 6 条**已在计划/P4/Task 0 账本里登记的遗留。
**4 条记录错逐条如下**（都已订正，订正处见括号）：

| # | 本记录原先写的 | 实测 | 根因 |
| --- | --- | --- | --- |
| 1 | `package.json` 加了 **4 个**钉版本 devDependency | **3 个**（`vitest@5.0.1`、`jsdom@30.1.1`、`@testing-library/react@16.3.3`；`@vitejs/plugin-react` 本来就在）：`git diff ae9ec00..HEAD -- package.json` 只有 3 行新增依赖 | 把「计划里要装的 4 个包」当成了「新增的 4 个」——计划那一条写的确实是 4 个包名（§1、§3.1 已订正） |
| 2 | 测试数轨迹「**456 → 458**（前置 1/3）」 | **456（`a0db688`）→ 457（`1f15b6b`，补 `task_title` + 一条用例）→ 458（`c22eb8e`）**：`git show --stat 1f15b6b` 里 `tests/timer_snapshot.rs` 新增了 `the_snapshot_task_title_tracks_a_rename_and_is_not_cached` | 把 `task_title` 那一格算到了前置件头上（§3.1 已订正） |
| 3 | `scripts/check-layers.ps1:156`–`:178` | **`src-tauri/scripts/check-layers.ps1:156`–`:178`**（行号本身是对的，路径漏了 `src-tauri/` 前缀） | 沿用计划里的简写（§3.1 已订正） |
| 4 | 「推送状态」段整段过期：`origin/dev == 94aa875`、17 提交未 push、本轮不 push；且「勾上 **61** 条」 | `origin/dev == 67cf055`，`ae9ec00..67cf055` 的 **64 个提交全部已推送**（写完那段之后确实推过一次）；计划里现在是 **67 条已勾 / 9 条未勾**（本轮**新勾 61 条**，HEAD 时是 6 勾 / 70 未勾） | 写的是**开工时**的快照，之后没回头复核；「61」是**新勾数**、被写成了总数（文末「推送状态」与 §2 Task 6b 已订正；「61 = 新勾数」这一层在 `p7-task6b-report.md` 里写明） |

**漏登的 6 条 + 顺带 2 条**已补进 §6.6（第 29–36 条）。另外按评审的点名，
**补写了 `manual-shell.md` §4「F-001 / F-002 与计时非法请求（Task 6b 走查）」**
（七步 + 判据 + 「怎么算不通过」）——原先本记录把这项的「步骤在哪」指向 `manual-shell.md` §5，
而 §5 只有 checkbox、没有步骤，属于**指向了一个不存在的东西**。
`manual-sync.md` 也补了 §2.6「收尾：关掉实验窗口 `sync-lab`」（否则 `manual-shell.md` §2
的「关掉全部窗口」前提会看到两个窗），以及计划 Task 4 那条过期的「11 条」→ **14 条**。

**这一节的教训**：评审抽查的 4 条里，**3 条是「写了就不再看」的事实性陈述**（版本轨迹、
路径、推送状态），1 条是**把计划里的目标值当成了实测值**。验收记录的价值全在「可复核」上，
所以每一条数字都该带**能复跑的命令或提交号**——fix round 1 之后本记录的每处订正都附了命令。

---

## 九、与规划的关系：P7 完成意味着什么、P8 的入口条件

### 9.1 V0.1 现在「能用」到哪一步

按本计划的边界（计划开头的「范围（诚实声明）」）：

- **能用**：捕获（F-001）→ 理清（F-002 的轻量半边）→ 开始/暂停/继续/结束计时（F-003 的三处展示）；
  项目管理（F-004）；基础标签与情境筛选（F-005 的查询侧）；Tasks 页的 GTD 三列表 + 分页；
  **关掉全部窗口后进程与托盘留下、周期采样继续**（F-009 的设计与决策函数有断言，
  **实机待跑**）；单实例与唤起（F-016 的收发两侧都接上了，**实机待跑**）。
- **还不能交付**：统计（F-010 统计半边、F-018）、恢复确认（F-003/F-015 的界面）、导出——
  它们的界面在 **P8**；维护态、备份恢复在 **P6**；HUD 与全局热键属 **V0.1b**。
- **一句话**：V0.1 的**外壳**有了，**业务闭环还差「恢复」与「统计」两段界面**。

### 9.2 P8 的入口条件（P7 交出去的东西）

**P8 可以直接站在这些上面，不需要重做外壳**：

1. **外壳与导航**：`src/App.tsx` 的导航四项 + 挂载区 `switch`；加页面 = 加一个分支 + 一个
   `src/pages/*.tsx`（页面无 props、外壳不持有跨页面业务状态、仍不引路由）。
2. **状态镜像**：`domainState` 单例 + 6 个 hooks；事件只作缓存失效、tick 只更新展示值。
3. **判旧的正确姿势**：页面查询用 `viewWatermark`（**一个视图一份水位**），
   **不要**调 `domainState.markApplied` / `isStaleResponse`（那两个只给全量快照）。
4. **24 条命令与错误口径**：`src/ipc.ts` 的转发 + R8（只按 `code` 决定行为，
   文案只有 `message`、未知 `code` 原样展示）。
5. **托盘**：菜单装配在 `platform/tray.rs`，新动作只要加一个 `TrayAction` + 一行路由
   （`commands` 层的命令体复用）；「完成」项已留位（禁用态）。
6. **显式退出入口**：`RunningApp::shutdown()` 是唯一入口。**P8 新增退出路径时必须显式调它**
   （`RunEvent::Exit` 不兜底；`Drop for Scheduler` 不查持锁，见 §6.5 第 19 条）。
7. **实机验收的待办清单**：`manual-shell.md` §1–§4（F-011 / F-009 / F-016 / F-001·F-002）与
   `manual-sync.md` §2.1–§2.5（双窗口竞态 + 时序），**结论一栏仍是空的**——
   P8 执行、填表、复核；`manual-sync.md` §3 与 §5 的记录表已经排好。
8. **门禁**：`.dsh_tmp/p4-gate.ps1`（Rust 六条规则）+ Windows 侧 `pnpm test` / `pnpm build`；
   P8 还要补上 **R-04 的发布产物门禁**（`cargo check --lib --release` 探针、
   `DockviewDemo` 不进产物、打包路径验证）。
9. **两条不许忘的口径**：① 人工验收记录要能对上**具体版本与机器**（提交号 + 库路径 + 机器）；
   ② **不得把仓储/服务层测试标为「UI 已验收」**（§5.3）。

---

## 提交链（P7：`ae9ec00` → `67cf055`）

| 阶段 | 提交 |
| --- | --- |
| 计划修订 | `09e2cb3` |
| Task 0 | `08be12b` / `38094fc` / `d64caa9` / `4845d51`（计划） |
| Task 1a | `580b4b5` / `12f0453` / `3669b0e` / `602ed03`（计划）/ `695cd57` / `874d9b2` / `fc1f2f9` / `487f526`（计划） |
| Task 1b | `2fc8a41` / `b5ac956` / `7d8f3eb` / `865b2f4` / `8c7e228`（计划）/ `428e639` / `e0912b0` / `a52463f`（计划） |
| Task 2 | `e02401a` / `32bb3e8` / `aa1cb0e`（计划）/ `d72d600` / `9aabf26`（计划）/ `49beb41` / `0dbc761`（计划） |
| Task 4 | `b0290c3` / `dabb79e` / `27b8473`（计划）/ `4050c8f` / `fcb4d93`（计划）/ `86c75c0` |
| Task 3 | `57d3fb8` / `7574f6c` / `17b6835` / `d0abc6e` / `26cb48f`（计划）；契约 `a0db688` / `faf1c78` / `1f15b6b` / `94aa875` |
| Task 5 | `f7a423b` / `1bb275f` / `f574659` / `db60a7c` / `44e06b4`（计划）/ `2cb637d`；fix round 1 `87e810b` / `77deb3d` |
| Task 6a | `b7b9250` / `7111c8f` / `98f8217`（计划）；fix round 1 `684d495` / `ec45527` / `d4e042d`（计划） |
| Task 6a 前置件 | `c22eb8e` / `2660303` / `34d84e5` |
| Task 6b | `44d63f1`（两处文档同步）/ `d3eac98`（完成门槛与计划勾选）/ `75afe46`（本记录）/ `67cf055`（自查订正）/ fix round 1 的文档提交（整分支评审的 4 条记录错 + 8 条补登 + `manual-shell.md` §4）；细节见 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task6b-report.md` |

**推送状态**（2026-10-04 整分支评审 fix round 1 时实测）：`origin/dev == 67cf055` ——
`ae9ec00..67cf055` 的 **64 个提交全部已推送**（含 Task 6b 自己的 4 个文档提交
`44d63f1` / `d3eac98` / `75afe46` / `67cf055`）。**`67cf055` 之后的提交尚未推送**：
`e60192d`（另一位实施者的 Task 5 fix round 2）与本记录 fix round 1 的文档提交在其后。
（本节早先写的是「`origin/dev == 94aa875`、17 个提交未 push、本轮按纪律不 push」——
那是 Task 6b **开工时**的快照；写完那段之后确实推送过一次，**该段已按实测订正**。）

## 仍未达成 / 存疑（一句话索引）

| 项 | 状态 |
| --- | --- |
| F-009 / F-011 / F-016 的实机结论 | **空**（步骤就位，P8 执行） |
| 双窗口同步的实机结论（06 §4） | **空**（器材就位，P8 执行） |
| 平台事件实机验收（锁屏/休眠/唤醒/改时/关窗后采样/到达延迟） | **空且仍无归属**：P7 登记步骤、P8 复核 |
| 「多入口开发/打包路径」与 Windows 打包验证 | **未做**，归 P8 |
| `manual-sync.md` §2.5 的两条 | **不可观察 / 存疑**（缺计数出口） |
| 托盘「完成」/ 视图跳转 / 动态菜单标签 / 项目详情分页 / `mode` 判别 / 维护态 / 统计 / 备份恢复 | **已登记未做**，归属见第六节 |
| 术语「上下文」vs「情境」 | **仍待用户拍**（5 处联动） |
