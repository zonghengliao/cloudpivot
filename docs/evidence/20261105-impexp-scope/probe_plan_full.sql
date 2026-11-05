-- 对照组：同样 3 页，mode = FULL（"能看能办"）
CREATE TEMP TABLE _plan (role_code text NOT NULL, path text NOT NULL, mtype smallint NOT NULL, mode text NOT NULL);
INSERT INTO _plan (role_code, path, mtype, mode) VALUES
 ('PURCHASE_MANAGER',  '/business/impexp/import',      2, 'FULL'),
 ('PURCHASE_MANAGER',  '/business/impexp/import-task', 2, 'FULL'),
 ('PURCHASE_MANAGER',  '/business/impexp/export-task', 2, 'FULL'),
 ('WAREHOUSE_MANAGER', '/business/impexp/import',      2, 'FULL'),
 ('WAREHOUSE_MANAGER', '/business/impexp/import-task', 2, 'FULL'),
 ('WAREHOUSE_MANAGER', '/business/impexp/export-task', 2, 'FULL'),
 ('FINANCE_MANAGER',   '/business/impexp/import',      2, 'FULL'),
 ('FINANCE_MANAGER',   '/business/impexp/import-task', 2, 'FULL'),
 ('FINANCE_MANAGER',   '/business/impexp/export-task', 2, 'FULL');
