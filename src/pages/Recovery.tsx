import { domainState } from "../state/domainState";
/**
 * 恢复页（P8 Task 2c）：F-015 恢复确认。
 *
 * 这一页只做**订阅、渲染与转发命令**（00 §6）：什么算待确认、哪些状态能对账、
 * 起止怎么校验、作废写哪些行，全在 Rust（`services/recovery.rs`）。前端一个数字都不重算
 * ——连"有几条待确认"都读响应里的 `pending_intervals` / `pending_sessions`，不自己数列表。
 *
 * ## 唯一数据源
 *
 * 只读 `attention_overview`（命令 8）：它是恢复页与"待确认"栏的唯一数据源。
 * **不**用 `TimerSnapshot.pending_ms` 顶替它，也**不**按"列表是否为空"反推计时门禁
 * （两者不等价，`AttentionOverview` 的类型文档写了为什么）。
 *
 * ## 界面上的三块（照 02 §3/§4，不许合并）
 *
 * 1. **可信那一段**：会话此前**已闭合**的区间照旧计入统计，**不在**本次确认范围
 *    ——这个 DTO 只给候选（`items[].intervals`），可信前缀根本不在里面。所以界面上
 *    "可信"是一句明确的范围说明、候选是另一块，两块视觉可分，不混成一张表；
 * 2. **待确认候选**：每条候选的 `ended_at` / `duration_ms` 是**候选端点**、不是事实
 *    （"终点未知不推算"）⇒ 只当输入框的**提示**（placeholder），**不预填**成默认值
 *    强迫接受。用户给出合法且不重叠的起止之后才提交；重叠由**服务**判，页面上屏的是
 *    Rust 那句具体冲突（R8），前端不做重叠校验（做两份就会有两套口径）；
 * 3. **作废整次**：`discard_session` 是**另一条命令、另一个入口**，带二次确认，文案写清
 *    影响范围（全部区间软作废 + 会话 `discarded`、审计保留）。它与"丢弃不确定区间"
 *    （`reconcile(discard_uncertain)`：只作废待确认段、**保留此前闭合工时**）在界面上是
 *    **两个按钮**，不许合并成一个"丢弃"。
 *
 * ## 显式触发，不自动
 *
 * `retry_recovery` 与 `accept_detected_clock_correction` 都由用户点击触发：挂载时一条都
 * 不发（不做自动重试、也不自动接受校正）。重试的说明按 Ruling P6-3 写成"计时不可用
 * （故障态或提交后待刷新）"——快照上那两种成因合并成一个信号，页面不细分、也不去猜。
 *
 * ## 维护态
 *
 * 写命令被 `DATA_RESTORE_IN_PROGRESS` 拒绝 ⇒ 上屏的就是 Rust 的 message
 * （"正在恢复数据，请稍候重试。"，所以不需要前端另编一档文案），并按 `code` 把本页的写入
 * 入口**禁用**；库身份一变（`epoch` 换了 = 维护结束后的新库）即解禁。这是 P6 定的口径：
 * 维护态对 UI 只有两个出口——被拒的新码，与维护结束后的 `data_epoch` 变化，**没有**维护态事件。
 *
 * ## 判旧与失败
 *
 * 响应先过**本视图水位**（换库或比已上屏的那份旧 ⇒ 丢弃，不覆盖新状态），真的上屏之后
 * 才 `applied`；重拉时机 = 挂载、`useInvalidation()` 计数变化、写命令成功之后那一次。
 * 失败分两条路（收件箱页的既定口径）：查询失败用 `toIpcError(cause).message`（查询路径
 * **不** `reportCommandError`，那会自触发重拉）；写命令失败走 `reportCommandError`
 * ——`RECOVERY_REQUIRED` 那一档还会把用户导到本页（M8）。
 */

import { useCallback, useEffect, useState } from "react";
import { Button, Card, Empty, Flex, Input, Popconfirm, Radio, Space, Tag, Typography } from "antd";

import {
  acceptDetectedClockCorrection,
  attentionOverview,
  discardSession,
  reconcile,
  retryRecovery,
  toIpcError,
} from "../ipc";
import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { formatDuration } from "../components/duration";
import { formatLocalMinute, parseLocalMinute } from "../components/localTime";
import { createViewWatermark } from "../components/viewWatermark";
import { useDataEpoch, useInvalidation } from "../state/hooks";
import type {
  AttentionOverview,
  ConfirmedRangeRequest,
  PendingIntervalItem,
  ReconcileAction,
  ReconcileTargetState,
  SessionAttention,
  SessionAttentionItem,
} from "../types/ipc";
import { SESSION_STATE_TEXT } from "./Timer";

