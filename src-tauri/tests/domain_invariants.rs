//! P1 Task 3 要求的领域不变量测试。
//!
//! 计划原文：覆盖允许/拒绝跃迁、0 长度半开区间、重叠、待确认和作废排除；
//! **无需一仓储函数一个机械测试**（01 §3）。所以这里按**规则**组织，不按函数。

use worktrace_lib::domain::error::DomainError;
use worktrace_lib::domain::interval::{IntervalFacts, IntervalRange, IntervalSet};
use worktrace_lib::domain::session::{SessionMode, SessionState, TimerBudget, TimerKind};
use worktrace_lib::domain::task::{TaskStatus, TaskTransition, TransitionCause};

// ─────────────────────────────────────────────────────────────────────────────
// Task 状态机（02 §5 全表）
// ─────────────────────────────────────────────────────────────────────────────

/// 把 02 §5 的表**逐行**抄成期望，而不是只挑几个case。表改了这里就该红。
#[test]
fn the_transition_table_matches_spec_section_5() {
    use TaskStatus::*;
    let expected: &[(TaskStatus, &[TaskStatus])] = &[
        (Inbox, &[Clarifying, Ready, Cancelled]),
        (Clarifying, &[Inbox, Ready, Cancelled]),
        (
            Ready,
            &[Doing, Scheduled, Blocked, Waiting, Review, Done, Cancelled],
        ),
        (
            Scheduled,
            &[Ready, Doing, Blocked, Waiting, Review, Done, Cancelled],
        ),
        (Doing, &[Ready, Blocked, Waiting, Review, Done, Cancelled]),
        (Blocked, &[Ready, Cancelled]),
        (Waiting, &[Ready, Cancelled]),
        (Review, &[Ready, Done, Cancelled]),
        (Done, &[Ready]),
        (Cancelled, &[Ready]),
    ];

    for (from, targets) in expected {
        assert_eq!(
            from.allowed_targets(),
            *targets,
            "{from:?} 的允许目标与 02 §5 不符"
        );
    }
}

/// 穷举全表：表内的全部放行，表外的全部拒绝。**除 `Scheduled` 与显式 reopen 两条附加规则。**
#[test]
fn every_offtable_transition_is_rejected() {
    for from in TaskStatus::ALL {
        for to in TaskStatus::ALL {
            let listed = from.allowed_targets().contains(&to);
            let cause = if from.is_terminal() {
                TransitionCause::Reopen
            } else {
                TransitionCause::User
            };
            let result = TaskTransition::new(from, to, cause);

            if listed && to != TaskStatus::Scheduled {
                assert!(
                    result.is_ok(),
                    "{from:?} -> {to:?} 在表内却被拒：{result:?}"
                );
            } else {
                assert!(result.is_err(), "{from:?} -> {to:?} 不在表内却被放行");
            }
        }
    }
}

/// `Scheduled` 属 V0.2，即使在跃迁表里，V0.1 也必须拒绝写入。
#[test]
fn scheduled_is_rejected_even_though_it_is_in_the_table() {
    assert!(
        TaskStatus::Ready
            .allowed_targets()
            .contains(&TaskStatus::Scheduled),
        "表里应当有它"
    );
    let err = TaskTransition::new(
        TaskStatus::Ready,
        TaskStatus::Scheduled,
        TransitionCause::User,
    )
    .unwrap_err();
    assert_eq!(err, DomainError::NotInThisVersion { what: "Scheduled" });
    assert!(!TaskStatus::Scheduled.is_writable_in_v01());
}

