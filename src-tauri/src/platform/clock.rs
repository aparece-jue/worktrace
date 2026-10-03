//! 时钟接缝（00 §5、08 §1）。
//!
//! 两个数值必须**一次性**取到：先取挂钟再取单调钟会引入一个不受控的间隔，
//! 归属基线 `A(M) = wall_at + (M - monotonic_at)` 就是靠「同一采样点」才成立的。
//! 所以接口是 `sample() -> ClockSample`，不是两个独立的 getter。
//!
//! `SystemClock` 的 `monotonic_ms` 来自本次进程的 `Instant`，**只在当前 run 内有意义**——
//! 重启后它会从头开始。跨 run 比较单调值是无意义的，恢复路径必须走持久化事实。

use std::cell::Cell;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 一次原子采样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSample {
    /// Unix 毫秒（挂钟）。会被用户改系统时间影响。
    pub wall_ms: i64,
    /// 进程内单调毫秒。不受改时影响，但重启归零。
    pub monotonic_ms: i64,
}

/// 采样失败的原因。真实的锁屏/休眠/权限问题都会走到这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleError {
    /// 系统调用失败（例如系统时间不可读）。
    Unavailable,
    /// 注入的故障，仅测试用。
    Injected,
}

/// 时钟。协调器只通过这个接口取时间，测试因此可以完全控制时序。
pub trait Clock {
    fn sample(&self) -> Result<ClockSample, SampleError>;
}

/// 真实时钟。
pub struct SystemClock {
    /// 进程启动时刻。`monotonic_ms` 以它为原点——这就是「只在本次 run 内有效」的含义。
    origin: Instant,
    /// 进程启动时的挂钟，用于在系统时间剧烈跳变时仍给出连续值。
    ///
    /// 注意：**这不是**用来掩盖改时的。改时是必须被检测到的真实事件，
    /// 所以每次采样仍然重新读系统挂钟；`origin_wall_ms` 只用于计算单调钟。
    origin_wall_ms: i64,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemClock {
    pub fn new() -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH);
        let wall = now.map(|d| d.as_millis() as i64).unwrap_or(0);
        Self {
            origin: Instant::now(),
            origin_wall_ms: wall,
        }
    }

    /// 本次 run 的挂钟原点。P2 建立基线时用。
    pub fn origin_wall_ms(&self) -> i64 {
        self.origin_wall_ms
    }
}

impl Clock for SystemClock {
    fn sample(&self) -> Result<ClockSample, SampleError> {
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .map_err(|_| SampleError::Unavailable)?;
        Ok(ClockSample {
            wall_ms,
            monotonic_ms: self.origin.elapsed().as_millis() as i64,
        })
    }
}

/// 异常判断阈值（08 §1，P2 Task 2）。
///
/// 「两差任一绝对值 **>** 2000ms」——恰好等于 2000ms **不算**越界。
/// 相邻增量差与累计偏差用的是同一个阈值。
pub const THRESHOLD_MS: i64 = 2000;

/// 一次采样相对**上一次**的观察值。探针与协调器共用这套判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriftSample {
    pub index: u64,
    pub wall_ms: i64,
    pub monotonic_ms: i64,
    /// 相对上一次采样的挂钟增量（首次为 0）。
    pub d_wall_ms: i64,
    /// 相对上一次采样的单调增量（首次为 0）。
    pub d_monotonic_ms: i64,
    /// 相对**首次**采样的累计挂钟增量。
    pub cum_wall_ms: i64,
    /// 相对**首次**采样的累计单调增量。
    pub cum_monotonic_ms: i64,
}

impl DriftSample {
    /// 相邻增量差：这一次两个时钟走得一样快吗。
    pub fn delta_gap_ms(&self) -> i64 {
        self.d_wall_ms - self.d_monotonic_ms
    }

    /// 累计偏差：从起点看，两个时钟总共差了多少。
    ///
    /// 与相邻增量差**都要**看：缓慢漂移每次只差几十毫秒、单次不越界，
    /// 但累计起来会越界——只查相邻增量会漏掉这种最真实的情形。
    pub fn cumulative_gap_ms(&self) -> i64 {
        self.cum_wall_ms - self.cum_monotonic_ms
    }

    /// 两差任一绝对值超过阈值（严格大于）。
    pub fn exceeds_threshold(&self) -> bool {
        self.delta_gap_ms().abs() > THRESHOLD_MS || self.cumulative_gap_ms().abs() > THRESHOLD_MS
    }

    /// 挂钟倒退：用户把系统时间往回调。
    pub fn wall_went_backwards(&self) -> bool {
        self.d_wall_ms < 0
    }

    /// 单调钟倒退：不该发生，出现即说明平台或实现有问题。
    pub fn monotonic_went_backwards(&self) -> bool {
        self.d_monotonic_ms < 0
    }