/**
 * `attention` 的中文。
 *
 * `invariant_broken` 是**诊断**档（记录已损坏、禁止自动修复），不是"更严重的待确认"；
 * `none` 那一类落在概览里是因为"不属于当前 run 且未结束"——它也需要人看一眼。
 */
const ATTENTION_TEXT: Record<SessionAttention, string> = {
  none: "需要处理",
  needs_review: "待确认",
  invariant_broken: "记录损坏",
};

/** 一条候选区间的口径文字：候选端点是材料、不是事实（"终点未知不推算"）。 */
function candidateText(interval: PendingIntervalItem): string {
  if (interval.ended_at === null) {
    return "候选起点已知、终点未知（不推算时长）";
  }
  return `候选跨度 ${formatDuration(interval.ended_at - interval.started_at)}（未确认）`;
}

/** 待确认区间的输入草稿：`interval_id` ⇒ 用户写的两段文本（候选值只进 placeholder）。 */
type Drafts = Record<string, { start: string; end: string }>;

/**
 * 把用户写的起止收成一次 `confirm` 的 `ranges`；任一条缺输入或格式不对 ⇒ `null`。
 *
 * `confirm` 必须**恰好覆盖**该会话的全部待确认区间（缺一条整条拒绝），所以这里逐条收、
 * 有一条不合格就整批不提交——提示也是整批一句，不半提交。
 */
function rangesOf(item: SessionAttentionItem, drafts: Drafts): ConfirmedRangeRequest[] | null {
  const ranges: ConfirmedRangeRequest[] = [];
  for (const interval of item.intervals) {
    const draft = drafts[interval.id] ?? { start: "", end: "" };
    const started_at = parseLocalMinute(draft.start);
    const ended_at = parseLocalMinute(draft.end);
    if (started_at === null || ended_at === null) return null;
    ranges.push({ interval_id: interval.id, started_at, ended_at });
  }
  return ranges;
}