/// 终结态只能经**显式 reopen** 回到 Ready；普通推进必须被拒。
#[test]
fn terminal_states_require_an_explicit_reopen() {
    for from in [TaskStatus::Done, TaskStatus::Cancelled] {
        let implicit = TaskTransition::new(from, TaskStatus::Ready, TransitionCause::User)
            .expect_err("普通推进不得离开终结态");
        assert_eq!(
            implicit,
            DomainError::ReopenMustBeExplicit {
                from: from.as_str()
            }
        );

        let explicit = TaskTransition::new(from, TaskStatus::Ready, TransitionCause::Reopen)
            .expect("显式 reopen 应当放行");
        assert!(explicit.clears_quality(), "重开要清除当前质量（02 §5 末）");
    }

    // 非终结态的跃迁不该被误判成需要 reopen。
    assert!(
        !TaskTransition::new(TaskStatus::Doing, TaskStatus::Review, TransitionCause::User)
            .unwrap()
            .clears_quality()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 区间的半开语义
// ─────────────────────────────────────────────────────────────────────────────

/// **零长度区间合法**——`ended_at == started_at` 不是负区间。
#[test]
fn zero_length_intervals_are_valid_but_negative_ones_are_not() {
    let zero = IntervalRange::new(1000, 1000).expect("0 长度必须合法");
    assert!(zero.is_empty());
    assert_eq!(zero.duration_ms(), 0);

    let err = IntervalRange::new(1000, 999).unwrap_err();
    assert_eq!(
        err,
        DomainError::NegativeInterval {
            started_at: 1000,
            ended_at: 999
        }
    );
}

/// **端点相接不算重叠**——半开区间 `[1,2)` 与 `[2,3)` 交集为 0。
#[test]
fn touching_endpoints_do_not_overlap() {
    let a = IntervalRange::new(1, 2).unwrap();
    let b = IntervalRange::new(2, 3).unwrap();
    assert_eq!(a.overlap_ms(b), 0);
    assert!(!a.overlaps(b));

    // 真重叠一个毫秒就要被抓到。
    let c = IntervalRange::new(1, 3).unwrap();
    assert_eq!(a.overlap_ms(c), 1);
    assert!(a.overlaps(c));
}

/// 02 §6 的裁剪公式：`max(0, min(end,to) - max(start,from))`。
#[test]
fn clipping_follows_the_spec_formula() {
    let r = IntervalRange::new(100, 200).unwrap();
    assert_eq!(r.clipped_ms(0, 50), 0, "完全在范围外");
    assert_eq!(r.clipped_ms(150, 400), 50, "只算右半");
    assert_eq!(r.clipped_ms(0, 150), 50, "只算左半");
    assert_eq!(r.clipped_ms(120, 180), 60, "范围在区间内");
    assert_eq!(r.clipped_ms(0, 1000), 100, "范围包住区间");
}

/// F-010 的验收项：23:50–00:10 的无暂停 session，两天**各 10 分钟**。
#[test]
fn a_session_crossing_midnight_splits_evenly() {
    let ten_min = 10 * 60 * 1000;
    let day1_end = 24 * 60 * 60 * 1000; // 当日 24:00
    let start = day1_end - ten_min; // 23:50
    let end = day1_end + ten_min; // 次日 00:10

    let r = IntervalRange::new(start, end).unwrap();
    assert_eq!(r.clipped_ms(0, day1_end), ten_min, "第一天应得 10 分钟");
    assert_eq!(
        r.clipped_ms(day1_end, day1_end * 2),
        ten_min,
        "第二天应得 10 分钟"
    );
    assert_eq!(r.duration_ms(), 2 * ten_min, "整段是 20 分钟");
}

/// 区间集合拒绝重叠，但允许端点相接，并且总和可直接相加。
#[test]
fn interval_set_rejects_overlap_but_allows_adjacency() {
    let mut set = IntervalSet::new();
    set.insert(IntervalRange::new(0, 100).unwrap()).unwrap();
    set.insert(IntervalRange::new(100, 200).unwrap())
        .expect("端点相接应允许");

    let err = set
        .insert(IntervalRange::new(150, 250).unwrap())
        .unwrap_err();
    assert_eq!(
        err,
        DomainError::OverlappingInterval {
            existing_start: 100,
            existing_end: 200
        }
    );

    assert_eq!(set.len(), 2);
    assert_eq!(set.total_ms(), 200, "两两不重叠，可直接求和");
}

// ─────────────────────────────────────────────────────────────────────────────
// 事实的三分类：可信 / 待确认 / 作废
// ─────────────────────────────────────────────────────────────────────────────

fn facts(duration: Option<i64>, needs_review: bool, voided: bool) -> IntervalFacts {
    IntervalFacts {
        range: IntervalRange::new(0, duration.unwrap_or(100)).unwrap(),
        duration_ms: duration,
        needs_review,
        voided,
    }
}

/// 三类互斥：可信闭合计入已确认；待确认只进待确认栏；**作废两边都不进**。
#[test]
fn trusted_pending_and_voided_are_three_different_buckets() {
    let trusted = facts(Some(100), false, false);
    assert!(trusted.counts_as_confirmed());
    assert!(!trusted.is_pending());

    let pending = facts(None, true, false);
    assert!(!pending.counts_as_confirmed());
    assert!(pending.is_pending(), "未作废的待确认应进待确认栏");

    let voided = facts(Some(100), false, true);
    assert!(!voided.counts_as_confirmed());
    assert!(!voided.is_pending(), "已作废不得显示为待确认");
}

/// 计划原文：「记录损坏与普通待确认分开」。待确认 + 作废同时成立是**损坏**，不是待确认。
#[test]
fn pending_and_voided_at_once_is_corruption_not_a_review_item() {
    let broken = facts(None, true, true);
    assert_eq!(
        broken.validate().unwrap_err(),
        DomainError::PendingAndVoided
    );
}

/// 可信闭合必须有 `duration_ms`，且与起止一致——这条直接对应 schema 的
/// `ck_interval_duration`（那里用 CASE 而不是 OR，理由见 schema_v1.rs）。
#[test]
fn duration_must_match_the_range_when_present() {
    let inconsistent = IntervalFacts {
        range: IntervalRange::new(0, 100).unwrap(),
        duration_ms: Some(999), // 与 100 不符
        needs_review: false,
        voided: false,
    };
    assert!(inconsistent.validate().is_err());

    assert!(facts(Some(100), false, false).validate().is_ok());
    assert!(
        facts(None, true, false).validate().is_ok(),
        "待确认可以没有 duration"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 会话状态与计时预算
// ─────────────────────────────────────────────────────────────────────────────

/// **只有 `running` 允许开放区间**，也只有它累计实时暂计；`recovering` 停止正常计时。
#[test]
fn only_running_holds_an_open_interval_and_accrues_live_time() {
    for s in SessionState::ALL {
        let should = s == SessionState::Running;
        assert_eq!(s.allows_open_interval(), should, "{s:?} 的开放区间资格不对");
        assert_eq!(s.accrues_live_time(), should, "{s:?} 的实时暂计资格不对");
    }
    assert!(
        !SessionState::Recovering.accrues_live_time(),
        "recovering 不得继续累计"
    );
    assert!(SessionState::Finished.is_terminal());
    assert!(SessionState::Discarded.is_terminal());
    assert!(
        !SessionState::Recovering.is_terminal(),
        "recovering 要等用户处理，不是终态"
    );
}

/// 02 §6：人工**只有** FOREGROUND。并行机器时长不得加成人工。
#[test]
fn only_foreground_counts_as_human() {
    assert!(SessionMode::Foreground.counts_as_human());
    for m in [
        SessionMode::Background,
        SessionMode::Passive,
        SessionMode::Waiting,
    ] {
        assert!(!m.counts_as_human(), "{m:?} 不得计入人工");
    }
    // 四种模式都要能往返序列化——与 schema 的 CHECK 取值一致。
    for m in SessionMode::ALL {
        assert_eq!(SessionMode::parse(m.as_str()), Some(m));
    }
}

/// 倒计时必须有**正**预算；正计时必须没有。与 schema 的 `ck_timer_budget` 同一规则。
#[test]
fn timer_budget_matches_the_schema_constraint() {
    assert!(TimerBudget::countdown(0).is_err(), "0 预算不合法");
    assert!(TimerBudget::countdown(-1).is_err(), "负预算不合法");

    let c = TimerBudget::countdown(1500).unwrap();
    assert_eq!(c.kind, TimerKind::Countdown);
    assert_eq!(c.remaining_ms(500), Some(1000));
    assert_eq!(c.remaining_ms(1500), Some(0), "到点剩余为 0");
    assert_eq!(c.remaining_ms(2000), Some(0), "超时不出现负数");
    assert_eq!(c.overtime_ms(500), Some(0));
    assert_eq!(c.overtime_ms(1500), Some(0), "恰好到点不算超时");
    assert_eq!(c.overtime_ms(1700), Some(200));

    let s = TimerBudget::stopwatch();
    assert_eq!(s.kind, TimerKind::Stopwatch);
    assert_eq!(s.remaining_ms(500), None, "正计时没有剩余");
    assert_eq!(s.overtime_ms(999_999), None, "正计时不会超时");
}

/// 状态与计时的取值必须与 schema 的 CHECK 逐字一致，否则落库才会报错。
#[test]
fn domain_strings_match_the_schema_checks() {
    for s in SessionState::ALL {
        assert_eq!(SessionState::parse(s.as_str()), Some(s));
    }
    for t in [TimerKind::Stopwatch, TimerKind::Countdown] {
        assert_eq!(TimerKind::parse(t.as_str()), Some(t));
    }
    for s in TaskStatus::ALL {
        assert_eq!(TaskStatus::parse(s.as_str()), Some(s));
    }
    assert!(
        SessionState::parse("Running").is_none(),
        "状态串是大小写敏感的"
    );
    assert!(TaskStatus::parse("doing").is_none(), "状态串是大小写敏感的");
}

#[test]
fn a_closed_trusted_fact_requires_duration() {
    let missing = IntervalFacts {
        range: IntervalRange::new(0, 100).unwrap(),
        duration_ms: None,
        needs_review: false,
        voided: false,
    };
    assert_eq!(
        missing.validate(),
        Err(DomainError::TrustedIntervalWithoutDuration)
    );
    assert!(IntervalFacts {
        needs_review: true,
        ..missing
    }
    .validate()
    .is_ok());
    assert!(IntervalFacts {
        voided: true,
        ..missing
    }
    .validate()
    .is_ok());
    assert!(IntervalFacts {
        range: IntervalRange::new(100, 100).unwrap(),
        duration_ms: Some(0),
        ..missing
    }
    .validate()
    .is_ok());
}

#[test]
fn negative_fact_ranges_are_rejected_even_without_duration() {
    for (needs_review, voided) in [(false, false), (true, false), (false, true)] {
        let broken = IntervalFacts {
            range: IntervalRange { start: 100, end: 0 },
            duration_ms: None,
            needs_review,
            voided,
        };
        assert_eq!(
            broken.validate(),
            Err(DomainError::NegativeInterval {
                started_at: 100,
                ended_at: 0,
            })
        );
    }
}
