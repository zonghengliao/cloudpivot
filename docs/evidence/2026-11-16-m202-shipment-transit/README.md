# M2-02 发货过账 → 在途 + 出库待确认 · 证据

- 卡：`t_6c090f97`（M2-02【M2·仓库】在途库存：发货单过账 → 在途 + 出库待确认）
- 上游：`t_7ba5b0e2`（M2-01 建表 `biz_inventory_in_transit`，V0420）
- 下游：`t_5f324a4c`（M2-03 签收确认 → 在途转已出库）
- 执行环境：99（开发机）→ PG `172.18.10.145:5432`，模板库 `cloudpivot_test`
- 结论：**14/14 演练断言全绿**（`00_drill.out`），`cargo fmt` / `clippy --lib -D warnings` / `check.sh --l0` 全通过

## 一、卡面口径与本卡实现

卡面两句话：
1. 「发货单过账 → 在途 + 出库待确认」
2. 「发货时数量进在途，库存不扣」

落地成**两段式**（这是本卡唯一的语义变化，也是 M2 批次的既定切法）：

```text
发货过账（本卡，shipment_posting::post_effects）
  ① 在途账 biz_inventory_in_transit：一行 = 物料 × 发货仓 × 发货单，
     shipped_qty 按物料汇总 upsert（唯一键 uk_biz_inv_transit_key 兜住）
  ② 生成销售出库单 biz_stock_out（source_type='SALES'）→ 推进到 PENDING_POST
     = 「出库待确认」（状态日志写明「待客户签收确认后过账出库」）
  ③ 三本账 / 成本结转凭证 / 应收挂账**一律不动**（库存不扣）
  ④ 回填 biz_shipment.stock_out_id → 订单行 delivered_qty → 订单 PARTIAL_OUT/OUTBOUND

签收确认（M2-03，下一张卡）
  ⑤ 在途账 received_qty += n（一条 UPDATE 同时推 status，V0420 的 CHECK 咬合）
  ⑥ 出库单 PENDING_POST → POSTED：三本账负向 + 成本结转凭证 + 挂应收
```

「出库待确认」用**已有的** `biz_stock_out.status = PENDING_POST`（V0203 的状态域里本来就有，
语义就是「已提交、账没动、等过账」），**零迁移、零新增状态值、零新权限码、零新接口**。

## 二、为什么要「在途占住可用量」（本卡的第二个必做项）

库存不再随发货扣减 ⇒ 账面一直是满的 ⇒ 不处理就能把同一批货反复发出去。
所以过账前的库存闸 `shipment_model::stock_shortfalls` 改成：

```text
可用量 = 账面(fifo_plan.available) − 该仓该物料未签收的在途量(status='IN_TRANSIT')
        且按**物料**合并本单所有行后判定一次（逐行判会让「两行各 6 / 账面 10」双过）
```

证据：`shipment_posting::tests::in_transit_reserves_available_quantity`
（20 库存 → 发 15 通过 → 再发 10 被 409 拦下（可用只剩 5）→ 发 5 边界通过）。

## 三、改动文件

| 文件 | 改动 |
|:--|:--|
| `backend/app/src/business/erp/shipment/shipment_transit.rs` | **新增**：在途账写入口（`merge_by_material` / `record_shipped` upsert / `open_qty` 取未签收量）+ 4 条单测 |
| `backend/app/src/business/erp/shipment/shipment_posting.rs` | 过账效果改写：建出库单 → 落 `PENDING_POST` → 写在途账 → 回填/回写；**不再**过账出库单；5 条纯逻辑 + 6 条真库用例重写 |
| `backend/app/src/business/erp/shipment/shipment_handler.rs` | 提交后日志改为「在途 + 出库待确认」；不再跑出库过账的延迟联动；库存闸文案带上「账面 / 在途未签收 / 缺口」 |
| `backend/app/src/business/erp/shipment/shipment_model.rs` | `StockShortfall` 增加 `on_hand` / `in_transit`；`stock_shortfalls` 按物料合并 + 扣在途 |
| `backend/app/src/business/erp/shipment/mod.rs` | 登记新模块 + 文档口径 |
| `backend/app/src/business/erp/stockout/stockout_handler.rs` | `post_out_in_tx` / `run_deferred_linkage_after_commit` 加 `#[allow(dead_code)]` + 注释（M2-03 签收确认是下一个调用点） |
| `backend/app/src/business/erp/receivable/receivable_bridge.rs` | 下游适配：测试夹具「过账」= 发货过账 + 出库确认；`missing_customer` 用例改为「出库确认失败 ⇒ 账不动、单据可重试」 |
| `scripts/verify/m202_shipment_transit_drill.sh` | **新增**：克隆模板库 → 补 V0405…V0420 → 跑 4 组真库用例 → SQL 复核落库形态（14 条断言） |
| `docs/evidence/2026-11-16-m202-shipment-transit/` | 本证据目录 |

