# SO-14 出库 → 应收挂账联动 —— 证据

卡：`t_23278334`【SO-14】出库 → 应收挂账联动
父卡：`t_b0fb1724`（SO-13 应收 API + 账龄）
本轮工作区：`/opt/cloudpivot`（99 工作仓），隔离库 `cloudpivot_so14`（引擎迁到 289）

---

## 一、交付物（改了哪些文件 / 函数）

| 文件 | 类型 | 内容 / 主要函数 |
|------|------|------------------|
| `backend/src/business/erp/receivable_bridge.rs` | **新增**（~1100 行，含单测） | 出库过账 → 应收挂账。`post_in_tx`（主入口）/ `receivable_of` / `taxed_amount_of` / `line_taxed_amount` / `out_source_of` / `shipment_customer_and_lines` / `credit_days_of` + 10 条单测（4 纯逻辑 + 6 真库） |
| `backend/src/business/erp/stockout_handler.rs` | 改 | `post_out_tx` 在 **⑤ 成本结转凭证之后、⑥ 联动之前** 插入 `receivable_bridge::post_in_tx(ex, id, operator.id, now)`；过账完成日志加 `receivable=AR-…` |
| `backend/src/business/erp/receivable.rs` | 改 | `insert` 拆出 `insert_with_reason(...)`（建单状态日志说明可定制，手工挂账仍写「手工挂账」，自动挂账写「销售出库过账自动挂账」）；新增常量 `CREATED_REASON_MANUAL` |
| `backend/src/business/erp/customer_model.rs` | 改 | `biz_customer.credit_days` 全链路：`CUSTOMER_COLUMNS` / `CustomerRow` / `CustomerWrite` / INSERT / UPDATE / `credit_days_allowed()` + 单测 |
| `backend/src/business/erp/customer_handler.rs` | 改 | `CreateCustomerReq` / `UpdateCustomerReq` / `CustomerRes` 加 `credit_days`；`require_credit_days()` 入口校验（负数 400/10001）；`merge_write` 部分更新沿用旧值 |
| `backend/src/business/erp/mod.rs` | 改 | 声明 `pub mod receivable_bridge;` + 模块文档表一行 |
| `backend/migrations/business/V0289__sales_customer_credit_days.{up,down}.sql` | **新增** | `biz_customer` 加 `credit_days INT NOT NULL DEFAULT 0` + `ck_biz_customer_credit_days (>= 0)`，末尾 DO 块自检（列型 / NOT NULL / 默认 0 / 插 -1 必被 23514 拒） |
| `docs/evidence/20261106-so14-receivable-bridge/curl-e2e.sh` | **新增** | 真实 token 端到端脚本（36 条断言，自带清场，可重放） |

**没有**新增 HTTP 接口、没有新增权限码 ⇒ `perm_menu_gate` 无需改动（`sys_menu.auth_code` 集合不变）。

---

## 二、事务边界（卡面「必须同一事务」怎么落的）

调用链一行不改库存写法，只是**在已有的过账事务里多插一步**：

```text
shipment_handler::change_shipment_status（POSTED）
  └─ db().begin()
      └─ shipment_posting::post_in_tx
          ├─ 建销售出库单（source_type='SALES', source_id=shipment.id，V0285）
          ├─ stockout_handler::post_out_in_tx
          │    ├─ ① 锁单头 / 期间闸 / 制单≠过账 / 状态机 CAS
          │    ├─ ② 三本账负向（成本账 / 库位账 / 流水，inventory_writer）
          │    ├─ ③ 单头金额 + 成本结转凭证（6401 / 1901 ← 1405）
          │    ├─ ④ ★ receivable_bridge::post_in_tx   ← 本卡
          │    └─ ⑤ 跨单据联动（排除库存/凭证四类动作）
          ├─ 回填 biz_shipment.stock_out_id / delivered_qty / 订单状态
      └─ commit（任何一步 Err → 整笔 rollback）
```

