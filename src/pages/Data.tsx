/**
 * 数据页（P8 Task 3b）：F-018 导出与 F-019 备份 / 恢复。
 *
 * 这一页只做**订阅、渲染与转发命令**（00 §6）：导出什么内容、周界怎么算、文件名怎么起、
 * 落在哪个目录、恢复怎么换库，全在 Rust（`services/export.rs` / `services/backup.rs`）。
 * 前端一个数字都不重算——连"导出哪一段"都读 `stats_today` 的响应
 * （`range` / `timezone` / `date`），不自己算日期与时区。
 *
 * ## 为什么本页要读一次 `stats_today`
 *
 * `export_data` 的 `json` 形状要一个**半开范围**（`from` / `to`）加时区，而这两个值只有
 * 统计服务能权威给出（日界换算、时区归一都在服务层）。所以本页读的是与今日页**同一条**
 * 查询（`stats_today`），把响应里的 `range` / `timezone` 原样转发给导出：导出文件里的数字
 * 与上屏的数字因此来自同一次样本（"同源"不是两处各算一遍凑出来的）。
 * `markdown` 形状**不带** `from` / `to`（周界由服务按 `anchor` 算；省略 `anchor` = 本周，
 * 同一次样本的归属终点）——判别式是"不适用就别传"，多传一个键会被命令层拒绝成
 * `DOMAIN_ERROR`。
 *
 * ## 三个动作
 *
 * 1. **导出**（`export_data`，命令 9）：格式二选一（JSON 范围明细 / Markdown 周回顾）。
 *    成功拿到**真实存在的绝对路径**后，把它显示出来（可一键复制）并调一次
 *    `revealItemInDir` 给出可打开的位置（"打开所在位置"按钮用来再打开一次）；
 * 2. **备份**（`backup`，命令 10）：成功后同样给路径与大小。备份**不改业务事实**
 *    （不加 `revision`、不广播）⇒ 页面**不拿"版本没变"当失败判据**；
 * 3. **恢复**（`restore`，命令 11）：全项目唯一的危险动作。**二次确认是界面的硬前置**：
 *    确认框里写清"会用备份覆盖当前数据"，确认后才带 `confirmed: true` 发命令；
 *    取消 ⇒ 一条命令都不发、也不报错（见下面"取消 ≠ 失败"）。
 *
 * 页面**不列目录内容**（也没有这条命令）：只显示命令交回的那条路径，所以导出失败可能留下的
 * `<名字>.partial` 残留不会被当成正式产物。也**不做重试**：同名产物的覆盖拒绝（Ruling
 * P8-33）是服务侧的事，页面把 Rust 的错误原样上屏就够。
 *
 * ## 取消 ≠ 失败
 *
 * 按零依赖方案，V0.1 **没有**"选择保存位置"这一步（路径固定、不引对话框）⇒ 这一档今天落在
 * **其它用户主动放弃**的场景上（本页是恢复的二次确认被取消）：不弹错误、不写文件、不重试，
 * 界面回到可再次触发的状态。**将来若引入保存对话框，用户取消走的就是这一档**——它不是
 * `AppError`，不该被上屏成失败。
 *
 * ## 恢复之后：立即切换库身份，旧展示一律作废
 *
 * `restore` 成功返回的是**新 `data_epoch`**：旧库的展示不属于新库 ⇒ 页面立即采用恢复回执
 * `domainState.acceptRestore(restored)`（立即采用恢复返回的库身份，旧请求作废 ⇒ 失效
 * 计数 +1 ⇒ 本页按新 epoch 重读），并把已上屏的样本与产物清掉（回落到重新加载态）。
 * 失败（含回滚也失败）按 Rust 的 `message` 上屏，**不伪装成功**："正在恢复"只由
 * `DATA_RESTORE_IN_PROGRESS` 这个码表达；恢复失败卡住时上屏的是它自己的原因
 * （Rust 那边 `error_response_without_lock` 的刻意口径），两者在界面上因此可以区分。
 *
 * ## 维护态（P6 口径）
 *
 * 写命令被 `DATA_RESTORE_IN_PROGRESS` 拒绝 ⇒ 上屏的就是 Rust 的 message（"正在恢复数据…"），
 * 并按 `code` 把三个动作的入口**禁用**；解禁**只认 `data_epoch` 变化**（没有维护态事件）。
 *
 * ## 能力不可用要明确（不得静默失败）
 *
 * "打开所在位置"用已装好的 `@tauri-apps/plugin-opener` 的 `revealItemInDir`，它的能力是
 * 构建期登记的（`src-tauri/capabilities/default.json` 的 `opener:allow-reveal-item-in-dir`）。
 * Tauri 2 **没有**给前端查询这条 ACL 的接口（opener 插件只注册了 `open_url` / `open_path` /
 * `reveal_item_in_dir` 三条命令，`checkPermissions()` 要找的 `plugin:opener|check_permissions`
 * 不在其中；tauri 2.12 的核心也没有这条命令）⇒ 能**先验**判定的只有"在不在 Tauri 运行时"
 * （`isTauri()`）：
 *
 * - **不在**（浏览器里跑前端）⇒ "打开所在位置"先就**禁用**并给出一句说明，点击也不会去打
 *   插件（`reveal` 第一句就返回）；产物路径照常显示、可复制；
 * - **在** Tauri 里但 ACL 没登记 ⇒ 调用当场被拒，页面把插件给的原因上屏允许修复原因后重试
 *   （同一句话里带上原因）。
 *
 * 两条路都不静默。导出/备份**本身**不依赖这条能力，所以它们不受它影响。
 *
 * ## 判旧与失败（与今日页/恢复页同一条口径）
 *
 * 读响应先过**本视图水位**（`createViewWatermark`；事件只作失效，数据从命令拉）；查询失败用
 * `toIpcError(cause).message`（查询路径**不** `reportCommandError`，那会自触发重拉），
 * 写命令失败走 `reportCommandError`。
 */

