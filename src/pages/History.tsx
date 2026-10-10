/**
 * 历史页（P8 Task 2c）：F-017 修正与补录。
 *
 * 这一页只做**订阅、渲染与转发命令**（00 §6）：哪些会话进常规历史、哪些区间能改、
 * 重叠怎么判、软删除写什么，全在 Rust（`services/history.rs`）。
 *
 * ## 数据源
 *
 * 只读 `history_view`（命令 13）：**不能**拿恢复页的 `attention_overview` 顶替常规历史
 * ——那个列表回答的是"哪些记录要处理"，只含待确认/损坏/别的 run 未结束的会话，
 * 而且**看不到**已经正常结束的历史。`history_view` 的 `sessions` 只含 `finished` /
 * `discarded`，按 `(started_at, id)` 升序。
 *
 * ## 翻页：没有 `total`，所以只能按"取满一页"判
 *
 * 响应形状由计划钉死为 `{data_epoch, revision, sessions, selected}`——**没有** `total`
 * （Ruling P8-23）。所以"还有下一页"的唯一判据是**取满了 `limit` 条**；界面上给不出
 * "共 N 条 / 共 M 页"，本页也不自己数。
 *
 * 窗口是半开 `[0, 此刻)`：V0.1 的常规历史就是"到此刻为止的全部"。下界给 Unix 毫秒起点
 * 而不是"最近 N 天"，是为了不藏历史、也省掉一个说不清的任意窗口；上界取**每次读的当下**
 * （升序 + 上界只增 ⇒ 已在看的那一页不会因为新会话而错位）。
 *
 * ## 修正与补录
 *
 * - **`correct` 只对 `finished` 开放**：界面据会话 `state` 禁用入口（服务端同样拒），
 *   而且只对**已确认且未作废**的区间给入口（候选端点、已作废的行服务端一律拒绝）；
 * - **起止是用户给的**：输入框用 placeholder 显示现状，**不预填**——分钟精度的截断若是
 *   当默认值提交，就会把一条事实悄悄改成另一个时刻；
 * - **删除误记是软删除**：区间与审计都留着，只是不再计入工时，界面上写明这一点；
 * - **`backfill` 是独立入口**：新建一条已结束的人工会话，不启动计时、也不伪造完成事件；
 *   任务下拉**能翻页**（下拉底部「加载更多」+「已列出 N / 共 M 条」，`total` 取自响应）
 *   ——只取一页会静默截断到**最旧**的 50 条（`task_repo` 的 `ORDER BY created_at, id`
 *   是升序），而契约里没有关键词字段（`TaskFilter` 只有 statuses / project /
 *   context_tag_id）⇒ "列全"只能靠分页，且必须把"没列全"说出来；
 * - **`VERSION_CONFLICT` 不静默重试**：走 `reportCommandError`（冲突刷新 + 上屏 Rust 的
 *   那句"请刷新后重试"），请求里带的是**详情给的真实 `row_version`**——不自己加一，
 *   也不用列表里那一行的版本。
 *
 * ## 判旧与失败
 *
 * 响应先过**本视图水位**（换库或比已上屏的那份旧 ⇒ 丢弃上屏），真的上屏之后才 `applied`；
 * 重拉时机 = 挂载、`useInvalidation()` 计数变化、写命令成功之后那一次。查询失败用
 * `toIpcError(cause).message`（**不** `reportCommandError`：那会自触发重拉），
 * 写命令失败走 `reportCommandError`。补录的任务下拉是**辅助数据**：单独一条查询、
 * 失败静默保留上一次（照今日页候选下拉的口径）。
 */

import { useCallback, useEffect, useState } from "react";
import { Button, Card, Empty, Flex, Input, Popconfirm, Select, Space, Tag, Typography } from "antd";

import { backfill, correct, historyView, listTasks, toIpcError } from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { formatDuration } from "../components/duration";
import { formatLocalMinute, parseLocalMinute } from "../components/localTime";
import { createViewWatermark } from "../components/viewWatermark";
import { useDataEpoch, useInvalidation } from "../state/hooks";
import type {
  CorrectAction,
  HistoryDetail,
  HistoryView,
  IntervalRow,
  SessionRow,
  SessionState,
  TaskRow,
  TimeEdit,
} from "../types/ipc";
import { SESSION_STATE_TEXT } from "./Timer";

