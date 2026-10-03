//! P4 Task 3：标签与幂等关联（F-005 基础标签）。
//!
//! 断言口径按总纲 §5 第 8 条：
//! - **被拒的用户命令**：`revision` 不变、`tag` / `task_tag` 各自的行数与字段值不变、
//!   审计（`task_change`）无新增、涉及的既有记录（任务行、标签行）逐字不变
//!   （`baseline_of` / `assert_unchanged` 一次断言这几件事）；
//! - **幂等重复**（重复打标、重复去标）：第二次调用 `revision` 不变，且返回值明确
//!   表达「没有变化」（`WriteOutcome::Unchanged`）；
//! - **错误**：断言 `code()` 字符串；文案口径用 `assert_domain_error`——CJK 判定落在
//!   `detail()` 上。**不在 `message()` 上找中文**：它的模板 `操作不被允许：{detail}`
//!   自带中文，对 `DOMAIN_ERROR` 恒真（Task 1 踩过的坑）。
//!
//! 建库样板照 `tests/transaction_boundary.rs::bootstrap`。

use serde_json::json;

use worktrace_lib::domain::tag::TagKind;
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::services::catalog;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::tag_repo::{self, TagRow};
use worktrace_lib::storage::task_repo::{self, TaskRow};
use worktrace_lib::storage::WriteOutcome;

// ─────────────────────────────────────────────────────────────────────────────
// 夹具
// ─────────────────────────────────────────────────────────────────────────────

struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
    /// 建库时定下的库身份，用来构造合法的写请求。
    epoch: String,
}

/// 新建命令的信封（只需 epoch）。
fn create_env(epoch: &str) -> WriteEnvelope {
    WriteEnvelope::for_create(epoch.to_string())
}

/// 临时文件库 + 迁移 + 一个 run + 两个任务 `t1` / `t2`（都走仓储建，口径一致）。
fn bootstrap() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Db::open(dir.path().join("w.db")).unwrap();
    migrate(db.connection()).unwrap();

    let tx = db.connection_mut().unchecked_transaction().unwrap();
    let meta = init_meta(&tx).unwrap();
    tx.execute(
        "INSERT INTO application_run(id, started_at) VALUES('run-1', 1000)",
        [],
    )
    .unwrap();
    task_repo::create_task(&tx, "t1", "任务一", None, 1000).unwrap();
    task_repo::create_task(&tx, "t2", "任务二", None, 1000).unwrap();
    tx.commit().unwrap();

    Fixture {
        _dir: dir,
        db,
        epoch: meta.data_epoch,
    }
}

impl Fixture {
    fn revision(&self) -> i64 {
        require_meta(self.db.connection()).unwrap().revision
    }

