-- ============================================================
-- V0150：凭证查询 / 总账 / 科目余额表权限码（批次 D · P2-D1 · TSK-202610-000857）
--
-- 依据：`backend/src/business/erp/voucher_query.rs`（`business:erp:voucher:*`）与
--       `backend/src/business/erp/ledger.rs`（`business:erp:ledger:*`）的 `#[nik::permission]` 声明。
-- 权限码必须与 Rust 声明**逐字一致**，否则接口 403。
--
--   business:erp:voucher:list            凭证查询列表    GET /business/erp/voucher/page
--   business:erp:voucher:detail          凭证详情        GET /business/erp/voucher/{id}
--   business:erp:ledger:list             科目流水（总账）GET /business/erp/ledger/page
--   business:erp:ledger:trial-balance    科目余额表      GET /business/erp/ledger/trial-balance
--
-- # 为什么需要这条迁移
--
--   P2-C1（V0149）只建了 `biz_voucher` / `biz_voucher_entry` / `biz_voucher_rule` 三张表，
--   没种权限码（它的模块头明写「可见性由 business:erp:voucher:* 权限码控制（P2-D1 落地）」）。
--   没有码的话只有持通配权限（`*:*:*`）的超管能调，会计 / 财务全 403。
--   P2-B1 的 V0147、P2-A2 的 V0146 是同一性质的补齐。
--
-- # 为什么「余额表」独立于「流水」
--
--   余额表是全公司口径的**汇总报表**（Σ借 = Σ贷 的对外交付物），流水是某个科目的明细。
--   前者给财务主管 / 审计看，后者给会计对账用 —— 授权粒度本来就不该一样。
--
-- # 与卡面的差异
--
--   卡面 §六 的 `git add` 清单里没有本迁移（只有两个 .rs + 验收脚本）。
--   但卡面 §二 要求 4 个接口都带权限码、硬红线 4 只禁止改 V0137–V0148：
--   本文件是**新增**的 V0150，不碰任何既有迁移。V0149 属 P2-C1，也不改。
--   本仓没有 `sys_permission` 表：权限码的唯一来源是 `sys_menu.auth_code`，
--   登录装载权限时经 `sys_role_menu` 挂到角色上。
--
-- # ID 段
--
--   V0141 用 0940–0978，V0142 用 0981–0985，V0143 用 0986，V0144 用 0987–0992，
--   V0146 用 0993–0997，V0147 用 0998–1003。开工前
--   `select right(id::text,4) from sys_menu order by 1` 扫过全量，**1004 起为空段**。
--   1004 = 「凭证查询」菜单（auth_code 就是 `business:erp:voucher:list`），
--   1005 = 它的「详情」按钮；
--   1007 = 「总账 / 科目余额表」菜单（auth_code `business:erp:ledger:list`），
--   1008 = 它的「余额表」按钮。
--   菜单节点自带 `:list` 码是 V0141–V0147 的既有口径（菜单与其主按钮同码）。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。
--
-- # 空库首启
--
--   迁移先于 bootstrap 跑，此刻 `sys_role` 可能一个角色都没有 → 授权段是空操作。
--   自检块按「角色数 = 0 则只打 NOTICE，不抛异常」处理（V0141–V0147 同款）。
-- ============================================================

-- ---------- ① 凭证查询菜单节点（menu_type 2，auth_code = :list） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001004'::uuid, 'business:erp:voucher:list', 2,
     'business:erp:voucher:list', '/business/erp/voucher',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/voucher/index',
     8, 1, NULL, '凭证查询', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 凭证详情按钮（menu_type 3） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001005'::uuid, 'business:erp:voucher:detail', 3,
     'business:erp:voucher:detail', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     1, 1, NULL, '凭证详情（含全部分录）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 总账 / 科目余额表菜单节点（menu_type 2，auth_code = :list） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001007'::uuid, 'business:erp:ledger:list', 2,
     'business:erp:ledger:list', '/business/erp/ledger',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/ledger/index',
     9, 1, NULL, '总账 / 科目余额表', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ④ 科目余额表按钮（menu_type 3） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001008'::uuid, 'business:erp:ledger:trial-balance', 3,
     'business:erp:ledger:trial-balance', NULL,
     '20000000-0000-0000-0000-000000001007'::uuid, NULL,
     1, 1, NULL, '科目余额表（Σ借 = Σ贷 校验）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 授权给已存在的角色（空库首启时是空操作） ----------
DO $$
DECLARE
    n_roles   integer;
    n_menus   integer;
    n_grants  integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001004'::uuid,
                  '20000000-0000-0000-0000-000000001005'::uuid,
                  '20000000-0000-0000-0000-000000001007'::uuid,
                  '20000000-0000-0000-0000-000000001008'::uuid);
    IF n_menus <> 4 THEN
        RAISE EXCEPTION 'V0150 自检失败：预期 4 个权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0150：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    -- 只读报表：给超管 / 管理员 / 会计相关角色（角色不存在时自然 0 行，不影响幂等）
    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000001004'::uuid),
            ('20000000-0000-0000-0000-000000001005'::uuid),
            ('20000000-0000-0000-0000-000000001007'::uuid),
            ('20000000-0000-0000-0000-000000001008'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0150：凭证 / 总账 4 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;
