# 2026-10-05 第一轮实机验收

> 最新状态：测试应用已从托盘正常退出，当前没有持续计时。当前工作区完整 Rust 回归 599 passed / 0 failed / 1 ignored，前端 142 passed。部分实机链路通过；平台验收仍未全部完成，详见文末收口记录。

结论：已执行真实 Windows / Tauri WebView2 操作，部分链路通过；不是完整 F-009/F-011/F-016/F-020 或双窗口竞态验收。manual_platform_verified 仍为 false。

## 环境与范围

- Windows 11，版本 10.0.26300；WebView2 154.0.4258.53（真实 DevTools 进程路径）。
- 记录时 HEAD 为 107959e，累计/待确认时长边界修复已提交；本轮构建使用包含这些修复的工作区源码；debug 二进制，由 pnpm tauri dev 构建，Vite localhost:1420。
- APPDATA 仅对子进程改为 D:/ProJect/worktrace-manual-acceptance/appdata；全新独立测试库，未清理或修改正常工作数据库。
- 主窗 main，实验窗口 sync-lab（通过既有 __p7_open_sync_lab 命令在真实 DevTools 打开）。
- 自动操作者 Codex，computer-use / sky 控制真实窗口，Python sqlite3 只读核对数据库。
- DPI/多显示器参数未采集，本轮不声称覆盖不同缩放。

## 实际操作与证据

| 项 | 观察结果 | 裁定 |
| --- | --- | --- |
| 捕获与开始计时 | UI 捕获“实机验收-20261005-关窗计时”，点击开始后 Doing / running；库 revision=2 | 本轮通过 |
| F-009 §2A 关掉全部业务窗口后后台采样 | main 关闭后 list_windows 未发现 Worktrace 业务窗口。观察超过 60 秒，检查点 elapsed_ms 从 373 增至 121208（+120835ms），wall_at 从 1791182890634 增至 1791183011470；revision 保持 2，run 仍未退出 | 后台采样子项通过；不能替代托盘点击验收 |
| F-016 第二实例通知与重建 | 同一独立 APPDATA 下再次执行 exe，退出码 0，日志 existing_instance_notified / notified=true；主窗 HWND 从 2754084 变为 1642284。最终 application_run 仍仅一行 da1db770-1734-4d14-8923-a7ab4083cc5c | 关闭主窗后重建子项通过；最小化/聚焦分支未完整验 |
| 真实暂停 | UI 暂停后显示 04:24；库 paused / row_version=1，可信闭合 duration_ms=264545，needs_review=0 | UI 暂停子项通过；不是 F-011 托盘暂停 |
| 真实双窗口基础一致性 | sync-lab 初始化显示同一任务与“已暂停 04:24”；main 捕获“实机验收-双窗口广播-01”，切回 sync-lab 后无需手动刷新显示新任务，暂停值保持一致 | 基础一致性已观察；不能区分广播与激活校验，未验收完整竞态 |
| F-009 §2B 暂停态关闭/重建 | 关闭 sync-lab 与 main，再启动第二实例唤起；首份采集到的主窗界面显示“已暂停 04:24”，库仍 paused，没有误续计时 | 本轮通过；没有高帧率首屏录像，瞬时闪烁未排除 |

证据文件（独立测试目录，未提交）：

- D:/ProJect/worktrace-manual-acceptance/before-close.json
- D:/ProJect/worktrace-manual-acceptance/after-close.json
- D:/ProJect/worktrace-manual-acceptance/final-observation.json

最终库 revision=4（捕获、开始、暂停、再捕获各一次），一条 paused 会话，一条可信闭合区间，application_run 一行。真实窗口截图与可访问性观察见本次聊天工具输出；未保存独立截图文件，不能当作可重放录像证据。

## 未完成与观察

1. **F-011 与 F-009 §2C**：当前 sky.list_windows 没有返回任务栏/系统托盘可选窗口；不使用猜坐标或另一套自制 UI 自动化。托盘四个动作与禁用项、无会话/重复暂停零写入、托盘退出 clean_exit_at/进程结束，需要人工操作后再核对库。没有用 UI 暂停替代托盘验收。
2. **双窗口 §2.1–§2.6**：丢事件、旧响应晚到、重播、隐藏/最小化、旧 tick 等竞态未跑；30 秒校验启动计数仍须落实文档规定的开发观测出口。本轮两个真实 WebView 不等于这些全部通过。
3. **启动环境**：沙箱内启动在 setup 阶段 os error 5，使用独立库仍失败；沙箱外同一二进制正常完成 startup。当前证据指向运行权限限制，不据此修改业务代码或记产品启动失败。第一次正常路径启动失败在进入业务前；正式路径运行未做。
4. **开发诊断**：控制台 favicon.ico 404（非阻断）；第二实例退出有 Chrome_WidgetWin_0 unregister error=1412。单实例行为与 run 数正常，记录观察，未证明该诊断影响业务。
5. 本轮不覆盖系统锁屏/休眠/改时、500 ppm 跨机器、安装包/release，也未执行强杀恢复。

