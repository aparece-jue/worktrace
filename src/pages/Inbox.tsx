import { domainState } from "../state/domainState";
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
 * **按钮的取舍**（02 §5 的跃迁表）：理清用 `clarify_ready`、开始用 `start_timer`，
 * 其余状态跃迁（完成 / 取消 / 阻塞 / 等待 / 重开）一律走 `transition_task` 这**一条**命令
 * ——F-003 的入口由 P8 Task 2d 补上（P7 登记为"无对应入口可验"，见 `p7-acceptance` §6.1
 * 第 2 条）。可用性照抄 `domain/task.rs` 的 `allowed_targets`（下文的 `canFinish` /
 * `canBlock` / `canCancel` / `canReopen` 逐条对应它），不自己发明规则。前端的取舍
 * **只是体验**：即使漏了或多给了，Rust 也会按同一张表拒绝，那条 `message` 照 R8 上屏。
 *
 * **完成/取消会结束正在跑的会话、阻塞/等待会暂停它**——这是同一条命令、同一个事务里的
 * 联动事实（`services/tasks.rs` 的 `transition_task`）。回执（`TaskTransitionReport` 的
 * 两个会话名单）由 {@link transitionNotice} 当场说清"这一次动作碰了哪条会话"；按钮上的
 * `title` 先把影响讲在前面。
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

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { Alert, Button, Empty, Flex, Input, Select, Space, Tag, Typography } from "antd";
import type { InputRef } from "antd";

import {
  clarifyReady,
  createTask,
  listSelectableProjects,
  listTasks,
  startTimer,
  toIpcError,
  transitionTask,
} from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { createViewWatermark } from "../components/viewWatermark";
import { useDataEpoch, useInvalidation, useRunningTaskId } from "../state/hooks";
import type {
  ProjectRow,
  TaskRow,
  TaskStatus,
  TaskTransitionReport,
  TransitionCause,
} from "../types/ipc";

/**
 * 这一页列出的状态：捕获进来的（Inbox）、待理清的（Clarifying）、可开始的（Ready）、
 * 以及**已经开始过**的（Doing）。
 *
 * 为什么带上 `Doing`：`finish`（结束计时）不改任务状态（02 §5「Doing 表示任务尚在处理，
 * 不等同 running session」），而"完成"这类跃迁入口就挂在行上（P8 Task 2d）——不带上的话，
 * 一条任务只要开始过就再也看不见、也点不到了。
 *
 * **`Blocked`/`Waiting` 与两个终结态不在这一页**（`Blocked`/`Waiting` 归 `Tasks` 页的三个列表；
 * 终结态今天出现在**不筛状态**的列表里——`Projects.tsx` 的任务详情与 `History.tsx`
 * 的补录下拉都是 `statuses: []`）。
 * 而"完成 / 取消 / 阻塞 / 等待 / 重开"这五个入口**只挂在收件箱**这一页的行上 ⇒
 * "从阻塞/等待回 Ready"与"重开"在真实查询下**取不到行**（只有"替身不筛条件"的用例能摆出来）。
 * 这是**入口挂在哪一页**与**本页列哪些状态**两件事叠加的结果，不是入口可用性判据的一部分：
 * 判据照 `domain/task.rs` 的 `allowed_targets` 一条不少（fix round 1，评审 Minor-5 订正了
 * 本节原先那两条错误说法：`p7-acceptance` §6.4 第 18 条讲的是**归档/完成的批量操作**，
 * 支撑不了"完成列表属 V0.1 之外"；"终结态任何页面都不列"也不成立）。
 */
const LISTED_STATUSES: TaskStatus[] = ["Inbox", "Clarifying", "Ready", "Doing"];

/** 一页取多少条。分页属 Task 5，本页只取第一页。 */
const PAGE_SIZE = 50;

/** 「无项目」在下拉里的哨兵值（antd 的 Select 不适合拿 `null` 当取值）。 */
const NO_PROJECT = "";

