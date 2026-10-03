//! 归属基线 `A(M) = wall_at + (M - monotonic_at)`（08 §1）。
//!
//! 为什么需要它：单调钟知道「过了多久」，挂钟知道「现在是几点」。`A(M)` 把两者
//! 缝在一起——给一个单调读数，算出它**对应哪个挂钟时刻**。工时一律由它推算，
//! 不直接读挂钟。
//!
//! 本任务只放**纯函数**；什么时候重建基线、怎么判断样本可信，属 Task 2。

use crate::platform::clock::ClockSample;

/// 一次可信采样建立的基线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// 建立时的挂钟读数。
    pub wall_at: i64,
    /// 建立时的单调读数。
    pub monotonic_at: i64,
}

impl Anchor {
    /// 由一次**已验证**的采样建立。
    pub fn establish(sample: ClockSample) -> Self {
        Self {
            wall_at: sample.wall_ms,
            monotonic_at: sample.monotonic_ms,
        }
    }

    /// `A(M)`：把单调读数换算成它对应的挂钟时刻。
    ///
    /// `M` 早于基线时返回值会小于 `wall_at`——**不做截断**。截断会把
    /// 「时钟倒退」这种要观测的现象悄悄抹平；判定该不该接受是 Task 2 的事。
    pub fn attribute(&self, monotonic_ms: i64) -> i64 {
        self.wall_at + (monotonic_ms - self.monotonic_at)
    }

    /// 从基线到 `M` 之间经过的时长。同一个 `A(M)` 减去基线即得。
    pub fn elapsed_since(&self, monotonic_ms: i64) -> i64 {
        monotonic_ms - self.monotonic_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_maps_a_monotonic_reading_onto_the_wall_clock() {
        let a = Anchor::establish(ClockSample {
            wall_ms: 1_700_000_000_000,
            monotonic_ms: 1_000,
        });
        // 单调钟再走 5 秒 → 对应的挂钟时刻应当正好晚 5 秒
        assert_eq!(a.attribute(6_000), 1_700_000_005_000);
        assert_eq!(a.elapsed_since(6_000), 5_000);
    }

    #[test]
    fn attribute_at_the_anchor_returns_the_anchor_wall_time() {
        let a = Anchor::establish(ClockSample {
            wall_ms: 42,
            monotonic_ms: 7,
        });
        assert_eq!(a.attribute(7), 42);
        assert_eq!(a.elapsed_since(7), 0);
    }

    /// 早于基线的读数**不被截断**——截断会把要观测的现象抹平。
    #[test]
    fn attribute_before_the_anchor_goes_negative_instead_of_clamping() {
        let a = Anchor::establish(ClockSample {
            wall_ms: 1_000,
            monotonic_ms: 500,
        });
        assert_eq!(
            a.attribute(400),
            900,
            "M 早于基线 100ms，归属时刻也应早 100ms"
        );
        assert_eq!(a.elapsed_since(400), -100);
    }
}
