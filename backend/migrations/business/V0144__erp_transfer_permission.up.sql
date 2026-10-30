-- ============================================================
-- V0144：库存调拨单权限码（批次 D · W3b · TSK-202610-000682）
--
-- 依据：`backend/src/business/erp/transfer_dto.rs` 的 `TRANSFER_PERMISSIONS`
-- （6 个码）。权限码必须与 Rust 声明**逐字一致**，否则接口 403。
--
--   business:erp:transfer:list     调拨单列表      GET   /business/erp/transfer/page
--   business:erp:transfer:detail   调拨单详情      GET   /business/erp/transfer/{id}
--                                                 GET   /business/erp/transfer/{id}/serials
--   business:erp:transfer:create   新建调拨单      POST  /business/erp/transfer
--   business:erp:transfer:update   编辑调拨单      PUT   /business/erp/transfer/{id}
--   business:erp:transfer:delete   删除调拨单      DELETE /business/erp/transfer/{id}
--   business:erp:transfer:status   状态流转        PATCH /business/erp/transfer/{id}/status
--
-- # 为什么 :status 独立于 :update
--
--   状态流转会**发起审批单**（DRAFT→PENDING）、**终止审批单**（PENDING→CANCELLED）、
--   并在 IN_TRANSIT→DONE 触发库存双写（W3c）。这与「改几行明细」不是同一类授权，
--   与 W2b 的 `business:erp:serial:status` 同口径。
--
-- # 与卡面的差异（以真实 schema 为准，与 V0141/V0142/V0143 同款）
--
--   · 卡面写「V0144 种进 sys_permission + sys_menu」，但**本仓没有 `sys_permission` 表**：
--     权限码的唯一来源是 `sys_menu.auth_code`（`system/menu/model.rs` 文件头），
--     登录装载权限时经 `sys_role_menu` 把 `auth_code` 挂到角色上。
--   · 卡面列的 6 个码与本迁移**逐字一致**，无差异。
--
-- # ID 段
--
--   0987（菜单）/ 0988–0993（6 个按钮）。09xx 段已用到 0986：
--     · 任务看板 0900–0906 / ERP 主数据与台账菜单 0940–0957 / 权限按钮 0960–0978
--     · 序列号 0981–0985 / 库存巡检按钮 0986
--   动手前已 `select right(id::text,3) from sys_menu` 扫过全量，0987 起为空段。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。
--
-- # 空库首启
--
--   迁移先于 bootstrap 跑，此刻 `sys_role` 里可能一个角色都没有 → 授权段是空操作。
--   自检块按「角色数 = 0 则只打 NOTICE，不抛异常」处理（V0141/V0142/V0143 同款）：
--   bootstrap 之后再重放一次本迁移即可补齐授权（幂等）。
--
-- # 乱序补迁
--
--   若本迁移晚于更大版本号应用（并行卡抢号），启动日志会出现
--   「待应用迁移的版本低于已应用最大值（乱序补迁）」——不阻断，应用成功后重启即
--   「应用 0 个迁移」。
-- ============================================================

-- ---------- ① 菜单节点：库存调拨（menu_type 2，挂在 0946「ERP 台账」下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000987'::uuid, '库存调拨', 2,
     'business:erp:transfer:list', '/business/erp/transfer',
     '20000000-0000-0000-0000-000000000946'::uuid, NULL,
     20, 1, NULL, '库存调拨', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 六个按钮（menu_type 3，挂在 0987 菜单下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000988'::uuid, 'business:erp:transfer:detail', 3,
     'business:erp:transfer:detail', NULL, '20000000-0000-0000-0000-000000000987'::uuid, NULL,
     1, 1, NULL, '调拨单详情（含明细/序列号）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000989'::uuid, 'business:erp:transfer:create', 3,
     'business:erp:transfer:create', NULL, '20000000-0000-0000-0000-000000000987'::uuid, NULL,
     2, 1, NULL, '新建调拨单', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000990'::uuid, 'business:erp:transfer:update', 3,
     'business:erp:transfer:update', NULL, '20000000-0000-0000-0000-000000000987'::uuid, NULL,
     3, 1, NULL, '编辑调拨单（仅草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000991'::uuid, 'business:erp:transfer:delete', 3,
     'business:erp:transfer:delete', NULL, '20000000-0000-0000-0000-000000000987'::uuid, NULL,
     4, 1, NULL, '删除调拨单（草稿/已取消）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000992'::uuid, 'business:erp:transfer:status', 3,
     'business:erp:transfer:status', NULL, '20000000-0000-0000-0000-000000000987'::uuid, NULL,
     5, 1, NULL, '调拨单状态流转（提交/发货/收货/撤回/取消）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 说明：卡面的 6 个码里，`business:erp:transfer:list` 挂在菜单节点 ① 上（与 0941/0955
-- 等既有菜单同款：菜单本身就是 list 码），按钮段只补 5 个动词码。
-- 0987–0992 共 6 个节点 == 6 个权限码，一一对应。

