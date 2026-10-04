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
import type { TaskIdentity } from "../components/timerRequests";
import { domainState } from "../state/domainState";
import { useDataEpoch, useInvalidation } from "../state/hooks";
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

export interface InboxProps {
  /**
   * 会话真的开起来之后，把"这条会话属于哪个任务"交给外壳（计时页发 `resume` 要用）。
   *
   * ⚠️ 过渡接口：契约给 `TimerSnapshot` 补上任务身份之后，这个回调与外壳里那份状态
   * 一起删掉（见 `src/components/timerRequests.ts` 的模块头）。
   */
  onSessionStarted(task: TaskIdentity): void;
}

export function Inbox({ onSessionStarted }: InboxProps) {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();

  const [tasks, setTasks] = useState<TaskRow[]>([]);
  const [projects, setProjects] = useState<ProjectRow[]>([]);
  const [draft, setDraft] = useState("");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /**
   * 重拉本页的两条查询。
   *
   * 响应先过闸门（00 §5 规则 3 的读侧）：`isStaleResponse` 为真说明它回答的不是我们现在
   * 问的那个世界（换过库 / 比已应用水位旧），**丢弃，不覆盖已经上屏的新状态**。
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
      if (domainState.isStaleResponse(found, epoch)) return;
      if (domainState.isStaleResponse(selectable, epoch)) return;
      setTasks(found.tasks);
      setProjects(selectable.items);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch]);

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
      const outcome = await startTimer({
        expected_data_epoch: epoch,
        task_id: task.id,
        task_expected_version: task.row_version,
        // V0.1 的前台会话；倒计时要有正预算，而本阶段没有设置预算的入口（V0.2），
        // 所以这里是正计时。展示侧照旧由 DTO 驱动（倒计时会话的 remaining/overtime 也认）。
        mode: "FOREGROUND",
        timer_kind: "stopwatch",
      });
      // 任务行在上面那笔事务里跃迁过（Inbox → Ready → Doing），所以版本取响应里的
      // 提交后版本，而不是列表里那份旧值。
      onSessionStarted({ id: task.id, title: task.title, row_version: outcome.task_version });
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
                  disabled={disabled}
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
