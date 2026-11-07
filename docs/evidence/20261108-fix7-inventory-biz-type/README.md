# FIX-7 库存流水口径：biz_type 记错 + 桶表对账口径统一（145 真机取证）

> 卡：t_6207df79 · 环境：145 `cloudpivot_dev` 的**迁移前拷贝库**（隔离实例 8048，8033 服役实例全程未动）
> 结论：**三个问题全部修掉**；E2E 35/35 断言全绿、幂等 12/12 全绿、真库用例 49+15 全绿。

## 0. 一句话结论（对照卡面验收）

| 验收项 | 结果 | 证据 |
|---|---|---|
| ① 销售出库后 `biz_inventory_txn.biz_type = SALES_OUT` | ✅ 真 HTTP 销售链实测 = **`SALE_OUT`**（不是 `OTHER_OUT`；词表里的正确拼写就是 `SALE_OUT`，卡面写的 `SALES_OUT` 是笔误） | `raw/e2e.txt` §5；`shipment_posting` 真库用例 |
| ② 按 `biz_type` 统计销售出库能统计到（数量对得上） | ✅ 统计前 18 + 本次 5 = 23，逐单核对 `SALE_OUT` 流水数量 = 发货 5 件 | `raw/e2e.txt` §5 |
| ③ 库存巡检脚本用正确口径，(物料,仓库) 聚合差异 = 0 | ✅ 差异行 0；**逐桶**（成本账 vs 库位账）差异行 0；流水自洽 0 异常；`erp_invariants --group 账账` exit=0（B1/B1b/B4 全绿） | `raw/e2e.txt` §6/§7、`raw/invariants_acct.txt` |
| ④ 无源入库单 `in_type = SOURCELESS` | ✅ 真 HTTP 无源入库单：建单/详情/库内三处都是 `SOURCELESS`，供应商仍必填 | `raw/e2e.txt` §3 |
| ⑤ 存量数据回填后口径一致 | ✅ 迁移把 4 条存量销售出库流水 `OTHER_OUT → SALE_OUT`、2 张存量无源单 `PURCHASE → SOURCELESS`，**没有迁移前后两套口径** | `raw/pre_migration_state.txt`、`raw/e2e.txt` §1、`raw/idempotency.txt` |

## 1. 改了什么（问题 → 修法）

### 问题 1：销售出库记成 `OTHER_OUT`（P3）

* **根因**：销售出库单的 `out_type` 固定 `OTHER`（四值 ISSUE/SCRAP/LOSS/OTHER 里没有「销售」），
  来源由 `source_type='SALES'` 表达（V0285）。过账只按 `out_type` 取 `biz_type` ⇒ 销售出库与手工
  「其他出库」共用一个值，按 `biz_type` 统计销售出库直接归零。
* **修法**：`stockout_base::biz_type_of_doc(out_type, source_type)` 作为**唯一取 biz_type 的入口**，
  `source_type='SALES'` → `TxnBizType::SaleOut`（`SALE_OUT`）。
  **所有常量改从词表 `doc_kind.rs` 派生**（`TxnBizType::*.as_str()`），不再手写字面量：
  `BIZ_TYPE_ISSUE/SCRAP/LOSS/OTHER/SALE_OUT/STOCK_OUT_CANCEL`、`DOC_TYPE_STOCK_OUT`、`OUT_TYPES`。
* **连带**：作废前完整性闸 `stockout_model::txn_report` 的 `biz_type IN (...)` 列表改成从
  `OUT_TXN_BIZ_TYPES`（5 值，含 `SALE_OUT`）动态拼 —— 漏一个值就等于「作废看不见那条流水」。
* **存量回填**：V0330（判据 `biz_stock_out.source_type='SALES'` + `biz_type='OTHER_OUT'`，
  手工其他出库不受影响）。
