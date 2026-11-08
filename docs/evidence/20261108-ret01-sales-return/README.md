# RET-01 销售退货链演练 —— 退货申请 → 审批 → 库存回冲 → 红字应收 → 凭证

> 卡号：`t_cb306e4a`（RET-01）
> 日期：2026-11-08 ｜ 环境：145 测试机（`cloudpivot_ret01` 隔离库 + `127.0.0.1:18225`）
> 被验代码：`91e36c3`（本卡唯一功能提交，含 P0 修复）
> 结论：**链路全通，92/92 断言绿**；演练发现 **1 个 P0（全额退货重复红冲收入）并当场修复**，另登记 4 条 P2/P3/P4 问题。

---

## 1. 环境与方法（为什么这份证据可信）

| 项 | 值 |
|---|---|
| 代码版本 | `91e36c320ce523c2443968438c20f3e56c78a1a5`（145 上 `git pull` 后构建） |
| 后端二进制 | `/opt/cloudpivot/backend/target/debug/cloudpivot`，构建于 `2026-11-08 22:41:05`，sha256 `5defface74620eeb8495c31beb97d738801b78cfc6838371ae4201ee7dd504f8` |
| 数据库 | `cloudpivot_ret01` —— **空库首启**（不克隆模板库，避开「别人改过已应用迁移 → 校验和失败”） |
| 迁移 | 281 条全部 `success`（`raw/../ret01_env_out.txt`） |
| 基线数据 | 仅迁移种子：`users=1 / mat=5 / wh=3 / unit=28 / role=11 / so=0 / sr=0` |
| 服务 | `127.0.0.1:18225`，health=200，`profile=dev`；不碰共用的 `cloudpivot_dev`（nginx:80 → 8033） |
| 认证 | **真实 token**：`POST /api/system/auth/login` → Bearer JWT；每一步真 curl，响应逐条落 `raw/*.json`（token 已脱敏） |
| 写入路径 | 全部走 HTTP API：期初 / 订单 / 发货 / 出库 / 应收 / 发票 / 退货 / 凭证都经业务内核，**没有一条直写三本账或凭证的 SQL** |
| 脚本 | `tools/00_env.sh`（建库起实例）· `tools/01_drill.sh`（链路 + 断言）· `tools/03_run_all.sh`（一键重放）· `tools/02_inspect_acc.sh`（只读巡检） |

重放命令（在 145 上，仓库根）：

```bash
bash docs/evidence/20261108-ret01-sales-return/tools/03_run_all.sh
# 产物：/tmp/ret01_env_out.txt /tmp/ret01_drill_out.txt /tmp/ret01_raw/
```

### 145 共用实例（nginx:80 → `cloudpivot_dev`）只读 smoke

演练数据全部写在隔离库，**共用库一个字节没写**。为证明「部署在 145 的那份实例」也带这套接口，
另跑了一条**只读** smoke（`tools/04_smoke_145_nginx.sh`，真 token，只 GET）：

```
login http=200 code=0 msg=ok                                  ← 真 JWT
GET /api/business/erp/sales-return/page            http=200 code=0 total=0
GET /api/business/erp/sales-return/returnable/{不存在} http=404 code=10002 发货单不存在或已删除（路由存在 + 参数校验生效）
GET /api/business/erp/ledger/trial-balance?period=2026-11   http=200 code=0 balanced=true
```

原始输出：`smoke_145_nginx.txt`。
（共用实例跑的是本卡修复**之前**的二进制 —— 按铁律「不自动部署」，本卡只构建不重启；
重启/上线由老六执行。）

---

## 2. 链路逐步：API + DB 断言

> 下面每一行都对应 `ret01_drill_out.txt` 里的一条 `✅`；DB 断言的原始输出在同名 `raw/*.txt`。

### ① 建客户 + 物料 + 库存

| 动作 | API | 实测 |
|---|---|---|
| 客户 | `POST /business/erp/customer` | `C-RET01-001`，额度 100000，账期 30 天 |
| 物料 | `POST /business/erp/material` | `RET01-MAT-001`（产成品、移动加权、税率 13%、非一物一码） |
| 期初 | `POST /business/erp/opening-balance/inventory` | 2026-11 期初 200 × 300 = 60000（`biz_inventory` 200/60000/avg 300；流水 `IN/OPENING`） |

`raw/s1_master.txt`：

