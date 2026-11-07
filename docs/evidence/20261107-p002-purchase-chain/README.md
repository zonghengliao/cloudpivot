# P0-02 采购链端到端演练报告（145 测试机 · cloudpivot_dev · 2026-11-07）

- 环境：`http://172.18.10.145`（nginx 80 → 127.0.0.1:8033），库 `cloudpivot_dev`（145 本机 PG）
- 后端版本：`fd76a03`（= origin/main；本卡**未改代码**，无需部署）
- 账号：`admin`（SUPER_ADMIN，制单人）+ 本卡新建 `demo_lead`（DEPT_LEADER + WAREHOUSE_MANAGER，审批人）
- 数据全部为本卡自造，未碰 144 生产；演练数据清单见文末「数据与清理」

## 演练结果

### 链 A：供应商 → 采购订单 → 审批 → 收货入库 → 应付 → 付款 → 凭证 → 总账/报表（税率 0，金额 2000）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 1 | `POST /api/business/erp/supplier`（DEMO-S001 演练供应商A） | 200 | `biz_supplier` id=`01a1b5d2-504f-72f7-92b2-00f884285a2c`，credit_days=30 | ✅ |
| 1b | `POST /api/business/erp/material` ×2（DEMO-P101 / DEMO-P102，单位=套，MOVING_AVG） | 200 / 200 | `biz_material` 2 行，tax_rate=0 | ✅ |
| 2 | `POST /api/business/erp/purchase-order`（2 行：100×10 + 50×20，price_mode=EXCL） | 200 | `biz_purchase_order` PO-20261107-000001 **DRAFT**，total_qty=150，total_amount=**2000**，total_with_tax=2000；`biz_purchase_order_item` 2 行（1000 + 1000） | ✅ |
| 3 | `POST /api/business/erp/purchase-order/{id}/submit-approval` | 200 | status=**SUBMITTED**，`approval_instance_id` 落库；`biz_flow_instance` **FLW-20261107-9CC1EB84**（po_approve / PENDING，节点 n_dept 部门主管审批） | ✅ |
| 3b | `POST /api/business/flow/task/{id}/approve`（**demo_lead** 审批） | 200 | 实例 APPROVED；`biz_flow_task` n_dept APPROVED（comment 留痕）；`biz_purchase_order.status`=**APPROVED**（approved_at 2026-11-07 19:44:03） | ✅ |
| 4 | `POST /api/business/erp/stock-in`（`source_order_id`=PO，明细留空 → 按未收量自动带出） | 200 | `biz_stock_in` IN-20261107-000001 **DRAFT**，in_type=PURCHASE，source_order_no=PO-20261107-000001，supplier 由采购单派生，total_qty=150，total_amount=2000；`biz_stock_in_item` 2 行，`source_item_id` 指向采购明细行 | ✅ |
| 4b | `POST /api/business/erp/stock-in/{id}/submit-post`（挂 `stock_in_post` 已发布流程） | 200 | status=**PENDING_POST**，实例 FLW-20261107-13B33B1B（节点 n_wh 仓管主管） | ✅ |
| 4c | `POST /api/business/flow/task/{id}/approve`（demo_lead） | 200 | 收货单 **POSTED**，voucher_id=936e0567…，payable_id=e3c8b4a0…，posted_at 19:44:16 | ✅ |
| 4d | **库存核对**（关键） | — | `biz_inventory`：P101 0→**100** @10=1000、P102 0→**50** @20=1000（**+150**）；`biz_inventory_txn` IN/PURCHASE_IN ×2（before 0 → after 100 / 50，period 2026-11，doc_no=IN-20261107-000001）；采购单 status 自动 → **RECEIVED**，明细 `received_qty` 100/50、`pending_qty` 0 | ✅ |
| 5 | 应付出账（过账联动生成，无人工接口） | — | `biz_payable` **AP-20261107-000001**，source_type=STOCK_IN / source_id=收货单，amount=**2000**，paid 0，unpaid 2000，status=**OPEN（未付）**，due_date=2026-12-07（供应商账期 30 天） | ✅ |
| 6 | `POST /api/business/erp/payable/{id}/pay`（2000 / BANK_TRANSFER） | **202** | `biz_payment` PM-20261107-000001，approval_status=PENDING → 实例 FLW-20261107-86EE8020（payable_pay / n_dept）；**卡面写 200 直付，实际 202 待审批**（见问题 2） | ✅ |
| 6b | `POST /api/business/flow/task/{id}/approve`（demo_lead） | 200 | 付款 APPROVED；`biz_payment.voucher_id`=f52d0baa…；`biz_payable` paid=**2000** / unpaid=**0** / status=**SETTLED** | ✅ |
| 7 | 凭证核对（`biz_voucher` + `biz_voucher_entry`） | — | **PZ-20261107-000007**（STOCK_IN）：借 1405 库存商品 1000+1000 / 贷 2202 应付账款 2000，合计 2000=2000；**PZ-20261107-000008**（PAYMENT）：借 2202 2000 / 贷 1002 银行存款 2000，合计 2000=2000；科目、方向、辅助核算（供应商/物料）全部正确 | ✅ |
| 8 | `GET /ledger/page?account_code=1405`、`/ledger/trial-balance`、`/report/balance-sheet`、`/report/cash-flow` | 200 | 总账 1405 本期借 20000（期初 18000 + 采购 2000）/ 贷 1440 / 期末 18560；试算 本期 27892=27892、期末 20452=20452、balanced=true；资产负债表 资产 **19012** = 负债 52 + 权益 18960（balanced，diff=0），**存货 18560**；现金流量表「购买商品、接受劳务支付的现金」=**2000**，check.balanced=true（期末现金 0 = 货币资金 0） | ✅ |