* **其他出库类型扫查结果**：调拨 `TRANSFER_OUT/IN` ✅、盘点 `STOCK_TAKE_GAIN/LOSS` ✅、
  领用 `ISSUE_OUT` ✅、报废 `SCRAP_OUT` ✅、盘亏 `LOSS_OUT` ✅ —— 都没有同类错记。
  （另发现一处**不在本卡范围**的口径缺口，见 §6。）

### 问题 2：库存是「桶」表、流水无库位列（P3）

* **口径统一到文档**：新增 `docs/ERP-库存对账口径.md`（唯一口径）——
  库存 = 流水只能按 `(物料, 仓库)` 聚合比；逐桶只能比「成本账 vs 库位账」且必须按桶键 FULL JOIN；
  方向列是 `txn_type`（不是 `direction`）；盘点差异是 `STOCK_TAKE_GAIN/LOSS`（不是 `ADJUST`）。
* **写进巡检脚本**：`scripts/verify/erp_invariants.py`
  * 新增 **B1b 逐桶 成本账 = 库位账**（`(物料, 仓库, 库位字典)` FULL JOIN，WARN 级）；
  * B4 补「流水无库位列 ⇒ 只能按 (物料,仓库) 聚合」的口径注释；B1/B4 原本就是正确口径，未改 SQL。
* **P0-03 evidence 文档**：`09_reconcile.sql` / `13_final_reconcile.sql` **本来就已经是正确口径**
  （上一卡 P0-03 自己发现并改正了卡面 SQL），本次在 README 里补了指向唯一口径文档的说明，
  并把问题 #2 / #5 标成「已修（FIX-7）」。

### 问题 3：无源入库单 `in_type` 仍是 `PURCHASE`（P4）

* **修法（选「新增 SOURCELESS」）**：`is_sourceless=true` 落 `in_type='SOURCELESS'`。
  为什么不选 `OTHER`：`ck_biz_si_supplier_scope` 钉死「非 PURCHASE 不能带供应商」，而**无源单必须带供应商**
  （无源 = 没有采购单，不是没有供货方）。选 `OTHER` 就得先把这条两向约束拆掉；
  选 `SOURCELESS` 只加宽枚举并让该值同样要求供应商，语义与约束都自洽。
* 词表同步：`DocSubtype::Sourceless`（`doc_kind.rs`）+ `IN_TYPES` 4 值 +
  前端 `doc-kinds.ts` 镜像 + `form-field-labels.ts` 文案「无源入库」（后端单测钉死两边逐字一致）。
* 代码入口唯一：`stockin_base::in_type_of(is_sourceless)`，建单/改单都走它；
  `serial_source_of_in_type(SOURCELESS) = None`（不冒充采购来源）。

### 迁移 V0330（只追加、幂等）

`backend/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.{up,down}.sql`

1. `biz_inventory_txn`：SALES 单的 `OTHER_OUT → SALE_OUT`；
2. `biz_stock_in`：加宽 `ck_biz_si_type`（+`SOURCELESS`），换 `ck_biz_si_supplier_scope`
   （`PURCHASE`/`SOURCELESS` 必须有供应商，其余必须没有），存量无源单 `PURCHASE → SOURCELESS`；
3. 迁移内自检（失败即启动失败）：无残留 `OTHER_OUT` 销售流水、无「无源 + PURCHASE」、
   两条约束定义文本含 `SOURCELESS`、`in_type` 全域在词表内。

## 2. 改了哪些文件（+行数）

