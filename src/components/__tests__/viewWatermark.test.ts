/**
 * 视图水位用例（P7 Task 5 fix round 1，评审 I1）。
 *
 * 钉的是页面判据②那一条**纯函数**：epoch 变了算旧、同 epoch 比我上屏过的那份旧算旧、
 * 同版本不算旧、水位只前进、还没上屏过就不算旧。
 *
 * 全局水位（`domainState` 的闸门）**不在这里**：它有镜像自己的用例，而"页面查询不得推进它"
 * 是页面级行为，回归用例在 `src/pages/__tests__/Tasks.test.tsx`（I1-A / I1-C 两条）。
 */

import { describe, expect, it } from "vitest";

import { createViewWatermark } from "../viewWatermark";

const EPOCH = "epoch-a";
const OTHER_EPOCH = "epoch-b";

/** 一份响应身上的版本标记（`TaskQueryResult` / `ProjectList` 的形状里就有这两个字段）。 */
function stamp(data_epoch: string, revision: number): { data_epoch: string; revision: number } {
  return { data_epoch, revision };
}

describe("视图水位：只回答「这条响应是不是比我上屏过的那份更旧」", () => {
  it("还没上屏过任何一份时，什么都不算旧（首次响应照收）", () => {
    // 反向验证：把 `isStale` 的 `held === null` 那一支改成 `return true`（首次也判旧）
    // ⇒ 这一句与两条页面 I1 回归用例同时红。
    const watermark = createViewWatermark();

    expect(watermark.isStale(stamp(EPOCH, 1), EPOCH)).toBe(false);
    expect(watermark.isStale(stamp(EPOCH, 99), EPOCH)).toBe(false);
  });

  it("epoch 变了就算旧——与版本高低无关", () => {
    // 反向验证：删掉第一句 epoch 比对（只比 revision）⇒ 这里两条都变成 false ⇒ 红。
    const watermark = createViewWatermark();
    watermark.applied(stamp(EPOCH, 5));

    expect(watermark.isStale(stamp(OTHER_EPOCH, 9), EPOCH)).toBe(true);
    // 纯读、不带期望 epoch（`null`）的路径不走这个函数；页面手上的 epoch 一定非空。
    expect(watermark.isStale(stamp(OTHER_EPOCH, 9), OTHER_EPOCH)).toBe(false);
  });

  it("同 epoch：比已上屏的那份旧 ⇒ 旧；同版本与更新的版本 ⇒ 不旧", () => {
    // 反向验证：把 `stamp.revision < held.revision` 改成 `<=` ⇒ "同版本不算旧"那句红；
    // 改成 `!==` ⇒ 更新版本那句也红。
    const watermark = createViewWatermark();
    watermark.applied(stamp(EPOCH, 5));

    expect(watermark.isStale(stamp(EPOCH, 4), EPOCH)).toBe(true);
    expect(watermark.isStale(stamp(EPOCH, 5), EPOCH)).toBe(false);
    expect(watermark.isStale(stamp(EPOCH, 6), EPOCH)).toBe(false);
  });

  it("水位只前进：更旧/同版/换 epoch 的 applied 不会把它拉回去", () => {
    // 反向验证：把 `applied` 改成无条件赋值 ⇒ 最后那两句（更旧的标记之后，
    // 第 5 版仍算旧）红。
    const watermark = createViewWatermark();
    watermark.applied(stamp(EPOCH, 6));

    watermark.applied(stamp(EPOCH, 4));
    watermark.applied(stamp(EPOCH, 6));
    expect(watermark.isStale(stamp(EPOCH, 5), EPOCH)).toBe(true);

    // 换 epoch：重新起算（新库的第 1 版相对新库不算旧）
    watermark.applied(stamp(OTHER_EPOCH, 1));
    expect(watermark.isStale(stamp(OTHER_EPOCH, 1), OTHER_EPOCH)).toBe(false);
    expect(watermark.isStale(stamp(OTHER_EPOCH, 0), OTHER_EPOCH)).toBe(true);
    // 手上水位记的是别的 epoch 时，**与本请求 epoch 一致**的那份不算旧（它回答的正是我们
    // 问的那个问题）；真正的换库由 epoch 那一半挡——见上一条用例。
    expect(watermark.isStale(stamp(EPOCH, 9), EPOCH)).toBe(false);
  });
});