```
inv=200.0000|amt=60000.0000|avg=300.000000
txn=IN|OPENING|200.0000|before=0.0000|after=200.0000
```

### ② 销售订单 → 审批

`POST /business/erp/sales-order`（100 × 300，税率 13）→ `SO-20261108-000001`：

```
amt=30000.0000  tax=3900.0000  total=33900.0000
```

`POST /business/erp/sales-order/{id}/submit-approval` → `SUBMITTED`；审批流（`so_approve`）两个节点由**制单人之外的审批人** `ret01_approver` 逐级通过 → 订单 `APPROVED`（断言 `biz_sales_order.status`）。

### ③ 发货 → 出库 → 应收

`POST /business/erp/shipment`（100 台）→ `PATCH /status {PENDING_POST}` → `{POSTED}`：

| 断言 | 实测 |
|---|---|
| 库存 | `200 − 100 = 100` |
| 出库流水 | `OUT/SALE_OUT`（判据看 `source_type=SALES`，非 `out_type`） |
| 应收 | `AR-20261108-000001` `33900.0000`，`OPEN` |
| 成本结转凭证 | 借 6401 30000 / 贷 1405 30000 |
| 订单 | `OUTBOUND`，`delivered_qty=100` |

### ④ 开票（收入确认）

`POST /business/erp/invoice`（应收 33900 → 不含税 30000，税率 13%）→ `IV-20261108-000001`；**先撞 ATT-06/07 闸**（无影像开票 400），补传发票影像后 `PATCH {ISSUED}`：

借 1122 33900 / 贷 5001 30000 + 贷 2221.02 3900。

### ⑤ 建退货单（关联原发货单）

`GET /business/erp/sales-return/returnable/{shipment_id}` → `shipped=100 returned=0 remaining=100 price=300 rate=13`。

`POST /business/erp/sales-return`（`return_type=QUALITY`，退 40 台，只传 `so_item_id + qty`，单价/税率由订单行带出）：

```
SR-20261108-000001  DRAFT  amt=12000.0000  tax=1560.0000  total=13560.0000
明细 price=300.000000  tax_rate=13.0000（服务端从订单行带出，前端改不了）
```

### ⑥ 提交审批 → 批准 → 待过账

- 未传附件提交 → **400**（ATT-06：销售退货单必须上传退货单/物流凭证，pdf/jpg/png，至少 1 个）
- 上传附件后 `SUBMITTED` → `APPROVED` → `PENDING_POST`（退货单**不走**流程引擎，按 `SALES_RETURN_EDGES` 的 7 条边由有权限的人流转）

### ⑦ 确认退货（过账）—— 一个事务里六件事

`PATCH /business/erp/sales-return/{id}/status {target_status: POSTED}`：

| 断言 | 实测（`raw/s6_post.txt`） |
|---|---|
| 回库（成本账 + 库位账 + 流水） | `biz_inventory` 100 → **140**，金额 30000 → **42000**（140×300） |
| 回库流水 | `IN / SALES_RETURN_IN / 40`，`before=100 after=140 amt=12000` |
| 退货入库单 | `biz_stock_in`：`in_type=RETURN / source_type=SALES_RETURN / status=POSTED` |
| 红字入库凭证（冲成本） | **借 1405 12000 / 贷 6401 12000** |
| 应收冲减 | 33900 → **20340**，`unsettled=20340`，状态仍 `OPEN`（部分退） |
| 红字发票 | `IV-20261108-000002` `OUT_RED` `ISSUED`，amt **−12000** / tax **−1560** / total **−13560** |
| 收入冲减凭证 | **借 5001 12000 / 借 2221.02 1560 / 贷 1122 13560** |
| 回写已发量 | `delivered_qty` 100 → 60 |
| 三条回链列 | `stock_in_id / receivable_id / red_invoice_id` 全部非空 |

### ⑧ 二次退货 = 全额退（应收作废）

再退剩余 60 台（`total_with_tax = 20340` = 应收未收金额，且一分没收）→ 应收 `CANCELLED`：

```
红字发票价税合计 = −33900.0000（两张红票合计，正好冲平蓝票 33900）
红字发票不含税合计 = −30000.0000
库存回到期初      = 200.0000 / 60000.0000      ← 全退后库存与金额精确回到演练起点
delivered_qty     = 0.0000
成本冲减累计 借1405 30000 = 原结转 借6401 30000
收入冲减累计 借5001 30000 = 原确认 贷5001 30000（净额 0）
应收账款净额 借 33900 = 贷 33900（余额 0）
销项税净额   借 3900 = 贷 3900（余额 0）
无多余红冲凭证：全链路 is_red=1 共 0 张
```

