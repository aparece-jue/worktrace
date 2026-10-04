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

afterEach(async () => {
  if (lab !== null) await disposeSyncLab(lab);
  lab = null;
  vi.useRealTimers();
});

describe("双窗口同步实验：两个上下文 + 一个假后端", () => {
  it("场景 1：A 写之后 B 失效并重新取数（跨上下文一致）；事件不推水位，旧响应不覆盖新状态", async () => {
    // 反向验证（三条，任一即红）：
    // ① 把 `domainState.onDomainChanged` 的 `apply` 分支去掉 ⇒ "invalidated 1" 与
    //    B 屏上那条新任务同时红（事件不再让缓存失效）；
    // ② 把事件的 `revision` 直接并进视图/水位（"载荷并进镜像"）⇒ 中间那两条
    //    "水位仍是 5 / 屏上仍是旧列表" 红；
    // ③ 去掉 `domainState.isStaleResponse`（或让它恒 false）⇒ 场景 3 的 dropped 红。
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
    await lab.settle();

    // B：收到通知 ⇒ **只作缓存失效**。水位没有被事件推走（载荷不并进镜像），屏上还是旧列表。
    expect(lab.b.state.getView().invalidated).toBe(1);
    expect(lab.b.state.getView().revision).toBe(START_REVISION);
    expect(lab.b.screen.shown()).toMatchObject({ titles: ["写周报"], revision: START_REVISION });
    expect(lab.b.screen.held()).toBe(1);
    expect(lab.b.calls.listTasks).toBe(2); // 首屏 + 这次重拉

    release();
    await lab.settle();

    // B 自己拉回来的那份才推水位 ⇒ 两个上下文收敛到同一份真相。
    expect(lab.b.screen.dropped()).toBe(0);
    expect(lab.b.screen.applied()).toBe(2);
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶"],
      revision: START_REVISION + 1,
      total: 2,
    });
    expect(lab.b.state.getView().revision).toBe(START_REVISION + 1);
    expect(lab.a.screen.shown()?.titles).toEqual(["写周报", "买牛奶"]);
    expect(lab.a.state.getView().revision).toBe(START_REVISION + 1);
  });

  it("场景 2：(a) 末次事件丢失 ⇒ 仍在 30 秒校验周期内收敛（假时钟，不等真的 30 秒）", async () => {
    // 反向验证：把 `domainState.verify()` 里"版本比已见版本靠前 ⇒ resync"那句去掉
    // （或把 `startPolling` 的 30 秒轮询摘掉）⇒ "第 30 秒收敛"整段红：B 会永远停在旧列表。
    // 另一条：把 `VERIFY_INTERVAL_MS` 改成 60_000 ⇒ "29.999 秒不动"仍绿，
    // 但第 30 秒那条红（说明这条用例验的确实是"至多 30 秒"这个上界）。
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
    // 反向验证：把 `createScreen` 里那句 `isStaleResponse` 去掉（页面不再过水位），
    // 或让 `FreshnessGate.isStaleResponse` 恒 false ⇒ `dropped()` 0、屏上被旧列表覆盖 ⇒ 全红。
    // 把 `markApplied` 改成空实现 ⇒ 水位不前进 ⇒ 旧响应判不出过期 ⇒ 同样红。
    lab = await createSyncLab();

    // B 先发一条查询（等价于页面自己那次重拉）：数据按**发起那一刻**的真相取好
    // （第 5 版、只有"写周报"），然后被扣住——它要在 A 写完之后才回到这一页。
    const release = lab.b.screen.holdNextRead();
    const inflight = lab.b.screen.reload();
    await lab.settle();
    expect(lab.b.screen.held()).toBe(1);
    expect(lab.b.calls.listTasks).toBe(2);

    const { change } = await lab.writeFromA("买牛奶");
    await lab.settle();
    // B 收到通知后重拉，拿到第 6 版并**上屏**（水位随之上到第 6 版）。
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶"],
      revision: change.revision,
    });
    expect(lab.b.state.getView().revision).toBe(change.revision);

    // 现在才放行那条旧响应（第 5 版、旧列表）：它回答的已经不是现在这个世界。
    release();
    await inflight;
    await lab.settle();

    expect(lab.b.screen.dropped()).toBe(1);
    expect(lab.b.screen.applied()).toBe(2); // 首屏 + 新那份；旧的没有上屏
    expect(lab.b.screen.shown()).toMatchObject({
      titles: ["写周报", "买牛奶"],
      revision: change.revision,
    });
    expect(lab.b.state.getView().revision).toBe(change.revision);
  });

  it("场景 4：(c) 通知跳号 ⇒ 取新快照并收敛；迟到的补号通知不改变最终状态", async () => {
    // 反向验证：把闸门④（`revision > seen + 1 ⇒ resync`）删掉 ⇒ 跳到第 7 版那条会被
    // 当成连续的一条接纳，"取新快照"与"水位到 7"同时红；把闸门②（`<= 已应用水位 ⇒ drop`）
    // 删掉 ⇒ 迟到的第 6 版会被接纳（`invalidated` 再涨一次）⇒ 后半段红。
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

    // 迟到的第 6 版现在才投出来：同 epoch 且 `revision <=` 已应用水位 ⇒ 丢弃。
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

  it("场景 4 附加：重复通知（同版本再来一条）不再失效、也不再取快照", async () => {
    // 反向验证：把闸门③（`revision <= seen ⇒ drop`）删掉 ⇒ `invalidated` 涨到 2 ⇒ 红。
    lab = await createSyncLab();
    const { envelope } = await lab.writeFromA("买牛奶");
    await lab.settle();
    expect(lab.b.state.getView().invalidated).toBe(1);

    const snapshots = lab.b.calls.timerSnapshot;
    expect(lab.bus.broadcast(envelope)).toBe("delivered"); // 同一条真的又投了一次
    await lab.settle();

    expect(lab.b.state.getView().invalidated).toBe(1);
    expect(lab.b.calls.timerSnapshot).toBe(snapshots);
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