/** 一条需要处理的会话：可信范围说明 + 待确认候选 + 两个互不合并的作废入口。 */
function SessionCard({
  item,
  target,
  drafts,
  disabled,
  onTarget,
  onDraft,
  onReconcile,
  onDiscardWhole,
}: {
  item: SessionAttentionItem;
  target: ReconcileTargetState;
  drafts: Drafts;
  disabled: boolean;
  onTarget(value: ReconcileTargetState): void;
  onDraft(intervalId: string, edge: "start" | "end", value: string): void;
  onReconcile(item: SessionAttentionItem, action: ReconcileAction): void;
  onDiscardWhole(item: SessionAttentionItem): void;
}) {
  const broken = item.attention === "invariant_broken";
  /**
   * `reconcile` 只接 `recovering`（R6）；损坏那一类还被服务额外拒绝——损坏不是"不确定"，
   * 确认一下就能洗成事实（服务层的原文）。界面只是体验，服务端同样拒。
   */
  const reconcilable = !broken && item.state === "recovering";

  return (
    <Card
      size="small"
      data-testid={`recovery-item-${item.session_id}`}
      title={
        <Space size="middle" wrap>
          <span data-testid={`recovery-task-${item.session_id}`}>{item.task_id}</span>
          <Tag data-testid={`recovery-state-${item.session_id}`}>
            {SESSION_STATE_TEXT[item.state]}
          </Tag>
          <Tag
            data-testid={`recovery-attention-${item.session_id}`}
            color={broken ? "error" : item.attention === "needs_review" ? "warning" : undefined}
          >
            {ATTENTION_TEXT[item.attention]}
          </Tag>
          <Tag data-testid={`recovery-run-${item.session_id}`}>
            {item.is_current_run ? "当前 run" : "旧 run"}
          </Tag>
        </Space>
      }
      extra={
        <Typography.Text type="secondary">{`会话 ${item.session_id}`}</Typography.Text>
      }
    >
      <Flex vertical gap={12}>
        {broken ? (
          <Typography.Text type="danger" data-testid={`recovery-fault-${item.session_id}`}>
            {`记录损坏，只作诊断、不自动修复：${item.fault_reason ?? "（没有诊断文本）"}`}
          </Typography.Text>
        ) : null}

        {/* ① 可信那一段：不在本次确认范围（列表只给候选，可信前缀不在这个 DTO 里） */}
        <Typography.Text type="secondary" data-testid={`recovery-trusted-${item.session_id}`}>
          此前已闭合的可信区间不在本次确认范围：它们照旧计入统计。
        </Typography.Text>

        {/* ② 待确认候选：与上面那块分开渲染，视觉可分 */}
        <div data-testid={`recovery-pending-${item.session_id}`}>
          {item.intervals.length === 0 ? (
            <Typography.Text
              type="secondary"
              data-testid={`recovery-no-candidate-${item.session_id}`}
            >
              这条会话没有待确认区间（它属于"不属于当前 run 且未结束"的那一类）。
            </Typography.Text>
          ) : (
            <Flex vertical gap={12}>
              <Typography.Text type="warning">
                {`待确认候选 ${item.intervals.length} 条：下面是候选端点，不是已确认事实。请逐条给出起止。`}
              </Typography.Text>
              {item.intervals.map((interval) => (
                <Flex
                  key={interval.id}
                  vertical
                  gap={4}
                  data-testid={`recovery-candidate-${interval.id}`}
                >
                  <Space size="middle" wrap>
                    <Tag color="warning">待确认</Tag>
                    <Typography.Text
                      type="secondary"
                      data-testid={`recovery-candidate-span-${interval.id}`}
                    >
                      {candidateText(interval)}
                    </Typography.Text>
                  </Space>
                  <Space size="small" wrap>
                    <Input
                      aria-label="候选区间起点"
                      data-testid={`recovery-start-${interval.id}`}
                      style={{ width: 190 }}
                      placeholder={formatLocalMinute(interval.started_at)}
                      value={drafts[interval.id]?.start ?? ""}
                      disabled={disabled || !reconcilable}
                      onChange={(event) => onDraft(interval.id, "start", event.target.value)}
                    />
                    <Input
                      aria-label="候选区间终点"
                      data-testid={`recovery-end-${interval.id}`}
                      style={{ width: 190 }}
                      placeholder={
                        interval.ended_at === null
                          ? "终点未知（请填）"
                          : formatLocalMinute(interval.ended_at)
                      }
                      value={drafts[interval.id]?.end ?? ""}
                      disabled={disabled || !reconcilable}
                      onChange={(event) => onDraft(interval.id, "end", event.target.value)}
                    />
                    <Typography.Text type="secondary">YYYY-MM-DD HH:MM</Typography.Text>
                  </Space>
                </Flex>
              ))}
            </Flex>
          )}
        </div>

        {reconcilable ? (
          <Flex vertical gap={8}>
            <Space size="middle" wrap>
              <Typography.Text type="secondary">对账之后这条会话停在：</Typography.Text>
              <Radio.Group
                size="small"
                data-testid={`recovery-target-${item.session_id}`}
                value={target}
                disabled={disabled}
                onChange={(event) => onTarget(event.target.value as ReconcileTargetState)}
                options={[
                  { label: "已结束", value: "finished" },
                  { label: "暂停", value: "paused" },
                ]}
              />
            </Space>
            <Space size="middle" wrap>
              <Button
                type="primary"
                data-testid={`recovery-confirm-${item.session_id}`}
                disabled={disabled}
                onClick={() => onReconcile(item, "confirm")}
              >
                确认这些起止
              </Button>
              <Button
                data-testid={`recovery-discard-uncertain-${item.session_id}`}
                disabled={disabled}
                onClick={() => onReconcile(item, "discard_uncertain")}
              >
                丢弃不确定区间
              </Button>
            </Space>
            <Typography.Text
              type="secondary"
              data-testid={`recovery-discard-uncertain-note-${item.session_id}`}
            >
              丢弃不确定区间只作废上面这些待确认段：此前已闭合的可信工时保留，会话转为上面选的状态。
            </Typography.Text>
          </Flex>
        ) : (
          <Typography.Text
            type="secondary"
            data-testid={`recovery-not-reconcilable-${item.session_id}`}
          >
            {broken
              ? "损坏的记录不能通过确认修复。"
              : "确认与丢弃只对 recovering 会话开放（Rust 会拒绝其它状态）。"}
          </Typography.Text>
        )}

        {/* ③ 作废整次：另一条命令 + 二次确认（点开确认框本身不发命令） */}
        <Space size="middle" wrap>
          <Popconfirm
            title="作废整次记录？"
            description="这条会话的全部计时区间都会被软作废、会话标记为已作废（审计保留）。它和「丢弃不确定区间」不是一件事：那个只丢待确认段。"
            okText="确认作废"
            cancelText="取消"
            disabled={disabled}
            onConfirm={() => onDiscardWhole(item)}
          >
            <Button
              danger
              data-testid={`recovery-discard-session-${item.session_id}`}
              disabled={disabled}
            >
              作废整次记录
            </Button>
          </Popconfirm>
        </Space>
      </Flex>
    </Card>
  );
}