    /// 这一次的间隔是否远超预期——进程大概率被挂起了（休眠）。
    ///
    /// 判据是**单调钟**的增量，因为挂起期间它也照走；挂钟反而可能被系统校时。
    pub fn looks_suspended(&self, expected_interval_ms: i64) -> bool {
        expected_interval_ms > 0 && self.d_monotonic_ms > expected_interval_ms * 3
    }
}

/// 采样序列的汇总。探针结束时打印，记录表据此填「观察结果」与「阈值结论」。
#[derive(Debug, Clone)]
pub struct DriftReport {
    pub samples: u64,
    /// 绝对值最大的相邻增量差。
    pub worst_delta_gap_ms: i64,
    /// 绝对值最大的累计偏差。
    pub worst_cumulative_gap_ms: i64,
    /// 超过阈值的采样数。
    pub flagged: u64,
    /// 挂钟倒退次数。
    pub wall_backwards: u64,
    /// 单调钟倒退次数。
    pub monotonic_backwards: u64,
    /// 实际间隔相对期望的最大偏离（休眠/长阻塞的迹象）。
    pub worst_interval_gap_ms: i64,
    /// 被判为疑似挂起的次数。
    pub suspends: u64,
}

impl DriftReport {
    pub fn new() -> Self {
        Self {
            samples: 0,
            worst_delta_gap_ms: 0,
            worst_cumulative_gap_ms: 0,
            flagged: 0,
            wall_backwards: 0,
            monotonic_backwards: 0,
            worst_interval_gap_ms: 0,
            suspends: 0,
        }
    }

    pub fn push(&mut self, s: &DriftSample, expected_interval_ms: i64) {
        self.samples += 1;
        if s.delta_gap_ms().abs() > self.worst_delta_gap_ms.abs() {
            self.worst_delta_gap_ms = s.delta_gap_ms();
        }
        if s.cumulative_gap_ms().abs() > self.worst_cumulative_gap_ms.abs() {
            self.worst_cumulative_gap_ms = s.cumulative_gap_ms();
        }
        if s.exceeds_threshold() {
            self.flagged += 1;
        }
        if s.wall_went_backwards() {
            self.wall_backwards += 1;
        }
        if s.monotonic_went_backwards() {
            self.monotonic_backwards += 1;
        }
        // 首样本没有「上一次」，增量恒为 0——把它算进间隔偏离会得到一个
        // 假的「偏离了一个完整间隔」。实测时就是这样冒出一个 500ms 的假信号。
        if expected_interval_ms > 0 && s.index > 1 {
            let gap = (s.d_monotonic_ms - expected_interval_ms).abs();
            if gap > self.worst_interval_gap_ms {
                self.worst_interval_gap_ms = gap;
            }
            if s.looks_suspended(expected_interval_ms) {
                self.suspends += 1;
            }
        }
    }

    /// 单调钟倒退是硬故障：正常平台永远不会出现。
    pub fn monotonic_is_sane(&self) -> bool {
        self.monotonic_backwards == 0
    }
}

impl Default for DriftReport {
    fn default() -> Self {
        Self::new()
    }
}

/// 由连续采样推进分析器：喂入原始样本，它负责算增量并汇总。
pub struct DriftAnalyzer {
    expected_interval_ms: i64,
    first: Option<ClockSample>,
    last: Option<ClockSample>,
    index: u64,
    pub report: DriftReport,
}

impl DriftAnalyzer {
    pub fn new(expected_interval_ms: i64) -> Self {
        Self {
            expected_interval_ms,
            first: None,
            last: None,
            index: 0,
            report: DriftReport::new(),
        }
    }

    /// 喂入一次采样，返回相对上一次的观察值（首次的增量为 0）。
    pub fn feed(&mut self, s: ClockSample) -> DriftSample {
        let first = *self.first.get_or_insert(s);
        let d_wall = self.last.map(|l| s.wall_ms - l.wall_ms).unwrap_or(0);
        let d_mono = self
            .last
            .map(|l| s.monotonic_ms - l.monotonic_ms)
            .unwrap_or(0);
        self.last = Some(s);
        self.index += 1;

        let sample = DriftSample {
            index: self.index,
            wall_ms: s.wall_ms,
            monotonic_ms: s.monotonic_ms,
            d_wall_ms: d_wall,
            d_monotonic_ms: d_mono,
            cum_wall_ms: s.wall_ms - first.wall_ms,
            cum_monotonic_ms: s.monotonic_ms - first.monotonic_ms,
        };
        self.report.push(&sample, self.expected_interval_ms);
        sample
    }
}

