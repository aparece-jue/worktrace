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

// ─────────────────────────────────────────────────────────────────────────────
// 采样检测（Task 2）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次采样的判定。**顺序有意义**：硬故障 → 单拍跳变 → 累计漂移 → 挂起 → 可信。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleVerdict {
    /// 可信：两个时钟同步推进，没有越界。
    Trusted,
    /// 本次没有取得样本；候选终点未知。
    Unavailable,
    /// 单调钟倒退——**硬故障**，正常平台永不出现。
    MonotonicBackwards { d_mono_ms: i64 },
    /// 挂钟倒退——用户把系统时间往回调。事实仍可用，但该走异常判断。
    WallBackwards { d_wall_ms: i64 },
    /// 单拍跳变：相邻增量差超过阈值。
    Jumped { delta_gap_ms: i64 },
    /// 累计漂移：相对**最近一次心跳锚点**的偏差超过阈值。
    Drifted { cumulative_gap_ms: i64 },
    /// 疑似挂起：这一拍特别长，但**两个时钟仍然同步**。
    ///
    /// 实测（`docs/validation/p2-clock-mapping.md` §4.2）：休眠 129 秒时
    /// `d_mono = 129121ms`，QPC 照常推进、两钟不错位（Δgap 仅 −36ms）。
    /// 仅凭长间隔不能确定离开开始边界，等待可信平台事件或用户确认。
    Suspended { gap_ms: i64 },
}

impl SampleVerdict {
    /// 仅普通样本可信；长间隔无法自行确定离开边界。
    pub fn facts_are_trustworthy(self) -> bool {
        matches!(self, SampleVerdict::Trusted)
    }

    /// 是否需要走异常/恢复路径。
    pub fn needs_recovery(self) -> bool {
        !self.facts_are_trustworthy()
    }
}

/// 检测 + 归属的状态机。
///
/// **为什么有两个参照点**（这一条是实测逼出来的）：
/// - `anchor` 是**归属基线**，用来算 `A(M)`。它在一次连续可信的 run 内保持不变——
///   否则 `started_at` 会随时间漂移，区间会重叠。**成功心跳不得重置它**（Task 2 原文）。
/// - `drift_ref` 是**累计偏差的参照点**，每次成功心跳前移。
///   实测挂钟与单调钟以 10–14 ms/分钟单向分叉，若累计偏差永远对着 run 起点算，
///   两三小时后健康会话就会越界——那检测的不是异常，是时间流逝本身。
#[derive(Debug, Clone, Copy)]
pub struct AnchorState {
    /// **归属基线**：算 `A(M)` 用。只在已提交的变更之后重建。
    anchor: Anchor,
    /// **短期检测参照**：每次成功心跳前移。心跳本来每 30 秒一次，是天然的重新基准点。
    drift_ref: Anchor,
    /// **进程生命期参照**：只有「已确认的时钟校正」才移动它。
    ///
    /// 为什么必须与上面两个分开：它守的是**机器走时特性**（NTP 校准速率、RTC 偏差、
    /// 虚拟机 TSC），那是进程级的事实，与某一次会话、某一次暂停无关。若跟着
    /// `reestablish` 一起重置，那么**每次休眠唤醒、每次暂停都会把长期偏差一笔勾销**
    /// ——一台一天暂停几次的机器，长期界就永远不会触发，等于没有。
    lifetime_ref: Anchor,
    last: Option<ClockSample>,
    expected_interval_ms: i64,
}

impl AnchorState {
    /// 由一次已验证采样建立。
    pub fn establish(sample: ClockSample, expected_interval_ms: i64) -> Self {
        let a = Anchor::establish(sample);
        Self {
            anchor: a,
            drift_ref: a,
            lifetime_ref: a,
            last: Some(sample),
            expected_interval_ms,
        }
    }

    pub fn anchor(&self) -> Anchor {
        self.anchor
    }

    pub fn drift_ref(&self) -> Anchor {
        self.drift_ref
    }

    pub fn last(&self) -> Option<ClockSample> {
        self.last
    }

    /// 归属：`A(M)` 用**归属基线**，不受心跳影响。
    pub fn attribute(&self, monotonic_ms: i64) -> i64 {
        self.anchor.attribute(monotonic_ms)
    }