**零迁移**：本卡不动 schema（表在 M2-01 已建；`PENDING_POST` 是既有状态值）。

## 四、实测（真实工具输出）

```bash
# 1) 静态门禁
cd backend && cargo fmt --check                     # 0 diff
cargo clippy --lib -- -D warnings                    # 0 warning（1m00s）
cd .. && SKIP_METRICS_CHECK=1 bash scripts/check.sh --l0   # 全部检查通过（L0）

# 2) 真库演练（脚本自建库、自删库）
bash scripts/verify/m202_shipment_transit_drill.sh
#   == 汇总：PASS=14 FAIL=0 ==
#   ALL PASS
```

演练覆盖（`00_drill.out` 原文）：

| 步骤 | 断言 |
|:--|:--|
| 模板克隆 + 补迁移 | 16 条业务迁移（V0405…V0420）逐条 `ON_ERROR_STOP` 通过；在途账表就位；`biz_voucher.source_no` 已 VARCHAR(64) |
| `shipment_posting`（11 条） | 发货过账写在途 / 出库单 `PENDING_POST` / **三本账一分不动** / 无凭证 / 部分发货两行在途 / 库存不足全回滚 / 在途占用可用量 / 重复过账幂等 / 后置失败整笔回滚 |
| `shipment_transit`（4 条） | 物料合并顺序 / 空明细 / 状态域与 V0420 CHECK 逐字一致 / 日期 ISO |
| `shipment_model`（14 条） | 含 `stock_shortfalls` 在途扣减后的口径 |
| `receivable_bridge`（13 条） | 出库确认才挂应收、作废冲销、已收款拦作废、幂等 |
| SQL 复核（5 条） | 生成列恒等 / 无孤儿在途行 / 状态×数量咬合 / `PENDING_POST` 无 `posted_at` / `PENDING_POST` 无成本结转凭证 |

## 五、本卡引起的下游口径变化（**只报不改**，各归其卡）

| 变化 | 影响 | 归属 |
|:--|:--|:--|
| 发货过账不再扣三本账、不再生成成本结转凭证、不再挂应收 | `scripts/verify/e2e02_sales_a.sh` 的「库存 200→190」「出库流水」「自动挂应收」断言在签收确认（M2-03）落地前会红 | M2-03 补「签收确认」步骤后恢复 |
| 佣金/应收的触发点后移到出库确认 | 代码判据没变（都读 `biz_stock_out` / `biz_receivable`），只是时间点变 | M2-03 |
| 销售退货取原单单价（`biz_inventory_txn`） | 未确认出库的发货单没有流水行 ⇒ 退货单价回落分支；正常顺序（签收后才发现退货）不受影响 | M2-03 之后复核 |
| 前端 | **零改动**：出库单 `PENDING_POST` 是既有状态，页面已有「待过账」标签；本卡无新接口 / 新码 / 新页面，故未跑 pnpm | — |

## 六、诚实的缺口

1. **M2-03 尚未实现**：本卡只把「出库确认」这一步留在 `PENDING_POST`，签收确认入口（含在途 `received_qty` 推进）是下一张卡。本卡的 `post_out_in_tx` 已 `#[allow(dead_code)]` 标注并写明理由。
2. 模板库 `cloudpivot_test` 停在 V0404，本卡演练脚本会补 V0405…V0420；**不是**本卡引入的迁移。
3. 基线红（与本卡无关，pristine 克隆同样红）：`business::erp::voucher::tests::account_mapping_uses_voucher_rule` —— 期望 `P2C1.1405` 科目映射规则行，模板库没种这行（已在干净克隆 `cp_m202_probe` 复现后删除该库）。
4. `--test-threads>1` 时 ERP 多个模块共享夹具会互相踩（仓里既有约定是串行跑真库用例），本卡所有实测均为 `--test-threads=1`。