/// 可完全控制的假时钟。
///
/// 两个数值**独立**推进——这正是要测的场景：挂钟被改而单调钟照走（用户改时间），
/// 或两者一起跳（休眠）。把它们绑在一起就测不出这类分歧。
#[derive(Debug, Default)]
pub struct FakeClock {
    wall_ms: i64,
    monotonic_ms: i64,
    /// 下一次采样是否失败。`None` 表示正常。
    ///
    /// 用 `Cell` 是因为 `Clock::sample` 取 `&self`（时钟要被多处共享），
    /// 而「只失败一次」必须能消费掉这个标志——否则它会变成永久失败。
    fail_next: Cell<Option<SampleError>>,
    /// 是否**持续**失败。
    fail_always: bool,
}

impl FakeClock {
    pub fn new(wall_ms: i64, monotonic_ms: i64) -> Self {
        Self {
            wall_ms,
            monotonic_ms,
            fail_next: Cell::new(None),
            fail_always: false,
        }
    }

    /// 只推进挂钟（模拟用户改系统时间）。
    pub fn advance_wall(&mut self, delta_ms: i64) {
        self.wall_ms += delta_ms;
    }

    /// 只推进单调钟（模拟正常流逝）。
    pub fn advance_monotonic(&mut self, delta_ms: i64) {
        self.monotonic_ms += delta_ms;
    }

    /// 两个一起推进（模拟正常等待一段时间）。
    pub fn advance_both(&mut self, delta_ms: i64) {
        self.wall_ms += delta_ms;
        self.monotonic_ms += delta_ms;
    }

    /// 让**下一次**采样失败，之后恢复。
    pub fn fail_once(&mut self) {
        self.fail_next.set(Some(SampleError::Injected));
    }

    /// 让之后所有采样都失败。
    pub fn fail_forever(&mut self) {
        self.fail_always = true;
    }

    /// 恢复成功。
    pub fn recover(&mut self) {
        self.fail_always = false;
        self.fail_next.set(None);
    }

    pub fn wall_ms(&self) -> i64 {
        self.wall_ms
    }

    pub fn monotonic_ms(&self) -> i64 {
        self.monotonic_ms
    }
}

impl Clock for FakeClock {
    fn sample(&self) -> Result<ClockSample, SampleError> {
        if self.fail_always {
            return Err(SampleError::Injected);
        }
        // 取走并消费：只失败一次。
        if let Some(e) = self.fail_next.take() {
            return Err(e);
        }
        Ok(ClockSample {
            wall_ms: self.wall_ms,
            monotonic_ms: self.monotonic_ms,
        })
    }
}