/** 每页条数（服务端上限 100）。 */
const PAGE_SIZE = 50;

/** 一条区间的起止文字（终点未知是**事实**，不拿起点凑一个"零长度"出来）。 */
function intervalRangeText(interval: IntervalRow): string {
  const from = formatLocalMinute(interval.started_at);
  const to = interval.ended_at === null ? "终点未知" : formatLocalMinute(interval.ended_at);
  return `${from} → ${to}`;
}

/** 一条区间的时长文字：`duration_ms` 为 `null` 是**没有已确认时长**，与 0 不是一回事。 */
function intervalDurationText(interval: IntervalRow): string {
  if (interval.duration_ms !== null) return formatDuration(interval.duration_ms);
  return interval.ended_at === null ? "无已确认时长（终点未知）" : "无已确认时长（候选终点）";
}

/**
 * 一条区间的可信度标签（三态互斥）：已作废 > 待确认 > 已确认。
 */
function intervalKind(interval: IntervalRow): { text: string; color?: string } {
  if (interval.voided_at !== null) return { text: "已作废" };
  if (interval.needs_review) return { text: "待确认", color: "warning" };
  return { text: "已确认", color: "success" };
}

/**
 * 非 `finished` 的会话为什么不给修正入口：三种原因各说各的（fix round 1 / Minor-3）。
 *
 * `discarded` **必须单独一句**：本页能打开的详情只有 `finished` 与 `discarded` 两种状态
 * （`history_view.sessions` 就只含这两类），对一条已作废的会话说"请先结束这次会话"是一条
 * **永远做不到**的指引。
 */
function correctionHint(state: SessionState): string {
  if (state === "recovering") return "，请先在恢复页对账";
  if (state === "discarded") return "，已作废的会话不能修正起止或删除误记";
  return "，请先结束这次会话";
}

/**
 * 审计行的一句摘要：只取可读的 `change` 键。
 *
 * 两个 JSON 列是**字符串**（按 `change` 分叉的原始载荷），全文属诊断/导出的事，
 * 不上屏；解析不出来就回退到机器可过滤的 `reason`，不假装读懂。
 */
function editSummary(edit: TimeEdit): string {
  try {
    const parsed: unknown = JSON.parse(edit.after_json);
    if (typeof parsed === "object" && parsed !== null) {
      const change = (parsed as { change?: unknown }).change;
      if (typeof change === "string") return change;
    }
  } catch {
    // 落到下面的回退分支。
  }
  return edit.reason ?? "（没有可读的改动摘要）";
}

/**
 * 一条区间 + 它的修正入口。
 *
 * 每个区间一个组件实例（`key` 是区间 id）：详情换了就是重新挂载，输入草稿跟着重置，
 * 不需要在详情变化时手工清 state。
 */
