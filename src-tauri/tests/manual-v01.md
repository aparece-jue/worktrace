# V0.1 端到端人工验收记录（P8 Task 5；计划 §「Task 5：V0.1 端到端人工验收」）

> **这份文档是给人照着做的**：每一步都写清前置状态、操作、要抄下来的证据与**怎么算通过 /
> 怎么算不通过**。每一步记**观察到的现象**（界面文案逐字抄、SQL 结果、日志行、截图路径），
> 不要只写「通过」——没有现象的「通过」在验收记录里等于没做（08 §6）。
>
> **`manual_platform_verified` 在实机跑完之前保持 `false`。** 本文档里的全部条目
> **当前都没有实测结论**（P8 只做到"清单可执行"这一步）。要把它翻成 `true`，需要：
> ① 本文档 §0–§11 逐条跑完并填上现象；② §12 的 500 ppm 跨机器校准在 **≥2 台不同机器**上
> 跑完；③ §13 的结论段由验收人签字。**任何一条没跑，就保持 `false`**，不得据"自动化全绿"
> 宣称平台行为已验收（`docs/validation/p7-acceptance.md` §5.2 与
> `docs/validation/p8-acceptance.md` §4 是同一条口径）。
>
> **自动化那半边在别处**：`cargo test --offline`（含 50 个集成测试文件）、
> 前端 `pnpm test`、`pnpm check:bundle`。本文档只补**只有真机/真窗口/真时钟才能验**的部分。
>
> 相关文档：[`manual-shell.md`](manual-shell.md)（托盘、关窗、单实例的 P7 步骤）、
> [`manual-sync.md`](manual-sync.md)（双窗口同步实验的步骤与记录模板）、
> [`p8-acceptance.md`](../../docs/validation/p8-acceptance.md)（P8 验收记录骨架）。
> 三份互相引用，**不重复**：P7 那两份的条目本轮只需复核结论，步骤以那两份为准。

---

## 0. 环境（每次验收先填这张表；跨机器校准时**每台机器一张**）

| 项 | 值 |
| --- | --- |
| 机器名 / CPU | |
| Windows 版本（`winver` 抄全文，如 `Windows 11 24H2 (OS 内部版本 26100.1742)`） | |
| 提交（`git rev-parse --short HEAD`） | |
| 构建与启动方式（`pnpm tauri dev` / `pnpm tauri build` + 安装包 / `target\release\worktrace.exe`） | |
| 库路径（`%APPDATA%\com.worktrace.desktop\worktrace.db`） | |
| **本次验收的库是否为干净库**（是 / 否；判据见 §0.3） | |
| 验收人 / 日期 | |
| 本文档跑的是第几趟（口述不给结论；每趟一个副本） | |

### 0.1 两条证据通道（先分清，不然后面会抄错地方）

| 通道 | 在哪儿看 | 什么时候有 |
| --- | --- | --- |
| **控制台输出**（`println!` / `eprintln!`：启动六步、托盘动作、唤醒接收、退出结果、配对检查） | 跑 `pnpm tauri dev` 的那个终端 | **只有 dev**。release 的 Windows 子系统没有控制台（`src-tauri/src/main.rs` 第一行 `windows_subsystem = "windows"`），这些行会被丢弃 |
| **正式诊断日志**（`services/bootstrap.rs` 的 `Diagnostics`：启动每一步、维护态、故障态、`tray.*` 拒绝、`system_events.*`、`startup.failed`） | `%APPDATA%\com.worktrace.desktop\worktrace.log` | **dev 与 release 都有**（release 里这是唯一通道） |

```powershell
# 看诊断日志（应用运行中也能读；PowerShell 5.1 用 -Wait 就是 tail -f）
Get-Content "$env:APPDATA\com.worktrace.desktop\worktrace.log" -Tail 40
Get-Content "$env:APPDATA\com.worktrace.desktop\worktrace.log" -Wait
```

**判据要用"哪条通道"说清**：要求"日志里有一行 `event=…`"的，指的是 `worktrace.log`；
要求"控制台有一行 `[worktrace] tray: …`"的，只有 dev 跑法看得到。

### 0.2 读库（应用运行中也能读，用另一条连接）

```powershell
sqlite3 "$env:APPDATA\com.worktrace.desktop\worktrace.db"
```

**先取本次 run 的 id**（下面每一节都用它，别写字面量 `<当前 run>`）：

```sql
SELECT id FROM application_run ORDER BY started_at DESC LIMIT 1;
```

其余常用查询（后文按名字引用）：

```sql
-- 业务版本：每次成功业务写 +1；心跳与只读不加
SELECT revision FROM app_meta WHERE singleton = 1;
-- 当前这次 run 的会话
SELECT id, task_id, state, started_at, ended_at, needs_review
  FROM work_session WHERE run_id = '<run id>' ORDER BY started_at;
-- 工作区间（工时唯一依据）
SELECT i.id, i.session_id, i.started_at, i.ended_at, i.duration_ms, i.needs_review, i.voided_at
  FROM work_interval i JOIN work_session s ON s.id = i.session_id
 WHERE s.run_id = '<run id>' ORDER BY i.started_at;
```

### 0.3 开跑前确认库是干净的（否则 §2/§3 的「开始计时」会直接失败）

启动时 `scan_recovery` 按 **`run_id <> 当前 run`** 查三类事实：未结束会话
（`state NOT IN ('finished','discarded')`）、待确认区间（`needs_review = 1 AND voided_at IS NULL`）、
不变量损坏（`running` 带待确认区间 / `running` 无开放区间 / 非 `running` 却有开放区间）。
命中任一 ⇒ `start`/`resume` 被拒成 `RECOVERY_REQUIRED`（界面文案：「存在待确认的计时记录，
请先处理恢复再继续。」）。**这是设计**：P3 的恢复入口就是为它准备的（§5 用它）。

```sql
SELECT 'unfinished_session', id, state, run_id FROM work_session
 WHERE run_id <> '<run id>' AND state NOT IN ('finished','discarded');
SELECT 'pending_interval', i.id FROM work_interval i JOIN work_session s ON s.id = i.session_id
 WHERE i.needs_review = 1 AND i.voided_at IS NULL AND s.run_id <> '<run id>';
```

**有输出就是脏库**：先走一遍 §5（恢复页）把它们处理掉，或换一个**新库路径**
（把 `worktrace.db` 改名后重启，应用会重新建库并跑迁移）。把结论填进 §0 的表格。

### 0.4 怎么看 `domain.changed`（多处判据靠它）

在**目标窗口**按 `F12` 打开 DevTools，在 Console 里粘一次：

```js
await __TAURI_INTERNALS__.invoke("plugin:event|listen", {
  event: "worktrace:event", target: { kind: "Any" },
  handler: __TAURI_INTERNALS__.transformCallback((e) =>
    console.log("[event]", e.payload.event, "rev", e.payload.revision)),
});
```

之后每来一条通知打一行「事件名 + revision」。**不想开控制台就用库里的 `revision` 兜底**：
每条 `domain.changed` 都带写入后的 `revision`，而 `timer.tick` 不加 `revision`——所以
「`revision` 不变」与「没有第二条 `domain.changed`」在多数步骤里是同一件事。

### 0.5 托盘视图跳转的事件（§8 用）

托盘的两条视图动作会先发一条**窗口定向事件**给主窗（只发给 `main`，不走
`worktrace:event` 那条广播），再抬窗：

```js
await __TAURI_INTERNALS__.invoke("plugin:event|listen", {
  event: "worktrace:tray-view", target: { kind: "Any" },
  handler: __TAURI_INTERNALS__.transformCallback((e) => console.log("[tray-view]", e.payload)),
});
```

期望看到的载荷：点「当前任务」⇒ `{page: "timer", focus: false}`；点「快速捕获」⇒
`{page: "inbox", focus: true}`。

---

## 1. 发布前必须通过：R-04 发布产物门禁（Task 4 收尾必办项，**原文收纳**）

> 来源：`.superpowers/sdd/2026-10-03-p8-stats-recovery-export-ui/task-4-report.md` **§6**
> （原文照录，未改一字；当时的评审把它登记为「Important 追踪项：交付物 #3 只落在报告里」，
> 本节就是它的收纳点）。