| 文件 | 行数 | 说明 |
|---|---|---|
| `backend/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.up.sql` | +125 | 回填 + 约束 + 自检 |
| `backend/migrations/business/V0330__inventory_biz_type_and_sourceless_in_type.down.sql` | +72 | 回滚 + 反向自检 |
| `backend/src/business/erp/stockout_base.rs` | +117/−? | 常量改从词表派生、`biz_type_of_doc`、`OUT_TXN_BIZ_TYPES`、3 条单测 |
| `backend/src/business/erp/stockout_model.rs` | +33/−? | `txn_report` 的 biz_type 列表动态取（含 SALE_OUT） |
| `backend/src/business/erp/stockout_handler.rs` | +5/−1 | 过账取 biz_type 走 `biz_type_of_doc` |
| `backend/src/business/erp/stockin_base.rs` | +79/−? | `IN_TYPE_*` 常量、`in_type_of`、3 条单测 |
| `backend/src/business/erp/stockin_write.rs` | +9/−? | 建单/改单写 `in_type_of(is_sourceless)` |
| `backend/src/business/erp/shipment_posting.rs` | +19 | 真库用例断言销售出库流水 `SALE_OUT` |
| `backend/src/business/erp/stockin_handler_tests_a4.rs` | +43/−? | 无源单断言 `SOURCELESS`、同型单断言 `PURCHASE`、约束两向探针 |
| `backend/src/business/shared/doc_kind.rs` | +19/−? | `DocSubtype::Sourceless` + `IN_TYPES`/`IN_TYPE_VALUES` 4 值 |
| `frontend/src/api/business/erp/doc-kinds.ts` | +10/−? | 前端镜像 `IN_TYPES`/`IN_TYPE_LABELS` 加 `SOURCELESS` |
| `frontend/src/views/business/flow/form-field-labels.ts` | +2 | 流程表单枚举文案「无源入库」 |
| `scripts/verify/erp_invariants.py` | +55 | 新增 B1b 逐桶 + B4 口径注释 |
| `docs/ERP-库存对账口径.md` | +192 | 唯一口径文档（新增） |
| `docs/README.md` | +1 | 文档地图加一行 |
| `docs/evidence/20261107-p003-inventory-chain/README.md` | +10/−? | 口径指向 + 问题 #2/#5 标已修 |
| `docs/evidence/20261108-fix7-inventory-biz-type/*` | +707 | 本卡取证脚本（00/10/11/12/13/13b/14/21） |

## 3. 测试结果

### 3.1 后端（145 拷贝库，`ERP_DB_URL` 指向 `cloudpivot_fix7_*`）

```
cargo test --lib -- shipment_posting::tests stockin_handler_tests_a4 stockin_base::tests
                    stockout_base::tests doc_kind            → 49 passed; 0 failed
cargo test --lib -- stockout_model::tests stockin_model::tests → 15 passed; 0 failed
```

含新增断言：`shipment_posting::tests::normal_post_deducts_three_books_and_closes_order`
（销售出库流水 `biz_type = SALE_OUT`）、`a4_06`（无源单 `in_type=SOURCELESS`）、
`a4_07`（`SOURCELESS + 供应商` 允许 / `SOURCELESS` 无供应商被 DB 拒）、
`stockout_base::tests::*` / `stockin_base::tests::*`（词表与映射）。
原始输出：`raw/db_tests.txt`、`raw/db_tests_stockout_stockin.txt`。

**未做**：全量 `cargo test` 没跑绿 —— 与本卡无关的两类既有红（种子缺失库 / 共享库历史不平数据，
FIX-6 已建卡 t_13ddcb84），本卡只跑触及模块（全绿）。`cargo check --all-targets` 通过，
`cargo clippy --all-targets` 只剩 5 处**既有**告警（`sales_order_model.rs:699`、`asset_linkage.rs:423`、
`fx_revaluation.rs:788`、`opening.rs:2738`、`year_end_linkage_tests.rs:214`，均不在本卡文件）。

### 3.2 前端

* `doc-kinds.ts` 改动由后端单测 `doc_kind_tests::frontend_ts_mirror_matches_rust` /
  `frontend_ts_labels_match_rust` 钉住（49 条用例里包含，全绿）。
* `vite build`：**EXIT=0**（`NODE_OPTIONS=--max-old-space-size=6144 npx vite build`，dist 产出 169 个 chunk）。
* `pnpm typecheck` 仍红，但**不是本卡文件**：`src/api/business/erp/price-inquiry.ts(363)`、
  `src/views/business/erp/price-inquiry/index.vue(159/721)` —— 他人未跟踪的价审模块（FIX-6 报告里已记录同类红），
  本卡改动的 4 个前端文件 0 报错。`pnpm build`（= `vue-tsc --noEmit && vite build`）因此卡在 typecheck 阶段。

