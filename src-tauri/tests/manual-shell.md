# 外壳人工验收记录（P7 Task 4 / Task 6b 共用）

> **为什么必须实机做**：托盘图标与菜单的真实交互、关掉全部窗口后计时是否继续、重开窗口
> 是否立即拉快照、第二个进程能不能把既有实例的主窗抬起来——这些都需要真实的窗口对象与
> 事件循环。集成测试进程里两者都没有（`tauri::test` 的 mock 运行时本轮没有启用），
> 所以 `tests/shell_lifecycle.rs` 只能钉住**决策函数与命令路径**，钉不住这些现象。
>
> 每一步记**观察到的现象**（数字、截图路径、SQL 结果），不要只写「通过」。
>
> **双窗口同步实验（Task 6a）在另一份**：[`manual-sync.md`](manual-sync.md)——两个窗口之间的
> 三种竞态（末次事件丢失 / 旧响应晚到 / 乱序跳号）与 30 秒收敛的步骤、记录模板都在那里，
> 本文档只管外壳侧（托盘、关窗、单实例）。两份互相引用。

## 0. 环境

| 项 | 值 |
| --- | --- |
| 机器 / CPU | |
| Windows 版本（`winver`） | |
| 提交（`git rev-parse --short HEAD`） | |
| 构建与启动方式（`pnpm tauri dev` / `pnpm tauri build` + 安装包） | |
| 库路径（`%APPDATA%\com.worktrace.desktop\worktrace.db`） | |
| 验收人 / 日期 | |

读库用另一条连接（应用运行中也能读）：`sqlite3 "%APPDATA%\com.worktrace.desktop\worktrace.db"`。

**先取一次本次 run 的 id**（下面各节都用它，别写字面量 `<当前 run>`）：

```sql
SELECT id FROM application_run ORDER BY started_at DESC LIMIT 1;
```

**看诊断输出**（fix round 1，评审 M2）：本轮所有诊断都走 `println!`/`eprintln!`（启动六步、
托盘动作、唤醒接收、退出结果），而 release 的 Windows 子系统没有控制台
（`src/main.rs` 的 `windows_subsystem = "windows"`），输出会被**丢弃**。所以验收用
`pnpm tauri dev`，或者把二进制重定向：`worktrace.exe > log.txt 2>&1`。正式诊断日志归 P6。

## 1. F-011 托盘：四项 + 一个预留项

1. 启动应用，**右键**托盘图标展开菜单：
   - [ ] 可点项恰好四项：**当前任务**、**暂停**、**快速捕获**、**退出**
   - [ ] **完成（P8 启用）** 以禁用（灰）状态出现；点击**没有任何反应** → 现象：
   - [ ] 没有「显示 HUD」一类 V0.1b 项 → 现象：
2. 点「当前任务」；把窗口最小化后再点一次：主窗被抬起并聚焦（最小化时被还原）→ 现象：
3. 点「快速捕获」：主窗被抬起（P7 只保证抬窗；跳转到捕获输入框属 P8）→ 现象：
4. **没有计时**时点「暂停」：
   - [ ] 界面无变化；`SELECT revision FROM app_meta WHERE singleton=1` 不变 →
   - [ ] `SELECT COUNT(*) FROM work_session WHERE run_id = '<上面取到的 run id>'` 不变 →
5. 开始一次计时，**记下界面上的秒数**，再点「暂停」：
   - [ ] 界面在下一拍（≤ 1 秒）变成已暂停，暂停值冻结 →
   - [ ] `SELECT state FROM work_session WHERE run_id = '<上面取到的 run id>'` = `paused`
   - [ ] `revision` 恰好 +1
   - 现象：
6. 再点一次「暂停」：
   - [ ] 什么都不发生（没有第二条 `domain.changed`、`revision` 不变）→ 现象：

## 2. F-009：关掉全部窗口后托盘仍可用、计时继续

