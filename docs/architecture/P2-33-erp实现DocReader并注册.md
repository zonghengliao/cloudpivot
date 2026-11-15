# P2-33 · `erp` 实现 `DocReader` 并注册

> 归属批次：P2-B 解环 · 卡 `t_72242e32` · 前置 P2-32（`flow` 侧接缝就位）
> 后续卡：P2-34（`flow` 摘掉对 `erp` 的 99 行直连 = **消边**）

## 1. 结论

本卡把 P2-32 立起来的接缝**在业务域里实现掉并在启动期注册**，共 16 个实现（一条审批一个），
三个方法全部**委派**给现有 `*_bridge`，只有 `doc_status` 是新写的表驱动查询：

```
改前：flow ──直连 135 行──→ erp
P2-32：flow ──trait/注册表──→ (抽象)                    ← 接缝就位，仍直连
P2-33：                        erp ──实现+注册──→ 抽象    ← 本卡（erp→flow 多 4 行，同向边）
P2-34：flow 的 99 行直连 → 4 个注册表调用                ← 消边
```

- 实现与注册：`backend/app/src/business/erp/doc_reader_impl.rs`（新，约 550 行）；
- 注册点：`backend/app/src/bootstrap.rs` 的 `register_doc_readers()`（`bootstrap()` 第一件事，
  `tokio::join!` 之前同步做完）；
- 用例：`backend/app/src/business/erp/doc_reader_impl_tests.rs`（新，16 条：9 条离线 + 6 条真库 + 1 条进程级注册）。

**本卡不消边，也不该消边**：`flow → erp` 的 135 行直连一行没动（`flow::doc_reader_tests` 的
燃烧率闸门仍以 135 为基线并通过），消边是 P2-34 的交付。本卡交付的是「P2-34 换调用点时
不必再写一行实现」—— 16 个实现已经躺在注册表里了。

## 2. 16 个实现与它们委派的对象

| # | `biz_type` | 单据表 | 单号列 | 状态列 | 制单人列 | 状态文案 | 终态回调 |
|:--:|:--|:--|:--|:--|:--|:--|:--|
| 1 | `PO_APPROVE` | `biz_purchase_order` | `order_no` | `status` | `create_by` | `purchase_model::status_label` | `purchase_bridge::on_po_settled` |
| 2 | `STOCK_IN_POST` | `biz_stock_in` | `in_no` | `status` | `create_by` | `stockin_base::status_label` | `stockin_bridge::on_post_settled` |
| 3 | `STOCK_IN_CANCEL` | `biz_stock_in` | `in_no` | `status` | `create_by` | 同上 | `stockin_bridge::on_cancel_settled` |
| 4 | `STOCK_TAKE_POST` | `biz_stock_take` | `take_no` | `status` | `create_by` | `stocktake_model::status_label` | `stocktake_bridge::on_stock_take_settled` |
| 5 | `PAYABLE_PAY` | `biz_payment` | `payment_no` | **`approval_status`** | `create_by` | `payable_payment::approval_status_label` | `payable_bridge::on_payment_settled` |
| 6 | `PAYABLE_CANCEL` | `biz_payable` | `payable_no` | `status` | `create_by` | `payable_query::status_label` | `payable_bridge::on_cancel_settled` |
| 7 | `VOUCHER_REVERSE` | `biz_voucher` | `voucher_no` | `status` | `create_by` | `voucher_query::status_label` | `voucher_bridge::on_reverse_settled` |
| 8 | `CONTRACT_APPROVE` | `biz_contract` | `contract_no` | `status` | `create_by` | `contract_model::status_label` | `contract_bridge::on_contract_settled` |
| 9 | `BUDGET_APPROVE` | `biz_budget` | `budget_no` | `status` | `create_by` | `budget_dto::status_label` | `budget_bridge::on_budget_settled` |
| 10 | `ASSET_DISPOSAL` | `biz_fixed_asset` | `asset_no` | `status` | `create_by` | `asset_dto::status_label` | `asset_bridge::on_disposal_settled` |
| 11 | `EXPENSE` | `biz_expense_report` | `report_no` | `status` | **`reporter_id`** | `expense_dto::status_label` | `expense_bridge::on_expense_settled` |
| 12 | `SO_APPROVE` | `biz_sales_order` | `so_no` | `status` | `create_by` | `sales_model::status_label` | `sales_approval_bridge::on_so_settled` |
| 13 | `PR_APPROVE` | `biz_purchase_requisition` | `req_no` | `status` | `create_by` | `purchase_requisition_dto::status_label` | `requisition_bridge::on_pr_settled` |
| 14 | `PRT_APPROVE` | `biz_purchase_return` | `return_no` | `status` | `create_by` | `purchase_return_dto::status_label` | `purchase_return_bridge::on_settled` |
| 15 | `COST_SETTLE` | `biz_cost_period` | `batch_no` | `status` | `create_by` | `cost_period_dto::status_label` | `cost_period_bridge::on_cost_settle_decided` |
| 16 | `RECEIPT_REFUND` | `biz_receipt_refund` | `refund_no` | `status` | `create_by` | `receipt_refund::status_label` | `receipt_refund_bridge::on_refund_settled` |

