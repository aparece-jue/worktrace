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
 *    ② 它是不是**比已应用水位更旧**（`data_epoch` / `revision`，走
 *    `domainState.isStaleResponse`，用的是本上下文唯一那把水位）。
 *    两条都过了才上屏；**上屏之后**才 `markApplied`（"收到" ≠ "用上"，见 `src/ipc.ts`）。
 * 4. **改条件就重置分页**：状态 / 项目 / 情境任一变化都把页号归 1（`resetPage`），
 *    否则新条件会带着旧 `offset` 去查——那正是"改变条件后列表对不上"的来源。
 *
 * 本页**不自己订阅事件**：状态从 `src/state/hooks.ts` 读，`invalidated` 一变就重拉一次。
 * 拉取失败**只提示、不刷新**：查询路径调 `refresh()` 会自己触发自己（失效 ⇒ 重拉 ⇒ 又失败），
 * 所以这里用 `toIpcError(cause).message`，命令路径才走 `reportCommandError`。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Empty, Flex, Pagination, Segmented, Select, Space, Tag, Typography } from "antd";

import { listSelectableProjects, listTags, listTasks, toIpcError } from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { domainState } from "../state/domainState";
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
  const [error, setError] = useState<string | null>(null);

  /** 现在这个问题：三个筛选条件 + 分页窗口。 */
  const question = [status, projectId, contextId ?? "", page].join("|");
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
   * 它们**不进水位**（不 `markApplied`）：这是辅助数据，让它推水位会把一条正在飞的
   * 主列表响应按"更旧"丢掉，界面就会停在加载态。主列表那一份才代表"屏幕上的数据是第几版"。
   */
  const loadOptions = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    try {
      const [tags, selectable] = await Promise.all([
        listTags({ expected_data_epoch: epoch, kind: "Context" }),
        listSelectableProjects({ expected_data_epoch: epoch }),
      ]);
      if (domainState.isStaleResponse(tags, epoch)) return;
      if (domainState.isStaleResponse(selectable, epoch)) return;
      setContexts(tags.items);
      setProjects(selectable.items);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch]);

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
      // 判据②：世界变没变（比 epoch / revision，不比到达顺序）。
      if (domainState.isStaleResponse(result, epoch)) return;
      // 判据①：这条响应回答的是不是现在这个问题。
      if (asked !== questionRef.current) return;
      // 「收到」≠「用上」：真的上屏之后才推水位，此后更旧的响应会被判据②挡掉。
      domainState.markApplied(result);
      setView({ question: asked, tasks: result.tasks, total: result.total });
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch, status, projectId, contextId, page, question]);

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
  const busy = epoch !== null && shown === null;

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

      {view !== null ? (
        <Space size={8}>
          <Typography.Text type="secondary" data-testid="task-total">
            共 {view.total} 条
          </Typography.Text>
        </Space>
      ) : null}

      {epoch === null ? (
        <Typography.Text type="secondary">正在连接…</Typography.Text>
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
