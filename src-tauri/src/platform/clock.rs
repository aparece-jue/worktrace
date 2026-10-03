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
}
