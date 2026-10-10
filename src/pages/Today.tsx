import { domainState } from "../state/domainState";
/**
 * 今日页（P8 Task 1b）：F-010 的五项。
 *
 * 这一页只做**订阅、渲染与转发命令**（00 §6）：统计口径、日界换算、三类分列与
 * "作废不算待确认"全在 Rust（`services/stats.rs`），前端**不重算任何一个数字**。
 *
 * ## 五项来自一次读
 *
 * F-010 的五项 = ① 今日选择列表（`tasks`）、② 当前任务（`current`）、③ 确认人工工时、
 * ④ 运行暂计、⑤ 待确认时间——**全部来自一次 `stats_today`**（同一 `as_of` / `revision` /
 * `data_epoch`）。所以这一页**不发 `plan_for`**：`TodayView.tasks` 已经是同一个今日选择
 * 列表，再发一条只会造出第二个水位，还会让"今天"有两个来源。
 *
 * 三个工时数字各取三组列里 `measure === "human"` 的那一列（按 `class` + `measure` 找，
 * **不按下标**）；机器（后台 / 被动）与等待**分列显示**（04 §6），界面上没有任何"总计"
 * ——人工与机器是两件不同的事实，不得相加。
 *
 * ## 判旧与重拉
 *
 * 响应先过**本视图水位**（`src/components/viewWatermark.ts`）：`isStale` ⇒ 丢弃不上屏，
 * 真的上屏之后才 `applied`。页面**不推**全局水位，理由与收件箱/项目/任务页相同（那是给
 * 权威快照用的；拿一份视图去推会把同版本的 `domain.changed` 静默吞掉）。重拉时机 =
 * 挂载、`useInvalidation()` 计数变化（事件只作失效，数据从命令拉）、写命令成功之后那一次。
 *
 * ## 写（今日选择列表的增删）
 *
 * `add_to_plan` / `remove_from_plan` 的 `date` / `timezone` **取自响应**
 * （`TodayView.date` / `TodayView.timezone`），不是 JS 自己算的日期：日界换算与时区归一
 * 都在服务层，只有响应里那两个值保证"今天"是同一个"今天"。成功后**重拉一次**，
 * 不把写响应里的 `tasks` 直接上屏（响应只是"提交成功"，权威数据仍以读为准），
 * 也不推水位。
 *
 * 失败分两条路（收件箱页的既定口径）：查询失败用 `toIpcError(cause).message`
 * ——查询路径**不** `reportCommandError`，那会自触发重拉；写命令失败走
 * `reportCommandError(cause)`（`code` 决定行为，文案恒为 Rust 的 `message`）。
 */

import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Button, Card, Empty, Flex, Select, Space, Tag, Typography } from "antd";

import { addToPlan, listTasks, removeFromPlan, statsToday, toIpcError } from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { formatDuration } from "../components/duration";
import { formatLocalMinute } from "../components/localTime";
import { createViewWatermark } from "../components/viewWatermark";
import { useDataEpoch, useInvalidation } from "../state/hooks";
import {
  MEASURES,
  type Measure,
  type MeasureColumn,
  type TaskRow,
  type TaskStatus,
  type TodayView,
} from "../types/ipc";
import { SESSION_STATE_TEXT } from "./Timer";

/**
 * 候选任务的状态：与收件箱页的 `LISTED_STATUSES` 同一套口径（捕获进来的、待理清的、
 * 可开始的、已经开始过的）。
 *
 * 两处各写一份是**有意**的：收件箱回答"我手上有哪些任务"，这里回答"哪些还能放进今天"，
 * 两个问题将来会分叉，不该被一个共享常量绑在一起。
 */
const CANDIDATE_STATUSES: TaskStatus[] = ["Inbox", "Clarifying", "Ready", "Doing"];

/** 候选下拉取多少条（与收件箱同一个窗口；服务端上限 100）。 */
const CANDIDATE_LIMIT = 50;

/** 除人工以外的三项。次序就是契约里 `MEASURES` 的次序，不另写一份字面量。 */
const OTHER_MEASURES: Measure[] = MEASURES.filter((measure) => measure !== "human");

