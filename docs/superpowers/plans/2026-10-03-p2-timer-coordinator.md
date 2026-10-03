# P2 · 计时协调器与检查点实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 做出唯一持有计时内存状态的串行协调器：用同一次时钟采样同时产出区间事实与 tick 快照，并让墙钟与单调钟的分歧能被稳定地检测与分割。

**Architecture:** 新增 `services/timer/`。协调器所有改状态的方法取 `&mut self`，因此进程内由借用检查器保证不会交错；跨命令的串行由 P6/P7 的单实例与调度接线提供。`anchor.rs` 是纯函数（只收数字），可穷举边界；`coordinator.rs` 是唯一把基线、仓储与时钟缝在一起的地方。时间一律经 `Clock::sample()` 一次性取得。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· 无新依赖

**Spec:** `../specs/2026-10-02-worktrace-architecture/08-implementation-contracts.zh.md` §1/§7/§8 · `00-architecture.zh.md` §5 · `02-data-model.zh.md` §3/§6 · `04-functional-spec.zh.md` F-006/F-007

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。时间用例一律用 `FakeClock` 显式推进两个数值。

状态：计划修订待审核；实施未开始。依赖：[P1](2026-10-03-worktrace-v01-foundation.md) 实现验收后，按真实签名复核本计划。上游：[08 §1/7](../specs/2026-10-02-worktrace-architecture/08-implementation-contracts.zh.md)、[总纲](2026-10-03-v01-plan-index.md)。覆盖 F-006/F-007 核心；不做番茄钟、启动扫描、历史确认或 IPC。

## Task 1：串行协调器与快照

文件：services/mod.rs、services/timer/{mod.rs,anchor.rs,snapshot.rs,coordinator.rs}、tests/timer_snapshot.rs。

- [ ] Coordinator 是唯一计时内存持有者；所有采样、命令、查询、系统事件串行进入同一执行边界。&mut self 只是内部约束，正式部署还需 P6/P7 单实例与调度接线。
- [ ] ClockSample 一次生成事实与 DTO，不再次独立取墙钟。TimerSnapshot 包含 data_epoch、revision、run_id、session_id、session_version、tick_seq、as_of、active_ms、state、timer_kind、remaining_ms/overtime_ms；字段命名与 00 统一。
- [ ] snapshot/tick 接受同一个已验证采样；查询也需可变协调器入口，不能通过 &self 绕过检测。tick_seq 当前 run 内递增，新会话不清零；新 run 才重置。
- [ ] active_ms 为有效可信闭合区间和当前可信开放区间暂计之和；recovering 不叠加可疑 live。倒计时预算从数据库取得；正计时剩余/超时为 null。
- [ ] 测试：暂停冻结、倒计时超时、同一采样查询/tick 一致、旧会话/版本 tick 被过滤、预算重新载入不丢失。

## Task 2：统一采样检测与持续归属

文件：anchor.rs、coordinator.rs、tests/timer_clock.rs。

- [ ] Anchor { wall_at, monotonic_at }，A(M)=wall_at+(M-monotonic_at)。检查单调/墙钟倒退、采样失败、相邻增量差与累计偏差；两差任一绝对值 >2000ms 进入异常判断，恰好 2000ms 不超过阈值。
- [ ] 每个命令、tick、查询和系统事件先检测，再决定是否持久化。30 秒只控制检查点频率，不能用于提前跳过异常检测。
- [ ] 连续可信 run 中暂停、继续、结束后开始另一会话均沿用基线；start/resume 的 started_at 取 A(M)。没有可信基线才建立，成功心跳不得重置。
- [ ] 新 run、休眠后或显式校正重新建基线，但先关闭/隔离旧开放事实，清理旧 Instant/区间单调起点；确认新归属与既有人工历史无冲突后才能开新区间。
- [ ] 可信锁屏/休眠边界按策略 pause；延迟或边界不可信则 recovering。唤醒不自动继续；系统事件不受心跳间隔限制。
- [ ] 测试：500ms 回拨暂停/继续不重叠、缓慢累计漂移、1999/2000/2001ms 边界、30 秒前异常、单调回拨、采样失败、可信/延迟系统事件。

## Task 3：start/pause/resume/finish 原语与事务

文件：coordinator.rs、tests/timer_commands.rs。

业务接口：start(request, sample)、pause(request, sample)、resume(request, sample)、finish(request, sample)。request 包含 expected_data_epoch、task/session ID、修改对象的 expected_row_version；start 还带 timer_kind/target_duration_ms。具体 Rust 签名在 P1 真实接口基础上定稿并登记，不能用内部 read_meta 得到的 epoch 代替请求。

- [ ] 先计划内存变化，同一事务检查请求 epoch/version、任务可执行/项目状态、前台占用与历史冲突，再调用 P1 仓储原语。
- [ ] start 同事务理清任务、持久化预算、创建 session/open interval/elapsed=0 初始检查点、审计及 revision；首次估时基准在此冻结。失败不留下半条事实。
- [ ] resume 校验 paused 且无待确认，以 A(M) 开新区间，创建 elapsed=0 检查点并更新 run_id/state/version，不清空 session 已用工时。
- [ ] pause/finish 用已验证单调差关闭区间，保留 sampled_end_wall_at；finish 可从 paused 直接结束。recovering 拒绝普通工作命令，需 P3 reconcile。
- [ ] 一个业务事务只加一次 revision。提交失败不应用内存；提交后内存应用或响应生成失败不得返回普通可重试失败，进入故障恢复并从已提交事实重建。
- [ ] 测试：初始检查点与预算持久化、旧 epoch/version 拒绝、paused 直接 finish、占用冲突、每个写入步骤注入故障整体回滚、提交后故障不重复创建。

## Task 4：可信检查点与异常原子跃迁

文件：coordinator.rs、tests/timer_checkpoint.rs、tests/timer_anomaly.rs。

- [ ] 正常样本可更新最后检测样本；另存最后成功持久化检查点，不能将内存可信点当作恢复事实。正常心跳约每 30 秒写检查点，不加 revision；失败不推进持久化标记，后续可重试。
- [ ] 首次异常立即一次事务：保留最后成功检查点之前可信前缀、创建/标记不确定余段、保存候选采样与原因审计、session 设 recovering/needs_review 并增加 row_version/revision。没有检查点则整段待确认，零长度初始前缀可省略。
- [ ] 提交后停止 live 暂计、释放前台占用、清理开放计时内存。余段有候选归属但不是正在运行的区间，不能建立新的单调运行起点。
- [ ] 已 recovering 的重复事件返回现有恢复结果，不重复分割、审计或增加版本。不可信样本不能写入可信检查点。
- [ ] snapshot 返回此前确认工时与待确认信息分列；后续确认由 P3 完成，P2 不偷偷接受候选时间。
- [ ] 测试：有/无/初始零检查点、心跳失败后的异常、分割失败回滚、重复事件幂等、recovering 暂计冻结、待确认不影响既有闭合统计。

## 完成门槛

在 src-tauri 运行 cargo fmt --check、cargo test、cargo clippy --all-targets，并执行 P1 分层检查。P3 只能消费本计划已验证的事实与原语，不能重新实现一套时钟。

人工平台实验记录机器/系统版本、事件到达延迟和行为：锁屏 30 分钟、休眠/唤醒、正反改时、关窗后继续。正式系统事件接线在 P6/P7 验收前完成，FakeClock 通过不代表实机通过。P2 核心测试可独立完成，平台验收状态另列。
