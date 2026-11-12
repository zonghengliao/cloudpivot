# HR-20 考勤台账（`biz_attendance_record`）落地证据

卡面：`attendance_record` model + 台账分页 + 单行修正（依赖 HR-19）。
设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.1 ⑤（DDL）、§4.2.3（LOCKED 后记录不许改）、
§4.9 W1（考勤天数守恒只报不拦）、§5.2 #21~#22（接口 + 权限码）、§6.2.3（台账页交互 = 单元格失焦保存）。

## 交付了什么

| 层 | 文件 | 内容 |
|---|---|---|
| 契约 | `backend/src/business/hr/attendance_record_dto.rs`（845 行） | 列表入参 / 单行修正入参（**部分更新**，缺省字段保持库里值）/ 台账行出参 / 一致性警告 + 9 条单测 |
| 数据 | `backend/src/business/hr/attendance_record_model.rs`（777 行） | `biz_attendance_record` **唯一** SQL 出口：建 / 读 / 锁定读 / 整行改写 / 台账分页 / 变更留痕快照 + 撞码与 CHECK 翻译 + 6 条单测 |
| 接口 | `backend/src/business/hr/attendance_record_handler.rs`（427 行） | #21 台账分页 / #22 单行修正（一事务：改记录 + 写审计 + 重算期间汇总）+ 3 条单测 |
| 真库 | `backend/src/business/hr/attendance_record_db_tests.rs`（883 行） | 5 条真库用例（台账一人一行 + 数据权限 / 修正写审计 + 汇总重算 / 锁定不可改 / 入参与 404 / 筛选形状） |
| 接线 | `hr/mod.rs`（4 个模块声明）、`shared/scope.rs`（`ScopeTarget::BIZ_ATTENDANCE_RECORD`）、`changelog/diff.rs`（`BIZ_ATTENDANCE_RECORD` 白名单 12 列） | — |

`hr/handler.rs` 的 `ROUTE_CONTRACT` **无需改动**：#21 / #22 早在 HR-12 就把设计 §5.2 的 46 条全部登记为
`card: "HR-20"` 的规划行，本卡只是把它们实现出来（`paths_are_registered_in_contract` 由全量 `cargo test --lib` 覆盖）。

**零迁移**：`biz_attendance_record` 由 HR-01 的 `V0383__hr_tables.up.sql` 建（唯一索引
`uk_biz_att_rec`、`CHECK ck_biz_att_rec_nonneg / ck_biz_att_rec_source`）。本卡只读写数据，不新增、不改任何迁移
（迁移已应用且指纹校验，AGENTS.md 约定 6）。

## 接口清单

| # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 说明 |
|:--:|---|---|---|---|
| 21 | GET | `/business/hr/attendance/record/page` | `business:hr:attendance:record` | `period_id`（**必填**）+ `dept_id` + `keyword` + 分页（默认 20 / 上限 200） |
| 22 | PUT | `/business/hr/attendance/record/{id}` | `business:hr:attendance:record` | 手工修正天数 / 加班小时 / 迟到早退次数 / 备注（部分更新） |

出参（#22）除改后的行还带三样：`changed`（值没变 = 没写库没留痕）、`warnings`（W1 一致性提示）、
`period_totals`（**同事务重算过的期间汇总**，省掉前端再打一次 `recalc`）。

## 验收实测

| 验收项 | 证据（`01_tests.out`） | 结果 |
|---|---|---|
| **台账按人一行** | `ledger_is_one_row_per_person_with_scope` | 一期两人 = 两行、`total=2`、按工号升序、工号不重复；同一人同一期插第二行撞 `uk_biz_att_rec` → `409 + 10003` 且报文点名工号；部门范围看不见别部门（`total=0`）、本部门看得见（`total=2`）；`dept_id` 过滤真生效（指别部门 → 0 行）；`keyword` 按工号 / 姓名都能命中；期间不存在 → 空台账（不是 500） |
| **单行修正写入审计** | `patch_writes_audit_and_recalcs_totals` | 改 `actual_days` 20 → 19.5：`changed=true`，`sys_change_log` **恰好 1 条**，`changed_fields=["actual_days"]`，`diff.fields.actual_days.b="20.00"` / `.a="19.50"`（两位小数，无假变更），未改的那一行 0 条；同值再提交 → `changed=false`、库与审计都不动；一次改多列 → 1 条日志记 2 列（升序）+ 备注落进 `a` |
| **锁定期间不可改** | `locked_period_blocks_record_updates` | `DRAFT→SUBMITTED→LOCKED` 后：台账仍可读但每行 `editable=false`；`PUT` → `409 + 68010`，报文点名期间码 + 给出「先解锁」下一步；**库里 11 个数值列逐列未变**、审计 0 条（不是「报错了但写进去了」）；解锁（要原因）后同一个请求又能改成功 |
| 汇总随修正重算 | `patch_writes_audit_and_recalcs_totals` | 期间头 `actual_days_total` 42 → 41.5，`employee_count` / `should_days_total` / `ot_hours_total` 不变 —— 「合计 = 各行之和」继续成立，不需要用户再点 `recalc` |
| 入参纪律 | `invalid_payloads_are_rejected_before_writing`、`filter_shape_is_enforced` | 空 body → 400；天数 40 → 400（点名 `should_days`）；不存在的记录 → 404；被拒的请求一个字节都没写进库（数值列与审计都未变）；`period_id` 缺失 / 非 uuid、`dept_id` 非 uuid → 400 |
| 门禁 | `02_gates.out` | `cargo clippy --all-targets -- -D warnings` **EXIT=0**；本卡 7 个文件 `rustfmt --check` 全 `FMT_OK` |
| 残留 | `03_residue.out` | 演练库 `hr20_drill`（145）用例自带清场后：`biz_attendance_record` / `biz_attendance_period` / `biz_employee` / `sys_change_log` **全 0 行**；跑完 DROP 库 |

