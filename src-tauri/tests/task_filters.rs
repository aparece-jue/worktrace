//! P4 Task 5：任务筛选查询（轻量 GTD 列表）与捕获 / 理清为待办入口（F-002、F-005）。
//!
//! 断言口径按总纲 §5 第 8 条：
//! - **只读查询**：`revision` 不变、`task` / `task_change` 的行数与字段值都不动，
//!   且返回的 `data_epoch` / `revision` 必须与库里的一致（四项出自同一读事务）；
//! - **被拒的命令**（非法情境标签、越界分页、旧版本、坏 epoch）：`revision` 不变、
//!   任务行逐字不变、审计无新增（`baseline` / `assert_unchanged` 一次断言这三件事）；
//! - **错误**：断言 `code()` 字符串（`DOMAIN_ERROR` / `VERSION_CONFLICT` /
//!   `DATA_EPOCH_MISMATCH`），并用 `assert_domain_error` 顺带钉住 `detail()` 里的
//!   **具体中文理由**。**不在 `message()` 上找中文**：它的模板 `操作不被允许：{detail}`
//!   自带中文，对 `DOMAIN_ERROR` 恒真（Task 1 踩过的坑）。
//!
//! 建库样板照 `tests/transaction_boundary.rs::bootstrap` 与 `tests/projects.rs`。

use serde_json::json;

use worktrace_lib::domain::tag::TagKind;
use worktrace_lib::domain::task::{TaskStatus, TransitionCause};
use worktrace_lib::envelope::WriteEnvelope;
use worktrace_lib::error::AppError;
use worktrace_lib::services::catalog;
use worktrace_lib::storage::db::Db;
use worktrace_lib::storage::meta::{init_meta, require_meta};
use worktrace_lib::storage::migrations::migrate;
use worktrace_lib::storage::tag_repo::{self, TagRow};
use worktrace_lib::storage::task_repo::{self, Page, ProjectFilter, TaskFilter, TaskRow};
use worktrace_lib::storage::WriteOutcome;

// ─────────────────────────────────────────────────────────────────────────────
// 夹具
// ─────────────────────────────────────────────────────────────────────────────

struct Fixture {
    _dir: tempfile::TempDir,
    db: Db,
    /// 建库时定下的库身份，用来构造合法的请求。
    epoch: String,
}

/// 新建类命令的信封（只需 epoch）。
fn create_env(epoch: &str) -> WriteEnvelope {
    WriteEnvelope::for_create(epoch.to_string())
}

/// 更新类命令的信封：带请求方看到的记录版本。
fn update_env(epoch: &str, expected_row_version: i64) -> WriteEnvelope {
    WriteEnvelope::for_update(epoch.to_string(), expected_row_version)
}