export function Recovery() {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();

  const [overview, setOverview] = useState<AttentionOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** 一次**正常路径**的结果说明（`accepted: false` 也算，所以不放在错误提示里）。 */
  const [info, setInfo] = useState<string | null>(null);
  /** 维护态：写命令被 `DATA_RESTORE_IN_PROGRESS` 拒过 ⇒ 禁用写入入口。 */
  const [maintenance, setMaintenance] = useState(false);
  const [busy, setBusy] = useState(false);
  /** 每条会话的收尾状态（确认 / 丢弃之后停在哪）：默认"已结束"。 */
  const [targets, setTargets] = useState<Record<string, ReconcileTargetState>>({});
  /** 每条候选区间的输入草稿。 */
  const [drafts, setDrafts] = useState<Drafts>({});

  /**
   * **本视图这一次读**的水位。`useState(createViewWatermark)` 只借它拿一个稳定实例
   * （工厂只跑一次），它本身不是会变的状态、也不触发重渲染。
   */
  const [watermark] = useState(createViewWatermark);

  /** 读一次概览。查询失败**只提示、不刷新**（查询路径调 `refresh()` 会自己触发自己）。 */
  const load = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    try {
      const found = await attentionOverview({ expected_data_epoch: epoch });
      // 换过库、或比已经上屏的那一份旧 ⇒ 丢弃，不覆盖新状态。
      if (watermark.isStale(found, epoch, domainState.getView().dataEpoch)) return;
      // 「收到」≠「用上」：真的上屏之后才推进本视图水位。
      watermark.applied(found);
      setOverview(found);
      setError(null);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch, watermark]);

  // 事件只作缓存失效：`invalidated` 一变就重拉（挂载时也跑一次）。
  useEffect(() => {
    void load();
  }, [load, invalidated]);

  // 库身份变了（换库 / 维护结束后的新 epoch）⇒ 解禁写入入口（没有维护态事件，见文件头）。
  useEffect(() => {
    setMaintenance(false);
  }, [epoch]);

  /** 一次成功的命令之后当场重拉一次：不赌 `domain.changed` 到得比响应早。 */
  async function afterWrite(): Promise<void> {
    // **先清提示再重拉**：`load()` 失败时会写回新的提示，顺序反了会把刚写上的那句擦掉。
    setError(null);
    await load();
  }

  /**
   * 写命令失败的统一收尾：文案恒为 Rust 的 `message`；维护态按 `code` 记下来（禁用入口）。
   *
   * 只**置真**、不置假：解禁的判据是库身份变化（上面的 `useEffect`），不是"下一条命令成功了"
   * ——维护态下别的命令照样会被拒。
   */
  function fail(cause: unknown): void {
    if (toIpcError(cause).code === "DATA_RESTORE_IN_PROGRESS") setMaintenance(true);
    setError(reportCommandError(cause));
  }

  function setDraft(intervalId: string, edge: "start" | "end", value: string): void {
    setDrafts((current) => {
      const draft = current[intervalId] ?? { start: "", end: "" };
      const next = edge === "start" ? { ...draft, start: value } : { ...draft, end: value };
      return { ...current, [intervalId]: next };
    });
  }

  function setTarget(sessionId: string, value: ReconcileTargetState): void {
    setTargets((current) => ({ ...current, [sessionId]: value }));
  }

  /** 确认 / 丢弃不确定区间：都是 `reconcile`，差别在 `action` 与 `ranges`。 */
  async function reconcileSession(
    item: SessionAttentionItem,
    action: ReconcileAction,
  ): Promise<void> {
    if (epoch === null) return;
    let ranges: ConfirmedRangeRequest[] = [];
    if (action === "confirm") {
      const collected = rangesOf(item, drafts);
      if (collected === null) {
        // 界面层的前置提示（判据与 Rust 的"整条拒绝"同一口径，拦在本地只是省一次往返）。
        setError("请为每一条待确认区间填写起止时间，格式为 YYYY-MM-DD HH:MM。");
        return;
      }
      ranges = collected;
    }
    setInfo(null);
    setBusy(true);
    try {
      await reconcile({
        expected_data_epoch: epoch,
        session_id: item.session_id,
        expected_row_version: item.session_row_version,
        action,
        target_state: targets[item.session_id] ?? "finished",
        ranges,
      });
      await afterWrite();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  /** 作废整次：另一条命令（`discard_session`），二次确认在 `SessionCard` 的 Popconfirm 上。 */
  async function discardWhole(item: SessionAttentionItem): Promise<void> {
    if (epoch === null) return;
    setInfo(null);
    setBusy(true);
    try {
      await discardSession({
        expected_data_epoch: epoch,
        session_id: item.session_id,
        expected_row_version: item.session_row_version,
      });
      await afterWrite();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  /** 重试恢复：**用户显式触发**（挂载时一条都不发，也不做定时重试）。 */
  async function retry(): Promise<void> {
    if (epoch === null) return;
    setInfo(null);
    setBusy(true);
    try {
      await retryRecovery({ expected_data_epoch: epoch });
      setInfo("已重试恢复，计时状态已刷新。");
      await afterWrite();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  /** 接受一次已检测的墙钟校正：同样显式触发；`accepted: false` 是正常路径。 */
  async function acceptClockCorrection(): Promise<void> {
    if (epoch === null) return;
    setInfo(null);
    setBusy(true);
    try {
      const accepted = await acceptDetectedClockCorrection({ expected_data_epoch: epoch });
      setInfo(
        accepted.accepted ? "已接受这次时钟校正。" : "当前没有待接受的时钟校正，库没有变化。",
      );
      await afterWrite();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  const disabled = epoch === null || busy || maintenance;
  const items = overview?.items ?? [];

  return (
    <Flex vertical gap={16} data-testid="recovery-page">
      <Typography.Title level={5} style={{ margin: 0 }}>
        恢复
      </Typography.Title>

      <ErrorNotice message={error} />
      {info === null ? null : (
        <Typography.Text type="success" data-testid="recovery-info">
          {info}
        </Typography.Text>
      )}

      {overview === null ? (
        <Typography.Text type="secondary" data-testid="recovery-loading">
          正在读取待确认记录…
        </Typography.Text>
      ) : (
        <>
          {/* 计数直接来自响应（同一读事务）：本页不自己数列表 */}
          <Space size="middle" wrap data-testid="recovery-summary">
            <Typography.Text type="secondary" data-testid="recovery-pending-intervals">
              {`待确认区间 ${overview.pending_intervals} 条`}
            </Typography.Text>
            <Typography.Text type="secondary" data-testid="recovery-pending-sessions">
              {`待确认会话 ${overview.pending_sessions} 个`}
            </Typography.Text>
            <Typography.Text type="secondary" data-testid="recovery-fault-sessions">
              {`记录损坏 ${overview.fault_sessions} 个`}
            </Typography.Text>
          </Space>

          <Card size="small" title="计时状态" data-testid="recovery-timer">
            <Flex vertical gap={8}>
              <Typography.Text type="secondary" data-testid="recovery-retry-note">
                计时不可用（故障态或提交后待刷新）时点「重试恢复」。两条动作都由你显式触发：
                页面不自动重试，也不自动接受时钟校正。
              </Typography.Text>
              <Space size="middle" wrap>
                <Button data-testid="recovery-retry" disabled={disabled} onClick={() => void retry()}>
                  重试恢复
                </Button>
                <Button
                  data-testid="recovery-accept-clock"
                  disabled={disabled}
                  onClick={() => void acceptClockCorrection()}
                >
                  接受检测到的时钟校正
                </Button>
              </Space>
            </Flex>
          </Card>

          {items.length === 0 ? (
            <Empty description="没有需要处理的记录。" data-testid="recovery-empty" />
          ) : (
            items.map((item) => (
              <SessionCard
                key={item.session_id}
                item={item}
                target={targets[item.session_id] ?? "finished"}
                drafts={drafts}
                disabled={disabled}
                onTarget={(value) => setTarget(item.session_id, value)}
                onDraft={setDraft}
                onReconcile={(target, action) => void reconcileSession(target, action)}
                onDiscardWhole={(target) => void discardWhole(target)}
              />
            ))
          )}
        </>
      )}
    </Flex>
  );
}
