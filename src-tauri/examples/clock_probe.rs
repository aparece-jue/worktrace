//! P2 开工前的「单调/墙钟映射」探针（06 §4 的实现前技术验证）。
//!
//! 这个探针**只测量、不决策**：它按固定间隔同时读挂钟与单调钟，打印每一拍的
//! 两个数值与两个差（相邻增量差、累计偏差），并在结束时给出汇总。
//! 协调器的检测逻辑（P2 Task 2）要按这次实测的结论来定，而不是凭假设。
//!
//! **它不会修改系统时间。** 锁屏、休眠、正反改时都由人工操作；操作时按回车
//! 打一个标记，汇总里就能把「哪一段是人为干预」和「哪一段是自然漂移」分开。
//!
//! 用法：
//! ```text
//! cargo run --example clock_probe -- --seconds 600 --interval-ms 1000
//! cargo run --example clock_probe -- --seconds 30   --interval-ms 200   # 快速冒烟
//! ```
//!
//! 输出分两段：逐拍明细（可直接贴进记录表）与汇总（填「阈值结论」用）。

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use worktrace_lib::platform::clock::{
    measure_clock_resolution, Clock, ClockSample, DriftAnalyzer, SystemClock, THRESHOLD_MS,
};

fn parse_arg(args: &[String], name: &str, default: i64) -> i64 {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

/// 人工操作标记的计数。回车一次加一；**具体落在哪一拍由主循环解析**——
/// 读 stdin 的线程拿不到「当前采样」的单调值，硬取会引入另一个时钟读数。
fn spawn_marker_reader(count: Arc<AtomicUsize>, done: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            if done.load(Ordering::Relaxed) || line.is_err() {
                break;
            }
            count.fetch_add(1, Ordering::Relaxed);
        }
    });
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds = parse_arg(&args, "--seconds", 600);
    let interval_ms = parse_arg(&args, "--interval-ms", 1000);

    if seconds <= 0 || interval_ms <= 0 {
        eprintln!("--seconds 与 --interval-ms 必须为正数");
        std::process::exit(2);
    }

    println!("# Worktrace 时钟探针（P2 开工前，06 §4）");
    println!("# os            {}", std::env::consts::OS);
    println!("# arch          {}", std::env::consts::ARCH);
    println!("# interval_ms   {interval_ms}");
    println!("# duration_s    {seconds}");
    println!("# threshold_ms  {THRESHOLD_MS}（08 §1 的异常阈值，严格大于才越界）");
    println!("#");
    println!("# 操作提示：要测锁屏 / 休眠 / 改系统时间时，先按回车打标记，再去做操作。");
    println!("# 探针不会自己改系统时间。");
    println!("#");
    println!("idx,wall_ms,monotonic_ms,d_wall,d_mono,delta_gap,cum_gap,flag");

    // 先测两个时钟的实际分辨率：它决定「累计偏差」到底是真实漂移还是取整偏差。
    // 单位是纳秒——毫秒粒度会把亚毫秒的差别抹平，看不出台阶。
    {
        let (wall, mono) = measure_clock_resolution(20_000);
        println!("#");
        println!("# ---- 时钟分辨率（纳秒）----");
        for (name, r) in [("wall_systemtime", &wall), ("monotonic_instant", &mono)] {
            println!(
                "# {name}: samples={} min_positive_ns={:?} max_ns={} zero_permille={} distinct={:?}",
                r.samples,
                r.min_positive_ms,
                r.max_delta_ms,
                r.zero_ratio_permille,
                r.distinct_deltas.iter().take(6).collect::<Vec<_>>()
            );
        }
        println!("# 判读：distinct 里只有少数几个整齐的台阶值 → 该时钟粒度粗；");
        println!("#       若挂钟粒度远粗于单调钟，则「累计偏差」多半是取整偏差在单向累积。");
        println!("#");
    }

    let clock = SystemClock::new();
    let mut analyzer = DriftAnalyzer::new(interval_ms);

    let marker_count = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    spawn_marker_reader(Arc::clone(&marker_count), Arc::clone(&done));

    let total = (seconds * 1000 / interval_ms).max(1);
    let step = Duration::from_millis(interval_ms as u64);
    let mut seen_markers = 0usize;
    let mut sample_failures = 0u64;
    let mut last: Option<ClockSample> = None;

    for _ in 0..total {
        std::thread::sleep(step);

        let s = match clock.sample() {
            Ok(s) => s,
            Err(e) => {
                // 采样失败也要记：它本身就是要观测的现象之一。
                sample_failures += 1;
                println!("# sample failed: {e:?}");
                continue;
            }
        };
        let d = analyzer.feed(s);
        let flag = if d.exceeds_threshold() {
            "EXCEED"
        } else if d.looks_suspended(interval_ms) {
            "SUSPECT_SUSPEND"
        } else if d.wall_went_backwards() {
            "WALL_BACK"
        } else {
            ""
        };
        println!(
            "{},{},{},{},{},{},{},{}",
            d.index,
            d.wall_ms,
            d.monotonic_ms,
            d.d_wall_ms,
            d.d_monotonic_ms,
            d.delta_gap_ms(),
            d.cumulative_gap_ms(),
            flag
        );

        // 打标记：主循环知道当前拍的单调值，所以在这里落真实数值。
        let pending = marker_count.load(Ordering::Relaxed);
        if pending > seen_markers {
            for _ in seen_markers..pending {
                println!(
                    "# MARKER at idx={} monotonic_ms={}",
                    d.index, d.monotonic_ms
                );
            }
            seen_markers = pending;
        }
        last = Some(s);
    }

    done.store(true, Ordering::Relaxed);
    let r = &analyzer.report;

    println!("#");
    println!("# ---- 汇总 ----");
    println!("# samples              {}", r.samples);
    println!("# sample_failures      {sample_failures}");
    println!("# flagged(>threshold)  {}", r.flagged);
    if let Some(l) = last {
        println!("# last_wall_ms         {}", l.wall_ms);
        println!("# last_monotonic_ms    {}", l.monotonic_ms);
    }
    println!("# worst_delta_gap_ms   {}", r.worst_delta_gap_ms);
    println!("# worst_cum_gap_ms     {}", r.worst_cumulative_gap_ms);
    println!("# wall_backwards       {}", r.wall_backwards);
    println!("# monotonic_backwards  {}", r.monotonic_backwards);
    println!("# worst_interval_gap   {}", r.worst_interval_gap_ms);
    println!("# suspends             {}", r.suspends);
    println!("# markers              {seen_markers}");
    println!("#");
    if r.monotonic_is_sane() {
        println!("# 结论：单调钟全程未倒退（正常平台的基本前提）。");
    } else {
        println!("# 结论：★ 单调钟出现倒退，平台或实现有问题，必须查明后再写协调器。");
    }
    if r.flagged == 0 {
        println!("# 结论：本次未出现超过 {THRESHOLD_MS}ms 的偏差。");
        println!(
            "#       注意：这不等于「阈值合理」——要看人工操作（锁屏/休眠/改时）那几段的表现。"
        );
    } else {
        println!(
            "# 结论：出现 {} 拍越界，对照 MARKER 判断是人為操作还是自然漂移。",
            r.flagged
        );
    }
}