    fn count(&self, table: &str) -> i64 {
        self.db
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn tag(&self, id: &str) -> Option<TagRow> {
        tag_repo::get_tag(self.db.connection(), id).unwrap()
    }

    /// 全部标签（含四类），按仓储的稳定排序。
    fn all_tags(&self) -> Vec<TagRow> {
        tag_repo::list_tags(self.db.connection(), None).unwrap()
    }

    fn tags_of(&self, task_id: &str) -> Vec<TagRow> {
        catalog::tags_of_task(&self.db, task_id).unwrap()
    }

    /// 全部关联的快照：行数、字段值与 `weight` 一起钉住。
    ///
    /// 排序取「任务 + 标签的创建顺序」，与 `tags_of_task` 同一口径——用裸的
    /// `tag_id`（UUID）排序虽然也稳定，但断言里读不出「谁先谁后」。
    fn links(&self) -> Vec<(String, String, Option<f64>)> {
        let mut stmt = self
            .db
            .connection()
            .prepare(
                "SELECT link.task_id, link.tag_id, link.weight
                 FROM task_tag link JOIN tag ON tag.id = link.tag_id
                 ORDER BY link.task_id, tag.created_at, link.tag_id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    /// 某一对关联的 `weight`：`None` = 没有这条关联；`Some(None)` = 关联存在但权重为空。
    fn link_weight(&self, task_id: &str, tag_id: &str) -> Option<Option<f64>> {
        use rusqlite::OptionalExtension;
        self.db
            .connection()
            .query_row(
                "SELECT weight FROM task_tag WHERE task_id = ?1 AND tag_id = ?2",
                rusqlite::params![task_id, tag_id],
                |r| r.get::<_, Option<f64>>(0),
            )
            .optional()
            .unwrap()
    }

    fn task(&self, id: &str) -> Option<TaskRow> {
        task_repo::get_task(self.db.connection(), id).unwrap()
    }

    /// 最近一条 `task_change` 的 `(before_json, after_json)`，原样返回。
    fn raw_last_change(&self) -> (String, String) {
        self.db
            .connection()
            .query_row(
                "SELECT before_json, after_json FROM task_change ORDER BY rowid DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    /// 建一个标签（走服务入口，与生产路径同一形状），返回落库的那一行。
    fn create_tag(&mut self, kind: TagKind, name: &str, at: i64) -> TagRow {
        let env = create_env(&self.epoch);
        expect_changed(
            catalog::create_tag(&mut self.db, env, kind.as_str(), name, None, at).unwrap(),
        )
        .tag
    }

    fn tx(&mut self) -> rusqlite::Transaction<'_> {
        self.db.connection_mut().unchecked_transaction().unwrap()
    }
}

/// 被拒 / 幂等命令的「零变化」基线（总纲 §5 第 8 条）。
///
/// 不派生 `Eq`：`weight` 是浮点，`Option<f64>` 没有可用的全等定义。
#[derive(Debug, Clone, PartialEq)]
struct Baseline {
    revision: i64,
    tags: Vec<TagRow>,
    links: Vec<(String, String, Option<f64>)>,
    task: Option<TaskRow>,
    task_changes: i64,
}

fn baseline_of(f: &Fixture, task_id: &str) -> Baseline {
    Baseline {
        revision: f.revision(),
        tags: f.all_tags(),
        links: f.links(),
        task: f.task(task_id),
        task_changes: f.count("task_change"),
    }
}

fn baseline(f: &Fixture) -> Baseline {
    baseline_of(f, "t1")
}

/// ① `revision` 不变 ② `tag` 行一个不动 ③ `task_tag` 行数与字段值不变
/// ④ 目标任务字段逐字不变 ⑤ 审计无新增。
fn assert_unchanged(f: &Fixture, task_id: &str, before: &Baseline) {
    assert_eq!(
        f.revision(),
        before.revision,
        "被拒/幂等的命令不得改动 revision"
    );
    assert_eq!(f.all_tags(), before.tags, "标签行不得有任何变化（含行数）");
    assert_eq!(
        f.links(),
        before.links,
        "关联行不得有任何变化（含行数、weight）"
    );
    assert_eq!(
        f.task(task_id),
        before.task,
        "任务行的字段值必须与调用前逐字相等（状态/版本/归属/时间）"
    );
    assert_eq!(
        f.count("task_change"),
        before.task_changes,
        "审计表不得新增"
    );
}

/// 结果必须是「真的写了」。
fn expect_changed<T>(outcome: WriteOutcome<T>) -> T {
    match outcome {
        WriteOutcome::Changed(value) => value,
        WriteOutcome::Unchanged(_) => panic!("这次调用本该写库，却返回了「没有变化」"),
    }
}

/// 结果必须是「没有变化」——幂等重复的返回值得能表达它（R-T3-b）。
fn expect_unchanged<T>(outcome: WriteOutcome<T>) -> T {
    match outcome {
        WriteOutcome::Unchanged(value) => value,
        WriteOutcome::Changed(_) => panic!("幂等重复不得返回「写了」"),
    }
}

/// 领域拒绝：`code()` 对，且 `detail()` 是**具体的中文理由**（含全部期望片段）。
fn assert_domain_error(err: AppError, expected_detail_has: &[&str]) {
    assert_eq!(err.code(), "DOMAIN_ERROR", "应当是领域拒绝");
    let detail = err.detail().unwrap_or_default().to_string();
    assert!(
        detail
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "领域拒绝的 detail 必须是中文（空串、英文、SQL 片段都不许）：{detail:?}"
    );
    for needle in expected_detail_has {
        assert!(
            detail.contains(needle),
            "detail 应当说清原因：期望含 {needle:?}，实际 {detail:?}"
        );
    }
}

/// 最近一条审计的 before / after（解析成 JSON 值，数组顺序是断言的一部分）。
fn last_change(f: &Fixture) -> (serde_json::Value, serde_json::Value) {
    let (before, after) = f.raw_last_change();
    (
        serde_json::from_str(&before).unwrap(),
        serde_json::from_str(&after).unwrap(),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 建标签（四类、重名、层级、空值）
// ─────────────────────────────────────────────────────────────────────────────

/// F-005：四类标签都能创建，读回来的 kind / 名字 / 版本 / 时间都对；
/// `parent_id` 恒为 NULL，`row_version` 从 0 起（未来重命名要用它做版本校验）。
#[test]
fn all_four_tag_kinds_are_created_and_read_back() {
    let mut f = bootstrap();
    let mut created = Vec::new();

    for (i, kind) in TagKind::ALL.into_iter().enumerate() {
        let at = 2_000 + i as i64;
        let name = format!("标签{i}");
        let tag = f.create_tag(kind, &name, at);

        assert_eq!(tag.kind, kind, "kind 必须原样落库");
        assert_eq!(tag.name, name);
        assert_eq!(tag.row_version, 0, "新建标签的版本从 0 起");
        assert_eq!(tag.parent_id, None, "V0.1 不做层级：parent_id 恒为 NULL");
        assert_eq!(tag.created_at, at);
        assert_eq!(
            f.tag(&tag.id),
            Some(tag.clone()),
            "刚建好的行读回来逐字相等"
        );
        created.push(tag);
    }

    assert_eq!(f.revision(), 4, "四次成功的新建恰好加 4 次 revision");
    assert_eq!(f.all_tags(), created, "列表按 created_at, id 稳定排序");
}

/// 读入口的两种作用域：按 kind 过滤只列那一类；`tags_of_task` 只列那个任务身上的。
#[test]
fn tag_reads_are_scoped_by_kind_and_by_task() {
    let mut f = bootstrap();
    let domain = f.create_tag(TagKind::Domain, "写作", 2_000);
    let context = f.create_tag(TagKind::Context, "家里", 2_001);
    let report = f.create_tag(TagKind::Report, "周报", 2_002);

    let only_context = catalog::list_tags(&f.db, Some(TagKind::Context)).unwrap();
    assert_eq!(
        only_context,
        vec![context.clone()],
        "按 kind 过滤只列这一类"
    );

    expect_changed(
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t2", &report.id, 3_000).unwrap(),
    );

    assert_eq!(
        f.tags_of("t1"),
        Vec::<TagRow>::new(),
        "t1 身上没有标签（关联不会串到别的任务）"
    );
    assert_eq!(f.tags_of("t2"), vec![report.clone()]);
    assert_eq!(
        f.all_tags(),
        vec![domain, context, report],
        "全量列表不受关联影响"
    );
}

/// 名字的规范化只有一处（`services::catalog::normalize_tag_name`）：先 trim 再落库，
/// 预检与存储层的唯一索引因此给出同一个答案。
#[test]
fn tag_names_are_trimmed_before_they_are_stored_and_compared() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "  写作  ", 2_000);
    assert_eq!(tag.name, "写作", "落库的是 trim 之后的名字");

    let before = baseline(&f);
    let err = catalog::create_tag(
        &mut f.db,
        create_env(&f.epoch),
        "Domain",
        "写作",
        None,
        2_100,
    )
    .unwrap_err();

    assert_domain_error(err, &["领域", "写作"]);
    assert_unchanged(&f, "t1", &before);
}

/// R-T3-f：同 kind 内重名拒绝，错误里能看出**是哪个 kind 的哪个名字**被拒，
/// 而不是让 SQLite 的 UNIQUE 报错冒泡成 `STORAGE_ERROR`；跨 kind 可以同名。
#[test]
fn duplicate_names_are_refused_within_a_kind_but_allowed_across_kinds() {
    let mut f = bootstrap();
    let first = f.create_tag(TagKind::Domain, "写作", 2_000);

    let before = baseline(&f);
    let err = catalog::create_tag(
        &mut f.db,
        create_env(&f.epoch),
        "Domain",
        "写作",
        None,
        2_100,
    )
    .unwrap_err();
    // 重名是领域拒绝（`assert_domain_error` 已断 `code()`），不是 SQLite 的 UNIQUE
    // 冒泡出来的 `STORAGE_ERROR`；文案里给的是中文类别名，不是 `Domain` 这种内部取值。
    assert_domain_error(err, &["领域", "写作"]);
    assert_unchanged(&f, "t1", &before);

    // 跨 kind 同名是另一个标签（唯一索引是 (kind, name)）。
    let other = f.create_tag(TagKind::Activity, "写作", 2_200);
    assert_ne!(other.id, first.id);
    assert_eq!(f.all_tags(), vec![first, other], "两个同名标签各自成行");
}

/// 名字的比较是**大小写敏感**的（`domain::tag::normalize_name` 的口径、
/// `uq_tag_root` 的 BINARY 排序规则、02 §2 的规定）。
#[test]
fn tag_names_are_case_sensitive() {
    let mut f = bootstrap();
    let upper = f.create_tag(TagKind::Domain, "Abc", 2_000);
    let lower = f.create_tag(TagKind::Domain, "abc", 2_001);

    assert_ne!(upper.id, lower.id);
    assert_eq!(f.all_tags(), vec![upper, lower]);
}

/// 空名与不在取值域里的 kind 都拒绝，且不留任何痕迹。
#[test]
fn blank_names_and_unknown_kinds_are_refused_at_the_entry() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let blank = catalog::create_tag(
        &mut f.db,
        create_env(&f.epoch),
        "Domain",
        "   ",
        None,
        2_000,
    )
    .unwrap_err();
    assert_domain_error(blank, &["标签名"]);

    let unknown_kind = catalog::create_tag(
        &mut f.db,
        create_env(&f.epoch),
        "Knowledge",
        "写作",
        None,
        2_000,
    )
    .unwrap_err();
    assert_domain_error(unknown_kind, &["标签类型"]);

    let blank_kind =
        catalog::create_tag(&mut f.db, create_env(&f.epoch), "", "写作", None, 2_000).unwrap_err();
    assert_domain_error(blank_kind, &["标签类型"]);

    assert_unchanged(&f, "t1", &before);
    assert_eq!(f.count("tag"), 0, "被拒的新建不得留下标签行");
}

/// R-T3-e：V0.1 没有标签层级。非空 `parent_id` 一律拒绝（理由说「标签层级」），
/// 而空白串按「未提供」处理——前端把「不选」序列化成空串是常见形状。
#[test]
fn hierarchy_requests_are_refused_in_this_version() {
    let mut f = bootstrap();
    let parent = f.create_tag(TagKind::Domain, "父标签", 2_000);

    let before = baseline(&f);
    let err = catalog::create_tag(
        &mut f.db,
        create_env(&f.epoch),
        "Domain",
        "子标签",
        Some(&parent.id),
        2_100,
    )
    .unwrap_err();
    assert_domain_error(err, &["标签层级"]);
    assert_unchanged(&f, "t1", &before);
    assert_eq!(f.count("tag"), 1, "被拒的层级请求不得留下标签行");

    // 空白 parent 等于「没提供」：能建，且 parent_id 仍为 NULL。
    let blank_parent = catalog::create_tag(
        &mut f.db,
        create_env(&f.epoch),
        "Domain",
        "甲",
        Some("   "),
        2_200,
    )
    .unwrap();
    assert_eq!(expect_changed(blank_parent).tag.parent_id, None);
}

// ─────────────────────────────────────────────────────────────────────────────
// 打标 / 去标（集合操作、幂等、审计、零痕迹）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次成功的打标：恰好一条关联、恰好一条审计、恰好加一次 revision；
/// 任务行与标签行**一个字段都不动**（关系增删不伪造实体版本，R-T3-i）。
#[test]
fn tagging_appends_exactly_one_link_one_audit_row_and_one_revision() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);
    let before = baseline(&f);

    let change = expect_changed(
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_000).unwrap(),
    );

