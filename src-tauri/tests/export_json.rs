//! P5 Task 3：JSON 明细导出（F-018）。
//!
//! 覆盖（Task 3 的测试清单）：
//! - 顶层 `schema_version` / 单位 / `timezone` / 生成时间 / `data_epoch` / `revision` 齐全；
//! - 同一输入两次导出**除生成时间外逐字节一致**；
//! - 导出里的数字与 `services::stats.rs` 的返回值**逐字段相等**（导出不另写取数逻辑）；
//! - 空范围（`from == to`）导出合法且不 panic；
//! - 导出的区间集合两两不重叠（P3 的保证在此可见）；
//! - 明细求和与列合计对得上：`ms` 为 `Some(n)` 时 `Σ clipped_ms == n`；`ms` 为 `None`
//!   时明细贡献只能是 0，且导出里必须**保留 `null`**（不悄悄变成 0）；
//! - R-03：导出写清范围 / 时区 / 分类依据，并按 `task_id` 做**标签连接**
//!   （任务 / 项目 / 标签），**不重算任何时长**；
//! - 导出是只读的：不写库、不加 `revision`；
//! - 生产入口 `AppState::export_json` 与 `services::export::json` 是同一份文档。
//!
//! 断言一律给出**期望数值本身**（计划 §断言口径），不用「非空」「大于 0」。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use worktrace_lib::domain::interval::IntervalRange;
use worktrace_lib::domain::session::{SessionMode, TimerKind};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::platform::clock::FakeClock;
use worktrace_lib::services::bootstrap::{
    lock_app, startup, AppState, NoProbe, RunningApp, Startup, StartupConfig,
};
use worktrace_lib::services::events::{EventEnvelope, EventSink};
use worktrace_lib::services::export;
use worktrace_lib::services::history::{self, CorrectAction, CorrectRequest};
use worktrace_lib::services::stats::{self, Measure, RangeReport, StatsClass, StatsRangeQuery};
use worktrace_lib::services::timer::coordinator::{Coordinator, SessionRequest, StartRequest};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::run_repo;

/// 与本进程启动同名的本次 run（手工装置与真启动装置都用它）。
const RUN: &str = "run-1";

/// 上海 2026-03-10 00:00（+08:00）= 1773072000000。
const SH_MID: i64 = 1_773_072_000_000;
/// 上海 2026-03-10 12:00——「此刻」的固定取值。
const SH_NOON: i64 = 1_773_115_200_000;
/// 一小时 / 一天的毫秒数（只有步进才用它，日界一律由服务算）。
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
const MINUTE: i64 = 60_000;

/// 导出里列的落库形状（`Measure` 的 serde 名），用于取列与报错信息。
fn measure_name(measure: Measure) -> &'static str {
    match measure {
        Measure::Human => "human",
        Measure::MachineBackground => "machine_background",
        Measure::MachinePassive => "machine_passive",
        Measure::Waiting => "waiting",
    }
}

/// 三类的 serde 名。
fn class_name(class: StatsClass) -> &'static str {
    match class {
        StatsClass::Confirmed => "confirmed",
        StatsClass::Live => "live",
        StatsClass::Pending => "pending",
    }
}

/// 取导出文档里某一类某一 measure 的列（每类固定四项，取不到就是用例/实现写错了）。
fn column<'a>(doc: &'a Value, class: &str, measure: &str) -> &'a Value {
    doc[class]
        .as_array()
        .unwrap_or_else(|| panic!("{class} 必须是数组"))
        .iter()
        .find(|column| column["measure"] == measure)
        .unwrap_or_else(|| panic!("{class} 里必须有 {measure} 列"))
}

/// 导出里的某一列给的毫秒数：JSON `null` → `None`。
///
/// **`None` 与 `Some(0)` 必须区分得开**：前者是「这条 measure 的候选一条已知端点的
/// 都没有，不推算」，后者是「算出来就是 0」。
fn ms_of(doc: &Value, class: &str, measure: &str) -> Option<i64> {
    column(doc, class, measure)["ms"].as_i64()
}

/// 明细里 `class` 与 `needs_review` 必须一致：**待确认候选带标记、已确认与暂计不带**。
///
/// `criteria.exclusions` 的文案说的就是这件事（「未作废的待确认候选会出现在明细里
/// （needs_review=true、class=pending）」），所以文案与数据必须能互相对上——照字面把
/// `needs_review` 的行滤掉的第三方会丢掉候选行，让 `Σ clipped_ms` 与 `pending` 列的
/// `ms` 对不上。
fn assert_detail_review_flags(details: &[Value]) {
    for detail in details {
        let class = detail["class"].as_str().unwrap();
        let needs_review = detail["needs_review"].as_bool().unwrap();
        assert_eq!(
            needs_review,
            class == "pending",
            "明细 {} 的 needs_review 与 class={class} 不符",
            detail["id"]
        );
    }
}

/// 把所有**时间水位**字段压成同一个值：`generated_at` 与 `as_of`（含每一列、每个日桶
/// 里的 `as_of`）。两次导出之间会动的只有它们——数字与口径必须逐字段相同。
fn normalize_watermarks(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key.as_str() == "generated_at" || key.as_str() == "as_of" {
                    *child = serde_json::json!(0);
                } else {
                    normalize_watermarks(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                normalize_watermarks(item);
            }
        }
        _ => {}
    }
}

/// 忽略全部事件的出口：导出用例不关心广播，但 `startup` 需要一个。
struct NoSink;

impl EventSink for NoSink {
    fn broadcast(&self, _envelope: &EventEnvelope) -> Result<(), String> {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 装置：手工造事实（形状可控）+ 真协调器（活会话走真 `start`）
// ─────────────────────────────────────────────────────────────────────────────

struct H {
    _dir: tempfile::TempDir,
    db: Db,
    coord: Coordinator,
    clock: Arc<Mutex<FakeClock>>,
    epoch: String,
}

fn setup(wall: i64) -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    run_repo::start_run(&tx, RUN, wall).unwrap();
    tx.commit().unwrap();

    let clock = Arc::new(Mutex::new(FakeClock::new(wall, 0)));
    let coord = Coordinator::new(Box::new(Arc::clone(&clock)), RUN);
    H {
        _dir: dir,
        db,
        coord,
        clock,
        epoch: meta.data_epoch,
    }
}

impl H {
    fn project(&self, id: &str, name: &str) {
        self.db
            .connection()
            .execute(
                "INSERT INTO project(id,name,row_version,status,created_at,updated_at)
                 VALUES(?1,?2,0,'active',1,1)",
                rusqlite::params![id, name],
            )
            .unwrap();
    }

