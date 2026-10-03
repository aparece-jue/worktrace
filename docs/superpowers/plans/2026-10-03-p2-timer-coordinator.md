# P2 · 计时协调器与检查点实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 做出唯一持有计时内存状态的串行协调器：用同一次时钟采样同时产出区间事实与 tick 快照，并让墙钟与单调钟的分歧能被稳定地检测与分割。

**Architecture:** 新增 `services/timer/`。协调器所有改状态的方法取 `&mut self`，因此进程内由借用检查器保证不会交错；跨命令串行由 P7 的共享协调器执行边界与调度接线提供，P6 后续硬化；单实例不替代进程内串行。`anchor.rs` 是纯函数（只收数字），可穷举边界；`coordinator.rs` 是唯一把基线、仓储与时钟缝在一起的地方。时间一律经 `Clock::sample()` 一次性取得。

**Tech Stack:** Rust 1.98 · rusqlite 0.40（沿用 P1）· 无新依赖

**Spec:** `../specs/2026-10-02-worktrace-architecture/08-implementation-contracts.zh.md` §1/§7/§8 · `00-architecture.zh.md` §5 · `02-data-model.zh.md` §3/§6 · `04-functional-spec.zh.md` F-006/F-007

**断言口径：** 见 [总纲](2026-10-03-v01-plan-index.md) §5 第 8 条。时间用例一律用 `FakeClock` 显式推进两个数值。

**开工前必须先做的一件事**：06 §4 把「单调/墙钟映射」列为**实现前**技术验证，结论可能反过来改本计划的阈值与状态机（总纲 §5 第 9 条）。先做实验、记录机器/系统版本与休眠/锁屏/改时的实际行为，再动手写协调器。**不要把这条留到完成门槛**——那时设计已经定了。

状态：计划修订待审核；实施未开始。依赖：[P1](2026-10-03-worktrace-v01-foundation.md) **已实现并验收**（69 个测试全绿），本计划里的 P1 接口是真实签名，不是预设。上游：[08 §1/7](../specs/2026-10-02-worktrace-architecture/08-implementation-contracts.zh.md)、[总纲](2026-10-03-v01-plan-index.md)。覆盖 F-006/F-007 核心；不做番茄钟、启动扫描、历史确认或 IPC。

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
- [ ] **动手前先读 `docs/validation/p2-clock-mapping.md`，那里有一个会改变本条实现的实测结果**：这台机器上相邻增量差始终是 ±1ms，但**累计偏差按约 14ms/分钟单向增长**——照这个速率，约 2.4 小时后「相对 run 锚点的绝对累计偏差」会**在无任何用户干预的正常会话里自己越过 2000ms**，把一个健康会话判成异常。所以累计判据不能照字面实现：要么在**每次成功心跳时前移锚点**（心跳本来每 30 秒一次，是天然的重新基准点），要么改成**速率**判据而不是绝对毫秒数。该结论的成因（真实漂移 vs Windows 计时器量化）尚未定论，**先按记录表第 7 节逐条确认再定稿**。
- [ ] 每个通过请求校验的命令、tick、查询和系统事件先检测，再决定是否持久化；用户命令先拒绝旧 epoch/旧版本请求，不借无效请求触发系统写入。30 秒只控制检查点频率，不能用于提前跳过异常检测。
- [ ] 连续可信 run 中暂停、继续、结束后开始另一会话均沿用基线；start/resume 的 started_at 取 A(M)。没有可信基线才建立，成功心跳不得重置。
- [ ] 新 run、休眠后或显式校正重新建基线，但先关闭/隔离旧开放事实，清理旧 Instant/区间单调起点；确认新归属与既有人工历史无冲突后才能开新区间。
- [ ] 可信锁屏/休眠边界按策略 pause；延迟或边界不可信则 recovering。唤醒不自动继续；系统事件不受心跳间隔限制。
- [ ] 测试：500ms 回拨暂停/继续不重叠、缓慢累计漂移、1999/2000/2001ms 边界、30 秒前异常、单调回拨、采样失败、可信/延迟系统事件。

## Task 3：start/pause/resume/finish 原语与事务

文件：coordinator.rs、storage/task_repo.rs、storage/session_repo.rs、storage/time_edit_repo.rs、storage/mod.rs、tests/timer_commands.rs。

