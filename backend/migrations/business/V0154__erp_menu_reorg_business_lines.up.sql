-- V0154 fix：补 auth_code（sys_menu.auth_code 是 NOT NULL）
-- 现有目录的 auth_code 风格：system / support / notice-center / content（短横线小写）
-- 所以：erp / erp-purchase / erp-warehouse / erp-finance / erp-master-data

BEGIN;

-- ═══════ ① 修脏数据（幂等，重跑安全）═══════
UPDATE sys_menu SET name = '收货入库', updated_at = now()
WHERE name = 'business:erp:stock-in:list';

UPDATE sys_menu SET component = 'business/erp/purchase/index', updated_at = now()
WHERE path = '/business/erp/purchase'
  AND component = 'business/erp/purchase-order/index';

UPDATE sys_menu SET component = 'business/erp/transfer/index', updated_at = now()
WHERE path = '/business/erp/transfer'
  AND (component IS NULL OR component = '');

-- ═══════ ② ERP 顶级目录（原「ERP 主数据」）═══════
UPDATE sys_menu
SET name = 'ERP', auth_code = 'erp', path = '/business/erp', component = NULL,
    icon = 'app', sort_order = 60, updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000940';

-- ═══════ ③ 采购管理（原「ERP 台账」）═══════
UPDATE sys_menu
SET name = '采购管理', auth_code = 'erp-purchase', path = '/business/erp/purchase-mgmt',
    component = NULL, icon = 'shopping-cart', sort_order = 10,
    parent_id = '20000000-0000-0000-0000-000000000940', updated_at = now()
WHERE id = '20000000-0000-0000-0000-000000000946';

-- ═══════ ④ 新建 3 个业务目录（带 auth_code）═══════
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, component, parent_id, sort_order, icon, is_visible)
VALUES
  ('20000000-0000-0000-0000-000000000e01', '仓库管理', 1, 'erp-warehouse',
   '/business/erp/warehouse-mgmt', NULL, '20000000-0000-0000-0000-000000000940', 20, 'server', true),
  ('20000000-0000-0000-0000-000000000e02', '财务管理', 1, 'erp-finance',
   '/business/erp/finance-mgmt', NULL, '20000000-0000-0000-0000-000000000940', 30, 'money', true),
  ('20000000-0000-0000-0000-000000000e03', '基础数据', 1, 'erp-master-data',
   '/business/erp/master-data', NULL, '20000000-0000-0000-0000-000000000940', 40, 'folder', true)
ON CONFLICT (id) DO UPDATE SET
  name = EXCLUDED.name, auth_code = EXCLUDED.auth_code,
  parent_id = EXCLUDED.parent_id, sort_order = EXCLUDED.sort_order,
  icon = EXCLUDED.icon, is_visible = EXCLUDED.is_visible, updated_at = now();

-- ═══════ ⑤ 挂载子菜单 ═══════
-- 采购管理（...946）
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000946', sort_order = 10, updated_at = now() WHERE path = '/business/erp/purchase';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000946', sort_order = 20, updated_at = now() WHERE path = '/business/erp/stock-in';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000946', sort_order = 30, updated_at = now() WHERE path = '/business/erp/payable';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000946', sort_order = 40, updated_at = now() WHERE path = '/business/erp/supplier';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000946', sort_order = 50, updated_at = now() WHERE path = '/business/erp/purchase-order-item';

-- 仓库管理（...e01）
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e01', sort_order = 10, updated_at = now() WHERE path = '/business/erp/warehouse';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e01', sort_order = 20, updated_at = now() WHERE path = '/business/erp/location';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e01', sort_order = 30, updated_at = now() WHERE path = '/business/erp/serial';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e01', sort_order = 40, updated_at = now() WHERE path = '/business/erp/inventory';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e01', sort_order = 50, updated_at = now() WHERE path = '/business/erp/transfer';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e01', sort_order = 60, updated_at = now() WHERE path = '/business/erp/inventory-location';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e01', sort_order = 70, updated_at = now() WHERE path = '/business/erp/inventory-txn';

-- 财务管理（...e02）
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e02', sort_order = 10, updated_at = now() WHERE path = '/business/erp/account';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e02', sort_order = 20, updated_at = now() WHERE path = '/business/erp/period';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e02', sort_order = 30, updated_at = now() WHERE path = '/business/erp/voucher';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e02', sort_order = 40, updated_at = now() WHERE path = '/business/erp/ledger';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e02', sort_order = 50, updated_at = now() WHERE path = '/business/erp/seq';

-- 基础数据（...e03）
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e03', sort_order = 10, updated_at = now() WHERE path = '/business/erp/material';
UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000e03', sort_order = 20, updated_at = now() WHERE path = '/business/erp/unit';

COMMIT;

-- ═══════ 验证 ═══════
\echo ''
\echo '═══ 重组结果 ═══'
SELECT
  substring(m.id::text, 25, 8) AS 号,
  m.name AS 名称,
  m.menu_type AS 型,
  m.path AS 路径,
  m.auth_code AS 鉴权码,
  m.is_visible AS 可见,
  coalesce(p.name, '(顶级)') AS 上级
FROM sys_menu m
LEFT JOIN sys_menu p ON p.id = m.parent_id
WHERE m.deleted_at IS NULL
  AND m.menu_type IN (1, 2)
  AND (
    m.name = 'ERP'
    OR m.parent_id = '20000000-0000-0000-0000-000000000940'
    OR p.parent_id = '20000000-0000-0000-0000-000000000940'
  )
ORDER BY
  CASE WHEN m.name = 'ERP' THEN 0
       WHEN m.parent_id = '20000000-0000-0000-0000-000000000940' THEN 1
       ELSE 2 END,
  COALESCE(p.sort_order, m.sort_order), m.sort_order;