/// 临时文件库 + 迁移 + 一个 run + 两个 active 项目 + 八个任务 + 三个标签与关联。
///
/// 任务的 `created_at` **刻意有重复**（1000 与 2000 各三个/四个）：稳定排序与分页的
/// 用例要靠它证明「同一时间戳内按 id 决出先后」。
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
    tx.execute(
        "INSERT INTO project(id,name,description,row_version,status,created_at,updated_at)
         VALUES('p1','项目一',NULL,0,'active',1000,1000)",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO project(id,name,description,row_version,status,created_at,updated_at)
         VALUES('p2','项目二',NULL,0,'active',1000,1000)",
        [],
    )
    .unwrap();
    // t1 / t2 走仓储建：审计行数与后面的断言口径一致（同 bootstrap 的理由）。
    task_repo::create_task(&tx, "t1", "任务一", Some("p1"), 1000).unwrap();
    task_repo::create_task(&tx, "t2", "任务二", None, 1000).unwrap();
    task_repo::transition_task(&tx, "t1", 0, TaskStatus::Ready, TransitionCause::User, 1000)
        .unwrap();
    tx.commit().unwrap();

    let mut f = Fixture {
        _dir: dir,
        db,
        epoch: meta.data_epoch,
    };

    // 其它状态直接插行：走正常写路径到 Waiting/Blocked/Doing 要好几步跃迁，
    // 那是 P3 的地盘；这里只**布置前置状态**（与 `tests/projects.rs` 同一口径）。
    f.insert_task("t3", TaskStatus::Waiting, Some("p1"), 2000);
    f.insert_task("t4", TaskStatus::Blocked, Some("p1"), 2000);
    f.insert_task("t5", TaskStatus::Ready, Some("p2"), 2000);
    f.insert_task("t6", TaskStatus::Ready, None, 2000);
    f.insert_task("t7", TaskStatus::Doing, Some("p1"), 1000);
    f.insert_task("t8", TaskStatus::Clarifying, Some("p2"), 3000);

    // 标签与关联走服务 / 仓储入口（与生产路径同一形状）。
    let ctx_a = f.create_tag(TagKind::Context, "家里", 1000);
    let ctx_b = f.create_tag(TagKind::Context, "电脑前", 1000);
    let dom = f.create_tag(TagKind::Domain, "写作", 1000);
    // t1 与 t3 身上都挂着**两个上下文标签**：按其中一个筛选时它们只能出现一次。
    for (task, tag) in [
        ("t1", &ctx_a),
        ("t1", &ctx_b),
        ("t1", &dom),
        ("t3", &ctx_a),
        ("t3", &ctx_b),
        ("t5", &ctx_a),
        ("t2", &dom),
    ] {
        f.tag(task, &tag.id, 1000);
    }

    f
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

    fn task(&self, id: &str) -> Option<TaskRow> {
        task_repo::get_task(self.db.connection(), id).unwrap()
    }

    /// 全部任务行的快照，**直连 SQL**（`created_at, id` 顺序）。
    ///
    /// 刻意不走 `task_repo::list_tasks_filtered`：那是被测函数，拿它当 oracle 会让
    /// 「已有记录字段值不变」这条断言在它静默返回空集/错集时退化成恒真
    /// （评审 fix round 2 第 2 条）。行数另有一条独立断言（`baseline` 的 `task_rows`）。
    fn task_snapshot(&self) -> Vec<TaskSnapshot> {
        let mut stmt = self
            .db
            .connection()
            .prepare(
                "SELECT id, status, title, project_id, row_version, updated_at
                 FROM task ORDER BY created_at, id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(TaskSnapshot {
                    id: r.get(0)?,
                    status: r.get(1)?,
                    title: r.get(2)?,
                    project_id: r.get(3)?,
                    row_version: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }

    /// 直接插一个任务。**只用于布置前置状态**。
    fn insert_task(&self, id: &str, status: TaskStatus, project_id: Option<&str>, at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO task(id,project_id,title,status,row_version,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,0,?5,?5)",
                rusqlite::params![id, project_id, format!("任务{id}"), status.as_str(), at],
            )
            .unwrap();
    }

    /// 直接插一条正在运行的会话，用来构造「任务正在计时」这一前置状态。
    fn insert_running_session(&self, session_id: &str, task_id: &str, at: i64) {
        self.db
            .connection()
            .execute(
                "INSERT INTO work_session(id,task_id,run_id,mode,state,timer_kind,started_at,row_version)
                 VALUES(?1,?2,'run-1','FOREGROUND','running','stopwatch',?3,0)",
                rusqlite::params![session_id, task_id, at],
            )
            .unwrap();
    }

    /// 按 kind + 名字取夹具里那个标签的 id（用例里写标签名比写 UUID 好读）。
    fn tag_id(&self, kind: TagKind, name: &str) -> String {
        tag_repo::list_tags(self.db.connection(), Some(kind))
            .unwrap()
            .into_iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("夹具里没有 {kind:?} 类的标签 {name:?}"))
            .id
    }

    /// 建标签走服务入口。
    fn create_tag(&mut self, kind: TagKind, name: &str, at: i64) -> TagRow {
        let env = create_env(&self.epoch);
        expect_changed(
            catalog::create_tag(&mut self.db, env, kind.as_str(), name, None, at).unwrap(),
        )
        .tag
    }

    /// 打标走服务入口；新关联必须是「真的写了」。
    fn tag(&mut self, task_id: &str, tag_id: &str, at: i64) {
        let env = create_env(&self.epoch);
        expect_changed(catalog::tag_task(&mut self.db, env, task_id, tag_id, at).unwrap());
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
}

/// 查询请求：筛选 + 分页 + 请求方手上的库身份（服务入口的统一形状）。
fn ask(f: &Fixture, filter: TaskFilter, page: Page) -> Result<catalog::TaskQueryResult, AppError> {
    catalog::list_tasks_filtered(
        &f.db,
        catalog::TaskQuery {
            filter,
            page,
            expected_data_epoch: f.epoch.clone(),
        },
    )
}

/// 不限制任何条件的筛选（等于过去的「整表列表」）。
fn any_filter() -> TaskFilter {
    TaskFilter::default()
}

/// 只按状态筛。
fn by_status(list: &[TaskStatus]) -> TaskFilter {
    TaskFilter {
        statuses: list.to_vec(),
        ..TaskFilter::default()
    }
}

/// 只按上下文标签筛。
fn by_context(tag_id: &str) -> TaskFilter {
    TaskFilter {
        context_tag_id: Some(tag_id.to_string()),
        ..TaskFilter::default()
    }
}

fn page(limit: i64, offset: i64) -> Page {
    Page { limit, offset }
}

/// 断言这一页的任务与顺序（`created_at, id` 稳定排序），以及不受分页影响的 `total`。
fn assert_page(result: &catalog::TaskQueryResult, expected_ids: &[&str], expected_total: i64) {
    let got: Vec<&str> = result.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        got, expected_ids,
        "这一页的任务与顺序（同一 created_at 内按 id 决出先后）"
    );
    assert_eq!(
        result.total, expected_total,
        "total 是满足条件的总数，与分页窗口无关"
    );
}

/// 任务行的**直连 SQL** 快照（不经被测的筛选查询，见 `task_snapshot`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskSnapshot {
    id: String,
    status: String,
    title: String,
    project_id: Option<String>,
    row_version: i64,
    updated_at: i64,
}