    assert_eq!(
        f.revision(),
        before.revision + 1,
        "一次成功的打标恰好加一次 revision"
    );
    assert_eq!(
        change.revision,
        f.revision(),
        "返回的 revision 是提交后的权威值"
    );
    assert_eq!(
        change.tags,
        vec![tag.clone()],
        "返回值带回这个任务现在的标签集合"
    );
    assert_eq!(
        f.links(),
        vec![("t1".to_string(), tag.id.clone(), None)],
        "只多了一条关联，且 weight 恒为 NULL（R-T3-d）"
    );
    assert_eq!(
        f.count("task_change"),
        before.task_changes + 1,
        "一次成功的打标恰好写一条审计"
    );
    let (audit_before, audit_after) = last_change(&f);
    assert_eq!(
        audit_before,
        json!({"tags": []}),
        "审计的 before 是打标前的标签集合"
    );
    assert_eq!(
        audit_after,
        json!({"tags": [tag.id.clone()]}),
        "审计的 after 是打标后的标签集合"
    );
    assert_eq!(
        f.task("t1"),
        before.task,
        "打标不改任务行的任何字段（含 row_version）"
    );
    assert_eq!(
        f.tag(&tag.id),
        Some(tag),
        "打标不改标签行的任何字段（含 row_version）"
    );
}