> **这一节是两趟（外加收尾的退出），前置状态不同，判据也不同**（fix round 1 复评 N1）：
>
> | 趟 | 关窗前的前置状态 | 只看什么 |
> | --- | --- | --- |
> | **2A** | 计时**正在跑** | 关窗 60 秒后 `interval_checkpoint` 的列值**前进**、`revision` **不变** |
> | **2B** | 计时**已经暂停**（另起一趟） | 重开窗口**第一眼**就是 `paused` |
> | **2C** | 接在 2A 那趟后面（计时仍在跑） | 托盘「退出」后 `clean_exit_at` 与会话收尾 |
>
> ⚠️ 不要混着做：**暂停之后**心跳直接 `Ok(false)`（`coordinator.rs` 的 `heartbeat` 第一句
> 就查 `state != Running`），而且 `pause` 已经闭合了开放区间 ⇒ 2A 那条
> `ended_at IS NULL` 的子查询**取不到行**，你会看到一个空结果，那不是「采样没在跑」，
> 而是「这一趟的前置状态不对」。

### 2A. 计时中关窗：核心还在采样

1. **前置**：开始一次计时（会话 `running`），记下界面秒数（或 `active_ms`）；
   **先查一次检查点并抄下两列的值**（60 秒后要对比）：

   ```sql
   SELECT wall_at, attribution_at, elapsed_ms FROM interval_checkpoint
    WHERE interval_id = (SELECT id FROM work_interval
                          WHERE ended_at IS NULL ORDER BY started_at DESC LIMIT 1);
   ```

   同时抄下 `SELECT revision FROM app_meta WHERE singleton = 1`。
2. **关掉主窗**（标题栏 ×）：
   - [ ] 进程仍在（任务管理器里有 `worktrace.exe`；托盘图标仍在）
   - [ ] 托盘菜单仍能展开、四项仍可点
   - [ ] `SELECT clean_exit_at FROM application_run ORDER BY started_at DESC LIMIT 1` 仍为 NULL
   - [ ] 开放区间仍在：`SELECT COUNT(*) FROM work_interval WHERE ended_at IS NULL` ≥ 1
   - 现象：
3. 关窗后**等 60 秒**，从托盘点「当前任务」重开窗口：
   - [ ] 展示的计时**继续走了这 60 秒**（不是停在关窗那一刻）
   - [ ] **核心判据——检查点在前进**（采样真的还在跑）。⚠️ 界面那个秒数是从
     `started_at` 算出来的：**采样线程就算死了，它照样「继续走」**，所以秒数本身证明不了
     关窗后还在采样。真正的证据在 `interval_checkpoint`，而且它**以 `interval_id` 为主键
     做 upsert**（`src/storage/schema_v1.rs:129`）——**行数不会涨，必须读列值**：
     把第 1 步的 SQL 再跑一次，比列值：

     - [ ] `wall_at` 与 `elapsed_ms` 都**前进**，前进量 **≥ 20 秒**（心跳周期 30 秒、
       采样每秒一拍 ⇒ 正常情况下观察到 30–60 秒；**< 20 秒判不通过**：采样没在跑，
       或心跳没写）。两次读到的值写进现象栏。
   - [ ] `revision` **不变**——心跳有自己的短事务、**不加 revision**
     （原文见 `src/services/timer/coordinator.rs` 的 `heartbeat`：「心跳有自己的短事务，
     且**不加 revision**」；`services/bootstrap.rs` 的 `sample_tick` 说明同）。所以这一栏
     看到 revision 不动是**正确**的，不要把它当成「没有活动」。
     ⚠️ 如果这一栏看到 revision **涨了**，那不是心跳，是别的东西在写（例如你自己点了暂停）：
     先查清再判定。
   - 现象：
4. **接着做 2C**（这一趟的计时还在跑，正好用来验退出收尾）。

### 2B. 另起一趟：暂停后关窗，重开第一眼就该是暂停态

> **前置状态与 2A 不同**：这一趟开始前，计时要**先暂停**（`state = 'paused'`）。
> 这一趟**不看**检查点（暂停后心跳不写、开放区间已闭合，2A 那条 SQL 取不到行），
> 只看「重开窗口第一眼对不对」。

