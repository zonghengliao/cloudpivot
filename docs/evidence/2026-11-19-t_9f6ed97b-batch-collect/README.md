# M5-03 · 分批法：料工费归集到批次 — 证据包

> 卡：`t_9f6ed97b`（M5 成本批第 3 张，接 M5-02 归集表）· 日期：2026-11-19 · 主机：99（开发机，PG 在 145）
> 验证库：`cp_m503_dev`（145，`createdb -T cloudpivot_test_m502`；模板库 `cloudpivot_test` 的 schema 落后，见 M5-02 卡内结论）
> 真跑实例：`127.0.0.1:18024`（隔离库 `cp_m503_dev`，探针用完即清场并停进程）

## 一、交付了什么（两条接口 + 契约/取数/编排 + 11 条真库用例 + 1 个新错误码）

| 层 | 文件 | 说明 |
|---|---|---|
| 契约/纯逻辑 | `backend/app/src/business/erp/cost/cost_batch_collect_dto.rs` | 三路行状态（已归集 / 待归集 / 不可归集）、状态闸（期间批次冻结 · 批次关批 · 已作废）、旧归集行取舍 `subset_for_object`、出参；**复用** `cost-object:list` + `cost-period:calc` 两个既有权限码 |
| 取数 | `backend/app/src/business/erp/cost/cost_batch_collect_model.rs` | 本期活跃成本批次（闸门）、本批本期三要素已有归集额（一条 `FILTER` 聚合）、物料名批量补（防 N+1）、批次抬头（复用 M5-02 的范围探针）。**三路扫描与写入原语直接复用 calc 各段，不抄 SQL** |
| 编排 | `backend/app/src/business/erp/cost/cost_batch_collect.rs` | `preview`（同一套扫描 + 取差，不落库）与 `collect`（单事务：再确认状态 → 扫三路 → 校验 → 清过期 → 插入 → 回查）；先软删再插入的顺序与 calc 同纪律 |
| 接口 | `backend/app/src/business/erp/cost/cost_batch_collect_handler.rs` | `GET …/cost-collect-sheet/{object_id}/sources`（三路来源预览）、`POST …/cost-collect-sheet/{object_id}/collect`（执行归集） |
| 错误码 | `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` | 新增 `66106 ERP_COST_BATCH_COLLECT_BLOCKED`（409；66xxx 段续号，注册表用例同步加一行） |
| 接线 | `backend/app/src/business/erp/cost/mod.rs` | 4 个 `pub mod` + 1 个 `#[cfg(test)]` |
| 用例 | `backend/app/src/business/erp/cost/cost_batch_collect_db_tests.rs` | 11 条真库用例（`ERP_DB_URL` 开关）+ 23 条纯函数/契约用例，合计 **34** |

**没有**：新迁移（`biz_cost_collect` / `biz_cost_labor` / `biz_cost_overhead` / `biz_cost_period` 都是 V0370–V0379 已有的）、新权限码、新菜单、前端页面（前端卡是 M5-07）。

## 二、口径（决定，不是待办）