### ⑨ 库存对账（两条口径，差异必须为 0）

```sql
-- ① 逐 (物料, 仓库)：库存表 vs 流水累计
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT count(*) FROM inv FULL JOIN txn USING (material_id, warehouse_id)
 WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0);
-- ② 逐桶：成本账 vs 库位账
WITH cost AS (SELECT i.material_id, i.warehouse_id,
                     COALESCE(il.location_id::text,'(未分配)') bucket, SUM(i.qty) qty
                FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id
               GROUP BY 1,2,3),
     loc  AS (SELECT material_id, warehouse_id,
                     COALESCE(location_id::text,'(未分配)') bucket, SUM(qty) qty
                FROM biz_inventory_location GROUP BY 1,2,3)
SELECT count(*) FROM cost FULL JOIN loc ON cost.material_id=loc.material_id
  AND cost.warehouse_id=loc.warehouse_id AND cost.bucket=loc.bucket
 WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0);
```

输出（`raw/s9_reconcile.txt`）：

```
recon1 RET01-MAT-001 wh=WH01 inv=200.0000 txn=200.0000 diff=0.0000
对账① 差异行数 = 0
对账② 差异桶数 = 0
```

### ⑩ 报表不变形（试算平衡 / 资产负债表）

| 接口 | 实测 |
|---|---|
| `GET /business/erp/ledger/trial-balance?period=2026-11` | 本期借 **187800** = 贷 **187800**；期末借 **60000** = 贷 **60000**；`balanced=true` |
| `GET /business/erp/report/balance-sheet?period=2026-11` | 资产 **60000** = 负债+权益 **60000**；`diff=0`，`balanced=true` |

期末只剩期初那笔库存（借 1405 60000 / 贷 3103 60000）——整条销售链+两次退货在总账上**净归零**，这正是「全退」应该有的样子。

---

## 3. 凭证一览（7 张，全部借贷平衡）

| 凭证 | 来源 | 借 | 贷 | 含义 |
|---|---|---|---|---|
| PZ-…000001 | OPENING | 1405 库存商品 60000 | 3103 本年利润 60000 | 期初 |
| PZ-…000002 | STOCK_OUT | 6401 主营业务成本 30000 | 1405 库存商品 30000 | 销售出库结转成本 |
| PZ-…000003 | INVOICE | 1122 应收账款 33900 | 5001 收入 30000 + 2221.02 销项税 3900 | 开票确认收入 |
| PZ-…000004 | STOCK_IN（退货） | 1405 库存商品 12000 | 6401 主营业务成本 12000 | 退货回库、冲减成本 |
| PZ-…000005 | INVOICE（红票） | 5001 收入 12000 + 2221.02 1560 | 1122 应收账款 13560 | 退货冲减收入 |
| PZ-…000006 | STOCK_IN（退货） | 1405 库存商品 18000 | 6401 主营业务成本 18000 | 第二次退货回库 |
| PZ-…000007 | INVOICE（红票） | 5001 收入 18000 + 2221.02 2340 | 1122 应收账款 20340 | 第二次退货冲减收入 |

科目净额（`raw/02_inspect_acc_修复后.txt`）：`1122=0`、`5001=0`、`2221.02=0`、`6401=0`、`1405=60000`（期初库存）、`3103=−60000`。

> 与卡面的口径差：卡面写「借 主营业务收入(红字) / 贷 应收账款(红字)」。
> 实现的红字是**借贷方向正常、金额为正的冲销分录**（借 5001 / 借 2221.02 / 贷 1122），
> 因为库层 `biz_voucher_entry.ck: debit_amount >= 0` 与 `is_red` 借贷互换口径都禁负数金额。
> 经济含义与「红字冲销」一致，报表净额也是对的（见 §2⑧）。

---

## 4. 负例矩阵（实测状态码 + 报文）