/// 被拒 / 只读调用的「零变化」基线：`revision` + 任务表行数 + 任务行快照 + 审计行数。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Baseline {
    revision: i64,
    tasks: Vec<TaskSnapshot>,
    /// 独立的行数断言：即使快照本身出了问题，「行数不变」这条也还站着。
    task_rows: i64,
    task_changes: i64,
}

fn baseline(f: &Fixture) -> Baseline {
    let tasks = f.task_snapshot();
    let task_rows = f.count("task");
    // 快照必须真的读到了行：否则「字段值不变」会退化成「空对空」（评审 fix round 2 第 2 条）。
    assert_eq!(
        tasks.len() as i64,
        task_rows,
        "快照条数必须与 task 表行数一致（快照坏了就当场发现，而不是让断言恒真）"
    );
    assert!(task_rows > 0, "基线快照不能是空的");
    Baseline {
        revision: f.revision(),
        tasks,
        task_rows,
        task_changes: f.count("task_change"),
    }
}

/// ① `revision` 不变 ② 任务表行数不变 ③ 任务行字段值逐字不变 ④ 审计无新增。
fn assert_unchanged(f: &Fixture, before: &Baseline) {
    assert_eq!(
        f.revision(),
        before.revision,
        "只读 / 被拒的调用不得改动 revision"
    );
    assert_eq!(
        f.count("task"),
        before.task_rows,
        "任务表的行数不得变化（新建被拒时不许留半行）"
    );
    assert_eq!(
        f.task_snapshot(),
        before.tasks,
        "任务行不得有任何变化（含状态、版本、归属、标题、时间）"
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

/// 领域拒绝：`code()` 对，且 `detail()` 是**具体的中文理由**（含全部期望片段）。
///
/// `expected_detail_has` 传空表就是只断言「中文、且说得清」——用于 P1 遗留文案的
/// 路径：那类文案本身是坏的（英文列名），**不在这里把它钉死**。
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

// ─────────────────────────────────────────────────────────────────────────────
// 筛选：条件组合
// ─────────────────────────────────────────────────────────────────────────────

/// 不限制任何条件时列**整表**，并按 `created_at, id` 稳定排序
/// （这正是被删掉的 `task_repo::list_tasks` 的语义，裁决 R-T5-d）。
#[test]
fn an_empty_filter_lists_the_whole_table_in_a_stable_order() {
    let f = bootstrap();

    let result = ask(&f, any_filter(), page(100, 0)).unwrap();

    assert_page(
        &result,
        &["t1", "t2", "t7", "t3", "t4", "t5", "t6", "t8"],
        8,
    );
}

/// 状态是**集合**筛选：多个状态取并集，与其它条件取交集。
#[test]
fn a_status_set_is_ored_within_itself_and_intersected_with_the_other_conditions() {
    let f = bootstrap();

    let ready = ask(&f, by_status(&[TaskStatus::Ready]), page(100, 0)).unwrap();
    assert_page(&ready, &["t1", "t5", "t6"], 3);

    let waiting_or_blocked = ask(
        &f,
        by_status(&[TaskStatus::Waiting, TaskStatus::Blocked]),
        page(100, 0),
    )
    .unwrap();
    assert_page(&waiting_or_blocked, &["t3", "t4"], 2);

    // 交集：待办 + 项目一 ⇒ 只剩 t1（t5 在项目二、t6 没有项目）。
    let ready_in_p1 = ask(
        &f,
        TaskFilter {
            statuses: vec![TaskStatus::Ready],
            project: ProjectFilter::Id("p1".into()),
            context_tag_id: None,
        },
        page(100, 0),
    )
    .unwrap();
    assert_page(&ready_in_p1, &["t1"], 1);
}

/// **状态 + 项目 + 情境三条件同时在场**：交集，以及「项目子句 + `EXISTS` 子句」并存时的
/// 参数绑定顺序（brief 的测试清单点名「项目与情境组合」；顺序错了这条会红）。
///
/// 先用 `tag_repo::get_tag` 确认这个标签确实是 `Context` 类——否则请求会在服务层的
/// 前置校验里被拒，仓储的联合过滤一次都跑不到（评审 fix round 2 第 1 条）。
#[test]
fn a_status_set_combines_with_a_project_and_a_context_tag() {
    let f = bootstrap();
    // 夹具里原本**没有**「待办 + 在 p1 + 一个标签都没有」的候选：t5 在 p2、t6 没有项目、
    // t3 还在等待中——挡掉它们的全是状态与项目子句，把 `EXISTS` 情境子句整段删掉，
    // 这条用例照样绿（终评 I1：计划点名的用例抓不住目标）。就地补上这个候选，
    // 让「情境条件真的参与过滤」变成可观测的事实。
    f.insert_task("t9", TaskStatus::Ready, Some("p1"), 2000);
    let ctx_a = f.tag_id(TagKind::Context, "家里");
    assert_eq!(
        tag_repo::get_tag(f.db.connection(), &ctx_a)
            .unwrap()
            .expect("夹具里必须有这个标签")
            .kind,
        TagKind::Context,
        "这条用例要的是「能过前置校验」的情境标签"
    );

    let result = ask(
        &f,
        TaskFilter {
            statuses: vec![TaskStatus::Ready],
            project: ProjectFilter::Id("p1".into()),
            context_tag_id: Some(ctx_a),
        },
        page(100, 0),
    )
    .unwrap();

    // 三个条件都真的排除掉一个候选：t5 是待办 + 家里但在 p2；t6 是待办但没有项目也没有
    // 标签；t3 挂着「家里」却在等待中；t9 是待办 + 在 p1 但**没有**「家里」标签——
    // 只有它是由 `EXISTS` 子句单独排除的。
    assert_page(&result, &["t1"], 1);

    // 同一个请求**只去掉情境条件** ⇒ t9 冒出来。这一对结果（1 条 vs 2 条）就是
    // 「情境条件真的参与了过滤」的证据：只断言 ["t1"] 时，把整段 `EXISTS` 子句删掉
    // 这条用例仍然是绿的（终评 I1 的原话：期望值与「无情境条件」用例完全相同）。
    let without_context = ask(
        &f,
        TaskFilter {
            statuses: vec![TaskStatus::Ready],
            project: ProjectFilter::Id("p1".into()),
            context_tag_id: None,
        },
        page(100, 0),
    )
    .unwrap();
    assert_page(&without_context, &["t1", "t9"], 2);
}

/// 项目过滤是**三值**（裁决 R-T5-a）：不限制 / 无项目 / 指定项目，三者互不混同。
#[test]
fn the_project_filter_distinguishes_any_none_and_a_specific_project() {
    let f = bootstrap();

    let any = ask(&f, any_filter(), page(100, 0)).unwrap();
    assert_eq!(any.total, 8, "「不限制」不能把「无项目」的任务排除在外");

    let none = ask(
        &f,
        TaskFilter {
            project: ProjectFilter::None,
            ..TaskFilter::default()
        },
        page(100, 0),
    )
    .unwrap();
    assert_page(&none, &["t2", "t6"], 2);

    let p1 = ask(
        &f,
        TaskFilter {
            project: ProjectFilter::Id("p1".into()),
            ..TaskFilter::default()
        },
        page(100, 0),
    )
    .unwrap();
    assert_page(&p1, &["t1", "t7", "t3", "t4"], 4);

    let p2 = ask(
        &f,
        TaskFilter {
            project: ProjectFilter::Id("p2".into()),
            ..TaskFilter::default()
        },
        page(100, 0),
    )
    .unwrap();
    assert_page(&p2, &["t5", "t8"], 2);

    // 未知项目 ID：只是「这个项目里没有任务」，不是非法输入（与情境标签的
    // 拒绝口径不同——那是类别筛选用错了标签）。
    let missing = ask(
        &f,
        TaskFilter {
            project: ProjectFilter::Id("没这个项目".into()),
            ..TaskFilter::default()
        },
        page(100, 0),
    )
    .unwrap();
    assert_page(&missing, &[], 0);
}

/// 按上下文标签筛选：任务挂多个标签时**只出现一次、只计一次数**
/// （`EXISTS`，不是 JOIN 的行数）。
#[test]
fn a_context_tag_filter_returns_each_task_once_even_when_it_carries_several_tags() {
    let f = bootstrap();
    let ctx_a = f.tag_id(TagKind::Context, "家里");
    let ctx_b = f.tag_id(TagKind::Context, "电脑前");

    // t1 与 t3 各有**两个**上下文标签：按其中一个筛，它们依然各只出现一次。
    let by_a = ask(&f, by_context(&ctx_a), page(100, 0)).unwrap();
    assert_page(&by_a, &["t1", "t3", "t5"], 3);
    assert_eq!(
        by_a.tasks.iter().filter(|t| t.id == "t1").count(),
        1,
        "多标签的任务不得重复返回"
    );

    let by_b = ask(&f, by_context(&ctx_b), page(100, 0)).unwrap();
    assert_page(&by_b, &["t1", "t3"], 2);

    // 情境 + 状态取交集。
    let ready_by_a = ask(
        &f,
        TaskFilter {
            statuses: vec![TaskStatus::Ready],
            context_tag_id: Some(ctx_a),
            ..TaskFilter::default()
        },
        page(100, 0),
    )
    .unwrap();
    assert_page(&ready_by_a, &["t1", "t5"], 2);
}

/// 非法情境 ID 一律拒绝：不存在的标签、以及**不是上下文类**的标签
/// （04 F-005「非法情境 ID 拒绝」，裁决 R-T5-c）。
#[test]
fn an_unknown_or_non_context_tag_is_refused_without_touching_the_database() {
    let f = bootstrap();
    let before = baseline(&f);

    // ① 标签根本不存在
    let err = ask(&f, by_context("没有这个标签"), page(100, 0)).unwrap_err();
    assert_domain_error(err, &["找不到这个标签"]);

    // ② 标签存在，但它是「领域」类，不是「上下文」
    let dom = f.tag_id(TagKind::Domain, "写作");
    let err = ask(&f, by_context(&dom), page(100, 0)).unwrap_err();
    {
        // 文案里的类别名必须是**中文译名**（99-glossary §5：Domain=领域、Context=上下文），
        // 不能把 `Domain` 这种内部标识印给用户（Task 3 刚因此返工过）。
        let detail = err.detail().unwrap_or_default();
        assert!(detail.contains("领域"), "类别名要用中文译名：{detail:?}");
        for banned in ["Domain", "Activity", "Context", "Report"] {
            assert!(
                !detail.contains(banned),
                "不得漏出内部标识 {banned:?}：{detail:?}"
            );
        }
    }
    assert_domain_error(err, &["上下文"]);

    // 两次拒绝都不得改动任何事实（查询是只读，但仍按 §5 第 8 条钉住）。
    assert_unchanged(&f, &before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 筛选：分页与只读
// ─────────────────────────────────────────────────────────────────────────────

/// 时间戳相同的任务靠 id 决出先后，分页窗口因此可复现；窗口越过末尾不报错，
/// 只是空页——但 `total` 仍是满足条件的总数。
#[test]
fn pagination_windows_are_stable_when_timestamps_are_equal() {
    let f = bootstrap();

    let first = ask(&f, any_filter(), page(4, 0)).unwrap();
    assert_page(&first, &["t1", "t2", "t7", "t3"], 8);

    let second = ask(&f, any_filter(), page(4, 4)).unwrap();
    assert_page(&second, &["t4", "t5", "t6", "t8"], 8);

    let tail = ask(&f, any_filter(), page(2, 6)).unwrap();
    assert_page(&tail, &["t6", "t8"], 8);

    let past_the_end = ask(&f, any_filter(), page(4, 8)).unwrap();
    assert_page(&past_the_end, &[], 8);

    // 边界值：1 与 100 都合法（等于阈值不算越界）。
    let single = ask(&f, any_filter(), page(1, 0)).unwrap();
    assert_page(&single, &["t1"], 8);
    let full = ask(&f, any_filter(), page(100, 0)).unwrap();
    assert_page(&full, &["t1", "t2", "t7", "t3", "t4", "t5", "t6", "t8"], 8);
}

/// 越界分页**拒绝**而不是钳制（`LIMIT -1` 在 SQLite 里等于「不限制」、
/// 负数 `OFFSET` 等于 0——放过去不会报错，只会悄悄返回错误的一页）。
#[test]
fn out_of_range_pagination_is_refused_instead_of_clamped() {
    let f = bootstrap();
    let before = baseline(&f);

    for bad in [page(0, 0), page(101, 0), page(-1, 0)] {
        let err = ask(&f, any_filter(), bad).unwrap_err();
        assert_domain_error(err, &["每页条数"]);
    }
    let err = ask(&f, any_filter(), page(10, -1)).unwrap_err();
    assert_domain_error(err, &["跳过条数"]);

    assert_unchanged(&f, &before);
}

/// 仓储是**不能信任调用方**的那一层：分页越界在仓储也拒绝
/// （服务层只是转发，规则只留一处）。
#[test]
fn the_repository_itself_refuses_out_of_range_pagination() {
    let f = bootstrap();
    let before = baseline(&f);

    let err =
        task_repo::list_tasks_filtered(f.db.connection(), &any_filter(), page(0, 0)).unwrap_err();
    assert_domain_error(err, &["每页条数"]);

    let err =
        task_repo::list_tasks_filtered(f.db.connection(), &any_filter(), page(10, -1)).unwrap_err();
    assert_domain_error(err, &["跳过条数"]);

    // 合法的窗口在仓储层照常工作。
    let ok =
        task_repo::list_tasks_filtered(f.db.connection(), &any_filter(), page(100, 0)).unwrap();
    assert_eq!(ok.total, 8);
    assert_eq!(ok.tasks.len(), 8);

    assert_unchanged(&f, &before);
}

/// 查不到就是空页 + `total = 0`，不是错误。
#[test]
fn a_query_reports_no_matches_as_an_empty_page_with_a_zero_total() {
    let f = bootstrap();

    let done = ask(&f, by_status(&[TaskStatus::Done]), page(100, 0)).unwrap();
    assert_page(&done, &[], 0);

    let none_and_ready = ask(
        &f,
        TaskFilter {
            statuses: vec![TaskStatus::Ready],
            project: ProjectFilter::None,
            context_tag_id: Some(f.tag_id(TagKind::Domain, "写作")),
        },
        page(100, 0),
    );
    // 这一条里的标签是「领域」类：**先**被拒绝，与筛不出东西是两回事。
    assert_domain_error(none_and_ready.unwrap_err(), &["上下文"]);
}

/// 查询是纯读：`revision` 不变、审计无新增；返回的 `data_epoch` / `revision`
/// 必须就是库里的值（四项出自同一读事务）。
#[test]
fn a_query_is_read_only_and_reports_the_epoch_and_revision_of_one_transaction() {
    let f = bootstrap();
    let before = baseline(&f);

    let result = ask(&f, by_status(&[TaskStatus::Ready]), page(2, 1)).unwrap();

    assert_page(&result, &["t5", "t6"], 3);
    assert_unchanged(&f, &before);

    let meta = require_meta(f.db.connection()).unwrap();
    assert_eq!(result.data_epoch, f.epoch, "返回的库身份就是库里的库身份");
    assert_eq!(
        result.revision, meta.revision,
        "返回的 revision 就是库里的 revision"
    );
    assert_eq!(result.data_epoch, meta.data_epoch);
}

/// 请求带的是旧库身份（数据被恢复/替换）⇒ `DATA_EPOCH_MISMATCH`，零变化，
/// 且用户文案里不得出现 epoch 字面量。
#[test]
fn a_query_with_a_stale_data_epoch_is_refused_with_data_epoch_mismatch() {
    let f = bootstrap();
    let before = baseline(&f);

    let err = catalog::list_tasks_filtered(
        &f.db,
        catalog::TaskQuery {
            filter: any_filter(),
            page: page(100, 0),
            expected_data_epoch: "不是这个库的身份".to_string(),
        },
    )
    .unwrap_err();

    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
    assert!(
        !err.message().contains(&f.epoch) && !err.message().contains("不是这个库的身份"),
        "库身份不得进入用户文案：{}",
        err.message()
    );
    assert_unchanged(&f, &before);
}

// ─────────────────────────────────────────────────────────────────────────────
// 捕获任务（F-002 的 Inbox 入口）
// ─────────────────────────────────────────────────────────────────────────────

/// 捕获：写入一条 `Inbox` 行（标题已去空白），`revision` 恰好 +1，审计恰好一条。
#[test]
fn capturing_a_task_writes_an_inbox_row_with_exactly_one_revision() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let change = expect_changed(
        catalog::create_task(
            &mut f.db,
            create_env(&f.epoch),
            "  捕获的想法  ",
            None,
            5000,
        )
        .unwrap(),
    );

    assert_eq!(
        change.revision,
        before.revision + 1,
        "一次业务写恰好加一次 revision"
    );
    assert_eq!(change.task.title, "捕获的想法", "落库的是去空白后的标题");
    assert_eq!(
        change.task.status,
        TaskStatus::Inbox,
        "捕获进来的任务在收集箱"
    );
    assert_eq!(change.task.project_id, None);
    assert_eq!(change.task.row_version, 0, "新行版本从 0 起");
    assert_eq!(change.task.created_at, 5000);
    assert_eq!(change.task.updated_at, 5000);
    assert_eq!(
        f.count("task_change"),
        before.task_changes + 1,
        "恰好一条审计"
    );
    let stored = f.task(&change.task.id).expect("新任务必须读得回来");
    assert_eq!(stored, change.task);

    let (_, after_json) = f.raw_last_change();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&after_json).unwrap(),
        json!({"status": "Inbox", "title": "捕获的想法"})
    );
}

/// 捕获时可以直接归属到一个 active 项目（F-002 的「可选项目」）。
#[test]
fn capturing_a_task_can_bind_it_to_an_active_project() {
    let mut f = bootstrap();

    let change = expect_changed(
        catalog::create_task(
            &mut f.db,
            create_env(&f.epoch),
            "项目里的想法",
            Some("p1"),
            5000,
        )
        .unwrap(),
    );

    assert_eq!(change.task.project_id.as_deref(), Some("p1"));
    let in_p1 = ask(
        &f,
        TaskFilter {
            project: ProjectFilter::Id("p1".into()),
            ..TaskFilter::default()
        },
        page(100, 0),
    )
    .unwrap();
    assert_eq!(in_p1.total, 5, "新任务出现在项目筛选中");
    assert!(in_p1.tasks.iter().any(|t| t.id == change.task.id));
}

/// 空标题被拒：连事务都不必开——`revision`、任务行、审计都不动，
/// 且用户看到的字段名是中文的「任务标题」。
#[test]
fn capturing_with_a_blank_title_is_refused_without_touching_the_database() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::create_task(&mut f.db, create_env(&f.epoch), "   ", None, 5000).unwrap_err();
    assert_domain_error(err, &["任务标题"]);

    assert_unchanged(&f, &before);
}