    fn task(&self, id: &str, title: &str, created_at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
                 VALUES(?1,?2,'Doing',0,?3,?3)",
                rusqlite::params![id, title, created_at],
            )
            .unwrap();
    }

    fn task_in_project(&self, id: &str, title: &str, project_id: &str, created_at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task(id,project_id,title,status,row_version,created_at,updated_at)
                 VALUES(?1,?2,?3,'Doing',0,?4,?4)",
                rusqlite::params![id, project_id, title, created_at],
            )
            .unwrap();
    }

    fn tag(&self, id: &str, kind: &str, name: &str, created_at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO tag(id,kind,name,row_version,created_at) VALUES(?1,?2,?3,0,?4)",
                rusqlite::params![id, kind, name, created_at],
            )
            .unwrap();
    }

    fn tag_task(&self, task_id: &str, tag_id: &str) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task_tag(task_id,tag_id) VALUES(?1,?2)",
                rusqlite::params![task_id, tag_id],
            )
            .unwrap();
    }

    fn session(
        &self,
        id: &str,
        task_id: &str,
        mode: &str,
        state: &str,
        needs_review: i64,
        started_at: i64,
    ) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,
                                          needs_review,row_version)
                 VALUES(?1,?2,?3,?4,?5,'stopwatch',?6,?7,0)",
                rusqlite::params![id, task_id, RUN, mode, state, started_at, needs_review],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn interval(
        &self,
        id: &str,
        session_id: &str,
        started_at: i64,
        ended_at: Option<i64>,
        duration_ms: Option<i64>,
        needs_review: i64,
        voided_at: Option<i64>,
    ) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_interval(id,session_id,started_at,ended_at,duration_ms,
                                           needs_review,voided_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![
                    id,
                    session_id,
                    started_at,
                    ended_at,
                    duration_ms,
                    needs_review,
                    voided_at
                ],
            )
            .unwrap();
    }

    /// 真起一次前台计时（协调器的内存镜像与库行一起建立）。
    fn start(&mut self, task_id: &str) {
        let req = StartRequest {
            expected_data_epoch: self.epoch.clone(),
            task_id: task_id.to_string(),
            task_expected_version: 0,
            mode: SessionMode::Foreground,
            timer_kind: TimerKind::Stopwatch,
            target_duration_ms: None,
            expected_interval_ms: 30_000,
        };
        self.coord.start(&mut self.db, req).unwrap();
    }

    /// 推进一段真实流逝：**60 秒一片、每片一次真采样**（一次跳满会被判成长间隔异常）。
    fn elapse(&mut self, total_ms: i64) {
        let mut left = total_ms;
        while left > 0 {
            let step = left.min(MINUTE);
            self.clock.lock().unwrap().advance_both(step);
            self.coord.stats_sample(&mut self.db).unwrap();
            left -= step;
        }
    }

    fn query(&self, from: i64, to: i64, timezone: &str) -> StatsRangeQuery {
        StatsRangeQuery {
            from,
            to,
            timezone: timezone.to_string(),
            expected_data_epoch: self.epoch.clone(),
        }
    }

    /// 与生产入口同一条取数路径：一次样本 + 一次范围报表（导出要对齐的就是它）。
    fn report(&mut self, from: i64, to: i64, timezone: &str) -> RangeReport {
        let query = self.query(from, to, timezone);
        let sample = self.coord.stats_sample(&mut self.db).unwrap();
        stats::snapshot(&self.db, sample, &query)
            .unwrap()
            .report()
            .unwrap()
    }

    /// 与生产入口同一条取数路径：一次样本 + 一次 JSON 导出。
    fn export(
        &mut self,
        from: i64,
        to: i64,
        timezone: &str,
        generated_at: i64,
    ) -> export::ExportJson {
        let query = self.query(from, to, timezone);
        let sample = self.coord.stats_sample(&mut self.db).unwrap();
        export::json(&self.db, sample, &query, generated_at).expect("导出应当成功")
    }

    fn export_doc(
        &mut self,
        from: i64,
        to: i64,
        timezone: &str,
        generated_at: i64,
    ) -> (export::ExportJson, Value) {
        let exported = self.export(from, to, timezone, generated_at);
        let doc: Value = serde_json::from_str(&exported.text).expect("导出必须是合法 JSON");
        (exported, doc)
    }

    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    /// 这条连接累计写过的行数（只读断言的判据，与 `tests/today.rs` 同一写法）。
    fn total_changes(&self) -> i64 {
        self.db
            .connection()
            .query_row("SELECT total_changes()", [], |r| r.get(0))
            .unwrap()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ① 格式身份、单位、生成时间与「同一份数据」
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_export_carries_its_format_identity_units_and_the_same_data_watermark() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );

    let generated_at = SH_NOON + 12_345;
    let (exported, doc) = h.export_doc(SH_MID, SH_MID + DAY, "Asia/Shanghai", generated_at);

    // —— 格式身份：`schema_version` 是**导出格式**的版本（这里 = 1），
    //    与 07 入站导入协议里那个同名字段不是一回事。
    assert_eq!(doc["schema_version"], 1);
    // —— 单位口径写清：时长是毫秒、时刻是 Unix 毫秒、日期是查询时区的本地日；
    //    没有任何字段以分钟计。
    assert_eq!(doc["units"]["durations"], "milliseconds");
    assert_eq!(doc["units"]["instants"], "unix_epoch_milliseconds");
    assert_eq!(doc["units"]["dates"], "YYYY-MM-DD");
    assert!(
        doc["units"]["minutes"]
            .as_str()
            .unwrap()
            .contains("not used"),
        "分钟口径要写清「不用」：{:?}",
        doc["units"]["minutes"]
    );
    // —— 筛选口径（R-03）：范围 / 时区 / 分类依据
    assert_eq!(doc["criteria"]["classification"], "current_assignments");
    assert!(
        !doc["criteria"]["exclusions"].as_str().unwrap().is_empty(),
        "排除口径必须写进导出"
    );
    assert!(!doc["criteria"]["measures"].as_str().unwrap().is_empty());
    assert!(!doc["criteria"]["pending"].as_str().unwrap().is_empty());
    assert!(!doc["criteria"]["labels"].as_str().unwrap().is_empty());
    assert!(!doc["criteria"]["range_basis"].as_str().unwrap().is_empty());
    // —— 两个时间的口径（Ruling P5-19）：`as_of` 是数据水位、`generated_at` 是文件产出时刻
    assert!(
        doc["criteria"]["watermarks"]
            .as_str()
            .unwrap()
            .contains("generated_at"),
        "口径里必须写清 generated_at 不代表数据更新到那一刻：{:?}",
        doc["criteria"]["watermarks"]
    );

    // —— 生成时间：**原样**是调用方传进来的那个值（服务层不读时钟）
    assert_eq!(doc["generated_at"], generated_at);

    // —— 与界面核对「是不是同一份数据」：epoch / revision / as_of
    assert_eq!(doc["data_epoch"], h.epoch);
    assert_eq!(doc["revision"], h.revision());
    assert_eq!(doc["as_of"], SH_NOON, "没有活动会话时 as_of 就是采样时刻");
    assert_eq!(exported.data_epoch, h.epoch);
    assert_eq!(exported.revision, h.revision());

    // —— 范围与时区（过归一入口）
    assert_eq!(doc["timezone"], "Asia/Shanghai");
    assert_eq!(doc["range"]["from"], SH_MID);
    assert_eq!(doc["range"]["to"], SH_MID + DAY);

    // —— 数字：那条 09:00–10:00 的前台区间 = 人工 1 小时
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(HOUR));
    assert_eq!(column(&doc, "confirmed", "human")["intervals"], 1);
    assert_eq!(doc["intervals"].as_array().unwrap().len(), 1);
    assert_eq!(doc["days"].as_array().unwrap().len(), 1);
    assert_eq!(doc["days"][0]["date"], "2026-03-10");
    assert_eq!(doc["fault_sessions_excluded"], 0);
    assert_eq!(
        ms_of(&doc, "live", "human"),
        Some(0),
        "没有开放区间 ⇒ 暂计 0"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ② 两次导出：除生成时间外逐字节一致
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn two_exports_of_the_same_input_differ_only_in_the_generated_time() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.task("t-b", "改代码", 2_000);
    h.session(
        "s-b",
        "t-b",
        "BACKGROUND",
        "finished",
        0,
        SH_NOON - 2 * HOUR,
    );
    h.interval(
        "i-b",
        "s-b",
        SH_NOON - 2 * HOUR,
        Some(SH_NOON - HOUR),
        Some(HOUR),
        0,
        None,
    );

    let first = h.export(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON);
    let second = h.export(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON);
    assert_eq!(
        first.text, second.text,
        "同一输入两次导出必须逐字节一致（含生成时间）"
    );

    // 只有一个数字不同（生成时间），位数一致 ⇒ 把它换回去后**逐字节**相同。
    let later = h.export(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON + 1);
    assert_ne!(later.text, first.text, "生成时间不同 ⇒ 文本不同");
    assert_eq!(
        later
            .text
            .replace(&(SH_NOON + 1).to_string(), &SH_NOON.to_string()),
        first.text,
        "除生成时间外没有任何字节随调用变化"
    );
    // 逐字段复核（比字节更细）：把生成时间对齐后整个文档相等。
    let mut a: Value = serde_json::from_str(&first.text).unwrap();
    let b: Value = serde_json::from_str(&later.text).unwrap();
    assert_eq!(b["generated_at"], SH_NOON + 1);
    a["generated_at"] = serde_json::json!(SH_NOON + 1);
    assert_eq!(a, b, "除生成时间外逐字段一致");
}

