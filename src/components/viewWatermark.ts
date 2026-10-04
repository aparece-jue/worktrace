/**
 * 一条查询自己的**视图水位**（P7 Task 5 fix round 1，评审 I1）。
 *
 * ## 为什么不直接用 `domainState.isStaleResponse` / `markApplied`（全局那把水位）
 *
 * 全局那把水位的前提是「已应用的是**权威快照**」——全量、含所有状态、能回答任何视图的问题。
 * 而页面里的 `list_tasks` / `list_projects` 是**过滤 + 分页后的局部视图**：它只覆盖
 * "这个条件、这一页"，不满足那个前提。把它的 `revision` 推进全局水位有两个可观测的后果
 * （评审探针 A/C 实测）：
 *
 * 1. **同 `revision` 的 `domain.changed` 会被判 `drop`**（闸门规则②：快照已经包含它）——
 *    可那条通知是发给**所有**视图的失效信号：一个局部视图"读到了第 6 版"并不代表镜像里
 *    别的缓存（计时展示、其它页面）也到了第 6 版。结果：失效计数不动、页面不再重拉。
 * 2. **30 秒校验失去判据**：`verify()` 比的是 `identity.revision > seenRevision()`，而
 *    `seen` 已被局部视图推平 ⇒ 末次通知丢失时不再 `resync`（状态栏永远停在旧状态，
 *    连 `rehandshake()` 也救不回来）。
 *
 * 所以页面用**自己这一份**水位判"旧响应"：它只回答"这条响应是不是比我**上屏过**的那份
 * 更旧"，与别的视图、与全局快照互不干扰。全局水位仍由真正的快照推进
 * （`get_revision` 与 `timer_snapshot`，见 `domainState.applyStamp`）。
 *
 * ## 判据的两半（与 `FreshnessGate::isStaleResponse` 的关系）
 *
 * - **epoch**：与发起请求时手上的 `requestEpoch` 不同 ⇒ 丢弃。这一半与闸门那条的第一句
 *   同义（"回答的不是我们问的那个世界"），但它**不**顺带比全局水位；
 * - **revision**：同 epoch 内比本视图**已上屏**的那一版，且水位只前进。
 *
 * 「同版本」不算旧（重复查询、同一版的重拉都合法）——与闸门 `<=` 的分寸一致。
 *
 * ## 用法
 *
 * 一个视图一个实例（`useState(createViewWatermark)[0]`：只借它拿一个稳定实例，
 * 它本身不是会变的状态）：响应回来先 `isStale(...)`，真的上屏之后再 `applied(...)`
 * （"收到" ≠ "用上"）。
 */

import type { VersionStamp } from "../ipc";

/** 一个视图自己的水位。 */
export interface ViewWatermark {
  /** 这条响应是不是"比本视图已上屏的那份更旧"（含换 epoch）。 */
  isStale(stamp: VersionStamp, requestEpoch: string): boolean;
  /** 记下**已经上屏**的一份响应（只前进：同版/更旧不动它）。 */
  applied(stamp: VersionStamp): void;
}

export function createViewWatermark(): ViewWatermark {
  let held: VersionStamp | null = null;

  return {
    isStale(stamp, requestEpoch) {
      // 换 epoch：这条响应回答的不是我们现在问的那个世界（闸门规则①的意思）。
      if (stamp.data_epoch !== requestEpoch) return true;
      if (held === null || held.data_epoch !== stamp.data_epoch) return false;
      return stamp.revision < held.revision;
    },

    applied(stamp) {
      if (held === null || held.data_epoch !== stamp.data_epoch || stamp.revision > held.revision) {
        held = { data_epoch: stamp.data_epoch, revision: stamp.revision };
      }
    },
  };
}
