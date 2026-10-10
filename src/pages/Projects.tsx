/**
 * 项目页（P7 Task 5）：F-004 的创建、改名、归档，以及项目详情里的任务列表与
 * 「新增第一条行动」。
 *
 * 这一页只做**订阅、渲染与转发命令**（00 §6）：重名规则、版本守卫、事务边界全在 Rust。
 * 具体到本页的四条：
 *
 * 1. **列表用服务数据**：`list_projects`（`status: null` ⇒ **含归档与 `done` 的历史**，
 *    归档只让项目不再接收新归属、从选择器消失，历史照旧可读）。页面**不直调** `project_repo`
 *    ——命令层与服务层的边界在 P4 就定死了，前端只有 37 条命令。
 * 2. **改既有对象一律带项目版本**：改名与归档都提交 `expected_row_version`
 *    （取自列表里那一行），版本旧了由 Rust 拒绝并给出 `VERSION_CONFLICT`；本页只按 R8
 *    决定行为（冲突刷新 + 展示 Rust 的 `message`），**不维护第二份「码 → 文案」表**。
 * 3. **写完当场重拉**：`run()` 在命令成功之后才 `loadProjects()` / `loadDetail()`——
 *    "确认归档后更新列表"不赌 `domain.changed` 到得比响应早。
 * 4. **旧响应不得覆盖新结果**：与任务页同一套判据——**本视图**水位（`data_epoch` /
 *    `revision`，不比到达顺序，见 `src/components/viewWatermark.ts`）+ 详情列表的
 *    "问题身份"（选中的是哪个项目）。**不推全局水位**：这两条查询都是过滤 / 分页后的
 *    局部视图（评审 I1）。
 *
 * **项目详情的任务列表只列第一页**（服务端上限 100，`task_repo::require_page`）：
 * 04 §「V0.1 轻量 GTD 补充验收」把"稳定分页"要求挂在 F-002 的下一步/等待/阻塞列表上
 * （任务页有分页器），F-004 这一句只要求"项目任务列表"。列表上方那行「共 N 条」用的是
 * `TaskQueryResult.total`（与 `tasks` 出自同一个读事务），**不是**前端自己数行数；
 * 条数超过一页时文案会把这件事说出来，不做静默截断。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { Button, Empty, Flex, Input, Popconfirm, Space, Tag, Typography } from "antd";

import {
  archiveProject,
  createProject,
  createTask,
  listProjects,
  listTasks,
  renameProject,
  toIpcError,
} from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { createViewWatermark } from "../components/viewWatermark";
import { useDataEpoch, useInvalidation } from "../state/hooks";
import type { ProjectRow, ProjectStatus, TaskRow } from "../types/ipc";

/** 项目状态的中文（**展示用**，不是错误码表：它只是 `ProjectStatus` 的界面措辞）。 */
export const PROJECT_STATUS_TEXT: Record<ProjectStatus, string> = {
  active: "在办",
  archived: "已归档",
  done: "已完成",
};

/**
 * 项目详情一次列多少条（服务端上限 100）。**本页不分页**，见模块头第 4 条。
 */
const DETAIL_LIMIT = 100;

/**
 * 改名的入口：`project_repo::rename_project` 会过 `ensure_writable_in_v01`，
 * V0.1 只写 `active`/`archived`，`done` 一律拒绝——不给必败的入口。
 */
function canRename(project: ProjectRow): boolean {
  return project.status !== "done";
}

/**
 * 归档的入口：只给 `active`。
 *
 * `archived` 再归档是幂等空转（`project_repo::archive_project` 的 `Unchanged` 分支），
 * `done` 会被 `ensure_writable_in_v01` 拒绝（V0.1 读得懂但不改它）——两者都不给入口。
 */
function canArchive(project: ProjectRow): boolean {
  return project.status === "active";
}

