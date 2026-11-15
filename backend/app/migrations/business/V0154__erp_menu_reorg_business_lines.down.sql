-- 回滚：恢复「ERP 主数据 / ERP 台账」两级结构
BEGIN;

-- 子菜单回挂 ERP 台账（原结构）
UPDATE sys_menu SET parent_id = 20000000-0000-0000-0000-000000000946, updated_at = now()
WHERE path IN (
  /business/erp/purchase,/business/erp/stock-in,/business/erp/payable,
  /business/erp/supplier,/business/erp/purchase-order-item,
  /business/erp/account,/business/erp/period,/business/erp/voucher,
  /business/erp/ledger,/business/erp/seq,
  /business/erp/inventory,/business/erp/inventory-location,/business/erp/inventory-txn,
  /business/erp/transfer
);
UPDATE sys_menu SET parent_id = 20000000-0000-0000-0000-000000000940, updated_at = now()
WHERE path IN (/business/erp/warehouse,/business/erp/location,/business/erp/material,
               /business/erp/serial,/business/erp/unit);

-- 目录改名回去
UPDATE sys_menu SET name=ERP 主数据, auth_code=erp-master, path=/business/erp,
       component=NULL, sort_order=60, updated_at=now()
WHERE id=20000000-0000-0000-0000-000000000940;
UPDATE sys_menu SET name=ERP 台账, auth_code=erp-ledger, path=/business/erp/ledger,
       component=NULL, sort_order=70, updated_at=now()
WHERE id=20000000-0000-0000-0000-000000000946;

-- 删掉三个新目录（其子已回挂，不会有孤儿）
DELETE FROM sys_role_menu WHERE menu_id IN (
  20000000-0000-0000-0000-000000000e01,
  20000000-0000-0000-0000-000000000e02,
  20000000-0000-0000-0000-000000000e03
);
DELETE FROM sys_menu WHERE id IN (
  20000000-0000-0000-0000-000000000e01,
  20000000-0000-0000-0000-000000000e02,
  20000000-0000-0000-0000-000000000e03
);

COMMIT;
