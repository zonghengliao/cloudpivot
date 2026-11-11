# HR-10 `employee_model.rs` —— 员工主档 CRUD + 三个唯一键冲突翻译 · 验收证据

卡：`t_f5943a71`（批 2 员工档案第 2 张）· 依赖 HR-09 · 设计：`docs/architecture/ARCH-9-人力资源模块设计.md`

## 1. 本卡做了什么

| 交付 | 文件 | 说明 |
|---|---|---|
| 员工主档数据层 | `backend/src/business/hr/employee_model.rs`（新，约 1150 行） | `biz_employee` 的**唯一** SQL 出口：建 / 读（详情 + 列表分页）/ 改 / 软删 / 改绑账号 / 状态 CAS 回写 + 三个唯一键的冲突翻译 + `CHECK` 文案 |
| 真库用例 | `backend/src/business/hr/employee_model_db_tests.rs`（新，6 条） | `HR_DB_URL`（回退 `ERP_DB_URL`）开关；不设则 SKIP |
| HR 错误码段 | `backend/src/shared/error_code.rs`（+68 行） | 新增 `68xxx` 段：`68001` 工号重复 / `68002` 身份证重复 / `68003` 账号已绑定 / `68004` 业务闸，含码值冻结单测 |
| 模块挂载 | `backend/src/business/hr/mod.rs`（+5 行） | `pub mod employee_model;` + `#[cfg(test)] mod employee_model_db_tests;` |

**本卡零前端改动**（数据层卡），故未跑 `pnpm build`；`pnpm typecheck` 已跑绿（见 `03_frontend_typecheck.out`）。

## 2. 验收项逐条对照

### ① CRUD 全通

| 动作 | 函数 | 真库用例断言 |
|---|---|---|
| Create | `insert` / `insert_with_status` | `crud_roundtrip_and_soft_delete_frees_unique_keys`：建完能查、默认状态 `PROBATION`、`create_by` 落库 |
| Read（详情） | `find` / `find_in` / `lock_in` | 全字段读回；`dept_name` / `username` **join 实时取名**（非空断言 —— 防「界面显示短 uuid」） |
| Read（列表） | `page` + `EmployeeFilter` | 关键字命中工号能查到本行；`dept_name` / `username` 与详情一致；走 `scope::paginate`（唯一分页入口） |
| Read（唯一键查重） | `existing_by_emp_no` / `existing_by_id_card` / `existing_by_user_id` | 建后能查到、软删后查不到 |
| Update | `update` | 改名 / 手机 / 备注生效；**工号不变**（夹具故意把 `emp_no` 改成别的值，落库仍是原值）；改不存在的 id 返回 0 行 |
| Delete | `soft_delete` | 软删后 `find` 为 `None`、列表不含、重复软删返回 0（幂等）；**三个唯一键全部释放**（同工号 / 同身份证 / 同账号可重新建档 = 离职返聘） |

### ② 三种唯一键冲突都翻译成友好文案

`write_error(DbError, ctx, WriteValues) -> ApiError` 按**约束名**分支：

| 约束 | 码 | 实际文案（`conflict_message`） |
|---|---|---|
| `uk_biz_employee_no` | `409 + 68001` | 工号 `HR10-…` 已被其他员工使用（同一公司内工号唯一）。请换一个工号，或打开已有员工档案直接修改 |
| `uk_biz_employee_idcard` | `409 + 68002` | 身份证号 `110101********1234` 已登记在其他员工档案上（一人只允许一份在职档案）。如属重复录入，请打开原档案修改；如确属两人同号，请先核实身份证信息 |
| `uk_biz_employee_user` | `409 + 68003` | 该登录账号已绑定其他员工（一个账号只能挂一份员工档案）。请先在原员工档案上解除账号绑定，再来绑定这一个 |
| 其它唯一索引 | `409 + 10003` | 报文点名约束名（供运维定位），不假装认识 |
| `ck_biz_employee_*` | `400 + 10001` | 逐约束人话（「离职日期与员工状态不一致…」等 10 条） |
| 其它 `DbError` | `500 + 10004` | 对外只说「服务器内部错误」，真实原因进 `internal_detail` 日志 |

**身份证在报文里一律脱敏**（设计 D-14：明文要 `business:hr:employee:sensitive`）—— 冲突提示是最容易被忽略的泄漏点，`mask_id_card` 与 `sys_field_mask(mask_type=2, 6/4)` 同口径。

### ③ 单测覆盖三种冲突

