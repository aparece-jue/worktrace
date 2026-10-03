# P4 · 项目、标签与今日计划实施计划

状态：计划修订待审核；实施未开始。依赖：[P1](2026-10-03-worktrace-v01-foundation.md)。上游：[总纲](2026-10-03-v01-plan-index.md)、02/04/05。覆盖 F-002 项目/标签、F-004/F-005、F-010 今日选择；不做权重、Knowledge/层级、排期、UI。

## Task 1：领域输入和时区校验

文件：domain/project.rs、domain/tag.rs、domain/localdate.rs、services/catalog.rs、services/daily_plan.rs、services/mod.rs、tests/catalog_validation.rs。

- [ ] ProjectStatus 支持 active/archived/done，与 02 一致；本版 UI 只提供创建、重命名和归档。TagKind 只有 Domain/Activity/Context/Report，拒绝 Knowledge 与非空 parent_id。
- [ ] LocalDate::parse 校验真实 YYYY-MM-DD，含闰年。标签名称去首尾空白，同 kind 大小写敏感唯一。
- [ ] Rust 服务校验 IANA 时区及 UTC，不能把任意无空白字符串当合法时区；验证时区库在 Windows 打包/断网可用后选定依赖并固定 Cargo.lock。
- [ ] 时区别名处理使用一致策略：若规范化则所有读写同样转换；用户更换时区不静默移动旧计划。日期按所选时区计算，不取 updated_at。
- [ ] 测试：闰年/不存在日期、未知时区 Mars/Olympus、UTC/上海/纽约、空输入、非法标签 kind/parent，发布环境离线校验。

## Task 2：项目仓储与服务

文件：storage/project_repo.rs、services/catalog.rs、tests/projects.rs。

- [ ] 仓储提供 get_project/list_projects、create_project(tx, input)、rename_project(tx, id, expected_version, name, now)、archive_project(tx, id, expected_version, now)；&Transaction 写入不自行提交或增加 revision。
- [ ] 服务入口带 expected_data_epoch，编辑带 project.row_version；同事务校验、写入、版本/revision 变化并返回提交信封。实际值没有变化则不增加版本/revision。
- [ ] 新建任务选择列表只列 active；归档保留既有任务/历史。新关联归档项目及在归档项目启动计时均拒绝，检查在事务中执行，不只依赖 UI 过滤。
- [ ] 测试：重命名、归档保留历史、归档选择过滤、旧 epoch/version、归档与创建/启动竞态、故障回滚。

## Task 3：标签及幂等关联

文件：storage/tag_repo.rs、services/catalog.rs、tests/tags.rs。

- [ ] 仓储 create_tag/get_tag/list_tags/tags_of_task；同 kind 内重复名拒绝，跨 kind 可同名。新建 tag 保存 row_version，未来重命名使用版本校验；首版不提供未规划的删除/层级接口。
- [ ] tag_task(tx, task_id, tag_id)/untag_task(tx, task_id, tag_id) 返回是否真正改变集合。服务校验 epoch 及实体存在，重复增删幂等，无变化不加 revision；首次关联 weight 恒 null。
- [ ] 关系增删是显式集合操作，不冒充整个任务表单覆盖；若以后批量替换标签集合，必须加关联集合版本契约。
- [ ] 测试：四类标签、多标签、同类重名/跨类同名、重复增删无 revision、未知实体、非法权重/层级请求、失败不留半个关联。

## Task 4：今日选择列表

文件：storage/daily_plan_repo.rs、services/daily_plan.rs、tests/daily_plan.rs。

- [ ] 仓储 add_to_plan(tx, task_id, date, timezone)/remove_from_plan(tx, ...)/plan_for(conn, date, timezone)；所有写入只操作调用方事务。
- [ ] 服务校验 epoch、真实日期、有效时区和任务存在。复合键仍为 task/date/timezone，重复增删不加 revision。按 task.created_at/id 稳定排序。
- [ ] 计划表示某日人工选择，不自动设置 Scheduled、不建 time_block、不启动计时。完成项可以保留在当天历史列表，由 UI 显示状态；不因任务更新日期自动搬移。
- [ ] 测试：日期/时区隔离、跨日、同创建时刻稳定排序、重复增删、任务不存在、改时区旧项保留、旧 epoch 拒绝、事务失败回滚。

## 验收与下游接口

- [ ] src-tauri 下 cargo fmt --check、cargo test、cargo clippy --all-targets 通过，执行 P1 分层检查。
- [ ] P1/P2 测试无回归；P4 可在 P1 后独立实施，不需要等 P2。
- [ ] 登记服务输入/输出及真实 Rust 签名供 P5/P7 消费；项目/标签版本字段与迁移一致。实际 UI 选择和发布应用离线验收由 P7 完成，不能将仓储测试标为 UI 已验收。

服务拥有事务并返回 epoch/revision；仓储只依赖 domain/shared error，不依赖 commands/platform。版本拒绝和任一步失败不得写审计、增加 revision 或留下部分变更。