function IntervalEditor({
  session,
  interval,
  disabled,
  onInvalid,
  onRetime,
  onDelete,
}: {
  session: SessionRow;
  interval: IntervalRow;
  disabled: boolean;
  onInvalid(message: string): void;
  onRetime(interval: IntervalRow, range: { started_at: number; ended_at: number }): void;
  onDelete(interval: IntervalRow): void;
}) {
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  /**
   * 可修正 = 会话已结束 **且** 这条区间已确认、未作废。
   * 服务层的前置逐字相同（候选端点、已作废的行一律拒绝）——界面只是体验。
   */
  const correctable =
    session.state === "finished" && interval.voided_at === null && !interval.needs_review;
  const kind = intervalKind(interval);

  return (
    <li className="history-interval" data-testid={`history-interval-${interval.id}`}>
      <Flex vertical gap={4}>
        <Space size="middle" wrap>
          <Typography.Text data-testid={`history-interval-range-${interval.id}`}>
            {intervalRangeText(interval)}
          </Typography.Text>
          <Tag color={kind.color} data-testid={`history-interval-kind-${interval.id}`}>
            {kind.text}
          </Tag>
          <Typography.Text type="secondary">{intervalDurationText(interval)}</Typography.Text>
        </Space>

        {correctable ? (
          <Space size="small" wrap>
            <Input
              aria-label="新的起点"
              data-testid={`history-retime-start-${interval.id}`}
              style={{ width: 180 }}
              placeholder={formatLocalMinute(interval.started_at)}
              value={start}
              disabled={disabled}
              onChange={(event) => setStart(event.target.value)}
            />
            <Input
              aria-label="新的终点"
              data-testid={`history-retime-end-${interval.id}`}
              style={{ width: 180 }}
              placeholder={formatLocalMinute(interval.ended_at ?? interval.started_at)}
              value={end}
              disabled={disabled}
              onChange={(event) => setEnd(event.target.value)}
            />
            <Button
              data-testid={`history-retime-${interval.id}`}
              disabled={disabled}
              onClick={() => {
                const started_at = parseLocalMinute(start);
                const ended_at = parseLocalMinute(end);
                if (started_at === null || ended_at === null) {
                  onInvalid("请填写新的起止时间，格式为 YYYY-MM-DD HH:MM。");
                  return;
                }
                onRetime(interval, { started_at, ended_at });
              }}
            >
              重定时
            </Button>
            <Popconfirm
              title="删除这条误记？"
              description="软删除：区间与审计都保留，只是不再计入工时。"
              okText="确认删除"
              cancelText="取消"
              disabled={disabled}
              onConfirm={() => onDelete(interval)}
            >
              <Button danger data-testid={`history-delete-${interval.id}`} disabled={disabled}>
                删除误记
              </Button>
            </Popconfirm>
          </Space>
        ) : session.state === "finished" ? (
          <Typography.Text type="secondary">
            {interval.voided_at !== null
              ? "已作废的区间不再计入工时，也不能再改。"
              : "待确认的区间归恢复页处理（correct 只接受已确认且未作废的区间）。"}
          </Typography.Text>
        ) : null}
      </Flex>
    </li>
  );
}