| # | 场景 | 期望 | 实测 | 报文/证据 |
|---|---|---|---|---|
| 1 | 退货数量 > 已发货（200 > 100） | 400 | **400** | `items[0].qty = 200 超过可退量 100.0000（已发 100.0000 − 已退 0）：超退被拒绝` |
| 2 | 退货原因非法（`return_type=REFUND`） | 400 | **400** | `return_type 必须是 QUALITY / WRONG_ITEM / OTHER 之一（当前 REFUND）` |
| 3 | 未审批就确认（`DRAFT → POSTED`） | 卡面写 400 | **409** | `非法状态迁移：草稿（DRAFT）→ 已过账（POSTED）不在合法边表里；当前可流转到：SUBMITTED / CANCELLED` |
| 4 | 跳过审批（`DRAFT → PENDING_POST`） | 400/409 | **409** | 同上（边表里没有这条边） |
| 5 | 未上传附件就提交（ATT-06） | 400 | **400** | `销售退货单必须上传退货单或物流凭证（pdf/jpg/png）：至少要 1 个，当前 0 个` |
| 6 | 发票无影像就开票（ATT-07） | 400 | **400** | 发票影像必传（纸质票 jpg/png/pdf） |
| 7 | 已全额退货的订单行再退 1 台 | 400 | **400** | `items[0].so_item_id 不属于发货单 … 的可退明细（行不存在 / 没发过货 / 已全部退完）` |

负例 3 的差异（卡面 400 vs 实测 409）登记为 P3：本仓的统一约定是「**非法状态迁移 = 409 冲突**，
参数非法 = 400」，前端按钮态也按 409 处理；把它改成 400 会破坏其他 20 多个状态机的同口径。

---

## 5. 问题清单

### P0（已修，本卡唯一功能提交 `91e36c3`）——全额退货时收入被冲两遍

**现象**：第二次（全额）退货把应收推到 `CANCELLED` 时，`sales_return_posting::offset_receivable`
直接复用了 `receivable::cancel_for_stock_out`。该函数的语义是「**出库作废**」，
会红冲这张应收挂的**原收入确认凭证**；而退货链路本身**已经**用红字发票 + 收入冲减凭证
把收入/销项税/应收冲过了 ⇒ 同一笔收入被冲两遍。

现场数据（修复前，`raw_run1_bug/02_inspect_acc_修复前.txt`）：

```
多出凭证 PZR-20261108-000001（red=1，33900/33900，红冲 PZ-…000003）
5001    D=60000.0000 C=30000.0000 → 主营业务收入净额 −30000（凭空多一笔亏损）
1122    D=33900.0000 C=67800.0000 → 应收账款挂出 33900 贷方余额
2221.02 D=7800.0000  C=3900.0000  → 销项税多冲 3900
资产负债表：资产=30000（真实应是 60000，库存 60000 / 应收 0）
试算平衡：balanced=true（★ 一张凭证内部借贷相等，掩盖了重复冲销）
```

**影响**：全退场景（电商退货、质量问题整单退）会让利润表和资产负债表同时错账，
且「借贷平衡」类巡检发现不了，只能靠「同一经济事项的净额是否为零」这类断言抓到。

**修法**（2 文件）：
- `receivable.rs`：把作废执行体抽成 `cancel_core(..., mode: CancelRevenue)`；
  原 `cancel_for_stock_out` = `CancelRevenue::Reverse`（行为不变），
  新增 `cancel_for_sales_return` = `CancelRevenue::Keep`（只作废台账，不碰总账）。
- `sales_return_posting.rs`：`offset_receivable` 的全额冲减分支改调 `cancel_for_sales_return`。
- 单测：`receivable::tests::cancel_revenue_mode_decides_whether_original_voucher_is_reversed`
  + `sales_return_posting::tests::full_return_cancel_must_not_reverse_original_revenue_voucher`。

**修复后实测**：凭证 8 张 → **7 张**（PZR 消失），`is_red=1` 共 **0 张**，
`1122 / 5001 / 2221.02` 净额 **全部 0**，资产负债表资产 60000 = 负债+权益 60000，
演练断言 92/92 全绿（`ret01_drill_out.txt`）。

### P2 —— 红字发票金额没有按「已开票金额」封顶

`create_red_invoice` 只判 `invoiced > 0` 就按退货单的 `total_amount/total_tax` 全额开红票。
若原应收**只开了一部分票**（发票金额 < 应收），退货全额冲减时红票会超过当初确认的收入，
同样会把 5001 冲成负数。建议：红票金额 = `min(退货金额, 该应收未红冲的已开票余额)`。
（本卡证据链是「应收全额开票」，未触发；属独立卡范围。）