    /// 观察一次采样并判定。会更新「上一次采样」；**不动归属基线**。
    pub fn observe(&mut self, sample: ClockSample) -> SampleVerdict {
        let Some(prev) = self.last else {
            self.last = Some(sample);
            return SampleVerdict::Trusted;
        };
        self.last = Some(sample);

        let d_mono = sample.monotonic_ms - prev.monotonic_ms;
        let d_wall = sample.wall_ms - prev.wall_ms;

        // ① 硬故障优先：单调钟倒退说明平台或实现有问题。
        if d_mono < 0 {
            return SampleVerdict::MonotonicBackwards { d_mono_ms: d_mono };
        }
        // ② 挂钟倒退是正常用户行为，但事实不再可信。
        if d_wall < 0 {
            return SampleVerdict::WallBackwards { d_wall_ms: d_wall };
        }
        // ③ 单拍跳变：这一拍两个时钟走得不一样快。
        let delta_gap = d_wall - d_mono;
        if delta_gap.abs() > threshold() {
            return SampleVerdict::Jumped {
                delta_gap_ms: delta_gap,
            };
        }
        // ④ 累计漂移：对着**最近一次心跳锚点**算，不是对 run 起点。
        let cum_gap = (sample.wall_ms - self.drift_ref.wall_at)
            - (sample.monotonic_ms - self.drift_ref.monotonic_at);
        if cum_gap.abs() > threshold() {
            return SampleVerdict::Drifted {
                cumulative_gap_ms: cum_gap,
            };
        }
        // 长期边界不随心跳清零。500 ppm 是初始自然漂移容差，并非精度保证。
        let elapsed = (sample.monotonic_ms - self.lifetime_ref.monotonic_at).max(0);
        let lifetime_gap = sample.wall_ms - self.lifetime_ref.attribute(sample.monotonic_ms);
        if lifetime_gap.abs()
            > threshold().saturating_add(elapsed.saturating_mul(NATURAL_DRIFT_PPM) / 1_000_000)
        {
            return SampleVerdict::Drifted {
                cumulative_gap_ms: lifetime_gap,
            };
        }
        // ⑤ 长间隔——两钟同不同步都一样。**它证明不了离开开始于何时**：
        //    实测休眠 129 秒时 Δgap 只有 −36ms，两钟同步只说明「这段时间真实流逝了」，
        //    不说明「这段时间都在工作」。所以它不是可信样本，须等可信平台边界或用户确认。
        //    （改这条时别退回「两钟同步即可信」——那会把整个休眠时长算成工时。）
        if self.expected_interval_ms > 0 && d_mono > self.expected_interval_ms * 3 {
            return SampleVerdict::Suspended { gap_ms: d_mono };
        }
        SampleVerdict::Trusted
    }

    /// 接受一次**已确认的**时钟校正：把长期参照移到当前样本。
    ///
    /// 只在「偏差已经被检测到、记录在案、并已提交恢复事务」之后调用。
    /// 不在检测时自动调用——那等于让故障自己把证据擦掉；
    /// 也不在任何系统事件里调用——唤醒不是校正。
    pub fn accept_clock_correction(&mut self, sample: ClockSample) {
        self.lifetime_ref = Anchor::establish(sample);
    }

    /// 长期参照（诊断与测试用）。
    pub fn lifetime_ref(&self) -> Anchor {
        self.lifetime_ref
    }

    /// 一次**成功**心跳后调用：只前移累计偏差的参照点。
    ///
    /// **绝不碰归属基线**——心跳每 30 秒一次，若让它重置 `anchor`，
    /// `A(M)` 就会随心跳跳变，区间起点随之漂移。
    pub fn reanchor_drift_on_heartbeat(&mut self, sample: ClockSample) {
        self.drift_ref = Anchor::establish(sample);
    }

    /// 显式重建归属基线（新 run、休眠唤醒后、用户显式校正）。
    ///
    /// 调用方**必须**先关闭/隔离旧的开放事实——重建基线会让 `A(M)` 跳变，
    /// 旧区间若还开着，它的起点就与新的归属对不上了。
    pub fn reestablish(&mut self, sample: ClockSample) {
        let a = Anchor::establish(sample);
        self.anchor = a;
        self.drift_ref = a;
        // **不动 `lifetime_ref`**：见字段文档。重建归属基线是会话级的事，
        // 而长期偏差守的是机器级的事，两者不该互相抵消。
        self.last = Some(sample);
    }
}

