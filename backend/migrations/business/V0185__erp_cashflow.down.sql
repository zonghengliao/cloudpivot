-- ======================================================
-- V0185 回滚：现金流量表（ERP-R3）
--
-- 只删本迁移种下的东西：
--   ① 权限节点 …1018 及其授权行
--   ② 12 条对方科目分类行
--   ③ `biz_cashflow_item` 表本身
-- 科目 / 凭证 / 资产负债表与利润表配置 / 其它菜单一行不动。
-- ======================================================

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001018'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001018'::uuid;

DELETE FROM biz_cashflow_item
 WHERE item_code IN (
    'OPER_SALES_RECEIPTS', 'OPER_OTHER_INFLOW',
    'OPER_PURCHASE_PAYMENTS', 'OPER_STAFF_PAYMENTS', 'OPER_TAX_PAYMENTS', 'OPER_OTHER_OUTFLOW',
    'INVEST_RECOVERY', 'INVEST_PURCHASES',
    'FIN_CAPITAL_IN', 'FIN_LOANS_IN', 'FIN_DEBT_REPAY', 'FIN_DISTRIBUTION'
 );

DROP TABLE IF EXISTS biz_cashflow_item;
