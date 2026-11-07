# ERP 税额与含税口径（FIX-6）

> 卡：`t_728f716e`【FIX-6】税务与科目：tax_rate 标度统一 + 期初科目可配 + 含税口径 + 留抵重分类
> 迁移：`V0327__tax_rate_percent_scale`、`V0328__opening_equity_account_config`、
> `V0329__report_item_negative_reclass`
> 代码：`backend/src/business/erp/tax_rate.rs`（唯一入口）、`invoice.rs`、`dto.rs`/`handler.rs`（物料）、
> `sales_return_posting.rs`、`opening.rs`、`report.rs`

本文是这四件事的**口径唯一来源**。改一处必须同步本文件 + 对应迁移 + 单测。

---

## 1. 税率只有一个标度：**百分数**（`13` = 13%）

### 1.1 历史问题（100 倍税差）

| 落库位置 | FIX-6 之前 | 13% 存成 | 现在 |
|---|---|---|---|
| `biz_sales_order_item.tax_rate` | 百分数 | `13.0000` | `13.0000` |
| `biz_purchase_order_item.tax_rate` | 百分数 | `13.0000` | `13.0000` |
| `biz_stock_in_item.tax_rate` | 百分数 | `13.0000` | `13.0000` |
| `biz_sales_return_item.tax_rate` | 百分数 | `13.0000` | `13.0000` |
| **`biz_invoice.tax_rate`** | **小数（≤1）** | `0.1300` | **`13.0000`** |
| **`biz_material.tax_rate`** | **小数（≤1）** | `0.1300` | **`13.0000`** |
| `biz_expense_item.tax_rate` | 小数（≤1） | `0.1300` | 未改（**报销单独一张卡**，见 §6 遗留项） |

同一张单上「订单行 13 / 发票 0.13」并存，任何一处按错标度相乘就是 **100 倍税差**，
而且 `POST /invoice {"tax_rate":13}` 会直接被 400 顶掉（`ck_biz_invoice_tax_rate: <= 1`）。

### 1.2 换算公式（唯一实现：`tax_rate::tax_of` / `split`）

```text
税额   = round(不含税金额 × 税率 / 100, 4)
不含税 = round(价税合计 / (1 + 税率/100), 4)
税额   = 价税合计 − 不含税        ← 先定不含税，保证「不含税 + 税额 ≡ 价税合计」
```

发票（`ck_biz_invoice_amount`）、销售订单（`ck_sales_order_amount`）、采购/收货
（`ck_biz_po_item_tax` / `ck_biz_si_item_tax`）都是**精确恒等**约束，所以必须先定不含税再取差，
反过来先算税额会留 1 分尾差直接撞约束。

### 1.3 存量为 0.13 的数据怎么迁的（V0327）

- 只迁 `(0, 1]` 区间：`0`（免税）永远不放大；
- 迁完把 `tax_rate_mode` 标成 `DECIMAL`（由小数迁移而来），其余标 `PERCENT`；
- **不动任何金额列**（`amount` / `tax_amount` / `total_with_tax`），历史单据金额一分不变；
- `biz_invoice.tax_rate` 从 `NUMERIC(6,4)` 放宽到 `NUMERIC(7,4)`（否则 `100.0000` 存不下），默认值 `13.0000`。

### 1.4 过渡期：`tax_rate_mode`（可空，`PERCENT` / `DECIMAL`）

老前端仍可能传小数，三种解析模式（`tax_rate::TaxRateMode`）：

| 入参 | `tax_rate_mode=PERCENT` | `tax_rate_mode=DECIMAL` | 不传（LEGACY，过渡期默认） |
|---|---|---|---|
| `13` | 13% | 400（小数模式上限 1） | 13% |
| `0.13` | 0.13%（合法但语义不同） | 13% | **13% + WARN 日志** |
| `1` | **1%** | 100% | **100% + WARN 日志** |
| `0` | 0%（免税） | 0%（免税） | 0%（免税，永不放大） |