/// 阈值。与 08 §1 一致：**严格大于**才越界，恰好 2000ms 不算。
///
/// 实测依据：单拍相邻增量差在自然状态下恒为 0 或 ±1ms，余量三个数量级；
/// 真实异常（改时、休眠）是千毫秒级跳跃。
pub const NATURAL_DRIFT_PPM: i64 = 500;

pub fn threshold() -> i64 {
    crate::platform::clock::THRESHOLD_MS
}

#[cfg(test)]
mod detector_tests {
    use super::*;

    fn s(wall: i64, mono: i64) -> ClockSample {
        ClockSample {
            wall_ms: wall,
            monotonic_ms: mono,
        }
    }

    #[test]
    fn a_steady_clock_is_trusted() {
        let mut st = AnchorState::establish(s(1_000_000, 0), 1_000);
        for i in 1..=10 {
            // 每拍挂钟与单调钟各走 1000ms
            assert_eq!(
                st.observe(s(1_000_000 + i * 1000, i * 1000)),
                SampleVerdict::Trusted
            );
        }
        assert_eq!(st.attribute(10_000), 1_010_000, "归属基线未被打扰");
    }

    /// 阈值边界：**恰好 2000ms 不算越界**。
    #[test]
    fn the_threshold_is_strictly_greater_than_2000() {
        for (gap, expect_trusted) in [(1999, true), (2000, true), (2001, false)] {
            let mut st = AnchorState::establish(s(0, 0), 1_000);
            // 单调钟走 1000，挂钟走 1000+gap
            let v = st.observe(s(1000 + gap, 1000));
            assert_eq!(
                v.facts_are_trustworthy(),
                expect_trusted,
                "gap={gap} 的判定不对：{v:?}"
            );
        }
    }

    /// 挂钟倒退：用户改时间。事实不再可信。
    #[test]
    fn a_backwards_wall_clock_is_not_trusted() {
        let mut st = AnchorState::establish(s(10_000, 5_000), 1_000);
        let v = st.observe(s(9_500, 6_000)); // 挂钟回拨 500ms
        assert_eq!(v, SampleVerdict::WallBackwards { d_wall_ms: -500 });
        assert!(v.needs_recovery());
    }

    /// 单调钟倒退是硬故障。
    #[test]
    fn a_backwards_monotonic_clock_is_a_hard_fault() {
        let mut st = AnchorState::establish(s(10_000, 5_000), 1_000);
        let v = st.observe(s(11_000, 4_900));
        assert_eq!(v, SampleVerdict::MonotonicBackwards { d_mono_ms: -100 });
        assert!(v.needs_recovery());
    }

    /// **缓慢累计漂移**：每拍只差 100ms、单拍永不越界，但累计会越界。
    #[test]
    fn slow_cumulative_drift_is_caught() {
        let mut st = AnchorState::establish(s(0, 0), 1_000);
        let mut verdict = SampleVerdict::Trusted;
        for i in 1..=30 {
            verdict = st.observe(s(i * 1100, i * 1000)); // 每拍挂钟多走 100ms
        }
        match verdict {
            SampleVerdict::Drifted { cumulative_gap_ms } => {
                assert_eq!(cumulative_gap_ms, 3000, "30 拍累计 3 秒");
            }
            other => panic!("应判为累计漂移，实际 {other:?}"),
        }
        // 单拍从未越界
        let mut st2 = AnchorState::establish(s(0, 0), 1_000);
        assert_eq!(st2.observe(s(1100, 1000)), SampleVerdict::Trusted);
    }

    /// **心跳前移参照点后，累计漂移不再误报**——这是实现那条实测结论的关键。
    #[test]
    fn reanchoring_on_heartbeat_stops_the_drift_false_positive() {
        let mut st = AnchorState::establish(s(0, 0), 1_000);
        // 模拟「每分钟漂 14ms」的机器，跑 40 拍（够越过 2000ms 吗？不，但足够验证前移）
        let mut drifted = 0;
        for i in 1..=40 {
            let wall = i * 1000 + (i * 14); // 每拍多 14ms
            let v = st.observe(s(wall, i * 1000));
            if !v.facts_are_trustworthy() {
                drifted += 1;
            }
            // 每 30 拍当作一次成功心跳：前移累计参照点
            if i % 30 == 0 {
                st.reanchor_drift_on_heartbeat(s(wall, i * 1000));
            }
        }
        // 前移之后累计偏差重新从 0 起算，永远到不了阈值
        assert_eq!(drifted, 0, "心跳前移参照点后不该有累计漂移判定");

        // 对照：不前移就会越界
        let mut st2 = AnchorState::establish(s(0, 0), 1_000);
        let mut hit = false;
        for i in 1..=200 {
            let wall = i * 1000 + (i * 14);
            if !st2.observe(s(wall, i * 1000)).facts_are_trustworthy() {
                hit = true;
                break;
            }
        }
        assert!(hit, "不前移参照点，早晚会误报（实测 10–14ms/分钟）");
    }

