# P0-01 销售链端到端演练（145 测试机 · cloudpivot_dev · 2026-11-07）

- 目标环境：`http://172.18.10.145`（nginx 80 → 127.0.0.1:8033，服务 `cloudpivot-test`）
- 库：`cloudpivot_dev`（145 本机 PG 18）
- 后端版本：`5e3657d`（演练中修 1 个 P2 后部署复验；之前 9daf306）
- 账号：`admin`（SUPER_ADMIN）+ 本次新建 `sales_sup`（SALES_SUPERVISOR，仅用于审批）
- 演练数据全部自造，未连 144 生产

## 结论先行

销售链 **9/9 步全绿，链路通畅**：订单→审批→发货→出库（真扣库存）→应收→收款→凭证→总账→报表
每一环都在同一事务内落库，金额与借贷全部勾稽。发现 1 个阻断「开箱即用」的 P2 数据缺口（无审批人）、
修掉 1 个 P2 读接口缺列 bug（`5e3657d`），另有 4 条 P3/P4 口径与文档问题。

## 1. 链 A（主链，税率 0，金额 2000）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 1 | `POST /api/business/erp/customer`（DEMO-C001 演练客户A） | 200 | `biz_customer` id=01a1b5c9-23bb-7180-b916-d4ba9d0d7a6a，ACTIVE | ✅ |
| 1b | `POST /api/business/erp/material` ×2（DEMO-M001/M002） | 200 | `biz_material` 2 行，tax_rate=0，MOVING_AVG | ✅ |
| 1c | `POST /api/business/erp/opening-balance/inventory`（100@60、100@120） | 200 | `biz_inventory` 6000/12000；txn IN×2(OPENING)；期初凭证 PZ-…0001 借1405 18000/贷3103 18000 | ✅ |
| 2 | `POST /api/business/erp/sales-order`（A 10×100 + B 5×200） | 200 | SO-20261107-000001 DRAFT，total_amount=2000；明细 2 行（1000+1000） | ✅ |
| 3 | `POST /api/business/erp/sales-order/{id}/submit-approval` | 200（首次 422，见问题 1） | SUBMITTED，实例 FLW-20261107-444F436F（so_approve） | ✅ |
| 3b | `POST /api/business/flow/task/{id}/approve`（sales_sup） | 200 | 实例 APPROVED，任务 n_sales APPROVED（comment 留痕），订单 **APPROVED** | ✅ |
| 4 | `POST /api/business/erp/shipment`（明细留空＝按未发量带出） | 200 | SH-20261107-000001 DRAFT，total_qty=15；明细 10 / 5 | ✅ |
| 4b | `PATCH /shipment/{id}/status` `PENDING_POST` | 200 | PENDING_POST | ✅ |
| 5 | `PATCH /shipment/{id}/status` `POSTED` | 200 | 出库单 OUT-…0001（POSTED，成本 1200）；**库存 100→90、100→95**；txn OUT×2；delivered_qty 10/5；订单 **OUTBOUND** | ✅ |
| 6 | 应收自动挂账（出库过账联动） | — | AR-…0001 = **2000**，OPEN，due 2026-12-07 | ✅ |
| 7 | `POST /api/business/erp/receipt`（2000，BANK_TRANSFER） | 200 | RC-…0001 DRAFT | ✅ |
| 7b | `POST /receipt/{id}/allocate` | 200 | 核销 2000；应收已收 2000 / 未收 **0** / **SETTLED** | ✅ |
| 7c | `PATCH /receipt/{id}/status` CONFIRMED→POSTED | 200 | 凭证 PZ-…0003 借1002 2000/贷1122 2000 | ✅ |
| 8 | `POST /invoice`（从应收开票，税 0）+ `PATCH /invoice/{id}/status` ISSUED | 200 | IV-…0001；凭证 PZ-…0004 借1122 2000/贷5001 2000 | ✅ |
| 8b | 凭证核对 | — | 本链 3 张 + 期初 1 张，**逐张借=贷**，科目 1405/6401/1122/5001/1002 正确 | ✅ |
| 9 | `ledger/page?account_code=1122`、`ledger/trial-balance`、`report/balance-sheet`、`report/income-statement` | 200 | 1122 借2000/贷2000；试算 本期 23200=23200、期末 20000=20000；BS 18800=18800（balanced=true）；IS 收入 2000/成本 1200/净利 800，与未分配利润变动勾稽 | ✅ |