/**
 * 「把光标放进捕获输入框」的**一次性意图**（P8 Task 7）。
 *
 * 托盘的「快速捕获」要求"切到收件箱 + 聚焦捕获输入框"（计划 §6.4-11）。前半个由
 * `src/trayViewRequests.ts` 经 `requestPage` 交给外壳；后半个落在**有输入框的这一页**：
 * 意图记在这里、由收件箱**消费一次**（[`takeCaptureFocus`]），消费即清。
 *
 * 为什么不是 prop：外壳不持有业务状态，而"聚焦"是**一次事件**不是一份状态——用 prop 传
 * 下去还得在聚焦后清掉它（否则每次重渲染都会把光标抢回输入框，正在列表上点按钮的用户会
 * 突然失去焦点）。这里与 `pageRequest` 同一姿势：一次请求、没有队列。
 *
 * ⚠️ **只置一个模块级布尔是不够的**（fix round 1，评审 Critical-1；实测确认）：
 * 收件箱**已经挂着**时（最小化后回来、被别的窗口遮挡、用户本来就在这一页）
 * `requestCaptureFocus()` 不会让 React 重渲染——**模块级变量不是 React 状态**，
 * 改它不等于调度一次更新 ⇒ 聚焦 effect 不重跑 ⇒ ① 点托盘什么都不发生，而意图还留着
 * ⇒ ② 等下一次 `busy` 翻转或任意 `domain.changed` 重拉时，把光标**抢**进捕获框。
 *
 * 所以本模块对外是一个用 [`useSyncExternalStore`] 读的**外部存储**
 * （[`subscribeCaptureFocus`] + [`captureFocusVersion`]）：请求时既置意图、也通知订阅者，
 * 于是**已挂载**的那条路径会重渲染并让"三段式"判据重跑一次；而挂载路径本来就会跑一次
 * effect。两条路走的是**同一段**判据（`hasPending` → 输入框真的可聚焦 → `take`），
 * 语义不会分叉。
 *
 * 本页没挂载时意图留在原地：外壳收到跳转 ⇒ 收件箱挂载 ⇒ 立刻消费（同一拍内）。
 */
let captureFocusPending = false;

/**
 * 第几次聚焦请求（**单调递增**，`useSyncExternalStore` 的快照值）。
 *
 * 它只用来回答"有没有变过"：订阅者拿它当快照比较（变了 ⇒ 重渲染 ⇒ effect 重跑）。
 * 不需要回绕处理：JS 的 `number` 要加到 `2^53` 才丢精度，而这个计数每次点托盘才 +1。
 * 它**不导出**：外部只表达"请聚焦一次"，不需要知道是第几次。
 */
let captureFocusVersion = 0;

/** 订阅者（`useSyncExternalStore` 注册；同一时刻最多一个——只有收件箱会订阅）。 */
const captureFocusSubscribers = new Set<() => void>();

/** `useSyncExternalStore` 的订阅端：返回退订函数（与 `pageRequest` 同形）。 */
function subscribeCaptureFocus(onChange: () => void): () => void {
  captureFocusSubscribers.add(onChange);
  return () => {
    captureFocusSubscribers.delete(onChange);
  };
}

/** `useSyncExternalStore` 的快照端：读那个只增不减的版本号。 */
function captureFocusSnapshot(): number {
  return captureFocusVersion;
}

/** 请求"到了收件箱就把捕获输入框聚焦一次"（`App.tsx` 转发跳转意图时调）。 */
export function requestCaptureFocus(): void {
  captureFocusPending = true;
  captureFocusVersion += 1;
  // 复制一份再遍历：订阅者可能在回调里退订（React 的卸载路径）。
  for (const notify of [...captureFocusSubscribers]) notify();
}

/** 还欠着一次聚焦吗？（**只读**，见 [`takeCaptureFocus`] 的说明。） */
export function hasPendingCaptureFocus(): boolean {
  return captureFocusPending;
}

/**
 * 取走那个一次性意图（取走即清）。**收件箱是唯一消费者**：别的页面读到也不会做任何事。
 *
 * 与 [`hasPendingCaptureFocus`] 分开两步（读→聚焦→再取走）：先取走再找落点的话，
 * 落点还没挂上（`captureRef.current === null`）的那一次就把意图吃掉了——托盘点一下
 * 什么都没发生，而"重试"也无从谈起。
 *
 * 为什么放在这里而不是 `src/state/pageRequest.ts`：那是"切页"的口，不认识任何页面的
 * 内部结构（见它的模块头）；而"捕获输入框在哪"只有本页知道。
 */
