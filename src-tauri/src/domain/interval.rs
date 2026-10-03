//! 区间的半开语义与重叠判定（02 §6、P3 共用）。

use super::error::{DomainError, DomainResult};

/// 一个半开区间 `[start, end)`，单位为 Unix 毫秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalRange {
    pub start: i64,
    pub end: i64,
}

impl IntervalRange {
    /// 校验并构造。`end == start`（零长度）**合法**——计划明确要求覆盖这个边界。
    pub fn new(start: i64, end: i64) -> DomainResult<Self> {
        if end < start {
            return Err(DomainError::NegativeInterval {
                started_at: start,
                ended_at: end,
            });
        }
        Ok(Self { start, end })
    }

    pub fn duration_ms(self) -> i64 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.end == self.start
    }

    /// 半开区间相交：`max(0, min(end,to) - max(start,from))`（02 §6 的公式）。
    ///
    /// 端点相接**不算**重叠——`[1,2)` 与 `[2,3)` 交集为 0。
    pub fn overlap_ms(self, other: IntervalRange) -> i64 {
        let lo = self.start.max(other.start);
        let hi = self.end.min(other.end);
        (hi - lo).max(0)
    }

    pub fn overlaps(self, other: IntervalRange) -> bool {
        self.overlap_ms(other) > 0
    }

    /// 与范围 `[from, to)` 的交集时长。P5 的统计口径直接用这一条。
    pub fn clipped_ms(self, from: i64, to: i64) -> i64 {
        self.overlap_ms(IntervalRange {
            start: from,
            end: to,
        })
    }
}

/// 一段区间在「事实」层面的性质——决定它是否计入已确认工时。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntervalFacts {
    pub range: IntervalRange,
    pub duration_ms: Option<i64>,
    pub needs_review: bool,
    pub voided: bool,
}

impl IntervalFacts {
    /// 可信闭合：已闭合、有 `duration_ms`、不待确认、未作废。
    pub fn is_trusted_closed(&self) -> bool {
        self.duration_ms.is_some() && !self.needs_review && !self.voided
    }

    /// 计入「待确认」栏：待确认且未作废。
    ///
    /// **作废不是待确认**：已作废的记录只能在历史/审计里看，不进待确认栏。
    pub fn is_pending(&self) -> bool {
        self.needs_review && !self.voided
    }

    /// 是否计入任何「已确认」数字。待确认、已作废、已丢弃一律排除。
    pub fn counts_as_confirmed(&self) -> bool {
        self.is_trusted_closed()
    }

    /// 校验这批事实自洽。计划要求「记录损坏与普通待确认分开」——
    /// 不满足不变量的是**损坏**，不是待确认。
    pub fn validate(&self) -> DomainResult<()> {
        // Public fields may be assembled from persisted facts without using new().
        IntervalRange::new(self.range.start, self.range.end)?;
        if !self.needs_review && !self.voided && self.duration_ms.is_none() {
            return Err(DomainError::TrustedIntervalWithoutDuration);
        }
        // 待确认与已作废不能同时成立（schema 的 ck_interval_voided 兜底）。
        if self.needs_review && self.voided {
            return Err(DomainError::PendingAndVoided);
        }
        // 可信闭合必须有 duration_ms，且与起止一致。
        if let Some(d) = self.duration_ms {
            let expected = self.range.duration_ms();
            if d != expected || d < 0 {
                return Err(DomainError::NegativeInterval {
                    started_at: self.range.start,
                    ended_at: self.range.end,
                });
            }
        }
        Ok(())
    }
}

/// 一组互不重叠的区间。服务层在修正/确认路径上用它做重叠校验。
#[derive(Debug, Default, Clone)]
pub struct IntervalSet {
    items: Vec<IntervalRange>,
}

impl IntervalSet {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// 加入一段区间；与既有区间重叠则拒绝（端点相接允许）。
    pub fn insert(&mut self, range: IntervalRange) -> DomainResult<()> {
        for existing in &self.items {
            if existing.overlaps(range) {
                return Err(DomainError::OverlappingInterval {
                    existing_start: existing.start,
                    existing_end: existing.end,
                });
            }
        }
        self.items.push(range);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 总时长（两两不重叠，可直接求和）。
    pub fn total_ms(&self) -> i64 {
        self.items.iter().map(|r| r.duration_ms()).sum()
    }

    pub fn items(&self) -> &[IntervalRange] {
        &self.items
    }
}

/// 协调器**已验证**的闭合事实，交给仓储落库（P1 Task 3 定义、Task 4 消费）。
///
/// 仓储**不得**由挂钟自行推算这里任何一项——工时只能来自协调器的单调差。
/// 这正是「不得用 wall-now 在仓储计算工时」那条约束的载体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedIntervalFacts {
    /// 归属后的结束时刻（挂钟毫秒）。
    pub ended_at: i64,
    /// 可信时长。待确认的余段为 `None`。
    pub duration_ms: Option<i64>,
    /// 协调器采样到的结束挂钟值，原样保留供事后核对。
    pub sampled_end_wall_at: i64,
    /// 是否为待确认的余段。
    pub needs_review: bool,
}
