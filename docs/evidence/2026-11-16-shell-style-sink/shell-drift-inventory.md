# 壳样式（shell）变体漂移清单

> 由卡 `t_3a87ffac`（壳样式下沉）产出。**只出清单，不动手**：这些是各页 `<style>` 里
> 仍然保留的本地定义，和 `src/style/shell.less` 的全局基线不一致。合并任何一条都会改观感，
> 必须逐页截图比对后再动。

## 背景：为什么还剩这么多

下沉只做了「与基线逐字相同 → 删本地」这一件事（248 块 / 1377 行）。不一致的一律保留，
靠 scoped 的 `[data-v-x]`（0,2,0）压过全局裸类（0,1,0）。因此下面这些变体**目前是生效的**，
清单的价值是「哪些页在偷偷不一样」。

## 一、没有进全局基线的 4 个类（实测全局化会改观感，必须成对改）

| 类 | 为什么不能只发全局基线 |
|---|---|
| `.panel-empty` | 多个页面本地只写 `padding + text-align`（没有 `display`）；补上基线的 `display:flex; flex-direction:column; align-items:center; gap:4px` 会把块级空态改成居中 flex，实测 8 个页面空态高度/文字宽度变化 |
| `.panel-empty__title` | 本地常只写 `margin + font-weight`；补 `font-size:14px; color:var(--td-text-color-secondary)` 会改字号与颜色 |
| `.panel-empty__desc` | 同上（补 `margin:0` 会改行距） |
| `.tool-row__label` | 4 个报表页本地只有 `color`；补 `font-size:13px` 会让原本继承 14px 的标签缩水 |

## 二、各类变体分布（改造后现状）

「基线」= `shell.less` 里的全局值；「本地保留」= 各页 scoped 里仍在生效的定义。

### `.panel-empty` —— 37 个页面有本地定义，8 种变体（**未进基线**，见第一节）

- **19 页** —— `display: flex`；`flex-direction: column`；`align-items: center`；`gap: 4px`；`padding: 24px 0`
  - `views/business/erp/account/index.vue`
  - `views/business/erp/commission-rule/index.vue`
  - `views/business/erp/commission/index.vue`
  - `views/business/erp/customer-price/index.vue`
  - `views/business/erp/exchange-rate/index.vue`
  - `views/business/erp/expense/pay/index.vue`
  - `views/business/erp/inventory/index.vue`
  - `views/business/erp/ledger/index.vue`
  - `views/business/erp/location/index.vue`
  - `views/business/erp/payable/index.vue`
  - `views/business/erp/period/index.vue`
  - `views/business/erp/purchase-order-item/index.vue`
  - `views/business/erp/purchase/index.vue`
  - `views/business/erp/seq/index.vue`
  - `views/business/erp/serial/index.vue`
  - `views/business/erp/stockin/index.vue`
  - `views/business/erp/transfer/index.vue`
  - `views/business/erp/voucher/index.vue`
  - `views/business/flow/def/index.vue`
- **6 页** —— `display: flex`；`flex-direction: column`；`align-items: center`；`justify-content: center`；`gap: 6px`；`padding: 48px 16px`；`text-align: center`
  - `views/business/erp/customer/index.vue`
  - `views/business/erp/material/index.vue`
  - `views/business/erp/supplier/index.vue`
  - `views/business/erp/unit/index.vue`
  - `views/business/erp/warehouse/index.vue`
  - `views/business/hr/employee/index.vue`
- **6 页** —— `padding: 40px 0`；`text-align: center`
  - `views/business/erp/stock-take/index.vue`
  - `views/business/hr/leave/index.vue`
  - `views/business/impexp/export-task/index.vue`
  - `views/business/impexp/import-task/index.vue`
  - `views/business/storage/backend/index.vue`
  - `views/business/storage/file/index.vue`
- **2 页** —— `padding: 36px 0`；`text-align: center`
  - `views/business/erp/contract/index.vue`
  - `views/business/erp/stock-out/index.vue`
- **1 页** —— `padding: 32px 0`；`text-align: center`
  - `views/business/erp/lot/index.vue`
- **1 页** —— `padding: 28px 0`；`text-align: center`
  - `views/business/hr/attendance/index.vue`