export function takeCaptureFocus(): boolean {
  const pending = captureFocusPending;
  captureFocusPending = false;
  return pending;
}

/** 02 §5：`Inbox → Ready`、`Clarifying → Ready` 合法，服务端也只接受这两个。 */
function canClarify(status: TaskStatus): boolean {
  return status === "Inbox" || status === "Clarifying";
}

/**
 * 02 §5：`Ready → Doing` 合法；`Inbox`/`Clarifying` 由 `start` 在同一条命令里先理清
 * （02 §5「从 Inbox 直接 start 可在同一业务命令内先理清为 Ready，不强迫经过每个状态」）；
 * `Doing` 上再开一次会话不涉及任何状态跃迁（02 §5 表里没有 `Doing → Doing` 这一行）。
 * 其余状态（Blocked/Waiting/Review/Done/Cancelled/Scheduled）不给「开始」入口
 * ——它们各有各的出口，见下面的跃迁入口。
 */
function canStart(status: TaskStatus): boolean {
  return status === "Inbox" || status === "Clarifying" || status === "Ready" || status === "Doing";
}

/**
 * 02 §5：`Done` 在跃迁表里只从「正在处理」的状态进入
 * （`allowed_targets` 的 `Ready` / `Scheduled` / `Doing` / `Review`）。
 * `Inbox` / `Clarifying` 到不了 `Done`（要先理清），`Blocked` / `Waiting` 也不行
 * ——那两个状态的出口只有 `Ready` 与 `Cancelled`。
 */
function canFinish(status: TaskStatus): boolean {
  return status === "Ready" || status === "Scheduled" || status === "Doing" || status === "Review";
}

/**
 * 02 §5：`Blocked` / `Waiting` 只从 `Ready` / `Scheduled` / `Doing` 进入。
 * 已经在 `Blocked` 的任务不再给这两个入口（表里 `Blocked | Waiting => [Ready, Cancelled]`），
 * 免得摆一个点了必败的按钮。
 */
function canBlock(status: TaskStatus): boolean {
  return status === "Ready" || status === "Scheduled" || status === "Doing";
}

/** 02 §5：`Cancelled` 是除两个终结态以外**所有**状态都能到的目标（`allowed_targets` 的并集）。 */
function canCancel(status: TaskStatus): boolean {
  return status !== "Done" && status !== "Cancelled";
}

/**
 * 02 §5：终结态（`Done` / `Cancelled`）回到 `Ready` **只能**由显式 reopen 触发
 * （`TaskTransition::new` 的 `ReopenMustBeExplicit`），所以这条入口只在终结态出现，
 * 请求也必须带 `cause: "reopen"`（不是 `"user"`——同一个请求带 `"user"` 会被服务拒绝）。
 */
function canReopen(status: TaskStatus): boolean {
  return status === "Done" || status === "Cancelled";
}

/**
 * 一次跃迁成功后上屏的回执（P8 Task 2d）。
 *
 * 为什么必须有：完成/取消会在**同一个事务**里结束这条任务正在跑的会话、阻塞/等待会暂停它
 * ——这不是"换了个标签"，用户得知道自己的计时被动了、动的是哪一条。三个事实全部取自
 * **响应**（`TaskTransitionReport` 是提交后的事实），前端不自己推断、也不乐观改列表。
 *
 * 会话只有 id 可给（`ended_sessions` / `paused_sessions` 就是 id 列表）：原样列出来，
 * 不缩写、不编名字——编出来的东西没有第二个来源。
 */