## 交接状态

测试应用保留运行，当前计时已暂停；使用独立库，没有持续增加工作时长。Vite 开发服务仍在 1420，供该测试窗口使用。若继续人工验收，请从托盘操作开始；结束时用托盘“退出”，再核对 clean_exit_at 与会话 finished，而不是强杀。完整手工步骤见 src-tauri/tests/manual-shell.md 和 manual-sync.md。

## 第二轮进展（2026-10-05，进行中）

- 人工确认托盘包含四个可点项及灰色“完成（P8 启用）”；仅菜单结构得到确认，未以此代替全部托盘动作。
- 两次真实托盘暂停有运行日志：`tray: 没有运行中的计时，暂停未执行` 连续两条。该阶段数据库 revision=4、paused / row_version=1，后续任务捕获前仍为4；与零写入预期一致。
- 丢一次 domain.changed 后，B 在提交后约17秒采集时仍缺新任务，约44秒再次采集时已有新任务；不能由这个稀疏采样判定30秒门槛。B 未被激活，真实末次通知丢失后的自动收敛已观察。
- DevTools 临时替换 invoke 的尝试得到空 trace，不能作为真实 IPC 时序证据。已新增 debug-only CommandProbe，真实 run_command 包装记录 start/body_complete/return，广播出口记录实际 event_dropped。无新增业务命令、无请求载荷日志、无库写入；工具文档同步。新版已构建并运行；实际观测结果见下节。
- 修改后库单元59通过、Clippy all-targets / fmt / diff通过。dev_injections 首次构建因正在运行的 worktrace.exe 被Windows占用而失败；正常退出后重跑6个测试全部通过，debug构建通过。
- 当前已恢复测试会话：revision=6、running / row_version=2，一条开放区间。人工已点击真实托盘“退出”，随后确认原进程结束；revision=7、会话finished / row_version=3、开放区间为0，clean_exit_at=1791192025309，退出时可信闭合区间duration_ms=2499885、needs_review=0。F-009 §2C运行中正常退出子项通过，证据tray-after-exit.json。


## 第二轮实际时序与重试记录

- **末次广播丢失后的周期校验**：新版实际记录event_dropped（revision=8）。提交入口start=1791192312783，sync-lab的get_revision入口start=1791192320309，相差7526ms；随后实际list_tasks返回，新任务在未激活B的观察中出现。30秒内发起校验子项通过。证据drop-verification.json、round2-runtime.log。
- **旧响应晚到**：使用真实main DevTools调用既有create_task IPC，两次写入相隔1秒，revision从12到13。B旧list_tasks（id103）先完成读取，延迟10000ms后于1791193067570返回；B新list_tasks（id106）于1791193058598返回，比旧响应早约9秒。晚到后观察B仍保留两条任务；其间没有新的list_tasks补救读取。核心旧响应防覆盖通过。触发端采用真实IPC，不能记为全程捕获UI输入版本通过。证据late-response-verification.json。
- **旧事件重播**：真实DevTools调用__p7_replay_event({revision:12})，返回同一epoch的revision12事件；随后B仍保留revision13新增任务。只裁定列表未倒退，不声称零重绘。
- 延迟响应前两次尝试分别未实际注入、未满足旧响应晚于第二次写入的前提，均未作为通过证据。
- **隐藏/恢复尚未完成**：此前点击B最小化后，尚未确认document.visibilityState。重试时旧main、sync-lab与DevTools窗口均不再可选，后台进程仍在；同一独立APPDATA启动第二实例成功通知原进程，并重建main（HWND331868），后端正常list_tasks。自动化返回的可访问性只含空区域，截图出现与目标窗口不一致的内容，不能据此操作或裁定页面空白缺陷。需要恢复有效窗口观察后继续§2.5。

当前测试进程保留运行，第一轮计时会话已正常finished；独立测试库保留。完整托盘动作、隐藏恢复、计时状态双窗收敛及系统事件实机验收仍未全部完成，manual_platform_verified继续为false。以上证据文件位于D:/ProJect/worktrace-manual-acceptance，未纳入仓库。


## 第三轮补验（2026-10-05）

