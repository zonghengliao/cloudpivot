# ERP 凭证辅助核算口径（DC-11 · P2-1）

> 代码入口：`backend/src/business/erp/voucher.rs`（写入口径）、
> `backend/src/business/erp/ledger.rs`（辅助账查询）、
> `backend/src/business/erp/payable_payment.rs`（付款凭证的部门）。
> 真机验收：`scripts/verify/p21_aux_e2e.py`（42 项断言）。

## 1. 一句话口径

**每条分录只挂"这条分录自己说得清"的维度。** 说不清就留空 —— 留空是信息缺失，
记错是信息错误，后者财务发现不了、且会污染辅助账余额。

## 2. 哪些凭证写哪些维度（**当前实现**）

| 业务 | 凭证来源 | 借方行 | 贷方行 |
|---|---|---|---|
| 采购入库（收货过账） | `STOCK_IN` | `aux_material_id`（**仅单物料单**）、`aux_warehouse_id`（收货仓） | `aux_supplier_id` |
| 期初 / 其他入库（序列号批量生成） | `SERIAL_BATCH` | 存货行：`aux_material_id` + `aux_warehouse_id`；对方 1901 行只有 `aux_material_id` | — |
| 序列号出库 / 报废 | `SERIAL_OUT` / `SERIAL_SCRAP` | 6401 / 1901 行：`aux_material_id` | 1405 行：`aux_material_id` + `aux_warehouse_id` |
| 出库 / 领用 / 报废单 | `STOCK_OUT` | 6401 / 1901 行：`aux_material_id`（**仅单物料单**），不挂仓库 | 1405 行：`aux_material_id` + `aux_warehouse_id` |
| 盘盈盘亏 | `STOCK_TAKE` | 按物料拆行：每行 `aux_material_id`；盘亏带 `aux_warehouse_id` | 同左 |
| 付款 | `PAYMENT` | 2202 行：`aux_supplier_id` + **`aux_dept_id`** | 1002 行：`aux_supplier_id`（**不挂部门**） |
| 手工凭证 / 期初 / 年结 / 红冲 | `MANUAL` / `OPENING` / `YEAR_END` | 不写维度 | 不写维度 |
| 红字（红冲） | 任意 | **原样复制原分录的四个辅助列**（技术架构 §3.1.7 第 4 条） | 同左 |

## 3. 部门从哪来（DC-11 的"先定部门从哪来"）

```
付款凭证的部门 = 申请人所属部门
  申请人 = biz_payment.create_by（付款登记人 = 登录操作人）
  部门   = sys_user.dept_id 的**当前值**，写进凭证后即快照
```

* **为什么是快照**：与 `biz_task.dept_id`（"创建时负责人所属部门"）同口径。人调部门后，
  历史凭证的部门辅助账不跟着漂移 —— 凭证是审计证据，不允许事后变。
* **为什么采购 / 收货凭证不写部门**：`biz_purchase_order` / `biz_stock_in` **没有 `dept_id`**，
  "谁买的"在系统里没有权威字段（DC-11 原文指向 DC-16）。按"操作人部门"猜一个值等于
  给财务一张错的部门辅助账，所以**故意留空**。要让采购也归部门，先给采购单加 `dept_id`
  （那是 DC-16 的项目/部门归集设计，另一张卡）。
* **为什么银行行不挂部门**：1002 银行存款是公司级账户，挂了会让同一个账户按部门拆成多份，
  银行对账直接对不上。
* 系统账号（AI / 集成账号）没有部门 → 留空 + `WARN`，不阻断付款入账（部门是增强维度，
  不是记账前提）。

## 4. 多物料单：为什么物料维度留空（**已知缺口**）

采购入库 / 出库的存货借方是**一条汇总行**（`purchase_in_lines` 用单头金额出一行 1405），
而分录只有一组辅助列。此时：

```
写 first_material  → 整笔金额挂到首个物料上 → 物料成本报表虚增（错账）
留空（当前做法）    → 该凭证的成本查不到物料维度（信息缺失，但不误导）
```

当前实现取后者：`material_count > 1` 时 `aux_material_id = NULL` + `tracing::warn!`
（日志点名单号与物料种类数）。单物料单照常写值。

**正确解法是按物料拆分录**（`biz_stock_in_item.amount` 逐行拆，末行吸收尾差，
总额不变）。它已经是盘点凭证的做法（`stocktake_model` 逐物料出行），但动收货/出库的
分录取数会改 `entry_count`、影响 D3 影子对比与一批既有断言，属独立决议 → 见 P2-1b 卡。

## 5. 辅助账接口

```
GET /business/erp/ledger/aux-balance
      ?aux_type=MATERIAL|WAREHOUSE|DEPT|SUPPLIER   # 必填，大小写不敏感
      &aux_id=<uuid>                               # 可选：不给 = 该维度全部辅助项
      &period_from=2026-11&period_to=2026-11       # 可选：yyyy-MM 或 yyyy-MM-dd

权限码：business:erp:ledger:aux-balance（V0214）
```

返回（节选）：

```json
{
  "aux_type": "MATERIAL",
  "aux_id": "…", "aux_name": "冷轧钢板 1.5mm",
  "date_from": "2026-11-01", "date_to": "2026-11-30",
  "rows": [{
    "aux_id": "…", "aux_name": "冷轧钢板 1.5mm",
    "account_code": "1405", "account_name": "库存商品",
    "opening_debit": "100.0000", "opening_credit": "0.0000",
    "period_debit": "800.0000", "period_credit": "80.0000",
    "closing_debit": "820.0000", "closing_credit": "0.0000"
  }],
  "total_period_debit": "800.0000", "total_period_credit": "80.0000",
  "unassigned": { "entry_count": 3, "period_debit": "1210.0000", "period_credit": "0.0000" }
}
```

口径要点：

* 只算 `status='POSTED'` 且凭证未软删；
* 红字凭证金额恒为正、借贷互换 ⇒ 净额天然为负，不需要特判；
* `closing = opening + 本期借 − 本期贷`，再按正负拆借 / 贷两侧；
* 一个辅助项在**不同科目上各占一行**（1405 / 2202 / 6401），不做科目间抵销；
* **不做 `Σ借 = Σ贷` 断言**：按辅助维度筛出来的子集本来就不平衡（物料的 1405 借方
  对应供应商的 2202 贷方，不在同一辅助项下）。全公司口径的平衡由 `trial-balance` 负责。
* **`unassigned` 是自证字段**：本期该维度为空的发生额。只回"有值的那些"会让人以为
  查到的就是全部（多物料汇总行、银行行、手工凭证本来就不挂维度）。

## 6. 存量不回填

历史凭证的辅助列保持为空，**没有任何回填脚本**：凭证是审计证据，事后改历史凭证的
辅助维度等于篡改证据。历史期间要按物料看成本，用「库存流水」
（`biz_inventory_txn`）而不是凭证辅助账。