impl Clock for std::sync::Mutex<FakeClock> {
    fn sample(&self) -> Result<ClockSample, SampleError> {
        self.lock().map_err(|_| SampleError::Unavailable)?.sample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_moves_monotonically() {
        let c = SystemClock::new();
        let a = c.sample().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = c.sample().unwrap();
        assert!(b.monotonic_ms >= a.monotonic_ms);
        assert!(b.wall_ms >= a.wall_ms);
    }

    #[test]
    fn fake_clock_advances_the_two_values_independently() {
        let mut c = FakeClock::new(1_000, 0);
        c.advance_wall(-500); // 用户把系统时间往回改
        c.advance_monotonic(700); // 单调钟照走
        let s = c.sample().unwrap();
        assert_eq!(s.wall_ms, 500);
        assert_eq!(s.monotonic_ms, 700);
    }

    #[test]
    fn fake_clock_can_fail_once_then_recover() {
        let mut c = FakeClock::new(0, 0);
        c.fail_once();
        assert_eq!(c.sample(), Err(SampleError::Injected));
        assert!(c.sample().is_ok(), "fail_once 只应影响一次");

        c.fail_forever();
        assert!(c.sample().is_err());
        c.recover();
        assert!(c.sample().is_ok());
    }
    /// 阈值边界：恰好 2000ms **不**越界（08 §1 的「>2000ms」）。
    #[test]
    fn threshold_is_strictly_greater_than_2000() {
        let mut a = DriftAnalyzer::new(1000);
        a.feed(ClockSample {
            wall_ms: 0,
            monotonic_ms: 0,
        });
        let at = a.feed(ClockSample {
            wall_ms: 3000,
            monotonic_ms: 1000,
        }); // 差 2000
        assert_eq!(at.delta_gap_ms(), 2000);
        assert!(!at.exceeds_threshold(), "恰好 2000ms 不算越界");

        // 挂钟再走 2001ms，单调钟**不动**——增量差 2001ms，越界一格。
        let over = a.feed(ClockSample {
            wall_ms: 5001,
            monotonic_ms: 1000,
        });
        assert_eq!(over.delta_gap_ms(), 2001);
        assert!(over.exceeds_threshold());
    }

    /// **缓慢累计漂移**：每次只差 100ms、单次都不越界，但累计会越界。
    ///
    /// 这正是只查相邻增量会漏掉的情形，所以两个差都要看。
    #[test]
    fn slow_cumulative_drift_is_caught_even_though_each_step_is_small() {
        let mut a = DriftAnalyzer::new(1000);
        a.feed(ClockSample {
            wall_ms: 0,
            monotonic_ms: 0,
        });
        let mut last = None;
        for i in 1..=30 {
            // 每次挂钟多走 100ms
            last = Some(a.feed(ClockSample {
                wall_ms: i * 1100,
                monotonic_ms: i * 1000,
            }));
        }
        let last = last.unwrap();
        assert_eq!(
            last.delta_gap_ms(),
            100,
            "单次只差 100ms，相邻增量永远不越界"
        );
        assert_eq!(last.cumulative_gap_ms(), 3000, "30 次累计 3 秒");
        assert!(last.exceeds_threshold(), "累计越界必须被抓到");

        // 第 i 次的累计偏差是 i*100；严格大于 2000 从 i=21 起，所以是 10 个。
        assert_eq!(a.report.flagged, 10);
        assert_eq!(a.report.worst_cumulative_gap_ms, 3000);
        assert_eq!(
            a.report.worst_delta_gap_ms, 100,
            "相邻增量差的最大值仍是 100"
        );
    }

    /// 挂钟可以被用户改回去；单调钟不可以。
    #[test]
    fn wall_clock_may_go_backwards_but_the_monotonic_clock_may_not() {
        let mut a = DriftAnalyzer::new(1000);
        a.feed(ClockSample {
            wall_ms: 10_000,
            monotonic_ms: 5_000,
        });
        let back = a.feed(ClockSample {
            wall_ms: 9_500,
            monotonic_ms: 6_000,
        });
        assert!(back.wall_went_backwards());
        assert!(!back.monotonic_went_backwards());
        assert_eq!(a.report.wall_backwards, 1);
        assert!(a.report.monotonic_is_sane());

        let bad = a.feed(ClockSample {
            wall_ms: 9_600,
            monotonic_ms: 5_900,
        });
        assert!(bad.monotonic_went_backwards(), "单调钟倒退是硬故障");
        assert!(!a.report.monotonic_is_sane());
    }

    /// 休眠：单调钟的增量远超采样间隔。判据用单调钟，因为挂起期间它照走。
    #[test]
    fn a_suspend_shows_up_as_a_huge_monotonic_gap() {
        let mut a = DriftAnalyzer::new(1000);
        a.feed(ClockSample {
            wall_ms: 0,
            monotonic_ms: 0,
        });
        let woke = a.feed(ClockSample {
            wall_ms: 300_000,
            monotonic_ms: 300_000,
        });
        assert!(woke.looks_suspended(1000));
        assert_eq!(a.report.suspends, 1);
        assert_eq!(a.report.worst_interval_gap_ms, 299_000);
        // 正常的 1 秒间隔不该被判为挂起
        let normal = a.feed(ClockSample {
            wall_ms: 301_000,
            monotonic_ms: 301_000,
        });
        assert!(!normal.looks_suspended(1000));
    }

    /// 首样本没有「上一次」，增量必须为 0 而不是拿 0 当基准算出天文数字。
    #[test]
    fn the_first_sample_has_no_delta() {
        let mut a = DriftAnalyzer::new(1000);
        let s = a.feed(ClockSample {
            wall_ms: 1_700_000_000_000,
            monotonic_ms: 42,
        });
        assert_eq!((s.d_wall_ms, s.d_monotonic_ms), (0, 0));
        assert_eq!(s.delta_gap_ms(), 0);
        assert_eq!(s.cumulative_gap_ms(), 0);
        assert_eq!(a.report.samples, 1);
        assert_eq!(a.report.flagged, 0);
    }

    /// 首样本不得被算成「间隔偏离一个完整周期」——实测时它就是这么冒出
    /// 一个假的 500ms 的。
    #[test]
    fn the_first_sample_never_counts_as_an_interval_gap() {
        let mut a = DriftAnalyzer::new(500);
        a.feed(ClockSample {
            wall_ms: 0,
            monotonic_ms: 0,
        });
        assert_eq!(a.report.worst_interval_gap_ms, 0, "首样本不该产生间隔偏离");
        assert_eq!(a.report.suspends, 0);

        a.feed(ClockSample {
            wall_ms: 500,
            monotonic_ms: 500,
        });
        assert_eq!(a.report.worst_interval_gap_ms, 0, "正常的一拍偏离为 0");

        a.feed(ClockSample {
            wall_ms: 1600,
            monotonic_ms: 1600,
        });
        assert_eq!(a.report.worst_interval_gap_ms, 600, "晚了 600ms 才算偏离");
    }
}
