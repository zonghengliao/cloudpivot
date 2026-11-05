-- ============================================================================
-- 授权计划表（唯一的「授予矩阵」真相源）
-- ----------------------------------------------------------------------------
-- 本文件只建 **临时表** `_plan`，不写库。被 20_grant.up.sql / 20_grant.down.sql
-- `\i` 引入，保证 up 与 down 用的是**同一份**计划（否则 down 删不干净/删多）。
--
-- mode 语义：
--   FULL  节点 + 全部后代（页面 + 全部按钮码）—— 职能域「能看能办」
--   READ  节点 + 只读后代（白名单动作：list/read/detail/export/download/mine/
--         aux-balance/trial-balance/health/self-check/cancel-impact/template）
--   NODE  只授该节点本身（页面的 list 码，不含任何按钮）
--
-- 所有条目都按 (path, menu_type) 命中：生产上 menu_type=2 的 path 唯一（已断言），
-- menu_type=1 的目录 path 也唯一，因此不依赖 uuid（换库/重建也能对齐）。
-- ============================================================================
DROP TABLE IF EXISTS _plan;
CREATE TEMP TABLE _plan (role_code text NOT NULL, path text NOT NULL, mtype smallint NOT NULL, mode text NOT NULL);

INSERT INTO _plan (role_code, path, mtype, mode) VALUES
-- ===== R3 人人自用（5 个业务角色都授）=====================================
('DEPT_LEADER',       '/dashboard',                        2, 'NODE'),
('DEPT_LEADER',       '/help',                             2, 'READ'),
('DEPT_LEADER',       '/announcement',                     2, 'NODE'),
('DEPT_LEADER',       '/business/notice/message',          2, 'NODE'),
('DEPT_LEADER',       '/business/storage/file',            2, 'READ'),
('DEPT_LEADER',       '/user/preference',                  2, 'NODE'),

('FINANCE_MANAGER',   '/dashboard',                        2, 'NODE'),
('FINANCE_MANAGER',   '/help',                             2, 'READ'),
('FINANCE_MANAGER',   '/announcement',                     2, 'NODE'),
('FINANCE_MANAGER',   '/business/notice/message',          2, 'NODE'),
('FINANCE_MANAGER',   '/business/storage/file',            2, 'READ'),
('FINANCE_MANAGER',   '/user/preference',                  2, 'NODE'),

('PURCHASE_MANAGER',  '/dashboard',                        2, 'NODE'),
('PURCHASE_MANAGER',  '/help',                             2, 'READ'),
('PURCHASE_MANAGER',  '/announcement',                     2, 'NODE'),
('PURCHASE_MANAGER',  '/business/notice/message',          2, 'NODE'),
('PURCHASE_MANAGER',  '/business/storage/file',            2, 'READ'),
('PURCHASE_MANAGER',  '/user/preference',                  2, 'NODE'),

('WAREHOUSE_MANAGER', '/dashboard',                        2, 'NODE'),
('WAREHOUSE_MANAGER', '/help',                             2, 'READ'),
('WAREHOUSE_MANAGER', '/announcement',                     2, 'NODE'),
('WAREHOUSE_MANAGER', '/business/notice/message',          2, 'NODE'),
('WAREHOUSE_MANAGER', '/business/storage/file',            2, 'READ'),
('WAREHOUSE_MANAGER', '/user/preference',                  2, 'NODE'),

('GENERAL_MANAGER',   '/dashboard',                        2, 'NODE'),
('GENERAL_MANAGER',   '/help',                             2, 'READ'),
('GENERAL_MANAGER',   '/announcement',                     2, 'NODE'),
('GENERAL_MANAGER',   '/business/notice/message',          2, 'NODE'),
('GENERAL_MANAGER',   '/business/storage/file',            2, 'READ'),
('GENERAL_MANAGER',   '/user/preference',                  2, 'NODE'),

-- ===== R1 任务 / 项目 / 审批（DEPT_LEADER 全权，总经理只读）==============
-- /task 是 menu_type=1 的目录，其 auth_code 也是 business:task:read，
-- 4 个任务页与 13 个任务按钮都是它的子节点 ⇒ FULL 一次覆盖。
('DEPT_LEADER',       '/task',                             1, 'FULL'),
('DEPT_LEADER',       '/project',                          2, 'FULL'),
('DEPT_LEADER',       '/flow/def',                         2, 'READ'),
('DEPT_LEADER',       '/business/notice/template',         2, 'READ'),
('DEPT_LEADER',       '/business/notice/send',             2, 'NODE'),
('DEPT_LEADER',       '/business/announcement',            2, 'NODE'),

('GENERAL_MANAGER',   '/task',                             1, 'READ'),
('GENERAL_MANAGER',   '/project',                          2, 'READ'),
('GENERAL_MANAGER',   '/flow/def',                         2, 'READ'),
('GENERAL_MANAGER',   '/business/announcement',            2, 'FULL'),
('GENERAL_MANAGER',   '/business/notice/template',         2, 'FULL'),
('GENERAL_MANAGER',   '/business/notice/send',             2, 'NODE'),

