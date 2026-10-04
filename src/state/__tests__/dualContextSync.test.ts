/**
 * 双窗口同步实验（P7 Task 6a，06 §4「双窗口同步」）：**两个各自独立的 JS 上下文** +
 * 一个共用的假后端，逐条验 00 §5 规则 1–4 在两个上下文里各自成立。
 *
 * ## 这一份证明什么
 *
 * 两个窗口就是两次 `createDomainState(...)`（每个 JS 上下文一个镜像，互不共享内存），
 * 中间只有两条真实系统里也存在的通路：**假后端**（进程侧的真相：`revision` + 业务行 +
 * 计时）与**事件通道**（`syncLabBus` 的广播，替身落在 Tauri 的 `listen` 上）。
 * 事件订阅、暂存、按序交付、水位去重、30 秒校验、旧响应丢弃——跑的都是
 * `src/ipc.ts` 与 `src/state/domainState.ts` 的**生产实现**，一处签名都没为测试改过。
 *
 * ## 这一份不证明什么（不要拿它冒充实机结论）
 *
 * 真实双 WebView 的**广播时序**：Tauri 把一条事件投给两个 WebView 的先后、Windows 上
 * 关窗/休眠时事件通道的行为、以及"注入手段"在真实进程里的可用性。集成测试进程里没有
 * 事件循环，也没有第二个 WebView。实机步骤与记录模板见 `src-tauri/tests/manual-sync.md`。
 *
 * ## 三种竞态的注入手段（都只在测试侧，见 `syncLabBus.ts` / `syncLab.ts` 的模块头）
 *
 * | 竞态 | 注入 | 落点 |
 * | --- | --- | --- |
 * | (a) 末次事件丢失 | `lab.bus.dropNext()` | 事件通道：下一条广播谁都收不到 |
 * | (b) 旧响应晚到 | `win.screen.holdNextRead()` | 这一页的查询：先按发起时刻取数，晚回来 |
 * | (c) 乱序 / 跳号 | `lab.bus.holdNext()` + `releaseHeld()` | 事件通道：扣住第 N 版、先投第 N+1 版 |
 *
 * 每条用例的注释里都写明**把实现改坏成什么样它会红**；其中 (a)(b)(c) 各做过一次反向
 * 验证，原始输出见 `.superpowers/sdd/2026-10-03-p4-projects-tags-today/p7-task6a-report.md`。
 *
 * ## fix round 1（2026-10-04）：替身跟上新页面契约
 *
 * Task 5 的评审 I1 把页面从"推全局水位"改成"**本视图水位**"
 * （`src/components/viewWatermark.ts`：页面查询是过滤 + 分页后的局部视图，不许推
 * `domainState.markApplied`）。本实验的 `createScreen` 当时还是旧口径，四处断言也跟着旧
 * 契约走——**替身落后于生产契约**，已按新契约对齐：旧响应由**本视图水位**判过期，
 * 全局水位只由真正的快照（`get_revision` / `timer_snapshot`）推进。等价的新断言：
 * `screen.watermark()`（本视图水位）到第 6 版 + `state.getView().revision`（全局水位）
 * **仍是第 5 版**——后者正是"局部视图不推全局水位"这条契约的判据。
 */

import { afterEach, describe, expect, it, vi } from "vitest";

// 事件通道替身：`vi.mock` 只作用于**本文件的模块图**，所以 `src/ipc.ts` 的
// `startEventSession` 会去调它，而替身自己（注入原语）住在 `syncLabBus.ts`。
// 用异步工厂 + 动态 import：`vi.mock` 会被提升到 import 之前，静态引用那时还没初始化。
vi.mock("@tauri-apps/api/event", async () => {
  const bus = await import("./syncLabBus");
  return { listen: bus.transport.listen };
});

import { VERIFY_INTERVAL_MS } from "../domainState";
import {
  AT,
  EPOCH,
  RUN,
  SESSION,
  START_REVISION,
  createSyncLab,
  disposeSyncLab,
  type SyncLab,
} from "./syncLab";

let lab: SyncLab | null = null;

