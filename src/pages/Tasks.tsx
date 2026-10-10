import { domainState } from "../state/domainState";
/**
 * 任务列表页（P7 Task 5）：F-002 的轻量 GTD 列表 + F-005 的情境筛选。
 *
 * 这一页只做**订阅、渲染与转发命令**（00 §6）：合法性与统计口径全在 Rust。
 * 具体到本页的四条：
 *
 * 1. **三个列表不混**：下一步行动 = `Ready`、等待中 = `Waiting`、阻塞 = `Blocked`，
 *    一次只查一个状态（`statuses: [选中的那一个]`）。把 Waiting 与 Blocked 合成一条
 *    查询，等于在界面上把它们当成同一个状态——04 §「V0.1 轻量 GTD 补充验收」把它们
 *    列成两个列表。
 * 2. **筛选与计数都用服务数据**：`tasks` 与 `total` 出自**同一个读事务**
 *    （`services/catalog.rs` 的 `list_tasks_filtered`）。「共 N 条」与分页器都读
 *    `total`，前端**不自己数** `tasks.length`，也不自己拼分页窗口以外的条件
 *    （三个条件放进**同一条**查询，交集在服务端算）。
 * 3. **旧响应不得覆盖新结果**：`load()` 里按顺序问两条判据——
 *    ① 这条响应回答的是不是**现在这个问题**（条件 + 分页窗口的身份，**不是到达顺序**）；
 *    ② 它是不是**比本视图已上屏的那份更旧**（`data_epoch` / `revision`，走
 *    {@link createViewWatermark}：**本页自己**的水位，不是全局那把——理由是
 *    `list_tasks` 只是"过滤 + 分页后的局部视图"，推全局水位会吞掉同版本的失效通知、
 *    并让 30 秒校验失去判据，详见 `src/components/viewWatermark.ts` 的模块头与评审 I1）。
 *    两条都过了才上屏；**上屏之后**才推进本视图水位（"收到" ≠ "用上"）。
 * 4. **改条件就重置分页**：状态 / 项目 / 情境任一变化都把页号归 1（`resetPage`），
 *    否则新条件会带着旧 `offset` 去查——那正是"改变条件后列表对不上"的来源。
 *
 * 本页**不自己订阅事件**：状态从 `src/state/hooks.ts` 读，`invalidated` 一变就重拉一次。
 * 拉取失败**只提示、不刷新**：查询路径调 `refresh()` 会自己触发自己（失效 ⇒ 重拉 ⇒ 又失败），
 * 所以这里用 `toIpcError(cause).message`，命令路径才走 `reportCommandError`。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Empty, Flex, Pagination, Segmented, Select, Space, Tag, Typography } from "antd";

import { listSelectableProjects, listTags, listTasks, transitionTask, toIpcError } from "../ipc";
import { Button } from "antd";
import { ErrorNotice } from "../components/ErrorNotice";
import { createViewWatermark } from "../components/viewWatermark";
import { useDataEpoch, useInvalidation } from "../state/hooks";
import type { ProjectRow, ProjectSelector, TagRow, TaskRow, TaskStatus } from "../types/ipc";

/**
 * 本页的三个列表。**Waiting 与 Blocked 各是各的**（04 §轻量 GTD 补充验收），
 * 所以它们是三个选项、三条查询，不是一个"未完成"选项。
 */
const LISTS: Array<{ status: TaskStatus; label: string }> = [
  { status: "Ready", label: "下一步行动" },
  { status: "Waiting", label: "等待中" },
  { status: "Blocked", label: "阻塞" },
];

/** 一页取多少条（服务端上限是 100，见 `task_repo::require_page`）。 */
const PAGE_SIZE = 20;

/**
 * 项目筛选在下拉里的两个哨兵值。
 *
 * `ProjectSelector` 是**三值**的（`"any"` / `"none"` / `{id}`，D3 裁决），而 antd 的
 * Select 取值是字符串；两个单元态各给一个不可能与项目 id 撞车的哨兵，转换只有
 * {@link selectorOf} 一处。
 */
const ANY_PROJECT = "__any__";
const NO_PROJECT = "__none__";

/** 情境筛选的「不限」哨兵（未选择时状态是 `null`，不是空串）。 */
const ANY_CONTEXT = "__any__";

/** 项目筛选的三值形状 → `ProjectSelector`。 */
export function selectorOf(value: string): ProjectSelector {
  if (value === NO_PROJECT) return "none";
  if (value === ANY_PROJECT) return "any";
  return { id: value };
}