公开入口：start(request)、pause(request)、resume(request)、finish(request)。先校验请求，再由协调器调用 Clock::sample()；调用方不得预先传入采样。内部事务原语才接收已验证样本；采样失败走异常路径，不伪造 ClockSample。request 包含 expected_data_epoch、task/session ID、修改对象的 expected_row_version；start 还带 timer_kind/target_duration_ms。具体 Rust 签名在 P1 真实接口基础上定稿并登记，不能用内部 read_meta 得到的 epoch 代替请求。
- [ ] **与 P1 已实现的 `commands::envelope::WriteEnvelope` 的关系**：它只有**一个** `expected_row_version: Option<i64>`（P1 已落地，但目前除自身单测外**没有任何消费者**）。本计划的 `resume` 要同时校验任务与会话两份版本，**装不进这个信封**。定稿时二选一并在总纲登记：① 多对象命令的 request 直接带各自版本字段，`WriteEnvelope` 只服务**单对象更新**（P4 的项目/标签改动、P7 的简单修改）——此时 P4 必须真的用它，否则它就是死代码；② 把信封扩展成可承载多对象版本。**倾向前者**：信封的语义是「一次写一个对象」，硬塞两份版本会让「幂等关系增删不伪造实体版本」这条规则更难表达。

- [ ] 串行边界内先检查请求 epoch/version 和目标存在性，再取得样本并检测；正常路径先计划内存变化，同一业务事务再次校验 epoch/version、任务可执行/项目状态、前台占用与历史冲突，再调用 P1 仓储原语。若检测异常，按总纲 §9 独立系统事务提交恢复状态，原用户命令返回 RECOVERY_REQUIRED，不执行原意图；响应携带该事务后的权威版本，不自动重试。
- [ ] start 同事务理清任务、持久化预算、创建 session/open interval/elapsed=0 初始检查点、审计及 revision；首次估时基准在此冻结。失败不留下半条事实。
- [ ] **「理清」是两步，不是一步**：02 §5 里 `Inbox → Doing` **不合法**，必须 `Inbox → Ready → Doing`；`Clarifying` 出发同理。F-002 要求 `start` 在同一命令内原子完成它，所以这里是同一事务里的**两次** `task_repo::transition_task`（各自的 `expected_row_version` 要按前一步的结果递进），**不是**把状态直接写成 Doing。直接跳会被跃迁表拒绝——P1 的测试已经把这个陷阱钉住了。
- [ ] resume 校验 paused 且无待确认、项目可执行及前台占用；任务 Doing 保持，Ready 同事务转 Doing；Inbox/Clarifying/Waiting/Blocked/Review/Done/Cancelled/Scheduled 拒绝，不隐式解除等待或重开任务。请求携带 task 与 session 两份 expected_row_version，采样前及事务内均校验。先更新 session 为 running 及当前 run_id，再以 A(M) 开新区间、写 elapsed=0 检查点；同事务提交，不清空已用工时，返回任务/会话权威版本。
- [ ] pause/finish 用已验证单调差关闭区间，保留 sampled_end_wall_at；finish 可从 paused 直接结束。recovering 拒绝普通工作命令，需 P3 reconcile。
- [ ] P2 扩展 P1 仓储：task_repo 读取估时并冻结 baseline_estimate_json；session_repo 更新 run_id/needs_review、分割可信前缀与不确定余段；新增 time_edit_repo 写异常审计。签名实现时登记并供 P3 复用。仅接受调用方 Transaction，不自行提交或加 revision；协调器不嵌 SQL，不改 schema。无估时的首次 start 也以首次会话事实标识已冻结，后续不得因 baseline 为 null 重新冻结。
- [ ] 测试：首次有/无估时冻结及后续不覆盖；resume 各任务状态与两份版本校验；新增仓储字段和审计失败同事务回滚。
- [ ] 一个业务事务只加一次 revision。提交失败不应用内存；提交后内存应用或响应生成失败不得返回普通可重试失败，进入故障恢复并从已提交事实重建。
- [ ] 测试：初始检查点与预算持久化、旧 epoch/version 拒绝（同时有异常仍不借此请求写入）、paused 直接 finish、占用冲突、每个写入步骤注入故障整体回滚（比较涉及记录及区间字段，不只计行数）、有效命令遇异常仅提交系统恢复事务、提交后故障不重复创建。

## Task 4：可信检查点与异常原子跃迁

文件：coordinator.rs、storage/session_repo.rs、storage/time_edit_repo.rs、tests/timer_checkpoint.rs、tests/timer_anomaly.rs。