### 链 B（变体）：13% 含税采购，验证进项税科目

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 2 | `POST /purchase-order`（P102 100×20，tax_rate=**13**） | 200 | PO-20261107-000002：amount 2000、tax **260**、with_tax **2260** | ✅ |
| 3 | submit-approval + demo_lead 审批 | 200 | 实例 FLW-20261107-…；订单 APPROVED | ✅ |
| 4 | `POST /stock-in`（自动带出）+ submit-post + demo_lead 审批 | 200 | IN-20261107-000002 **POSTED**，150 件库存 → **200 件**（50→150），金额 1000→3000 | ✅ |
| 5 | 应付联动 | — | AP-20261107-000002 = **2260**（含税），OPEN，未付款（刻意保留，用于证明期末应付有数） | ✅ |
| 7 | 凭证 PZ-20261107-000009 | — | 借 1405 **2000** + 借 **2221.01 应交税费-进项税 260** / 贷 2202 **2260**，合计 2260=2260 | ✅ |
| 8 | 资产负债表 / 试算 / 现金流量表（变体后终态） | 200 | 资产 **21012** = 负债 2052（应付 2260 + 应交税费 **-208** 借方留抵）+ 权益 18960；存货 **20560**；试算 期末 22712=22712、本期 30152=30152 | ✅ |

## 金额勾稽

| 勾稽项 | 结果 |
|---|---|
| 采购订单金额合计 = 明细合计之和？ | **是**（2000 = 1000 + 1000；变体 2000 + 260 = 2260） |
| 采购订单 qty = 入库单 qty + 未入库 qty？ | **是**（150 = 150 + 0；`pending-items` pending_qty 均为 0，订单自动 RECEIVED） |
| 入库单 qty = 库存增加量？ | **是**（150 = P101 +100、P102 +50；`biz_inventory_txn` before/after 一致） |
| 采购订单（价税合计）= 应付金额？ | **是**（2000 = 2000；变体 **2260 = 2260**。注意：`total_amount` 是不含税，税率 > 0 时应比 `total_with_tax`，见问题 5） |
| 应付 = 入库单（价税合计）？ | **是**（AP-000001 2000 = IN-000001 2000；AP-000002 2260 = IN-000002 2260） |
| 付款核销 = 应付金额？ | **是**（付款 2000，paid 2000 / unpaid 0 / SETTLED；AP-000002 保留未付 2260） |
| 凭证借贷平衡？ | **是**（本链 3 张：2000=2000、2000=2000、2260=2260；全账 9 张凭证 `HAVING dr<>cr` 零行） |
| 试算平衡？ | **是**（主链收口时点 本期 27892=27892、期末 20452=20452；变体后 30152=30152、22712=22712） |
| 资产负债表平衡？ | **是**（主链收口 19012=19012；终态 21012=21012，`check.diff=0`、`unmapped=[]`、`unbalanced_vouchers=[]`） |
| 现金流量表勾稽？ | **是**（购买商品支付现金 2000；期末现金 0 = 资产负债表货币资金 0） |

## 发现的问题

