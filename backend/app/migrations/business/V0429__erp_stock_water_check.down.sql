-- ============================================================================
-- V0429__erp_stock_water_check.down（M2-14 · 安全库存水位检查）
--
-- 只回滚本迁移**种下的四样东西**：
--   ① 部分唯一索引 `uk_biz_task_open_inventory`（`biz_task` 上，本迁移创建）；
--   ② 授权行（sys_role_menu，按固定 menu id 反查）；
--   ③ 权限点本身（sys_menu，固定 UUID 20000000-0000-0000-0000-000000004290）；
--   ④ `sys_job` 的 `erp_stock_water_check` 行 + `sys_config` 的 `erp.stock_water.realert_days` 行。
--
-- **不动**：
--   · V0428 的 `biz_material_stock_policy` 表与三个水位码（那是 M2-13 的能力）；
--   · `biz_task` 里的任何行（含本卡生成的水位待办 —— 回滚只撤「约束与入口」，
--     历史待办是事实，删了人就不知道曾经缺过料；它们之后由人工按普通任务处理）；
--   · `biz_inventory` / `biz_material.safety_stock` / 「即时库存」页面行与其它兄弟按钮。
--
-- 回滚后：`POST /business/erp/stock-policy/check` 因权限码行不存在而 403（路由代码仍在），
-- 定时任务由内置注册继续跑（`sys_job` 行没了 → 不落 `sys_job_log`，但检查照跑）。
-- 这是「迁移必须先于能力」的正常表现，不是需要修的数据。
-- ============================================================================

DROP INDEX IF EXISTS uk_biz_task_open_inventory;

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004290'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004290'::uuid;

DELETE FROM sys_job
 WHERE job_name = 'erp_stock_water_check'
   AND invoke_target = 'erp_stock_water_check';

DELETE FROM sys_config
 WHERE config_key = 'erp.stock_water.realert_days'
   AND id = '2f121000-0000-4000-8000-000000004290'::uuid;

DO $$
DECLARE
    n_idx   integer;
    n_menu  integer;
    n_grant integer;
    n_job   integer;
    n_cfg   integer;
BEGIN
    SELECT count(*) INTO n_idx
      FROM pg_indexes
     WHERE schemaname = current_schema() AND indexname = 'uk_biz_task_open_inventory';
    SELECT count(*) INTO n_menu
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000004290'::uuid;
    SELECT count(*) INTO n_grant
      FROM sys_role_menu WHERE menu_id = '20000000-0000-0000-0000-000000004290'::uuid;
    SELECT count(*) INTO n_job
      FROM sys_job WHERE job_name = 'erp_stock_water_check';
    SELECT count(*) INTO n_cfg
      FROM sys_config WHERE config_key = 'erp.stock_water.realert_days';

    IF n_idx <> 0 OR n_menu <> 0 OR n_grant <> 0 OR n_job <> 0 OR n_cfg <> 0 THEN
        RAISE EXCEPTION 'V0429.down 自检失败：未删净（索引 % / 菜单 % / 授权 % / 任务行 % / 参数行 %）',
            n_idx, n_menu, n_grant, n_job, n_cfg;
    END IF;

    -- 上游的东西必须还在（down 不许误伤）
    IF to_regclass('public.biz_task') IS NULL THEN
        RAISE EXCEPTION 'V0429.down 自检失败：biz_task 被误删（任务表与本卡无关）';
    END IF;
    IF to_regclass('public.biz_material_stock_policy') IS NULL THEN
        RAISE EXCEPTION 'V0429.down 自检失败：biz_material_stock_policy 被误删（V0428 的表，本卡只读）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_menu
                    WHERE auth_code = 'business:erp:stock-policy:list'
                      AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0429.down 自检失败：V0428 的水位设置码被误删（本卡只删 check 一个码）';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_menu
                    WHERE auth_code = 'business:erp:inventory:list'
                      AND menu_type = 2
                      AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0429.down 自检失败：「即时库存」页面行被误删';
    END IF;

    RAISE NOTICE 'V0429.down 完成：索引/菜单/授权/任务行/参数行已清，V0428 的表与三个码仍在';
END $$;
