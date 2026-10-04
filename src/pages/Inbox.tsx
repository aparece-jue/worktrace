/**
 * 收件箱页（P7 Task 3）：F-001 快速捕获 + F-002 任务理清。
 *
 * 这一页只做**订阅、渲染与转发命令**（00 §6）：状态合法性、事务边界、统计口径全在 Rust。
 * 具体到本页的三条：
 *
 * 1. **捕获**：输入一句话回车 ⇒ 一条 `create_task`（不要求项目/标签/日期；项目是**可选**的
 *    下拉，选项只来自 `list_selectable_projects`——只含 `active` 是**服务端**的保证，
 *    前端不自己过滤）。成功后当场重拉列表，所以"立即可见"不赌事件到得比响应早。
 *    **空白标题在界面层拦住**：判据与 `services::catalog::create_task` 的
 *    `title.trim().is_empty()` 逐字相同，拦住只是省一次往返——Rust 对同一输入同样拒绝
 *    （P4 的保证），那边给的 `message` 才是用户看到的文案（R8）。
 * 2. **理清**：`clarify_ready`（Inbox/Clarifying → Ready）。
 * 3. **开始**：`start_timer` **一条**命令。`Inbox → Ready → Doing` 两步跃迁在 Rust 的
 *    **同一个事务**里完成（`services/timer/coordinator.rs` 的 `start`），前端**不许**
 *    自己拆成 `clarify_ready` + `start_timer`——那会多一次完全不必要的写命令，而且中间
 *    失败会留下"已理清但没开始"的半截状态。
 *
 * **按钮的取舍**（02 §5 的跃迁表）：只展示本阶段允许的三个入口——置为 Ready、开始、
 * 以及列表里的状态标签（捕获 / 理清 Ready / 开始计时这三处）。完成/取消/Blocked/Waiting/
 * reopen 属 P8（P3 的 `transition_task` 接入后）。前端的取舍**只是体验**：即使漏了，
 * Rust 也会按同一张表拒绝，那条 `message` 照 R8 上屏。
 * 另有一条与状态表无关的禁用：**正在计时的那条任务**上不给「开始」（再点必败，见
 * `runningTaskId`）。
 *
 * **判旧用本视图水位，不用全局那把**（fix round 2，整分支评审点名）：收件箱列表是
 * **过滤 + 分页后的局部视图**（`statuses: [...]`、`limit`/`offset`），而全局水位的前提是
 * 「已应用的是**权威快照**（全量、含所有状态）」——拿局部视图去比全局水位，会把一条合法
 * 响应按"比水位旧"静默丢掉（例如刚 `create_task` 完、30 秒校验把水位推到新版之后回来的
 * 那次读），两次重叠的 `load()` 乱序时旧响应还可能覆盖新列表。所以本页有自己的
 * `watermark`（`src/components/viewWatermark.ts`），并且**不推**全局水位。
 *
 * 本页**不自己订阅事件**：状态从 `src/state/hooks.ts` 读，事件只通过 `invalidated`
 * 让这里的 `useEffect` 重拉一次数据。
 */

import { useCallback, useEffect, useState } from "react";
import { Button, Empty, Flex, Input, Select, Space, Tag, Typography } from "antd";

import {
  clarifyReady,
  createTask,
  listSelectableProjects,
  listTasks,
  startTimer,
  toIpcError,
} from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { createViewWatermark } from "../components/viewWatermark";
import { useDataEpoch, useInvalidation, useRunningTaskId } from "../state/hooks";
import type { ProjectRow, TaskRow, TaskStatus } from "../types/ipc";

/**
 * 这一页列出的状态：捕获进来的（Inbox）、待理清的（Clarifying）、可开始的（Ready）、
 * 以及**已经开始过**的（Doing）。
 *
 * 为什么带上 `Doing`：`finish` 不改任务状态（02 §5「Doing 表示任务尚在处理，不等同
 * running session」），P7 又没有"完成/重开"入口（P8）——不带上的话，一条任务只要开始过
 * 就再也看不见、也点不到了。
 */
const LISTED_STATUSES: TaskStatus[] = ["Inbox", "Clarifying", "Ready", "Doing"];

