-- ============================================================
-- V0214：辅助账接口的权限码（ARCH-3 §5 DC-11 · P2-1）
--
-- # 补的是什么
--
-- 新增一个端点：
--   GET /business/erp/ledger/aux-balance   辅助账（按物料 / 仓库 / 部门 / 供应商查发生额与余额）
--
-- `ledger.rs` 的 `#[nik::permission]` 声明的权限码是
-- `business:erp:ledger:aux-balance`，必须与 `sys_menu.auth_code` **逐字一致**，
-- 否则该接口只有持通配权限（`*:*:*`）的超管能调，会计 / 财务全 403。
--
-- # 为什么挂在「总账 / 科目余额表」菜单下
--
-- 辅助账与科目余额表是同一张报表的两种看法（后者按科目，前者按辅助项），
-- 授权粒度也应该同一层：能看余额表的人本来就能看到同样金额的合计。
-- 所以父菜单按 `auth_code = 'business:erp:ledger:list'` 找（**不按 path 找**：
-- V0213 的教训 —— 历史库里同一菜单的 path 被改过，按 path 匹配会漏）。
--
-- # 幂等
--
-- 按「auth_code + parent_id」判在不在，重复执行不产生重复行；
-- 授权按 `sys_role_menu` 的（role_id, menu_id）唯一性去重。
-- ============================================================

-- ---------- ① 按钮节点 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT gen_random_uuid(), 'business:erp:ledger:aux-balance', 3,
       'business:erp:ledger:aux-balance', NULL, m.id, NULL,
       2, 1, NULL, '辅助账（按物料 / 仓库 / 部门 / 供应商查发生额与余额）', FALSE, 'business'
  FROM (SELECT id FROM sys_menu
         WHERE auth_code = 'business:erp:ledger:list'
           AND deleted_at IS NULL) AS m
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_menu x
         WHERE x.auth_code = 'business:erp:ledger:aux-balance'
           AND x.parent_id = m.id
           AND x.deleted_at IS NULL
 );

-- ---------- ② 授权（只读报表：超管 / 管理员 / 会计口径角色）+ 自检 ----------
DO $$
DECLARE
    n_parents integer;
    n_menus   integer;
    n_roles   integer;
    n_grants  integer;
BEGIN
    SELECT count(*) INTO n_parents
      FROM sys_menu
     WHERE auth_code = 'business:erp:ledger:list' AND deleted_at IS NULL;
    IF n_parents <> 1 THEN
        RAISE EXCEPTION
            'V0214 自检失败：总账菜单（auth_code business:erp:ledger:list）应为 1 条，实际 %',
            n_parents;
    END IF;

    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE auth_code = 'business:erp:ledger:aux-balance' AND deleted_at IS NULL;
    IF n_menus <> 1 THEN
        RAISE EXCEPTION
            'V0214 自检失败：辅助账按钮应为 1 条，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0214：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (SELECT id FROM sys_menu
                   WHERE auth_code = 'business:erp:ledger:aux-balance'
                     AND deleted_at IS NULL) AS m
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0214：辅助账权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;