/// 归属到一个**不存在**的项目 ⇒ 领域拒绝，理由说的是「找不到这个项目」。
///
/// 与归档那条各占一个用例：两种拒绝的理由必须**可辨别**（前端提示不同），
/// 也把 P1 原先那句「「project」不能为空。」钉死在历史里（Task 5 fix round 1）。
#[test]
fn capturing_into_an_unknown_project_is_refused_with_the_right_reason() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::create_task(
        &mut f.db,
        create_env(&f.epoch),
        "想法",
        Some("没有这个项目"),
        5000,
    )
    .unwrap_err();

    assert_domain_error(err, &["找不到这个项目"]);
    assert_unchanged(&f, &before);
}

/// 归属到一个**已归档**的项目 ⇒ 领域拒绝，理由说的是「项目已归档」（F-004）。
///
/// 原先走 `IntervalOpenInWrongState{state:"project archived"}`，用户会读到一句关于
/// 计时区间的胡话；现在与 `set_task_project` 用同一套词汇（Task 5 fix round 1）。
/// 顺带补上计划 Task 2 第 4 条点名的「归档与创建竞态」：归档发生在事务内被判定，
/// 不只依赖 UI 过滤。
#[test]
fn capturing_into_an_archived_project_is_refused_with_the_right_reason() {
    let mut f = bootstrap();
    f.db.connection()
        .execute(
            "INSERT INTO project(id,name,description,row_version,status,created_at,updated_at)
             VALUES('p9','归档项目',NULL,0,'archived',1000,1000)",
            [],
        )
        .unwrap();
    let before = baseline(&f);

    let err = catalog::create_task(&mut f.db, create_env(&f.epoch), "想法", Some("p9"), 5000)
        .unwrap_err();

    assert_domain_error(err, &["项目已归档"]);
    assert_unchanged(&f, &before);
}