export function Tasks() {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();

  const [status, setStatus] = useState<TaskStatus>("Ready");
  const [projectId, setProjectId] = useState<string>(ANY_PROJECT);
  const [contextId, setContextId] = useState<string | null>(null);
  const [page, setPage] = useState(1);

  const [projects, setProjects] = useState<ProjectRow[]>([]);
  const [contexts, setContexts] = useState<TagRow[]>([]);
  /** 已经上屏的那一份结果，连同**它回答的那个问题**。 */
  const [view, setView] = useState<{
    question: string;
    tasks: TaskRow[];
    total: number;
  } | null>(null);
  /**
   * 两条查询**各自的**失败提示（M1）。共用一个槽位会让一条查询的成功把另一条的失败
   * 擦掉——实测过：`list_tags` 成功上屏会把 `list_tasks` 刚写上的失败提示清掉。
   */
  const [listError, setListError] = useState<string | null>(null);
  const [acting, setActing] = useState(false);
  const [optionsError, setOptionsError] = useState<string | null>(null);
  /** 要上屏的那条：主列表那条优先（它是这一页的主体），其次是筛选选项那条。 */
  const error = listError ?? optionsError;

  /**
   * **本页两条查询各自**的视图水位（评审 I1）：主列表一份、筛选选项一份。
   *
   * 分开是必须的：它们是两个问题、两条读路径，一个的版本比另一个新并不代表另一个的数据旧。
   * `useState(createViewWatermark)[0]` 只借它拿一个**稳定实例**（工厂只跑一次），
   * 它本身不是会变的状态、也不触发重渲染。
   */
  const [listWatermark] = useState(createViewWatermark);
  const [optionsWatermark] = useState(createViewWatermark);

  /**
   * 现在这个问题：三个筛选条件 + 分页窗口（M7：语义是**元组**，用 JSON 而不是 `|` 拼串——
   * 拼串在取值里出现分隔符时会撞车）。
   */
  const question = JSON.stringify([status, projectId, contextId, page]);
  /**
   * 最新那个已提交的问题。`load()` 的异步续体拿它跟**发起时**的问题比：
   * 不一样就说明这条响应已经不是现在要显示的东西了（判据①）。
   *
   * 在 effect 里同步而不是渲染期写 ref：只有**已提交**的那次渲染才算"现在"。
   * 这个 effect 必须排在下面那个拉取 effect **前面**（同一轮 effect 按声明顺序跑）。
   */
  const questionRef = useRef(question);
  useEffect(() => {
    questionRef.current = question;
  }, [question]);

  /**
   * 筛选/分页的任何变化都从第一页重新开始。
   *
   * 不重置的话，新条件会带着旧 `offset` 去查（例如在第 3 页切成「等待中」，
   * 却去要第 41 条起的等待任务）——服务端只会照做，界面就对不上了。
   */
  function resetPage(): void {
    setPage(1);
  }

  /**
   * 筛选用的两个下拉：只含 `active` 的项目（`list_selectable_projects`，服务端保证）
   * 与 `Context` 类标签（`list_tags` 的 `kind` 过滤在服务端）。
   *
   * 它们有**自己那一份**水位（`optionsWatermark`），与主列表互不影响：两条读路径的版本
   * 各自前进，谁都别拿自己的版本去判对方旧。
   */
  const loadOptions = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    try {
      const [tags, selectable] = await Promise.all([
        listTags({ expected_data_epoch: epoch, kind: "Context" }),
        listSelectableProjects({ expected_data_epoch: epoch }),
      ]);
      if (optionsWatermark.isStale(tags, epoch, domainState.getView().dataEpoch)) return;
      if (optionsWatermark.isStale(selectable, epoch, domainState.getView().dataEpoch)) return;
      optionsWatermark.applied(tags);
      optionsWatermark.applied(selectable);
      // M1：成功上屏就把**这条查询**上一次的失败提示清掉（另一条的提示不受影响）。
      setOptionsError(null);
      setContexts(tags.items);
      setProjects(selectable.items);
    } catch (cause) {
      setOptionsError(toIpcError(cause).message);
    }
  }, [epoch, optionsWatermark]);

  /** 主查询：一条 `list_tasks`，三个条件 + 分页窗口全在请求里（交集在服务端算）。 */
  const load = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    const asked = question;
    try {
      const result = await listTasks({
        statuses: [status],
        project: selectorOf(projectId),
        context_tag_id: contextId,
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
        expected_data_epoch: epoch,
      });
      // 判据②：比**本视图**已上屏的那一份旧（epoch 变了也算），不比到达顺序。
      if (listWatermark.isStale(result, epoch, domainState.getView().dataEpoch)) return;
      // 判据①：这条响应回答的是不是现在这个问题。
      if (asked !== questionRef.current) return;
      // M4：页码越界（总数变小，这一页已经不存在了）⇒ 夹回最后一页再查一次。
      // 这一次响应**不上屏**（它回答的"问题"已经不是现在的了），也不推水位。
      // 不夹取的话，越界页会渲染成"这个条件下没有任务"——那是在说谎。
      const lastPage = Math.max(1, Math.ceil(result.total / PAGE_SIZE));
      if (page > lastPage) {
        setPage(lastPage);
        return;
      }
      // 「收到」≠「用上」：真的上屏之后才推进本视图水位，此后更旧的响应会被判据②挡掉。
      listWatermark.applied(result);
      // M1：成功上屏就把**这条查询**上一次的失败提示清掉。
      setListError(null);
      setView({ question: asked, tasks: result.tasks, total: result.total });
    } catch (cause) {
      setListError(toIpcError(cause).message);
    }
  }, [epoch, status, projectId, contextId, page, question, listWatermark]);

  useEffect(() => {
    void loadOptions();
  }, [loadOptions, invalidated]);

  useEffect(() => {
    void load();
  }, [load, invalidated]);

  /**
   * 要显示的那一份：**必须与当前问题对得上**。
   *
   * 条件一变、新响应还没回来时，`view` 里那份旧结果就与 `question` 不符了——
   * 这时少显示（回到加载态），而不是把上一个条件的行挂在新的筛选标签下面。
   */
  const shown = view !== null && view.question === question ? view : null;
  const busy = epoch !== null && shown === null && listError === null;
  /**
   * M3：失败态与在飞态必须分得开——查询失败后 `shown` 仍是 `null`，如果只按它判，
   * 界面会**永远停在「正在查询…」**。判据是**主列表**那条错误（筛选选项的失败不影响
   * 列表自己的状态）。
   */
  const failed = epoch !== null && shown === null && listError !== null;

  return (
    <Flex vertical gap={16}>
      <Typography.Title level={5} style={{ margin: 0 }}>
        任务
      </Typography.Title>

      <Space size={8} wrap>
        <Segmented
          aria-label="列表"
          data-testid="task-lists"
          value={status}
          onChange={(value) => {
            setStatus(value as TaskStatus);
            resetPage();
          }}
          options={LISTS.map(({ status: value, label }) => ({ value, label }))}
        />
        <Select
          aria-label="项目"
          data-testid="project-select"
          style={{ width: 200 }}
          value={projectId}
          onChange={(value: string) => {
            setProjectId(value);
            resetPage();
          }}
          options={[
            { value: ANY_PROJECT, label: "（不限项目）" },
            { value: NO_PROJECT, label: "（无项目）" },
            ...projects.map((project) => ({ value: project.id, label: project.name })),
          ]}
        />
        <Select
          aria-label="情境"
          data-testid="context-select"
          style={{ width: 200 }}
          value={contextId ?? ANY_CONTEXT}
          // 一个 Context 标签都没有时不给选择：这里没有可筛的东西（服务端仍会拒绝非法 id）。
          disabled={contexts.length === 0}
          placeholder="（暂无情境标签）"
          onChange={(value: string) => {
            setContextId(value === ANY_CONTEXT ? null : value);
            resetPage();
          }}
          options={[
            { value: ANY_CONTEXT, label: "（不限情境）" },
            ...contexts.map((tag) => ({ value: tag.id, label: tag.name })),
          ]}
        />
      </Space>

      <ErrorNotice message={error} />

      {/*
        M2：「共 N 条」读的是**当前这一份**（`shown`）的 total。读 `view.total` 会在条件刚变、
        新响应还在飞时把**上一个条件**的计数挂在下面——那句数字与屏幕上的列表对不上。
      */}
      {shown !== null ? (
        <Space size={8}>
          <Typography.Text type="secondary" data-testid="task-total">
            共 {shown.total} 条
          </Typography.Text>
        </Space>
      ) : null}

      {epoch === null ? (
        <Typography.Text type="secondary">正在连接…</Typography.Text>
      ) : failed ? (
        // M3：失败态有自己的一句话（不是永远转圈的"正在查询…"）。文案不给第二份错误内容：
        // 具体原因由上面的 ErrorNotice 展示 Rust 的 message。
        <Typography.Text type="secondary" data-testid="tasks-failed">
          查询失败：见上面的提示。改条件、或等下一次自动刷新时会重试。
        </Typography.Text>
      ) : busy ? (
        <Typography.Text type="secondary" data-testid="tasks-loading">
          正在查询…
        </Typography.Text>
      ) : shown !== null && shown.tasks.length === 0 ? (
        <Empty description="这个条件下没有任务。" />
      ) : shown !== null ? (
        <ul className="task-list" data-testid="task-list">
          {shown.tasks.map((task) => (
            <li key={task.id} className="task-item" data-testid={`task-${task.id}`}>
              <span className="task-title">{task.title}</span>
              <Tag className="task-status">{task.status}</Tag>
              {(task.status === "Blocked" || task.status === "Waiting") && epoch !== null ? <Button disabled={acting} data-testid={`ready-${task.id}`} onClick={() => {
                setActing(true);
                void transitionTask({ expected_data_epoch: epoch, task_id: task.id,
                  expected_row_version: task.row_version, target: "Ready", cause: "user" })
                  .then(() => load()).catch((cause) => setListError(toIpcError(cause).message))
                  .finally(() => setActing(false));
              }}>恢复为可执行</Button> : null}
            </li>
          ))}
        </ul>
      ) : null}

      {shown !== null && shown.total > PAGE_SIZE ? (
        <Pagination
          aria-label="分页"
          size="small"
          // 计数取自服务端的 `total`（与 `tasks` 同一个读事务），不是 `tasks.length`。
          total={shown.total}
          pageSize={PAGE_SIZE}
          current={page}
          showSizeChanger={false}
          onChange={(next) => setPage(next)}
        />
      ) : null}
    </Flex>
  );
}
