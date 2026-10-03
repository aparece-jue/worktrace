//! P4 Task 1 的领域输入校验测试（02 §2/§9；04 F-002/F-004/F-005/F-010）。
//!
//! 计划要求的用例：闰年与不存在的日期、未知时区 `Mars/Olympus`、`UTC`/上海/纽约、
//! 空输入、非法标签 kind/parent，以及发布环境（离线、无系统 tzdata）下的时区解析。
//!
//! 两点说明：
//! - 这里**按规则组织**用例，不按函数一个机械测试（01 §3）。
//! - 时区解析是全套用例里唯一依赖外部数据的部分：Windows 没有 zoneinfo，
//!   解析全靠 `jiff` 的 `tzdb-bundle-platform`。所以「五个时区能解析」加上
//!   「同一时刻在不同时区落在不同的日期」本身就是**发布环境离线可用**的证据。

use worktrace_lib::domain::error::DomainError;
use worktrace_lib::domain::localdate::LocalDate;
use worktrace_lib::domain::project::ProjectStatus;
use worktrace_lib::domain::tag::{self, TagKind};
use worktrace_lib::error::AppError;
use worktrace_lib::services::{catalog, daily_plan};
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::init_meta;
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::task_repo;

/// 校验类失败一律是 `DOMAIN_ERROR`，且 `detail` 是**用户读得懂的中文**。
///
/// 为什么断言落在 `detail` 而不是 `message`：前端只按 `code` 分支，而 `detail` 会被
/// `AppError::Domain` 的 Display 原样拼进用户看到的句子（00 §4），
/// 「文案口径」这条规则的载体就是它。
///
/// 两个恒真陷阱（评审指出后修正，别再写回去）：
/// - `detail().is_some()`：上一行的 `code() == "DOMAIN_ERROR"` 已经蕴含它
///   （`AppError::Domain` 必定带 detail），断言不可能失败；
/// - 在 `message()` 上找中文：它的模板是 `format!("操作不被允许：{detail}")`，
///   模板自带中文，于是 detail 是空白、是英文、是 SQL 片段都照样通过。
fn assert_domain_error(err: AppError) -> AppError {
    assert_eq!(err.code(), "DOMAIN_ERROR", "校验失败应当是领域拒绝");
    let detail = err.detail().unwrap_or_default();
    assert!(
        detail
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "领域拒绝的 detail 必须是中文（空串、英文、SQL 片段都不许）：{detail:?}"
    );
    err
}

// ─────────────────────────────────────────────────────────────────────────────
// 本地日期：形状 + 真实日历
// ─────────────────────────────────────────────────────────────────────────────

/// 真实存在的日子都要过，含闰年（2024、2000）与闰年的 2 月 29。
#[test]
fn real_calendar_days_are_accepted() {
    for raw in [
        "2024-02-29", // 闰年
        "2000-02-29", // 世纪闰年（能被 400 整除）
        "2026-10-03",
        "2023-01-31",
        "2023-04-30",
        "0001-01-01", // 4 位年的下界
        "9999-12-31", // 4 位年的上界
    ] {
        let date = LocalDate::parse(raw).unwrap_or_else(|e| panic!("{raw} 应合法：{e}"));
        assert_eq!(date.to_string(), raw, "落库形状必须与输入逐字相同");
    }
}

/// 不存在的日子一律拒：闰年规则、月长、越界的月与日。
#[test]
fn nonexistent_calendar_days_are_rejected() {
    for raw in [
        "2023-02-29", // 平年没有 2 月 29
        "2023-02-30",
        "2024-02-30", // 闰年也只有 29
        "2100-02-29", // 世纪年但不是 400 的倍数
        "2023-04-31", // 4 月只有 30 天
        "2024-13-01",
        "2024-00-10",
        "2024-01-00",
        "2024-01-32",
    ] {
        assert!(
            LocalDate::parse(raw).is_err(),
            "{raw} 不是真实存在的日期，必须拒绝"
        );
    }
}