/** 一页取多少条。分页属 Task 5，本页只取第一页。 */
const PAGE_SIZE = 50;

/** 「无项目」在下拉里的哨兵值（antd 的 Select 不适合拿 `null` 当取值）。 */
const NO_PROJECT = "";

/** 02 §5：`Inbox → Ready`、`Clarifying → Ready` 合法，服务端也只接受这两个。 */
function canClarify(status: TaskStatus): boolean {
  return status === "Inbox" || status === "Clarifying";
}

/**
 * 02 §5：`Ready → Doing` 合法；`Inbox`/`Clarifying` 由 `start` 在同一条命令里先理清
 * （02 §5「从 Inbox 直接 start 可在同一业务命令内先理清为 Ready，不强迫经过每个状态」）；
 * `Doing` 上再开一次会话不涉及任何状态跃迁（02 §5 表里没有 `Doing → Doing` 这一行）。
 * 其余状态（Blocked/Waiting/Review/Done/Cancelled/Scheduled）在本阶段没有入口。
 */
function canStart(status: TaskStatus): boolean {
  return status === "Inbox" || status === "Clarifying" || status === "Ready" || status === "Doing";
}

export function Inbox() {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();
  /**
   * 正在计时的那条任务（快照的 `task_id`）：它上面的「开始」**点必败**
   * （Rust 的 `require_no_running_foreground` 与唯一索引 `uq_running_foreground` 都只认
   * `state='running'` 的前台会话），所以直接不给入口。
   *
   * 判据只在 `running` 上生效：**暂停的会话不占用前台槽位**，Rust 允许为同一条任务再开一个
   * 会话，前端就不多拦（多拦会把一个合法动作变成灰色的）。
   */
  const runningTaskId = useRunningTaskId();

  const [tasks, setTasks] = useState<TaskRow[]>([]);
  const [projects, setProjects] = useState<ProjectRow[]>([]);
  const [draft, setDraft] = useState("");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /**
   * **本页这一次读**的视图水位（fix round 2）：收件箱列表是一份视图（两条查询在
   * `Promise.all` 里一起取、一起上屏），所以共用一份水位。
   *
   * `useState(createViewWatermark)` 只借它拿一个**稳定实例**（工厂只跑一次），
   * 它本身不是会变的状态、也不触发重渲染。
   */
  const [watermark] = useState(createViewWatermark);

  /**
   * 重拉本页的两条查询。
   *
   * 响应先过**本视图**水位（判据与任务页/项目页同一套，见
   * `src/components/viewWatermark.ts` 的模块头）：换过库（epoch 不同）或比**这一页**
   * 已上屏的那一份旧 ⇒ **丢弃，不覆盖已经上屏的新状态**。比的是本页自己的水位，不是
   * 全局那把——收件箱是过滤视图，去比全局快照的水位会把合法响应丢掉（见文件头）。
   *
   * 查询失败**只提示、不刷新**：这里调 `refresh()` 会自己触发自己（失效 ⇒ 重拉 ⇒ 又失败）。
   * 所以查询路径用 `toIpcError(cause).message`，命令路径才走 `reportCommandError`。
   */
  const load = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    try {
      const [found, selectable] = await Promise.all([
        listTasks({
          statuses: LISTED_STATUSES,
          project: "any",
          limit: PAGE_SIZE,
          offset: 0,
          expected_data_epoch: epoch,
        }),
        listSelectableProjects({ expected_data_epoch: epoch }),
      ]);
      if (watermark.isStale(found, epoch)) return;
      if (watermark.isStale(selectable, epoch)) return;
      // 「收到」≠「用上」：真的上屏之后才推进本视图水位（只前进，取两条里较新的那一版）。
      watermark.applied(found);
      watermark.applied(selectable);
      setTasks(found.tasks);
      setProjects(selectable.items);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch, watermark]);

  // 事件只作缓存失效：`invalidated` 一变就重拉（挂载时也跑一次）。
  useEffect(() => {
    void load();
  }, [load, invalidated]);

  /** 一次成功的命令之后当场重拉一次：不赌 `domain.changed` 到得比响应早。 */
  async function afterWrite(): Promise<void> {
    // **先清提示再重拉**：`load()` 失败时会写回新的提示，顺序反了会把刚写上的那句擦掉。
    setError(null);
    await load();
  }

  /** F-001：回车捕获。 */
  async function capture(): Promise<void> {
    const title = draft.trim();
    if (title === "") {
      // 界面层的前置提示（判据与 Rust 的 `title.trim().is_empty()` 相同，见文件头）。
      setError("请输入任务标题。");
      return;
    }
    if (epoch === null) return;
    setBusy(true);
    try {
      await createTask({ expected_data_epoch: epoch, title, project_id: projectId });
      setDraft("");
      await afterWrite();
    } catch (cause) {
      setError(reportCommandError(cause));
    } finally {
      setBusy(false);
    }
  }

  /** F-002：直接置 Ready。 */
  async function clarify(task: TaskRow): Promise<void> {
    if (epoch === null || !canClarify(task.status)) return;
    setBusy(true);
    try {
      await clarifyReady({
        expected_data_epoch: epoch,
        task_id: task.id,
        expected_row_version: task.row_version,
      });
      await afterWrite();
    } catch (cause) {
      setError(reportCommandError(cause));
    } finally {
      setBusy(false);
    }
  }

  /** F-002：原子理清并启动——**一条**命令，理清那两步在 Rust 的同一个事务里。 */
  async function start(task: TaskRow): Promise<void> {
    if (epoch === null || !canStart(task.status)) return;
    setBusy(true);
    try {
      // 提交后的任务版本不用在这里记：会话一开起来，快照就会带上任务的 id / 版本 / 标题
      // （`build()` 每次采样重读任务行），计时页与状态栏都从那里取。
      await startTimer({
        expected_data_epoch: epoch,
        task_id: task.id,
        task_expected_version: task.row_version,
        // V0.1 的前台会话；倒计时要有正预算，而本阶段没有设置预算的入口（V0.2），
        // 所以这里是正计时。展示侧照旧由 DTO 驱动（倒计时会话的 remaining/overtime 也认）。
        mode: "FOREGROUND",
        timer_kind: "stopwatch",
      });
      await afterWrite();
    } catch (cause) {
      setError(reportCommandError(cause));
    } finally {
      setBusy(false);
    }
  }

  const disabled = epoch === null || busy;

  return (
    <Flex vertical gap={16}>
      <Typography.Title level={5} style={{ margin: 0 }}>
        收件箱
      </Typography.Title>

      <Space.Compact style={{ width: "100%" }}>
        <Input
          placeholder="输入一句话，回车创建（项目可留空）"
          value={draft}
          disabled={disabled}
          onChange={(event) => setDraft(event.target.value)}
          onPressEnter={() => {
            void capture();
          }}
        />
        <Select
          aria-label="项目（可选）"
          style={{ width: 200 }}
          value={projectId ?? NO_PROJECT}
          disabled={disabled}
          onChange={(value: string) => setProjectId(value === NO_PROJECT ? null : value)}
          options={[
            { value: NO_PROJECT, label: "（无项目）" },
            ...projects.map((project) => ({ value: project.id, label: project.name })),
          ]}
        />
      </Space.Compact>

      <ErrorNotice message={error} />

      {tasks.length === 0 ? (
        <Empty description="还没有任务：上面输入一句话，回车即可捕获。" />
      ) : (
        <ul className="inbox-list" data-testid="inbox-list">
          {tasks.map((task) => (
            <li key={task.id} className="inbox-item" data-testid={`task-${task.id}`}>
              <span className="inbox-title">{task.title}</span>
              <Tag className="inbox-status">{task.status}</Tag>
              {canClarify(task.status) ? (
                <Button
                  size="small"
                  data-testid={`clarify-${task.id}`}
                  disabled={disabled}
                  onClick={() => void clarify(task)}
                >
                  置为 Ready
                </Button>
              ) : null}
              {canStart(task.status) ? (
                <Button
                  size="small"
                  type="primary"
                  data-testid={`start-${task.id}`}
                  // 这条任务已经在计时（快照的 running 会话属于它）⇒ 再点必败，不给入口。
                  disabled={disabled || task.id === runningTaskId}
                  onClick={() => void start(task)}
                >
                  开始
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </Flex>
  );
}
