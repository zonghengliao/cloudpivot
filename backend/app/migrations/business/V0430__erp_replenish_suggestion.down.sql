-- ============================================================================
-- V0430__erp_replenish_suggestion.down（M2-15 · 补货建议）
--
-- 只回滚本迁移**种下的两样东西**：
--   ① 授权行（sys_role_menu，按固定 menu id 反查）；
--   ② 权限点本身（sys_menu，固定 UUID 20000000-0000-0000-0000-000000004300）。
--
-- **不动**：
--   · V0420 的 `biz_inventory_in_transit`（在途账，M2-01~M2-05 的能力）；
--   · V0428 的 `biz_material_stock_policy` 与四个水位码（M2-13 / M2-14 的能力）；
--   · `biz_inventory` / `biz_purchase_order(_item)` / 任何业务数据 —— 本卡一行都没写；
--   · 「即时库存」页面行与它的其它兄弟按钮（V0421~V0429 种的）。
--
-- 回滚后：`GET /business/erp/replenish/page` 因权限码行不存在而对非超管 403（路由代码仍在），
-- 超管靠 `role_code` 短路仍可访问。这是「迁移必须先于能力」的正常表现，不是需要修的数据。
-- ============================================================================

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004300'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004300'::uuid;

DO $$
DECLARE
    n_menu  integer;
    n_grant integer;
BEGIN
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004300'::uuid;
    SELECT count(*) INTO n_grant
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000004300'::uuid;
    IF n_menu <> 0 OR n_grant <> 0 THEN
        RAISE EXCEPTION 'V0430.down 自检失败：补货建议按钮 / 授权未删净（菜单 % 行、授权 % 行）', n_menu, n_grant;
    END IF;

    -- 上游的东西必须还在（down 不许误伤）
    IF to_regclass('public.biz_inventory_in_transit') IS NULL THEN
        RAISE EXCEPTION 'V0430.down 自检失败：在途账 biz_inventory_in_transit 被误删（V0420 的表，本卡只读）';
    END IF;
    IF to_regclass('public.biz_material_stock_policy') IS NULL THEN
        RAISE EXCEPTION 'V0430.down 自检失败：水位设置表 biz_material_stock_policy 被误删（V0428 的表，本卡只读）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_menu
                    WHERE auth_code = 'business:erp:stock-policy:list'
                      AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0430.down 自检失败：V0428 的水位设置码被误删（本卡只删 replenish 一个码）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_menu
                    WHERE auth_code = 'business:erp:inventory:list'
                      AND menu_type = 2
                      AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0430.down 自检失败：「即时库存」页面行被误删';
    END IF;

    RAISE NOTICE 'V0430.down 完成：菜单 / 授权已清；在途账与水位设置（V0420 / V0428）仍在';
END $$;