- **1 页** —— `padding: 32px 12px`；`text-align: center`；`color: var(--td-text-color-secondary, #00000099)`
  - `views/business/hr/salary/detail.vue`
- **1 页** —— `padding: 40px 16px`；`text-align: center`；`font-size: 13px`；`color: var(--td-text-color-placeholder)`
  - `views/business/hr/salary/index.vue`

### `.panel-empty__title` —— 37 个页面有本地定义，6 种变体（**未进基线**，见第一节）

- **25 页** —— `margin: 0`；`font-size: 14px`；`color: var(--td-text-color-secondary)`
  - `views/business/erp/account/index.vue`
  - `views/business/erp/commission-rule/index.vue`
  - `views/business/erp/commission/index.vue`
  - `views/business/erp/customer-price/index.vue`
  - `views/business/erp/customer/index.vue`
  - `views/business/erp/exchange-rate/index.vue`
  - `views/business/erp/expense/pay/index.vue`
  - `views/business/erp/inventory/index.vue`
  - `views/business/erp/ledger/index.vue`
  - `views/business/erp/location/index.vue`
  - `views/business/erp/material/index.vue`
  - `views/business/erp/payable/index.vue`
  - `views/business/erp/period/index.vue`
  - `views/business/erp/purchase-order-item/index.vue`
  - `views/business/erp/purchase/index.vue`
  - `views/business/erp/seq/index.vue`
  - `views/business/erp/serial/index.vue`
  - `views/business/erp/stockin/index.vue`
  - `views/business/erp/supplier/index.vue`
  - `views/business/erp/transfer/index.vue`
  - `views/business/erp/unit/index.vue`
  - `views/business/erp/voucher/index.vue`
  - `views/business/erp/warehouse/index.vue`
  - `views/business/flow/def/index.vue`
  - `views/business/hr/employee/index.vue`
- **4 页** —— `margin: 8px 0 4px`；`font-weight: 600`
  - `views/business/erp/contract/index.vue`
  - `views/business/erp/lot/index.vue`
  - `views/business/hr/attendance/index.vue`
  - `views/business/hr/leave/index.vue`
- **4 页** —— `margin: 12px 0 4px`；`font-size: 15px`；`color: var(--td-text-color-primary)`
  - `views/business/impexp/export-task/index.vue`
  - `views/business/impexp/import-task/index.vue`
  - `views/business/storage/backend/index.vue`
  - `views/business/storage/file/index.vue`
- **2 页** —— `margin: 8px 0 4px`；`font-size: 14px`；`color: var(--td-text-color-primary)`
  - `views/business/erp/stock-out/index.vue`
  - `views/business/erp/stock-take/index.vue`
- **1 页** —— `margin: 8px 0 4px`；`font-size: 14px`；`color: var(--td-text-color-primary, #000000e6)`
  - `views/business/hr/salary/detail.vue`
- **1 页** —— `margin: 0`；`line-height: 20px`
  - `views/business/hr/salary/index.vue`

### `.panel-empty__desc` —— 35 个页面有本地定义，10 种变体（**未进基线**，见第一节）

- **20 页** —— `margin: 0`；`font-size: 12px`；`color: var(--td-text-color-placeholder)`
  - `views/business/erp/account/index.vue`
  - `views/business/erp/commission/index.vue`
  - `views/business/erp/customer-price/index.vue`
  - `views/business/erp/exchange-rate/index.vue`
  - `views/business/erp/expense/pay/index.vue`
  - `views/business/erp/inventory/index.vue`
  - `views/business/erp/ledger/index.vue`
  - `views/business/erp/location/index.vue`
  - `views/business/erp/payable/index.vue`
  - `views/business/erp/period/index.vue`
  - `views/business/erp/purchase-order-item/index.vue`
  - `views/business/erp/purchase/index.vue`
  - `views/business/erp/seq/index.vue`
  - `views/business/erp/serial/index.vue`
  - `views/business/erp/stock-out/index.vue`
  - `views/business/erp/stock-take/index.vue`
  - `views/business/erp/stockin/index.vue`
  - `views/business/erp/transfer/index.vue`
  - `views/business/erp/voucher/index.vue`
  - `views/business/flow/def/index.vue`