/// 旧库身份 ⇒ 拒绝；零变化。
#[test]
fn capturing_with_a_stale_data_epoch_is_refused() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::create_task(
        &mut f.db,
        create_env("不是这个库的身份"),
        "想法",
        None,
        5000,
    )
    .unwrap_err();

    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");
    assert_unchanged(&f, &before);
}

/// 端到端：捕获 → 理清为待办 → 用「待办 + 项目」筛得到它（F-002 的那条链）。
#[test]
fn a_captured_task_becomes_findable_once_it_is_clarified() {
    let mut f = bootstrap();

    let captured = expect_changed(
        catalog::create_task(
            &mut f.db,
            create_env(&f.epoch),
            "端到端的想法",
            Some("p1"),
            5000,
        )
        .unwrap(),
    )
    .task;

    let ready_in_p1 = |f: &Fixture| {
        ask(
            f,
            TaskFilter {
                statuses: vec![TaskStatus::Ready],
                project: ProjectFilter::Id("p1".into()),
                context_tag_id: None,
            },
            page(100, 0),
        )
        .unwrap()
    };

    // 还没理清：按待办筛不到它。
    let before = ready_in_p1(&f);
    assert_page(&before, &["t1"], 1);

    let clarified = catalog::clarify_ready(
        &mut f.db,
        update_env(&f.epoch, captured.row_version),
        &captured.id,
        6000,
    )
    .unwrap();
    assert_eq!(clarified.task.status, TaskStatus::Ready);

    // 理清之后筛得到它，而且排在 t1 之后（created_at 5000 > 1000）。
    let after = ready_in_p1(&f);
    assert_page(&after, &["t1", captured.id.as_str()], 2);
}