// ─────────────────────────────────────────────────────────────────────────────
// ③ 数字与 `stats` 的返回值逐字段相等
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn every_number_in_the_export_equals_the_stats_report_field_by_field() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);

    // 三条事实，范围只盖住 00:10–00:20：
    //   前台 [00:00, 01:00) ⇒ 范围内 10 分钟（**行自身**是 1 小时，两者不同义）
    //   后台 [00:05, 00:35) ⇒ 范围内 10 分钟
    //   待确认 [00:15, 00:25)（已知端点）⇒ 范围内 5 分钟
    h.session("s-a", "t-a", "FOREGROUND", "finished", 0, SH_MID);
    h.interval(
        "i-a",
        "s-a",
        SH_MID,
        Some(SH_MID + HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.session(
        "s-b",
        "t-a",
        "BACKGROUND",
        "finished",
        0,
        SH_MID + 5 * MINUTE,
    );
    h.interval(
        "i-b",
        "s-b",
        SH_MID + 5 * MINUTE,
        Some(SH_MID + 35 * MINUTE),
        Some(30 * MINUTE),
        0,
        None,
    );
    h.session(
        "s-c",
        "t-a",
        "FOREGROUND",
        "recovering",
        1,
        SH_MID + 15 * MINUTE,
    );
    h.interval(
        "i-c",
        "s-c",
        SH_MID + 15 * MINUTE,
        Some(SH_MID + 25 * MINUTE),
        // 候选端点不是既成事实 ⇒ 待确认区间**不带**行自身的时长（P2/P3 的写入形状）。
        None,
        1,
        None,
    );

    let from = SH_MID + 10 * MINUTE;
    let to = SH_MID + 20 * MINUTE;
    let report = h.report(from, to, "Asia/Shanghai");
    let (_, doc) = h.export_doc(from, to, "Asia/Shanghai", SH_NOON);

    // 三类的 12 列：每个字段都与报表逐字段相等。
    for class in [StatsClass::Confirmed, StatsClass::Live, StatsClass::Pending] {
        for measure in Measure::ALL {
            let expected = report.column(class, measure);
            let json = column(&doc, class_name(class), measure_name(measure));
            assert_eq!(json["class"], class_name(class));
            assert_eq!(json["measure"], measure_name(measure));
            assert_eq!(json["timezone"], expected.timezone);
            assert_eq!(json["range"]["from"], expected.range.from);
            assert_eq!(json["range"]["to"], expected.range.to);
            assert_eq!(json["as_of"], expected.as_of);
            assert_eq!(json["data_epoch"], expected.data_epoch);
            assert_eq!(json["revision"], expected.revision);
            assert_eq!(
                json["ms"].as_i64(),
                expected.ms,
                "{} / {} 的毫秒数必须与报表相等",
                class_name(class),
                measure_name(measure)
            );
            assert_eq!(json["intervals"], expected.intervals);
        }
    }

    // 具体数值（不是「非空」）：范围内人工 10 分钟、后台 10 分钟、待确认 5 分钟。
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(10 * MINUTE));
    assert_eq!(
        ms_of(&doc, "confirmed", "machine_background"),
        Some(10 * MINUTE)
    );
    assert_eq!(ms_of(&doc, "pending", "human"), Some(5 * MINUTE));
    assert_eq!(column(&doc, "pending", "human")["intervals"], 1);
    assert_eq!(ms_of(&doc, "confirmed", "machine_passive"), Some(0));
    assert_eq!(ms_of(&doc, "confirmed", "waiting"), Some(0));

    // 日桶：范围内只有这一天，逐字段与报表相等。
    let days = doc["days"].as_array().unwrap();
    assert_eq!(days.len(), report.days.len());
    assert_eq!(days.len(), 1);
    for (json, expected) in days.iter().zip(&report.days) {
        assert_eq!(json["date"], expected.date);
        assert_eq!(json["confirmed"].as_array().unwrap().len(), 4);
        for measure in Measure::ALL {
            let column = column(json, "confirmed", measure_name(measure));
            let expected_column = expected.column(measure);
            assert_eq!(
                column["ms"].as_i64(),
                expected_column.ms,
                "日桶 {} / {} 必须与报表相等",
                expected.date,
                measure_name(measure)
            );
            assert_eq!(column["intervals"], expected_column.intervals);
            assert_eq!(column["range"]["from"], expected_column.range.from);
            assert_eq!(column["range"]["to"], expected_column.range.to);
        }
    }

    // 明细：逐条逐字段相等（含 `duration_ms` 与 `clipped_ms` 的**不同义**）。
    let details = doc["intervals"].as_array().unwrap();
    assert_eq!(details.len(), report.intervals.len());
    assert_eq!(details.len(), 3);
    // 待确认候选**在**明细里，并带着自己的标记（与 `criteria.exclusions` 的文案一致）。
    assert_detail_review_flags(details);
    for (json, expected) in details.iter().zip(&report.intervals) {
        assert_eq!(json["id"], expected.id);
        assert_eq!(json["session_id"], expected.session_id);
        assert_eq!(json["task_id"], expected.task_id);
        assert_eq!(json["class"], class_name(expected.class));
        assert_eq!(json["measure"], measure_name(expected.measure));
        assert_eq!(json["started_at"], expected.started_at);
        assert_eq!(json["ended_at"].as_i64(), expected.ended_at);
        assert_eq!(json["duration_ms"].as_i64(), expected.duration_ms);
        assert_eq!(json["needs_review"], expected.needs_review);
        assert_eq!(json["clipped_ms"], expected.clipped_ms);
    }
    // 前台那一条：行自身 1 小时，范围内只有 10 分钟——两个数**不同义**。
    let human_detail = details
        .iter()
        .find(|detail| detail["id"] == "i-a")
        .expect("明细里必须有那条前台区间");
    assert_eq!(human_detail["duration_ms"], HOUR);
    assert_eq!(human_detail["clipped_ms"], 10 * MINUTE);
    // 待确认那一条：`duration_ms` 是 `null`（候选端点不是既成事实）。
    let pending_detail = details
        .iter()
        .find(|detail| detail["id"] == "i-c")
        .expect("明细里必须有那条待确认候选");
    assert!(pending_detail["duration_ms"].is_null());
    assert_eq!(pending_detail["clipped_ms"], 5 * MINUTE);
}