import { useCallback, useEffect, useState } from "react";
import { useDayBoundaryRefresh } from "../components/useDayBoundaryRefresh";
import { Button, Card, Flex, Input, Popconfirm, Radio, Space, Typography } from "antd";

import { isTauri } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

import { ErrorNotice } from "../components/ErrorNotice";
import { reportCommandError } from "../components/commandError";
import { formatDuration } from "../components/duration";
import { createViewWatermark } from "../components/viewWatermark";
import { backup, exportData, restore, statsToday, toIpcError } from "../ipc";
import { domainState } from "../state/domainState";
import { useDataEpoch, useInvalidation } from "../state/hooks";
import type { BackupResult, ExportResult, TodayView } from "../types/ipc";

/** `ExportRequest.format` 的判别式（字符串枚举；页面只在这两个值里选）。 */
type ExportFormat = "json" | "markdown";

/**
 * 本机时区（IANA 名字）。
 *
 * 它是 `stats_today` 的**原始输入**（归一在服务入口）；导出用的时区取响应回显的那个
 * （`view.timezone`），所以"今天的日界"与"导出的时区"永远是同一个答案。
 */
function localTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone;
}

/** 本地时间的 `YYYY-MM-DD HH:MM`（只把 `range` 的两个端点标成人看得懂的区间）。 */
function formatLocal(ms: number): string {
  const at = new Date(ms);
  const pad = (value: number): string => String(value).padStart(2, "0");
  const date = `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
  return `${date} ${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/**
 * 一条产物（导出与备份同形：绝对路径 + 字节数）。
 *
 * 路径用 `Typography.Text copyable` 显示：V0.1 的"复制路径"口径（不引额外的剪贴板依赖，
 * 也不自己造一个复制实现）；"打开所在位置"是同一个能力的第二次入口。
 */
function Artifact({
  prefix,
  label,
  path,
  bytes,
  canReveal,
  onReveal,
}: {
  prefix: string;
  label: string;
  path: string;
  bytes: number;
  canReveal: boolean;
  onReveal(path: string): void;
}) {
  return (
    <Flex vertical gap={4} data-testid={prefix}>
      <Typography.Text type="secondary">{label}</Typography.Text>
      <Typography.Text code copyable={{ text: path }} data-testid={`${prefix}-path`}>
        {path}
      </Typography.Text>
      <Space size="middle" wrap>
        <Typography.Text type="secondary" data-testid={`${prefix}-bytes`}>
          {`大小 ${bytes} 字节`}
        </Typography.Text>
        <Button
          size="small"
          data-testid={`${prefix}-reveal`}
          disabled={!canReveal}
          onClick={() => onReveal(path)}
        >
          打开所在位置
        </Button>
      </Space>
    </Flex>
  );
}

export function Data() {
  /** 库身份：还没握手成功（`null`）时**一条带 epoch 的命令都不发**。 */
  const epoch = useDataEpoch();
  /** 缓存失效计数（事件只作失效）：它一变就重拉本页数据。 */
  const invalidated = useInvalidation();

  /** 本次统计样本（与今日页同一条查询）：导出用的范围/时区/数字都从它来。 */
  const [view, setView] = useState<TodayView | null>(null);
  /** 导出格式（判别式）：`json` 带范围、`markdown` 只带时区。 */
  const [format, setFormat] = useState<ExportFormat>("json");
  /** 最近一次**成功**的导出/备份产物（失败时清空，不留上一次的路径）。 */
  const [exported, setExported] = useState<ExportResult | null>(null);
  const [backedUp, setBackedUp] = useState<BackupResult | null>(null);
  /** 待恢复的备份路径：用户从别处复制过来的那一条（V0.1 的手填/复制口径）。 */
  const [restorePath, setRestorePath] = useState("");
  const [error, setError] = useState<string | null>(null);
  /** 一次**正常路径**的说明（今天只有"恢复完成"），不是错误。 */
  const [info, setInfo] = useState<string | null>(null);
  /** 维护态：写命令被 `DATA_RESTORE_IN_PROGRESS` 拒过 ⇒ 禁用三个入口。 */
  const [maintenance, setMaintenance] = useState(false);
  const [busy, setBusy] = useState(false);
  /** "打开所在位置"不可用的原因（不在 Tauri 运行时 / 插件被拒）；`null` = 可用。 */
  const [revealError, setRevealError] = useState<string | null>(null);

  /**
   * **本视图这一次读**的水位。`useState(createViewWatermark)` 只借它拿一个稳定实例
   * （工厂只跑一次），它本身不是会变的状态、也不触发重渲染。
   */
  const [watermark] = useState(createViewWatermark);

  /** 读一次统计样本。查询失败**只提示、不刷新**（查询路径调 `refresh()` 会自己触发自己）。 */
  const load = useCallback(async (): Promise<void> => {
    if (epoch === null) return;
    try {
      const sample = await statsToday({
        timezone: localTimezone(),
        expected_data_epoch: epoch,
      });
      // 换过库、或比已经上屏的那一份旧 ⇒ 丢弃，不覆盖新状态。
      if (watermark.isStale(sample, epoch, domainState.getView().dataEpoch)) return;
      // 「收到」≠「用上」：真的上屏之后才推进本视图水位。
      watermark.applied(sample);
      setView(sample);
      setError(null);
    } catch (cause) {
      setError(toIpcError(cause).message);
    }
  }, [epoch, watermark]);

  // 事件只作缓存失效：`invalidated` 一变就重拉（挂载时也跑一次）。
  useEffect(() => {
    void load();
  }, [load, invalidated]);

  useDayBoundaryRefresh(view?.range.to, load);

  /**
   * 库身份一变（恢复之后的新库、维护结束后的新 epoch）⇒ **旧展示一律作废**：样本与产物
   * 都退回空态（页面回落到"正在读取"），维护态解禁只认这一件事（没有维护态事件）。
   *
   * `info` 刻意**不**清：恢复成功那句说明正是在换库这一拍写上的，它讲的不是旧库的数据。
   */
  useEffect(() => {
    setView(null);
    setExported(null);
    setBackedUp(null);
    setRestorePath("");
    setMaintenance(false);
  }, [epoch]);

  /**
   * "打开所在位置"能不能用（**先验**判定，见文件头）：不在 Tauri 运行时里就没有这条能力；
   * 插件失败显示原因，并保留重试入口。
   */
  const canReveal = isTauri();

  /**
   * 给出可打开的位置（导出/备份成功后自动调一次；按钮用来再打开一次）。
   *
   * 不可用**先就返回**——不试一次再说（"点了才报错"正是要避免的形状）。插件失败**不是**
   * 业务命令失败（六码契约管的是 Rust 命令），所以不走 `reportCommandError`：把插件给的
   * 原因上屏，保留重试入口，不静默。
   */
  async function reveal(path: string): Promise<void> {
    if (!canReveal) return;
    try {
      await revealItemInDir(path);
      setRevealError(null);
    } catch (cause) {
      setRevealError(toIpcError(cause).message);
    }
  }

  /**
   * 写命令失败的统一收尾：文案恒为 Rust 的 `message`；维护态按 `code` 记下来（禁用入口）。
   *
   * 只**置真**、不置假：解禁的判据是库身份变化（上面的 effect），不是"下一条命令成功了"。
   */
  function fail(cause: unknown): void {
    if (toIpcError(cause).code === "DATA_RESTORE_IN_PROGRESS") setMaintenance(true);
    setError(reportCommandError(cause));
  }

  /** 导出：按判别式构造请求（不适用键一个都不带），成功后给路径、大小与可打开的位置。 */
  async function runExport(): Promise<void> {
    if (epoch === null || view === null) return;
    setInfo(null);
    // 先清掉上一次的产物：这条失败之后屏幕上不该留着旧路径（"失败 ⇒ 不显示成功路径"）。
    setExported(null);
    setBusy(true);
    try {
      const result =
        format === "json"
          ? await exportData({
              format: "json",
              timezone: view.timezone,
              expected_data_epoch: epoch,
              from: view.range.from,
              to: view.range.to,
            })
          : await exportData({
              format: "markdown",
              timezone: view.timezone,
              expected_data_epoch: epoch,
            });
      setExported(result);
      setError(null);
      await reveal(result.path);
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  /** 备份：只带库身份（备份不改业务事实，没有可校验的实体版本）。 */
  async function runBackup(): Promise<void> {
    if (epoch === null) return;
    setInfo(null);
    setBackedUp(null);
    setBusy(true);
    try {
      const result = await backup({ expected_data_epoch: epoch });
      setBackedUp(result);
      setError(null);
      await reveal(result.path);
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  /**
   * 恢复：二次确认已经在确认框里收过（界面硬前置），这里按契约把 `confirmed` 带上
   * ——命令层会**再校验一次**，`false` 在那边是零副作用的拒绝。
   */
  async function runRestore(): Promise<void> {
    if (epoch === null) return;
    setInfo(null);
    setBusy(true);
    try {
      const restored = await restore({
        backup_path: restorePath.trim(),
        expected_data_epoch: epoch,
        confirmed: true,
      });
      setRestorePath("");
      setError(null);
      setInfo("恢复完成，已切换到新库并刷新。");
      // 成功 = 换了库：立即采用恢复回执，不等待在途握手。
      // 旧展示由上面那个 effect 作废；失败走 `fail()`，**不**走这一支（不伪装成功）。
      await domainState.acceptRestore(restored);
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  }

  const disabled = epoch === null || busy || maintenance;
  /** 人工那一列：本页不重算任何数字，取不到就按"未给数"显示（不猜）。 */
  const humanMs = view?.confirmed.find((column) => column.measure === "human")?.ms ?? null;
  const restorePathReady = restorePath.trim() !== "";

  return (
    <Flex vertical gap={16} data-testid="data-page">
      <Typography.Title level={5} style={{ margin: 0 }}>
        数据
      </Typography.Title>

      <ErrorNotice message={error} />
      {info === null ? null : (
        <Typography.Text type="success" data-testid="data-info">
          {info}
        </Typography.Text>
      )}
      {maintenance ? (
        <Typography.Text type="warning" data-testid="data-maintenance">
          正在恢复：导出、备份与恢复暂时不可用（写类命令都会被拒），库身份一变即自动解禁。
        </Typography.Text>
      ) : null}
      {canReveal && revealError === null ? null : (
        <Typography.Text type="warning" data-testid="data-reveal-note">
          {revealError ??
            "当前环境没有「打开所在位置」的能力：产物路径仍会显示、可复制，但不能在文件管理器里打开。"}
        </Typography.Text>
      )}

      {view === null ? (
        <Typography.Text type="secondary" data-testid="data-loading">
          正在读取统计数据…
        </Typography.Text>
      ) : (
        <>
          {/* 口径标注：导出用的就是这一次样本（日期/时区/半开范围与数字同源） */}
          <Space size="middle" wrap data-testid="data-scope">
            <Typography.Text type="secondary">导出范围与今日页同一次统计样本</Typography.Text>
            <Typography.Text type="secondary" data-testid="data-date">
              {view.date}
            </Typography.Text>
            <Typography.Text type="secondary" data-testid="data-timezone">
              {view.timezone}
            </Typography.Text>
            <Typography.Text type="secondary" data-testid="data-range">
              {`[${formatLocal(view.range.from)}, ${formatLocal(view.range.to)})`}
            </Typography.Text>
            <Typography.Text type="secondary" data-testid="data-confirmed-human">
              {`已确认人工 ${humanMs === null ? "—" : formatDuration(humanMs)}`}
            </Typography.Text>
          </Space>

          <Card size="small" title="导出（F-018）" data-testid="data-export-card">
            <Flex vertical gap={12}>
              <Radio.Group
                data-testid="data-format"
                value={format}
                disabled={disabled}
                onChange={(event) => setFormat(event.target.value as ExportFormat)}
                options={[
                  { label: "JSON 范围明细", value: "json" },
                  { label: "Markdown 周回顾", value: "markdown" },
                ]}
              />
              <Typography.Text type="secondary" data-testid="data-export-note">
                JSON 带上面那一段半开范围；Markdown 只给时区——周界由服务按这一周算，
                页面不传范围，也不自己算周界。
              </Typography.Text>
              <Space size="middle" wrap>
                <Button
                  type="primary"
                  data-testid="data-export-run"
                  disabled={disabled}
                  onClick={() => void runExport()}
                >
                  导出
                </Button>
                <Typography.Text type="secondary">
                  路径固定（应用数据目录下的 exports/，文件名带时间戳）：V0.1 不做"选择保存位置"。
                </Typography.Text>
              </Space>
              {exported === null ? null : (
                <Artifact
                  prefix="data-export"
                  label="导出产物"
                  path={exported.path}
                  bytes={exported.bytes}
                  canReveal={canReveal}
                  onReveal={(path) => void reveal(path)}
                />
              )}
            </Flex>
          </Card>
        </>
      )}

      <Card size="small" title="备份与恢复（F-019）" data-testid="data-backup-card">
        <Flex vertical gap={12}>
          <Typography.Text type="secondary" data-testid="data-backup-note">
            备份产出当前库的一份副本，不改业务事实（版本不动、不广播）——版本没变不是失败。
          </Typography.Text>
          <Space size="middle" wrap>
            <Button
              data-testid="data-backup-run"
              disabled={disabled}
              onClick={() => void runBackup()}
            >
              备份
            </Button>
            <Typography.Text type="secondary">
              路径固定（应用数据目录下的 backups/）；产物路径可以复制出来，填进下面的恢复入口。
            </Typography.Text>
          </Space>
          {backedUp === null ? null : (
            <Artifact
              prefix="data-backup"
              label="备份产物"
              path={backedUp.path}
              bytes={backedUp.bytes}
              canReveal={canReveal}
              onReveal={(path) => void reveal(path)}
            />
          )}

          <Typography.Text type="danger" data-testid="data-restore-note">
            恢复会用备份覆盖当前数据：当前库的业务数据会被换掉，库身份随之改变（这是全项目唯一
            的危险动作）。点「恢复」之后还要在确认框里再确认一次，命令才会发出去。
          </Typography.Text>
          <Space size="middle" wrap>
            <Input
              aria-label="备份路径"
              data-testid="data-restore-path"
              style={{ width: 420 }}
              placeholder="粘贴一份备份产物的绝对路径"
              value={restorePath}
              disabled={disabled}
              onChange={(event) => setRestorePath(event.target.value)}
            />
            {/*
              二次确认是**界面硬前置**：点开确认框一条命令都不发，取消 ⇒ 零命令、零报错
              （"取消 ≠ 失败"，见文件头）。
            */}
            <Popconfirm
              title="用这份备份覆盖当前数据？"
              description="恢复会替换当前数据库：旧数据不再出现在界面上，库身份会变。请确认这条路径就是你想要的那份备份。"
              okText="确认恢复"
              cancelText="取消"
              disabled={disabled || !restorePathReady}
              onConfirm={() => void runRestore()}
            >
              <Button danger data-testid="data-restore-run" disabled={disabled || !restorePathReady}>
                恢复
              </Button>
            </Popconfirm>
          </Space>
        </Flex>
      </Card>
    </Flex>
  );
}
