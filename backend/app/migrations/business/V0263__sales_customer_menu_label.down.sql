-- ============================================================================
-- V0263 down：把客户档案菜单的显示名写回 V0262 的长 title
--
-- 只还原 `title` 一列；`path` / `component` / `is_visible` 一概不动
-- （那是 V0262 的职责，回滚 V0263 不该把页面关掉，否则路由还在、菜单却消失）。
-- ============================================================================

DO $$
DECLARE
    v_page  uuid := '20000000-0000-0000-0000-000000002601'::uuid;
    v_long  text := '客户档案（销售链交易对手主档：编码 / 名称 / 联系人 / 信用额度 / 状态；冻结只影响开新单）';
    n_upd   int := 0;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = v_page) THEN
        RAISE EXCEPTION 'V0263 down 失败：客户档案页面节点 …2601 不存在';
    END IF;

    UPDATE sys_menu
       SET title = v_long, updated_at = now()
     WHERE id = v_page
       AND title IS DISTINCT FROM v_long;
    GET DIAGNOSTICS n_upd = ROW_COUNT;

    RAISE NOTICE 'V0263 down 完成：客户档案 title 还原成长文案（改 % 行）', n_upd;
END $$;