- **4 页** —— `margin-bottom: 16px`；`font-size: 13px`；`color: var(--td-text-color-secondary)`
  - `views/business/impexp/export-task/index.vue`
  - `views/business/impexp/import-task/index.vue`
  - `views/business/storage/backend/index.vue`
  - `views/business/storage/file/index.vue`
- **3 页** —— `margin: 0`；`font-size: 12px`；`color: var(--td-text-color-placeholder)`；`max-width: 460px`；`line-height: 1.6`
  - `views/business/erp/customer/index.vue`
  - `views/business/erp/supplier/index.vue`
  - `views/business/erp/unit/index.vue`
- **2 页** —— `margin: 0`；`font-size: 12px`；`color: var(--td-text-color-placeholder)`；`max-width: 420px`；`line-height: 1.6`
  - `views/business/erp/material/index.vue`
  - `views/business/erp/warehouse/index.vue`
- **1 页** —— `margin: 0`；`color: var(--td-text-color-secondary)`；`font-size: 13px`
  - `views/business/erp/contract/index.vue`
- **1 页** —— `color: var(--td-text-color-secondary)`；`font-size: 12px`
  - `views/business/erp/lot/index.vue`
- **1 页** —— `margin: 0`；`font-size: 12px`；`color: var(--td-text-color-secondary)`
  - `views/business/hr/attendance/index.vue`
- **1 页** —— `margin: 0`；`font-size: 12px`；`color: var(--td-text-color-placeholder)`；`max-width: 460px`；`line-height: 20px`
  - `views/business/hr/employee/index.vue`
- **1 页** —— `margin: 0`；`color: var(--td-text-color-placeholder, #0000004d)`；`font-size: 13px`
  - `views/business/hr/leave/index.vue`
- **1 页** —— `margin: 0`；`font-size: 12px`
  - `views/business/hr/salary/detail.vue`

### `.panel-empty__icon` —— 18 个页面有本地定义，6 种变体（基线：`font-size: 28px`; `color: var(--td-text-color-placeholder)`）

- **8 页** —— `font-size`: 本地 `40px` ≠ 基线 `28px`
  - `views/business/erp/lot/index.vue`
  - `views/business/erp/stock-out/index.vue`
  - `views/business/erp/stock-take/index.vue`
  - `views/business/hr/employee/index.vue`
  - `views/business/impexp/export-task/index.vue`
  - `views/business/impexp/import-task/index.vue`
  - `views/business/storage/backend/index.vue`
  - `views/business/storage/file/index.vue`
- **6 页** —— `font-size`: 本地 `32px` ≠ 基线 `28px`
  - `views/business/erp/customer/index.vue`
  - `views/business/erp/material/index.vue`
  - `views/business/erp/supplier/index.vue`
  - `views/business/erp/unit/index.vue`
  - `views/business/erp/warehouse/index.vue`
  - `views/business/hr/attendance/index.vue`
- **1 页** —— 与基线**逐字相同**（理论上可删，但只剩 1 份时删了就靠全局）
  - `views/business/erp/contract/index.vue`
- **1 页** —— `color`: 本地 `var(--td-success-color)` ≠ 基线 `var(--td-text-color-placeholder)`
  - `views/business/erp/expense/pay/index.vue`
- **1 页** —— `color`: 本地 `var(--td-text-color-placeholder, #0000004d)` ≠ 基线 `var(--td-text-color-placeholder)`；`font-size`: 本地 `32px` ≠ 基线 `28px`
  - `views/business/hr/leave/index.vue`
- **1 页** —— `color`: 本地 `var(--td-text-color-placeholder, #0000004d)` ≠ 基线 `var(--td-text-color-placeholder)`
  - `views/business/hr/salary/detail.vue`

### `.panel-head__tip` —— 17 个页面有本地定义，5 种变体（基线：`font-size: 12px`; `color: var(--td-text-color-placeholder)`）

- **6 页** —— `color`: 本地 `var(--td-text-color-secondary)` ≠ 基线 `var(--td-text-color-placeholder)`
  - `views/business/erp/asset-category/index.vue`
  - `views/business/erp/asset/index.vue`
  - `views/business/erp/depreciation/index.vue`
  - `views/business/erp/expense-type/index.vue`
  - `views/business/erp/expense/index.vue`
  - `views/business/erp/expense/pay/index.vue`