/** 四个 measure 的中文（口径词取自 04 §6：人工仅 FOREGROUND、机器分列、WAITING 单列）。 */
const MEASURE_TEXT: Record<Measure, string> = {
  human: "人工",
  machine_background: "机器（后台）",
  machine_passive: "机器（被动）",
  waiting: "等待",
};

/**
 * 本机时区（IANA 名字）。
 *
 * 它是 `stats_today` 的**原始输入**（归一在服务入口），也是本页唯一需要 JS 侧的日期/时区
 * 知识的地方——`date` / `range` 一律用响应回显的那两个值，不在前端另算"今天"。
 */
function localTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone;
}

/**
 * 一列毫秒上屏的三态（P5 口径）：
 *
 * - `null` = **未给数**（待确认列里一条已知端点的候选都没有，"终点未知不推算"）⇒ `—`；
 * - `0` = **零时长**（空数据）⇒ `0`：零是一个事实，不是空白，也不写成 `00:00`
 *   跟"未给数"混成一样；
 * - 其余 ⇒ `formatDuration`。
 */
function formatMs(ms: number | null): string {
  if (ms === null) return "—";
  return ms === 0 ? "0" : formatDuration(ms);
}

/** 取一组里某个 measure 的列；契约保证四项齐全，缺列按"未给数"处理（不猜、不补）。 */
function columnOf(columns: MeasureColumn[], measure: Measure): MeasureColumn | undefined {
  return columns.find((column) => column.measure === measure);
}

/**
 * 一组工时：F-010 的那一项（`human`）单独上屏，机器与等待**分列**在它下面。
 *
 * 三组（确认 / 运行暂计 / 待确认）的判据完全一样，所以只有这一份实现；`extra` 给待确认
 * 组挂"候选区间 N 条"那一句。
 */
function MeasureCard({
  title,
  columns,
  testId,
  extra,
}: {
  title: string;
  columns: MeasureColumn[];
  testId: string;
  extra?: ReactNode;
}) {
  return (
    <Card size="small" title={title} data-testid={testId}>
      <Flex vertical gap={12}>
        <div>
          <Typography.Text type="secondary">人工</Typography.Text>
          <div className="today-ms" data-testid={`${testId}-human`}>
            {formatMs(columnOf(columns, "human")?.ms ?? null)}
          </div>
          {extra}
        </div>
        <Typography.Text type="secondary">机器与等待（分列，不并入人工）</Typography.Text>
        <Space size="large" wrap>
          {OTHER_MEASURES.map((measure) => (
            <div key={measure}>
              <Typography.Text type="secondary">{MEASURE_TEXT[measure]}</Typography.Text>
              <div className="today-ms" data-testid={`${testId}-${measure}`}>
                {formatMs(columnOf(columns, measure)?.ms ?? null)}
              </div>
            </div>
          ))}
        </Space>
      </Flex>
    </Card>
  );
}

