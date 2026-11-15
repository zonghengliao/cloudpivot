-- ============================================================
-- V0143：库存巡检权限码（批次 D · W2c-2 · 云枢 TSK-202610-000764）
--
-- 依据：`backend/src/business/erp/inventory_audit.rs` 的 `AUDIT_PERMISSIONS`
-- （1 个码 `business:erp:inventory:audit`）。权限码必须与 Rust 声明**逐字一致**，
-- 否则接口 403。
--
-- # 与卡面的差异（以真实 schema 为准）
--
--   · 卡面写「种进 `sys_permission`」，但**本仓没有 `sys_permission` 表**：
--     权限码的唯一来源是 `sys_menu.auth_code`（`system/menu/model.rs` 文件头），
--     登录装载权限时经 `sys_role_menu` 把 `auth_code` 挂到角色上。与 V0141/V0142 同口径。
--   · 卡面写「或者复用 business:erp:inventory:list + 要求超管」——已按卡面的**决策**做
--     新码，不动 `:list`（巡检能改库，与只读列表必须分开授权）。
--
-- # 码表（1 个）
--
--   business:erp:inventory:audit   库存巡检（GET /business/erp/inventory/audit）
--                                  menu_type=3（按钮），挂在 0955「即时库存」菜单下
--
-- # ID 段
--
--   0986（09xx 段已用到 0985：任务看板 0900–0906、ERP 主数据/台账 0940–0978、
--         序列号 0981–0985）。本迁移只加 1 个按钮节点，不新建菜单/目录。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。
--
-- # 空库首启
--
--   迁移先于 bootstrap 跑，此刻 `sys_role` 里可能一个角色都没有 → 授权段是空操作。
--   自检块按「角色数 = 0 则只打 NOTICE，不抛异常」处理（与 V0141/V0142 同款）：
--   bootstrap 之后再重放一次本迁移即可补齐授权（幂等，实测）。
--
-- # 乱序补迁
--
--   若本迁移晚于更大版本号应用（并行卡抢号），启动日志会出现
--   「待应用迁移的版本低于已应用最大值（乱序补迁）」——不阻断，应用成功后重启即
--   「应用 0 个迁移」。
-- ============================================================

-- ---------- ① 巡检按钮（menu_type 3，auth_code 与 name 同值，与 V0141/V0142 一致） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000986'::uuid, 'business:erp:inventory:audit', 3,
     'business:erp:inventory:audit', NULL, '20000000-0000-0000-0000-000000000955'::uuid, NULL,
     10, 1, NULL, '库存巡检（账面 vs 序列号件数）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 授权：超管 ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id = '20000000-0000-0000-0000-000000000986'::uuid
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- ③ 授权：laoliu（用户名或角色名/码含 laoliu，与 V0142 同款双路解析） ----------
-- 注意：派生表的列名取自 UNION 的第一支（`role_id`），别名后必须写 `src.role_id`。
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
 WHERE m.id = '20000000-0000-0000-0000-000000000986'::uuid
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = src.role_id AND rm.menu_id = m.id
   );

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    n_btn     integer;
    n_parent  integer;
    n_roles   integer;
    n_granted integer;
    n_same    integer;
BEGIN
    -- ① 按钮节点存在、类型正确
    SELECT count(*) INTO n_btn
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000986'::uuid
       AND menu_type = 3
       AND auth_code = 'business:erp:inventory:audit'
       AND deleted_at IS NULL;
    IF n_btn <> 1 THEN
        RAISE EXCEPTION 'V0143 自检失败：巡检按钮节点缺失或类型/auth_code 不对';
    END IF;

    -- ② 必须挂在 0955「即时库存」菜单下（挂错位置前端看不见）
    SELECT count(*) INTO n_parent
      FROM sys_menu c
      JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id = '20000000-0000-0000-0000-000000000986'::uuid
       AND p.id = '20000000-0000-0000-0000-000000000955'::uuid
       AND p.menu_type = 2;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0143 自检失败：巡检按钮没有挂在 0955「即时库存」菜单下';
    END IF;

    -- ③ 码不得与既有菜单重复（auth_code 全局唯一才不会被角色权限合并吃掉）
    SELECT count(*) INTO n_same
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory:audit'
       AND deleted_at IS NULL;
    IF n_same <> 1 THEN
        RAISE EXCEPTION 'V0143 自检失败：business:erp:inventory:audit 出现 % 次（应为 1）', n_same;
    END IF;

    -- ④ 授权：空库首启时角色还不存在 → 只 NOTICE，不阻断
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0143：库里还没有任何角色（迁移先于 bootstrap），跳过授权核对；'
                     'bootstrap 之后重放本迁移即可补齐';
    ELSE
        SELECT count(*) INTO n_granted
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = 'SUPER_ADMIN'
           AND rm.menu_id = '20000000-0000-0000-0000-000000000986'::uuid;
        IF n_granted = 0 THEN
            RAISE EXCEPTION 'V0143 自检失败：超管没有拿到库存巡检授权';
        END IF;
    END IF;

    RAISE NOTICE 'V0143 自检通过：1 个权限码（business:erp:inventory:audit）挂在 0955 下';
END $$;