- 纯单测（不连库，`employee_model.rs::tests`，15 条）：`classify_maps_exactly_the_three_keys`、
  `conflict_messages_are_friendly_and_distinct`、`write_error_translates_unique_and_check_violations`、
  `unique_key_names_match_v0383_ddl`（约束名与 `V0383` DDL 对拍）等；
- **真库用例（6 条）**：三条冲突各一条 + 互斥性 + CHECK + 状态 CAS。

> 真库用例是必需的：驱动要**从 PG 报文里解析出约束名**（`extract_identifiers`），
> 解析规则一改，「按名字翻译」就静默失效 —— 纯单测证明不了。
> 真实报文（`04_pg_error_real.out` 摘录）：
> `duplicate key value violates unique constraint "uk_biz_employee_idcard"`，
> `detail: Key (org_id, id_card)=(00000000-…, 110101199006004) already exists.`
> —— 即 PG 对**部分唯一索引**报的约束名就是索引名，与代码常量逐字一致。

## 3. 门禁（实测）

| 门禁 | 结果 | 证据 |
|---|---|---|
| `cargo check --all-targets` | 绿，零警告 | （日志见卡内汇报） |
| `cargo clippy --all-targets -- -D warnings` | 绿 | `02_clippy.out` |
| `rustfmt --check`（本卡 4 个文件） | clean | `06_fmt.out` |
| `cargo test --lib business::hr`（带真库） | **35 passed / 0 failed**（其中 6 条真库用例**真跑**，无 SKIP） | `01_hr_tests_real_db.out` |
| `cargo test --lib`（全量，带真库） | **4847 passed / 0 failed**（HR-09 时 4819 ⇒ +28 = 15 纯单测 + 1 码表 + 6 真库 + 6 其它） | `05_full_lib_tests.out` |
| `pnpm typecheck` | EXIT=0（本卡零前端改动） | `03_frontend_typecheck.out` |

真库：`hr10_card`（全新空库，`./target/debug/cloudpivot profile=test` 启动自动跑完 `V0383`~`V0390` 全部迁移）。

## 4. 设计取舍（写下来供后续卡对齐）

1. **按约束名翻译，不按报文文案猜** —— PG 对部分唯一索引（`WHERE deleted_at IS NULL`）报的约束名 = 索引名；
   按 `duplicate key` 之类字面量匹配既会误判（驱动 `Display` 是 `unique constraint violated constraint=…`，
   根本不含 `duplicate key`），也无法区分是哪一处冲突。
2. **`PUT` 不含 `emp_no` / `org_id` / `status` / `leave_date` / `leave_type` / `user_id`** ——
   工号是身份、组织是隔离维度、状态只能走状态机（否则「状态变了但没有 `biz_status_log`」），
   账号走 `bind-user`。`update_sql_never_touches_guarded_columns` 钉住。
3. **可空日期一律 `NULLIF(?::text,'')::date`** —— `''::text::date` 在 PG 是硬错误（500），
   而「生日留空」是最常见表单状态；`nullable_dates_are_null_if_guarded` 钉住。
4. **`EmployeeWrite` 是「全量」不是 patch** —— 与设计 §5.2 #4「同上（不含 emp_no）」一致：
   接口层必须回填未提交字段，否则一存就清空。已在结构文档里写明。
5. **列表不取敏感列** —— `id_card` / `bank_account` 不进 `LIST_SQL`：脱敏是出参层的事，
   但列表根本不下发更省心（`select_sql_projects_every_column_the_row_reads` 反向断言）。
6. **`HR_DB_URL` 回退 `ERP_DB_URL`** —— 不给 HR 单独配演练库；同一台演练库上跑。

## 5. 已知待办 / 移交下游

| 事项 | 归属 |
|---|---|
| `SCOPED_TABLES` 加 `BIZ_EMPLOYEE`（本文件已给锚点 `EMPLOYEE_TARGET = ScopeTarget::new("biz_employee","e",dept_id,user_id)`） | HR-12 |
| 6 条接口注册 + 入参校验 + 出参脱敏（`EmployeeWrite` ↔ 请求 DTO） | HR-12 / HR-11 |
| 工号取号（`seq::next_master_code_free`，前缀 `EMP`）—— 目前 `insert` 只接收已取好的 `emp_no` | HR-14 |
| 合同台账（`contract_end_date` 已在列表子查询里取，合同 CRUD 不在本卡） | HR-15 |
| 敏感字段明文接口 + `audit_logs` 留痕 | HR-13 |

## 6. 残留

- 演练库 `hr10_card` 在验收后**已删除**；后端实例未常驻（用完即停，端口未占用）；
- 工作区只提交本卡 4 个文件，他人未提交改动（`erp/cost_calc_period*.rs`）未纳入。
