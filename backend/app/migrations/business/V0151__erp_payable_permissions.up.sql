-- ============================================================
-- V0151：应付单查询权限码（批次 D · P2-C2 · TSK-202610-000861）
--
-- 依据：`backend/src/business/erp/payable_query.rs` 的 `#[nik::permission]` 声明。
-- 权限码必须与 Rust 声明**逐字一致**，否则接口 403。
--
--   business:erp:payable:list      应付单分页    GET /business/erp/payable/page
--   business:erp:payable:detail    应付单详情    GET /business/erp/payable/{id}
--
-- # 为什么需要这条迁移
--
--   P2-C1（V0148）只建了 `biz_payable` 表并在过账时生成应付，没种权限码
--   （它的模块头明写「可见性由 business:erp:payable:* 权限码控制」）；
--   P2-D1（V0150）只补了凭证 / 总账的码。没有码时只有持通配权限（`*:*:*`）的超管能调，
--   财务 / 会计全 403 —— 本卡就是补这个缺口。
--
-- # 为什么迁移号是 V0151 而不是卡面写的 V0149
--
--   卡面 §三 写「V0149 种进 sys_permission + sys_menu + 角色授权」，但 V0149 已被
--   P2-C1 占用（`V0149__erp_voucher.up.sql`，凭证三表），V0150 被 P2-D1 占用（凭证/总账权限码）。
--   硬红线 4 禁止改 V0137–V0148，卡面也没允许改 V0149 —— 所以顺延到 **V0151**。
--   另：本仓**没有 `sys_permission` 表**，权限码的唯一来源是 `sys_menu.auth_code`
--   （登录装载权限时经 `sys_role_menu` 挂到角色上），与 V0141–V0150 同口径。
--
-- # ID 段
--
--   开工 `select right(id::text,4) from sys_menu where right(id::text,4) >= '0900'` 扫过全量：
--   0940–0978（V0141）、0981–0999（V0142–V0147）、1000–1005 / 1007–1008（V0146–V0150）已用，
--   **1009 起为空段**。1009 = 「应付单」菜单（auth_code 就是 `business:erp:payable:list`），
--   1010 = 它的「详情」按钮。菜单节点自带 `:list` 码是 V0141–V0150 的既有口径。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。
--
-- # 空库首启
--
--   迁移先于 bootstrap 跑，此刻 `sys_role` 可能一个角色都没有 → 授权段是空操作。
--   自检块按「角色数 = 0 则只打 NOTICE，不抛异常」处理（V0141–V0150 同款）。
-- ============================================================

-- ---------- ① 应付单菜单节点（menu_type 2，auth_code = :list） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001009'::uuid, 'business:erp:payable:list', 2,
     'business:erp:payable:list', '/business/erp/payable',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/payable/index',
     10, 1, NULL, '应付单', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 应付详情按钮（menu_type 3） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001010'::uuid, 'business:erp:payable:detail', 3,
     'business:erp:payable:detail', NULL,
     '20000000-0000-0000-0000-000000001009'::uuid, NULL,
     1, 1, NULL, '应付详情（含关联凭证）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 授权给已存在的角色（空库首启时是空操作） ----------
DO $$
DECLARE
    n_roles   integer;
    n_menus   integer;
    n_grants  integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001009'::uuid,
                  '20000000-0000-0000-0000-000000001010'::uuid);
    IF n_menus <> 2 THEN
        RAISE EXCEPTION 'V0151 自检失败：预期 2 个权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0151：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    -- 应付是财务数据：给超管 / 管理员与财务相关角色（角色不存在时自然 0 行，不影响幂等）
    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000001009'::uuid),
            ('20000000-0000-0000-0000-000000001010'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0151：应付 2 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;