`post_in_tx` 自己**没有** `begin/commit/rollback`，吃的是调用方的 `&impl SqlExecutor`；
这一点由单测 `missing_customer_rolls_back_stock_and_receivable` 反向证明（见 §四）。

---

## 三、金额 / 期间 / 到期日的口径

| 项 | 口径 | 依据 |
|----|------|------|
| `amount`（含税） | Σ per-line `round(price × 发货数量, 4) + round(不含税金额 × tax_rate/100, 4)` | 直接复用 `sales_order_model::line_amounts`（订单落库同一函数）⇒ 全量发货时 Σ 应收 == 订单价税合计（单测 `full_shipment_total_equals_order_line_total` 钉死 4 组价格/税率） |
| 不用的口径 | ❌ `biz_stock_out.total_amount`（那是**成本**，借 6401）❌ 按订单行金额比例摊（多行尾差与订单对不上） | — |
| `source_type` / `source_no` / `source_id` | `SALES_OUT` / 出库单号 `OUT-yyyyMMdd-000001` / 出库单 id | 表注释（V0287）+ `uk_biz_ar_source` 幂等 |
| `period_code` | `voucher::period_of(out_date)`（过账日所属会计期间；查不到期间时退化为 `yyyy-MM`，与成本凭证同函数） | 卡面「period_code 取过账期间」 |
| `due_date` | `receivable::due_date_of(out_date, credit_days)` = 过账日 + 客户账期天数 | 卡面；`credit_days` 来自新增的 `biz_customer.credit_days`（V0289，0 = 现结） |
| `credit_days` | 挂账时**快照**进 `biz_receivable.credit_days`；客户主档后续改账期不回写历史应收 | curl 第 8 步实测 |

### 客户 `credit_days` 为什么加迁移（而不是硬编码 30 或按 0）

卡面写「due_date = 过账日 + 客户 credit_days」，而 `biz_customer` 只有 `credit_limit`。
`biz_supplier` 早在 V0137 就有 `credit_days INT NOT NULL DEFAULT 0`（0 = 现结），
所以本卡逐列镜像供应商加列 + CHECK，**不硬编码天数**。
默认值取 0（现结）而不是 30：给 30 会让所有既有客户凭空多出 30 天账期，账龄/逾期口径立刻失真。

---

## 四、单测（真库：`ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_so14`）

```
cargo test --lib --features test-hooks -- --test-threads=1 \
  business::erp::receivable_bridge business::erp::customer
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 2793 filtered out; finished in 10.61s
```

`receivable_bridge` 10 条：

| 卡面要求 | 用例 | 断言要点 |
|----------|------|----------|
| （纯逻辑）| `sales_out_source_type_matches_stockout_const` | `SALES_OUT`(应收来源) ≠ `SALES`(出库来源) ≠ `OTHER`(out_type)；与 `stockout_base` 常量逐字一致 |
| （纯逻辑）| `line_taxed_amount_adds_tax` / `taxed_amount_of_sums_rounded_lines` | 逐行定标后求和（`10×100@13%` = 1130；`3×33.3333@6%` 尾差 6.0000 吸附） |
| （纯逻辑）| `full_shipment_total_equals_order_line_total` | 4 组 (qty, price, rate) 全量发货 Σ 应收 == 订单行含税 |
| （纯逻辑）| `partial_shipment_uses_shipped_qty` | 部分发货按实发数量算（3/10 → 339） |
| **正常挂账** | `post_shipment_creates_receivable_in_same_transaction` | 过账后 AR：金额 1130 / OPEN / source_type=SALES_OUT / source_no=out_no / credit_days=30 / `due_date = out_date + 30` / `period_code == voucher::period_of(out_date)` / 建单日志说明「销售出库过账自动挂账」 |
| **正常挂账（现结）** | `zero_credit_days_means_due_on_posting_date` | 账期 0 → 到期日 = 过账日（不是硬编码 30） |
| **事务回滚一致** | `missing_customer_rolls_back_stock_and_receivable` | 客户不存在 → 过账 409，且**库存 20 不动 / 应收 0 张 / 发货单仍 PENDING_POST / 无孤儿出库单**（挂账发生在三本账减去之后，证明同事务） |
| **重复过账幂等** | `repeated_post_keeps_single_receivable` | 第二次过账 409；应收仍 1 张；库存只扣一次；直接重入 `post_in_tx` 返回 `created=false` |
| **判据是来源不是类型** | `manual_stock_out_does_not_create_receivable` | `source_type` 为空的领用出库照常扣库存（20→18）但**不挂应收** |