- **6 页** —— 与基线**逐字相同**（理论上可删，但只剩 1 份时删了就靠全局）
  - `views/business/erp/contract/index.vue`
  - `views/business/erp/cost-alloc-rule/index.vue`
  - `views/business/erp/cost-labor/index.vue`
  - `views/business/erp/cost-object/index.vue`
  - `views/business/erp/cost-overhead/index.vue`
  - `views/business/erp/lot/index.vue`
- **2 页** —— `color`: 本地 `var(--td-text-color-secondary)` ≠ 基线 `var(--td-text-color-placeholder)`；`text-align`: 本地多出 `right`
  - `views/business/erp/budget/index.vue`
  - `views/business/erp/budget/mapping.vue`
- **2 页** —— `margin-left`: 本地多出 `auto`
  - `views/business/erp/receipt/index.vue`
  - `views/business/erp/receivable/index.vue`
- **1 页** —— `margin-left`: 本地多出 `auto`；`text-align`: 本地多出 `right`
  - `views/business/erp/invoice/index.vue`

### `.panel-head__title` —— 12 个页面有本地定义，3 种变体（基线：`font-size: 14px`; `font-weight: 600`; `color: var(--td-text-color-primary)`）

- **7 页** —— `font-size`: 本地 `16px` ≠ 基线 `14px`；`color`: 本地未写（基线 `var(--td-text-color-primary)` 会补进来）
  - `views/business/erp/asset/index.vue`
  - `views/business/erp/budget/mapping.vue`
  - `views/business/erp/budget/report.vue`
  - `views/business/erp/sales-order/index.vue`
  - `views/business/erp/sales-return/index.vue`
  - `views/business/erp/shipment/index.vue`
  - `views/business/hr/salary/index.vue`
- **3 页** —— `font-size`: 本地 `15px` ≠ 基线 `14px`；`color`: 本地未写（基线 `var(--td-text-color-primary)` 会补进来）
  - `views/business/erp/cost-period/steps/UncollectedPanel.vue`
  - `views/business/erp/price/index.vue`
  - `views/business/erp/report/report-item/index.vue`
- **2 页** —— `white-space`: 本地多出 `nowrap`
  - `components/ListPage/index.vue`
  - `views/business/erp/warehouse/index.vue`

### `.panel-head` —— 11 个页面有本地定义，8 种变体（基线：`display: flex`; `align-items: baseline`; `justify-content: space-between`; `gap: 12px`; `margin-bottom: 10px`）

- **3 页** —— `margin-bottom`: 本地 `8px` ≠ 基线 `10px`；`justify-content`: 本地未写（基线 `space-between` 会补进来）
  - `views/business/erp/sales-order/index.vue`
  - `views/business/erp/sales-return/index.vue`
  - `views/business/erp/shipment/index.vue`
- **2 页** —— `margin-bottom`: 本地 `12px` ≠ 基线 `10px`
  - `views/business/erp/budget/mapping.vue`
  - `views/business/erp/budget/report.vue`
- **1 页** —— 与基线**逐字相同**（理论上可删，但只剩 1 份时删了就靠全局）
  - `components/ListPage/index.vue`
- **1 页** —— `flex-wrap`: 本地多出 `wrap`
  - `views/business/erp/account/index.vue`
- **1 页** —— `margin-bottom`: 本地 `12px` ≠ 基线 `10px`；`gap`: 本地未写（基线 `12px` 会补进来）
  - `views/business/erp/asset/index.vue`
- **1 页** —— `align-items`: 本地 `center` ≠ 基线 `baseline`；`gap`: 本地 `8px` ≠ 基线 `12px`；`justify-content`: 本地未写（基线 `space-between` 会补进来）；`margin-bottom`: 本地未写（基线 `10px` 会补进来）；`flex-wrap`: 本地多出 `wrap`
  - `views/business/erp/cost-period/steps/UncollectedPanel.vue`
- **1 页** —— `align-items`: 本地 `flex-end` ≠ 基线 `baseline`；`margin-bottom`: 本地 `12px` ≠ 基线 `10px`
  - `views/business/erp/report/report-item/index.vue`