- [ ] 正常样本可更新最后检测样本；另存最后成功持久化检查点，不能将内存可信点当作恢复事实。正常心跳约每 30 秒写检查点，不加 revision；失败不推进持久化标记，后续可重试。
- [ ] **登记 `checkpoint_repo::write` 已经强制的三条（P1 已实现，违反会被运行期拒绝）**：
  1. **`attribution_at == interval.started_at + elapsed_ms`**。`elapsed_ms` 是**本区间**的已过时长，**不是**会话累计——`resume` 开新区间时它从 0 重新起算，所以写心跳时用的是区间自己的起点。用会话累计值会被拒。
  2. **只接受可信的 running 区间**：区间已闭合、已作废、`needs_review=1`，或会话不是 `running`（含 `recovering`、`needs_review=1`），一律拒绝。也就是说异常一发生，心跳就自然停下，不需要额外判断。
  3. **不得相对上一条倒退**：`run_id` 必须相同，且 `elapsed_ms` / `attribution_at` / `wall_at` 都不得回退。这条与「异常分割后不允许再写可信检查点」是同一个保护的两面。
- [ ] 首次异常立即一次事务：保留最后成功检查点之前可信前缀、创建/标记不确定余段、保存候选采样与原因审计、session 设 recovering/needs_review 并增加 row_version/revision。没有检查点则整段待确认，零长度初始前缀可省略。
- [ ] 提交后停止 live 暂计、释放前台占用、清理开放计时内存。余段有候选归属但不是正在运行的区间，不能建立新的单调运行起点。
- [ ] 已 recovering 的重复事件返回现有恢复结果，不重复分割、审计或增加版本。不可信样本不能写入可信检查点。
- [ ] snapshot 返回此前确认工时与待确认信息分列；正常查询不加 revision，异常检测可提交独立系统恢复事务，随后返回该提交后的权威快照。后续确认由 P3 完成，P2 不偷偷接受候选时间。
- [ ] 测试：有/无/初始零检查点、心跳失败后的异常、分割失败字段级回滚、重复事件幂等、recovering 暂计冻结、待确认不影响既有闭合统计、正常/异常查询 revision 口径。异常事务失败不得输出新的可信暂计，内存不应用未提交状态，明确进入故障处理。

## 完成门槛


- [ ] 对照 [总纲](2026-10-03-v01-plan-index.md) §5 第 9 条的权威清单（02 §8 必测案例、04 §9 集成用例、06 §4 实验）逐条确认与本计划相关的条目，并在验收记录里写明「已核对 / 不适用」。
在 src-tauri 运行 cargo fmt --check、cargo test、cargo clippy --all-targets，并执行 P1 分层检查。**本计划会改 P1 的 `task_repo`/`session_repo`/`mod.rs`，所以必须显式确认 P1 的 69 个测试无回归**——`cargo test` 虽然会跑到它们，但「跑到了」不等于「核对过」。P3 只能消费本计划已验证的事实与原语，不能重新实现一套时钟。

人工平台实验记录机器/系统版本、事件到达延迟和行为：锁屏 30 分钟、休眠/唤醒、正反改时、关窗后继续。P2 开工前完成独立探针验证；P7 建立正式系统事件接线并验证到达延迟及关窗行为；P6 硬化故障路径；P8 完成最终实机验收。FakeClock 通过不代表实机通过；P2 核心验收与后续平台验收分别列状态。

## 跨计划接缝

- [ ] 为 P3 的任务状态编排提供同一事务中的暂停/结束原语和提交后内存应用计划；不让组合服务调用已自行提交的公开命令。
- [ ] 为 P5 提供内部统计采样接缝：已验证样本、开放 interval_id、归属终点及对应 run/session_version；与一致数据库读快照在同一串行边界完成，避免闭合工时与 live 重复计入。
- [ ] P6 维护态期间禁止所有采样写入；库切换成功或失败重开后均从持久化事实重建运行态，不复用旧 Instant。

## 开工前时钟探针的交付物

- [ ] 新增 src-tauri/examples/clock_probe.rs 与 docs/validation/p2-clock-mapping.md 记录模板。探针仅依赖 platform::clock，不依赖协调器或正式事件接线，不自动修改系统时间；锁屏、休眠、正反改时由人工操作。
- [ ] 记录机器/系统版本、两个时钟采样序列、操作时点、观察结果、阈值结论与未验证项；完成后再实现协调器。探针不证明系统通知可靠性，事件边界与延迟由 P7 实测，P8 最终验收。