- 歧义只有 `(0, 1]` 这一段（`1` 到底是 1% 还是 100%），系统无从判断 ⇒ 一律 ×100 并打
  `WARN 税率按小数（0–1）传入，已自动 ×100`；
- 要表达「1%」必须显式传 `tax_rate_mode = "PERCENT"`（新前端就是这么传的）；
- 实际采用的标度落在 `biz_invoice.tax_rate_mode` / `biz_material.tax_rate_mode`（审计位），
  **过渡期结束后删列、删 LEGACY 分支**。

### 1.5 前端

- 展示：`taxRateText(13)` → `13%`（**不再 ×100**，`0.13` 现在显示 `0.13%`）；
- 开票弹窗：输入 `0–100`、`suffix="%"`、默认 `13`，提示「税率按百分数填（13 = 13%）」；
- 物料页没有税率表单（`biz_material.tax_rate` 只作默认值存着），无需改。

---

## 2. 含税口径与勾稽公式（问题 3）

**结论：应付 / 应收的金额口径是「价税合计」，不是「不含税金额」。**
`biz_sales_order.total_amount` 是**不含税**合计，它永远不等于应付金额 —— 差的就是税额。

| 对象 | 字段 | 公式 | 库层约束 |
|---|---|---|---|
| 销售订单（单头） | `total_amount` / `total_tax` / `total_with_tax` | `total_with_tax = total_amount + total_tax`（精确恒等） | `ck_sales_order_amount` |
| 销售订单（行） | `amount` / `tax_amount` | `amount = round(price × qty, 4)`；`tax_amount = round(amount × tax_rate/100, 4)` | `ck_biz_so_item_*` |
| 应收（销售出库挂账） | `biz_receivable.amount` | `Σ行(amount + tax_amount)`，逐行定标后求和 | — |
| **应付金额** | — | **= 订单 `total_with_tax`**（整单发货时 Σ应收 = 订单价税合计，分文不差） | 单测 `full_shipment_total_equals_order_line_total` |
| 采购订单 | `with_tax_amount` | `= amount + tax_amount` | `ck_biz_po_item_tax` |
| 发票 | `total_with_tax` | `= amount + tax_amount` | `ck_biz_invoice_amount` |

### 2.1 勾稽公式（写死在这，别在各处再推导一遍）

```text
① 订单级：   total_with_tax(订单) = total_amount + total_tax         -- 库层 CHECK，恒成立
② 挂账级：   应收.amount = Σ 行 round(qty×price,4) + round(amount×rate/100,4)   -- 逐行定标
③ 整单发货： 应收.amount = 订单.total_with_tax                        -- ①②合起来就是这句
④ 应付口径： 应付金额 = total_with_tax（不是 total_amount）
⑤ 开票额度： SUM(发票.total_with_tax WHERE receivable_id=? AND status<>'VOID') ≤ 应收.amount
```

**为什么 `total_amount` 看起来「对不上」**：2000（不含税）× 13% = 260 税额，价税合计 2260。
「应付 2260」对应的是 `total_with_tax` / 应收金额 / 发票价税合计，**不是** `total_amount`。
之前被记成 bug，是因为拿 `total_amount`（不含税）去比应付（含税）—— 口径不同，不是数据错。

### 2.2 自查 SQL（145 测试库 / 144 生产库都跑过）

```sql
-- ① 订单三列恒等（应为 0 行）
SELECT count(*) FROM biz_sales_order
 WHERE round(total_with_tax,4) <> round(total_amount + total_tax,4) AND deleted_at IS NULL;

-- ② 有明细但价税合计为 0（「没值」的订单，应为 0 行）
SELECT count(*) FROM biz_sales_order o
 WHERE o.deleted_at IS NULL AND round(o.total_with_tax,4) = 0
   AND EXISTS (SELECT 1 FROM biz_sales_order_item i WHERE i.so_id = o.id);

-- ③ 应收金额 = 归属订单的价税合计（整单发货的样本，应全部差 0）
SELECT o.so_no, o.total_with_tax, r.amount, round(r.amount - o.total_with_tax,4) AS diff
  FROM biz_sales_order o JOIN biz_receivable r ON r.source_no = ...
```