- **1 页** —— `align-items`: 本地 `center` ≠ 基线 `baseline`；`margin-bottom`: 本地 `12px` ≠ 基线 `10px`；`gap`: 本地未写（基线 `12px` 会补进来）
  - `views/business/hr/salary/index.vue`

### `.panel-head__main` —— 11 个页面有本地定义，5 种变体（基线：`display: flex`; `align-items: baseline`; `gap: 10px`）

- **6 页** —— `gap`: 本地 `8px` ≠ 基线 `10px`
  - `views/business/erp/asset/index.vue`
  - `views/business/erp/budget/report.vue`
  - `views/business/erp/price/index.vue`
  - `views/business/erp/sales-order/index.vue`
  - `views/business/erp/sales-return/index.vue`
  - `views/business/erp/shipment/index.vue`
- **2 页** —— `min-width`: 本地多出 `0`
  - `components/ListPage/index.vue`
  - `views/business/erp/warehouse/index.vue`
- **1 页** —— `flex-wrap`: 本地多出 `wrap`
  - `views/business/erp/account/index.vue`
- **1 页** —— `gap`: 本地 `8px` ≠ 基线 `10px`；`white-space`: 本地多出 `nowrap`
  - `views/business/erp/budget/mapping.vue`
- **1 页** —— `align-items`: 本地 `center` ≠ 基线 `baseline`；`gap`: 本地 `8px` ≠ 基线 `10px`；`flex-wrap`: 本地多出 `wrap`
  - `views/business/erp/cost-period/steps/UncollectedPanel.vue`

### `.panel-head__meta` —— 10 个页面有本地定义，4 种变体（基线：`font-size: 12px`; `color: var(--td-text-color-placeholder)`）

- **6 页** —— 与基线**逐字相同**（理论上可删，但只剩 1 份时删了就靠全局）
  - `views/business/erp/asset/index.vue`
  - `views/business/erp/budget/mapping.vue`
  - `views/business/erp/budget/report.vue`
  - `views/business/erp/sales-order/index.vue`
  - `views/business/erp/sales-return/index.vue`
  - `views/business/erp/shipment/index.vue`
- **2 页** —— `white-space`: 本地多出 `nowrap`
  - `components/ListPage/index.vue`
  - `views/business/erp/warehouse/index.vue`
- **1 页** —— `color`: 本地 `var(--td-text-color-secondary, #666)` ≠ 基线 `var(--td-text-color-placeholder)`
  - `views/business/erp/cost-period/steps/UncollectedPanel.vue`
- **1 页** —— `color`: 本地 `var(--td-text-color-placeholder, #0000004d)` ≠ 基线 `var(--td-text-color-placeholder)`；`font-size`: 本地 `13px` ≠ 基线 `12px`
  - `views/business/hr/salary/index.vue`

### `.tool-row__label` —— 10 个页面有本地定义，4 种变体（**未进基线**，见第一节）

- **4 页** —— `color: var(--td-text-color-secondary)`；`font-size: 13px`
  - `views/business/erp/invoice/index.vue`
  - `views/business/erp/receipt-refund/index.vue`
  - `views/business/erp/receipt/index.vue`
  - `views/business/erp/receivable/index.vue`
- **4 页** —— `color: var(--td-text-color-secondary)`
  - `views/business/erp/report/balance-sheet/index.vue`
  - `views/business/erp/report/cash-flow/index.vue`
  - `views/business/erp/report/income-statement/index.vue`
  - `views/business/erp/report/payable-aging/index.vue`
- **1 页** —— `color: var(--td-text-color-secondary)`；`font-size: 13px`；`white-space: nowrap`
  - `views/business/erp/contract/index.vue`
- **1 页** —— `color: var(--td-text-color-secondary)`；`font-size: 12px`
  - `views/business/erp/cost-labor/index.vue`

### `.tool-row` —— 8 个页面有本地定义，4 种变体（基线：`display: flex`; `align-items: center`; `gap: 12px`; `flex-wrap: wrap`）

- **4 页** —— `margin-bottom`: 本地多出 `12px`
  - `views/business/erp/report/balance-sheet/index.vue`
  - `views/business/erp/report/cash-flow/index.vue`
  - `views/business/erp/report/income-statement/index.vue`
  - `views/business/erp/report/payable-aging/index.vue`
