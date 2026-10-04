//! `time_edit` 审计载荷的公共骨架（P3 终审 I2c 收口）。
//!
//! `recovery`（扫描归一 / 对账 / 作废整次）与 `history`（修正 / 补录）写的是**同一份
//! 事实契约**（Ruling 8/R12）：`change` + 会话逐字段 + 全部区间的逐字段（含
//! `voided_at`）。它已经被扩过两次键（R12/R16），所以分量只能有一份——两份拷贝意味着
//! 下一次加键时有一半的消费者读不到它。
//!
//! **本模块只给公共骨架**：两个调用点各自显式加自己的可选键——
//! `recovery` 的 `candidate_end` / `candidate_end_source`（候选端点推导，Ruling 8）
//! 与 `history` 的 `user_reason`（用户给的理由）。那是两边的语义差异，
//! 所以留在调用点，不藏进参数里。
//!
//! **同步规则**：动 `session` / `intervals` 的键就动这里；动完之后，消费这两份 JSON
//! 的代码（P5 的统计取样、P8 的审计展示与 `before_json` 回放）要一起看——
//! 契约的这一处就是唯一的一处。

use crate::storage::session_repo::{IntervalRow, SessionRow};

/// `session` + `intervals` 的公共骨架。可选键由调用方自己往返回值上加。
pub(super) fn edit_json_base(
    change: &str,
    session: &SessionRow,
    intervals: &[IntervalRow],
) -> serde_json::Value {
    serde_json::json!({
        "change": change,
        "session": {
            "id": session.id,
            "state": session.state.as_str(),
            "run_id": session.run_id,
            "needs_review": session.needs_review,
            "row_version": session.row_version,
        },
        "intervals": intervals.iter().map(|i| serde_json::json!({
            "id": i.id,
            "started_at": i.started_at,
            "ended_at": i.ended_at,
            "duration_ms": i.duration_ms,
            "sampled_end_wall_at": i.sampled_end_wall_at,
            "needs_review": i.needs_review,
            "voided_at": i.voided_at,
        })).collect::<Vec<_>>(),
    })
}