`guard_self_approval` / `internal_control_creator` 同样逐一委派（`PAYABLE_CANCEL` 走
`payable_bridge::internal_control_creator_of_payable`）。

### 2.1 三处不是「照抄表格」的判断

1. **`EXPENSE` 的制单人列是 `reporter_id` 不是 `create_by`** —— `expense_bridge::
   internal_control_creator` 今天查的就是 `reporter_id`（报销人 ≠ 代录的财务）。快照若取
   `create_by`，「详情里显示的制单人」与「内控排除的那个人」会分成两个人。用例
   `expense_snapshot_uses_reporter_as_creator` 用**两个不同用户**做反证（`reporter` vs `recorder`）。
2. **`PAYABLE_PAY` 的状态列是 `approval_status`** —— 付款台账 `biz_payment` 没有 `status` 列
   （`information_schema` 实测），账务态在应付单上；付款单自身只有审批态。
3. **`STOCK_IN_POST` 与 `STOCK_IN_CANCEL` 共用一张表**：同一张收货单两条审批、两个注册键、
   两个回调，但快照是同一份（用例 `stock_in_post_and_cancel_share_one_table` 钉住「两键一表」）。

### 2.2 `doc_status` 的表驱动（唯一新写的 SQL）

16 张表的形态一致（单号 / 状态两列 `varchar NOT NULL`、制单人可空、都有 `deleted_at`），
所以表名 + 三列 + 文案函数做成常量表 `ErpDoc::spec()`，SQL 只写一遍：

```sql
SELECT <doc_no> AS doc_no, <status> AS status, <creator> AS creator_id
  FROM <table> WHERE id = ?::uuid AND deleted_at IS NULL
```

- 拼进 SQL 的四段**全部是编译期字面量**（没有任何一段来自请求参数；`doc_id` 走绑定）；
- `deleted_at IS NULL`：软删单据对流程侧 = 不存在 → `Ok(None)`，审批详情显示「单据已删除」；
- `status` 原值 + `status_label` 文案分开返回（P2-32 的 `DocSnapshot` 契约：判据要原值、展示要文案）。

### 2.3 不注册的 `biz_type`（如实说明）

| `biz_type` | 为什么不在本卡 |
|:--|:--|
| `STOCK_TRANSFER` | 调拨审批的回调住在 `flow::task_bridge`（任务与调拨共用入口），今天**也**不在 `internal_control` 的 `governed` 名单里 —— 不注册 = 行为不变。要不要搬进 ERP 是 P2-34 的判断 |
| `TASK_ACCEPT` / `FLOW_EDGE_CONFIG` / `OTHER` | 不是 ERP 单据，本来就不受内控约束 |
| `HR_*`（4 条） | 人力资源域，属另一张卡（`business::hr`） |

### 2.4 两处既有缺口：如实沿用，不在本卡补

1. **`STOCK_IN_CANCEL` 今天没有内控闸**：`stockin_bridge::guard_self_approval` 内部只认
   `STOCK_IN_POST`（`if instance.biz_type != POST_BIZ_TYPE { return Ok(()) }`）。本实现委派同一个
   函数 → 等价于放行，与今天逐字一致。
2. `PAYABLE_CANCEL` **有**闸（`payable_bridge::guard_self_approval` 对 `CANCEL_BIZ_TYPE` 单列了一段）。

补缺口 1 = 多拦一批人 = **改变生产行为**，不属于「解环」。任何行为变化都必须能用等价性解释，
补闸不能 —— 所以写在这里，避免下一个人把它当成「P2-33 抄漏了」。

## 3. 注册与失败口径

`bootstrap()` 的第一件事（`tokio::join!` 之前、同步执行）：

```rust
register_doc_readers();   // → erp::doc_reader_impl::register_all()
```