- **2 页** —— `gap`: 本地 `8px` ≠ 基线 `12px`
  - `views/business/erp/budget/mapping.vue`
  - `views/business/erp/budget/report.vue`
- **1 页** —— `margin`: 本地多出 `8px 0 12px`
  - `views/business/erp/ledger/index.vue`
- **1 页** —— `gap`: 本地 `8px` ≠ 基线 `12px`
  - `views/business/erp/receivable/index.vue`

### `.tool-row__spacer` —— 5 个页面有本地定义，1 种变体（基线：`flex: 1 1 auto`）

- **5 页** —— `flex`: 本地 `1` ≠ 基线 `1 1 auto`
  - `views/business/erp/budget/mapping.vue`
  - `views/business/erp/budget/report.vue`
  - `views/business/erp/receivable/index.vue`
  - `views/business/erp/report/report-item/index.vue`
  - `views/business/erp/year-end/index.vue`

### `.tool-row__period` —— 5 个页面有本地定义，3 种变体（基线：`width: 220px`）

- **2 页** —— `width`: 本地 `140px` ≠ 基线 `220px`
  - `views/business/erp/cost-alloc-rule/index.vue`
  - `views/business/erp/ledger/index.vue`
- **2 页** —— `width`: 本地 `132px` ≠ 基线 `220px`
  - `views/business/erp/cost-labor/index.vue`
  - `views/business/erp/cost-overhead/index.vue`
- **1 页** —— `width`: 本地 `200px` ≠ 基线 `220px`
  - `views/business/erp/report/payable-aging/index.vue`

### `.panel-head__hint` —— 4 个页面有本地定义，2 种变体（基线：`font-size: 12px`; `color: var(--td-text-color-placeholder)`）

- **3 页** —— `margin-left`: 本地多出 `auto`
  - `views/business/erp/sales-order/index.vue`
  - `views/business/erp/sales-return/index.vue`
  - `views/business/erp/shipment/index.vue`
- **1 页** —— `line-height`: 本地多出 `18px`；`max-width`: 本地多出 `460px`；`text-align`: 本地多出 `right`
  - `views/business/hr/employee/index.vue`

### `.tool-row__dept` —— 2 个页面有本地定义，2 种变体（未进基线：仅单页使用）

- **1 页** —— `width: 180px`
  - `views/business/erp/budget/mapping.vue`
- **1 页** —— `width: 160px`
  - `views/business/erp/budget/report.vue`

### `.tool-row__year` —— 2 个页面有本地定义，2 种变体（未进基线：仅单页使用）

- **1 页** —— `width: 112px`
  - `views/business/erp/budget/report.vue`
- **1 页** —— `width: 160px`
  - `views/business/erp/year-end/index.vue`

### `.tool-row__date` —— 2 个页面有本地定义，2 种变体（基线：`width: 150px`）

- **1 页** —— `width`: 本地 `140px` ≠ 基线 `150px`
  - `views/business/erp/receipt-refund/index.vue`
- **1 页** —— `width`: 本地 `180px` ≠ 基线 `150px`
  - `views/business/erp/report/payable-aging/index.vue`

### `.list-card` —— 2 个页面有本地定义，2 种变体（基线：`flex: 1 1 auto`; `min-width: 0`）

- **1 页** —— `flex`: 本地 `0 0 320px` ≠ 基线 `1 1 auto`；`min-width`: 本地未写（基线 `0` 会补进来）；`display`: 本地多出 `flex`；`flex-direction`: 本地多出 `column`；`min-height`: 本地多出 `0`；`width`: 本地多出 `320px`
  - `views/business/help/center/index.vue`
- **1 页** —— `flex`: 本地未写（基线 `1 1 auto` 会补进来）；`min-width`: 本地未写（基线 `0` 会补进来）；`margin-top`: 本地多出 `16px`
  - `views/support/config/index.vue`

### `.tool-row__category` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `width: 130px`
  - `views/business/erp/account/index.vue`

### `.tool-row__type` —— 1 个页面有本地定义，1 种变体（基线：`width: 140px`）