export function Today() {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();

  /** 已经上屏的那一份五项（连同它自己的口径字段）；还没上屏时为 `null`。 */
  const [view, setView] = useState<TodayView | null>(null);
  /** 候选任务（加入今日的下拉），来自 `list_tasks`。 */
  const [candidates, setCandidates] = useState<TaskRow[]>([]);
  /** 下拉里选中的候选任务 id。 */
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /**
   * **本视图这一次读**的水位。`useState(createViewWatermark)` 只借它拿一个稳定实例
   * （工厂只跑一次），它本身不是会变的状态、也不触发重渲染。
   */
  const [watermark] = useState(createViewWatermark);
  const [candidateWatermark] = useState(createViewWatermark);
  const [candidateError, setCandidateError] = useState<string | null>(null);

  /**
   * 重拉：`stats_today`（五项）与 `list_tasks`（候选下拉）**两条互不牵连**的查询。
   *
   * 五项那条是本页的交付物，失败就上屏提示；候选那条是辅助数据，独立发起、独立收尾
   * （失败保留上一次的候选并显示重试入口），所以它慢、它失败都不影响五项（fix round 1 / Important-1：
   * 原先两条绑在同一个 `Promise.all` + 同一个错误态上，`list_tasks` 一失败五项就一个都不渲染）。
   *
   * 判旧只有**一把**水位，按 `stats_today` 的戳判（`date` / `range` / 三组工时 / `revision`
   * 全在它身上，五项因此不可能来自两次不同步的读）；候选不参与判旧。
   *
   * 查询失败**只提示、不刷新**（查询路径调 `refresh()` 会自己触发自己）。
   */
  const load = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    // 候选下拉是**辅助数据**，单独一条查询、单独一条失败路径（照 `Tasks.tsx` 的
    // `loadOptions` 那条口径）：它**不 await**、失败也不进 `error`——慢或失败都不该拖住
    // 五项。失败时保留上一次拿到的候选（首次挂载就是空），不报错、不清屏。
    void listTasks({
      statuses: CANDIDATE_STATUSES,
      project: "any",
      limit: CANDIDATE_LIMIT,
      offset: 0,
      expected_data_epoch: epoch,
    })
      .then((selectable) => {
        if (candidateWatermark.isStale(selectable, epoch, domainState.getView().dataEpoch)) return;
        candidateWatermark.applied(selectable);
        setCandidates(selectable.tasks);
        setCandidateError(null);
      })
      .catch((cause) => {
        if (domainState.getView().dataEpoch === epoch) setCandidateError(toIpcError(cause).message);
      });
    try {
      const today = await statsToday({ timezone: localTimezone(), expected_data_epoch: epoch });
      // 换过库、或比已经上屏的那一份旧 ⇒ 丢弃，不覆盖新状态。
      if (watermark.isStale(today, epoch, domainState.getView().dataEpoch)) return;
      // 「收到」≠「用上」：真的上屏之后才推进本视图水位。
      watermark.applied(today);
      setView(today);
      // 成功上屏就把上一次的失败提示清掉（失败提示不该比它描述的那次读活得更久）。
      setError(null);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch, watermark, candidateWatermark]);

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

  /** 加入今日：日期与时区取响应里的（不是 JS 自己算的日期，见文件头）。 */
  async function add(taskId: string): Promise<void> {
    if (epoch === null || view === null) return;
    setBusy(true);
    try {
      await addToPlan({
        expected_data_epoch: epoch,
        task_id: taskId,
        date: view.date,
        timezone: view.timezone,
      });
      setSelected(null);
      await afterWrite();
    } catch (cause) {
      setError(reportCommandError(cause));
    } finally {
      setBusy(false);
    }
  }

  /** 从今日选择列表里移除一条。 */
  async function remove(task: TaskRow): Promise<void> {
    if (epoch === null || view === null) return;
    setBusy(true);
    try {
      await removeFromPlan({
        expected_data_epoch: epoch,
        task_id: task.id,
        date: view.date,
        timezone: view.timezone,
      });
      await afterWrite();
    } catch (cause) {
      setError(reportCommandError(cause));
    } finally {
      setBusy(false);
    }
  }

  const disabled = epoch === null || busy;

  /** 已经在今日列表里的任务 id：候选下拉要按它排除（按 `id`，不按标题）。 */
  const plannedIds = new Set(view?.tasks.map((task) => task.id) ?? []);
  const candidateOptions = candidates
    .filter((task) => !plannedIds.has(task.id))
    .map((task) => ({ value: task.id, label: task.title }));

  const current = view?.current ?? null;
  const liveHuman = view === null ? undefined : columnOf(view.live, "human");

  /**
   * 是否正在计时（Ruling P6-21）：**不能**按 `current !== null` 判——`current` 是协调器
   * 镜像里最后装载的那条会话，它可能已经结束（`finished` / `discarded`）。判据是
   * `state === "running"` **加** live 列真的有开放区间。
   */
  const running =
    current !== null && current.state === "running" && (liveHuman?.intervals ?? 0) > 0;

  return (
    <Flex vertical gap={16}>
      {candidateError ? <><ErrorNotice message={candidateError} /><Button onClick={() => void load()}>重试候选加载</Button></> : null}
      <Typography.Title level={5} style={{ margin: 0 }}>
        今日
      </Typography.Title>

      <ErrorNotice message={error} />

      {view === null ? (
        <Typography.Text type="secondary" data-testid="today-loading">
          正在读取今日数据…
        </Typography.Text>
      ) : (
        <>
          {/* R-03：口径标注可见——哪一天、哪个时区、覆盖哪一段（半开）、按什么分类算。 */}
          <Space size="middle" wrap data-testid="today-scope">
            <Typography.Text type="secondary">按当前分类</Typography.Text>
            <Typography.Text type="secondary" data-testid="today-date">
              {view.date}
            </Typography.Text>
            <Typography.Text type="secondary" data-testid="today-timezone">
              {view.timezone}
            </Typography.Text>
            <Typography.Text type="secondary">区间（半开）</Typography.Text>
            <Typography.Text type="secondary" data-testid="today-range">
              {`[${formatLocalMinute(view.range.from)}, ${formatLocalMinute(view.range.to)})`}
            </Typography.Text>
          </Space>

          {/* ① 今日选择列表 */}
          <Card size="small" title="今日选择列表" data-testid="today-plan">
            <Flex vertical gap={12}>
              <Space.Compact>
                <Select
                  aria-label="候选任务"
                  data-testid="today-candidate"
                  style={{ width: 260 }}
                  placeholder="选一条任务"
                  value={selected}
                  disabled={disabled}
                  onChange={(value: string) => setSelected(value)}
                  options={candidateOptions}
                />
                <Button
                  type="primary"
                  data-testid="today-add"
                  disabled={disabled || selected === null}
                  onClick={() => {
                    if (selected !== null) void add(selected);
                  }}
                >
                  加入今日
                </Button>
              </Space.Compact>
              {view.tasks.length === 0 ? (
                <Empty description="今天还没有选择任务：从上面的下拉里加一条。" />
              ) : (
                <ul className="today-list" data-testid="today-plan-list">
                  {view.tasks.map((task) => (
                    <li key={task.id} className="today-item" data-testid={`today-task-${task.id}`}>
                      <span className="today-title">{task.title}</span>
                      <Tag className="today-status">{task.status}</Tag>
                      <Button
                        size="small"
                        data-testid={`remove-${task.id}`}
                        disabled={disabled}
                        onClick={() => void remove(task)}
                      >
                        移除
                      </Button>
                    </li>
                  ))}
                </ul>
              )}
            </Flex>
          </Card>

          {/* ② 当前任务 */}
          <Card size="small" title="当前任务" data-testid="today-current">
            {current === null ? (
              <Typography.Text type="secondary" data-testid="today-current-none">
                当前没有会话
              </Typography.Text>
            ) : (
              <Space size="middle" wrap>
                <span className="today-current-title" data-testid="today-current-title">
                  {current.task_title}
                </span>
                <Tag data-testid="today-current-state">{SESSION_STATE_TEXT[current.state]}</Tag>
                <Tag data-testid="today-running" color={running ? "processing" : undefined}>
                  {running ? "正在计时" : "未在计时"}
                </Tag>
              </Space>
            )}
          </Card>

          {/* ③ 确认人工工时 */}
          <MeasureCard
            title="确认人工工时"
            columns={view.confirmed}
            testId="today-confirmed"
          />

          {/* ④ 运行暂计 */}
          <MeasureCard title="运行暂计" columns={view.live} testId="today-live" />

          {/* ⑤ 待确认时间：有没有待确认看 `intervals`（不是 `ms === null`，Ruling P5-12） */}
          <MeasureCard
            title="待确认时间"
            columns={view.pending}
            testId="today-pending"
            extra={
              <Typography.Text type="secondary">
                候选区间{" "}
                <span data-testid="today-pending-human-count">
                  {`${columnOf(view.pending, "human")?.intervals ?? 0} 条`}
                </span>
              </Typography.Text>
            }
          />
        </>
      )}
    </Flex>
  );
}