// ─────────────────────────────────────────────────────────────────────────────
// 理清为待办（P1 跃迁原语的复用）
// ─────────────────────────────────────────────────────────────────────────────

/// `Inbox → Ready`：一个事务里改状态 + 写一条审计，`revision` 恰好 +1，
/// `row_version` 前进一格，`quality` 仍为空。
#[test]
fn clarifying_an_inbox_task_moves_it_to_ready_with_one_revision_and_one_audit_row() {
    let mut f = bootstrap();
    let before = baseline(&f);
    let version = f.task("t2").unwrap().row_version;

    let change =
        catalog::clarify_ready(&mut f.db, update_env(&f.epoch, version), "t2", 5000).unwrap();

    assert_eq!(
        change.revision,
        before.revision + 1,
        "一次业务写恰好加一次 revision"
    );
    assert_eq!(change.task.status, TaskStatus::Ready);
    assert_eq!(change.task.row_version, version + 1);
    assert_eq!(change.task.updated_at, 5000);
    assert_eq!(change.task.quality, None);
    assert_eq!(
        f.count("task_change"),
        before.task_changes + 1,
        "恰好一条审计"
    );

    let (before_json, after_json) = f.raw_last_change();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&before_json).unwrap(),
        json!({"status": "Inbox", "quality": null})
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&after_json).unwrap(),
        json!({"status": "Ready", "quality": null})
    );
}