/// 形状是固定宽度：不做宽松解析（`2024-2-29` 这种「差一点」的输入不算合法）。
#[test]
fn the_date_shape_is_fixed_width() {
    for raw in [
        "2024-2-29",
        "24-02-29",
        "2024-002-29",
        "2024-02-9",
        "2024/02/29",
        "2024-02-29T00:00:00",
        "2024-02-29 ",
        " 2024-02-29",
        "2024-02-29\n",
        "2024-02-2九",
    ] {
        assert!(LocalDate::parse(raw).is_err(), "{raw:?} 形状不对，必须拒绝");
    }
}

/// 空输入单独报「不能为空」——前端拿到的提示才有指向性。
#[test]
fn blank_date_input_reports_empty_text() {
    for raw in ["", "   ", "\t"] {
        assert_eq!(
            LocalDate::parse(raw).unwrap_err(),
            DomainError::EmptyText {
                field: "本地日期"
            }
        );
    }
}

/// 组件可读，且与输入一致（下游要拿它做展示与分列）。
#[test]
fn date_components_are_readable() {
    let d = LocalDate::parse("2024-02-29").unwrap();
    assert_eq!((d.year(), d.month(), d.day()), (2024, 2, 29));
}

/// `new` 与 `parse` 是同一套日历规则，不是两条路径。
#[test]
fn local_date_new_validates_the_same_calendar() {
    assert_eq!(
        LocalDate::new(2024, 2, 29).unwrap(),
        LocalDate::parse("2024-02-29").unwrap()
    );
    assert!(LocalDate::new(2023, 2, 29).is_err());
    assert!(LocalDate::new(2024, 4, 31).is_err());
    // 4 位年的范围之外无法落回 `YYYY-MM-DD`，构造就该失败。
    assert!(LocalDate::new(10_000, 1, 1).is_err());
}