## 4. 145 实测输出（HTTP + DB）

环境：拷贝库 `cloudpivot_fix7_20261108014720`（迁移前 schema 329 → 启动应用 1 个迁移 V0330），
隔离实例 `127.0.0.1:8048`；`cloudpivot-test`(8033) 全程未动，收尾复核 `health=200`、`systemctl is-active` = active。

| 步骤 | HTTP | DB 断言 |
|---|---|---|
| 无源入库建单（`is_sourceless=true` + 供应商 + remark，100@9） | 200 | `biz_stock_in.in_type='SOURCELESS'`、`is_sourceless=t`、供应商在、金额 900 |
| 无源入库提交过账 | 200 | 状态 `POSTED`；流水 `PURCHASE_IN`（见 §6 已知口径） |
| 手工领用出库 30 件 + 过账 | 200/200 | 流水 `ISSUE_OUT`（回归：非销售路径没被波及） |
| 发货单（APPROVED 订单夹具）建单 → `PENDING_POST` → `POSTED` | 200/200/200 | 生成 `OUT-20261108-000005`：`out_type=OTHER` + `source_type=SALES`；流水 **`SALE_OUT`** 5 件；`SALE_OUT` 统计 18 → 23 |
| 对账 SQL（(物料,仓库) 聚合 库存 vs 流水） | — | 差异行 0（与迁移前基线一致） |
| 对账 SQL（逐桶 成本账 vs 库位账，桶键 FULL JOIN） | — | 差异行 0 |
| 流水自洽 `after_qty-before_qty = ±qty` | — | 异常 0 |
| 金额口径（库存金额 vs 流水金额） | — | 23804.4363 = 23804.4363，diff 0 |
| `erp_invariants.py --group 账账` | — | exit=0；B1 ✅ / **B1b ✅（新）** / B4 ✅ |
| `biz_type_domain.py` | — | `biz_inventory_txn.biz_type` **PASS**（8 种，含 `SALE_OUT`）、`biz_stock_in.in_type` **PASS**（含 `SOURCELESS`）；另有 2 列 `SO_APPROVE` 域外值 → 见 §6 |

原始输出：`raw/e2e.txt`（35 ✅ / 0 ❌ / `RESULT: ALL PASS`）、`raw/invariants_acct.txt`、`raw/biz_type_domain.txt`。

存量回填实测（`raw/pre_migration_state.txt` → `raw/e2e.txt` §1）：

```
迁移前：sales_out_docs=2  sales_txn_other_out=4  sales_txn_sale_out=0
        manual_other_out_txn=0  sourceless_docs=2  sourceless_purchase=2
迁移后：sales OTHER_OUT=0   SALE_OUT=4（全部迁到）  手工 OTHER_OUT=0（未误伤）
        「无源 + PURCHASE」=0   SOURCELESS=2（全部迁到）  SOURCELESS 无供应商=0
```

## 5. 幂等验证（`raw/idempotency.txt`，12 ✅ / 0 ❌）

1. **同库再启**：`数据库迁移完成: 应用 0 个迁移`，`schema_migrations` 指纹 `264|f51f0a8f…` 前后一致；
2. **V0330 up 直接重跑两遍**：两遍都是 `UPDATE 0` + `UPDATE 0`（数据不会被二次迁移），
   约束 `DROP IF EXISTS + ADD` 可重入；
3. **down → up 往返**：down 后 `SOURCELESS` 归零、销售流水回到 `OTHER_OUT`（4 条），
   再 up 后无源单重新变 `SOURCELESS`（证明回滚可用且可再前进）；
4. **空库首启**：空库应用 261 个迁移（含 V0330），`schema_migrations` 有 V0330、
   `ck_biz_si_type` 含 `SOURCELESS`，admin 可登录。