/// `Clarifying → Ready` 同样可以（两个来源状态都在允许集合里）。
#[test]
fn clarifying_a_clarifying_task_moves_it_to_ready() {
    let mut f = bootstrap();
    let version = f.task("t8").unwrap().row_version;

    let change =
        catalog::clarify_ready(&mut f.db, update_env(&f.epoch, version), "t8", 5000).unwrap();

    assert_eq!(change.task.status, TaskStatus::Ready);
    assert_eq!(change.task.row_version, version + 1);
}

/// 其它状态一律拒绝——**包括跃迁表里合法、但不属于这个入口的那些**
/// （`Doing → Ready`、`Review → Ready`、`Blocked/Waiting → Ready` 归 P3 的状态联动，
/// 裁决 R-T5-e）。
///
/// 每个状态配上**它自己的中文名**做 needle：把 `TaskNotClarifiable { status }` 写成
/// 一个写死的状态名，这里就会红（评审 fix round 2 第 3 条）。
/// `Review` / `Done` / `Cancelled` / `Scheduled` 在夹具里没有，**就地布置**（其余用例的
/// 期望值不受影响）：它们都是「已经走远」的一致状态——没有会话、也没有可理清的余地。
/// `Done` 是**可达的**（`Review → Done` 在跃迁表里，P3 的联动会走到它），漏了它这条
/// 用例只少一行、仍然全绿（终评 M5）。
#[test]
fn clarifying_is_refused_for_every_other_status() {
    let mut f = bootstrap();
    f.insert_task("treview", TaskStatus::Review, Some("p1"), 4000);
    f.insert_task("tdone", TaskStatus::Done, Some("p1"), 4000);
    f.insert_task("tcancelled", TaskStatus::Cancelled, None, 4000);
    // `Scheduled` 在 V0.1 没有任何写路径能把它落库（`is_writable_in_v01` 为假，服务层的
    // 跃迁校验一律拒绝），但 schema 的 CHECK **保留**了这个取值：更新版本写入的库会被
    // 旧版本读到，所以这一行真的可能出现，读路径必须照常处理它。夹具只能靠直连 SQL 摆出来
    // （与 `treview` / `tcancelled` 同一口径）。
    f.insert_task("tscheduled", TaskStatus::Scheduled, Some("p1"), 4000);
    let before = baseline(&f);

    for (id, status, zh) in [
        ("t1", TaskStatus::Ready, "待办"),
        ("t7", TaskStatus::Doing, "进行中"),
        ("t3", TaskStatus::Waiting, "等待中"),
        ("t4", TaskStatus::Blocked, "受阻"),
        ("treview", TaskStatus::Review, "复盘"),
        ("tdone", TaskStatus::Done, "已完成"),
        ("tcancelled", TaskStatus::Cancelled, "已取消"),
        ("tscheduled", TaskStatus::Scheduled, "已排期"),
    ] {
        assert_eq!(f.task(id).unwrap().status, status, "夹具前置状态");
        let version = f.task(id).unwrap().row_version;
        let err =
            catalog::clarify_ready(&mut f.db, update_env(&f.epoch, version), id, 5000).unwrap_err();
        let needle = format!("处于「{zh}」");
        assert_domain_error(err, &["理清为待办", needle.as_str()]);
    }

    assert_unchanged(&f, &before);
}

