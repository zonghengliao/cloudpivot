-- ============================================================================
-- V0252__menu_expense_icon_bill.down.sql
--
-- 回滚：把「我的报销」的图标改回 V0242 种子原值 `invoice`。
--
-- 注意：`invoice` 在 tdesign-icons-vue-next 里**不存在**，回滚会重新引入
-- 「侧栏图标空白 + n09 用例红」的缺陷 —— 这是「回到迁移前状态」的应有语义，
-- 只在需要严格回退到 V0251 时使用。
-- ============================================================================

DO $$
DECLARE
    c_id    CONSTANT uuid := '20000000-0000-0000-0000-000000001501';
    n_hit   int;
BEGIN
    UPDATE sys_menu
       SET icon = 'invoice', updated_at = now()
     WHERE id = c_id AND icon = 'bill';
    GET DIAGNOSTICS n_hit = ROW_COUNT;

    IF n_hit = 0 THEN
        RAISE NOTICE 'V0252 down：菜单 % 当前 icon 不是 bill（已回滚过 / 客户改过），跳过', c_id;
    ELSE
        RAISE NOTICE 'V0252 down：「我的报销」图标已改回 invoice（% 行）', n_hit;
    END IF;
END $$;