/// 一个任务可以挂多个标签（F-005），列表稳定；去掉一个不影响其余，也只写一条审计。
#[test]
fn a_task_can_carry_several_tags_and_untagging_removes_only_one() {
    let mut f = bootstrap();
    let domain = f.create_tag(TagKind::Domain, "写作", 2_000);
    let context = f.create_tag(TagKind::Context, "家里", 2_001);
    let report = f.create_tag(TagKind::Report, "周报", 2_002);
    let all = [&domain, &context, &report];

    for (i, tag) in all.iter().enumerate() {
        expect_changed(
            catalog::tag_task(
                &mut f.db,
                create_env(&f.epoch),
                "t1",
                &tag.id,
                3_000 + i as i64,
            )
            .unwrap(),
        );
    }
    assert_eq!(
        f.tags_of("t1"),
        vec![domain.clone(), context.clone(), report.clone()],
        "多标签按 created_at, id 稳定排序"
    );
    assert_eq!(f.revision(), 3 + 3, "建三个 + 打三次 = 6 次业务写");

    let before = baseline(&f);
    let change = expect_changed(
        catalog::untag_task(&mut f.db, create_env(&f.epoch), "t1", &context.id, 4_000).unwrap(),
    );

    assert_eq!(
        change.tags,
        vec![domain.clone(), report.clone()],
        "只去掉被点名的那个"
    );
    assert_eq!(
        change.revision,
        before.revision + 1,
        "一次成功的去标恰好加一次 revision"
    );
    assert_eq!(
        f.links(),
        vec![
            ("t1".to_string(), domain.id.clone(), None),
            ("t1".to_string(), report.id.clone(), None),
        ],
        "其余关联原样保留"
    );
    let (audit_before, audit_after) = last_change(&f);
    assert_eq!(
        audit_before,
        json!({"tags": [domain.id.clone(), context.id.clone(), report.id.clone()]})
    );
    assert_eq!(
        audit_after,
        json!({"tags": [domain.id.clone(), report.id.clone()]})
    );
}