function transitionNotice(report: TaskTransitionReport): string {
  const parts = [`已把「${report.task.title}」置为 ${report.task.status}`];
  if (report.ended_sessions.length > 0) {
    parts.push(`结束会话 ${report.ended_sessions.join("、")}`);
  }
  if (report.paused_sessions.length > 0) {
    parts.push(`暂停会话 ${report.paused_sessions.join("、")}`);
  }
  if (report.ended_sessions.length === 0 && report.paused_sessions.length === 0) {
    parts.push("没有会话被结束或暂停");
  }
  return `${parts.join("；")}。`;
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
  /**
   * 上一次跃迁的回执（{@link transitionNotice}）：成功时写上、失败时清掉。
   *
   * 与 `error` 分开：两者互斥（一次动作要么成、要么败），但文案来源不同——失败那句只有
   * Rust 一个来源（R8），成功这句是**响应里的事实**。
   */
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /**
   * 捕获输入框的落点（P8 Task 7）：托盘「快速捕获」要求把光标放在这里。
   *
   * 用 `Input` 的 ref（antd 的 `InputRef.focus()`）而不是查 DOM：这一页里没有别的
   * 输入框、也不该靠 `data-testid` 之类的选择器去找自己的控件。
   */
  const captureRef = useRef<InputRef>(null);

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
      if (watermark.isStale(found, epoch, domainState.getView().dataEpoch)) return;
      if (watermark.isStale(selectable, epoch, domainState.getView().dataEpoch)) return;
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

  /**
   * 每条命令开始时先清掉上一条提示：**成功回执与失败提示都算"上一条"**。
   *
   * 为什么清在**命令开头**，而不是只清在 `afterWrite()`（fix round 1，评审 Minor-1 给的是一行
   * `setNotice(null)`）：那一行只覆盖"下一条命令也成功"的情形——下一条命令**失败**时
   * `afterWrite()` 根本不会被调用，屏幕上会同时挂着上一条成功回执与这一次的失败提示，
   * 而评审点名的正是这一幕。清在开头，四种命令（捕获/理清/开始/跃迁）的成败两条路都覆盖到。
   */
  function clearNotices(): void {
    setError(null);
    setNotice(null);
  }

  /** 一次成功的命令之后当场重拉一次：不赌 `domain.changed` 到得比响应早。 */
  async function afterWrite(): Promise<void> {
    // **先清提示再重拉**：`load()` 失败时会写回新的提示，顺序反了会把刚写上的那句擦掉。
    clearNotices();
    await load();
  }

  /** F-001：回车捕获。 */
  async function capture(): Promise<void> {
    // 上一条回执/提示不再成立（空标题那条早退也算一次新尝试）。
    clearNotices();
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
    // 上一条回执/提示不再成立（见 `clearNotices`）。
    clearNotices();
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
    // 上一条回执/提示不再成立（见 `clearNotices`）。
    clearNotices();
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

  /** F-003：一次状态跃迁（完成 / 取消 / 阻塞 / 等待 / 重开）——**一条** `transition_task`。
   *
   * 与它会话联动（完成/取消结束、阻塞/等待暂停）由 Rust 在**同一个事务**里做，
   * 前端不拆成两条命令、也不自己先改状态。
   *
   * `expected_row_version` 用行上的真实值（`task.row_version`，与 `revision` 无关）：
   * 版本守卫的对象就是这一行任务行。`cause` 只按服务层的用法给：终结态回 `Ready` 是
   * `reopen`（唯一合法原因），其余是 `user`。
   */
  async function transition(
    task: TaskRow,
    target: TaskStatus,
    cause: TransitionCause,
  ): Promise<void> {
    if (epoch === null) return;
    // 上一条回执/提示不再成立（见 `clearNotices`）；这一次的回执在重拉之后写上。
    clearNotices();
    setBusy(true);
    try {
      const report = await transitionTask({
        expected_data_epoch: epoch,
        task_id: task.id,
        expected_row_version: task.row_version,
        target,
        cause,
      });
      // 成功后当场重拉（`afterWrite` 顺带清掉上一次的失败提示与**上一条回执**）：不赌
      // `domain.changed` 到得比响应早，也不乐观改本地列表——列表上永远是服务端给的最后一份事实。
      await afterWrite();
      // 回执在重拉**之后**才写：它说的是这一次动作碰了哪条会话（`afterWrite` 会清提示）。
      setNotice(transitionNotice(report));
    } catch (failure) {
      setNotice(null);
      setError(reportCommandError(failure));
    } finally {
      setBusy(false);
    }
  }

  const disabled = epoch === null || busy;

  /**
   * 托盘的「快速捕获」留下的一次性意图（P8 Task 7）：**消费一次**就不再有。
   *
   * 判据三段（`hasPending` → 输入框真的可聚焦 → `take`）：意图是**一次事件**，不是一份
   * 状态——落不到实处（还没挂载 / 还是禁用态 / 落点还没挂上）时**不能**取走它，否则托盘点
   * 一下就被这一次早跑的 effect 吃掉了；而落到了实处就必须立刻取走，否则每次重渲染都会把
   * 用户的光标抢回输入框。
   *
   * 依赖里的两项各有分工（fix round 1，评审 Critical-1）：
   *
   * - `focusVersion`（[`useSyncExternalStore`] 读到的版本号）：**页面已经挂着**时的那条
   *   路径——点托盘不改变 `disabled`/`busy`，只有这个版本号会变 ⇒ 重渲染 ⇒ 本 effect 重跑
   *   （旧实现只改模块级布尔，**根本不会重渲染**：点托盘什么都不发生，意图还留着，
   *   等下一次重拉把光标抢进来）；
   * - `disabled`：挂载头一拍 `epoch` 还是 `null`，输入框是禁用态，此刻 `focus()` 落空
   *   ⇒ 这次**不消费**，等禁用解除后重跑。
   */
  const focusVersion = useSyncExternalStore(subscribeCaptureFocus, captureFocusSnapshot);

  useEffect(() => {
    if (!hasPendingCaptureFocus()) return;
    const input = captureRef.current;
    // ⚠️ 光看 `disabled` 不够：挂载头一拍 `epoch` 还是 `null`，**输入框本身是禁用态**，
    // 此刻 `focus()` 会被浏览器丢掉（实测：`document.activeElement` 仍是 `body`）。
    // 所以这一拍**不消费**意图——禁用一解除（`disabled` 变化）这个 effect 会重跑。
    if (input === null || input.input?.disabled !== false) return;
    input.focus();
    // 到这一步聚焦是真的发生过了，这一次意图就此用掉（它是一次事件，不是一份状态）。
    takeCaptureFocus();
  }, [focusVersion, disabled]);

  return (
    <Flex vertical gap={16}>
      <Typography.Title level={5} style={{ margin: 0 }}>
        收件箱
      </Typography.Title>

      <Space.Compact style={{ width: "100%" }}>
        <Input
          ref={captureRef}
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

      {/* 跃迁回执：说清这一次动作碰了哪条会话（完成/取消结束、阻塞/等待暂停）。 */}
      {notice === null ? null : (
        <Alert
          className="transition-notice"
          data-testid="transition-notice"
          type="success"
          showIcon
          title={notice}
        />
      )}

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
              {/* F-003（P8 Task 2d）：完成 / 取消 / 阻塞 / 等待中 / 重开。
                  开合照 `domain/task.rs` 的跃迁表；`title` 把会话联动的影响讲在前面。 */}
              {canFinish(task.status) ? (
                <Button
                  size="small"
                  data-testid={`finish-${task.id}`}
                  title="完成会结束这条任务正在跑的会话"
                  disabled={disabled}
                  onClick={() => void transition(task, "Done", "user")}
                >
                  完成
                </Button>
              ) : null}
              {canCancel(task.status) ? (
                <Button
                  size="small"
                  danger
                  data-testid={`cancel-${task.id}`}
                  title="取消会结束这条任务正在跑的会话"
                  disabled={disabled}
                  onClick={() => void transition(task, "Cancelled", "user")}
                >
                  取消
                </Button>
              ) : null}
              {canBlock(task.status) ? (
                <Button
                  size="small"
                  data-testid={`block-${task.id}`}
                  title="置为阻塞会暂停这条任务正在跑的会话"
                  disabled={disabled}
                  onClick={() => void transition(task, "Blocked", "user")}
                >
                  阻塞
                </Button>
              ) : null}
              {canBlock(task.status) ? (
                <Button
                  size="small"
                  data-testid={`wait-${task.id}`}
                  title="置为等待会暂停这条任务正在跑的会话"
                  disabled={disabled}
                  onClick={() => void transition(task, "Waiting", "user")}
                >
                  等待中
                </Button>
              ) : null}
              {canReopen(task.status) ? (
                <Button
                  size="small"
                  data-testid={`reopen-${task.id}`}
                  title="重开把任务放回 Ready（不会恢复旧会话）"
                  disabled={disabled}
                  onClick={() => void transition(task, "Ready", "reopen")}
                >
                  重开
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </Flex>
  );
}
