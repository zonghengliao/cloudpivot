# 壳样式变体明细（`frontend/scripts/shell-variant-report.cjs --md` 产出，勿手改）

## B0 组件自持（allowlist，不动） —— 4 块 / 1 文件 / 约 24 行

- `components/ListPage/index.vue` — panel-head、panel-head__main、panel-head__title、panel-head__meta

## B5 空态与标签变体族（panel-empty* + tool-row__label） —— 136 块 / 46 文件 / 约 803 行

- `views/business/erp/account/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/commission-rule/index.vue` — panel-empty、panel-empty__title
- `views/business/erp/commission/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/contract/index.vue` — tool-row__label、panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/cost-labor/index.vue` — tool-row__label
- `views/business/erp/customer-price/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/customer/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/exchange-rate/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/expense/pay/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/inventory/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/invoice/index.vue` — tool-row__label
- `views/business/erp/ledger/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/location/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/lot/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/material/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/payable/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/period/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/purchase-order-item/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/purchase/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/receipt-refund/index.vue` — tool-row__label
- `views/business/erp/receipt/index.vue` — tool-row__label
- `views/business/erp/receivable/index.vue` — tool-row__label
- `views/business/erp/report/balance-sheet/index.vue` — tool-row__label
- `views/business/erp/report/cash-flow/index.vue` — tool-row__label
- `views/business/erp/report/income-statement/index.vue` — tool-row__label
- `views/business/erp/report/payable-aging/index.vue` — tool-row__label
- `views/business/erp/seq/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/serial/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/stock-out/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/stock-take/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/stockin/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/supplier/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/transfer/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/unit/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/erp/voucher/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/erp/warehouse/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/flow/def/index.vue` — panel-empty、panel-empty__title、panel-empty__desc
- `views/business/hr/attendance/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/hr/employee/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/hr/leave/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/hr/salary/detail.vue` — panel-empty、panel-empty--inline、panel-empty__title、panel-empty__desc
- `views/business/hr/salary/index.vue` — panel-empty、panel-empty__title
- `views/business/impexp/export-task/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/impexp/import-task/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/storage/backend/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc
- `views/business/storage/file/index.vue` — panel-empty、panel-empty__icon、panel-empty__title、panel-empty__desc

## B6 单页专有壳类（1-2 页） —— 26 块 / 20 文件 / 约 105 行

- `views/business/erp/account/index.vue` — tool-row__category
- `views/business/erp/budget/mapping.vue` — tool-row__dept
- `views/business/erp/budget/report.vue` — tool-row__year、tool-row__dept、panel-head__actions
- `views/business/erp/cost-object/index.vue` — tool-row__month
- `views/business/erp/cost-overhead/index.vue` — tool-row__item
- `views/business/erp/cost-period/steps/UncollectedPanel.vue` — panel-head__ops
- `views/business/erp/expense/pay/index.vue` — tool-row__tip
- `views/business/erp/inventory/index.vue` — tool-row__switch、panel-head__tip
- `views/business/erp/invoice/index.vue` — tool-row--second
- `views/business/erp/ledger/index.vue` — panel-head__account
- `views/business/erp/location/index.vue` — panel-head__link、panel-head__link
- `views/business/erp/receipt-refund/index.vue` — tool-row--second
- `views/business/erp/receipt/index.vue` — tool-row--second
- `views/business/erp/receivable/index.vue` — tool-row--second
- `views/business/erp/report/report-item/index.vue` — tool-row--filter
- `views/business/erp/serial/index.vue` — panel-head__warn
- `views/business/erp/transfer/index.vue` — tool-row__overdue、panel-head__overdue、panel-head__overdue
- `views/business/erp/warehouse/index.vue` — panel-head__sub
- `views/business/erp/year-end/index.vue` — tool-row__year
- `views/business/help/center/index.vue` — list-card__pager