- **1 页** —— `width`: 本地 `150px` ≠ 基线 `140px`
  - `views/business/erp/budget/mapping.vue`

### `.tool-row__keyword` —— 1 个页面有本地定义，1 种变体（基线：`width: 220px`）

- **1 页** —— `width`: 本地 `200px` ≠ 基线 `220px`
  - `views/business/erp/budget/report.vue`

### `.tool-row__hint` —— 1 个页面有本地定义，1 种变体（基线：`color: var(--td-text-color-placeholder)`; `font-size: 12px`）

- **1 页** —— 与基线**逐字相同**（理论上可删，但只剩 1 份时删了就靠全局）
  - `views/business/erp/budget/report.vue`

### `.panel-head__actions` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `display: flex`；`gap: 8px`
  - `views/business/erp/budget/report.vue`

### `.tool-row__month` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `width: 132px`
  - `views/business/erp/cost-object/index.vue`

### `.tool-row__item` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `width: 150px`
  - `views/business/erp/cost-overhead/index.vue`

### `.panel-head__ops` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `margin-left: auto`；`display: flex`；`gap: 8px`
  - `views/business/erp/cost-period/steps/UncollectedPanel.vue`

### `.tool-row__tip` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `color: var(--td-text-color-secondary)`；`font-size: 12px`
  - `views/business/erp/expense/pay/index.vue`

### `.tool-row__switch` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `display: inline-flex`；`align-items: center`；`gap: 8px`；`font-size: 13px`；`color: var(--td-text-color-secondary)`；`cursor: pointer`
  - `views/business/erp/inventory/index.vue`

### `.panel-head__side` —— 1 个页面有本地定义，1 种变体（基线：`display: flex`; `align-items: center`; `gap: 6px`）

- **1 页** —— `align-items`: 本地 `baseline` ≠ 基线 `center`；`display`: 本地 `inline-flex` ≠ 基线 `flex`；`gap`: 本地 `12px` ≠ 基线 `6px`；`flex-wrap`: 本地多出 `wrap`
  - `views/business/erp/ledger/index.vue`

### `.panel-head__account` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `color: var(--td-text-color-secondary)`；`font-weight: 400`
  - `views/business/erp/ledger/index.vue`

### `.panel-head__link` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `color: var(--td-brand-color)`；`text-decoration: none`
  - `views/business/erp/location/index.vue`

### `.panel-head__warn` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `font-size: 12px`；`color: var(--td-warning-color)`
  - `views/business/erp/serial/index.vue`

### `.tool-row__range` —— 1 个页面有本地定义，1 种变体（基线：`width: 250px`）

- **1 页** —— `width`: 本地 `240px` ≠ 基线 `250px`
  - `views/business/erp/stock-out/index.vue`

### `.tool-row__status` —— 1 个页面有本地定义，1 种变体（基线：`width: 130px`）

- **1 页** —— `width`: 本地 `140px` ≠ 基线 `130px`
  - `views/business/erp/transfer/index.vue`

### `.tool-row__overdue` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `margin-right: -2px`；`white-space: nowrap`
  - `views/business/erp/transfer/index.vue`

### `.panel-head__overdue` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `cursor: pointer`；`transition: filter 0.15s ease`
  - `views/business/erp/transfer/index.vue`

### `.panel-head__sub` —— 1 个页面有本地定义，1 种变体（未进基线：仅单页使用）

- **1 页** —— `font-size: 12px`；`color: var(--td-text-color-secondary)`；`overflow: hidden`；`text-overflow: ellipsis`；`white-space: nowrap`
  - `views/business/erp/warehouse/index.vue`

## 三、建议的推进顺序（给后续卡）

1. 先合「只差 1 个属性」的类（`.tool-row__spacer` 的 `flex:1` vs `flex:1 1 auto`、`.panel-head__meta` 的 `white-space:nowrap`）；
2. 再合「只差 gap / margin」的（`.tool-row` 的 8px vs 12px、`.panel-head` 的 margin-bottom 8/10/12）；
3. 最后才动 `.panel-empty*` 与 `.tool-row__label` —— 这两组要连同页面空态一起改，逐页截图；
4. 每合一条就删掉该页本地定义，`shell.less` 保持单一真源。

