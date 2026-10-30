-- ============================================================
-- V0146：采购订单写权限码（批次 D · P2-A2 · TSK-202610-000854）
--
-- 依据：`backend/src/business/erp/purchase_dto.rs` 的 `PURCHASE_ORDER_PERMISSIONS`
-- （5 个码）。权限码必须与 Rust 声明**逐字一致**，否则接口 403。
--
--   business:erp:purchase-order:detail  采购订单详情   GET    /business/erp/purchase-order/{id}
--   business:erp:purchase-order:create  新建采购订单   POST   /business/erp/purchase-order
--   business:erp:purchase-order:update  编辑采购订单   PUT    /business/erp/purchase-order/{id}
--   business:erp:purchase-order:delete  删除采购订单   DELETE /business/erp/purchase-order/{id}
--   business:erp:purchase-order:status  状态流转       PATCH  /business/erp/purchase-order/{id}/status
--   （列表 `business:erp:purchase-order:list` 由 V0141 种过，本迁移不动）
--
-- # 为什么需要这条迁移
--
--   V0141 只种了列表/明细台账两个只读码。P2-A2 才把「建 / 改 / 删 / 状态流转」四个写接口做出来
--   —— 没有码的话只有持通配权限（`*:*:*`）的超管能调，采购员全 403。
--
-- # 为什么 :status 独立于 :update
--
--   状态流转是不可逆的单据动作（提交后不能改、作废写 closed_at 并把原因拼进 remark），
--   与「改几行明细」不是同一类授权。与 W3b 的 `business:erp:transfer:status` 同口径。
--
-- # 与卡面的差异
--
--   · 卡面 §三 只列了 4 个接口、没提权限码，也没提迁移 —— 本迁移是「让接口对非超管可用」的
--     必要补齐（W2b/W3b 同类卡都各自补过一条）。硬红线 6「不改 V0137-V0145」照做：
--     本文件是**新增**的 V0146，不碰任何既有迁移。
--   · 本仓没有 `sys_permission` 表：权限码的唯一来源是 `sys_menu.auth_code`
--     （`system/menu/model.rs` 文件头），登录装载权限时经 `sys_role_menu` 挂到角色上。
--
-- # ID 段
--
--   V0141 用了 0940–0978（含 0953「采购订单」菜单、0954「采购明细」菜单），
--   V0142 用 0981–0985（序列号），V0143 用 0986（库存巡检），V0144 用 0987–0992（调拨）。
--   动手前 `select right(id::text,4) from sys_menu order by 1` 扫过全量，**0993 起为空段**。
--   5 个按钮挂在既有菜单 0953 下（不新建菜单节点）。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。
--
-- # 空库首启
--
--   迁移先于 bootstrap 跑，此刻 `sys_role` 可能一个角色都没有 → 授权段是空操作。
--   自检块按「角色数 = 0 则只打 NOTICE，不抛异常」处理（V0141–V0144 同款）。
-- ============================================================

-- ---------- ① 五个按钮（menu_type 3，挂在 0953「采购订单」菜单下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000993'::uuid, 'business:erp:purchase-order:detail', 3,
     'business:erp:purchase-order:detail', NULL,
     '20000000-0000-0000-0000-000000000953'::uuid, NULL,
     1, 1, NULL, '采购订单详情（含明细与三金额）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000994'::uuid, 'business:erp:purchase-order:create', 3,
     'business:erp:purchase-order:create', NULL,
     '20000000-0000-0000-0000-000000000953'::uuid, NULL,
     2, 1, NULL, '新建采购订单', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000995'::uuid, 'business:erp:purchase-order:update', 3,
     'business:erp:purchase-order:update', NULL,
     '20000000-0000-0000-0000-000000000953'::uuid, NULL,
     3, 1, NULL, '编辑采购订单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000996'::uuid, 'business:erp:purchase-order:delete', 3,
     'business:erp:purchase-order:delete', NULL,
     '20000000-0000-0000-0000-000000000953'::uuid, NULL,
     4, 1, NULL, '删除采购订单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000997'::uuid, 'business:erp:purchase-order:status', 3,
     'business:erp:purchase-order:status', NULL,
     '20000000-0000-0000-0000-000000000953'::uuid, NULL,
     5, 1, NULL, '采购订单状态流转（提交 / 关闭 / 作废）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 授权给已存在的角色（空库首启时是空操作） ----------
DO $$
DECLARE
    n_roles   integer;
    n_menus   integer;
    n_grants  integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000993'::uuid,
                  '20000000-0000-0000-0000-000000000994'::uuid,
                  '20000000-0000-0000-0000-000000000995'::uuid,
                  '20000000-0000-0000-0000-000000000996'::uuid,
                  '20000000-0000-0000-0000-000000000997'::uuid);
    IF n_menus <> 5 THEN
        RAISE EXCEPTION 'V0146 自检失败：预期 5 个权限按钮，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0146：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000000993'::uuid),
            ('20000000-0000-0000-0000-000000000994'::uuid),
            ('20000000-0000-0000-0000-000000000995'::uuid),
            ('20000000-0000-0000-0000-000000000996'::uuid),
            ('20000000-0000-0000-0000-000000000997'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'PURCHASER', 'BUYER')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0146：采购订单 5 个写权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;