    /// 两钟同步的长间隔仍需可靠的离开边界。
    #[test]
    fn a_long_gap_requires_a_trusted_departure_boundary() {
        let mut st = AnchorState::establish(s(0, 0), 1_000);
        // 实测数据：休眠 129 秒，Δgap 仅 −36ms
        let v = st.observe(s(129_085, 129_121));
        assert_eq!(v, SampleVerdict::Suspended { gap_ms: 129_121 });
        assert!(!v.facts_are_trustworthy(), "时钟同步不证明离开边界可信");
        assert!(v.needs_recovery(), "只有长间隔、没有事件边界时必须确认");
    }

    /// 采样失败由调用方转成不可信；检测器本身不需要采样即可保持状态。
    #[test]
    fn the_anchor_survives_a_gap_in_observation() {
        let mut st = AnchorState::establish(s(1_000, 0), 1_000);
        // 中间几次采样失败（调用方没有喂进来），随后恢复
        let v = st.observe(s(5_000, 4_000));
        assert_eq!(
            v,
            SampleVerdict::Suspended { gap_ms: 4_000 },
            "4 拍没喂，表现为长间隔"
        );
    }

    /// **重建归属基线不得移动长期参照。**
    ///
    /// 长期参照守的是机器走时特性（NTP 速率、RTC 偏差、虚拟机 TSC），那是进程级事实，
    /// 与某一次会话、某一次暂停无关。若跟着 `reestablish` 一起重置，每次休眠唤醒、
    /// 每次暂停都会把长期偏差一笔勾销——一天暂停几次的机器，长期界等于没有。
    #[test]
    fn reestablishing_must_not_move_the_lifetime_reference() {
        let mut st = AnchorState::establish(s(1_700_000_000_000, 0), 1_000);
        let lifetime_before = st.lifetime_ref();
        assert_eq!(lifetime_before.monotonic_at, 0);

        // 走了 30 秒、积累了一点偏差，然后重建归属基线（比如一次可信的系统暂停）
        st.observe(s(1_700_000_030_100, 30_000));
        st.reestablish(s(1_700_000_030_100, 30_000));

        assert_eq!(
            st.lifetime_ref(),
            lifetime_before,
            "长期参照必须留在原处——它记的是机器的事，不是这次会话的事"
        );
        assert_eq!(st.attribute(30_000), 1_700_000_030_100, "归属改用新基线");

        // 只有显式接受校正才移动它
        st.accept_clock_correction(s(1_700_000_030_100, 30_000));
        assert_eq!(st.lifetime_ref().monotonic_at, 30_000, "显式接受后才移动");
    }

    /// 重建基线后 `A(M)` 与短期参照改用新值，但**长期参照要显式接受校正才动**。
    ///
    /// 这条测试里那次跳变是 **1,000,000ms**——那正是长期界该抓的东西。
    /// 「重建归属基线」是会话级动作，它**不能**顺手把机器级的长期偏差也勾销。
    #[test]
    fn reestablishing_moves_attribution_but_leaves_the_lifetime_reference_alone() {
        let mut st = AnchorState::establish(s(0, 0), 1_000);
        st.observe(s(60_000, 60_000));
        st.reestablish(s(1_000_000, 60_000)); // 挂钟被校正到真实时间

        assert_eq!(st.attribute(60_000), 1_000_000, "归属改用新基线");

        // 只重建、没接受校正：长期界仍然盯着那 100 万毫秒
        assert!(
            st.observe(s(1_001_000, 61_000)).needs_recovery(),
            "归属重建不等于承认这次跳变是校正"
        );

        // 显式接受之后才恢复正常
        st.accept_clock_correction(s(1_001_000, 61_000));
        assert_eq!(st.observe(s(1_002_000, 62_000)), SampleVerdict::Trusted);
    }
}