```markdown
### 发布前必须通过：R-04 发布产物门禁

- [ ] `pnpm check:bundle` 退出码 **0**（= `pnpm build && powershell -NoProfile -ExecutionPolicy Bypass -File src-tauri/scripts/check-bundle.ps1`，脚本 `src-tauri/scripts/check-bundle.ps1`，纯 ASCII）。
      它一次证明两件事：
      1. 仓库根 `dist/` 里搜不到 `DockviewDemo` / dockview 的标记（`dockview`、`dock-workspace`、`dock-toolbar`、`dock-panel`，忽略大小写），命中即失败并打印 `file:行` 与片段；
      2. 发布档 `cargo check --offline --lib --release` 通过（Ruling P8-5 并入；首次冷跑约 21–93s）。
- [ ] **反例（必须成立）**：`dist/` 不存在、或存在但没有 `index.html` ⇒ 脚本**报错退出 1**，绝不当作"检查通过"；`dist/` 路径从**仓库根**解析，从 `src-tauri/` 等任意 cwd 调用都能找到（实测 `A2`/`A3`/`A4`）。
- [ ] 口径提醒：`dockview` / `dockview-react` **仍留在 `package.json`**（固定布局下的常态）不是失败；判的是"有没有被打进产物"。
- [ ] 实跑留档：`.dsh_tmp/t4-final-a1-pnpm-check-bundle.txt`（通过）、`.dsh_tmp/t4-final-b2-rendered-pnpm.txt`（故意接线后失败，8 组命中）、`.dsh_tmp/t4-final-a3-dist-missing.txt`（`dist/` 缺失 ⇒ 1）、`.dsh_tmp/t4-final-a2-cwd-src-tauri.txt`（异 cwd ⇒ 0）。
```

**同一份报告里的实跑输出（原文收纳）**：

```
$ pnpm build && powershell -NoProfile -ExecutionPolicy Bypass -File src-tauri/scripts/check-bundle.ps1
$ tsc && vite build
dist/index.html                   0.38 kB | gzip:   0.30 kB
dist/assets/index-MV07_ain.css    1.58 kB | gzip:   0.49 kB
dist/assets/index--xRzCwU0.js   831.00 kB | gzip: 261.28 kB
built in 380ms
=== release bundle gate (R-04) ===
repo root : D:\ProJect\worktrace
bundle dir: D:\ProJect\worktrace\dist
bundle    : 3 file(s), newest write 2026-10-10 08:45:39 UTC
=== 1. scan the bundle for DockviewDemo / dockview markers ===
PASS: none of the 5 markers appears in 3 bundle file(s)
=== 2. release-profile compile probe (cargo check --lib --release) ===
cargo check --offline --lib --release --manifest-path D:\ProJect\worktrace\src-tauri\Cargo.toml
    Finished `release` profile [optimized] target(s) in 0.57s
PASS: cargo check --offline --lib --release exited 0 after 1s
PASS: release bundle gate - no DockviewDemo/dockview marker in dist/ and the release-profile check is green
  pnpm check:bundle EXIT = 0
```

反向验证那一次（故意把 `DockviewDemo` 接进入口并渲染）的关键几行（全文见 `t4-final-b2-rendered-pnpm.txt`）：

```
FAIL: 8 marker hit group(s) in the release bundle
  HIT marker='dockview' file=dist\assets\index-BhbXejgQ.js:328 occurrences=67
  HIT marker='dockview' file=dist\assets\index-DJbwBlS7.css:1 occurrences=305
FAIL: DockviewDemo / dockview must not be part of the release bundle (R-04).
  b2-rendered pnpm check:bundle EXIT = 1
```

`dist/` 缺失那一次（`t4-final-a3-dist-missing.txt`）：

```
FAIL: bundle directory not found: D:\ProJect\worktrace\dist
FAIL: run pnpm build first. A missing dist/ is an error, not a pass.
  A3 EXIT = 1                       <-- 不是"检查通过"
```

### 1.1 本轮怎么跑（照着做就能打勾）

