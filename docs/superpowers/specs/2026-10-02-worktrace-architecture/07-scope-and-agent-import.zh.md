# Worktrace 产品边界与外部 Agent 导入契约

状态：按用户确认的收缩方向修订；2026-10-03。目标设计，尚未实现。[English](07-scope-and-agent-import.en.md)。
[总体架构](00-architecture.zh.md) · [模块分解](01-module-breakdown.zh.md) · [数据模型](02-data-model.zh.md) · [ADR](03-adr.zh.md) · [功能验收](04-functional-spec.zh.md) · [路线图](05-roadmap.zh.md) · [评审摘要](06-review-notes.zh.md) · [术语](99-glossary.zh.md) · [另一语言](07-scope-and-agent-import.en.md)

## 1. 定位与分工

帮助个人理清和安排工作、记录投入与成果、发现能力短板，并整理可追溯的回顾与 KPA 材料。普通日常记录不依赖 Agent：捕获、开始/暂停/结束、成果与问题备注即可；项目、标签、证据引用按需补充。

应用保留任务、工时、排期、标签、简要项目事实/决策、记录搜索、AI 建议与回顾、能力画像及 KPA。外部 Agent 及配套 skill 处理文件读取、OCR、正文分析/提取/搜索；应用不扫描目录、不跟踪源文件内容、不持有文件正文缓存，也不直接执行导入文本中的命令。文件引用仅供用户主动打开，必须确认并校验允许的路径/URL，拒绝可执行及危险协议，不自动访问引用以验证存在性。

配套 skill 是未来交付物，本轮只定义其输出协议，未创建或安装 skill。用户在外部工具中决定文件和模型访问范围；skill 不替代授权、不保证自动脱敏。导入信息可能敏感，采纳不代表同意外发。

## 2. AI 与成长及 KPA

AI 默认关闭；每次选择记录、预览实际发送内容和提供商/端点，再确认发送。改变输入版本或目的地需重新确认。AI 保留理清、拆分、估时、标签、优先级/排期、简要背景检查、总结与反馈；建议逐项采纳，不自动计时或修改事实。程序计算工时/数量，AI 写草稿，用户审核。没有配置模型时全部核心能力可用。

能力画像保留知识使用、用户自评、估时偏差、返工/阻塞原因；识别短板并关联学习/练习措施及后续复盘。可选熟练度/评分区分自评与推断，显示证据、样本、算法版本、未知和局限；单纯耗时不等于能力，不能承诺客观评价。KPA 按周期/项目整理任务时间点、确认工时、里程碑、成果和证据定位，所有草稿可追溯；不编造收益或给人自动绩效打分。

## 3. 最小 JSON 文件协议（schema_version=1）

批次包含 schema_version、batch_id、generated_at（带时区 ISO 8601）、producer（工具及 skill 名称/版本）、items；每项含稳定 item_id、kind、content 和 sources。kind 为 task、project_note、fact、decision、outcome。content 是按 kind 验证的对象；来源是 title、reference、locator（可空）、observed_at（可空）。来源由导出者声明，不等于应用验证过文件或技术结论。外部 ID 使用独立命名空间，不作为数据库主键。

```json
{
  "schema_version": 1,
  "batch_id": "project-review-001",
  "generated_at": "2026-10-03T09:00:00+08:00",
  "producer": {"tool": "external-agent", "skill": "worktrace-export", "skill_version": "1"},
  "items": [{
    "item_id": "item-001",
    "kind": "task",
    "content": {"title": "复核测试结果", "description": "确认异常项及后续措施", "completion_criteria": "记录复核结论"},
    "sources": [{"title": "测试报告", "reference": "report.pdf", "locator": "第 3 页", "observed_at": null}]
  }]
}
```

各 kind 最小 content：task.title（description/completion_criteria 可选）；project_note.title/body；fact.key/value；decision.title/rationale/decided_at；outcome.title/body/occurred_at。日期时间均带时区；允许不完整的建议注明 unknown，不允许生成虚构时间或工时。任务引用、项目归属、标签由导入预览映射；首版不导入 timer/session、工作区间、历史完成时间或直接批量覆盖状态。

## 4. 导入事务与失败边界

选择文件 → 本地验证结构/版本/大小 → 预览内容、来源与新增/冲突 → 选择目标项目及逐项采纳 → 单事务创建/修改选中记录、来源、导入映射和审计 → 返回 epoch/revision。未采纳条目不进入业务实体。未知格式版本、非法 kind/字段/日期、超限整体拒绝并给出定位。推荐首版硬限制 5 MiB、1000 条，具体性能通过 M01 实验验证；未知字段拒绝，正文不可作为指令执行。

相同 producer/batch_id/item_id 与同内容哈希重复采纳返回既有映射，不重复创建。哈希由应用按规范 JSON 计算（键排序、UTF-8，协议版本内固定），不信任外部提供的哈希；同 ID 不同内容显示冲突，不静默覆盖。新批次的相似条目提示人工核对，不承诺语义去重。取代已有事实/决策或编辑任务须显式映射并带 expected_data_epoch/row_version；所选条目任一失败整体回滚，可以减少选择后重试。导入审计与来源包含敏感信息，按用户数据备份，不进入诊断日志。

## 5. 交付与验收

V0.1 支持简单成果/问题备注及手动证据引用；V0.3 AI 输入确认与建议；V0.4 JSON 导入和配套 skill；V0.5 能力措施复盘及 KPA 证据材料。验收包含：无文件正文读取、未知版本/非法结构拒绝、重复采纳、ID 内容冲突、部分选择与失败回滚、旧版本编辑冲突、来源保留、危险引用拒绝、无自动网络请求及无自动 AI 发送。配套 skill 与应用共用协议样例，skill 更新不得隐式改变协议。