> ⚠ 不设 `ERP_DB_URL` 时真库用例打印 `SKIP` 并 return（libtest 记 passed，属假绿）——
> 上面 22 条是**带库**跑出来的。

---

## 五、真实 token 的端到端 curl（连跑两轮）

脚本：`docs/evidence/20261106-so14-receivable-bridge/curl-e2e.sh`
实例：`backend/target/debug/cloudpivot` @ `127.0.0.1:18117`，库 `cloudpivot_so14`，admin 真 token。

```bash
BASE=http://127.0.0.1:18117/api DB=cloudpivot_so14 bash docs/evidence/20261106-so14-receivable-bridge/curl-e2e.sh
```

```
第一轮：==> 汇总：PASS=36 FAIL=0   （rc=0）
第二轮：==> 汇总：PASS=36 FAIL=0   （rc=0）
```

链路与逐段实测（摘录第一轮）：

```
POST  /customer（credit_days=45）            → id 回显 credit_days=45；credit_days=-5 → 10001
POST  /sales-order（10×100@13%）             → total_amount=1000.0000 / total_with_tax=1130.0000
      夹具：UPDATE biz_sales_order SET status='APPROVED'（审批流不属本卡）
POST  /shipment                              → SH-20261106-00000x，total_qty=10.0000
PATCH /shipment/{id}/status PENDING_POST      → PENDING_POST
PATCH /shipment/{id}/status POSTED            → POSTED，stock_out_no=OUT-20261106-00000x
GET   /receivable/page?ar_no=OUT-…            → total=1
       金额 1130.0000 / source_type SALES_OUT / source_no = OUT-… / source_id = 出库单 id
       status OPEN / credit_days 45
GET   /receivable/{id}                        → 客户名「SO-14 挂账演示客户」/ unsettled 1130.0000
       due_date = 2026-12-21（出库日 2026-11-06 + 45）/ period_code = 2026-11
库层    biz_status_log(RECEIVABLE) 首条 reason = 销售出库过账自动挂账
       该出库单应收 1 张 / biz_inventory qty 20→10 / biz_voucher 1 张（成本结转）
幂等    重复 PATCH POSTED → code=10003；应收仍 1 张；库存仍 10
快照    PUT /customer {credit_days:30} → 30；已挂应收仍 credit_days=45、due_date 不变
```

> 脚本自带清场（只清 `SO14E2E%` 客户与其订单/发货/出库/应收/凭证/状态日志 + 夹具三张主数据的库存），
> 可重放；连跑多轮时脚本对 `10005 登录过于频繁` 自动退避重试。

---

## 六、迁移

```sql
-- schema_migrations（kind=versioned, applied_via=executed, success=t）
289 | sales_customer_credit_days | 289 | executed | t
```

启动日志（`scripts/verify/erp_clean_db.sh RECREATE=1` 建库时的 NOTICE）：

```
V0289：biz_customer.credit_days（账期天数，0=现结）已就位，坏数据被 CHECK 拒绝
```

列自检：

```
 column_name | data_type | is_nullable | column_default
 credit_days | integer   | NO          | 0
```

---

## 七、门禁

```
cargo fmt                                                      → rc=0
cargo clippy --all-targets --features test-hooks -- -D warnings → rc=0（0 告警）
cargo check --all-targets --features test-hooks                 → rc=0
前端                                                            → 本卡无前端改动（不涉及 pnpm）
```