export function History() {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();

  const [view, setView] = useState<HistoryView | null>(null);
  /** 当前页的偏移（翻页就是改它，重拉由 `load` 的依赖触发）。 */
  const [offset, setOffset] = useState(0);
  /** 要展开详情的会话 id；`null` = 只看列表。 */
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [tasks, setTasks] = useState<TaskRow[]>([]);
  /** 满足条件的任务**总数**（`list_tasks` 的 `total`）：把"没列全"说清楚，而不是静默截断。 */
  const [taskTotal, setTaskTotal] = useState(0);
  const [taskLoading, setTaskLoading] = useState(false);
  const [backfillTask, setBackfillTask] = useState<string | null>(null);
  const [backfillStart, setBackfillStart] = useState("");
  const [backfillEnd, setBackfillEnd] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /**
   * **本视图这一次读**的水位。`useState(createViewWatermark)` 只借它拿一个稳定实例
   * （工厂只跑一次），它本身不是会变的状态、也不触发重渲染。
   */
  const [watermark] = useState(createViewWatermark);

  /** 读一页历史（带可选详情）。查询失败**只提示、不刷新**（会自触发重拉）。 */
  const load = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    try {
      const found = await historyView({
        expected_data_epoch: epoch,
        from: 0,
        to: Date.now(),
        limit: PAGE_SIZE,
        offset,
        // `session_id` 省略（而不是传 null）= 只要列表；给了就额外返回那条会话的详情。
        ...(selectedId === null ? {} : { session_id: selectedId }),
      });
      // 换过库、或比已经上屏的那一份旧 ⇒ 丢弃，不覆盖新状态。
      if (watermark.isStale(found, epoch)) return;
      // 「收到」≠「用上」：真的上屏之后才推进本视图水位。
      watermark.applied(found);
      setView(found);
      setError(null);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch, watermark, offset, selectedId]);

  // 事件只作缓存失效：`invalidated` 一变就重拉（挂载时也跑一次）。
  useEffect(() => {
    void load();
  }, [load, invalidated]);

  /**
   * 补录下拉的一页候选（**辅助数据**：单独一条查询、失败静默保留上一次，照今日页的口径）。
   *
   * `offset` 传"已经拿到多少条"：`0` 是重取第一页，否则**追加**下一页。
   *
   * 为什么必须有翻页（fix round 1 / Important-2）：`task_repo::list_tasks_filtered` 的排序是
   * `ORDER BY created_at, id` **升序**，只取第一页时被截掉的正是**最近建**的任务——补录最
   * 可能要找的那些；而契约里**没有关键词字段**（`TaskFilter` 只有 statuses / project /
   * context_tag_id），后端搜索这条路今天不存在。所以界面至少要做到两件：**全量可达**
   * （`加载更多`）与**"列了多少条"可见**（`total` 是服务端给的，不是页面自己数的）。
   */
  const loadTasks = useCallback(
    async (offset: number): Promise<void> => {
      if (epoch === null) return;
      setTaskLoading(true);
      try {
        const found = await listTasks({
          // 空集合 = 不限制状态：补录可能针对任何状态的任务（服务端支持这条口径）。
          statuses: [],
          project: "any",
          limit: PAGE_SIZE,
          offset,
          expected_data_epoch: epoch,
        });
        setTaskTotal(found.total);
        setTasks((current) => (offset === 0 ? found.tasks : [...current, ...found.tasks]));
      } catch {
        // 辅助数据：失败静默保留上一次拿到的候选（首次就是空），不报错、不清屏。
      } finally {
        setTaskLoading(false);
      }
    },
    [epoch],
  );

  // 补录的任务下拉：挂载与每次缓存失效时重取**第一页**（用户已经加载的后续页随之作废）。
  useEffect(() => {
    void loadTasks(0);
  }, [loadTasks, invalidated]);

  /** 一次成功的命令之后当场重拉一次：不赌 `domain.changed` 到得比响应早。 */
  async function afterWrite(): Promise<void> {
    // **先清提示再重拉**：`load()` 失败时会写回新的提示，顺序反了会把刚写上的那句擦掉。
    setError(null);
    await load();
  }

  /** 写命令失败的统一收尾（文案与行为都由 `commandError` 决定）。 */
  function fail(cause: unknown): void {
    setError(reportCommandError(cause));
  }

  const detail: HistoryDetail | null = view?.selected ?? null;

  /**
   * 修正一条区间：`retime` 带上用户给的新起止，`delete` 不带那两个键（契约里可选）。
   *
   * `expected_row_version` 取**详情**里那条会话的真实版本——它同时也是服务端
   * `VERSION_CONFLICT` 的判据。
   */
  async function correctInterval(
    interval: IntervalRow,
    action: CorrectAction,
    range?: { started_at: number; ended_at: number },
  ): Promise<void> {
    if (epoch === null || detail === null) return;
    setBusy(true);
    try {
      await correct({
        expected_data_epoch: epoch,
        session_id: detail.session.id,
        expected_row_version: detail.session.row_version,
        interval_id: interval.id,
        action,
        ...(range === undefined ? {} : { started_at: range.started_at, ended_at: range.ended_at }),
      });
      await afterWrite();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  /** 补录：**独立入口**（`backfill` 一条命令，与计时命令无关）。 */
  async function submitBackfill(): Promise<void> {
    if (epoch === null) return;
    if (backfillTask === null) {
      setError("请先选一条要补录的任务。");
      return;
    }
    const started_at = parseLocalMinute(backfillStart);
    const ended_at = parseLocalMinute(backfillEnd);
    if (started_at === null || ended_at === null) {
      setError("请填写补录的起止时间，格式为 YYYY-MM-DD HH:MM。");
      return;
    }
    setBusy(true);
    try {
      await backfill({
        expected_data_epoch: epoch,
        task_id: backfillTask,
        started_at,
        ended_at,
      });
      setBackfillStart("");
      setBackfillEnd("");
      await afterWrite();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  const disabled = epoch === null || busy;
  const sessions = view?.sessions ?? [];
  const canPrev = offset > 0;
  // 没有 `total`（形状钉死）：取满一页就说明**可能**还有下一页。
  const canNext = sessions.length === PAGE_SIZE;

  return (
    <Flex vertical gap={16} data-testid="history-page">
      <Typography.Title level={5} style={{ margin: 0 }}>
        历史
      </Typography.Title>

      <ErrorNotice message={error} />

      <Typography.Text type="secondary" data-testid="history-scope">
        {`范围：到此刻为止的全部历史（半开窗口），按开始时间升序；每页 ${PAGE_SIZE} 条。`}
      </Typography.Text>

      {view === null ? (
        <Typography.Text type="secondary" data-testid="history-loading">
          正在读取历史…
        </Typography.Text>
      ) : (
        <>
          {sessions.length === 0 ? (
            <Empty description="这个窗口里没有已结束或已作废的会话。" data-testid="history-empty" />
          ) : (
            <ul className="history-list" data-testid="history-list">
              {sessions.map((session) => (
                <li key={session.id} className="history-item" data-testid={`history-row-${session.id}`}>
                  <Space size="middle" wrap>
                    <span className="history-task">{session.task_id}</span>
                    <Tag data-testid={`history-state-${session.id}`}>
                      {SESSION_STATE_TEXT[session.state]}
                    </Tag>
                    <Typography.Text type="secondary">
                      {`${formatLocalMinute(session.started_at)} → ${
                        session.ended_at === null ? "未结束" : formatLocalMinute(session.ended_at)
                      }`}
                    </Typography.Text>
                    <Typography.Text type="secondary">{`版本 ${session.row_version}`}</Typography.Text>
                    {session.id === selectedId ? <Tag color="processing">已打开</Tag> : null}
                    <Button
                      size="small"
                      data-testid={`history-open-${session.id}`}
                      disabled={disabled}
                      onClick={() => setSelectedId(session.id)}
                    >
                      打开详情
                    </Button>
                  </Space>
                </li>
              ))}
            </ul>
          )}

          <Space size="middle" wrap data-testid="history-paging">
            <Button
              data-testid="history-prev"
              disabled={disabled || !canPrev}
              onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
            >
              上一页
            </Button>
            <Button
              data-testid="history-next"
              // 取满一页才可能还有下一页（响应里没有 total，见文件头）。
              disabled={disabled || !canNext}
              onClick={() => setOffset(offset + PAGE_SIZE)}
            >
              下一页
            </Button>
            <Typography.Text type="secondary" data-testid="history-page-info">
              {`第 ${offset / PAGE_SIZE + 1} 页 · 本页 ${sessions.length} 条`}
            </Typography.Text>
          </Space>

          {selectedId === null ? null : detail === null ? (
            <Card size="small" data-testid="history-detail-missing">
              <Space size="middle" wrap>
                <Typography.Text type="secondary">
                  {`没有这条会话：${selectedId}（指名一个不存在的 id 不是错误，只是没有详情）。`}
                </Typography.Text>
                <Button
                  size="small"
                  data-testid="history-close-detail"
                  onClick={() => setSelectedId(null)}
                >
                  关闭详情
                </Button>
              </Space>
            </Card>
          ) : (
            <Card
              size="small"
              title="会话详情"
              data-testid="history-detail"
              extra={
                <Button
                  size="small"
                  data-testid="history-close-detail"
                  onClick={() => setSelectedId(null)}
                >
                  关闭详情
                </Button>
              }
            >
              <Flex vertical gap={12}>
                <Space size="middle" wrap data-testid="history-detail-session">
                  <span data-testid="history-detail-session-id">{detail.session.id}</span>
                  <Tag data-testid="history-detail-state">
                    {SESSION_STATE_TEXT[detail.session.state]}
                  </Tag>
                  <Typography.Text type="secondary">{`任务 ${detail.session.task_id}`}</Typography.Text>
                  <Typography.Text type="secondary" data-testid="history-detail-range">
                    {`${formatLocalMinute(detail.session.started_at)} → ${
                      detail.session.ended_at === null
                        ? "未结束"
                        : formatLocalMinute(detail.session.ended_at)
                    }`}
                  </Typography.Text>
                  <Typography.Text type="secondary" data-testid="history-detail-version">
                    {`版本 ${detail.session.row_version}`}
                  </Typography.Text>
                </Space>

                {detail.session.state === "finished" ? (
                  <Typography.Text type="secondary" data-testid="history-soft-delete-note">
                    删除误记是软删除：区间与审计都保留，只是不再计入工时。
                  </Typography.Text>
                ) : (
                  <Typography.Text type="secondary" data-testid="history-correct-disabled-note">
                    {`只有已结束的会话能修正起止或删除误记：这条是「${
                      SESSION_STATE_TEXT[detail.session.state]
                    }」${correctionHint(detail.session.state)}。`}
                  </Typography.Text>
                )}

                <div>
                  <Typography.Text type="secondary">{`区间 ${detail.intervals.length} 条`}</Typography.Text>
                  <ul className="history-intervals" data-testid="history-intervals">
                    {detail.intervals.map((interval) => (
                      <IntervalEditor
                        key={interval.id}
                        session={detail.session}
                        interval={interval}
                        disabled={disabled}
                        onInvalid={setError}
                        onRetime={(target, range) => void correctInterval(target, "retime", range)}
                        onDelete={(target) => void correctInterval(target, "delete")}
                      />
                    ))}
                  </ul>
                </div>

                <div>
                  <Typography.Text type="secondary">{`审计 ${detail.edits.length} 条`}</Typography.Text>
                  {detail.edits.length === 0 ? (
                    <Typography.Text type="secondary" data-testid="history-no-edits">
                      （这条会话还没有审计记录。）
                    </Typography.Text>
                  ) : (
                    <ul className="history-edits" data-testid="history-edits">
                      {detail.edits.map((edit) => (
                        <li key={edit.id} data-testid={`history-edit-${edit.id}`}>
                          {`${formatLocalMinute(edit.created_at)} · ${editSummary(edit)}`}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              </Flex>
            </Card>
          )}
        </>
      )}

      <Card size="small" title="补录一段已经发生的时间" data-testid="history-backfill">
        <Flex vertical gap={8}>
          <Typography.Text type="secondary" data-testid="history-backfill-note">
            补录是独立入口：它新建一条已结束的人工会话，不启动计时、也不伪造完成事件。
          </Typography.Text>
          <Space size="small" wrap>
            <Select
              aria-label="补录任务"
              data-testid="history-backfill-task"
              style={{ width: 240 }}
              placeholder="选一条任务"
              value={backfillTask}
              disabled={disabled}
              onChange={(value: string) => setBackfillTask(value)}
              options={tasks.map((task) => ({ value: task.id, label: task.title }))}
              // 搜索只在**已加载**的候选里过滤（契约没有关键词字段，见 `loadTasks` 的注释）；
              // "全量可达"由下拉底部的「加载更多」负责。
              showSearch={{ optionFilterProp: "label" }}
              // 候选只有几百条：关掉虚拟滚动，测试与用户看到的是同一份 DOM。
              virtual={false}
              popupRender={(menu) => (
                <>
                  {menu}
                  <Flex
                    align="center"
                    justify="space-between"
                    gap={8}
                    className="history-backfill-more"
                    data-testid="history-backfill-more"
                  >
                    <Typography.Text type="secondary" data-testid="history-backfill-tasks-info">
                      {`已列出 ${tasks.length} / 共 ${taskTotal} 条`}
                    </Typography.Text>
                    <Button
                      size="small"
                      type="link"
                      data-testid="history-backfill-load-more"
                      loading={taskLoading}
                      // 取满了就不给入口（判据是服务端给的 `total`，不是页面自己数出来的）
                      disabled={taskLoading || tasks.length >= taskTotal}
                      onClick={() => void loadTasks(tasks.length)}
                    >
                      加载更多
                    </Button>
                  </Flex>
                </>
              )}
            />
            <Input
              aria-label="补录起点"
              data-testid="history-backfill-start"
              style={{ width: 190 }}
              placeholder="2026-10-03 09:00"
              value={backfillStart}
              disabled={disabled}
              onChange={(event) => setBackfillStart(event.target.value)}
            />
            <Input
              aria-label="补录终点"
              data-testid="history-backfill-end"
              style={{ width: 190 }}
              placeholder="2026-10-03 10:00"
              value={backfillEnd}
              disabled={disabled}
              onChange={(event) => setBackfillEnd(event.target.value)}
            />
            <Button
              type="primary"
              data-testid="history-backfill-submit"
              disabled={disabled}
              onClick={() => void submitBackfill()}
            >
              补录
            </Button>
            <Typography.Text type="secondary">时间格式 YYYY-MM-DD HH:MM</Typography.Text>
          </Space>
        </Flex>
      </Card>
    </Flex>
  );
}