export function Projects() {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();

  /** 完整项目列表；`null` = 还没拿到（加载态）。 */
  const [projects, setProjects] = useState<ProjectRow[] | null>(null);
  /** 详情区选中的项目 id；`null` = 没打开详情。 */
  const [selected, setSelected] = useState<string | null>(null);
  /** 已经上屏的那一份详情任务列表，连同它是哪个项目的。 */
  const [detail, setDetail] = useState<{
    projectId: string;
    tasks: TaskRow[];
    total: number;
  } | null>(null);

  const [draft, setDraft] = useState("");
  const [action, setAction] = useState("");
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  /**
   * 三个**各自独立**的失败槽（M1）：写命令一条、项目列表一条、详情一条。
   * 共用一个槽位时，任何一条查询成功上屏都会把写命令的失败提示（例如
   * `VERSION_CONFLICT` 那句）擦掉——用户刚要重试就看不到原因了。
   */
  const [commandError, setCommandError] = useState<string | null>(null);
  const [projectsError, setProjectsError] = useState<string | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  /** 要上屏的那条：写命令那条优先（它带着用户刚做的动作），其次列表、最后详情。 */
  const error = commandError ?? projectsError ?? detailError;
  const [busy, setBusy] = useState(false);

  /**
   * 现在打开的是哪个项目。异步续体拿它跟**发起时**的选中项比：不一样就说明这条响应
   * 回答的不是现在这个问题（与任务页的判据①同一条口径，见 `src/pages/Tasks.tsx`）。
   */
  const selectedRef = useRef<string | null>(selected);
  useEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

  /**
   * **本页两条查询各自**的视图水位（评审 I1）：项目列表一份、详情任务列表一份。
   *
   * 与任务页同一条口径（`src/components/viewWatermark.ts` 的模块头写了为什么不推全局水位）：
   * 这两条都是"过滤 / 分页后的局部视图"，推全局水位会吞掉同版本的失效通知、并让 30 秒校验
   * 失去判据。分开两份是因为它们是两个问题：一个的版本新不代表另一个的数据旧。
   */
  const [projectsWatermark] = useState(createViewWatermark);
  const [detailWatermark] = useState(createViewWatermark);

  /** 项目列表（本页的**主查询**）：`status: null` ⇒ 归档与 `done` 的历史都在里面。 */
  const loadProjects = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    try {
      const result = await listProjects({ expected_data_epoch: epoch, status: null });
      // 判据：比**本视图**已上屏的那一份旧（epoch 变了也算）⇒ 丢弃，不覆盖新列表。
      if (projectsWatermark.isStale(result, epoch)) return;
      projectsWatermark.applied(result);
      // M1：成功上屏就把**这条查询**上一次的失败提示清掉（别的槽位不受影响）。
      setProjectsError(null);
      setProjects(result.items);
    } catch (cause) {
      setProjectsError(toIpcError(cause).message);
    }
  }, [epoch, projectsWatermark]);

  /** 项目详情的任务列表：`statuses: []` = 不限制状态（归档项目的历史照样看得见）。 */
  const loadDetail = useCallback(async (): Promise<void> => {
    if (epoch === null || selected === null) return;
    const asked = selected;
    try {
      const result = await listTasks({
        statuses: [],
        project: { id: asked },
        limit: DETAIL_LIMIT,
        offset: 0,
        expected_data_epoch: epoch,
      });
      if (detailWatermark.isStale(result, epoch)) return;
      if (asked !== selectedRef.current) return;
      detailWatermark.applied(result);
      setDetailError(null);
      setDetail({ projectId: asked, tasks: result.tasks, total: result.total });
    } catch (cause) {
      setDetailError(toIpcError(cause).message);
    }
  }, [epoch, selected, detailWatermark]);

  useEffect(() => {
    void loadProjects();
    void loadDetail();
  }, [loadProjects, loadDetail, invalidated]);

  /**
   * 一次写命令：成功之后**当场重拉**（不赌事件到得比响应早），失败走 R8 的统一处置。
   *
   * `reload` 逐条给：改名/归档动的是项目列表，新增行动动的是详情列表——多拉的那一条
   * 是多余的命令。
   */
  async function run(command: () => Promise<unknown>, reload: () => Promise<void>): Promise<void> {
    setBusy(true);
    try {
      await command();
      // **先清提示再重拉**：`load()` 失败时会写回新的提示，顺序反了会把刚写上的那句擦掉。
      setCommandError(null);
      await reload();
    } catch (cause) {
      // 命令路径：VERSION_CONFLICT ⇒ 冲突刷新（refresh 推失效 ⇒ 本页 useEffect 重拉），
      // requires_handshake ⇒ 重新握手；上屏的文案恒为 Rust 的 `message`。
      setCommandError(reportCommandError(cause));
    } finally {
      setBusy(false);
    }
  }

  /** 新建项目（空名在界面层拦住；Rust 对同一输入同样拒绝，P4 的保证）。 */
  async function create(): Promise<void> {
    const name = draft.trim();
    if (name === "") {
      setCommandError("请输入项目名称。");
      return;
    }
    if (epoch === null) return;
    await run(() => createProject({ expected_data_epoch: epoch, name }), async () => {
      setDraft("");
      await loadProjects();
    });
  }

  /** 改名：带**列表里那一行**的版本（旧了由 Rust 判 `VERSION_CONFLICT`）。 */
  async function rename(project: ProjectRow): Promise<void> {
    const name = renameDraft.trim();
    if (name === "") {
      setCommandError("请输入项目名称。");
      return;
    }
    if (epoch === null) return;
    await run(
      () =>
        renameProject({
          expected_data_epoch: epoch,
          project_id: project.id,
          expected_row_version: project.row_version,
          name,
        }),
      async () => {
        setRenaming(null);
        await loadProjects();
      },
    );
  }

  /** 归档：确认之后才发命令；**提交带 epoch 与项目版本**，归档不删任务、不动历史。 */
  async function archive(project: ProjectRow): Promise<void> {
    if (epoch === null) return;
    await run(
      () =>
        archiveProject({
          expected_data_epoch: epoch,
          project_id: project.id,
          expected_row_version: project.row_version,
        }),
      loadProjects,
    );
  }

  /** 项目详情里的「新增第一条行动」：一条 `create_task`，项目已经在请求里定死。 */
  async function addAction(): Promise<void> {
    const title = action.trim();
    if (title === "") {
      setCommandError("请输入任务标题。");
      return;
    }
    if (epoch === null || selected === null) return;
    await run(
      () => createTask({ expected_data_epoch: epoch, title, project_id: selected }),
      async () => {
        setAction("");
        await loadDetail();
      },
    );
  }

  const disabled = epoch === null || busy;
  /**
   * M3（同口径）：读项目列表失败时 `projects` 仍是 `null`，只按它判会**永远停在
   * 「正在读取项目…」**。失败态单独一句话，原因由上面的 `ErrorNotice` 给（Rust 的 message）。
   */
  const projectsFailed = epoch !== null && projects === null && projectsError !== null;
  const current = projects?.find((project) => project.id === selected) ?? null;
  /** 详情里要显示的那一份：必须与当前选中的项目对得上（否则回到加载态）。 */
  const shownDetail = detail !== null && detail.projectId === selected ? detail : null;

  return (
    <Flex vertical gap={16}>
      <Typography.Title level={5} style={{ margin: 0 }}>
        项目
      </Typography.Title>

      <Space.Compact style={{ width: "100%" }}>
        <Input
          placeholder="输入项目名称，回车创建"
          value={draft}
          disabled={disabled}
          onChange={(event) => setDraft(event.target.value)}
          onPressEnter={() => {
            void create();
          }}
        />
        <Button
          type="primary"
          data-testid="create-project"
          disabled={disabled}
          onClick={() => void create()}
        >
          创建项目
        </Button>
      </Space.Compact>

      <ErrorNotice message={error} />

      {epoch === null ? (
        <Typography.Text type="secondary">正在连接…</Typography.Text>
      ) : projectsFailed ? (
        <Typography.Text type="secondary" data-testid="projects-failed">
          读取项目失败：见上面的提示。改条件、或等下一次自动刷新时会重试。
        </Typography.Text>
      ) : projects === null ? (
        <Typography.Text type="secondary" data-testid="projects-loading">
          正在读取项目…
        </Typography.Text>
      ) : projects.length === 0 ? (
        <Empty description="还没有项目：上面输入名称，回车即可创建。" />
      ) : (
        <ul className="project-list" data-testid="project-list">
          {projects.map((project) => (
            <li key={project.id} className="project-item" data-testid={`project-${project.id}`}>
              {renaming === project.id ? (
                <>
                  <Input
                    size="small"
                    className="project-rename-input"
                    data-testid={`rename-input-${project.id}`}
                    value={renameDraft}
                    onChange={(event) => setRenameDraft(event.target.value)}
                    onPressEnter={() => void rename(project)}
                  />
                  <Button
                    size="small"
                    type="primary"
                    data-testid={`rename-ok-${project.id}`}
                    disabled={disabled}
                    onClick={() => void rename(project)}
                  >
                    确定
                  </Button>
                  <Button
                    size="small"
                    data-testid={`rename-cancel-${project.id}`}
                    onClick={() => setRenaming(null)}
                  >
                    取消
                  </Button>
                </>
              ) : (
                <>
                  <Button
                    type="link"
                    className="project-name"
                    data-testid={`open-${project.id}`}
                    onClick={() => setSelected(project.id)}
                  >
                    {project.name}
                  </Button>
                  <Tag className="project-status">{PROJECT_STATUS_TEXT[project.status]}</Tag>
                  {canRename(project) ? (
                    <Button
                      size="small"
                      data-testid={`rename-${project.id}`}
                      disabled={disabled}
                      onClick={() => {
                        setRenaming(project.id);
                        setRenameDraft(project.name);
                      }}
                    >
                      改名
                    </Button>
                  ) : null}
                  {canArchive(project) ? (
                    // 「确认归档后更新列表」：先确认，命令成功之后 loadProjects() 重拉。
                    <Popconfirm
                      title={`归档「${project.name}」？`}
                      okText="确定归档"
                      cancelText="取消"
                      onConfirm={() => void archive(project)}
                    >
                      <Button size="small" danger data-testid={`archive-${project.id}`} disabled={disabled}>
                        归档
                      </Button>
                    </Popconfirm>
                  ) : null}
                </>
              )}
            </li>
          ))}
        </ul>
      )}

      {selected !== null ? (
        <Flex vertical gap={12} className="project-detail" data-testid="project-detail">
          <Typography.Title level={5} style={{ margin: 0 }}>
            项目详情{current !== null ? `：${current.name}` : ""}
          </Typography.Title>

          <Space.Compact style={{ width: "100%" }}>
            <Input
              placeholder="给这个项目新增一条行动，回车创建"
              value={action}
              disabled={disabled}
              onChange={(event) => setAction(event.target.value)}
              onPressEnter={() => {
                void addAction();
              }}
            />
            <Button
              type="primary"
              data-testid="add-action"
              disabled={disabled}
              onClick={() => void addAction()}
            >
              新增行动
            </Button>
          </Space.Compact>

          {shownDetail === null && detailError !== null ? (
            // M3（同口径）：详情读失败时也有自己的一句话，不是永远转圈。
            <Typography.Text type="secondary" data-testid="detail-failed">
              读取项目任务失败：见上面的提示。
            </Typography.Text>
          ) : shownDetail === null ? (
            <Typography.Text type="secondary" data-testid="detail-loading">
              正在读取项目任务…
            </Typography.Text>
          ) : (
            <>
              <Typography.Text type="secondary" data-testid="detail-total">
                共 {shownDetail.total} 条
                {shownDetail.total > shownDetail.tasks.length
                  ? `（本次列出最早的 ${shownDetail.tasks.length} 条）`
                  : ""}
              </Typography.Text>
              {shownDetail.tasks.length === 0 ? (
                <Empty description="这个项目还没有任务：上面输入一条行动即可。" />
              ) : (
                <ul className="task-list" data-testid="detail-task-list">
                  {shownDetail.tasks.map((task) => (
                    <li key={task.id} className="task-item" data-testid={`detail-task-${task.id}`}>
                      <span className="task-title">{task.title}</span>
                      <Tag className="task-status">{task.status}</Tag>
                    </li>
                  ))}
                </ul>
              )}
            </>
          )}
        </Flex>
      ) : null}
    </Flex>
  );
}