用例总数：`cargo test --lib business::hr::attendance_record` → **23 passed / 0 failed**（5 条真库 + 18 条单测），
`0` 条 SKIP（`HR_DB_URL` 已指向 145 的 `hr20_drill`，不是「静默假绿」）。

## 关键实现取舍（代码注释里都写全了）

1. **单行修正「没有变化」返回 200 + `changed=false`，不是 409**：设计 §6.2.3 的台账是单元格**失焦保存**，
   用户点进格子、发现没错、点别处，前端就可能原样发一次。回 409 会弹无意义的「冲突」框。此时不写库、
   不写 `sys_change_log`（值没变就不该有「变更」）。HR-19 的期间 `PUT` 是整表单编辑，那里 409 才对。
2. **一事务三件事 + 固定加锁顺序「期间 → 记录」**：改记录、写审计、重算期间汇总必须同生同灭；
   期间先锁（`period_model::lock_in`）再锁记录（`model::lock_in`），与 HR-19 的 recalc、HR-21/22 的导入
   同一顺序，才不会互相等成死锁。
3. **锁定守卫双层**：handler 先 `FOR UPDATE` 锁期间判 `LOCKED` 并回滚（库值分毫不动）；
   `UPDATE_SQL` 自带 `AND NOT EXISTS (… p.status = 'LOCKED')` 是最后一道 —— 以后谁再写第二个改考勤的入口
   （导入 upsert、请假同步）忘了判期间状态，也改不动锁定期间。
4. **改路径不碰「这是谁 / 哪一期 / 从哪来」**：`period_id` / `employee_id` / `employee_no` / `employee_name` /
   `dept_id` / `source` 全不在 `UPDATE_SQL` 里（单测 `update_sql_never_touches_guarded_columns` 逐列钉住）。
5. **姓名口径按 AGENTS.md 约定 13**：`employee_no` / `employee_name` 读**快照列**（工资按快照出表，改名不影响
   历史考勤）；`dept_name`、`updated_by_name` **实时 join**，三个 join 与主查询同一条 SQL（防 N+1）。
6. **应出勤守恒只报不拦**：`consistency_warnings` 出「应出勤 ≠ 实 + 假 + 旷」的差额文本（容差 0.01），
   回 200 带 `warnings`；固定 21.75 / 排班制都是合法口径，硬拦会把合法数据挡在门外（设计 §4.9 W1）。
7. **`insert` 入参收进 `NewRecord` 结构体**：避开 `clippy::too_many_arguments`，也让 HR-21 导入链路调用时
   读起来是「谁、哪一期、多少天」（仓库里 `erp/*_model.rs` 同款做法）。

## 本卡没做的事（按纪律上报，不自己动）

| 事项 | 归属 | 说明 |
|---|---|---|
| 导入链路（`POST /attendance/import`）建记录 | **HR-21** | 本卡只提供 `model::insert` + `NewRecord`；台账页**不提供「新增一行」**（人是导入进来的，录错了改导入源） |
| 导出（`GET /attendance/export`）、本人考勤（`GET /attendance/my`） | **HR-30 / HR-41** | — |
| 「锁定后写保护」在**其他写入口**上的全接线（导入 upsert / 请假同步 / DB 层触发器） | **HR-23** | 本卡在自己的入口上封死，并把库层守卫写进 `UPDATE_SQL` 供 HR-23 复用 |
| 台账前端页（期间下拉 + 单元格编辑） | **HR-29** | 出参已给 `editable` / `period_status` / `period_totals`，前端不用自己判状态 |

> 工作区里另有 `backend/src/business/hr/attendance_lock.rs`、`attendance_lock_db_tests.rs`、
> `backend/migrations/business/V0392__attendance_lock_guard.*.sql` 三个**未提交**文件，内容属 HR-23
> （锁定写保护的 DB 层触发器 + 逃生门）。它们是早前一次中断的尝试留下的，**未纳入本卡提交、也未删除**
> （按纪律不越界，留给 HR-23 决定复用还是重写）。其中 `attendance_lock.rs` 目前有 11 处编译错误
> （`&nik::Error` vs `&DbError`、泛型参数个数），所以 `mod.rs` 里**没有**声明它们 —— 声明了整棵 lib 编译不过。