> 说明：幂等脚本用**没跑过单测的干净拷贝库**（`13b_prepare_idem_db.sh` 另建）。
> 原因：`stockout_tests` 的清场口径是 `DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-%'`（模块历史遗留的宽删），
> 会把存量销售出库单一起删掉 —— 之后做 down/up 往返时「流水找不到单据」，观察到的 0 条是清场造成的，不是迁移问题（第一次跑就踩到了）。

## 6. 已知问题 / 未做项（都不在本卡范围，如实登记）

1. **无源入库的库存流水仍记 `PURCHASE_IN`**：本次只把**单据侧** `in_type` 改成 `SOURCELESS`；
   `biz_inventory_txn.biz_type` 仍写 `PURCHASE_IN`（`stockin_write::write_txn` 没有 biz_type 入参）。
   于是「按 `biz_type` 统计采购入库」仍会把无源单算进去 —— 报表要么按 `in_type` 区分，
   要么另开卡给流水轴加 `SOURCELESS_IN`（要同步 `TxnBizType` / 前端镜像 / E4 登记表 / 凭证规则并集，
   属词表扩容，不宜塞进本卡）。
2. **`SALES_RETURN_IN` 不在 `TxnBizType` 词表里**（`sales_return_posting.rs` 自写常量）：
   本卡扫描其他出库类型时发现；`biz_type_domain.py` 会在有销售退货流水的库上报域外值
   （145 拷贝库此刻 0 行，所以没红）。属同族缺口，建议单独卡处理（同样要动词表 + 前端镜像）。
3. **`SO_APPROVE` 不在 `FlowBizType` 词表**（审批轴，不是库存轴）：`biz_type_domain.py` 在 145 拷贝库
   报 2 列 FAIL（`biz_flow_def.biz_type` ×1、`biz_flow_instance.biz_type` ×2），是本卡之前就有的缺口，
   与本次改动无关。
4. **全量 `cargo test` 未跑绿**：理由见 §3.1（种子缺失 / 共享库历史不平数据两类既有红）。
5. **前端 `pnpm typecheck`/`pnpm build` 被他人未跟踪的 price-inquiry 三处 TS 错误挡住**（§3.2）。
6. 145 的服役实例（8033 → `cloudpivot_dev`）**没动**：V0330 会在下一次部署该提交时应用；
   本卡全部取证在拷贝库上完成。

## 7. 复现脚本（145 上按顺序跑）

```bash
bash docs/evidence/20261108-fix7-inventory-biz-type/10_prepare_145.sh   # 拉代码 + 建二进制 + 备份 + 拷贝库 + 迁移前快照
bash docs/evidence/20261108-fix7-inventory-biz-type/11_start_instance.sh # 8048 起隔离实例（应用 V0330）
bash docs/evidence/20261108-fix7-inventory-biz-type/21_e2e.sh           # 真 HTTP + DB 断言（35 项）
bash docs/evidence/20261108-fix7-inventory-biz-type/12_run_db_tests.sh  # 真库用例（49 项）
# 幂等验证要一份没跑过单测的干净拷贝库：
bash docs/evidence/20261108-fix7-inventory-biz-type/13b_prepare_idem_db.sh
bash docs/evidence/20261108-fix7-inventory-biz-type/11_start_instance.sh
bash docs/evidence/20261108-fix7-inventory-biz-type/13_idempotency.sh   # 幂等 + 空库首启（12 项）
bash docs/evidence/20261108-fix7-inventory-biz-type/14_kill_instance.sh # 收尾（8033 复核）
```

## 8. commit

* `c8307e6` — 功能：SALE_OUT / SOURCELESS / V0330 / 口径文档 / 巡检 B1b / 单测
* `3ef63fe`、`a4e339a`、`e6b…`（取证脚本修正若干）— 均在 `docs/evidence/20261108-fix7-inventory-biz-type/`
* 最终 HEAD 见卡内汇报的 commit hash（含本 README 与 raw 证据）
