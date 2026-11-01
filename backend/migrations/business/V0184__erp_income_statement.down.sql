-- ======================================================
-- V0184 回滚：利润表（ERP-R2）
--
-- 只删本迁移种下的东西：
--   · `biz_report_item` 里 report_code = 'INCOME_STATEMENT' 的全部行
--   · 权限节点 `…1017`（利润表页面）及其授权
--
-- 不动：资产负债表配置（V0183）、科目表、凭证 / 分录 / 期间（利润表是只读报表）。
-- ======================================================

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001017'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001017'::uuid;

DELETE FROM biz_report_item
 WHERE report_code = 'INCOME_STATEMENT';

DO $$
DECLARE
    n_item integer;
    n_menu integer;
BEGIN
    SELECT count(*) INTO n_item FROM biz_report_item WHERE report_code = 'INCOME_STATEMENT';
    SELECT count(*) INTO n_menu FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000001017'::uuid;
    IF n_item <> 0 OR n_menu <> 0 THEN
        RAISE EXCEPTION 'V0184 回滚失败：报表项残留 % 行、权限节点残留 % 行', n_item, n_menu;
    END IF;
    RAISE NOTICE 'V0184 回滚完成：利润表报表项与权限节点已清除（资产负债表配置未动）';
END $$;