-- ===== R1 采购域（PURCHASE_MANAGER 全权）================================
('PURCHASE_MANAGER',  '/business/erp/purchase',            2, 'FULL'),
('PURCHASE_MANAGER',  '/business/erp/purchase-order-item', 2, 'FULL'),
('PURCHASE_MANAGER',  '/business/erp/supplier',            2, 'FULL'),
('PURCHASE_MANAGER',  '/business/erp/contract',            2, 'FULL'),
('PURCHASE_MANAGER',  '/business/erp/material',            2, 'FULL'),
('PURCHASE_MANAGER',  '/business/erp/unit',                2, 'READ'),
('PURCHASE_MANAGER',  '/flow/def',                         2, 'READ'),
('PURCHASE_MANAGER',  '/business/notice/template',         2, 'READ'),
-- R2 邻域只读：仓储 / 应付
('PURCHASE_MANAGER',  '/business/erp/warehouse',           2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/location',            2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/inventory',           2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/inventory-location',  2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/inventory-txn',       2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/serial',              2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/transfer',            2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/stock-in',            2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/stock-out',           2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/stock-take',          2, 'READ'),
('PURCHASE_MANAGER',  '/business/erp/payable',             2, 'READ'),

-- ===== R1 仓储域（WAREHOUSE_MANAGER 全权）===============================
('WAREHOUSE_MANAGER', '/business/erp/warehouse',           2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/location',            2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/inventory',           2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/inventory-location',  2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/inventory-txn',       2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/serial',              2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/transfer',            2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/stock-in',            2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/stock-out',           2, 'FULL'),
('WAREHOUSE_MANAGER', '/business/erp/stock-take',          2, 'FULL'),
('WAREHOUSE_MANAGER', '/flow/def',                         2, 'READ'),
('WAREHOUSE_MANAGER', '/business/notice/template',         2, 'READ'),
-- R2 邻域只读：基础数据 / 采购
('WAREHOUSE_MANAGER', '/business/erp/material',            2, 'READ'),
('WAREHOUSE_MANAGER', '/business/erp/unit',                2, 'READ'),
('WAREHOUSE_MANAGER', '/business/erp/purchase',            2, 'READ'),
('WAREHOUSE_MANAGER', '/business/erp/purchase-order-item', 2, 'READ'),
('WAREHOUSE_MANAGER', '/business/erp/supplier',            2, 'READ'),
-- 注：**不**授 /business/erp/contract（合同含商务条款，仓管按采购单收货，不需要看合同）

-- ===== R1 财务域（FINANCE_MANAGER 全权）=================================
('FINANCE_MANAGER',   '/business/erp/account',             2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/period',              2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/opening',             2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/voucher',             2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/ledger',              2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/report/balance-sheet',2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/report/income-statement', 2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/report/cash-flow',    2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/year-end',            2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/payable',             2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/seq',                 2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/budget',              2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/budget/report',       2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/budget/mapping',      2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/expense',             2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/expense/pay',         2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/expense-type',        2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/asset',               2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/asset-category',      2, 'FULL'),
('FINANCE_MANAGER',   '/business/erp/depreciation',        2, 'FULL'),
('FINANCE_MANAGER',   '/flow/def',                         2, 'READ'),
('FINANCE_MANAGER',   '/business/notice/template',         2, 'READ'),
-- R2 邻域只读：采购 / 仓储 / 物料
('FINANCE_MANAGER',   '/business/erp/purchase',            2, 'READ'),
('FINANCE_MANAGER',   '/business/erp/purchase-order-item', 2, 'READ'),
('FINANCE_MANAGER',   '/business/erp/supplier',            2, 'READ'),
('FINANCE_MANAGER',   '/business/erp/contract',            2, 'READ'),
('FINANCE_MANAGER',   '/business/erp/stock-in',            2, 'READ'),
('FINANCE_MANAGER',   '/business/erp/stock-out',           2, 'READ'),
('FINANCE_MANAGER',   '/business/erp/inventory',           2, 'READ'),
('FINANCE_MANAGER',   '/business/erp/material',            2, 'READ'),

-- ===== R2 总经理全域只读（GENERAL_MANAGER，data_scope=1）================
('GENERAL_MANAGER',   '/business/erp/purchase',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/purchase-order-item', 2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/supplier',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/contract',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/material',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/unit',                2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/warehouse',           2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/location',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/inventory',           2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/inventory-location',  2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/inventory-txn',       2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/serial',              2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/transfer',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/stock-in',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/stock-out',           2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/stock-take',          2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/account',             2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/period',              2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/opening',             2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/voucher',             2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/ledger',              2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/report/balance-sheet',2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/report/income-statement', 2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/report/cash-flow',    2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/year-end',            2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/payable',             2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/seq',                 2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/budget',              2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/budget/report',       2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/budget/mapping',      2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/expense',             2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/expense/pay',         2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/expense-type',        2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/asset',               2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/asset-category',      2, 'READ'),
('GENERAL_MANAGER',   '/business/erp/depreciation',        2, 'READ');

-- 计划自检：同一 (role,path,mtype) 不许重复登记两种 mode
DO $$
DECLARE r record;
BEGIN
    FOR r IN SELECT role_code, path, mtype, count(*) c, count(DISTINCT mode) m
               FROM _plan GROUP BY 1,2,3 HAVING count(*) > 1 LOOP
        RAISE EXCEPTION '计划自检失败：% 在 %(type=%) 重复登记 % 次（% 种 mode）',
              r.role_code, r.path, r.mtype, r.c, r.m;
    END LOOP;
END $$;