- 成功：`INFO ... ERP 单据查询器注册完成（P2-33）... count=16`；
- 失败（只会是重复注册 = 编程错误）：**prod `exit(1)`**（与 `preflight_permission_guard` /
  `ai_agent_invariant_guard` 同一先例），其它 profile `panic`。
  理由：注册不全的后果是**静默**的 —— 漏掉的那条审批线内控闸退化成放行、终态回调没人接，
  单据停在「审批结束、单据没动」的中间态（AGENTS.md 约定 11），只能改库才能恢复。

注册表是「启动期写、请求期读」；`register_all()` 只允许调一次，二次调用会因重复键返回 `Err`
并点名 `biz_type`（用例 `process_registry_is_filled_by_register_all` 钉住）。

## 4. 验证证据（2026-11-15 · 99 开发机）

| 判据 | 命令 | 实测 |
|:--|:--|:--|
| 编译 | `cargo check --lib` | `Finished dev profile ... 34.25s`，**零 warning** |
| 静态检查 | `cargo clippy --all-targets -- -D warnings` | `Finished ... 2m 50s`，**零告警** |
| 本卡用例（离线） | `cargo test --lib business::erp::doc_reader_impl` | 16 passed（含 7 条真库用例 SKIP） |
| 本卡用例（真库 `cp_p233_docreader`） | `ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_p233_docreader cargo test --lib business::erp::doc_reader_impl -- --test-threads=1` | **16 passed; 0 failed** |
| 接缝用例（P2-32 的，含燃烧率闸门） | `cargo test --lib business::flow::doc_reader` | 15 passed（`flow_erp_reference_budget_is_not_exceeded` 通过，135 ≤ 135） |
| 分层护栏 | `cargo test --lib business::shared::layering` | 1 passed |
| `biz_type` 域契约 | `cargo test --lib business::erp::flow_biz_type_contract` | 2 passed |
| 内控纯函数 | `cargo test --lib business::flow::internal_control` | 16 passed |
| **启动期注册（实机）** | 见下 | `INFO cloudpivot::bootstrap: ERP 单据查询器注册完成（P2-33）... count=16` + `/health` = `{"code":0,...,"status":"ok"}` |

### 4.1 实机启动取证

```text
$ NIK_PROFILE=test NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=cp_p233_docreader \
  NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=*** NIK_SERVER_LISTEN=127.0.0.1:18093 \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=*** CLOUDPIVOT_STORAGE_MASTER_KEY=*** X_CONFIG_SECRET_KEY=*** \
  timeout 90 ./target/debug/cloudpivot
INFO cloudpivot::bootstrap: 启动引导完成 app=cloudpivot profile=test db_driver=postgres
INFO cloudpivot::bootstrap: ERP 单据查询器注册完成（P2-33）：flow 侧的终态扇出 / 内控闸 / 制单人查询改走注册表 count=16
INFO nik_web::app: cloudpivot 启动完成, 耗时 853ms
$ curl -s 127.0.0.1:18093/health
{"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"test",...}}
```

没有 `ERROR` / `FATAL` / `panic`：说明 16 个键的注册没有重复，注册表在服务对外之前就已填满。

### 4.2 红绿反证（三次，改完即还原）

| 故意改坏 | 结果 |
|:--|:--|
| `VoucherReverse.spec().table` 写成 `biz_voucher_TYPO` | 离线 `doc_spec_identifiers_exist_in_migrations` **FAILED**「迁移里找不到标识符 `biz_voucher_TYPO`」；真库 `doc_status_sql_executes_for_every_doc` **FAILED**「VoucherReverse（VOUCHER_REVERSE，表 biz_voucher_TYPO）的 doc_status 查询失败」 |
| `Expense.spec().creator` 改成 `create_by` | `expense_takes_its_creator_from_reporter_id` **FAILED**；真库 `expense_snapshot_uses_reporter_as_creator` **FAILED**「报销单的制单人必须是 reporter_id」 |
| `StockTakePost.biz_type()` 改成 `StockTransfer` | `supported_set_matches_governed_branches` **FAILED**「STOCK_TAKE_POST（StockTakePost）没有对应的实现」 |

## 5. 依赖边证据（卡面要求，如实说明）

卡面要求「贴出 `cargo tree` 里该依赖边已消失的证据」。**本卡做不到，也不该由本卡做**，
理由与 P2-31 / P2-32 同口径：

```text
$ cargo tree --prefix none --depth 1
cloudpivot v0.1.0 (/opt/cloudpivot/backend)
ammonia v4.2.0
async-trait v0.1.92 (proc-macro)
axum v0.8.9
cloudpivot-doc v0.1.0 (/opt/cloudpivot/backend/crates/cloudpivot-doc)
nik v0.4.2 (/opt/cloudpivot/backend/vendor/nik)
...
```