/**
 * 当前实验台。`vi.waitFor` 的回调是异步闭包，TS 在它里面收窄不了模块级的 `let lab`。
 */
function rig(): SyncLab {
  if (lab === null) throw new Error("实验台还没建起来");
  return lab;
}

afterEach(async () => {
  if (lab !== null) await disposeSyncLab(lab);
  lab = null;
  vi.useRealTimers();
});

describe("双窗口同步实验：两个上下文 + 一个假后端", () => {
  it("场景 1：A 写之后 B 失效并重新取数（跨上下文一致）；事件不推水位，旧响应不覆盖新状态", async () => {
    // 反向验证（三条，任一即红；fix round 1 后逐条实跑，原始输出见报告「fix round 1」节）：
    // ① **不失效**：`domainState.onDomainChanged` 的 `apply` 分支不再 `publish`
    //    ⇒ "invalidated 1"、被扣住的那次重拉、B 屏上那条新任务同时红；
    // ② **载荷并进镜像**：把事件的 `revision` 直接并进视图（`publish({ revision: … })`）
    //    ⇒ 中间那句"全局水位仍是 5"红；
    // ③ **水位推平**：`ViewWatermark.applied` 空实现（本视图水位不前进）
    //    ⇒ 场景 3 的 `dropped`/`shown` 红。
    lab = await createSyncLab();

    // 两个上下文各自握手、各自读首屏：同一个后端，同一份真相。
    expect(lab.b.state.getView()).toMatchObject({
      dataEpoch: EPOCH,
      revision: START_REVISION,
      invalidated: 0,
    });
    expect(lab.b.state.getView().timer).toMatchObject({ run_id: RUN, session_id: SESSION });
    expect(lab.a.screen.shown()?.titles).toEqual(["写周报"]);
    expect(lab.b.screen.shown()?.titles).toEqual(["写周报"]);
    expect(lab.bus.subscribers()).toBe(2); // 每个上下文一条订阅

    // 注入 (b)：把 B 那条"重拉"的响应扣住，好让"事件到了、数据还没到"这个中间态可观察。
    const release = lab.b.screen.holdNextRead();

    const { change, delivery } = await lab.writeFromA("买牛奶");
    expect(delivery).toBe("delivered");
    expect(change.revision).toBe(START_REVISION + 1);
    // 等那条被扣住的响应真的到达扣留点：**条件驱动**（`vi.waitFor`），不靠"冲几轮微任务"。
    await vi.waitFor(() => expect(rig().b.screen.held()).toBe(1));

    // B：收到通知 ⇒ **只作缓存失效**。水位没有被事件推走（载荷不并进镜像），屏上还是旧列表。
    expect(lab.b.state.getView().invalidated).toBe(1);
    expect(lab.b.state.getView().revision).toBe(START_REVISION);
    expect(lab.b.screen.shown()).toMatchObject({ titles: ["写周报"], revision: START_REVISION });
    expect(lab.b.calls.listTasks).toBe(2); // 首屏 + 这次重拉

    release();
    await lab.settle();

    // B 自己拉回来的那份 ⇒ 两个上下文收敛到同一份真相（屏上内容 + 本视图水位）。
    expect(lab.b.screen.dropped()).toBe(0);
    expect(lab.b.screen.applied()).toBe(2);
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶"],
      revision: START_REVISION + 1,
      total: 2,
    });
    // 推的是**本视图水位**（页面契约 `src/components/viewWatermark.ts`）：页面那条
    // `list_tasks` 是过滤 + 分页后的局部视图，不许推全局水位（Task 5 fix round 1 / I1）。
    expect(lab.b.screen.watermark()).toEqual({ data_epoch: EPOCH, revision: START_REVISION + 1 });
    // 全局水位仍停在握手那一版：局部视图上屏**不**推它。推了会吞掉同 `revision` 的失效
    // 通知、并让 30 秒校验失去判据 —— 这条就是新契约的判据（旧口径下这里会变成第 6 版）。
    expect(lab.b.state.getView().revision).toBe(START_REVISION);
    expect(lab.a.screen.shown()?.titles).toEqual(["写周报", "买牛奶"]);
    expect(lab.a.screen.watermark()).toEqual({ data_epoch: EPOCH, revision: START_REVISION + 1 });
    expect(lab.a.state.getView().revision).toBe(START_REVISION);
  });

  it("场景 2：(a) 末次事件丢失 ⇒ 仍在 30 秒校验周期内收敛（假时钟，不等真的 30 秒）", async () => {
    // 反向验证：把 `domainState.verify()` 里"版本比已见版本靠前 ⇒ resync"那句去掉
    // ⇒ "第 30 秒收敛"整段红：B 会永远停在旧列表（fix round 1 实跑：场景 2/5 同时红）。
    //
    // ⚠️ **「30 秒」这个数值不在这里钉**（订正，评审实跑证伪）：本用例 advance 的是 import
    // 进来的那个常量**本身**，是纯相对计时——把 `VERIFY_INTERVAL_MS` 改成 60_000 / 1_000 /
    // 31_000，下面六条断言照样全绿。常量值由既有的
    // `domainState.test.ts` 的 `expect(VERIFY_INTERVAL_MS).toBe(30_000)` 钉住，不是本轮的功劳。
    // 这里钉的是**路径**：通知丢了以后，靠周期校验收敛（而不是靠别的通知/重试）。
    vi.useFakeTimers();
    lab = await createSyncLab();
    expect(lab.b.screen.shown()?.titles).toEqual(["写周报"]);

    // 注入 (a)：这条 `domain.changed` 谁都收不到（末次事件丢失）。
    lab.bus.dropNext();
    const { change, delivery } = await lab.writeFromA("买牛奶");
    expect(delivery).toBe("dropped");
    expect(lab.bus.stats.dropped).toBe(1);
    await lab.settle();

    // 事件真的没到：B 既没失效、也没重拉，屏上还是旧列表；库里已经是第 6 版。
    expect(lab.backend.revision()).toBe(change.revision);
    expect(lab.b.state.getView().invalidated).toBe(0);
    expect(lab.b.calls.getRevision).toBe(1); // 只有启动那次握手
    expect(lab.b.calls.listTasks).toBe(1); // 只有首屏那次读
    expect(lab.b.screen.shown()?.titles).toEqual(["写周报"]);

    // 29.999 秒：什么都不该发生（"至多每 30 秒"是上界，不是"每 30 秒必有一次动作"）。
    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS - 1);
    expect(lab.b.screen.shown()?.titles).toEqual(["写周报"]);

    // 第 30 秒：校验发现库里版本靠前 ⇒ 合并刷新 + 重拉一份，收敛到 A 写的那份真相。
    await vi.advanceTimersByTimeAsync(1);
    await lab.settle();
    expect(lab.b.calls.getRevision).toBe(2);
    expect(lab.b.state.getView().invalidated).toBe(1);
    expect(lab.b.state.getView().revision).toBe(change.revision);
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶"],
      revision: change.revision,
    });
  });

  it("场景 3：(b) 旧响应晚到 ⇒ 被丢弃，不覆盖已经上屏的新状态", async () => {
    // 反向验证（fix round 1 后实跑，原始输出见报告）：
    // ① **水位推平**：`src/components/viewWatermark.ts` 的 `applied` 空实现（本视图水位
    //    永不前进）⇒ 那条旧响应判不出过期、直接上屏 ⇒ `dropped()` 0、`shown()` 被打回旧列表；
    // ② 同理，把 `isStale` 改成恒 false（或让 `createScreen` 不过水位）⇒ 同上。
    lab = await createSyncLab();

    // B 先发一条查询（等价于页面自己那次重拉）：数据按**发起那一刻**的真相取好
    // （第 5 版、只有"写周报"），然后被扣住——它要在 A 写完之后才回到这一页。
    const release = lab.b.screen.holdNextRead();
    const inflight = lab.b.screen.reload();
    await vi.waitFor(() => expect(rig().b.screen.held()).toBe(1));
    expect(lab.b.calls.listTasks).toBe(2);

    const { change } = await lab.writeFromA("买牛奶");
    await lab.settle();
    // B 收到通知后重拉，拿到第 6 版并**上屏**（本视图水位随之上到第 6 版）。
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶"],
      revision: change.revision,
    });
    expect(lab.b.screen.watermark()).toEqual({ data_epoch: EPOCH, revision: change.revision });
    expect(lab.b.state.getView().revision).toBe(START_REVISION); // 全局水位没被页面推走

    // 现在才放行那条旧响应（第 5 版、旧列表）：它比**本视图已上屏**的那份旧。
    release();
    await inflight;
    await lab.settle();

    expect(lab.b.screen.dropped()).toBe(1);
    expect(lab.b.screen.applied()).toBe(2); // 首屏 + 新那份；旧的没有上屏
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶"],
      revision: change.revision,
    });
    expect(lab.b.screen.watermark()).toEqual({ data_epoch: EPOCH, revision: change.revision });
    expect(lab.b.state.getView().revision).toBe(START_REVISION);
  });

  it("场景 4：(c) 通知跳号 ⇒ 取新快照并收敛；迟到的补号通知不改变最终状态", async () => {
    // 反向验证：把闸门④（`revision > seen + 1 ⇒ resync`）删掉 ⇒ 跳到第 7 版那条会被
    // 当成连续的一条接纳：`timerSnapshot` 计数（唯一能分开"取新快照"与"就地接纳"的判据）
    // 与"水位到 7"同时红（fix round 1 实跑：`expected 1 to be 2`）。
    //
    // ⚠️ **订正（评审实跑证伪）**：这里原先写"把闸门②删掉 ⇒ 后半段红"——**不成立**。
    // `onNotification` 里闸门②（`<= applied.revision`）**不可达**：`applySnapshot` 恒有
    // `seen >= applied.revision`（`seen = max(seen, revision)`，`markApplied` 走同一条路），
    // 所以②能挡的通知③（`<= seen`）一定也挡得住。迟到的第 6 版是被**闸门③**挡下的；
    // ②只在文档里对照（黑盒用例杀不掉它，这是它的性质，不是覆盖缺口）。
    lab = await createSyncLab();

    // 真相连写两条（第 6、7 版），但第 6 版那条通知被扣住：
    // B 看到的是 5 → 7（**跳号**，不是连续两拍）。
    lab.bus.holdNext();
    const first = await lab.writeFromA("买牛奶");
    const second = await lab.writeFromA("倒垃圾");
    expect(first.delivery).toBe("held");
    expect(second.delivery).toBe("delivered");
    expect(lab.bus.stats.held).toBe(1);
    await lab.settle();

    // 闸门④：跳号无法证明一致 ⇒ 合并刷新 + 取一份新快照（**不是**就地补课）。
    // `timerSnapshot` 从 1 涨到 2 就是"真的取了一份新快照"的判据：只推失效、不取快照的实现
    // 在屏上看起来一样（都重拉了列表），这条断言是唯一能分开它们的地方。
    expect(lab.b.calls.timerSnapshot).toBe(2); // 启动那次 + 跳号这次
    expect(lab.b.state.getView().invalidated).toBe(1);
    expect(lab.b.state.getView().revision).toBe(second.change.revision);
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶", "倒垃圾"],
      revision: second.change.revision,
    });

    const snapshotsAfterResync = lab.b.calls.timerSnapshot;
    const invalidatedAfterResync = lab.b.state.getView().invalidated;

    // 迟到的第 6 版现在才投出来：同 epoch 且 `revision <=` 已应用水位 / 已见版本 ⇒ 丢弃
    // （这一格两者都在它之上，真正挡下它的是**闸门③**；闸门②在 `onNotification` 里不可达）。
    expect(lab.bus.releaseHeld()).toBe(1);
    await lab.settle();

    expect(lab.b.state.getView().revision).toBe(second.change.revision);
    expect(lab.b.state.getView().invalidated).toBe(invalidatedAfterResync);
    expect(lab.b.calls.timerSnapshot).toBe(snapshotsAfterResync); // 没有再取一次快照
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶", "倒垃圾"],
      revision: second.change.revision,
    });
  });

  it("场景 4 附加：重复通知由**闸门③**单独挡下（`applied` 还在第 5 版时也只有它能挡）", async () => {
    // 反向验证（fix round 1 实跑）：只把闸门③（`revision <= seen ⇒ drop`）删掉 ⇒ 下面
    // `invalidated` 涨到 2、`timerSnapshot` 也多一次 ⇒ 红。只删闸门②**不会**红——
    // 这一格 `applied` 还是 5，`6 <= 5` 为假（而且②本来就不可达，见场景 4 头的订正）。
    lab = await createSyncLab();

    // 让这次重拉**不落地**：B 的本视图水位与全局水位都停在第 5 版，屏上也还是旧列表。
    const release = lab.b.screen.holdNextRead();
    const { envelope } = await lab.writeFromA("买牛奶");
    await vi.waitFor(() => expect(rig().b.screen.held()).toBe(1));

    // 事件已经把 `seen` 推到第 6 版（闸门记下了这一版），但 `applied` 仍是第 5 版
    // （镜像视图的 `revision` 就是 `applied` 的投影）：重播同一条 rev 6 时闸门② 的
    // `6 <= 5` 不成立 —— 唯一能挡下它的是闸门③（`6 <= seen`）。
    expect(lab.b.state.getView().invalidated).toBe(1);
    expect(lab.b.state.getView().revision).toBe(START_REVISION);
    const snapshots = lab.b.calls.timerSnapshot;

    expect(lab.bus.broadcast(envelope)).toBe("delivered"); // 同一条 rev 6 又真的投了一次
    await lab.settle();
    expect(lab.b.state.getView().invalidated).toBe(1); // 闸门③ ⇒ drop
    expect(lab.b.calls.timerSnapshot).toBe(snapshots); // 也没有因此取快照

    release();
    await lab.settle();
    // 被扣住的那次重拉照常落地（它比本视图水位新），两个上下文仍一致。
    expect(lab.b.screen.shown()?.titles).toEqual(["写周报", "买牛奶"]);
    expect(lab.b.screen.watermark()).toEqual({ data_epoch: EPOCH, revision: START_REVISION + 1 });
  });

  it("场景 5：A 暂停之后 B 的展示在 30 秒校验周期内收敛（同一套假时钟）", async () => {
    // 反向验证：把 `orderTimer` 判据 4 里"权威样本可以落定更新的会话版本"改成一律
    // `stale`/`resync` ⇒ B 的展示永远停在 running ⇒ 红；把 30 秒轮询摘掉 ⇒ 同样红。
    vi.useFakeTimers();
    lab = await createSyncLab();
    expect(lab.a.state.getView().timer?.state).toBe("running");
    expect(lab.b.state.getView().timer).toMatchObject({ state: "running", session_version: 1 });

    // 注入 (a)：暂停那条 `domain.changed` 也丢掉——失效的只有"另一条路"。
    lab.bus.dropNext();
    const { outcome, delivery } = await lab.pauseFromA();
    expect(delivery).toBe("dropped");
    expect(outcome.snapshot.state).toBe("paused");
    await lab.settle();
    // 命令在 A 这边提交了（真相已经是 paused），B 的展示还停在旧状态。
    expect(lab.backend.timer()).toMatchObject({ state: "paused", session_version: 2 });
    expect(lab.b.state.getView().timer).toMatchObject({ state: "running", session_version: 1 });

    // 30 秒校验：版本靠前 ⇒ 取新快照 ⇒ 展示收敛（同一套假时钟，不真等 30 秒）。
    await vi.advanceTimersByTimeAsync(VERIFY_INTERVAL_MS);
    await lab.settle();
    expect(lab.b.state.getView().revision).toBe(outcome.revision);
    expect(lab.b.state.getView().timer).toMatchObject({
      state: "paused",
      session_version: 2,
      session_id: SESSION,
      data_epoch: EPOCH,
      run_id: RUN,
      as_of: AT,
    });
  });
});
