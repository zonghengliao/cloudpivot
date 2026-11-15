-- V0255 应付账款账龄分析（PRD E4-03 P0 / P1-FIX-8A）
--
-- 只动「菜单 + 权限」两件事，**不建表**：
--   账龄直接从 biz_payable 现算（due_date / unpaid_amount / status 都是 V0148 既有列），
--   不落快照表、不加列 —— 加一张「账龄快照表」就等于要求所有人记住「跑报表前先刷新快照」。
--
-- 权限码：business:erp:report:payable-aging（页面）
--   导出复用 V0183 已种的 business:erp:report:export（同一个「报表导出」动作码，
--   不新增按钮菜单行：新增一行同名 auth_code 会让「按码取菜单」出现两条，按钮也会在树里重复）。
--
-- 菜单 id：1600（页面）。父目录 0e02 =「财务管理」（V0154 建）。
--   已用号段：1000–1034、1315、1400–1533 → 1600 空闲（全仓 grep 已核对）。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001600'::uuid, 'business:erp:report:payable-aging', 2,
     'business:erp:report:payable-aging', '/business/erp/report/payable-aging',
     '20000000-0000-0000-0000-000000000e02'::uuid, 'business/erp/report/payable-aging/index',
     47, 1, NULL, '应付账龄分析', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权段与 V0183 同款：迁移先于 bootstrap 跑（可能还没有角色）→ 空操作 + NOTICE。
DO $$
DECLARE
    n_roles  integer;
    n_grants integer;
    n_menus  integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001600'::uuid;
    IF n_menus <> 1 THEN
        RAISE EXCEPTION 'V0255 自检失败：应付账龄分析菜单应有 1 行，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0255：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001600'::uuid
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN',
                               'FINANCE', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                               'ACCOUNTANT', 'GENERAL_MANAGER')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm
                 WHERE rm.role_id = r.id
                   AND rm.menu_id = '20000000-0000-0000-0000-000000001600'::uuid
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0255：应付账龄分析 1 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
    END IF;

    RAISE NOTICE 'V0255 自检通过：应付账龄分析菜单 1 个';
END $$;