- **整个后端是单 crate**（workspace 化 = P2-C），`erp` / `flow` 是**模块**不是 package：
  `cargo tree` 里**没有** `business::erp` / `business::flow` 这两个节点，所以「这条边消失」
  在这个粒度上无法表达（这也是 P2-31 的结论，逐字沿用）。
- 模块级边用 `scripts/lint/module_cycle_report.py` 取证：

```text
$ python3 scripts/lint/module_cycle_report.py --from business::flow
business::flow 的出边：7 条
  → business::erp  (代码 131 / 注释 4)      ← 与 P2-31/P2-32 基线逐字一致（本卡未动）
$ （同一报告里）business::erp → business::flow (代码 135 / 注释 3)   ← 本卡 +3 行代码
```

- 本卡新增的 4 行 `crate::business::flow` 引用（3 行代码 + 1 行文档注释）**全部在新增的两个文件里**，
  方向是 `erp → flow`（**本来就有**的那半条边，134 行 → 138 行不是新边，只是加粗同向边）：

```text
$ grep -rn 'crate::business::flow' backend/app/src/business/erp --include=*.rs | grep -v doc_reader_impl | wc -l
134
$ grep -rn 'crate::business::flow' backend/app/src/business/erp/doc_reader_impl.rs \
    backend/app/src/business/erp/doc_reader_impl_tests.rs | wc -l
4
```

- **消边的可测证据在 P2-34**：`flow → erp` 现在的闸门是「只许降不许升」（`FLOW_TO_ERP_REFS_BASELINE = 135`）。
  本卡实测仍是 135（未升），P2-34 换完调用点后应把该常量下调到新实测值。

## 6. 给 P2-34 的交接

替换点与调用（P2-32 §5 的表格不变，只是「实现已就位」）：

```rust
// ① 终态扇出（handler.rs 4 处 × ~15 行 → 1 行）：注册的正是今天扇出的那 16 个回调
doc_reader::dispatch_settled(Some(instance_id), "WITHDRAWN").await;

// ② 内控闸（handler.rs 17 处 → 1 行）：等价性依赖「桥内部按 biz_type 自过滤」
doc_reader::guard_self_approval_of_code(&instance.biz_type, task_id, operator).await?;

// ③ 制单人 + governed（internal_control::of_biz 的 16 段 if/else → 2 行）
let creator = doc_reader::internal_control_creator_of_code(biz_type, biz_id).await?;
let governed = doc_reader::reader_of_code(biz_type).is_some();

// ④ 单据状态 / 单号（PDF、详情）
let snapshot = doc_reader::doc_status_of_code(biz_type, doc_id).await?;
```

**P2-34 必须保住的三条等价性**（本卡的用例是安全网）：

1. `governed` 判定从 16 段 `if` 换成 `reader_of_code(..).is_some()` —— 集合必须仍然**恰好**是
   这 16 个（`supported_set_matches_governed_branches` 双向对账）；
2. `STOCK_IN_CANCEL` 仍然**不**被拦（§2.4 缺口 1）；
3. `dispatch_settled` 调**全部**注册实现（不是按实例 biz_type 只调一个）—— P2-32 已定：历史实例
   `biz_type` 与回调不匹配时，今天靠「另一条桥捞回来」。

## 7. 改动文件

| 文件 | 改动 |
|:--|:--|
| `backend/app/src/business/erp/doc_reader_impl.rs` | **新**：16 个实现 + `DocSpec` 表 + `register_into` / `register_all` |
| `backend/app/src/business/erp/doc_reader_impl_tests.rs` | **新**：16 条用例（9 离线 + 6 真库 + 1 进程级注册） |
| `backend/app/src/business/erp/mod.rs` | +6 行挂载（`pub mod doc_reader_impl` + `#[cfg(test)] mod doc_reader_impl_tests`） |
| `backend/app/src/bootstrap.rs` | +38 行：`register_doc_readers()` + `bootstrap()` 里一行调用 |
| `backend/app/src/business/flow/doc_reader.rs` | +9 行：`#[cfg(test)] pub static GLOBAL_LOCK`（把「动全局注册表」的锁搬到注册表持有者这边，供 `flow` / `erp` 两个测试模块共用） |
| `backend/app/src/business/flow/doc_reader_tests.rs` | −2 / +1 行：改用上面那把共享锁，不再自己定义一份 |