1. **写接口的入参只有 `period_code`**。金额 / 行数 / 落到哪个要素全部由来源单据决定：领料单（`biz_stock_out_item`，`ISSUE + POSTED + 挂本批`）、工时台账（`biz_cost_labor`，`CONFIRMED + 当期 + 挂本批`）、手工费用台账（`biz_cost_overhead`，当期 + **直接挂本批**）。绝不接受「往批上填钱」的字段 —— 手工补数只有三条单据入口。
2. **费用只收「直接挂本批」的手工台账**。折旧 / 报销恒进车间费用池（`cost_object_id IS NULL`），不属于任何一个批次，硬挂等于凭空分账；池的归集与分摊归 calc 第 3/5 段。
3. **与 calc 同一套幂等键**：`(STOCK_OUT, item_id, 1)` / `(LABOR_TICKET, labor_id, 0)` / `(OVERHEAD_TICKET, overhead_id, 0)`。所以「先按批归集、再跑期间 calc」不会算两遍（第 10 条真库用例实测 calc `inserted = 0`），反之亦然。
4. **只碰一个批次**：已有归集行里，只处理「对象 = 本批」的与「幂等键正好命中本批这次要写的」两类；别的批次的行一行不动（第 9 条用例 + 反证 A）。
5. **期间批次冻结即 409**：`SUBMITTED / APPROVED / SETTLED / CLOSED` 一律拒（`66106`，报文点名批次号 + 状态 + 怎么修），`DRAFT / CANCELLED / REVERSED` 放行。审批后不许改数是设计 §4.1 的硬口径。
6. **归集行变了 ≠ 计算单变了**：本期有 `DRAFT` 批次时出参 `recalc_required = true`，`message` 直接写「需重跑 calc 才跟上」——不偷偷重算（那是 calc 六段的活），也不假装已经算过。
7. **预览不做闸门报错**：被拦时照常返回来源清单 + `collectable=false` + 原因 + 修法（看数永远允许，改数才拦）。
8. **不可见即 404**（不是 403）：403 等于承认 id 存在，把别车间的批次号变成探测口。
9. **金额取来源账上值不重算**（料取 `item.amount`、工取 `lab.amount`、费取台账 `amount`）：重算就会与 1405/4101 的账面发生额分毫。

## 三、真跑的验收（每项可复核）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_fmt.out` | 5 个新文件 + `mod.rs` + `error_code.rs` 的 `rustfmt --check` | `rc=0` |
| `02_clippy.out` | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | 零告警（`rc=0`） |
| `03_tests_and_regression.out` | ① 本卡 34 条 ② 成本域全量 `business::erp::cost::` ③ 数据范围接线守卫 | ① **34 passed** ② **729 passed / 0 failed**（M5-02 时 695，本卡 +34）③ **1 passed** |
| `04_permission_audit.out` | `system::menu::permission_audit_tests`（N14–N21） | **8 passed**（含 N16 裸路由、N21 静态路由不被模板遮蔽） |
| `05_http_probe.out` | 起隔离实例 `:18024` → admin 登录 → 造夹具（料 150 / 工 240 / 费 20）→ 8 类请求 → 清场 | 见下「HTTP 八连」 |
| `07_teeth_a_subset_all_rows.out` | 反证 A：`subset_for_object` 改成恒真 | **红 4 条**（含真库「别的批次的归集行一行不许动」：`left 1 / right 0` —— 真的把别人的行清掉了） |
| `08_teeth_b_frozen_gate.out` | 反证 B：期间冻结分支永不进入 | **红 2 条**（真库 `collect_409_when_period_batch_is_frozen` 第 764 行 `expect_err` 崩：放行后真的写库了） |
| `09_model_sha_after_restore.txt` | 两文件变异前后 sha256 | 逐字节一致（`6dea4ddd…f192d` / `24216443…505c`） |
| `10_after_restore_green.out` | 还原后重跑 | **34 passed / 0 failed** |
| `11_check_fast.out` | `bash scripts/check.sh --fast`（L0 全部静态闸 + L1 fmt/clippy/check/typecheck） | **全部检查通过（L0 L1）**，`rc=0` |

### HTTP 八连（`05_http_probe.out`，真实例 + 真登录）

| # | 请求 | 结果 |
|---|---|---|
| ① | `GET …/sources`（缺 `period_code`） | **400 / 10001**「period_code 必填……没有期间就没有这张表」 |
| ② | `GET …/sources?period_code=2031-09` | **200**：`collectable=true`、无期间批次、料 1 行 150 / 工 1 行 240 / 费 1 行 20 全 `PENDING`、`totals=0`、`after_totals.total=410.0000` |
| ③ | `POST …/collect` | **200**：料/工/费各 `inserted=1`，`totals.total=410.0000`，`message="已归集 3 笔……合计 410.0000 元"` |
| ④ | 再 `POST …/collect` | **200**：`inserted=[0,0,0]`、`skipped=[1,1,1]`，合计仍是 410（幂等） |
| ⑤ | M5-02 明细 + 归集表 | **200**：明细 3 笔（料→工→费）合计 410 == 归集表该行 `mat 150 / labor 240 / oh 20 / total 410 / line_count 3` == 整表合计 |
| ⑥ | 不存在的批次 | **404 / 10002**（不区分「不存在 / 不可见」） |
| ⑦ | 已关批批次 `POST …/collect` | **409 / 66106**「批次 CB-M503-PROBE-2 已关批（CLOSED）……需要怎么做：关批批次只读……」 |
| ⑧ | `period_code=2031-13` | **400 / 10001**「期间编码必须是 yyyy-MM（收到「2031-13」）」 |

