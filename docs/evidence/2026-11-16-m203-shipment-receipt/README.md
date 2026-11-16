# M2-03 签收确认 → 在途转已出库 · 证据

- 卡：`t_5f324a4c`（M2-03【M2·仓库】在途库存：签收确认 → 在途转已出库）
- 上游：`t_6c090f97`（M2-02 发货过账 → 在途 + 出库待确认，commit `fba8f06e`）
- 下游：`t_f1bce846`（M2-04 在途库存查询接口）
- 执行环境：99（开发机）→ PG `172.18.10.145:5432`，模板库 `cloudpivot_test`
- 结论：**19/19 演练断言全绿**（`00_drill.out`），`cargo fmt --check` / `clippy --lib` / `clippy --all-targets`（均 `-D warnings`）/ `check.sh --l0` 全通过

## 一、卡面口径与实现

卡面两句话：

1. 「签收确认 → 在途转已出库」
2. 「签收后扣库存、清在途」

落地成一个**新入口 + 一个同一收口**：

```text
POST /business/erp/shipment/{id}/receive        ← 新接口（权限码复用 business:erp:shipment:status）
  └─ 同一事务
       ① post_out_in_tx(出库单)  = PENDING_POST → POSTED → 三本账负向 + OUT 流水
                                  + 成本结转凭证 + 挂应收（M2-02 已铺好、当时无调用方）
       ② post_out_tx 的 ⑤c       = 在途账 received_qty → shipped_qty / status=RECEIVED
                                  / received_at（一条 UPDATE，V0420 的 CHECK 咬合）

出库单页面的「过账」按钮（PATCH /business/erp/stock-out/{id}/status {POSTED}）
  └─ 走同一个 post_out_tx ⇒ ⑤c 一样执行（两个入口不会一边扣库存一边把在途挂着）
```

**零迁移**：表在 M2-01 建好（`biz_inventory_in_transit`），`PENDING_POST` 是既有状态值，
权限码复用 `business:erp:shipment:status`（签发/过账同为发货单的状态型动作，且该码已在
`sys_menu` 有承载行 —— 新建码会连带一条菜单迁移，属本卡范围外）。**零前端改动**（M2-07 才做
在途页面；当前用户可用的是「销售出库 → 过账」按钮，效果完全一致）。

## 二、为什么在途清零收口在 `post_out_tx`（而不是只在新接口里）

M2-02 之后，发货过账把出库单留在 `PENDING_POST`（= 出库待确认）。这张单在**出库单页面**
是可以被「过账」的（合法边 `PENDING_POST → POSTED`）。如果只在新的「签收确认」接口里清在途，
那么从出库单页面点过账的那条路就会「扣了库存、在途还挂着」—— 而 M2-02 的可用量口径是
`可用 = 账面 − 未签收在途`，在途挂着一辈子，这批货就永远发不出去（只能改库）。

所以在途清零跟着**动账点**走：

| 不变量 | 库层复核（`00_drill.out` §3.4/3.5） |
|---|---|
| 出库单已过账 ⇒ 该发货单没有未签收在途 | 0 行 |
| 出库单待确认且有在途行 ⇒ 必有一行未签收 | 0 行 |

两条都是 0 行才算过 —— 前者防「扣了库存挂账在途」，后者防「清了在途没扣库存」。

## 三、改动文件

| 文件 | 改动 |
|:--|:--|
| `backend/app/src/business/erp/shipment/shipment_transit.rs` | **新增**：`OpenLine` / `ReceiptLine` / `lock_open`（`FOR UPDATE`）/ `open_lines`（只读闸）/ `receive_open`（一条 UPDATE 推进 `received_qty`+`status`+`received_at`，CAS 到 `status='IN_TRANSIT'`） |
| `backend/app/src/business/erp/shipment/shipment_handler.rs` | **新增** `confirm_receipt`（三道前置闸 + 同一事务过账 + commit 后延迟联动）+ 路由 `POST /business/erp/shipment/{id}/receive` + DTO `ShipmentReceiveReq`；模块文档补「签收确认」段与路由表行 |
| `backend/app/src/business/erp/shipment/shipment_posting.rs` | **新增** `receipt_reason`（「客户签收确认 SH-xxx」，进状态日志 + 在途备注） |
| `backend/app/src/business/erp/stockout/stockout_handler.rs` | **新增** `post_out_tx` 的 ⑤c：`source_type='SALES'` 且带 `source_id` → `receive_open` 清在途（同一事务）；`post_out_in_tx` / `run_deferred_linkage_after_commit` 去掉 `#[allow(dead_code)]`（M2-02 留的接线点已接上） |
| `backend/app/src/business/erp/shipment/shipment_receipt_tests.rs` | **新增**：5 条真库用例 + 2 条纯逻辑用例（主数据走 `crate::test_seed::ensure_*`） |
| `backend/app/src/business/erp/shipment/mod.rs` | 登记测试模块 + 文档口径 |
| `scripts/verify/m203_shipment_receipt_drill.sh` | **新增**：克隆模板库 → 补 V0405…V0420 → 本卡用例 → SQL 形态复核 → 回归组（19 断言） |
| `scripts/verify/e2e02_sales_a.sh` | step4 补「签收确认」步骤（可重入）：M2-02 之后库存/流水/应收断言只有签收后才为真 |
| `docs/evidence/2026-11-16-m203-shipment-receipt/` | 本证据目录 |

