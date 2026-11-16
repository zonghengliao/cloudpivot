-- ============================================================================
-- V0428__erp_stock_policy_water_level.down（M2-13 · 安全库存字段（物料 × 仓库））
--
-- 只回滚本迁移**种下的三样东西**：
--   ① 授权行（sys_role_menu，按三个固定 menu id 反查）；
--   ② 权限点本身（sys_menu，三个固定 UUID）；
--   ③ 表 `biz_material_stock_policy`（含数据 —— 它是本迁移建的，回滚即撤销「按仓库设水位」这个能力）。
--
-- **不动**：
--   · `biz_material.safety_stock`（V0137 的物料级安全库存，V0210 的低库存扫描正在用它）；
--   · `biz_inventory` / `biz_inventory_txn`（结存与流水，本迁移一行没写）；
--   · 「即时库存」页面行与其它兄弟按钮（V0421~V0427 的菜单）。
--
-- 回滚后：水位设置接口 404（路由代码仍在，但表没了 —— 部署新代码配旧库时 `cargo run`
-- 会因为启动自检/第一次访问报错，这是预期的「迁移必须先于代码」纪律）。
-- ============================================================================

DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                   '20000000-0000-0000-0000-000000004281'::uuid,
                   '20000000-0000-0000-0000-000000004282'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000004280'::uuid,
              '20000000-0000-0000-0000-000000004281'::uuid,
              '20000000-0000-0000-0000-000000004282'::uuid);

DROP TABLE IF EXISTS biz_material_stock_policy;

DO $$
DECLARE
    n_menu  integer;
    n_grant integer;
    n_tab   integer;
BEGIN
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                  '20000000-0000-0000-0000-000000004281'::uuid,
                  '20000000-0000-0000-0000-000000004282'::uuid);
    SELECT count(*) INTO n_grant
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000004280'::uuid,
                       '20000000-0000-0000-0000-000000004281'::uuid,
                       '20000000-0000-0000-0000-000000004282'::uuid);
    SELECT count(*) INTO n_tab
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public' AND c.relname = 'biz_material_stock_policy';

    IF n_menu <> 0 OR n_grant <> 0 OR n_tab <> 0 THEN
        RAISE EXCEPTION 'V0428.down 自检失败：未删净（菜单 % 行 / 授权 % 行 / 表 % 张）', n_menu, n_grant, n_tab;
    END IF;

    -- 上下游的东西必须还在（down 不许误伤）
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = current_schema()
                      AND table_name = 'biz_material'
                      AND column_name = 'safety_stock') THEN
        RAISE EXCEPTION 'V0428.down 自检失败：biz_material.safety_stock 被误删（V0137 的物料级安全库存，本卡不许动）';
    END IF;
    IF to_regclass('public.biz_inventory') IS NULL THEN
        RAISE EXCEPTION 'V0428.down 自检失败：biz_inventory 被误删（结存表与本卡无关）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_menu
                    WHERE auth_code = 'business:erp:inventory:list'
                      AND menu_type = 2
                      AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0428.down 自检失败：「即时库存」页面行被误删（本卡只删自己种的三个按钮）';
    END IF;

    RAISE NOTICE 'V0428.down 完成：菜单/授权/表已清，物料级安全库存与结存表仍在';
END $$;