/// R-T3-b：重复打标是幂等重复——`Unchanged`、不加 revision、不写审计、不加任何行版本。
#[test]
fn tagging_the_same_tag_twice_changes_nothing_the_second_time() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);
    let first = expect_changed(
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_000).unwrap(),
    );

    let before = baseline(&f);
    let again = expect_unchanged(
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_500).unwrap(),
    );

    assert_eq!(
        again.revision, first.revision,
        "幂等重复读回的是当前 revision（不是新加的）"
    );
    assert_eq!(again.tags, first.tags, "标签集合没变");
    assert_unchanged(&f, "t1", &before);
}

/// R-T3-b：重复去标同样是幂等重复——`Unchanged`、零痕迹。
#[test]
fn untagging_a_tag_that_is_not_there_changes_nothing() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);

    // 从未打过标：直接去标也是「没有变化」。
    let never = baseline(&f);
    let untouched = expect_unchanged(
        catalog::untag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_000).unwrap(),
    );
    assert_eq!(untouched.revision, never.revision);
    assert_unchanged(&f, "t1", &never);

    expect_changed(
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_100).unwrap(),
    );
    expect_changed(
        catalog::untag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_200).unwrap(),
    );

    let cleared = baseline(&f);
    let again = expect_unchanged(
        catalog::untag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_300).unwrap(),
    );
    assert_eq!(again.revision, cleared.revision);
    assert_eq!(again.tags, Vec::<TagRow>::new(), "集合已经是空的");
    assert_unchanged(&f, "t1", &cleared);
}

/// 未知任务与未知标签都是**领域拒绝**，且两者可区分；都不留半个关联、不加 revision。
#[test]
fn unknown_tasks_and_unknown_tags_are_domain_refusals() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);
    let before = baseline(&f);

    let unknown_tag =
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", "no-such-tag", 3_000).unwrap_err();
    assert_domain_error(unknown_tag, &["找不到这个标签"]);

    let unknown_task = catalog::tag_task(
        &mut f.db,
        create_env(&f.epoch),
        "no-such-task",
        &tag.id,
        3_000,
    )
    .unwrap_err();
    assert_domain_error(unknown_task, &["找不到这个任务"]);

    let unknown_on_untag = catalog::untag_task(
        &mut f.db,
        create_env(&f.epoch),
        "no-such-task",
        &tag.id,
        3_000,
    )
    .unwrap_err();
    assert_domain_error(unknown_on_untag, &["找不到这个任务"]);

    assert_unchanged(&f, "t1", &before);
    assert_eq!(f.links(), Vec::new(), "被拒的关联不得留下任何行");
}

