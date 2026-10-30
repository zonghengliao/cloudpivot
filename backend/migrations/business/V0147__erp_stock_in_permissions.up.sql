-- ============================================================
-- V0147：收货入库单权限码（批次 D · P2-B1 · TSK-202610-000855）
--
-- 依据：`backend/src/business/erp/stockin_dto.rs` 的 `STOCK_IN_PERMISSIONS`（6 个码）。
-- 权限码必须与 Rust 声明**逐字一致**，否则接口 403。
--
--   business:erp:stock-in:list    收货入库单列表   GET    /business/erp/stock-in/page
--   business:erp:stock-in:detail  收货入库单详情   GET    /business/erp/stock-in/{id}
--   business:erp:stock-in:create  新建收货入库单   POST   /business/erp/stock-in
--   business:erp:stock-in:update  编辑收货入库单   PUT    /business/erp/stock-in/{id}
--   business:erp:stock-in:delete  删除收货入库单   DELETE /business/erp/stock-in/{id}
--   business:erp:stock-in:status  过账 / 作废     PATCH  /business/erp/stock-in/{id}/status
--
-- # 为什么需要这条迁移
--
--   V0145（P2-A1）只建了 `biz_stock_in` / `biz_stock_in_item` 两张表，没种权限码；
--   P2-B1 才把接口做出来。没有码的话只有持通配权限（`*:*:*`）的超管能调，仓管全 403。
--
-- # 为什么 :status 独立于 :update
--
--   过账会**真的加库存**（移动加权 + 库存流水 + 一物一码序列号 + 回写采购已收数量），
--   与「改几行草稿明细」不是同一类授权。与 W3b 的 `business:erp:transfer:status`、
--   P2-A2 的 `business:erp:purchase-order:status` 同口径。
--
-- # 与卡面的差异
--
--   卡面 §二 只列了接口、没提权限码与迁移 —— 本迁移是「让接口对非超管可用」的必要补齐
--   （W2b / W3b / P2-A2 同类卡各自补过一条）。硬红线 5「不改 V0137-V0145」照做：
--   本文件是**新增**的 V0147，不碰任何既有迁移。
--   本仓没有 `sys_permission` 表：权限码的唯一来源是 `sys_menu.auth_code`，
--   登录装载权限时经 `sys_role_menu` 挂到角色上。
--
-- # ID 段
--
--   V0141 用 0940–0978，V0142 用 0981–0985，V0143 用 0986，V0144 用 0987–0992，
--   V0146 用 0993–0997。开工前 `select right(id::text,4) from sys_menu order by 1` 扫过全量，
--   **0998 起为空段**。
--   0998 是「收货入库」菜单节点（menu_type 2，本批第一次出现这个业务对象），
--   0999–1003 是 5 个按钮（menu_type 3）。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。
--
-- # 空库首启
--
--   迁移先于 bootstrap 跑，此刻 `sys_role` 可能一个角色都没有 → 授权段是空操作。
--   自检块按「角色数 = 0 则只打 NOTICE，不抛异常」处理（V0141–V0146 同款）。
-- ============================================================

-- ---------- ① 菜单节点（menu_type 2） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000998'::uuid, 'business:erp:stock-in:list', 2,
     'business:erp:stock-in:list', '/business/erp/stock-in',
     '20000000-0000-0000-0000-000000000953'::uuid, 'business/erp/stock-in/index',
     6, 1, NULL, '收货入库', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 五个按钮（menu_type 3，挂在 0998 下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000999'::uuid, 'business:erp:stock-in:detail', 3,
     'business:erp:stock-in:detail', NULL,
     '20000000-0000-0000-0000-000000000998'::uuid, NULL,
     1, 1, NULL, '收货入库单详情（含明细与序列号件数）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001000'::uuid, 'business:erp:stock-in:create', 3,
     'business:erp:stock-in:create', NULL,
     '20000000-0000-0000-0000-000000000998'::uuid, NULL,
     2, 1, NULL, '新建收货入库单', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001001'::uuid, 'business:erp:stock-in:update', 3,
     'business:erp:stock-in:update', NULL,
     '20000000-0000-0000-0000-000000000998'::uuid, NULL,
     3, 1, NULL, '编辑收货入库单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001002'::uuid, 'business:erp:stock-in:delete', 3,
     'business:erp:stock-in:delete', NULL,
     '20000000-0000-0000-0000-000000000998'::uuid, NULL,
     4, 1, NULL, '删除收货入库单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001003'::uuid, 'business:erp:stock-in:status', 3,
     'business:erp:stock-in:status', NULL,
     '20000000-0000-0000-0000-000000000998'::uuid, NULL,
     5, 1, NULL, '收货入库单状态流转（过账 / 作废）', FALSE, 'business')
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
     WHERE id IN ('20000000-0000-0000-0000-000000000998'::uuid,
                  '20000000-0000-0000-0000-000000000999'::uuid,
                  '20000000-0000-0000-0000-000000001000'::uuid,
                  '20000000-0000-0000-0000-000000001001'::uuid,
                  '20000000-0000-0000-0000-000000001002'::uuid,
                  '20000000-0000-0000-0000-000000001003'::uuid);
    IF n_menus <> 6 THEN
        RAISE EXCEPTION 'V0147 自检失败：预期 6 个权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0147：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000000998'::uuid),
            ('20000000-0000-0000-0000-000000000999'::uuid),
            ('20000000-0000-0000-0000-000000001000'::uuid),
            ('20000000-0000-0000-0000-000000001001'::uuid),
            ('20000000-0000-0000-0000-000000001002'::uuid),
            ('20000000-0000-0000-0000-000000001003'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'PURCHASER', 'BUYER', 'WAREHOUSE_KEEPER')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0147：收货入库 6 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;