// ─────────────────────────────────────────────────────────────────────────────
// ④ 空范围：合法、不 panic
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_empty_range_is_a_valid_export_with_zero_columns_and_no_detail() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );

    // `from == to`：半开空范围（合法），不是负区间。
    let (exported, doc) = h.export_doc(SH_NOON, SH_NOON, "utc", SH_NOON);
    assert!(!exported.text.is_empty());
    assert_eq!(doc["range"]["from"], SH_NOON);
    assert_eq!(doc["range"]["to"], SH_NOON);
    assert_eq!(doc["timezone"], "UTC", "时区过归一入口（utc → UTC）");

    for class in ["confirmed", "live", "pending"] {
        assert_eq!(doc[class].as_array().unwrap().len(), 4, "每类固定四项");
        for measure in ["human", "machine_background", "machine_passive", "waiting"] {
            let column = column(&doc, class, measure);
            assert_eq!(column["range"]["from"], SH_NOON);
            assert_eq!(column["range"]["to"], SH_NOON);
            assert_eq!(column["intervals"], 0);
            if class == "pending" {
                assert!(
                    column["ms"].is_null(),
                    "空范围里待确认一条候选都没有 ⇒ 保留 null（不推算、也不填 0）"
                );
            } else {
                assert_eq!(column["ms"], 0, "{class} / {measure} 在空范围里就是 0");
            }
        }
    }
    assert_eq!(doc["intervals"].as_array().unwrap().len(), 0);
    assert_eq!(doc["days"].as_array().unwrap().len(), 0, "空范围没有日桶");
    assert_eq!(doc["tasks"].as_array().unwrap().len(), 0);
    assert_eq!(doc["fault_sessions_excluded"], 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑤ 明细求和 == 列合计（含 `ms == None` 那一支）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_detail_sums_to_each_column_total_and_keeps_null_for_unknown_endpoints() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 已确认：前台 1 小时 + 后台 1 小时（**并行**，互不合并）。
    h.session(
        "s-human",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-human",
        "s-human",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.session(
        "s-bg",
        "t-a",
        "BACKGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-bg",
        "s-bg",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    // 待确认（已知端点）：人工 10 分钟。
    h.session(
        "s-pend",
        "t-a",
        "FOREGROUND",
        "recovering",
        1,
        SH_NOON - HOUR,
    );
    h.interval(
        "i-pend",
        "s-pend",
        SH_NOON - HOUR,
        Some(SH_NOON - 50 * MINUTE),
        Some(10 * MINUTE),
        1,
        None,
    );
    // 待确认（**终点未知**）：被动会话，一条已知端点的候选都没有。
    h.session(
        "s-unknown",
        "t-a",
        "PASSIVE",
        "recovering",
        1,
        SH_NOON - 30 * MINUTE,
    );
    h.interval(
        "i-unknown",
        "s-unknown",
        SH_NOON - 30 * MINUTE,
        None,
        None,
        1,
        None,
    );
    // 实时暂计：真起一次前台计时，样本走过 5 分钟。
    h.task("t-live", "现在做的", 2_000);
    h.start("t-live");
    h.elapse(5 * MINUTE);

    let report = h.report(SH_MID, SH_MID + DAY, "Asia/Shanghai");
    let (_, doc) = h.export_doc(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON);

    // 逐列：`ms` 有数时，同类同 measure 的明细之和必须等于它。
    let mut none_columns = 0;
    for class in [StatsClass::Confirmed, StatsClass::Live, StatsClass::Pending] {
        for measure in Measure::ALL {
            let expected = report.column(class, measure);
            let sum: i64 = report
                .intervals
                .iter()
                .filter(|interval| interval.class == class && interval.measure == measure)
                .map(|interval| interval.clipped_ms)
                .sum();
            let json = column(&doc, class_name(class), measure_name(measure));
            match expected.ms {
                Some(ms) => assert_eq!(
                    sum,
                    ms,
                    "{} / {}：明细之和必须等于列合计",
                    class_name(class),
                    measure_name(measure)
                ),
                None => {
                    none_columns += 1;
                    assert_eq!(
                        sum,
                        0,
                        "{} / {}：终点未知不推算 ⇒ 明细贡献只能是 0",
                        class_name(class),
                        measure_name(measure)
                    );
                    assert!(
                        json["ms"].is_null(),
                        "{} / {}：`ms == None` 在导出里必须还是 null，不能悄悄变成 0",
                        class_name(class),
                        measure_name(measure)
                    );
                }
            }
        }
    }
    assert!(none_columns >= 1, "这个装置必须覆盖 `ms == None` 那一支");

    // 具体数值：已确认人工 1h、已确认后台 1h、暂计 5min、待确认人工 10min。
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(HOUR));
    assert_eq!(ms_of(&doc, "confirmed", "machine_background"), Some(HOUR));
    assert_eq!(ms_of(&doc, "live", "human"), Some(5 * MINUTE));
    assert_eq!(ms_of(&doc, "pending", "human"), Some(10 * MINUTE));
    // 逐 measure 判：人工有已知端点候选 ⇒ 有数；被动只有一条终点未知的候选 ⇒ null。
    assert_eq!(
        ms_of(&doc, "pending", "machine_passive"),
        None,
        "只有终点未知候选 ⇒ 该 measure 不给毫秒"
    );
    assert_eq!(column(&doc, "pending", "machine_passive")["intervals"], 1);

    // 全局口径：全部明细的 `clipped_ms` 之和 == 三类全部 measure 的合计（`None` 按 0）。
    let detail_sum: i64 = report
        .intervals
        .iter()
        .map(|interval| interval.clipped_ms)
        .sum();
    let column_sum: i64 = [StatsClass::Confirmed, StatsClass::Live, StatsClass::Pending]
        .iter()
        .flat_map(|class| Measure::ALL.iter().map(|measure| (*class, *measure)))
        .map(|(class, measure)| report.column(class, measure).ms.unwrap_or(0))
        .sum();
    assert_eq!(detail_sum, column_sum);
    assert_eq!(
        detail_sum,
        HOUR + HOUR + 5 * MINUTE + 10 * MINUTE,
        "期望数值本身：1h + 1h + 5min + 10min = 2h15min"
    );
    assert_eq!(
        doc["intervals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|detail| detail["clipped_ms"].as_i64().unwrap())
            .sum::<i64>(),
        detail_sum,
        "导出里的明细之和与报表一致"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑥ 导出的区间两两不重叠（P3 的保证在此可见）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_exported_intervals_never_overlap_even_after_a_boundary_touching_correction() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 两条已确认的前台区间，中间隔着一小时。
    h.session(
        "s-1",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 2 * HOUR,
    );
    h.interval(
        "i-1",
        "s-1",
        SH_NOON - 2 * HOUR,
        Some(SH_NOON - HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.session("s-2", "t-a", "FOREGROUND", "finished", 0, SH_NOON);
    h.interval(
        "i-2",
        "s-2",
        SH_NOON,
        Some(SH_NOON + 30 * MINUTE),
        Some(30 * MINUTE),
        0,
        None,
    );

    // P3 的修正入口把第二条**贴到**第一条的右端点：半开区间端点相接不算重叠。
    let outcome = history::correct(
        &mut h.db,
        WriteEnvelope::for_update(h.epoch.clone(), 0),
        CorrectRequest {
            session_id: "s-2".to_string(),
            interval_id: "i-2".to_string(),
            action: CorrectAction::Retime {
                started_at: SH_NOON - HOUR,
                ended_at: SH_NOON - 30 * MINUTE,
            },
            reason: None,
        },
        SH_MID + DAY,
    )
    .expect("端点相接不算重叠：贴着邻居边界的重定时是合法的");
    assert!(outcome.into_parts().1, "确实改了一条区间");

    let from = SH_NOON - 3 * HOUR;
    let to = SH_NOON + HOUR;
    let report = h.report(from, to, "Asia/Shanghai");
    let (_, doc) = h.export_doc(from, to, "Asia/Shanghai", SH_NOON);

    // 两条区间首尾相接：既没有缺口，也没有重叠。
    let spans: Vec<IntervalRange> = report
        .intervals
        .iter()
        .filter_map(|interval| {
            interval
                .ended_at
                .map(|end| IntervalRange::new(interval.started_at, end).unwrap())
        })
        .collect();
    assert_eq!(spans.len(), 2);
    for (index, left) in spans.iter().enumerate() {
        for right in spans.iter().skip(index + 1) {
            assert_eq!(
                left.overlap_ms(*right),
                0,
                "导出的区间两两不重叠（P3 的保证）：{left:?} 与 {right:?}"
            );
        }
    }
    assert_eq!(spans[0].end, spans[1].start, "端点相接：无缺口、无重叠");
    assert_eq!(spans[0].duration_ms(), HOUR);
    assert_eq!(spans[1].duration_ms(), 30 * MINUTE);
    assert_eq!(
        ms_of(&doc, "confirmed", "human"),
        Some(HOUR + 30 * MINUTE),
        "首尾相接的两段加起来正好是 1 小时 30 分"
    );
    assert_eq!(column(&doc, "confirmed", "human")["intervals"], 2);
    // 导出里同样两两不重叠（与报表同一批区间）。
    let detail_spans: Vec<IntervalRange> = doc["intervals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|detail| {
            IntervalRange::new(
                detail["started_at"].as_i64().unwrap(),
                detail["ended_at"].as_i64().unwrap(),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(detail_spans.len(), 2);
    assert_eq!(detail_spans[0].overlap_ms(detail_spans[1]), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑦ R-03：标签连接（只做连接，不重算时长）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_export_joins_current_tags_and_projects_without_recomputing_any_duration() {
    let mut h = setup(SH_NOON);
    h.project("p-1", "项目甲");
    h.task_in_project("t-a", "写文档", "p-1", 1_000);
    h.task("t-b", "改代码", 2_000);
    // 标签按 `tag.created_at, tag.id` 稳定排序（仓储的口径）。
    h.tag("tg-2", "Activity", "编码", 200);
    h.tag("tg-1", "Domain", "写作", 100);
    h.tag_task("t-a", "tg-1");
    h.tag_task("t-a", "tg-2");

    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.session(
        "s-b",
        "t-b",
        "BACKGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-b",
        "s-b",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );

    let (_, doc) = h.export_doc(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON);
    let tasks = doc["tasks"].as_array().unwrap();
    assert_eq!(
        tasks.len(),
        2,
        "明细里出现过的任务各一条（按 task_id 排序）"
    );

    // t-a：项目 + 两个标签，逐字段与 DTO 同形。
    let a = &tasks[0];
    assert_eq!(a["task"]["id"], "t-a");
    assert_eq!(a["task"]["title"], "写文档");
    assert_eq!(a["task"]["status"], "Doing");
    assert_eq!(a["task"]["project_id"], "p-1");
    assert_eq!(a["project"]["id"], "p-1");
    assert_eq!(a["project"]["name"], "项目甲");
    assert_eq!(a["project"]["status"], "active");
    let tags = a["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0]["id"], "tg-1");
    assert_eq!(tags[0]["name"], "写作");
    assert_eq!(tags[0]["kind"], "Domain");
    assert_eq!(tags[1]["id"], "tg-2");
    assert_eq!(tags[1]["name"], "编码");
    assert_eq!(tags[1]["kind"], "Activity");

    // t-b：没有项目、没有标签。
    let b = &tasks[1];
    assert_eq!(b["task"]["id"], "t-b");
    assert!(b["task"]["project_id"].is_null());
    assert!(b["project"].is_null());
    assert_eq!(b["tags"].as_array().unwrap().len(), 0);

    // **只做连接、不重算**：任务那一段里没有任何时长字段（数字只在列与明细里）。
    for task in tasks {
        let mut keys: Vec<&str> = task
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["project", "tags", "task"],
            "任务段只承载标签连接，不承载任何数字"
        );
    }
    // 标签行里也没有时长字段（按标签分配时长属 V0.2，本任务不做）。
    for tag in tasks[0]["tags"].as_array().unwrap() {
        for forbidden in ["ms", "clipped_ms", "duration_ms", "weight"] {
            assert!(
                tag.get(forbidden).is_none(),
                "标签连接不得携带 {forbidden}（不重算、不按 weight 分配）"
            );
        }
    }
    // 数字仍然只来自区间明细：人工 1 小时（t-a）、后台 1 小时（t-b）。
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(HOUR));
    assert_eq!(ms_of(&doc, "confirmed", "machine_background"), Some(HOUR));
    // 分类依据写在导出里（R-03）：第三方据此知道标签/项目是按**当前**归属连接的。
    assert_eq!(doc["criteria"]["classification"], "current_assignments");
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑧ 只读：不写库、不加 revision
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn an_export_writes_nothing_and_does_not_advance_the_revision() {
    let mut h = setup(SH_NOON);
    h.project("p-1", "项目甲");
    h.task_in_project("t-a", "写文档", "p-1", 1_000);
    h.tag("tg-1", "Domain", "写作", 100);
    h.tag_task("t-a", "tg-1");
    h.session(
        "s-a",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-a",
        "s-a",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );

    let before_changes = h.total_changes();
    let before_revision = h.revision();
    let exported = h.export(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON);

    assert_eq!(
        h.total_changes(),
        before_changes,
        "导出是只读的：一行都不写"
    );
    assert_eq!(h.revision(), before_revision, "导出不加 revision");
    assert_eq!(exported.revision, before_revision);
}

// ─────────────────────────────────────────────────────────────────────────────
// ⑨ 生产入口：真启动 + `AppState::export_json`
// ─────────────────────────────────────────────────────────────────────────────

struct AppFixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    lock_path: PathBuf,
}

fn app_fixture() -> AppFixture {
    let dir = tempfile::tempdir().unwrap();
    let fixture = AppFixture {
        db_path: dir.path().join("worktrace.db"),
        lock_path: dir.path().join("instance.lock"),
        _dir: dir,
    };
    let db = Db::open(&fixture.db_path).unwrap();
    migrate(db.connection()).unwrap();
    db.connection()
        .execute(
            "INSERT INTO task(id,title,status,row_version,created_at,updated_at)
             VALUES('t1','任务','Doing',0,1000,1000)",
            [],
        )
        .unwrap();
    drop(db);
    fixture
}

fn app_started(fixture: &AppFixture, clock: Arc<Mutex<FakeClock>>) -> Box<RunningApp> {
    let config = StartupConfig {
        db_path: fixture.db_path.clone(),
        lock_path: fixture.lock_path.clone(),
        sampling_interval_ms: 60_000,
    };
    let outcome = startup(config, Box::new(clock), Arc::new(NoSink), &NoProbe, &|| {
        Ok(())
    })
    .expect("启动应当成功");
    match outcome {
        Startup::Running(running) => running,
        Startup::AlreadyRunning { .. } => panic!("测试进程应当是唯一实例"),
    }
}

fn start_request(epoch: &str) -> StartRequest {
    StartRequest {
        expected_data_epoch: epoch.to_string(),
        task_id: "t1".into(),
        task_expected_version: 0,
        mode: SessionMode::Foreground,
        timer_kind: TimerKind::Stopwatch,
        target_duration_ms: None,
        expected_interval_ms: 30_000,
    }
}

fn session_request(guard: &mut AppState, epoch: &str) -> SessionRequest {
    let snapshot = guard.snapshot().unwrap();
    SessionRequest {
        expected_data_epoch: epoch.to_string(),
        session_id: snapshot.session_id.clone().unwrap(),
        session_expected_version: snapshot.session_version.unwrap(),
    }
}

/// 导出请求（与报表同一个请求类型）。
fn export_query(epoch: &str, from: i64, to: i64, timezone: &str) -> StatsRangeQuery {
    StatsRangeQuery {
        from,
        to,
        timezone: timezone.to_string(),
        expected_data_epoch: epoch.to_string(),
    }
}

#[test]
fn the_app_state_entry_point_exports_the_same_document_as_the_service() {
    let fixture = app_fixture();
    let wall = SH_MID;
    let clock = Arc::new(Mutex::new(FakeClock::new(wall, 0)));
    let running = app_started(&fixture, Arc::clone(&clock));
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    // 真起一次计时、走满 2 分钟、再结束 ⇒ 一条 2 分钟的已确认前台区间。
    {
        let mut guard = lock_app(&app);
        guard.start(start_request(&epoch)).unwrap();
    }
    for _ in 0..2 {
        clock.lock().unwrap().advance_both(MINUTE);
        let mut guard = lock_app(&app);
        guard.sample_tick().unwrap();
    }
    {
        let mut guard = lock_app(&app);
        let request = session_request(&mut guard, &epoch);
        guard.finish(request).unwrap();
    }

    let before_changes = {
        let guard = lock_app(&app);
        guard
            .db()
            .connection()
            .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before_revision = {
        let guard = lock_app(&app);
        require_meta(guard.db().connection()).unwrap().revision
    };

    let (exported, doc) = {
        let mut guard = lock_app(&app);
        let exported = guard
            .export_json(&export_query(&epoch, SH_MID, SH_MID + DAY, "Asia/Shanghai"))
            .expect("生产入口必须能导出");
        let doc: Value = serde_json::from_str(&exported.text).unwrap();
        (exported, doc)
    };

    // 数字来自与界面同一次查询：已确认人工 2 分钟。
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(2 * MINUTE));
    assert_eq!(column(&doc, "confirmed", "human")["intervals"], 1);
    assert_eq!(doc["data_epoch"], epoch);
    assert_eq!(exported.data_epoch, epoch);
    assert_eq!(doc["revision"], before_revision);
    assert_eq!(exported.revision, before_revision);
    // 生成时间：由调用方（AppState，走平台时钟接缝）给出——就是这一刻的墙钟。
    assert_eq!(doc["generated_at"], wall + 2 * MINUTE);
    assert_eq!(doc["as_of"], wall + 2 * MINUTE);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["timezone"], "Asia/Shanghai");

    // 只读：生产入口同样一行都不写。
    let after_changes = {
        let guard = lock_app(&app);
        guard
            .db()
            .connection()
            .query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(after_changes, before_changes, "导出不写库");

    // 再走一分钟（没有任何计时在跑）：生成时间跟着走，数字一个都不变。
    clock.lock().unwrap().advance_both(MINUTE);
    let (later, later_doc) = {
        let mut guard = lock_app(&app);
        let exported = guard
            .export_json(&export_query(&epoch, SH_MID, SH_MID + DAY, "Asia/Shanghai"))
            .expect("第二次导出");
        let doc: Value = serde_json::from_str(&exported.text).unwrap();
        (exported, doc)
    };
    assert_eq!(later_doc["generated_at"], wall + 3 * MINUTE);
    assert_eq!(
        ms_of(&later_doc, "confirmed", "human"),
        Some(2 * MINUTE),
        "生成时间前进，但工时事实不变"
    );
    assert_eq!(
        later_doc["revision"], before_revision,
        "纯读不前进 revision"
    );
    assert_ne!(later.text, exported.text, "生成时间不同 ⇒ 文本不同");
    // 两次导出之间会动的只有**时间水位**（`generated_at` 与每一处的 `as_of`）；数字与口径
    // 必须逐字段一致。
    let mut aligned = later_doc.clone();
    let mut expected = doc.clone();
    normalize_watermarks(&mut aligned);
    normalize_watermarks(&mut expected);
    assert_eq!(
        aligned, expected,
        "除时间水位（generated_at / as_of）外两份导出逐字段一致"
    );
}

#[test]
fn the_generated_time_is_the_wall_clock_and_not_the_data_watermark() {
    let fixture = app_fixture();
    let clock = Arc::new(Mutex::new(FakeClock::new(SH_MID, 0)));
    let running = app_started(&fixture, Arc::clone(&clock));
    let epoch = running.data_epoch().to_string();
    let app = Arc::clone(running.app());

    // 只拨**挂钟**、不动单调钟：数据的归属终点 `A(M)` 一动不动，
    // 而「生成本刻」的墙钟前进 2 秒——两个字段从此不相等（Ruling P5-19）。
    //
    // 为什么是 2000ms 而不是「5 秒」：08 §1 的判据是「两差任一绝对值 **严格大于** 2000ms」
    // 才算挂钟跳变（`platform::clock::THRESHOLD_MS`）。拨 5 秒会被判成跳变，
    // `stats_sample` 会**先落恢复事务再拒绝**（`RECOVERY_REQUIRED`）——那是异常路径、
    // 而且会写库，不是导出该走的正常路径。2000ms 恰好落在「不算越界」的那条边界上，
    // 既能制造两个不同的时刻，也不惊动检测器。
    clock.lock().unwrap().advance_wall(2_000);

    let (exported, doc) = {
        let mut guard = lock_app(&app);
        let exported = guard
            .export_json(&export_query(&epoch, SH_MID, SH_MID + DAY, "Asia/Shanghai"))
            .expect("小幅改钟不该让导出失败");
        let doc: Value = serde_json::from_str(&exported.text).unwrap();
        (exported, doc)
    };

    // 生成时间 = 生成本刻的墙钟（平台时钟接缝），**不是**样本的 `as_of`。
    assert_eq!(
        doc["generated_at"],
        SH_MID + 2_000,
        "generated_at 是生成本刻的墙钟"
    );
    assert_eq!(
        doc["as_of"], SH_MID,
        "数字的归属终点不动：数字仍全部来自那一次样本"
    );
    assert_ne!(
        doc["generated_at"], doc["as_of"],
        "两个字段必须能表达不同的时刻，否则「生成时间」没有信息"
    );
    // 事实与版本没被这次读时钟带动。
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(0));
    assert_eq!(doc["revision"], 0);
    assert_eq!(exported.revision, 0);
    assert_eq!(doc["data_epoch"], epoch);
    // 口径声明里写清两者不同义（第三方不必读代码）。
    assert!(
        doc["criteria"]["watermarks"]
            .as_str()
            .unwrap()
            .contains("不代表数据更新到那一刻"),
        "口径要写清 generated_at 不代表数据更新到那一刻"
    );
}

#[test]
fn the_exclusion_sentence_matches_the_pending_rows_that_are_in_the_detail() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 一条已确认（needs_review=0）+ 一条未作废的待确认候选（needs_review=1，已知端点）。
    h.session(
        "s-done",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-done",
        "s-done",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.session(
        "s-pend",
        "t-a",
        "FOREGROUND",
        "recovering",
        1,
        SH_NOON - HOUR,
    );
    h.interval(
        "i-pend",
        "s-pend",
        SH_NOON - HOUR,
        Some(SH_NOON - 50 * MINUTE),
        // 候选端点不是既成事实 ⇒ 待确认区间不带行自身的时长。
        None,
        1,
        None,
    );

    let (_, doc) = h.export_doc(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON);
    let details = doc["intervals"].as_array().unwrap();

    // ① 数据那一半：待确认候选**在**明细里，带着 `needs_review=true` 与 `class=pending`
    //    （照字面把 `needs_review` 的行滤掉的第三方会丢掉它）。
    assert_eq!(details.len(), 2, "已确认与待确认各一条，都在明细里");
    assert_detail_review_flags(details);
    let pending_row = details
        .iter()
        .find(|detail| detail["class"] == "pending")
        .expect("未作废的待确认候选必须出现在明细里");
    assert_eq!(pending_row["id"], "i-pend");
    assert_eq!(pending_row["needs_review"], true);
    assert_eq!(pending_row["clipped_ms"], 10 * MINUTE);
    let confirmed_row = details
        .iter()
        .find(|detail| detail["class"] == "confirmed")
        .expect("已确认区间也在明细里");
    assert_eq!(confirmed_row["id"], "i-done");
    assert_eq!(confirmed_row["needs_review"], false);
    // 那一行的贡献确实计入 `pending` 列（文案里说的「clipped_ms 计入 pending 列」）。
    assert_eq!(ms_of(&doc, "pending", "human"), Some(10 * MINUTE));
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(HOUR));

    // ② 文案那一半：`criteria.exclusions` 必须与上面的数据说同一件事。
    let exclusions = doc["criteria"]["exclusions"]
        .as_str()
        .expect("排除口径必须是字符串");
    assert!(
        exclusions.contains("不进任何「已确认」数字"),
        "三者的共同口径必须写清（needs_review=1 只要不进已确认，不是不进明细）：{exclusions}"
    );
    assert!(
        exclusions.contains("voided_at 非空与 discarded 会话的区间也不进明细"),
        "这句必须挂在 voided/discarded 上，别挂到 needs_review 上——它只说已作废 / 已丢弃，\
         不声称穷尽「不进明细」的行（损坏会话与零长度行也不进明细）：{exclusions}"
    );
    assert!(
        exclusions.contains("未作废的待确认候选会出现在明细里"),
        "待确认候选在明细里这件事必须写进口径，否则第三方会照字面滤掉它们：{exclusions}"
    );
    assert!(
        exclusions.contains("clipped_ms 计入 pending 列"),
        "要靠它才能把明细加回 `pending` 列的合计：{exclusions}"
    );
}

#[test]
fn a_broken_session_is_counted_in_the_criteria_and_keeps_out_of_the_detail() {
    let mut h = setup(SH_NOON);
    h.task("t-a", "写文档", 1_000);
    // 健康会话：一段 1 小时的已确认闭合区间。
    h.session(
        "s-ok",
        "t-a",
        "FOREGROUND",
        "finished",
        0,
        SH_NOON - 3 * HOUR,
    );
    h.interval(
        "i-ok",
        "s-ok",
        SH_NOON - 3 * HOUR,
        Some(SH_NOON - 2 * HOUR),
        Some(HOUR),
        0,
        None,
    );
    // 第 1 类（不变量损坏）：非 running 会话残留开放区间（P3 的判定，形状照
    // `tests/stats_window.rs` 的 `intervals_of_a_broken_session_are_not_counted_as_confirmed`）。
    // 它那段看似可信的 1 小时闭合区间也一律不计、不进明细——正是这一条会让导出里
    // `fault_sessions_excluded` 大于 0，而 T5 的端到端夹具恒为 0、照不到这里。
    h.session(
        "s-fault",
        "t-a",
        "FOREGROUND",
        "paused",
        0,
        SH_NOON - 2 * HOUR,
    );
    h.interval(
        "i-fault-closed",
        "s-fault",
        SH_NOON - 2 * HOUR,
        Some(SH_NOON - HOUR),
        Some(HOUR),
        0,
        None,
    );
    h.interval(
        "i-fault-open",
        "s-fault",
        SH_NOON - HOUR,
        None,
        None,
        0,
        None,
    );

    let (_, doc) = h.export_doc(SH_MID, SH_MID + DAY, "Asia/Shanghai", SH_NOON);
    let details = doc["intervals"].as_array().unwrap();

    // ① 计数那一半：被排除的会话数在导出里给得出来（第三方能解释「为什么少了行」）。
    assert_eq!(doc["fault_sessions_excluded"], 1);
    // 数字只算健康会话那 1 小时：损坏会话的两行一条都没进。
    assert_eq!(ms_of(&doc, "confirmed", "human"), Some(HOUR));
    assert_eq!(column(&doc, "confirmed", "human")["intervals"], 1);

    // ② 明细那一半：损坏会话的两行都不在明细里（闭合的那段也不在——不是只挡开放区间）。
    assert_eq!(details.len(), 1, "明细里只剩健康会话那一条");
    assert_eq!(details[0]["id"], "i-ok");
    let summed: i64 = details
        .iter()
        .map(|detail| detail["clipped_ms"].as_i64().unwrap())
        .sum();
    assert_eq!(
        summed, HOUR,
        "被排除的行在列与明细里同时缺席 ⇒ `Σ clipped_ms == 列 ms` 仍然成立"
    );

    // ③ 文案那一半：少掉的行必须在口径里可解释，而且指向那个计数字段。
    let exclusions = doc["criteria"]["exclusions"]
        .as_str()
        .expect("排除口径必须是字符串");
    assert!(
        exclusions.contains("第 1 类"),
        "口径要写明是第 1 类（不变量损坏）会话的区间不进明细：{exclusions}"
    );
    assert!(
        exclusions.contains("fault_sessions_excluded"),
        "口径要指向 `fault_sessions_excluded`，否则第三方只能猜为什么少了行：{exclusions}"
    );
}