| # | 问题 | 严重度 | 复现步骤 | 建议 |
|---|---|---|---|---|
| 1 | **新环境采购链在「提交审批」也断**（与 P0-01 同源）：出厂只有 admin，`po_approve` 的 n_dept 绑 DEPT_LEADER（无成员）、n_board 绑 PURCHASE_MANAGER+GENERAL_MANAGER（无成员），空审批人兜底 FALLBACK_ADMIN = admin = 制单人，而 `erp.separation_of_duties.approve` 默认 **true** → 节点无可用审批人 | **P2（阻断开箱即用）** | 145 上新库 → admin 建 15 万采购单（PO-20261107-000003）→ `POST /purchase-order/{id}/submit-approval` → **422 / 65018**「节点『采购经理+总经理会签』的审批人全部是制单人/发起人本人…」（负例单已软删，无孤儿审批实例） | 种子数据补 DEPT_LEADER / PURCHASE_MANAGER / GENERAL_MANAGER / WAREHOUSE_MANAGER 演示账号；或 422 时前端直接引导「去角色管理给该节点加人」 |
| 2 | **付款不是直付**：`payable_pay` 流程已发布（v2），`POST /payable/{id}/pay` 返回 **202 PENDING_APPROVAL**，要审批后记账；卡面写「200 + 直接生效」 | P4（卡面口径） | 见步骤 6 | 卡面/文档补一句：付款走审批时 202，未配流程才 200 直付 |
| 3 | **卡面 3 处接口形状与实际不符**（照抄必失败） | P4（文档） | ① `POST /erp/supplier {"name","code"}` → **422**（要 `supplier_code`/`supplier_name`）② `POST /erp/purchase-order` 明细用 `price`/`amount`、单头缺 `warehouse_id`/`price_mode` → **400**（要 `price_excl`+`price_mode`，`amount` 会被 serde 静默丢弃，金额只由服务端算）③ `POST /erp/payment` → **404**（真实是 `POST /erp/payable/{id}/pay`） | 按真实契约改卡面：供应商字段名、采购单必填 `warehouse_id`+`price_mode`+`price_excl`、付款走应付单 |
| 4 | **`received_qty` 不是表列**：`biz_purchase_order(.item)` 都没有该列，卡面 `SELECT received_qty` 无法执行。它是按 **POSTED 收货单聚合派生**（`GET /purchase-order/{id}/pending-items`、`GET /purchase-order-item/page` 都有值） | P4（卡面口径） | `\d biz_purchase_order_item` 无该列；但 item 接口返回 `received_qty=100/50` | 卡面 DB 验证改成「查 `biz_stock_in_item` 按 `source_item_id` 聚合」或直接用上面两个接口 |
| 5 | **税率 > 0 时 `total_amount` ≠ 应付**：订单 `total_amount` 是不含税，应付是价税合计。13% 变体：订单 amount 2000 / 应付 2260，差 260（税额） | P3（口径易误判） | 链 B 步骤 2 与 5 | 勾稽公式统一写「订单 `total_with_tax` = 应付金额」 |
| 6 | **资产负债表「应交税费」出现负数**：13% 变体后进项 260 > 销项 52 → 报表显示 **-208**，未重分类到「其他流动资产」 | P3（列报口径） | 跑链 B 后 `GET /report/balance-sheet` → liabilities 中 `TAXES_PAYABLE = -208` | 小企业准则下抵减可接受，但建议在报表说明里注明「借方余额 = 留抵税额」，或按流动性重分类 |
| 7 | 采购入库凭证行摘要里物料显示为 UUID 前 8 位（「存货 物料 01a1b5d2（第 1 行）」），人看不出来是哪个物料 | P4（可读性） | `PZ-20261107-000007` 前两行 summary | 摘要用物料编码/名称（aux 里已有 material_id，取名字不难） |

## 汇总

- 通过步骤：**链 A 8/8 全绿；链 B 变体 4/4 全绿**（另加 3 个负例：已收满再收货 400、缺 `purchase_order_item_id` 400、无审批人 422/65018）
- 阻断性 bug：**0 个**（唯一「阻断」是环境种子缺审批人，需先建审批账号；与 P0-01 同一问题）
- 结论：**采购链链路通畅**（库存、应付、付款、凭证、总账、三大报表全部勾稽一致）

## 数据与清理（cloudpivot_dev）

| 类型 | 数据 |
|---|---|
| 主数据 | `biz_supplier` DEMO-S001、`biz_material` DEMO-P101/DEMO-P102、`sys_user` demo_lead（DEPT_LEADER + WAREHOUSE_MANAGER） |
| 单据 | PO-20261107-000001（RECEIVED）、PO-20261107-000002（RECEIVED）、**PO-20261107-000003（150k 负例，已 DELETE 软删，无审批实例）** |
| 入库 | IN-20261107-000001（POSTED 150/2000）、IN-20261107-000002（POSTED 100/2260） |
| 应付/付款 | AP-20261107-000001（SETTLED 2000）、AP-20261107-000002（OPEN 2260，刻意保留证明期末应付有数）、PM-20261107-000001（APPROVED） |
| 凭证 | PZ-20261107-000007 / 000008 / 000009（全部借贷平衡） |
| 库存 | DEMO-P101 100@10、DEMO-P102 150@20（=3000） |

> 未 DROP / TRUNCATE 任何表；除负例采购单（自己造的草稿）外未删除任何数据。