/// 服务入口把领域错误映射成契约错误：前端只按 `code` 分支。
#[test]
fn the_date_service_entry_maps_errors_to_the_contract() {
    assert_eq!(
        daily_plan::parse_local_date("2024-02-29").unwrap(),
        LocalDate::parse("2024-02-29").unwrap()
    );

    let err = assert_domain_error(daily_plan::parse_local_date("2023-02-29").unwrap_err());
    assert!(
        err.message().contains("2023-02-29"),
        "文案要带上值：{err:?}"
    );

    assert_domain_error(daily_plan::parse_local_date("").unwrap_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// 时区：真的解析，而不是「非空白就算合法」
// ─────────────────────────────────────────────────────────────────────────────

/// 把一个 RFC 3339 时刻换成挂钟毫秒——storage 与 clock 用的就是毫秒。
fn wall_ms(rfc3339: &str) -> i64 {
    rfc3339
        .parse::<jiff::Timestamp>()
        .expect("测试里的时刻字面量必须合法")
        .as_millisecond()
}

/// 发布环境离线可用：不读系统 tzdata 也能解析 IANA 时区。
///
/// 本机是 Windows（没有 zoneinfo 目录），这些名字能解析出**带转换规则**的时区，
/// 说明数据来自打进产物的 tzdb，而不是机器上的某个文件。
#[test]
fn timezone_resolution_works_without_system_tzdata() {
    for name in [
        "UTC",
        "Asia/Shanghai",
        "America/New_York",
        "Europe/Warsaw",
        "Australia/Lord_Howe", // 半小时偏移的时区，只有真库才有
    ] {
        let key = daily_plan::normalize_timezone(name).unwrap();
        assert_eq!(key, name, "已是 IANA 名称的输入不该被改写");
        assert!(jiff::tz::TimeZone::get(&key).is_ok());
    }
    // 「解析成功」不是无条件 true：库找不到就必须失败。
    assert!(jiff::tz::TimeZone::get("Mars/Olympus").is_err());
}

/// 未知时区、任意无空白串、非 IANA 的哨兵值，全都拒。
#[test]
fn unknown_timezones_are_rejected() {
    for raw in [
        "Mars/Olympus",
        "Asia/Shangai",   // 少一个 h 的错拼
        "abcdefg",        // 任意无空白串
        "GMT+8",          // 不是 IANA 名称（真名是 Etc/GMT-8）
        "+08:00",         // 固定偏移不是 IANA 名称
        "2026-10-03",     // 形状像日期也不是时区
        "Etc/Unknown",    // jiff 的「未知」哨兵，描述不了日界
        "Asia/Shang hai", // 含空白的乱串
    ] {
        let err = assert_domain_error(daily_plan::normalize_timezone(raw).unwrap_err());
        assert!(
            err.message().contains(raw),
            "{raw:?} 的拒绝理由要带上这个值：{err:?}"
        );
    }
}

/// `UTC` 必须合法——它是本机时区映射失败时的最后一道退路（规格里点名）。
#[test]
fn utc_is_always_accepted() {
    assert_eq!(daily_plan::normalize_timezone("UTC").unwrap(), "UTC");
}

/// 空输入单独报「不能为空」。
#[test]
fn blank_timezone_input_reports_empty_text() {
    let expected = DomainError::EmptyText { field: "时区" }.to_string();
    for raw in ["", "   ", "\t"] {
        let err = assert_domain_error(daily_plan::normalize_timezone(raw).unwrap_err());
        assert_eq!(
            err.detail(),
            Some(expected.as_str()),
            "空输入要走 EmptyText 这条分支"
        );
    }
}

/// 别名策略在读写两侧一致：`normalize_timezone` 的输出是**不动点**。
///
/// 写路径存它、读路径拿它再算一次还是同一个键，所以
/// `daily_plan(task_id, local_date, timezone)` 不会为同一个时区裂成两行。
#[test]
fn the_normalized_timezone_is_a_fixed_point_for_read_and_write() {
    for raw in ["UTC", "utc", " Asia/Shanghai ", "America/New_York"] {
        let key = daily_plan::normalize_timezone(raw).unwrap();
        assert_eq!(daily_plan::normalize_timezone(&key).unwrap(), key, "{raw}");
    }
    assert_eq!(
        daily_plan::normalize_timezone("utc").unwrap(),
        daily_plan::normalize_timezone("UTC").unwrap(),
        "大小写别名必须收敛到同一个存储键"
    );
}

/// 换时区**不**改写旧计划的键：本任务不提供任何迁移入口，
/// 这条用例钉住它的前提——同一个时刻在两个时区是不同的键、也可能是不同的日期。
#[test]
fn changing_the_timezone_does_not_reuse_the_old_plan_key_or_date() {
    let shanghai = daily_plan::normalize_timezone("Asia/Shanghai").unwrap();
    let new_york = daily_plan::normalize_timezone("America/New_York").unwrap();
    assert_ne!(shanghai, new_york);

    let at = wall_ms("2026-10-03T16:30:00Z");
    assert_eq!(
        daily_plan::local_date_at(&shanghai, at)
            .unwrap()
            .to_string(),
        "2026-10-04",
        "上海已是第二天"
    );
    assert_eq!(
        daily_plan::local_date_at(&new_york, at)
            .unwrap()
            .to_string(),
        "2026-10-03",
        "纽约还是当天"
    );
}

/// 日界跟着**所选时区**走；`UTC` 只是其中一个时区，不是默认答案。
#[test]
fn the_local_day_follows_the_selected_zone() {
    let at = wall_ms("2026-10-03T23:50:00Z");
    assert_eq!(
        daily_plan::local_date_at("UTC", at).unwrap().to_string(),
        "2026-10-03"
    );
    assert_eq!(
        daily_plan::local_date_at("Asia/Shanghai", at)
            .unwrap()
            .to_string(),
        "2026-10-04"
    );
    // 未规范化的别名也走同一条校验路径，结果一致。
    assert_eq!(
        daily_plan::local_date_at("utc", at).unwrap(),
        daily_plan::local_date_at("UTC", at).unwrap()
    );
}

/// 日期换算复用同一套时区校验：没校验过的名字不许拿来算日期。
#[test]
fn the_date_conversion_rejects_zones_that_never_passed_validation() {
    assert_domain_error(daily_plan::local_date_at("Mars/Olympus", 0).unwrap_err());
    assert_domain_error(daily_plan::local_date_at("", 0).unwrap_err());
}

/// 超出可表示范围的时刻**报错**，不做静默钳制。
///
/// 钳制会把一个坏时刻变成一个看起来合理的日期，那正是「日期错了一天」
/// 这类最难查的缺陷的来源。这条 detail 是**手写**的（不是 `DomainError` 渲染出来的），
/// 所以它必须经过 `assert_domain_error` 的中文断言——手写文案正是最容易写成英文的地方。
#[test]
fn an_out_of_range_instant_is_rejected_instead_of_clamped() {
    assert_domain_error(daily_plan::local_date_at("UTC", i64::MAX).unwrap_err());
    assert!(daily_plan::local_date_at("UTC", i64::MIN).is_err());
}

/// 系统时区要真的能读出来（`tz-system`），并且能直接当存储键用。
#[test]
fn the_system_timezone_is_usable_and_already_normalized() {
    let name = daily_plan::system_timezone_name().expect("本机系统时区应能映射到 IANA 名称");
    assert!(!name.trim().is_empty());
    assert_eq!(
        daily_plan::normalize_timezone(&name).unwrap(),
        name,
        "系统时区本身必须是已规范化的键"
    );
    // 它得能真的算日期——解析出名字不等于拿到可用的转换规则。
    assert!(daily_plan::local_date_at(&name, wall_ms("2026-10-03T16:30:00Z")).is_ok());
    assert_eq!(
        Some(name.as_str()),
        jiff::tz::TimeZone::system().iana_name()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 标签：四类、命名规范化、拒绝层级
// ─────────────────────────────────────────────────────────────────────────────

/// 取值域就是规格里的四类，一个不多一个不少（Knowledge 属 V0.2）。
#[test]
fn tag_kind_accepts_exactly_the_four_v01_kinds() {
    assert_eq!(TagKind::ALL.len(), 4);
    for kind in TagKind::ALL {
        assert_eq!(
            TagKind::parse(kind.as_str()),
            Some(kind),
            "as_str/parse 往返"
        );
    }
    let names: Vec<&str> = TagKind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(names, vec!["Domain", "Activity", "Context", "Report"]);
}

/// `Knowledge` 与非取值域的写法一律拒（schema 的 CHECK 也是这套口径）。
#[test]
fn tag_kind_rejects_knowledge_and_anything_else() {
    for raw in ["Knowledge", "knowledge", "domain", "Tag", "", " Report"] {
        assert_eq!(TagKind::parse(raw), None, "{raw:?} 不在取值域里");
    }
}

/// 服务入口：空输入与非取值域要能分辨。
#[test]
fn the_tag_kind_entry_distinguishes_blank_from_unknown() {
    assert_eq!(
        catalog::parse_tag_kind("Context").unwrap(),
        TagKind::Context
    );

    let unknown = assert_domain_error(catalog::parse_tag_kind("Knowledge").unwrap_err());
    assert!(unknown.message().contains("Knowledge"), "{unknown:?}");

    assert_domain_error(catalog::parse_tag_kind("").unwrap_err());
}

/// 名字只去首尾空白；大小写是**有意义**的（`Abc` 与 `abc` 是两个标签）。
#[test]
fn tag_names_are_trimmed_and_case_significant() {
    assert_eq!(tag::normalize_name("  写作  ").unwrap(), "写作");
    assert_eq!(tag::normalize_name("写作").unwrap(), "写作");
    assert_eq!(
        tag::normalize_name("  写作  ").unwrap(),
        tag::normalize_name("写作").unwrap()
    );
    assert_ne!(
        tag::normalize_name("Abc").unwrap(),
        tag::normalize_name("abc").unwrap()
    );
    // 规范化是幂等的：同一个名字处理两次不会越走越远。
    let once = tag::normalize_name("  写作  ").unwrap();
    assert_eq!(tag::normalize_name(&once).unwrap(), once);
}

/// 空名（含全空白）拒绝，且走「不能为空」这条分支。
#[test]
fn blank_tag_names_are_rejected() {
    for raw in ["", "   ", "\n"] {
        assert_eq!(
            tag::normalize_name(raw).unwrap_err(),
            DomainError::EmptyText { field: "标签名" }
        );
        assert_domain_error(catalog::normalize_tag_name(raw).unwrap_err());
    }
}

/// V0.1 没有标签层级：非空 `parent_id` 拒绝，空白串按「未提供」处理。
#[test]
fn tag_parents_are_rejected_in_v01() {
    assert!(tag::ensure_no_parent(None).is_ok());
    // 前端把「不选」序列化成空串是常见形状；空 ID 指向不了任何标签，
    // 按「未提供」处理不会放过任何非法数据。
    assert!(tag::ensure_no_parent(Some("")).is_ok());
    assert!(tag::ensure_no_parent(Some("   ")).is_ok());
    assert!(tag::ensure_no_parent(Some("tag-parent")).is_err());
    assert_eq!(
        tag::ensure_no_parent(Some("tag-parent")).unwrap_err(),
        DomainError::NotInThisVersion {
            what: "标签层级"
        }
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 项目状态：与 02 一致，但 V0.1 只写 active/archived
// ─────────────────────────────────────────────────────────────────────────────

/// 取值域是 02 §2 的三个值，读写往返一致。
#[test]
fn project_status_accepts_the_three_spec_values() {
    assert_eq!(ProjectStatus::ALL.len(), 3);
    for status in ProjectStatus::ALL {
        assert_eq!(ProjectStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(ProjectStatus::parse("active"), Some(ProjectStatus::Active));
    assert_eq!(
        ProjectStatus::parse("archived"),
        Some(ProjectStatus::Archived)
    );
    assert_eq!(ProjectStatus::parse("done"), Some(ProjectStatus::Done));
    for raw in ["Active", "deleted", "", " archived"] {
        assert_eq!(ProjectStatus::parse(raw), None, "{raw:?} 不在取值域里");
    }
}

/// V0.1 的写范围只有创建（`active`）与归档（`archived`）；`done` 读得懂但写不进去。
#[test]
fn project_status_v01_write_scope_is_active_and_archived_only() {
    assert!(ProjectStatus::Active.is_writable_in_v01());
    assert!(ProjectStatus::Archived.is_writable_in_v01());
    assert!(
        !ProjectStatus::Done.is_writable_in_v01(),
        "done 属后续版本，UI 只提供创建/重命名/归档"
    );
}

/// 服务入口是**写路径**的入口：`done` 也拒。
#[test]
fn the_project_status_entry_rejects_done_and_unknown_values() {
    assert_eq!(
        catalog::parse_project_status("active").unwrap(),
        ProjectStatus::Active
    );
    assert_eq!(
        catalog::parse_project_status("archived").unwrap(),
        ProjectStatus::Archived
    );
    for raw in ["done", "Active", "", "deleted"] {
        assert_domain_error(catalog::parse_project_status(raw).unwrap_err());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 存储层的兜底：唯一索引与 CHECK 的实际口径
// ─────────────────────────────────────────────────────────────────────────────

/// 建库 + 迁移 + 元数据 + 一个任务。样板抄自 `tests/transaction_boundary.rs::bootstrap`。
fn bootstrap() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    init_meta(&tx).unwrap();
    task_repo::create_task(&tx, "t1", "任务", None, 1000).unwrap();
    tx.commit().unwrap();

    (dir, db)
}

/// 直接插一行标签。
///
/// T1 还没有 `tag_repo`（那是 T3），而这里要验的恰恰是 **schema 的**唯一索引口径，
/// 所以走裸 SQL；服务层的预检必须与它一致，T3 落地时对照这条用例写。
fn insert_tag(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    kind: TagKind,
    name: &str,
) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO tag(id, kind, name, row_version, created_at) VALUES(?1, ?2, ?3, 0, 1000)",
        rusqlite::params![id, kind.as_str(), name],
    )
}

/// 「同 kind、大小写敏感唯一」由 `uq_tag_root(kind,name)` 执行
/// （默认 BINARY 排序规则 ⇒ 大小写敏感）。规范化之后同名的才互斥。
#[test]
fn tag_uniqueness_is_per_kind_and_case_sensitive() {
    let (_dir, mut db) = bootstrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();

    let name = tag::normalize_name("  写作  ").unwrap();
    insert_tag(&tx, "tag-1", TagKind::Domain, &name).unwrap();

    // 规范化后同名 → 被唯一索引拒绝，而不是靠领域规则去重。
    let dup = insert_tag(&tx, "tag-2", TagKind::Domain, &name).unwrap_err();
    assert!(
        dup.to_string().contains("UNIQUE"),
        "应当由唯一索引拒绝，实际是：{dup}"
    );

    // 仅大小写不同是两个标签。
    insert_tag(&tx, "tag-3", TagKind::Domain, "Abc").unwrap();
    insert_tag(&tx, "tag-4", TagKind::Domain, "abc").unwrap();

    // 唯一性按 kind 分组：同名换个 kind 可以并存。
    insert_tag(&tx, "tag-5", TagKind::Activity, &name).unwrap();
}

/// `project.status` 的 CHECK 允许 `done`——所以「V0.1 不写 done」这条规则
/// 只能由服务入口表达，不能指望 schema 帮忙挡。
#[test]
fn the_project_status_column_holds_done_even_though_v01_never_writes_it() {
    let (_dir, mut db) = bootstrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();

    let insert = |id: &str, status: &str| {
        tx.execute(
            "INSERT INTO project(id, name, row_version, status, created_at, updated_at)
             VALUES(?1, '项目', 0, ?2, 1000, 1000)",
            rusqlite::params![id, status],
        )
    };

    insert("p-done", "done").expect("读路径必须读得懂 done（02 §2 的取值域里有它）");
    assert!(
        insert("p-bad", "deleted").is_err(),
        "取值域外的状态由 CHECK 兜底"
    );
    assert_domain_error(catalog::parse_project_status("done").unwrap_err());
}

/// `daily_plan.local_date` 的 GLOB 只管**形状**：`2023-02-30` 写得进去。
/// 所以日期合法性必须在领域层判——这条用例就是那份证据。
#[test]
fn the_local_date_shape_check_is_not_a_date_check() {
    let (_dir, mut db) = bootstrap();
    let tx = db.connection_mut().unchecked_transaction().unwrap();

    tx.execute(
        "INSERT INTO daily_plan(task_id, local_date, timezone) VALUES('t1', '2023-02-30', 'UTC')",
        [],
    )
    .expect("GLOB 只约束形状：形状对了就写得进去");

    assert!(
        tx.execute(
            "INSERT INTO daily_plan(task_id, local_date, timezone) VALUES('t1', '2023-2-30', 'UTC')",
            [],
        )
        .is_err(),
        "形状不对的仍被 GLOB 挡住"
    );

    assert!(
        LocalDate::parse("2023-02-30").is_err(),
        "领域层必须拒掉库里那个不存在的日期"
    );
}