/// 正在计时的任务不能理清（状态与事实对不上的数据也要拦住）。
#[test]
fn clarifying_is_refused_while_a_session_is_running() {
    let mut f = bootstrap();
    f.insert_running_session("s-run", "t2", 1500);
    let before = baseline(&f);
    let version = f.task("t2").unwrap().row_version;

    let err =
        catalog::clarify_ready(&mut f.db, update_env(&f.epoch, version), "t2", 5000).unwrap_err();

    // 那句话中性化为「这个任务正在计时，请先停止计时再继续。」：同一条规则两个入口
    // （改归属 / 理清为待办）共用，句子不能只对其中一个成立——这里做的正是理清，
    // 所以它不得出现「改归属」（Task 5 fix round 1）。
    {
        let detail = err.detail().unwrap_or_default();
        assert!(
            !detail.contains("改归属"),
            "计时文案不得只对「改归属」成立：{detail:?}"
        );
        assert!(
            detail.contains("停止计时"),
            "计时文案得说清下一步是停止计时：{detail:?}"
        );
    }
    assert_domain_error(err, &[]);
    assert_unchanged(&f, &before);
}

/// 未知任务 ⇒ 领域拒绝；零变化。
#[test]
fn clarifying_an_unknown_task_is_refused() {
    let mut f = bootstrap();
    let before = baseline(&f);

    let err = catalog::clarify_ready(&mut f.db, update_env(&f.epoch, 0), "没有这个任务", 5000)
        .unwrap_err();

    assert_domain_error(err, &["找不到这个任务"]);
    assert_unchanged(&f, &before);
}

/// 旧版本 ⇒ `VERSION_CONFLICT`（与「状态不对」分开：前者要刷新，后者是规则拒绝）。
///
/// 用例故意挑 `t1`——它的状态本来也会被拒；返回版本冲突而不是状态拒绝，说明
/// **版本守卫排在状态闸之前**（与 `set_task_project` 同一顺序）。
#[test]
fn clarifying_with_a_stale_version_is_refused_with_version_conflict() {
    let mut f = bootstrap();
    let before = baseline(&f);
    let stale = f.task("t1").unwrap().row_version - 1;

    let err =
        catalog::clarify_ready(&mut f.db, update_env(&f.epoch, stale), "t1", 5000).unwrap_err();

    assert_eq!(err.code(), "VERSION_CONFLICT");
    assert_unchanged(&f, &before);
}

/// 没带版本 / 库身份不对：更新入口与写事务各自的第一道闸。
#[test]
fn clarifying_without_a_version_or_with_a_stale_epoch_is_refused() {
    let mut f = bootstrap();
    let before = baseline(&f);

    // ① `for_create` 信封没有记录版本：更新类命令必须显式拒绝。
    let err = catalog::clarify_ready(&mut f.db, create_env(&f.epoch), "t2", 5000).unwrap_err();
    assert_domain_error(err, &["记录版本"]);

    // ② 旧库身份。
    let version = f.task("t2").unwrap().row_version;
    let err = catalog::clarify_ready(
        &mut f.db,
        update_env("不是这个库的身份", version),
        "t2",
        5000,
    )
    .unwrap_err();
    assert_eq!(err.code(), "DATA_EPOCH_MISMATCH");

    assert_unchanged(&f, &before);
}
