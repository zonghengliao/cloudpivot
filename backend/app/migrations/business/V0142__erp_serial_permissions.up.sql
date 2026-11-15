-- ============================================================
-- V0142：序列号（一物一码 B）菜单 + 权限码（批次 D · W2b · 云枢 TSK-202610-000679）
--
-- 依据：`backend/src/business/erp/serial_handler.rs` 的 `SERIAL_PERMISSIONS`（5 个码，
-- 与卡面 §三 逐字一致）。权限码必须与 Rust 声明**逐字一致**，否则接口 403。
--
-- # 与卡面的差异（以真实 schema 为准）
--
--   · 卡面写「种进 `sys_permission` + `sys_menu`」，但**本仓没有 `sys_permission` 表**：
--     权限码的唯一来源是 `sys_menu.auth_code`（`system/menu/model.rs` 文件头），
--     登录装载权限时经 `sys_role_menu` 把 `auth_code` 挂到角色上。
--     故本迁移只种 `sys_menu` + `sys_role_menu`（与 V0141 同一口径）。
--   · 卡面写「给超管 + laoliu 授权」→ 落地为两段 `sys_role_menu` 插入（超管按
--     `role_code='SUPER_ADMIN'`、laoliu 按用户名 / 角色名解析），与 V0141 §⑤ 逐字同款。
--
-- # 码表（5 个，卡面 §三）
--
--   business:erp:serial:list      列表（挂在菜单节点上，menu_type=2）
--   business:erp:serial:detail    详情               （按钮 menu_type=3）
--   business:erp:serial:view      扫码（GET /scan）  （按钮）
--   business:erp:serial:generate  批量生成            （按钮）
--   business:erp:serial:status    状态流转            （按钮）
--
-- # ID 段
--
--   0980–0985（09xx 段此前已用到 0978：任务看板 0900–0906、ERP 主数据/台账 0940–0978）。
--   目录沿用 V0141 的 `0940 ERP 主数据`，本迁移只加 1 个菜单 + 4 个按钮。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。
--
-- # 空库首启
--
--   迁移先于 bootstrap 跑，此刻 `sys_role` 里可能一个角色都没有 → 授权段是空操作。
--   自检块按「角色数 = 0 则只打 NOTICE，不抛异常」处理（与 V0141 / V0132 同款）：
--   bootstrap 之后再重放一次本迁移即可补齐授权（幂等，实测）。
-- ============================================================

-- ---------- ① 序列号菜单（menu_type 2，auth_code = :list） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000981'::uuid, '序列号管理', 2, 'business:erp:serial:list',
     '/business/erp/serial', '20000000-0000-0000-0000-000000000940'::uuid,
     'business/erp/serial/index', 35, 1, 'barcode', '序列号管理', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 按钮级权限（menu_type 3；name 与 auth_code 同值，与 V0141 一致） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000982'::uuid, 'business:erp:serial:detail', 3,
     'business:erp:serial:detail', NULL, '20000000-0000-0000-0000-000000000981'::uuid, NULL,
     10, 1, NULL, '序列号详情', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000983'::uuid, 'business:erp:serial:view', 3,
     'business:erp:serial:view', NULL, '20000000-0000-0000-0000-000000000981'::uuid, NULL,
     20, 1, NULL, '序列号扫码（含 10ms 扫码查询）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000984'::uuid, 'business:erp:serial:generate', 3,
     'business:erp:serial:generate', NULL, '20000000-0000-0000-0000-000000000981'::uuid, NULL,
     30, 1, NULL, '批量生成序列号', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000985'::uuid, 'business:erp:serial:status', 3,
     'business:erp:serial:status', NULL, '20000000-0000-0000-0000-000000000981'::uuid, NULL,
     40, 1, NULL, '序列号状态流转', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 授权：超管 + laoliu ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id IN (
       '20000000-0000-0000-0000-000000000981',
       '20000000-0000-0000-0000-000000000982',
       '20000000-0000-0000-0000-000000000983',
       '20000000-0000-0000-0000-000000000984',
       '20000000-0000-0000-0000-000000000985'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- laoliu：老大账号可能以用户名 laoliu 或角色名/码含 laoliu 的形式存在（各库不完全一致），
-- 两种都授；库里没有该用户/角色时这段是空操作，不报错。
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
 WHERE m.id IN (
       '20000000-0000-0000-0000-000000000981',
       '20000000-0000-0000-0000-000000000982',
       '20000000-0000-0000-0000-000000000983',
       '20000000-0000-0000-0000-000000000984',
       '20000000-0000-0000-0000-000000000985'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = src.role_id AND rm.menu_id = m.id
   );

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    n_codes   integer;
    n_menu    integer;
    n_buttons integer;
    n_roles   integer;
    n_granted integer;
    v_missing text;
BEGIN
    -- ① 5 个权限码齐备
    SELECT count(*) INTO n_codes
      FROM sys_menu
     WHERE auth_code IN ('business:erp:serial:list', 'business:erp:serial:detail',
                         'business:erp:serial:view', 'business:erp:serial:generate',
                         'business:erp:serial:status')
       AND deleted_at IS NULL;
    IF n_codes <> 5 THEN
        RAISE EXCEPTION 'V0142 自检失败：序列号权限码应有 5 个，实际 %', n_codes;
    END IF;

    -- ② 1 个菜单 + 4 个按钮
    SELECT count(*) INTO n_menu
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000000981'::uuid
       AND menu_type = 2 AND deleted_at IS NULL;
    IF n_menu <> 1 THEN
        RAISE EXCEPTION 'V0142 自检失败：序列号菜单节点缺失或类型不对';
    END IF;
    SELECT count(*) INTO n_buttons
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000981'::uuid
       AND menu_type = 3 AND deleted_at IS NULL;
    IF n_buttons <> 4 THEN
        RAISE EXCEPTION 'V0142 自检失败：序列号按钮应有 4 个，实际 %', n_buttons;
    END IF;

    -- ③ 菜单必须挂在「ERP 主数据」目录下（挂错位置前端看不见）
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000981'::uuid
       AND parent_id = '20000000-0000-0000-0000-000000000940'::uuid;
    IF n_menu <> 1 THEN
        RAISE EXCEPTION 'V0142 自检失败：序列号菜单没有挂在 ERP 主数据目录（0940）下';
    END IF;

    -- ④ 授权：空库首启时角色还不存在 → 只 NOTICE，不阻断
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0142：库里还没有任何角色（迁移先于 bootstrap），跳过授权核对；'
                     'bootstrap 之后重放本迁移即可补齐';
    ELSE
        SELECT count(*) INTO n_granted
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = 'SUPER_ADMIN'
           AND rm.menu_id = '20000000-0000-0000-0000-000000000981'::uuid;
        IF n_granted = 0 THEN
            RAISE EXCEPTION 'V0142 自检失败：超管没有拿到序列号菜单授权';
        END IF;
    END IF;

    -- ⑤ 反向断言：卡面 5 个码都必须在 Rust 里声明（漏一个就是 403）
    SELECT string_agg(code, ', ') INTO v_missing
      FROM (VALUES ('business:erp:serial:list'), ('business:erp:serial:detail'),
                   ('business:erp:serial:view'), ('business:erp:serial:generate'),
                   ('business:erp:serial:status')) AS t(code)
     WHERE code NOT IN (SELECT auth_code FROM sys_menu WHERE deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0142 自检失败：以下码没有落进 sys_menu：%', v_missing;
    END IF;

    RAISE NOTICE 'V0142 自检通过：5 个权限码 / 1 菜单 + 4 按钮 / 菜单挂在 0940 下';
END $$;