1. **前置**：开始一次计时，然后**暂停**它：
   `SELECT state FROM work_session WHERE run_id = '<上面取到的 run id>'` = `paused`。
2. **关掉主窗**（标题栏 ×），再等 10–30 秒（不必等 60 秒）。
3. 从托盘点「当前任务」重开窗口：
   - [ ] **第一眼**就是 paused（不是先空着/显示「运行中」、等下一拍通知才纠正）。
     **可操作判据**：窗口一出现就去看计时区；若需要「眨一下眼」才变对，就是没通过。
     devtools（`F12`）里也能看到：新窗口先完成握手/拉快照、再收到事件通知
     （Task 2 的启动顺序：先监听再拉一致快照）。
   - [ ] ⚠️ 「秒数马上就是对的」**没有判别力**：有会话时采样每秒广播一次，
     「等下一次通知」的实现看起来一模一样。
     （Rust 半边 = `platform::window` 的 `Rebuild` 分支，已由 `tests/shell_lifecycle.rs`
     钉住；前端半边归 Task 2 的单测。）
   - 现象：

### 2C. 收尾：从托盘「退出」

1. **前置**：至少还有一个**运行中**的会话（接 2A 那趟；若已做 2B，就再开一次计时）。
2. **先关掉全部窗口**（确保「没有窗口也退得掉」），再从托盘点「退出」：
   - [ ] 进程结束，托盘图标消失
   - [ ] `clean_exit_at` 已写入（非 NULL）
   - [ ] 会话被结束：`state = 'finished'`，开放区间已闭合（`ended_at` 非 NULL）
   - 现象：

## 3. F-016：单实例唤起既有主窗（接收侧闭环）

1. 应用运行中，**先关掉主窗**（托盘还在），再从命令行/快捷方式启动第二个实例：
   - [ ] 第二个进程自己退出（不会出现第二个托盘图标）
   - [ ] `SELECT COUNT(*) FROM application_run` **没有增加**（第二个进程不建 run）
   - [ ] 既有实例把主窗**重建**出来并聚焦（约 0.5 秒内，轮询间隔 `ACTIVATION_POLL_INTERVAL_MS`）
   - 现象：
2. 主窗开着时再启动第二个实例：
   - [ ] 既有主窗被**抬起**（不重建、界面状态不丢）
   - 现象：
3. 把主窗最小化后启动第二个实例：
   - [ ] 主窗被还原并聚焦 → 现象：
4. **重新核一遍 `clean_exit_at`**：关掉主窗（不退出）之后启动第二个实例，第二个进程
   **不得**给本次 run 写下 `clean_exit_at`（它连库都不打开）：
   `SELECT clean_exit_at FROM application_run WHERE id = '<上面取到的 run id>'` 仍为 NULL → 现象：

## 4. 结论

- [ ] 全部通过
- [ ] 不通过（附现象与复现步骤）：
- 未覆盖 / 存疑：

## 5. Task 6b 追加（占位，由 6b 填写）

- [ ] **Task 6a 的双窗口同步实机实验**：步骤、判据与记录模板见
  [`manual-sync.md`](manual-sync.md)（第二个窗口 `sync-lab` 与三个 dev 注入开关属
  `src-tauri/` 侧，**尚未落地**——见那份文档 §1）。**自动化那半边已完成**
  （`src/state/__tests__/dualContextSync.test.ts`，提交 `b7b9250`）：它证明的是
  「同一套规则在两个上下文里各自成立」，**真实双 WebView 的广播时序仍须真机跑**。
- [ ] 时序验证：窗口 A 暂停 → 窗口 B 的展示在 30 秒内收敛；窗口 B 隐藏后重新显示时先校验再展示
  → 步骤见 [`manual-sync.md`](manual-sync.md) §2.4 / §2.5
- [ ] F-001 / F-002：捕获、理清、计时非法请求
- [ ] F-020 界面侧：多窗口一致性（判据与自动化对照见 [`manual-sync.md`](manual-sync.md) §4）
- [ ] 对照总纲 §5 第 9 条的权威清单逐条确认（写明「已核对 / 不适用」）