- 窗口观察恢复，重新打开真实sync-lab，两窗计时页均观察到running。
- **暂停状态收敛**：真实UI暂停点击时刻1791195266030，pause_timer入口1791195266138，实际event_dropped revision15；B于1791195266710主动timer_snapshot，观察1791195277598已暂停02:39，距点击约11.6秒。后续恢复、再次暂停，累计11:01停止增长。子项通过；未在通知丢失后的最初短暂区间采到B仍running，不将该瞬时展示勾选为已观察。
- **最小化可见性差异**：原生只读查询返回`isMinimized=true`，同时`document.visibilityState=visible`（1791196017688）。关闭所有DevTools后，观察38秒，sync-lab仍发起get_revision（1791196090404）。因此不是凭观察到窗口遮挡而判隐藏；当前最小化不保证网页hidden，本次§2.5前提未满足。真实hidden停止轮询、显示前校验/首屏顺序仍未完整验收，需要原生窗口可见性适配后复验，归P8桌面生命周期收口；不影响后端计时可信性。
- **关闭实验窗口**：关闭sync-lab后，list_windows仅保留main（331868），主窗仍连接并保留暂停11:01；实验窗关闭独立性子项通过。
- 本轮自动检查：前端14文件/142测试通过，pnpm build通过；Rust lib59测试通过，Clippy all-targets、fmt、release lib check通过；分层脚本在src-tauri目录执行通过。最初在仓库根执行分层脚本因相对路径失败，改正确目录重跑通过。生产前端bundle约728kB，构建有chunk大小提示，作为后续按页面拆包优化观察，不计业务失败。
- 已请求人工补齐真实托盘计时中暂停、快速捕获重建、当前任务恢复、退出；后续以运行日志与只读库核对，不以请求发出代替验收通过。


### 本轮暂存与继续入口

第三轮证据：`round3-verification.json`、`round3-before-tray.json`（独立测试目录）。托盘操作前只读库为revision18，第二轮run=56edf992-5d24-4198-91cb-f3026c2d4fb4未clean退出；测试会话2431ac3b-510d-44d2-a3e6-49f7eb9ec7ca为running / row_version4，1条开放区间。它仅属于隔离测试库。此段覆盖前文历史“已暂停”的交接状态。

待人工操作完成后，先核对真实tray日志、会话状态/版本、开放区间及clean_exit_at，再运行当前工作区完整Rust集成回归。当前轮已完成的自动检查不替代该完整回归，也不替代系统事件、release安装包或真实hidden/首屏验收。当前未发现计时数据损坏或已验证链路的新功能失败，但不能声明全部测试完成。


## 正常退出与完整回归收口（2026-10-05）

用户确认已从托盘退出。真实日志记录run=56edf992-5d24-4198-91cb-f3026c2d4fb4，clean_exit_at=1791196509567，结束会话1；进程查询确认Worktrace已结束。只读库核对：revision18→19，测试会话2431ac3b-510d-44d2-a3e6-49f7eb9ec7ca由running / row_version4变为finished / row_version5，开放区间1→0，两个application_run均有clean_exit_at。退出收尾再次通过，证据`round3-after-exit.json`。此节覆盖此前“运行中、等待退出”的暂存状态。

本轮退出前日志没有新的tray Pause/QuickCapture/CurrentTask动作；用户仅确认退出，所以计时中托盘暂停、快速捕获重建、当前任务恢复仍保留未验证，不把它们与退出一起勾选。

应用结束后执行当前工作区`cargo test --offline --manifest-path src-tauri/Cargo.toml`，退出码0；39个test result汇总：**599 passed / 0 failed / 1 ignored**。忽略项为startup_order中的helper process，由a_killed_lock_holder_releases_the_lock父测试拉起，不是漏跑的产品验收项。前端14文件/142测试、生产前端构建、Rust Clippy all-targets / fmt / release lib check及六项分层检查均已通过。Windows linker有创建导入库/对象的stdout提示，不影响测试结果。

### 给后续开发的状态

- **已收口**：当前自动回归、隔离库正常退出、已记录的双窗旧响应/丢通知/暂停收敛与独立关闭子项。
- **P8桌面收口**：原生最小化与网页可见性适配、真实hidden停止业务轮询、恢复首屏顺序、跳号真实广播子项、上述尚未实际操作的托盘动作与单实例最小化聚焦分支。
- **P6实现 / P8验收**：正式OS事件源与锁屏/休眠/唤醒/改时；release安装包/正式路径、多DPI与跨机器时钟容差另按原计划执行。本轮release lib check不能替代安装包实机验收。
- **范围外**：HUD、Locked/Edit、Mini便签小窗仍未实现，不在本轮已验证功能中。

测试应用已结束，隔离数据库与日志保留，没有进行中的测试计时；未提交、未暂存仓库改动。后续可以继续P5开发，但仍不能宣告完整平台验收或发布门槛通过。manual_platform_verified保持false。