-- ---------- ③ 授权：超管 ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id IN (
        '20000000-0000-0000-0000-000000000987'::uuid,
        '20000000-0000-0000-0000-000000000988'::uuid,
        '20000000-0000-0000-0000-000000000989'::uuid,
        '20000000-0000-0000-0000-000000000990'::uuid,
        '20000000-0000-0000-0000-000000000991'::uuid,
        '20000000-0000-0000-0000-000000000992'::uuid,
        '20000000-0000-0000-0000-000000000993'::uuid
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- ④ 授权：laoliu（用户名或角色名/码含 laoliu，与 V0142/V0143 同款双路解析） ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT src.role_id, m.id
  FROM sys_menu m
  JOIN (
        SELECT ur.role_id AS role_id
          FROM sys_user_role ur
          JOIN sys_user u ON u.id = ur.user_id
         WHERE u.deleted_at IS NULL
           AND lower(u.username) = 'laoliu'
        UNION
        SELECT r2.id AS role_id
          FROM sys_role r2
         WHERE r2.deleted_at IS NULL
           AND (lower(r2.role_name) LIKE '%laoliu%' OR lower(r2.role_code) LIKE '%laoliu%')
       ) src ON TRUE
 WHERE m.id IN (
        '20000000-0000-0000-0000-000000000987'::uuid,
        '20000000-0000-0000-0000-000000000988'::uuid,
        '20000000-0000-0000-0000-000000000989'::uuid,
        '20000000-0000-0000-0000-000000000990'::uuid,
        '20000000-0000-0000-0000-000000000991'::uuid,
        '20000000-0000-0000-0000-000000000992'::uuid,
        '20000000-0000-0000-0000-000000000993'::uuid
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = src.role_id AND rm.menu_id = m.id
   );

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    n_menu    integer;
    n_btn     integer;
    n_parent  integer;
    n_roles   integer;
    n_granted integer;
    n_dup     integer;
    missing   text;
BEGIN
    -- ① 菜单节点存在、类型正确
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000987'::uuid
       AND menu_type = 2
       AND auth_code = 'business:erp:transfer:list'
       AND deleted_at IS NULL;
    IF n_menu <> 1 THEN
        RAISE EXCEPTION 'V0144 自检失败：库存调拨菜单节点缺失或类型/auth_code 不对';
    END IF;

    -- ② 必须挂在 0946「ERP 台账」目录下（挂错位置前端看不见）
    SELECT count(*) INTO n_parent
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id = '20000000-0000-0000-0000-000000000987'::uuid
       AND p.id = '20000000-0000-0000-0000-000000000946'::uuid;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0144 自检失败：库存调拨菜单没有挂在 0946「ERP 台账」下';
    END IF;

    -- ③ 6 个权限码逐个必须存在且唯一（与 Rust 的 TRANSFER_PERMISSIONS 逐字一致）
    FOR missing IN
        SELECT code FROM unnest(ARRAY[
            'business:erp:transfer:list',
            'business:erp:transfer:detail',
            'business:erp:transfer:create',
            'business:erp:transfer:update',
            'business:erp:transfer:delete',
            'business:erp:transfer:status'
        ]) AS t(code)
    LOOP
        SELECT count(*) INTO n_dup
          FROM sys_menu
         WHERE auth_code = missing AND deleted_at IS NULL;
        IF n_dup <> 1 THEN
            RAISE EXCEPTION 'V0144 自检失败：权限码 % 出现 % 次（应为 1）', missing, n_dup;
        END IF;
    END LOOP;

    -- ④ 5 个动词按钮必须挂在 0987 菜单下
    SELECT count(*) INTO n_btn
      FROM sys_menu c
     WHERE c.parent_id = '20000000-0000-0000-0000-000000000987'::uuid
       AND c.menu_type = 3
       AND c.auth_code IN (
            'business:erp:transfer:detail',
            'business:erp:transfer:create',
            'business:erp:transfer:update',
            'business:erp:transfer:delete',
            'business:erp:transfer:status'
       );
    IF n_btn <> 5 THEN
        RAISE EXCEPTION 'V0144 自检失败：5 个动词按钮只找到 % 个', n_btn;
    END IF;

    -- ⑤ 授权：空库首启时角色还不存在 → 只 NOTICE，不阻断
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0144：库里还没有任何角色（迁移先于 bootstrap），跳过授权核对；'
                     'bootstrap 之后重放本迁移即可补齐';
    ELSE
        SELECT count(*) INTO n_granted
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = 'SUPER_ADMIN'
           AND rm.menu_id = '20000000-0000-0000-0000-000000000987'::uuid;
        IF n_granted = 0 THEN
            RAISE EXCEPTION 'V0144 自检失败：超管没有拿到库存调拨菜单授权';
        END IF;
    END IF;

    RAISE NOTICE 'V0144 自检通过：1 个菜单（0987）+ 5 个按钮 + 6 个权限码挂在 0946 下';
END $$;
