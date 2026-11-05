-- 探针用计划：3 个导入导出页 × 3 个业务角色，mode = READ（卡面 A 方案的字面量）
-- （新建临时会话，无需先删表；形状与 plan.sql 的 _plan 完全一致）
CREATE TEMP TABLE _plan (role_code text NOT NULL, path text NOT NULL, mtype smallint NOT NULL, mode text NOT NULL);
INSERT INTO _plan (role_code, path, mtype, mode) VALUES
 ('PURCHASE_MANAGER',  '/business/impexp/import',      2, 'READ'),
 ('PURCHASE_MANAGER',  '/business/impexp/import-task', 2, 'READ'),
 ('PURCHASE_MANAGER',  '/business/impexp/export-task', 2, 'READ'),
 ('WAREHOUSE_MANAGER', '/business/impexp/import',      2, 'READ'),
 ('WAREHOUSE_MANAGER', '/business/impexp/import-task', 2, 'READ'),
 ('WAREHOUSE_MANAGER', '/business/impexp/export-task', 2, 'READ'),
 ('FINANCE_MANAGER',   '/business/impexp/import',      2, 'READ'),
 ('FINANCE_MANAGER',   '/business/impexp/import-task', 2, 'READ'),
 ('FINANCE_MANAGER',   '/business/impexp/export-task', 2, 'READ');