1. 在**仓库根**（`D:\ProJect\worktrace`）执行 `pnpm check:bundle`，抄下退出码与末三行。
   - [ ] 退出码 **0**，且输出里有 `PASS: none of the 5 markers appears in N bundle file(s)` 与
         `PASS: cargo check --offline --lib --release exited 0`
   - [ ] 输出里 `repo root` 是仓库根、`bundle dir` 是仓库根的 `dist\`
2. **反例一（`dist/` 缺失必须非零退出）**：

   ```powershell
   Rename-Item D:\ProJect\worktrace\dist dist.manual-hidden
   powershell -NoProfile -ExecutionPolicy Bypass -File D:\ProJect\worktrace\src-tauri\scripts\check-bundle.ps1
   $LASTEXITCODE
   Rename-Item D:\ProJect\worktrace\dist.manual-hidden dist
   ```

   - [ ] 退出码 **1**，且有一行 `FAIL: bundle directory not found: …`（**不得**当成"检查通过"）
   - [ ] 还原之后 `dist\` 回来了（`Test-Path D:\ProJect\worktrace\dist` = `True`）
3. **反例二（异 cwd 仍找得到仓库根）**：`cd D:\ProJect\worktrace\src-tauri` 后再跑一次那个
   `.ps1`（用绝对路径），输出里的 `repo root` 仍是仓库根、退出码 0。
4. **判据口径**：`dockview` / `dockview-react` 留在 `package.json` **不是失败**（判的是
   "有没有进产物"）。源码里 `src/components/DockviewDemo.tsx` 保留不动（ADR-003）。

**怎么算不通过**：退出码不是 0／缺失 `dist/` 时退出码是 0（假绿）／异 cwd 找不到仓库根／
产物里能搜到那 5 个标记却没有报错。

---

## 2. F-001 / F-002 / F-003：捕获、理清、非法跃迁

> **P7 的 §4 已经写过一遍步骤**（[`manual-shell.md`](manual-shell.md) §4「F-001/F-002 与计时非法请求」），
> 本轮**复核**它并补 P8 新增的四类跃迁入口（F-003，Task 2d）。两处不重复：下面只写
> P8 之后才有、或 P7 那份没覆盖的判据。

### 2.1 F-001 捕获（成功 / 空标题）

1. 前置：干净库、收件箱页（默认页）。
2. 在捕获输入框敲「写验收文档」回车：
   - [ ] 新任务**当场**出现在列表里（不刷新、不切页），输入框清空
   - [ ] 库里：`SELECT title, status, row_version FROM task ORDER BY created_at DESC LIMIT 1`
         ⇒ 标题逐字一致、`status = 'Inbox'`、`row_version = 0`
   - [ ] `revision` 恰好 +1；`task_change` 多一行
3. 清空输入框直接回车：
   - [ ] 界面上出现一句可理解的中文提示（逐字抄）；**没有**新任务、`revision` 不变
   - [ ] 那句话与 Rust 的 `ErrorResponse.message` 一致（前端不自己编文案，R8）

**怎么算不通过**：捕获后列表要手动刷新才可见；空标题也建出了任务；`revision` 涨了但没有新任务
（或反之）。

### 2.2 F-002 理清

1. 对刚才那条点「置为 Ready」：
   - [ ] 行上标签变 `Ready`；`revision` +1；`task_change` 多一行
   - [ ] 在 `Inbox` 行上点「开始」：**只发一条** `start_timer`（DevTools Network 里数），任务
         直接进 `Doing`（不是先 `Ready` 停一拍再变）、`work_session` +1、`work_interval` +1

**怎么算不通过**：点「开始」时发出了两条命令（`clarify_ready` + `start_timer`）；
中间失败留下"已理清但没开始"的半截状态。

### 2.3 F-003 状态跃迁（P8 Task 2d 新增的入口）

1. 在收件箱里准备三条任务：一条 `Ready`、一条 `Ready`、一条 `Inbox`。逐条点开入口观察：
   - [ ] `Ready` 行有「阻塞」「等待」两个入口；`Inbox` 行只有「置为 Ready」（不给「阻塞」）
   - [ ] `Blocked` 行只剩「取消」；`Done` 行只剩「重开」（入口照 `domain/task.rs` 的
         `allowed_targets`，前端不自己发明规则）
2. 点一条 `Ready` 行的「阻塞」：
   - [ ] 任务变 `Blocked`；**如果有正在跑的会话，它被暂停**（回执里点名那条会话）
   - [ ] `revision` +1（**恰好一次**：不是"改状态一条 + 停会话一条"）
3. 在**正在计时**的那条任务上找「开始」：
   - [ ] 入口**不在**（再点必败：Rust 的 `require_no_running_foreground`）
4. **非法跃迁**（服务端必须拒绝，而不是前端拦住）：
   - 在 `Done` 行上构造一次非法目标（例如先让它 `Done`，再用 DevTools 直接 `invoke`
     `transition_task` 传 `target: "Clarifying"`）：
   - [ ] 上屏的是 Rust 的 `message`（逐字抄，不是「未知错误」）
   - [ ] `revision` **不变**、任务行不动（零写入）

**怎么算不通过**：入口开合与 `allowed_targets` 不符；一次动作发出两条写命令；非法跃迁被
前端静默吞掉（没有提示）；被拒时 `revision` 或任务行动了。

**反向验证（改坏什么会让这条红）**：把 `Inbox.tsx` 的 `canFinish`/`canBlock`/`canCancel`/
`canReopen` 任一条改成恒真 ⇒ §2.3 第 1 步红；把跃迁入口接到 `pause_timer` 上 ⇒ 第 2 步的
"恰好一次 +1"红。

---

## 3. F-009 / F-011：关掉全部窗口后托盘可用、计时继续

> 步骤与判据以 [`manual-shell.md`](manual-shell.md) **§1（F-011 托盘）**、**§2A/2B/2C（F-009）**
> 为准，本轮**复核**并补两条 P8 之后才成立的事实。**不要**重写一套。

### 3.1 托盘五项（P8 Task 2d 起「完成」不再是禁用项）

1. 右键托盘图标：
   - [ ] 可点项**恰好五项**、次序是：当前任务、暂停、快速捕获、完成、退出
   - [ ] **没有**任何灰色禁用项（P7 的「完成（P8 启用）」占位项已接上真动作）
2. 计时中（`state='running'`）点「完成」：
   - [ ] 任务进 `Done`、会话进 `finished`（`ended_at` 落上）、`revision` **恰好 +1**
   - [ ] dev 控制台一行 `[worktrace] tray: 完成「<标题>」（revision N，结束会话 1，暂停会话 0）`
3. **暂停中**（`state='paused'`）点「完成」：
   - [ ] 什么都不发生：`revision` 不变、`work_session` 不动
   - [ ] dev 控制台一行 `[worktrace] tray: 没有正在计时（或已被判为待恢复）的会话，完成未执行`
4. **维护态**下点「完成」（配合 §6 的恢复操作，在恢复进行中快速点）：
   - [ ] `worktrace.log` 里出现 `event=tray.finish.refused`（带 `code=DATA_RESTORE_IN_PROGRESS`）
   - [ ] 界面不会有任何弹窗（托盘没有回执通道——这是**已知限制**，不是缺陷）

**怎么算不通过**：可点项不是五项/次序不对/仍有禁用项；一次「完成」出现**两条**
`domain.changed`；`recovering` 会话被顺手结束。

### 3.2 关窗后核心继续（F-009 的硬判据仍在检查点，不在秒数）

按 `manual-shell.md` §2A 做，**判据复述**（因为它最容易被误判）：

- [ ] 关窗 60 秒后再开，`interval_checkpoint` 的 `wall_at` 与 `elapsed_ms` 都**前进 ≥ 20 秒**
      （SQL 见那份文档；**界面秒数证明不了采样还活着**——它由 `started_at` 算出来）
- [ ] 同一段里 `revision` **不变**（心跳有自己的短事务、不加 revision）
- [ ] 重开的第一眼状态正确（暂停的就显示暂停，见 §2B）

**怎么算不通过**：`wall_at` 前进 < 20 秒（采样或心跳没在跑）；把 `revision` 涨了当成"正常活动"；
关窗后托盘图标消失或进程退出。

---

## 4. F-010：Today 五项数字与数据库明细一致；跨日 `23:50–00:10`

> **前提**：本节的数字全部来自**同一次** `stats_today`（同一个 `as_of` / `revision` /
> `data_epoch`）。界面上要能看到口径标注：今天日期、时区（`Asia/Shanghai`）、范围（半开日界）、
> "按当前分类"（R-03）。

### 4.1 五项是什么（别自己相加）

| # | 界面 | 来源 | 口径 |
| --- | --- | --- | --- |
| ① | 今日选择列表 | `today.tasks` | 不是工时 |
| ② | 当前任务 | `today.current` | **有 `current` ≠ 正在计时**：看它的 `state` |
| ③ | 确认人工工时 | `today.confirmed` 的 `human` | 只算 `FOREGROUND` |
| ④ | 运行暂计 | `today.live` 的 `human` | 当前开放区间裁剪到今日 |
| ⑤ | 待确认时间 | `today.pending` | 判"有没有"看条数，不看得数 |

机器（后台 / 被动）与等待**分列**在人工下面，界面上**不得**出现"总计 = 人工 + 机器"。

### 4.2 步骤

1. 用 §4.3 的夹具（或你自己的数据）造出：今天两段已确认前台（例如 `09:00–09:30`、
   `10:00–10:20`）、一段机器后台、一段等待、外加**一条待确认**。
2. 打开「今日」页，抄下 ③④⑤ 的三个数、页面上的日期/时区/范围、以及这次 `revision`。
3. 用 SQL 独立重算（**人工 = `mode='FOREGROUND'`**，只算已确认闭合的区间；半开范围
   `[from, to)` 的裁剪 = `min(ended_at, to) - max(started_at, from)`）：

   ```sql
   SELECT SUM(MIN(i.ended_at, <to>) - MAX(i.started_at, <from>)) AS human_ms
     FROM work_interval i JOIN work_session s ON s.id = i.session_id
    WHERE s.state NOT IN ('discarded') AND i.voided_at IS NULL
      AND i.needs_review = 0 AND s.mode = 'FOREGROUND'
      AND i.ended_at IS NOT NULL AND i.started_at < i.ended_at
      AND i.started_at < <to> AND <from> < i.ended_at;
   ```

   **`<from>` / `<to>` 必须是 `work_interval.started_at` / `ended_at` 那种单位与格式**：
   **INTEGER 的 Unix 毫秒**（`started_at`/`ended_at` 的列类型就是 INTEGER 毫秒）。
   **不要**把界面上显示的那个本地日期文本（`2026-10-03` 或 `2026-10-03 00:00`）或
   `LocalDate` 的字符串直接塞进来——那是**字符串**，与 INTEGER 比会得到"假不等"
   （要么 0 行、要么恒不等，看着像产品错了）。取法：从库里的半开日界抄，
   或用界面显示的日期自己换算成该时区零点对应的毫秒（`Asia/Shanghai` 是 UTC+8，
   当天 `00:00` = 前一天 `16:00 UTC`）。
4. 逐项核对：
   - [ ] ③ 界面数字 **==** 上面 SQL 的 `human_ms`（毫秒值相等；界面显示的是格式化文本，
         用 DevTools 或 `formatDuration` 的规则换算回毫秒再比）
   - [ ] ④ 有正在跑的会话时 > 0、且随秒数**前进**；没有正在跑的会话时是 0
   - [ ] ⑤ 的**条数** = `SELECT COUNT(*)` 那条待确认区间的条数（作废/已丢弃的**不算**）
   - [ ] 机器与等待**没有**被并进 ③（把机器那段改成前台，③ 才跟着涨）
   - [ ] 口径标注可见（日期 / 时区 / 范围 / "按当前分类"）
   - [ ] 空库或没有今天的区间时显示 **0**（不是空白、不是 `—`、不是错误）
5. **作废不算待确认**：把一条待确认区间作废（走 §5），回到今日页：
   - [ ] ⑤ 的条数减一，且它**没有**出现在待确认里（只在历史/审计里可查）

**怎么算不通过**：③ 与 SQL 不等（哪怕差 1 毫秒）；人工与机器相加出现在界面上；作废记录仍
计进待确认；空数据时页面是空白或报错。

### 4.3 跨日 `23:50–00:10`（两天各 10 分钟）

> **判据的关键是"一条区间跨过午夜"**：只有**同一段**区间的起止分别落在两天里，才能验出
> "按区间裁剪归属"（而不是"按 `started_at` 归属整天"）。**必须**用 §7 的 `correct` 把**一段**
> 已结束区间改成真跨午夜；两段拼接（`23:50–00:00` + `00:00–00:10`）**只算前置夹具**，
> 它各自落在一天内，**打不红**本节末尾那条反向验证（fix round 1 / Important-4）。

1. **造真跨午夜的一段**（两条命令，写清每条之后库里应该是什么样）：
   1. 先在历史页用**补录**（§7）录一段 `前一天 23:50 → 前一天 23:55`（5 分钟，纯夹具：
       让区间已经存在、且是 `finished` 的会话）；
   2. 在历史页点开那条会话 → 详情 → 用**修正起止**（`correct`）把它改成
      `前一天 23:50 → 次日 00:10`（20 分钟）⇒ **现在库里有一段真跨午夜的区间**。
   - [ ] 库里核对（唯一真相）：
     `SELECT started_at, ended_at, ended_at - started_at FROM work_interval WHERE id = '<刚改的那条>'`
     ⇒ 差值 = **1200000 ms**，且 `started_at` 与 `ended_at` **不属于同一个本地日期**
     （用界面显示的日界或自己换算成 `Asia/Shanghai` 的日期去比）
   - [ ] 反例前置（可选，用来确认你确实能看出"没跨"）：两段拼接的夹具（`23:50–00:00` +
         `00:00–00:10`）**不满足**上一条（两段各自 `started_at` 与 `ended_at` 在同一天）
2. 打开「今日」页看**次日**（在 `00:10` 之后做，"今天"就是次日）：
   - [ ] 次日的确认人工工时 = **10 分钟**（600000 ms）
3. 再看**前一天**（把系统日期/时区改成前一天，或在库里用前一天的 `<from>/<to>` 重跑 §4.2 的 SQL）：
   - [ ] 前一天也是 **10 分钟**（600000 ms）
   - [ ] 两天的和 = **20 分钟**：**没有**一天吃掉 20 分钟、另一天 0

**怎么算不通过**：两天加起来不是 20 分钟；某一天拿到 20 分钟（说明按 `started_at` 归属而不是
按区间裁剪）；出现负数或 0 毫秒的诡异明细；只有两段拼接的夹具就宣称"跨日验过了"。

**反向验证（这一节存在的理由）**：把 `IntervalRange::clipped_ms` 换成"整段都算给起点那天"
⇒ 第 2/3 步一天 20 分钟、一天 0 ⇒ 红。**注意**：只有第 1 步造的是**一段**真跨午夜区间时，
这条反向验证才有效；两段拼接的夹具下，改坏实现**也不会红**。

---

## 5. F-015 / F-017：强杀重启后四类判定；确认 / 丢弃 / 作废、修正与补录

> **F-015 的四类判定**（`services/recovery.rs` / `scan_recovery`）：
> ① 不变量损坏（`InvariantBroken`）② 待确认区间（`NeedsReview`）③ 未结束会话（`NeedsReview`）
> ④ 什么都不需要（`None`）。前三类在恢复页可见，第四类是"这个会话不用管"。

### 5.1 造一条"强杀后重启"的现场

```powershell
# 1) 开始一次计时，让它跑 30 秒以上（心跳 30 秒会写检查点）
# 2) 直接杀进程（不要走托盘「退出」）
Stop-Process -Name worktrace -Force
# 3) 10 秒内重新启动（F-015 的"10 秒内重启"）
```

- [ ] 启动**没有**重复初始化：`SELECT COUNT(*) FROM application_run` 只比上次 +1
- [ ] 上一次那条会话**没有**被自动结束（`state` 不是 `finished`、`ended_at` 为 NULL，
      或被判成 `recovering`）——强杀会留下未闭合事实，**这是设计**
- [ ] 打开「恢复」页：上面那三类**各有一条**可见（不变量损坏那条要靠构造：
      手工在库里造一条 `running` 但没有任何开放区间的会话，重启后它应当落在
      `invariant_broken` 那一类）
- [ ] 第四类：另起一条会话后**正常**用托盘「退出」，重启后它**不在**待处理列表里
      （`attention_overview` 的条目数不因它增加）

**怎么算不通过**：强杀后重启"什么都没发生"（未闭合事实被静默吞掉）；把待确认区间当成
已确认工时上了 Today；恢复页把第四类也列出来。

### 5.2 恢复页的三条动作（各自一次，互不合并）

| 动作 | 界面入口 | 语义 | 要点 |
| --- | --- | --- | --- |
| **确认** | 「确认这些起止」+ 上方单选「对账之后这条会话停在：已结束 / 暂停」 | `reconcile(Confirm)` | 起止必须**合法且不重叠**；重叠时显示服务返回的**具体冲突**，不是笼统失败 |
| **丢弃不确定区间** | 「丢弃不确定区间」 | `reconcile(DiscardUncertain)` | **只**作废待确认段；此前已闭合的可信工时**保留**；会话转为上面选的状态 |
| **作废整次** | 「作废整次记录」+ 二次确认弹窗（`确认作废` / `取消`） | `discard_session` | 全部区间软作废 + 会话标记已作废（审计保留）；与上一条**不是一件事** |

1. **确认**：
   - [ ] 先故意给一个**重叠**的起止（与已有的已确认区间重叠）⇒ 界面显示服务给的**具体冲突**
         （逐字抄），`revision` 不变
   - [ ] 再给合法起止 ⇒ 该会话的可信前缀 + 这次确认进 Today 的③；`revision` +1（恰好一次）
   - [ ] 已知单调时长**只作候选**（输入框的 placeholder 是候选值），**不是**强制默认值
2. **丢弃不确定区间**：
   - [ ] 只丢待确认段：作废那些区间的 `voided_at` 非空；**此前已闭合的工时还在**（今日页③不变）
3. **作废整次**：
   - [ ] 点「作废整次记录」先弹确认框：**点开确认框本身一条命令都不发**（`revision` 不变、
         DevTools 里没有 `discard_session`）；点「取消」⇒ 零命令、零报错
   - [ ] 点「确认作废」⇒ 这条会话的全部区间 `voided_at` 非空、会话 `state='discarded'`
   - [ ] 它**没有**出现在 Today 的待确认里
4. **重试入口**（P6 的故障路径）：恢复页的「重试」（`retry_recovery`）
   - [ ] 没有故障时点它：不报错、`revision` 不变（它可能什么都没做）
   - [ ] 计时区标注"计时不可用"时点它：原因消失则恢复可用（事件名见 `worktrace.log` 的
         `timer.unavailable.end`）；
5. **接受检测到的时钟校正**（`accept_detected_clock_correction`）：
   - [ ] 有**未被接受的墙钟异常**时，恢复页给「接受这次校正」入口；点它 ⇒ 之后可以正常
         开始/继续计时（在此之前 `start`/`resume` 被拒成 `RECOVERY_REQUIRED`）
   - [ ] 它**不自动接受**：不点就一直拒（"显式接受"是判据）

**怎么算不通过**：确认/丢弃/作废**合并成一个按钮**；作废只作废了待确认段（或反之）；
丢弃把已闭合的可信工时也丢了；纠正重叠时只给"失败"而不给具体冲突。

### 5.3 F-017 修正与补录（历史页）

1. 点历史列表里一条**已结束**的会话 → 「会话详情」：
   - [ ] 详情里能看到会话状态、真实起止、`row_version`、全部区间，以及**审计**（改动历史）
   - [ ] 只对 `finished` 开放「修正起止」：`discarded` 的行上看不到这个入口（或明确说明不可用）
2. **修正起止**（`correct`）：把一个区间的起止改成合法值
   - [ ] 请求带**真实** `row_version`（详情里的那个）；成功后 `revision` +1
   - [ ] **修正后 Today 与导出跟着变**：今日页③按新时长、导出的 JSON 也是新时长
3. **删除误记**（软删除）：
   - [ ] 二次确认弹窗（`确认删除` / `取消`）；取消 ⇒ 零命令
   - [ ] 确认后该区间 `voided_at` 非空（**软**删除：审计里还看得到），界面明说"保留审计"
4. **补录**（`backfill`，入口在历史页底部「补录一段已经发生的时间」）：
   - [ ] 它**不启动计时**（选完时间提交后，计时页仍然没有正在跑的会话）
   - [ ] 它**不伪造完成事件**：`task_change` 里没有因它新增的 `Done` 事件
   - [ ] 补录出的是一条**已结束**的人工会话 + 一条区间；`revision` +1
   - [ ] 负区间 / 与人工区间重叠 ⇒ 被拒，上屏 Rust 的 `message`（逐字抄），零写入
5. **分页与"没列全"**：任务很多时下拉底部有「已列出 N / 共 M 条」+「加载更多」
   - [ ] `M` 与服务端 `total` 一致；取满后「加载更多」禁用

**怎么算不通过**：`Discarded` 会话上还能点「修正起止」；删除是硬删（审计查不到）；补录
启动了一个会话或写了一条完成事件。

---

## 6. F-019：备份 → 恢复 → 旧请求被拒

> 恢复是**全项目唯一的危险操作**：它把库换成另一份，`data_epoch` 随之改变。
> **做之前先抄一份当前 `data_epoch` 与 `revision`**：

```sql
SELECT data_epoch, revision FROM app_meta WHERE singleton = 1;
SELECT id, clean_exit_at FROM application_run ORDER BY started_at DESC LIMIT 3;
```

### 6.1 备份

1. 「数据」页 → 「备份」：
   - [ ] 得到一条**真实存在的绝对路径**（`%APPDATA%\com.worktrace.desktop\backups\...`），
         界面能一键复制（`Typography.Text copyable`）
   - [ ] 文件确实存在且**不是 0 字节**：`Get-Item <路径> | Select Length`
   - [ ] `revision` **不变**、没有 `domain.changed`（它不改业务事实——"版本没变"不是失败）
   - [ ] `worktrace.log` 里没有 `maintenance.begin`（备份不长占维护态）
2. 记下这条备份路径（下面要用），另外**再备份一次**留一份"能回得去"的底。

### 6.2 恢复

1. 先把库改成一个**可辨认的状态**（例如再捕获一条任务「恢复前」），记下 `revision`。
2. 把 §6.1 的备份路径粘进「恢复」输入框，点「恢复」：
   - [ ] 弹出二次确认框（标题「用这份备份覆盖当前数据？」）；**点开不确认时零命令**
         （DevTools 里没有 `restore`）
   - [ ] 点「取消」⇒ 零命令、零报错（"取消 ≠ 失败"）
3. 点「确认恢复」，观察全过程：
   - [ ] `worktrace.log` 里 `event=maintenance.begin` →（恢复期间）`event=maintenance.end`
   - [ ] 恢复期间界面显示**"正在恢复"**并且**写入类入口被禁用**（导出/备份/恢复按钮灰掉）
   - [ ] 恢复期间**不能退出**：托盘「退出」被拒，`worktrace.log` 出现
         `event=tray.quit.refused`（`code=DATA_RESTORE_IN_PROGRESS`），进程仍在
   - [ ] 恢复之后**能**退出（拒绝不是终态）：`event=maintenance.end` 之后点「退出」正常结束
   - [ ] 恢复成功后界面**重新握手**：`data_epoch` 变成**新值**、旧展示不残留
         （恢复前那条「恢复前」任务应当**不在**列表里了——我们恢复到的是更早那份库）
4. **旧请求被拒**（这是本节的核心判据）：
   - 在恢复**刚完成**时立刻用 DevTools 发一条**带旧 epoch** 的写请求（把
     `<恢复前抄下的那个 epoch>` 与 `<任意任务 id>` 换成真值再粘）：

     ```js
     await __TAURI_INTERNALS__.invoke("start_timer", { request: {
       expected_data_epoch: "<恢复前抄下的那个 epoch>", task_id: "<任意任务 id>",
       task_expected_version: 0, mode: "FOREGROUND", timer_kind: "stopwatch" }})
     ```

   - [ ] 拒绝码是 `DATA_EPOCH_MISMATCH`（或带 `requires_handshake: true`），**不是**静默成功
   - [ ] 前端按这条码走**重新握手**，**不自动重试**那条写命令
   - [ ] 库里那份"旧库"的数据没有被这次旧请求写进去（`revision` 不动）

**怎么算不通过**：恢复期间还能写、还能退出；旧 epoch 的请求被接受或静默丢弃而没有提示；
恢复后旧数据仍显示在界面上（依赖"下一次握手"以外没有别的清空路径，见
`docs/validation/p8-acceptance.md` §3 的 known boundary）。

### 6.3 恢复失败也要能看见（可选加固，登记项）

- [ ] 用一份**坏备份**（例如把 `.db` 文件截断一半）走一次恢复：失败**不伪装成成功**，
      界面上屏的是 Rust 的 `message`；`worktrace.log` 里有失败诊断
- [ ] 若服务给出 `<db>.restore-rollback` 路径，界面要能把它告诉用户（**不要**当垃圾清理）

---

## 7. F-018：导出 JSON 用外部工具重算与界面一致；周回顾逐项核对

### 7.1 JSON 导出

1. 「数据」页 → 导出格式选「JSON 范围明细」→「导出」：
   - [ ] 得到真实路径（`%APPDATA%\com.worktrace.desktop\exports\worktrace-export-json-<毫秒>.json`）
         与字节数；路径可复制
   - [ ] 文件存在、非 0 字节；用编辑器能打开（UTF-8、合法 JSON）
2. **外部工具重算**（用 `jq`、PowerShell 或任何第三方脚本，**不要**用本应用）：
   - [ ] 把 JSON 里 `class == "confirmed"` 且 `measure == "human"` 的明细逐条相加，
         结果与 `columns` 里那一项**毫秒值相等**（明细加得起来才等于合计）
   - [ ] 再与**界面**上同一范围的数字比：**同一个 `as_of` / `revision`**（JSON 里有）⇒ 相等
   - [ ] 导出里的"任务 → 项目 / 标签"连接字段在，使第三方能按标签/项目归并（R-03）
   - [ ] 待确认明细里**作废的区间不出现**；`class=pending` 的 `duration_ms` 允许为 `null`
3. **导出不改业务**：
   - [ ] 导出前后 `revision` **不变**、没有 `domain.changed`（写文件不是业务事实）
4. **同名产物**：同一毫秒内对同一形状**再导一次**（几乎不可能人为复现，登记为边界）：
   - 期望：第二次拿到 `STORAGE_ERROR`（`detail=artifact already exists`），**不静默覆盖**

**怎么算不通过**：JSON 里的合计与明细对不上；导出数字与界面数字来自两个水位（`as_of` 或
`revision` 不同）；导出推进了 `revision`。

### 7.2 周回顾（Markdown）

1. 导出格式选「Markdown 周回顾」→「导出」：
   - [ ] 产物是 `.md`，含本周**人工投入**、**完成任务**、**待确认**三段
   - [ ] 周界由**服务**算（本周一到周日，按查询时区），**不**由页面传范围
   - [ ] 逐项核对：拿库里的 `task_change`（`json_extract(after_json,'$.status') = 'Done'`
         且 `created_at` 落在本周）核对"完成任务"那一节逐条对得上
   - [ ] 人工投入与今日页/JSON 的人工在**同一周**上一致（同一 `as_of`）
2. **打开所在位置**（`revealItemInDir`，P8 Task 3b 的能力收窄项）：
   - [ ] 点导出/备份产物下的「打开所在位置」⇒ **真的弹出资源管理器**并选中那个文件
         （这是"能力真的可用"的唯一判据——Tauri 2 没有 ACL 查询接口，只能实机验）
   - [ ] 拒绝/失败时界面给出明确说明（禁用 + 一句解释），**不是**静默失败
   - [ ] 反复点两次不会把界面卡住（`revealError` 是闩锁，见 known boundary）

**怎么算不通过**：点「打开所在位置」什么都没发生（无文件管理器、无提示）；周回顾的任务清单
与库里的事实对不上；周界由页面自己算（换时区后周界不变就说明是页面算的）。

---

## 8. F-020：多窗口乱序 / 丢通知后收敛（界面侧）

> **步骤与记录模板以 [`manual-sync.md`](manual-sync.md) 为准**（§1 开实验窗口、§2.0–§2.5
> 三个竞态、§4 界面侧判据）。本节只写**必须复核的结论**与**本轮新增的相关项**。
> 前置：dev 构建（四条注入开关只在 debug 存在，`commands/dev.rs`）：
>
> ⚠️ **原生最小化 ≠ `hidden`（fix round 1 / Important-2；2026-10-05 起就有的实机观察）**：
> Windows/WebView2 上原生 `isMinimized()` 为 true **不代表** `document.visibilityState`
> 为 `hidden`——实测过"原生最小化、页面仍 `visible`、周期校验照跑"。所以"**最小化的窗口**"
> 这一格**不能**替 §2.1 的"隐藏窗口"判据，**不得记通过**，必须单独登记（措辞与
> [`manual-sync.md`](manual-sync.md) §2.5 一致）。**更关键的是**：计划 2026-10-05 那条
> "**原生窗口可见性适配**（监听原生最小化、恢复首屏校验、最小化期间停止业务轮询）"
> **本轮没有实现**——代码里今天的判据仍只有 `document.visibilityState`
> （`src/state/domainState.ts`）。⇒ 它属 **V0.1 收尾的未达成项**，在本节与
> `docs/validation/p8-acceptance.md` §3 都有登记；**不得**用它当"隐藏/最小化路径已验"的证据。

```js
await __TAURI_INTERNALS__.invoke("__p7_open_sync_lab")
await __TAURI_INTERNALS__.invoke("__p7_drop_next_event", { kind: "domain.changed" })
await __TAURI_INTERNALS__.invoke("__p7_delay_next_query_ms", { command: "list_tasks", ms: 4000 })
await __TAURI_INTERNALS__.invoke("__p7_replay_event", { revision: 12 })
```

1. **末次通知丢失** ⇒ 30 秒内收敛（**窗口要真的"可见"**——按 `manual-sync.md` §2.5 的判据，
   在该窗口控制台确认 `document.visibilityState === "hidden"` 才算"隐藏"那一档）：
   - [ ] 在 A 窗口暂停 → 丢掉 B 窗口的那条通知 → B 的展示在 **≤30 秒**内变成"已暂停"
         （走周期校验：**可见**窗口每 30 秒校验一次 epoch/revision）
   - [ ] 收敛之后 `revision` 与 A 一致，且**没有**第二套状态（镜像只有一份）
   - [ ] **最小化**那一格：按上面那条 ⚠️ 单独登记现象（页面是不是仍 `visible`、
         周期校验有没有继续），**不填"通过"**
2. **旧响应晚到** ⇒ 不上屏：
   - [ ] 制造一次"旧响应比新响应晚到" ⇒ 旧的不覆盖新的（页面自己的视图水位判旧）
3. **乱序跳号** ⇒ 重新握手而不是错乱：
   - [ ] 用旧 `revision` 重播一条 `domain.changed` ⇒ 客户端判为乱序/过期，走 `Resync`
         （重新握手），最终状态与库一致
4. **判据链的两处实现**（P7 §6.6-29 的登记项）：前端 `orderTimer` 五级 vs Rust
   `is_stale_tick` 两级
   - [ ] 上面三条走完，两侧对"这一拍算不算新"的结论**没有分叉**（例如 B 窗口不会因为
         一次迟到的 tick 把秒数回退）
5. **`applied == None` 那一格**（P7 §6.6-30 的登记项）：
   - [ ] 新窗口**尚未应用任何快照**时到达一条通知：不丢、不重复上屏（按既有规则暂存/交付）

**怎么算不通过**：超 30 秒仍不收敛；旧响应覆盖了新状态；乱序被当成正常写入静默接受；
两个窗口的同一条会话显示不同的状态且不再收敛。

---

## 9. F-014：拔网线跑完整 V0.1 功能

1. **物理拔网线**（或禁用网卡），保持应用运行：
   - [ ] 全部八页逐个走一遍：捕获、理清、开始/暂停/结束、项目、标签、今日、恢复、历史、
         导出、备份 —— **无报错、无降级提示、无"重连中"**
   - [ ] DevTools 的 Network 面板：**没有**任何对外请求（`file:`/`tauri:`/IPC 之外零流量）
   - [ ] 导出产物在断网状态下照常落盘、周回顾照常生成
2. 断网状态下**重启应用**：
   - [ ] 启动成功（不要求首次下载依赖也能离线——依赖是**预先装好**的）
3. **插回网线**：
   - [ ] 不需要重启、不需要任何"同步/登录"，功能与断网时**逐项一致**

**怎么算不通过**：任何一页出现网络错误；出现"离线模式"一类的降级横幅（V0.1 没有在线态，
所以也不该有降级态）；启动时联网检查。

---

## 10. 本阶段新增功能的验收（P8 交付的 8 块页面里，前面没覆盖到的部分）

### 10.1 今日页与 `stats_today`（F-010 的载体）

见 §4。这里只补外壳侧：

- [ ] 默认页仍是**收件箱**（不是今日页）——挂载时恰好发那四条命令（`get_revision` /
      `timer_snapshot` / `list_tasks` / `list_selectable_projects`），**没有** `stats_today`
- [ ] 点「今日」才发 `stats_today`（一次），**不**发 `plan_for`
- [ ] 今日选择列表能加/删（`add_to_plan` / `remove_from_plan`）：删掉一条之后列表少一条、
      `revision` +1；**它不改任务的 status**（`task_change` 里没有因它新增的跃迁）

### 10.2 恢复页三条动作 + 二次确认 + 维护态提示

见 §5（动作）与 §6（维护态）。补：

- [ ] 恢复页在**维护态**下写入入口禁用且显示"正在恢复"（不是通用错误）

### 10.3 历史页 `correct` / `backfill`

见 §5.3。

### 10.4 Data 页导出与 `revealItemInDir`（**真能弹出资源管理器**）

见 §7。**这一条只有实机能验**：Tauri 2 没有 ACL 查询接口（`checkPermissions()` 在
tauri 2.12 无实现），页面只能"非 Tauri 环境先禁用 + ACL 被拒时锁存禁用"。

### 10.5 托盘「完成」与**视图跳转**（P8 Task 7；计划 §6.4-11）

> 这一条在 P7 是"只抬窗"，本轮要验的是**真的切了视图**。

1. **「当前任务」**：
   - [ ] 主窗**已开着**且在别的页（例如「数据」页）时点托盘「当前任务」⇒ **切到计时视图**，
         并且窗口同时被带到前台、拿到焦点（**抬窗不能丢**）。托盘这条路径是**同步抬窗**
         （`show` + `unminimize` + `set_focus`，不经轮询）；500 ms 那个轮询属**单实例唤醒**
         （第二个进程写请求文件 → 既有实例轮询消费），别混起来。
   - [ ] 主窗**最小化**时点它 ⇒ 还原 + 聚焦 + **仍然切到计时视图**
   - [ ] DevTools 里 `[tray-view] {page: "timer", focus: false}`（§0.5 的订阅）
   - [ ] 没有正在计时的任务时点它：**仍然切到计时视图**并显示空态（不报错、不造假会话）
   - [ ] ⚠️ **主窗已被关掉**（托盘还在）时点它：窗口会**重建**出来，但**停在收件箱**
         （不是计时视图）——这是**已登记的限制**，不是本步骤的失败：
     - **为什么**：跳转请求在重建**之前**发出，那时窗口还不存在（零接收者，而且
       `emit_to` 对不存在的 label 通常**不返回 `Err`** ⇒ 日志里可能连一行诊断都没有）；
       重建出来的是全新 JS 上下文，按默认页挂载。
     - **判据怎么拆**：窗口**已存在**（可见 / 最小化 / 被遮挡）⇒ 跳转**必须**成立；
       **关掉后重建** ⇒ 只要求"窗口回来 + 抬到前台"，跳转丢失记**已知限制**
       （`docs/validation/p8-acceptance.md` §3）。**这一格不要打勾**，写
       「重建路径跳转丢失（已登记）」，并附上你怎么区分出来的（见下一条）。
     - **怎么区分两种情形**：点之前看一眼导航高亮。点托盘后窗口回来了——高亮在「计时」
       ⇒ 情形一（通过）；高亮回到「收件箱」⇒ 情形二（已知限制）。DevTools 也能区分：
       情形二里**没有** `[tray-view]` 那一行。
2. **「快速捕获」**：
   - [ ] 在别的页（例如「历史」）点托盘「快速捕获」⇒ 切到**收件箱**，且**捕获输入框拿到焦点**
         （光标在输入框里，直接打字就进输入框）
   - [ ] ⚠️ **人本来就在收件箱**时点它（页面不重挂、`disabled` 与列表都不变）⇒ 光标
         **当场**落进捕获输入框（fix round 1 / Critical-1 的判据：这条路径上"只置一个模块级
         布尔"不够，必须有东西让聚焦判据重跑一次）
   - [ ] ⚠️ 点完一次之后**不再点第二次**，把光标移到别处（点列表空白 / 点别的输入框），
         等下一次自动重拉（`domain.changed`，或 1 秒一拍的 `timer.tick` 之后的任意重渲染）
         ⇒ 光标**不得**被抢回捕获输入框（意图是**一次性**的）
   - [ ] DevTools 里 `[tray-view] {page: "inbox", focus: true}`
   - [ ] 最小化后点它：窗口被还原 + 聚焦 + 切页 + 焦点在输入框
   - [ ] ⚠️ 冷启动/刚握手完那一刻（输入框还是禁用态）点它：**不能把这次跳转吃掉**
         ——禁用一解除，光标仍然落进输入框（实现里"禁用时不消费意图"就是为这一幕）
3. **失败姿势**（托盘没有回执通道）：
   - [ ] 事件发不出去时：dev 控制台一行
         `[worktrace] tray: <菜单 id> view request failed: …`，**不 panic**；抬窗照旧
         （成功的话仍能看到 `tray: <菜单 id> -> …` 那一行）。
         ⚠️ **"有诊断 ≠ 必然出现"**：那行只在 `emit_to` **真的返回 `Err`** 时打，而
         "主窗不存在"（上面情形二）通常是**零接收者、不报错** ⇒ **看不到诊断不等于跳转送到了**。
         判断跳转有没有送到看 DevTools 的 `[tray-view]`，不看这行。
   - [ ] 认不出的载荷（用 DevTools 手动 emit 一条 `{page: "recovery"}`）⇒ **什么都不做**
         （不切页、不崩），控制台有一句 `console.warn`
   - [ ] 订阅建立失败时（例如权限被拒）控制台有一句
         `[worktrace] 托盘跳转订阅失败，视图跳转本次不可用`（跳转不可用，其余功能照常）

**怎么算不通过**：窗口**已存在**时点了之后只有窗口抬起来（视图没变）；切了页但输入框没聚焦
（**人已在收件箱**那一格也要过）；光标被抢回输入框（意图不是一次性的）；跳转把抬窗弄丢了
（窗口没到前台）；认不出的载荷把界面切走或抛异常。（**不通过**不含"关窗后重建不跳转"——
那一条是已登记限制，见上。）

### 10.6 `RECOVERY_REQUIRED` 切页（M8）

1. 造一条会被门禁拒绝的写命令（干净库上比较难：最省事的做法是让库里存在一条非当前 run 的
   未闭合会话，见 §0.3）：在收件箱点「开始」
   - [ ] 界面**切到恢复页**（不是只弹一条提示），并把 Rust 的那句话一起带过去
         （「存在待确认的计时记录，请先处理恢复再继续。」逐字一致）
   - [ ] 那句说明**可关闭**；关掉之后恢复页仍在
   - [ ] 若这条失败**同时**带 `requires_handshake`：切页与重新握手**都**发生（不互相吞）
2. **维护态**下点写命令（§6.2 恢复进行中）：
   - [ ] 上屏的是 `DATA_RESTORE_IN_PROGRESS` 对应的中文（"正在恢复数据…"），并按码禁用入口

**怎么算不通过**：只弹了一句通用提示而没切页；切页把那句话丢了；两条动作互相吞并。

---

## 11. P6 的 OS 事件：锁屏 / 休眠 / 唤醒 / 改时的到达延迟与行为

> 实现归 P6（真 Win32 事件源：隐藏消息窗 + `PBT_APMSUSPEND` 一类），**结论归本轮**。
> 事件是**稀疏**的，所以诊断日志是主要证据；事件源本身起不来会记
> `event=system_events.unavailable`（**不 panic、不假装成功**）。

### 11.1 锁屏 / 解锁

1. 计时中按 `Win+L` 锁屏，等 **≥2 分钟**，解锁：
   - [ ] 解锁后计时**没有**把锁屏那段时间算成人工（人工只算有前台事实的时间；
         锁屏边界走 `system_pause`）
   - [ ] `work_session.state` 与区间事实与界面一致（键屏期间是 `paused` 还是保持 `running`
         取决于边界判定；**照实记**，并与 `worktrace.log` 那一行对齐）
   - [ ] 到达延迟：从解锁到界面数值正确 **≤ 下一条 `timer.tick`（1 秒）+ 一拍处理**；
         把 `worktrace.log` 里那条边界记录的时刻与解锁时刻都抄下来

### 11.2 休眠 / 唤醒（含 Modern Standby）

1. 计时中让机器**睡眠**（合盖或开始菜单 → 睡眠），过 **≥5 分钟**唤醒：
   - [ ] 唤醒后**不自动继续计时**（P2 的口径："醒来不自动继续"）；需要用户显式继续
   - [ ] 那段睡眠**不计**人工；未被接受的墙钟异常存在时，`start`/`resume` 被拒成
         `RECOVERY_REQUIRED`，恢复页给「接受这次校正」（§5.2 第 5 条）
   - [ ] ⚠️ **Modern Standby 可能不发 `PBT_APMSUSPEND`**：如果唤醒后**没有**边界记录，
         那不是"事件丢失 bug"而是**记录在案的平台差异**——照实登记现象与机型，
         并检查"长间隔 ⇒ recovering"那条兜底路径是否生效
   - [ ] 到达延迟：唤醒时刻 → 日志里那条边界记录的时刻，逐台抄下来

### 11.3 正反改时

1. 计时中把系统时间**往前**调 2 小时（+2h），等 30 秒：
   - [ ] 界面没有把 2 小时算成工时；出现"检测到墙钟异常"的地方（恢复页 / 计时区）
   - [ ] `start`/`resume` 被拒，直到显式「接受这次校正」
   - [ ] `work_session` 里没有凭空多出 2 小时的区间
2. 再把时间**往回**调 2 小时（回到原值附近）：
   - [ ] 单调钟**不倒退**（`abs(wall - L(M))` 那条长期边界不会因为回调而误判为正常）
   - [ ] 又一次显式接受之后功能恢复

### 11.4 判据汇总（把每类的"到达延迟"与"行为"填进下表）

| 事件 | 机型/系统 | 触发时刻（墙钟） | 日志记录时刻 | 到达延迟 | 行为（暂停 / recovering / 忽略） | 结论 |
| --- | --- | --- | --- | --- | --- | --- |
| 锁屏 | | | | | | |
| 解锁 | | | | | | |
| 休眠 | | | | | | |
| 唤醒 | | | | | | |
| 正向改时 | | | | | | |
| 反向改时 | | | | | | |

**怎么算不通过**：锁屏/休眠那段时间被算成人工；唤醒后**自动**继续计时；改时把 2 小时
写成工时；事件源起不来却假装成功（没有 `system_events.unavailable` 却也没有任何边界记录）。

---

## 12. 500 ppm 跨机器校准（**≥2 台机器**；P8 收尾必办）

> 来源：`docs/validation/p1-p2-acceptance.md` 与 `docs/validation/p1-p4-review-backlog.md`
> 的 FOLLOW-05；判据原文见 `docs/validation/p2-clock-mapping.md` §5/§7。
> **本机曾观察到约 233 ppm，500 ppm 只是初始可版本化策略、不是通用平台结论** ⇒
> 必须换机器复验。**单机结论不得当成跨机器结论。**

**长期边界（逐字）**：
`abs(wall - L(M)) > 2000 + floor(elapsed_ms × 500 / 1_000_000)`，其中
`L(M) = lifetime_wall_at + (M - lifetime_monotonic_at)` 是**本次 run 初始化**时定下、
**不随心跳清零**的长期参照（`A(M)` 只用于工时归属，不作为长期偏差参照）。

### 12.1 每台机器都要填的表（一行一次观察）

| 机器 / CPU | `winver` 全文 | 提交 | 验收人 / 日期 | 场景 | `elapsed_ms` | 实测 `wall - L(M)`（ms） | 界值 `2000 + floor(elapsed×500/1e6)` | 结论（在界内 / 越界） |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| | | | | 长时间挂机 | | | | |
| | | | | 一次休眠 / 唤醒 | | | | |
| | | | | 正向改时 | | | | |
| | | | | 反向改时 | | | | |
| | | | | 前台计时进行中 | | | | |
| | | | | 长时间挂机 | | | | |

**每台机器至少覆盖这五类**：① 长时间挂机（跑足够长的 `elapsed`，例如 ≥8 小时；界值因此
约 `2000 + 14_400` ≈ 16.4 秒）；② 一次休眠 / 唤醒；③ 正向改时；④ 反向改时；
⑤ **前台计时进行中**（这条同时验"计时归属不受漂移影响"）。

### 12.2 怎么取数

- 采样记录与字段名见 `docs/validation/p2-clock-mapping.md` §4（`samples` / `flagged` /
  `worst_cum_gap_ms` / `wall_backwards` / `monotonic_backwards` / `suspends` / `markers`）。
- 长期参照只在**新 run 初始化**或**已接受的墙钟偏移/漂移审计提交之后**移动：
  普通 pause/resume、可信系统离开边界、无可靠边界的长间隔、采样失败**都不得**移动它
  （这是判据的一部分，不是实现细节）。
- **结论必须写明是哪台机器、哪个系统版本、哪个提交**；界值不成立时给出实测数值与复现步骤，
  并**回写** `docs/validation/p2-clock-mapping.md`（不得只写在本文件里）。

### 12.3 怎么算通过 / 不通过

- **通过**：每台机器的每一类场景，`abs(wall - L(M))` 都**不大于**该 `elapsed_ms` 对应的界值。
- **不通过**：任何一类越界（给出实测值与 `elapsed_ms`）；或"只在一台机器上跑过"就当跨机器
  结论；或把 `A(M)` 当长期参照算出来的"没超"（参照选错，结论无效）。

---

## 13. P7 归 P8 的三条（逐条闭环）

| # | 条目 | 步骤在 | 判据 |
| --- | --- | --- | --- |
| 1 | **托盘视图跳转**（§6.4-11） | §10.5 | 点了之后**真的切视图**（不是只抬窗），且抬窗没丢 |
| 2 | **Windows 打包产物启动一次**（§6.4-15） | §13.2 | 见下 |
| 3 | **退出事务失败的用户可见提示**（§6.3-9） | §13.3 | 见下：**当前是"未达成"的发现**（P6 只交付了诊断出口，界面没有出口），不是"允许不做" |

### 13.1 复核 P7 那两份手册的结论

- [ ] `manual-shell.md` 的 §1/§2/§3（托盘、关窗、单实例）本轮逐条跑过并填了现象（或明确
      写"本轮未跑"，**不得**留空当作通过）
- [ ] `manual-sync.md` 的 §2.0–§2.5 本轮跑过（对应本文 §8）

### 13.2 Windows 打包产物启动一次（§6.4-15）

1. `pnpm tauri build` 之后，用**打包产物**启动（二选一，写清用的哪一种）：
   - 安装包：`src-tauri\target\release\bundle\msi\*.msi` 或 `nsis\*-setup.exe` → 安装 → 从开始菜单启动
   - 免安装：`src-tauri\target\release\worktrace.exe` 直接双击
2. 逐项打勾：
   - [ ] 主窗正常打开、标题是 `Worktrace`、尺寸 1100×760（最小 800×600）
   - [ ] 八页导航都在，切页各自发自己的读查询（DevTools 可用；release 里也可以 `F12`）
   - [ ] 捕获一条任务、开始计时、暂停、结束计时 —— 全流程可用
   - [ ] 托盘图标在、五项可点（含「完成」与两条视图跳转）
   - [ ] 关掉主窗（进程仍在 + 托盘在）→ 从托盘重开 ⇒ 第一眼状态正确
   - [ ] 从托盘「退出」⇒ 进程结束、托盘图标消失
   - [ ] **release 里没有控制台**（这是设计）：所有诊断只看
         `%APPDATA%\com.worktrace.desktop\worktrace.log`
   - [ ] 安装包路径下没有把 `DockviewDemo`/`dockview` 打进去（§1 的门禁已覆盖前端产物；
         这一条是人眼复核安装包能正常跑起来）
3. **怎么算不通过**：打包产物起不来 / 起来但一片空白（前端产物没被打进去）/
   `frontendDist` 指向的 `dist/` 缺失 / 托盘或命令在 release 里失效。

### 13.3 退出事务失败的用户可见提示（§6.3-9）

> ⚠️ **这一条已经是可以预期的"未达成"，不是"允许不做"**（fix round 1 / Important-3）。
> P8 计划 **`:327`** 明确订正过前提：P6 的实施已经交付了诊断出口
> （`startup.failed` / `restore.failed` / `backup.prune_failed` 等已落盘）⇒
> 「若 P6 未实施则记『未做，原因：无界面出口』」那个分支**不再成立**，
> **Task 5 必须真的加一步**"制造一次退出失败 ⇒ 有可见提示"。
> **而现状是**：`commands::spawn_tray_quit` 只把失败写进 `worktrace.log`
> （`event=tray.quit.failed`）+ 非零码退出，**界面上没有任何出口**。
> ⇒ 本节的结论按**发现**写：**「P6 的用户可见提示未交付」**，登记为 **V0.1 收尾的未达成项**
> （`docs/validation/p8-acceptance.md` §3），交用户/终审裁决——**不要**写成"允许不做"。

**制造一次退出失败**（最省事的构造：让库里有一条"结束不了"的会话，例如手工把
`work_session` 的一条 `running` 行的 `run_id` 改成一个不存在的 run，或在磁盘上把库置成只读）。

- [ ] 点托盘「退出」后，`worktrace.log` 里有 `event=tray.quit.failed`（带 `code=` 与
      `detail=`）——**这是盘上唯一的痕迹**
- [ ] **观察界面**：有没有任何用户可见提示（弹窗 / 横幅 / 状态栏文案）？逐字抄下来
- [ ] **按事实判定**：观察不到提示时，这一格写
      「**未达成：P6 的用户可见提示未交付**（只有 `worktrace.log` 的 `event=tray.quit.failed`
      与非零退出码）」——这是**发现**，不是"允许不做"（见上面 ⚠️ 与计划 `:327`）
- [ ] 进程行为：非维护态的退出失败**仍然退出**，用**非零码**标出来
      （`echo $LASTEXITCODE` / `echo %ERRORLEVEL%` 抄下来）
- [ ] **维护态**下的拒绝是另一种：进程**留着**，`event=tray.quit.refused`，
      界面照常显示"正在恢复"（这一档不算"退出失败"）

> **登记**：这条的"用户可见提示"半边至今没有实现（`docs/validation/p8-acceptance.md` §3）。
> 本节结论只有两种合法写法：① 观察到了提示（写清在哪、什么字样）；② 没观察到
> （写「未达成：P6 的用户可见提示未交付」+ 把盘上那行诊断抄下来）。**不得留空。**

---

## 14. 结论（由验收人填，别替人写）

- 实机项整体结论：
  - [ ] 全部通过
  - [ ] 不通过（附现象与复现步骤）：
  - 未覆盖 / 存疑：
- **`manual_platform_verified`**：保持 `false` / 可置 `true`（**只有 §0–§13 全部跑完并填了现象、
  §12 在 ≥2 台机器上跑完，才允许置 `true`**）
- 验收人 / 日期（与 §0 的提交号一起填）：
- 与之配套的自动化证据（提交号 + 门禁数字）：