## 四、实测（真实工具输出）

```bash
cd backend && cargo fmt -- --check                       # 0 diff
cd app && cargo clippy --lib -- -D warnings              # 0 warning
cargo clippy --all-targets -- -D warnings                # 0 warning（含测试目标）
cd /opt/cloudpivot && SKIP_METRICS_CHECK=1 bash scripts/check.sh --l0   # 全部检查通过（L0）
bash scripts/verify/m203_shipment_receipt_drill.sh       # PASS=19 FAIL=0 / ALL PASS
```

| 组 | 断言数 | 说明 |
|:--|:--:|:--|
| 模板克隆 + 补迁移 | 2 | 16 条业务迁移（V0405…V0420）逐条 `ON_ERROR_STOP`；在途账表就位 |
| `shipment_receipt_tests` | 7 | 本卡主证据（5 真库 + 2 纯逻辑） |
| SQL 形态复核 | 10 | 生成列恒等 / 对账恒等式 / 状态×数量咬合 / `RECEIVED` 必有 `received_at` / **两条不变量** / 正向计数 3 条 / 流水净额 = 出库总量 |
| 回归组 | 4 | `shipment_posting` 11 / `shipment_transit` 4 / `stockout` 60 / `receivable_bridge` 13 全绿 |

本卡用例逐条覆盖：

| 用例 | 断言要点 |
|:--|:--|
| `receipt_clears_transit_and_posts_stock_out` | 签收前：库存 20 不动 / 在途 6 / 出库单 `PENDING_POST` / 无凭证无应收。签收后：在途行 `6.0000/6.0000/0.0000/RECEIVED/true`、成本账与库位账 14、OUT 流水 −6、出库单 `POSTED` + `posted_at`、凭证 1 张、应收 1 条、状态日志写明「客户签收确认 SH-…」 |
| `repeat_receipt_is_rejected_at_gate` | 第二次 409；库存 16 不再减、流水不写第二遍、凭证/应收各 1 |
| `receipt_requires_posted_shipment` | `PENDING_POST` 发货单签收 → 409「…请先过账…」；库存/在途/出库单全不动 |
| `posting_from_stock_out_page_also_clears_transit` | 从出库单入口过账 ⇒ 在途清零；随后再点签收 409、库存不再减 |
| `receipt_failure_leaves_transit_open_and_retryable` | 清空三本账造负库存 → 签收 409；在途仍 `6/0/6/IN_TRANSIT`、出库单仍 `PENDING_POST`、无凭证无应收；补货后**同一接口重试成功** |

## 五、部分签收：本卡的口径与理由（不是漏做）

出库单是按发货单**整单**生成的（`biz_stock_out.total_qty` 固定），库存扣减以出库单为单位。
部分签收意味着「同一张出库单只过账一部分」，必须先按签收量拆单（另立单据）—— 属退货/拒收链。
本卡口径：**整单签收**（全签才动账），部分到货走销售退货把差异量退回。

库层已经准备好了：`received_qty` 是累加列、`qty_in_transit` 是生成列、`ck_biz_inv_transit_state`
允许「部分签收仍 IN_TRANSIT」（V0420 的探针 3.7j 就是这么测的）。后续要放开，只改
`confirm_receipt` 的入参与拆单逻辑，不需要迁移。

## 六、诚实的缺口

1. **未做 HTTP 端到端**（登录 → 打新接口）：99 上无运行中的实例、`/etc/cloudpivot/cloudpivot-dev.env`
   不存在（dev 配置要求 redis 缓存），本卡未启隔离实例。路由可用性的证据是：编译期宏注解 +
   `system::menu::permission_audit_tests`（N16「每条路由都有权限闸」）22 条全绿。
2. **`scripts/verify/e2e02_sales_a.sh` 已补签收步骤但未实跑**（同上：需要运行中的实例）。
   本卡演练用 `shipment_receipt_drill` 覆盖了同一组语义（库存扣减 / 出库流水 / 应收挂账）。
3. **签收备注（`remark`）只落在在途行**，不进 `biz_status_log`；状态日志的 reason 固定为
   「客户签收确认 SH-…」。要带签收单号/到货状况可走 remark（本卡已落）。
4. 出库单作废（`PENDING_POST → CANCELLED`）时**不释放**在途行（应写 `CANCELLED`）——
   M2-02 未做、本卡按「只做签收」不夹带；已在本卡评论里点名交接（见下）。
5. 模板库 `cloudpivot_test` 停在 V0404，演练脚本自补 V0405…V0420；**不是**本卡引入的迁移。
6. 基线红（与本卡无关）：`business::erp::voucher::tests::account_mapping_uses_voucher_rule`
   —— 模板库无 `P2C1.1405` 科目映射规则行（M2-02 已在干净克隆复现）。
7. ERP 真库用例必须 `--test-threads=1`（跨模块共享夹具会互踩）。