全量回归：见 §八。

---

## 八、全量回归与归因

```
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so14' \
  cargo test --lib --features test-hooks -- --test-threads=1

test result: FAILED. 2813 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1402.07s
```

两条失败**都不是本卡引入**，且与 SO-13 记录的既有缺陷逐字相同（同一 `so_approve` 流程、同一对 hash）：

| # | 用例 | 现象 | 归因 / 处置 |
|---|------|------|-------------|
| 1 | `business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | `assert left "" == right "APPROVED"`（订单状态没推进） | **既有缺陷**：`so_approve`（V0281 建的流程）图与代码不同源。单跑同样失败（见下），SO-13 父卡验收时已记录同一现象；本卡 diff **不碰** `business/flow/*`、不碰任何 `biz_flow_*` 定义 |
| 2 | `business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | `flow_graph_selfcheck: drifted=1 … flow_code "so_approve" kind RowsEdited stored_hash da3c2eed… ≠ recomputed 97fe9377…` | 同上（同一流程的图快照漂移）；单跑同样失败 |

单跑证据（`--test-threads=1`，与全量同一库）：

```
cargo test --lib business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved
  → test result: FAILED. 0 passed; 1 failed   （left "" / right "APPROVED"）
cargo test --lib business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean
  → test result: FAILED. 0 passed; 1 failed   （stored da3c2eed… / recomputed 97fe9377…）
```

本卡范围内的复跑（改了 `stockout_handler` 文档注释后重编）：

```
cargo test --lib business::erp::receivable_bridge business::erp::customer business::erp::shipment_posting
  → test result: ok. 31 passed; 0 failed   （bridge 10 + customer 12 + 发货过账回归 9）
```

### 其它门禁

```
bash scripts/check.sh 的闸⑥（死权限码）
  NIK_DATABASE_NAME=cloudpivot_so14 … python3 scripts/verify/perm_menu_gate.py
  → PASS（本卡不新增接口 / 不新增权限码）

scope 闸②（scripts/lint/scope_gate.py）
  → 本卡的 7 处 SQL 全部带 // SCOPE-EXEMPT: <理由>
  → 脚本报的一条 FAIL 是**既有**问题：business/erp/shipment_page_fill_tests.rs（SO-09 卡 4c2c1b0 新增）
     未登记进 SKIP_MANIFEST，与本卡无关

git commit 的 pre-commit 钩子（库存三本账唯一写入口闸）
  → 扫描 392 非测试 .rs，「零越界：非测试代码里没有任何一处直写库存三本账」
```

---

## 九、明确没做的（边界，别当成漏了）

| 项 | 为什么不做 | 去处 |
|----|-----------|------|
| 出库单**作废** → 应收红冲/作废 | 应收状态域是 `OPEN/PARTIAL/SETTLED/WRITTEN_OFF`，`WRITTEN_OFF` 语义是坏账；作废需要先定语义（新增 CANCELLED 边表 vs 红字负金额单据，`ck_biz_ar_amount` 是 `amount > 0`） | 已开卡 `t_b33adc45`【SO-14 遗留】 |
| 收入确认凭证（借 1122 / 贷 6001） | 与 SO-13 同口径：属「收入/减值凭证」那组卡，手搓分录过不了 `voucher::post_voucher` 的科目映射 | SO-22 / 收入凭证卡 |
| 客户 FROZEN 拦截过账挂账 | 冻结语义是「禁开**新**单」，过账挂账是已审批单据的会计后果；过账因冻结失败会让「货已出、账挂不上」卡死。拦截应在建单入口（SO-09 守卫） | 有意不做，写在模块文档 |
| 客户档案页面的「账期天数」输入框 | 属前端 customer 页（避免与并行前端卡抢同一文件） | 已开卡 `t_df9ec5e1` |