库内实况（探针结束前打印）：`MATERIAL|STOCK_OUT|1|150.0000`、`LABOR|LABOR_TICKET|1|240.0000`、`OVERHEAD|OVERHEAD_TICKET|1|20.0000`；清场后 `cost_collect=0 cost_object=0 stock_out=0`。

### 真跑实例的两个前置（排障用，不是缺陷）

1. `sys_config.erp.cost.enabled` 必须为 `true`，否则所有 `/business/erp/cost*` 统一 403 + 66079（框架级模块开关）。
2. `admin.must_change_password` 必须为 `false`，否则除改密/登出外一律 403。
3. 二进制必须在 `backend/` 目录下启动（迁移路径 `app/migrations` 是相对路径）—— 在别的 CWD 启动会「数据库未初始化」并 panic。

## 四、11 条真库用例覆盖（`cost_batch_collect_db_tests.rs`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `preview_lists_three_sources_with_states` | 三路各 1 行、状态 `PENDING`、已归集 0、`after_totals` = 150/240/20/410；**库里归集行仍为 0**（预览不写库） |
| 2 | `collect_writes_three_elements_and_is_idempotent` | 三路各写 1 行、金额取来源值、`totals=410`；第二次 `inserted=0/skipped=1`，行 id 与 `updated_at` 不变 |
| 3 | `collect_409_when_period_batch_is_frozen` | 期间批次 `SUBMITTED` ⇒ 409 + `66106`（报文点名 `SUBMITTED` + 撤回修法），一行不写；预览 `collectable=false` 且仍能看数 |
| 4 | `collect_409_when_object_is_closed` | `CLOSED` / `CANCELLED` 批次 ⇒ 409 + `66106`（点名「已关批」/「已作废」） |
| 5 | `collect_404_when_object_out_of_scope` | 别车间批次 ⇒ 404；非法 id 形状 ⇒ 404（不是 400） |
| 6 | `relinked_stock_out_moves_the_collected_row` | 领料改挂 B ⇒ 同一来源只留 1 条活行（归属 B），A 侧重跑纯无操作、金额 0 |
| 7 | `purges_rows_whose_source_no_longer_qualifies` | 来源退回草稿 ⇒ 旧行软删（行留痕、`live=0`）、金额归零，第三次收敛 |
| 8 | `labor_without_rate_is_reported_not_written` | `rate=0` 的工时 ⇒ 预览 `UNCOLLECTABLE` + 原因含「工资率」；归集 `inserted=0` 且一行不写 |
| 9 | `pool_rows_and_other_batches_are_untouched` | 别的批次的归集行幂等不动；未挂对象的费用台账不会凭空变成池行 |
| 10 | `period_calc_after_batch_collect_does_not_double_count` | 先按批归集 60，再跑 calc 料段：`inserted=0`、合计仍 60（同一套幂等键） |
| 11 | `zz_teardown_leaves_zero_residue` | 有 `DRAFT` 批次时 `recalc_required=true` 且 message 含「重跑 calc」；清场后归集/工时/费用/对象/期间批次全 0 |

## 五、边界

- 未改 `biz_cost_collect` / `biz_cost_object` / `biz_cost_period` 任何列与约束；未碰 settle / reverse / report / alloc 一行。
- 未新增迁移、未新增权限码、未新增菜单 ⇒ `menu_invariants.sh` 判据面不变。
- 未写前端（M5-07 的活）；本卡只保证接口契约、写入口径与状态闸。
- 只新增一个错误码 `66106`（66xxx 段续号），并同步了 `error_code.rs` 里的码段注册表用例与文档表。
- `cp_m503_dev` 用完即 drop（证据里所有数字均来自该库的真跑输出）。
