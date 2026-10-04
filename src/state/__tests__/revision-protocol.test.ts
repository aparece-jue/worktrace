/**
 * `RevisionGate` 的**前端镜像** vs 两侧共读的向量（P7 Task 2，评审 M2 的另一半）。
 *
 * 规范文本是 `src-tauri/src/services/events.rs`（`RevisionGate`）。这份用例把
 * `src/types/__vectors__/revision-gate.json` 的同一串步骤 replay 到
 * `createFreshnessGate`，断言**同一串判决**；Rust 侧由
 * `src-tauri/tests/revision_gate_vectors.rs` replay 同一份文件。
 *
 * 为什么要有这条：`ipc.ts` 的闸门与 Rust 的闸门是两份实现，靠人记得对齐是不成立的。
 * 改一条规则（换个比较符、合并一条分支）⇒ Rust 的 replay 先红；照着改向量之后，
 * 这条用例再红，直到前端跟着改。**「必须同时改两侧」是机械的，不是纪律的。**
 *
 * 这里只断言**协议判决**，不碰任何业务：向量里的 epoch/revision 都是抽象记号。
 */

import { describe, expect, it } from "vitest";

import vectors from "../../types/__vectors__/revision-gate.json";
import { createFreshnessGate } from "../../ipc";

interface VectorStep {
  op: string;
  epoch: string | null;
  revision?: number;
  required?: number;
  applied?: number;
  seen?: number;
  expect?: string;
}

interface VectorCase {
  name: string;
  steps: VectorStep[];
}

// JSON 的字面量类型推不出 `string`（`op`/`expect` 在文件里是具体串），只能显式过一道闩。
const CASES = (vectors as unknown as { cases: VectorCase[] }).cases;

function required(step: VectorStep, field: "revision" | "required" | "applied" | "seen"): number {
  const value = step[field];
  if (typeof value !== "number") throw new Error(`步骤 ${step.op} 缺少数字字段 ${field}`);
  return value;
}

function epochOf(step: VectorStep): string {
  if (typeof step.epoch !== "string") throw new Error(`步骤 ${step.op} 缺少 epoch`);
  return step.epoch;
}

function expectOf(step: VectorStep): string {
  if (typeof step.expect !== "string") throw new Error(`步骤 ${step.op} 缺少 expect`);
  return step.expect;
}

describe("RevisionGate 的前端镜像（两侧共读的向量）", () => {
  it("向量覆盖四条规则的全部判决取值（裁掉一半就红，防用例空过）", () => {
    expect(CASES.length).toBeGreaterThanOrEqual(6);
    expect(CASES.map((entry) => entry.steps.length).reduce((a, b) => a + b, 0)).toBeGreaterThanOrEqual(
      40,
    );

    const outcomes = new Set<string>();
    for (const entry of CASES) {
      for (const step of entry.steps) {
        if (step.op === "state") continue;
        outcomes.add(expectOf(step));
      }
    }
    expect([...outcomes].sort()).toEqual([
      "accept",
      "applied",
      "apply",
      "cache_invalidated",
      "drop",
      "rehandshake",
      "resync",
      "stale_ignored",
    ]);
  });

  for (const entry of CASES) {
    it(entry.name, () => {
      const gate = createFreshnessGate();

      for (const step of entry.steps) {
        switch (step.op) {
          case "apply_snapshot":
            expect(gate.applySnapshot(epochOf(step), required(step, "revision"))).toBe(
              expectOf(step),
            );
            break;
          case "notification":
            expect(
              gate.onNotification({
                data_epoch: epochOf(step),
                revision: required(step, "revision"),
              }),
            ).toBe(expectOf(step));
            break;
          case "query_response":
            expect(
              gate.onQueryResponse(
                epochOf(step),
                required(step, "revision"),
                required(step, "required"),
              ),
            ).toBe(expectOf(step));
            break;
          case "state":
            expect(gate.epoch()).toBe(step.epoch);
            expect(gate.applied()?.revision ?? 0).toBe(required(step, "applied"));
            expect(gate.seenRevision()).toBe(required(step, "seen"));
            break;
          default:
            throw new Error(`未知的 op ${step.op}：向量文件与这份 replay 对不上`);
        }
      }
    });
  }

  it("与 Rust 唯一的一处字面差异：还没应用过快照时不判未知（启动顺序让它不可达）", () => {
    const gate = createFreshnessGate();
    // Rust 的 `on_notification` 在这一格判 Rehandshake（`epoch == None` 分支），
    // 而 Task 1b 的 isUnknownEpoch 把「还没应用过任何快照」定义为**不未知**——
    // 所以两侧共读的向量不含这一格，这里单独把它写明白。
    expect(gate.isUnknownEpoch({ data_epoch: "e1", revision: 1 })).toBe(false);
    expect(gate.onNotification({ data_epoch: "e1", revision: 1 })).toBe("apply");
  });
});