## 2. 链 B（含税变体 13%，验证应交税费与期末应收账款有数）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 2 | 建单（A 2×100 + B 1×200，tax_rate=13） | 200 | SO-…0002：400 + 税 52 = 452 | ✅ |
| 3 | 提交 + 审批（sales_sup） | 200 | 实例 FLW-…8AE97290 APPROVED，订单 APPROVED | ✅ |
| 4-5 | 发货单 → PENDING_POST → POSTED | 200 | OUT-…0002（成本 240）；库存 90→88、95→94；订单 OUTBOUND | ✅ |
| 6 | 应收 | — | AR-…0002 = **452** OPEN（不回款，用于报表口径） | ✅ |
| 8 | 开票（tax_rate=**0.13**）+ ISSUED | 200 | 凭证 PZ-…0006 借1122 452 / 贷5001 400 / 贷**2221.02 52** | ✅ |
| 9 | BS / IS | 200 | 资产 **19012**（货币 2000 + 应收 **452** + 存货 16560）= 负债 52 + 权益 18960；IS 收入 2400/成本 1440/净利 960 | ✅ |

## 3. 金额勾稽

- 订单金额 = 明细合计：**是**（2000 = 1000+1000；变体 400 = 200+200）
- 应收 = 订单含税：**是**（2000；变体 452）
- 收款 = 应收：**是**（核销 2000，未收 0，SETTLED）
- 凭证借贷平衡：**是**（6 张逐张相等；全账 借 23892 = 贷 23892）
- 试算平衡：**是**（本期 23892=23892，期末 20452=20452）
- 资产负债表平衡：**是**（19012 = 52 + 18960，`check.diff=0`）
- 利润表勾稽：**是**（净利 960 = 未分配利润本年变动 960）
- 库存数量勾稽：**是**（100−10−2=88、100−5−1=94，与 txn 累计一致）
- 成本勾稽：**是**（出库成本 1200+240 = 主营业务成本 1440 = 库存商品减少 1440）

## 4. 发现的问题

| # | 问题 | 严重度 | 复现 | 处置 |
|---|---|---|---|---|
| 1 | 新环境只有 admin 一个账号，`so_approve` 的「销售主管审批」节点无成员且制单人=admin ⇒ 提交直接 `422 65018`（fail-closed 内控设计，见 `self-approval-deadlock-cards.md`）。内控正确，但开箱即用无任何非制单人审批人 | P2（阻断开箱） | 新库 → admin 建单 → submit-approval | 建议种子补 SALES_SUPERVISOR 演示账号 / 422 时前端引导指派审批人 |
| 2 | 出库单读接口丢 `source_type/source_id`（写入正确，`HEADER_COLUMNS`+`StockOutRow`+`StockOutRes` 三处没有）。销售出库 `out_type` 固定 `OTHER`，来源列是唯一区分维度，丢了无法追溯发货单 | P2（**已修**） | `GET /stock-out/{id}` 返回 `source_type:null` 而库里是 `SALES` | `5e3657d`：三处补两列（纯只读，零迁移）；145 已部署验证 `source_type=SALES`+`source_id=发货单 id`，手工 ISSUE 出库仍 `null` |
| 3 | `tax_rate` 同名字段两种标度：订单/收货明细=百分数（13），发票/物料=小数（0.13） | P3（100 倍税差风险） | `POST /invoice {"tax_rate":13}` → 400 | 统一标度，过渡期用 `tax_rate_pct` 区分 |
| 4 | 库存期初对方科目写死 `3103 本年利润`（`opening.rs:115`），期初 18000 计进「未分配利润期初」 | P3（口径） | `POST /opening-balance/inventory` 后看期初凭证 / IS check | 做成可配置，默认权益类科目 |
| 5 | 库存流水 `biz_inventory_txn.biz_type` 对销售出库记 `OTHER_OUT`（`stockout_base.rs:88`），按 biz_type 统计销售出库会漏 | P3（统计口径） | 发货过账后查 txn | 写 `SALES_OUT` 或报表注明口径 |
| 6 | 审批意见只落 `biz_flow_task.comment`，实例 `result_comment` 为空 | P4 | 审批后对比两表 | 末节点意见回写实例 |
| 7 | 卡面接口名与实际不符：无 `/sales-order/{id}/ship`、`/shipment/{id}/confirm`；收款 method 白名单是 `BANK_TRANSFER/CASH/CHECK/OTHER`（`BANK` → 400） | P4（文档） | 按卡面 curl | 文档按真实路由写 |

## 5. 汇总

- 通过步骤 **9/9**（链 A 全绿，链 B 为含税补充）
- 阻断性 bug **0 个**（1 个 P2 开箱配置缺口，建审批人即通）
- 本卡修复 **1 个**（P2，2 个源文件，`5e3657d`，已 push origin+github、145 编译 50s、health 200）
- 结论：**链路通畅**

## 6. 演练自造数据（cloudpivot_dev）

客户 DEMO-C001；物料 DEMO-M001/M002；用户 sales_sup；订单 SO-20261107-000001（已闭环）/
-000002（应收未收 452）；发货单 SH-…0001/0002；出库单 OUT-…0001/0002（+1 张已软删冒烟草稿）；
应收 AR-…0001（SETTLED）/0002（OPEN）；收款 RC-…0001；发票 IV-…0001/0002；凭证 PZ-…0001~0006。
