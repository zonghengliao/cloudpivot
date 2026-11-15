-- ============================================================
-- V0213：联动作废两个接口的权限码（ARCH-3 §6 R-P1-5 · 断链 DC-24）
--
-- # 补的是什么
--
-- 新增两个端点：
--   GET  /business/erp/{doc_type}/{id}/cancel-impact   作废影响分析（只读）
--   POST /business/erp/{doc_type}/{id}/cancel-cascade  联动作废（一个事务）
--
-- 一个端点服务三种单据（purchase-order / stock-in / payable），所以权限码挂成
-- **业务线级的按钮**（`business:erp:doc:*`），并同时种到三条业务线菜单下：
-- 谁能打开「采购订单 / 收货入库 / 应付单」任一页面，谁才看得见对应的按钮。
--
-- # 为什么按 auth_code 找父菜单，而不是按 path
--
-- 2026-11-03 实测踩坑：**同一条采购订单菜单，两个库里 path 不一样** ——
-- 全新库（V0141 从头种）是 `/business/erp/purchase-order`，
-- 生产库（历史菜单被改过）是 `/business/erp/purchase`。
-- 第一版按 path IN (...) 匹配：全新库能过，生产库副本上只命中 2/3 条，
-- 自检当场 RAISE EXCEPTION → 迁移失败、实例起不来（自检就是这么抓住它的）。
-- 父菜单的 `auth_code`（`business:erp:{单据}:list`）才是稳定标识，path 会随菜单重组漂移。
--
-- # 为什么只授给 SUPER_ADMIN / ADMIN
--
-- `cancel-cascade` 是**一次改多张账**的动作（作废收货单 + 红冲凭证 + 作废应付 +
-- 作废采购单），生产上等价于「把三个人各自的复核一次做完」。默认只发管理员；
-- 想让采购员自己就能联动作废的客户，在「角色-菜单」里勾上这两个按钮即可。
--
-- # 幂等
--
-- 重跑安全：菜单按「auth_code + parent_id」判在不在，授权按 `sys_role_menu` 唯一性判。
-- ============================================================

-- ---------- ① 两个按钮 × 三条业务线菜单 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT gen_random_uuid(), v.code, 3, v.code, NULL, m.id, NULL,
       v.sort_order, 1, NULL, v.title, FALSE, 'business'
  FROM (VALUES
        ('business:erp:doc:cancel-impact',  91, '作废影响分析（先看影响面再动手）'),
        ('business:erp:doc:cancel-cascade', 92, '联动作废（一个事务按顺序作废下游单据）')
       ) AS v(code, sort_order, title)
  CROSS JOIN (SELECT id FROM sys_menu
               WHERE auth_code IN ('business:erp:purchase-order:list',
                                   'business:erp:stock-in:list',
                                   'business:erp:payable:list')
                 AND deleted_at IS NULL) AS m
 WHERE NOT EXISTS (
        SELECT 1 FROM sys_menu x
         WHERE x.auth_code = v.code AND x.parent_id = m.id AND x.deleted_at IS NULL
 );

-- ---------- ② 授权（只给管理员）+ 自检 ----------
DO $$
DECLARE
    n_menus  integer;
    n_parents integer;
    n_roles  integer;
    n_grants integer;
BEGIN
    SELECT count(*) INTO n_parents
      FROM sys_menu
     WHERE auth_code IN ('business:erp:purchase-order:list',
                         'business:erp:stock-in:list',
                         'business:erp:payable:list')
       AND deleted_at IS NULL;
    IF n_parents <> 3 THEN
        RAISE EXCEPTION
            'V0213 自检失败：采购订单 / 收货入库 / 应付单三条业务线菜单（按 auth_code 找）应为 3 条，实际 %',
            n_parents;
    END IF;

    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE auth_code IN ('business:erp:doc:cancel-impact',
                         'business:erp:doc:cancel-cascade')
       AND deleted_at IS NULL;
    -- 三条业务线 × 两个码 = 6 行
    IF n_menus < 6 THEN
        RAISE EXCEPTION
            'V0213 自检失败：预期至少 6 个权限按钮（3 条业务线 × 2 个码），实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0213：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN sys_menu m
     WHERE m.auth_code IN ('business:erp:doc:cancel-impact',
                           'business:erp:doc:cancel-cascade')
       AND r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0213：联动作废 2 个权限码已种（% 个按钮），授权新增 % 行', n_menus, n_grants;
END $$;