/// 旧 epoch：三类写命令都在事务内被拒，且零痕迹、文案不含库身份。
#[test]
fn a_stale_epoch_is_refused_before_anything_is_written() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);
    let before = baseline(&f);
    // 闭包刻意**不捕获** `f`：下面的实参同时要 `&mut f.db`，闭包借走整个 `f` 就撞了。
    let stale = || WriteEnvelope::for_create("not-the-epoch".to_string());

    let errors = [
        catalog::create_tag(&mut f.db, stale(), "Domain", "另一个", None, 3_000).unwrap_err(),
        catalog::tag_task(&mut f.db, stale(), "t1", &tag.id, 3_000).unwrap_err(),
        catalog::untag_task(&mut f.db, stale(), "t1", &tag.id, 3_000).unwrap_err(),
    ];

    for err in errors {
        assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
        assert!(
            !err.message().contains("not-the-epoch"),
            "库身份不进用户可见文案：{}",
            err.message()
        );
    }
    assert_unchanged(&f, "t1", &before);
}

/// R-T3-d：V0.1 不暴露权重入参，首次关联的 `weight` 恒为 NULL；
/// 而 `task_tag.weight` 的 0..1 约束仍然有效（V0.2 的权重规则要靠它）。
#[test]
fn weights_are_not_written_in_this_version_but_the_column_still_guards_its_range() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);
    expect_changed(
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_000).unwrap(),
    );

    assert_eq!(
        f.link_weight("t1", &tag.id),
        Some(None),
        "首次关联的权重必须是 NULL"
    );

    let tx = f.tx();
    let too_big = tx.execute(
        "INSERT INTO task_tag(task_id, tag_id, weight) VALUES('t2', ?1, 1.5)",
        [&tag.id],
    );
    assert!(too_big.is_err(), "权重 > 1 必须被 schema 拒绝");
    let negative = tx.execute(
        "INSERT INTO task_tag(task_id, tag_id, weight) VALUES('t2', ?1, -0.1)",
        [&tag.id],
    );
    assert!(negative.is_err(), "负权重必须被 schema 拒绝");
    // V0.2 才写入的取值本身是合法的：约束挡住的是越界，不是权重本身。
    tx.execute(
        "INSERT INTO task_tag(task_id, tag_id, weight) VALUES('t2', ?1, 0.5)",
        [&tag.id],
    )
    .unwrap();
    drop(tx);
}

/// 末步骤故障（审计写不进去）⇒ 整个事务回滚：关联行、审计、revision 不留半条痕迹。
#[test]
fn a_failed_audit_write_leaves_no_half_link() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);
    // 关联行先写、审计后写：让审计插入必然失败，正是「半个事务」的经典形状。
    f.db.connection()
        .execute_batch(
            "CREATE TRIGGER p4_fail_tag_audit BEFORE INSERT ON task_change
             BEGIN SELECT RAISE(ABORT, 'audit unavailable'); END;",
        )
        .unwrap();

    let before = baseline(&f);
    let err = catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_000).unwrap_err();

    assert_eq!(err.code(), "STORAGE_ERROR", "基础设施失败走存储错误");
    assert_unchanged(&f, "t1", &before);
    assert_eq!(f.links(), Vec::new(), "不得留下半个关联");
}

/// R-T3-i：关系增删是 **epoch-only** 命令（`task_tag` 没有版本列，打标也不改
/// task/tag 的任何字段），所以只带 epoch 的信封就够；实体行的版本一次都不许动。
#[test]
fn relation_commands_are_epoch_only_and_fake_no_entity_version() {
    let mut f = bootstrap();
    let tag = f.create_tag(TagKind::Domain, "写作", 2_000);
    let task_before = f.task("t1").unwrap();

    let change = expect_changed(
        catalog::tag_task(&mut f.db, create_env(&f.epoch), "t1", &tag.id, 3_000).unwrap(),
    );

    assert_eq!(
        f.task("t1").unwrap().row_version,
        task_before.row_version,
        "标签集合不住在任务行上：不得推进 task.row_version"
    );
    assert_eq!(
        f.tag(&tag.id).unwrap().row_version,
        tag.row_version,
        "打标不改标签自身：不得推进 tag.row_version"
    );
    assert_eq!(
        f.task("t1").unwrap().updated_at,
        task_before.updated_at,
        "打标不是任务行的修改，不得刷新 updated_at"
    );
    assert_eq!(
        change.revision,
        1 + 1,
        "只有 revision 前进（建标签 1 次 + 打标 1 次）"
    );
}
