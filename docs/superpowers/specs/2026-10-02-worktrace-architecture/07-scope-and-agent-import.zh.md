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

各 kind 最小 content：task.title（description/completion_criteria 可选）；project_note.title/body；fact.key/value；decision.title/rationale/decided_at；outcome.title/body/occurred_at。日期时间均带时区；只有 occurred_at/decided_at 可用 null 表示未知；不使用字符串 unknown，不允许生成虚构时间或工时。任务引用、项目归属、标签由导入预览映射；首版不导入 timer/session、工作区间、历史完成时间或直接批量覆盖状态。

## 4. 导入事务与失败边界

选择文件 → 本地验证结构/版本/大小 → 预览内容、来源与新增/冲突 → 选择目标项目及逐项采纳 → 单事务创建/修改选中记录、来源、导入映射和审计 → 返回 epoch/revision。未采纳条目不进入业务实体。未知格式版本、非法 kind/字段/日期、超限整体拒绝并给出定位。推荐首版硬限制 5 MiB、1000 条，具体性能通过 M01 实验验证；未知字段拒绝，正文不可作为指令执行。

相同去重身份（定义见 §6） 与同内容哈希重复采纳返回既有映射，不重复创建。应用按 §6 定义计算哈希，不信任外部提供的哈希；同 ID 不同内容显示冲突，不静默覆盖。新批次的相似条目提示人工核对，不承诺语义去重。取代已有事实/决策或编辑任务须显式映射并带 expected_data_epoch/row_version；所选条目任一失败整体回滚，可以减少选择后重试。导入审计与来源包含敏感信息，按用户数据备份，不进入诊断日志。

![Agent 导入事务与失败边界](images/agent-import-flow.svg)

> 图：文件结构先整体校验；用户选中的采纳集合原子提交或回滚，未选条目不创建业务实体。

## 5. 交付与验收

V0.1 支持简单成果/问题备注及手动证据引用；V0.3 AI 输入确认与建议；V0.4 JSON 导入和配套 skill；V0.5 能力措施复盘及 KPA 证据材料。验收包含：不读取关联源文件正文（导入 JSON 与用户记录文本允许读取）、未知版本/非法结构拒绝、重复采纳、ID 内容冲突、部分选择与失败回滚、旧版本编辑冲突、来源保留、危险引用拒绝、无自动网络请求及无自动 AI 发送。配套 skill 与应用共用协议样例，skill 更新不得隐式改变协议。

实施细节补充见 [08：计时、阶段、AI 与报告快照](08-implementation-contracts.zh.md)。

## 6. 字段、去重与错误的精确定义

机器格式见 [JSON Schema](agent-import.schema.json)，样例见 [有效样例](examples/agent-import.valid.json)。该 Schema 仅定义结构；应用还必须验证真实日期、同批 item_id 唯一、大小、目标映射及事务。schema_version=1：fact.value 仅非空字符串；未知成果/决策日期为 null，其他必填内容不得用 null 或 unknown 代替。

去重身份为 producer.tool + producer.skill + batch_id + item_id；skill_version 为来源元数据，不改变身份。内容哈希 SHA-256 覆盖 kind/content/sources；sources 保持数组顺序，所有对象键递归按 UTF-16 排序，JSON.stringify 无空白，UTF-8、不做 Unicode 正规化，不含生成时间或 producer 版本。版本 1 内容仅字符串/null/对象/数组，无浮点数规范化问题。保存完整规范内容，以便排查哈希差异。

同身份不同内容返回 IMPORT_CONTENT_CONFLICT；目标实体已删除/作废或修改后返回 IMPORT_TARGET_CHANGED，由用户显式选择恢复/新增，不静默重建或覆盖。不同目标项目不得复用既有映射；返回 IMPORT_MAPPING_CONFLICT。非法格式/日期/重复 item_id 返回 IMPORT_VALIDATION_FAILED，未知版本返回 IMPORT_SCHEMA_UNSUPPORTED；任一所选项冲突整体不落库。

每项最多 100 个来源；拒绝重复 JSON 对象键、空白必填字符串及非法日期。可选 locator/observed_at 可省略或为 null；sources 可为空，但界面标为“无来源、待确认”。[无效输入样例](examples/agent-import.invalid-cases.json) 同时包含 Schema 与业务校验案例，具体错误归类以 §6 为准。