### P2 —— 已过账的退货单没有任何冲回入口

`SALES_RETURN_LOCKED = [POSTED, CANCELLED]`，且 `SALES_RETURN_EDGES` 里 `POSTED` 无出边
⇒ 退货单过账后发现退错（数量/物料/客户），界面上没有任何按钮，只能改库。
建议补一条受控的「退货红冲（红字退货单 / 反向过账）」链路，或给出反过账接口 + 审批。

### P3 —— 卡面「未审批就确认 → 400」与实测 409

见 §4 负例 3。建议卡面/文档改成 409，或统一状态机错误码（改动面较大，不建议为了对齐卡面改）。

### P3 —— 卡面「退货原因必填」的实现口径

实现里**必填的是 `return_type`（QUALITY / WRONG_ITEM / OTHER）**，`remark` 可选；
只有作废边（`SUBMITTED/APPROVED → CANCELLED`）强制 `reason`。
即「退货原因」是枚举而不是自由文本 —— 想记「具体为什么坏」得写备注。

### P4 —— 作废应收仍保留 `amount / unsettled_amount`

库层 `ck_biz_ar_unsettled: unsettled_amount = amount - received_amount` 决定了
作废单不能把金额清 0（`amount > 0` 也是 CHECK）。账龄（`AGING_STATUSES`）与信用占用
都已按 status 过滤，本次实测「账龄口径未结合计 = 0」；
但任何**手写** `SUM(unsettled_amount)` 的临时报表若不过滤 status 会虚高 20340。
建议只读巡检脚本统一带 `status IN ('OPEN','PARTIAL')` 谓词。

---

## 6. 改了哪些文件

| 文件 | 性质 |
|---|---|
| `backend/src/business/erp/receivable.rs` | 抽出 `cancel_core` + `CancelRevenue` 口径；新增 `cancel_for_sales_return`；+1 单测 |
| `backend/src/business/erp/sales_return_posting.rs` | 全额冲减分支改调 `cancel_for_sales_return`；文档口径订正；+1 单测 |
| `docs/evidence/20261108-ret01-sales-return/` | 本证据（README + 4 个脚本 + `raw/` 47 个文件 + `raw_run1_bug/` 修复前现场） |

**未改前端（0 文件）**，因此未跑 `pnpm typecheck && pnpm build`。
**未新增迁移**（`biz_sales_return` / 凭证 / 应收的表结构够用）。

### 测试结果（`tests_cargo.txt`）

| 命令（145，`CARGO_PROFILE_TEST_DEBUG=0`） | 结果 |
|---|---|
| `cargo test --lib erp::receivable` | **46 passed / 0 failed / 0 ignored** |
| `cargo test --lib erp::sales_return` | **33 passed / 0 failed / 0 ignored** |
| 定点跑两条新用例（`cancel_revenue_mode` / `full_return_cancel`） | 各 **1 passed**（确认没被 filter 掉） |
| `cargo test --lib`（全量 3768 条） | 3765 passed / **3 failed（既有红，非本卡）** |

3 条既有红是 `business::flow::linkage::*` 的 wiring 断言：`PURCHASE_RETURN` 由
`9cf6cc1`（P2-03 采购退货闭环）加进联动目录，但当时没同步 `tests_wiring.rs`（最后改动是 `e83e1f8`）。
`git merge-base --is-ancestor 9cf6cc1 07c39d4` 通过 ⇒ 它在本卡基线里就已经是红的；
本卡两张改动文件都不在 `flow/linkage/*`。**建议单开一张卡补这三条断言**。

> 145 的坑：`cargo test` 默认 debuginfo=2，链接 lib-test 时被 OOM killer SIGKILL（7.4G 内存多 agent 共享）。
> 用 `CARGO_PROFILE_TEST_DEBUG=0` 才稳定（只去调试符号，断言完全一致）。

---

## 7. 未覆盖 / 后续

1. **退货入库的库位**：本卡退货单不带 `location_id`（落「未分配桶」），
   逐桶核对 0 差异成立；指定库位回库未在本卡演练（属库存桶策略卡的覆盖范围）。
2. **一物一码物料退货**：`post_in_tx` 对 `serial_required=true` 的物料直接 400
   （"要走序列号状态流转"），本卡只演练普通物料。
3. **收款后再退货**：应收已收款时全额退被 409 挡住（要求先做收款退款单），
   属 `receipt_refund` 链，未在本卡演练。