---

## 3. 资产负债表「应交税费」为负数 → 留抵税额重分类（问题 4）

### 3.1 现象与口径

进项 260 > 销项 52 ⇒ `2221 应交税费` 期末**借方余额** 208。报表按「负债类科目贷方为正」取数，
于是「应交税费」列示 **-208**。会计上这不是「负负债」，而是**留抵税额**（可结转下期抵扣），
应当重分类到资产侧「其他流动资产 - 待抵扣进项税额」列报。

### 3.2 实现（配置驱动，不写死科目）

- `biz_report_item` 新增列 `reclass_negative_to`（弱引用同报表的 `item_code`）；
- V0329 把 `TAXES_PAYABLE`（应交税费）的该列指向 `OTHER_CURRENT_ASSETS`（其他流动资产）；
- `report::compute()` 多跑一遍「第 1.5 遍」：本项算出**负数**时，把 `-金额` 加到目标项、
  自己按 `0` 列示，并在 `reclass[]` 留一条可审计明细（来源项 / 目标项 / 金额 / 涉及科目）。

### 3.3 为什么重分类不会把报表改不平

- 只搬**列报金额**（`amounts`），不动 `contributions`（即「该侧科目净额被哪些报表项认领」）；
- 科目仍是负债类科目、认领关系不变 ⇒ 两侧 `UNMAPPED_*` 仍然为 0；
- 资产侧 `+208` 与负债侧 `-208 → 0` 同额移动 ⇒ `资产总计 ≡ 负债+权益总计` 不变；
- 目标项在 `CURRENT_ASSETS_TOTAL` 的算式里（V0329 自检会核对），否则搬过去也不进合计。

### 3.4 附注

出参新增 `notes[]`（人话附注）、`reclass[]`（结构化明细）。发生重分类时附注固定为：

> 「应交税费」期末为借方余额 208 元（进项税额大于销项税额 ⇒ 留抵税额），已按报表口径重分类到
> 「其他流动资产 - 待抵扣进项税额」列报（涉及科目：2221.01 / 2221.02）：**负数 = 留抵税额，可结转下期继续抵扣**。

「应交税费」行自身的「取数说明」也会写明「期末为借方余额 N，已重分类…本行按 0 列示」，
避免看报表的人以为取数失败。

---

## 4. 库存期初「对方科目」可配（问题 2）

- 系统参数：`sys_config.inventory_opening_equity_account`（V0328 种，默认 `3103` 本年利润）；
- 生成期初凭证：`借 1405 库存商品 / 贷 <配置科目>`；
- **同一科目**用于方式 A / B 的互斥校验与返回体 `inventory_credit_account` 提示；
- 配置成不存在 / 非末级科目 → 400 点名字段，**不静默回落 3103**（静默回落等于「改了配置没生效」）；
- 取值：`opening::inventory_equity_account_code()`（`sys_config` 读不到 → 回退常量 3103）。

验收口径：把参数改成 `3101` 后，**新生成**的期初凭证贷 3101、3103 一分钱都没有，借贷仍平衡
（单测 `r5_10b_inventory_offset_account_follows_config`）。已生成的旧凭证不回写。

---

## 5. 遗留项（不在本卡范围）

| 项 | 现状 | 说明 |
|---|---|---|
| `biz_expense_item.tax_rate` | 仍是小数 `0~1`（`ck_biz_exp_item_rate`） | 报销模块自己的价税分离（`expense_compute::split`），改动会影响报销单金额与审批快照，**单独立卡**；换标度时用 `tax_rate::to_decimal` 换算，别自己写 `/100` |
| `biz_supplier.tax_rate` | 列在库里，**全仓无读写**（摆设字段） | 若要启用，按本文件 §1 的百分数口径 |
| `tax_rate_mode` 两列 | 过渡期审计位 | 过渡期结束后：删列 + 删 `TaxRateMode::Legacy` 分支 + 删 `WARN` |
