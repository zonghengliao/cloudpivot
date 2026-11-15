-- ============================================================
-- V0141：ERP 主数据菜单 + 权限码（批次 D · W1c · 云枢 TSK-202610-000677）
--
-- 依据：`backend/src/business/erp/handler.rs` 的 `#[nik::permission]` 声明
-- （24 个卡面码 `CARD_PERMISSIONS` + 8 个只读码 `READONLY_PERMISSIONS`）。
-- 权限码必须与 Rust 声明**逐字一致**，否则接口 403；改这里必须同步改 Rust（反之亦然）。
--
-- # 与卡面的差异（以真实 schema 为准）
--
-- 卡面写「种进 sys_permission + sys_menu」，但**本仓没有 `sys_permission` 表**：
-- `system/menu/model.rs` 文件头明写「`auth_code` 是权限码的唯一来源」，
-- 登录装载权限时读的是 `sys_menu.auth_code`（经 `sys_role_menu` 关联到角色）。
-- 因此本迁移只种 `sys_menu`（`menu_type` 1 目录 / 2 菜单 / 3 按钮）+ `sys_role_menu`。
--
-- 卡面点名的 `biz_material_category` 表在 V0137 里不存在（物料分类属 V0138+），
-- 但卡面明确要求 `business:erp:category:*` 3 个码 → 作为「物料」菜单下的按钮节点**预留**，
-- 表落地后接口挂上即可用，不需要再补迁移。
--
-- # 码表（32 个：24 卡面 + 8 只读）
--
--   5 码制（5 张主力表 × list/detail/create/update/delete）：
--     business:erp:{warehouse,location,material}:{list,detail,create,update,delete}
--   3 码制（卡面只给 3 个码 → list 含详情、update 含删除）：
--     business:erp:{unit,supplier}:{list,create,update}
--   预留（分类表未建）：
--     business:erp:category:{list,detail,create}
--   只读（13 张真实表里剩下的 8 张事务/账务表，列表与详情共用 :list）：
--     business:erp:{account,period,seq,purchase-order,purchase-order-item,
--                   inventory,inventory-location,inventory-txn}:list
--
-- # 授权
--
-- 超管（`SUPER_ADMIN`）+ 老大（`laoliu`，按用户名或角色名解析）拿全部 34 个菜单/按钮。
-- 其它角色一个不授：ERP 是内部主数据，谁能改由运维在「角色 → 分配菜单」里勾。
--
-- # ID 段
--
-- 0940–0978（09xx 段此前已被任务看板/模板占掉 0900–0906，ERP 顺延到 0940+）。
-- 目录 0940 / 0946，主数据菜单 0941–0945，台账菜单 0950–0957，按钮 0960–0978。
--
-- # 版本号
--
-- 卡面要求「V0141 起用，若冲突则顺延」：本卡落地时 V0138–V0140 尚未被任何迁移占用（最高 = V0137），
-- 仍按卡面用 V0141，把 V0138–V0140 留给 W2/W3。若它们晚于本迁移落地，启动日志会出现一条
-- 「待应用迁移的版本低于已应用最大值（乱序补迁）」WARN —— 不阻断，应用成功后重启即「应用 0 个迁移」
-- （批次 C · C8 已记录同一现象）。
--
-- # 幂等
--
-- 固定 UUID + `ON CONFLICT (id) DO NOTHING`（重复执行不覆盖运维改过的标题）；
-- `sys_role_menu` 用 `NOT EXISTS` 去重。
-- ============================================================

-- ---------- ① 两个目录 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000940'::uuid, 'ERP 主数据', 1, '', '/business/erp', NULL, NULL,
     60, 1, 'database', 'ERP 主数据', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000946'::uuid, 'ERP 台账', 1, '', '/business/erp/ledger', NULL, NULL,
     70, 1, 'bar-chart', 'ERP 台账', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 5 个主数据菜单（menu_type 2，auth_code = :list） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000941'::uuid, '仓库管理', 2, 'business:erp:warehouse:list',
     '/business/erp/warehouse', '20000000-0000-0000-0000-000000000940'::uuid,
     'business/erp/warehouse/index', 10, 1, 'shop', '仓库管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000942'::uuid, '库位管理', 2, 'business:erp:location:list',
     '/business/erp/location', '20000000-0000-0000-0000-000000000940'::uuid,
     'business/erp/location/index', 20, 1, 'scan', '库位管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000943'::uuid, '物料管理', 2, 'business:erp:material:list',
     '/business/erp/material', '20000000-0000-0000-0000-000000000940'::uuid,
     'business/erp/material/index', 30, 1, 'box', '物料管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000944'::uuid, '计量单位', 2, 'business:erp:unit:list',
     '/business/erp/unit', '20000000-0000-0000-0000-000000000940'::uuid,
     'business/erp/unit/index', 40, 1, 'swap', '计量单位', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000945'::uuid, '供应商', 2, 'business:erp:supplier:list',
     '/business/erp/supplier', '20000000-0000-0000-0000-000000000940'::uuid,
     'business/erp/supplier/index', 50, 1, 'team', '供应商', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 8 个只读台账菜单（menu_type 2） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000950'::uuid, '会计科目', 2, 'business:erp:account:list',
     '/business/erp/account', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/account/index', 10, 1, 'book', '会计科目', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000951'::uuid, '会计期间', 2, 'business:erp:period:list',
     '/business/erp/period', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/period/index', 20, 1, 'calendar', '会计期间', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000952'::uuid, '单号序列', 2, 'business:erp:seq:list',
     '/business/erp/seq', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/seq/index', 30, 1, 'field-number', '单号序列', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000953'::uuid, '采购订单', 2, 'business:erp:purchase-order:list',
     '/business/erp/purchase-order', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/purchase-order/index', 40, 1, 'shopping-cart', '采购订单', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000954'::uuid, '采购明细', 2,
     'business:erp:purchase-order-item:list',
     '/business/erp/purchase-order-item', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/purchase-order-item/index', 50, 1, 'unordered-list', '采购明细', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000955'::uuid, '即时库存', 2, 'business:erp:inventory:list',
     '/business/erp/inventory', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/inventory/index', 60, 1, 'database', '即时库存', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000956'::uuid, '库位库存', 2,
     'business:erp:inventory-location:list',
     '/business/erp/inventory-location', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/inventory-location/index', 70, 1, 'environment', '库位库存', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000957'::uuid, '库存流水', 2,
     'business:erp:inventory-txn:list',
     '/business/erp/inventory-txn', '20000000-0000-0000-0000-000000000946'::uuid,
     'business/erp/inventory-txn/index', 80, 1, 'history', '库存流水', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ④ 按钮级权限（menu_type 3；name 与 auth_code 同值，与 V0045 一致） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 仓库（5 码制：list 已是菜单，其余 4 个按钮）
    ('20000000-0000-0000-0000-000000000960'::uuid, 'business:erp:warehouse:detail', 3,
     'business:erp:warehouse:detail', NULL, '20000000-0000-0000-0000-000000000941'::uuid, NULL,
     10, 1, NULL, '仓库详情', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000961'::uuid, 'business:erp:warehouse:create', 3,
     'business:erp:warehouse:create', NULL, '20000000-0000-0000-0000-000000000941'::uuid, NULL,
     20, 1, NULL, '新建仓库', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000962'::uuid, 'business:erp:warehouse:update', 3,
     'business:erp:warehouse:update', NULL, '20000000-0000-0000-0000-000000000941'::uuid, NULL,
     30, 1, NULL, '编辑仓库', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000963'::uuid, 'business:erp:warehouse:delete', 3,
     'business:erp:warehouse:delete', NULL, '20000000-0000-0000-0000-000000000941'::uuid, NULL,
     40, 1, NULL, '删除仓库', FALSE, 'business'),
    -- 库位
    ('20000000-0000-0000-0000-000000000964'::uuid, 'business:erp:location:detail', 3,
     'business:erp:location:detail', NULL, '20000000-0000-0000-0000-000000000942'::uuid, NULL,
     10, 1, NULL, '库位详情', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000965'::uuid, 'business:erp:location:create', 3,
     'business:erp:location:create', NULL, '20000000-0000-0000-0000-000000000942'::uuid, NULL,
     20, 1, NULL, '新建库位', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000966'::uuid, 'business:erp:location:update', 3,
     'business:erp:location:update', NULL, '20000000-0000-0000-0000-000000000942'::uuid, NULL,
     30, 1, NULL, '编辑库位', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000967'::uuid, 'business:erp:location:delete', 3,
     'business:erp:location:delete', NULL, '20000000-0000-0000-0000-000000000942'::uuid, NULL,
     40, 1, NULL, '删除库位', FALSE, 'business'),
    -- 物料
    ('20000000-0000-0000-0000-000000000968'::uuid, 'business:erp:material:detail', 3,
     'business:erp:material:detail', NULL, '20000000-0000-0000-0000-000000000943'::uuid, NULL,
     10, 1, NULL, '物料详情', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000969'::uuid, 'business:erp:material:create', 3,
     'business:erp:material:create', NULL, '20000000-0000-0000-0000-000000000943'::uuid, NULL,
     20, 1, NULL, '新建物料', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000970'::uuid, 'business:erp:material:update', 3,
     'business:erp:material:update', NULL, '20000000-0000-0000-0000-000000000943'::uuid, NULL,
     30, 1, NULL, '编辑物料', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000971'::uuid, 'business:erp:material:delete', 3,
     'business:erp:material:delete', NULL, '20000000-0000-0000-0000-000000000943'::uuid, NULL,
     40, 1, NULL, '删除物料', FALSE, 'business'),
    -- 物料分类（表未建，码位预留）
    ('20000000-0000-0000-0000-000000000972'::uuid, 'business:erp:category:list', 3,
     'business:erp:category:list', NULL, '20000000-0000-0000-0000-000000000943'::uuid, NULL,
     50, 1, NULL, '物料分类列表（预留）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000973'::uuid, 'business:erp:category:detail', 3,
     'business:erp:category:detail', NULL, '20000000-0000-0000-0000-000000000943'::uuid, NULL,
     60, 1, NULL, '物料分类详情（预留）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000974'::uuid, 'business:erp:category:create', 3,
     'business:erp:category:create', NULL, '20000000-0000-0000-0000-000000000943'::uuid, NULL,
     70, 1, NULL, '新建物料分类（预留）', FALSE, 'business'),
    -- 计量单位（3 码制：list 已是菜单，只剩 create / update）
    ('20000000-0000-0000-0000-000000000975'::uuid, 'business:erp:unit:create', 3,
     'business:erp:unit:create', NULL, '20000000-0000-0000-0000-000000000944'::uuid, NULL,
     10, 1, NULL, '新建计量单位', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000976'::uuid, 'business:erp:unit:update', 3,
     'business:erp:unit:update', NULL, '20000000-0000-0000-0000-000000000944'::uuid, NULL,
     20, 1, NULL, '编辑 / 删除计量单位', FALSE, 'business'),
    -- 供应商（3 码制）
    ('20000000-0000-0000-0000-000000000977'::uuid, 'business:erp:supplier:create', 3,
     'business:erp:supplier:create', NULL, '20000000-0000-0000-0000-000000000945'::uuid, NULL,
     10, 1, NULL, '新建供应商', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000978'::uuid, 'business:erp:supplier:update', 3,
     'business:erp:supplier:update', NULL, '20000000-0000-0000-0000-000000000945'::uuid, NULL,
     20, 1, NULL, '编辑 / 删除供应商', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ⑤ 授权：超管 + laoliu ----------
-- 超管按 role_code 短路本就全通，写 sys_role_menu 只是让「角色授权」树的勾选状态正确。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id IN (
       '20000000-0000-0000-0000-000000000940','20000000-0000-0000-0000-000000000946',
       '20000000-0000-0000-0000-000000000941','20000000-0000-0000-0000-000000000942',
       '20000000-0000-0000-0000-000000000943','20000000-0000-0000-0000-000000000944',
       '20000000-0000-0000-0000-000000000945',
       '20000000-0000-0000-0000-000000000950','20000000-0000-0000-0000-000000000951',
       '20000000-0000-0000-0000-000000000952','20000000-0000-0000-0000-000000000953',
       '20000000-0000-0000-0000-000000000954','20000000-0000-0000-0000-000000000955',
       '20000000-0000-0000-0000-000000000956','20000000-0000-0000-0000-000000000957',
       '20000000-0000-0000-0000-000000000960','20000000-0000-0000-0000-000000000961',
       '20000000-0000-0000-0000-000000000962','20000000-0000-0000-0000-000000000963',
       '20000000-0000-0000-0000-000000000964','20000000-0000-0000-0000-000000000965',
       '20000000-0000-0000-0000-000000000966','20000000-0000-0000-0000-000000000967',
       '20000000-0000-0000-0000-000000000968','20000000-0000-0000-0000-000000000969',
       '20000000-0000-0000-0000-000000000970','20000000-0000-0000-0000-000000000971',
       '20000000-0000-0000-0000-000000000972','20000000-0000-0000-0000-000000000973',
       '20000000-0000-0000-0000-000000000974','20000000-0000-0000-0000-000000000975',
       '20000000-0000-0000-0000-000000000976','20000000-0000-0000-0000-000000000977',
       '20000000-0000-0000-0000-000000000978'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- laoliu：老大账号可能以用户名 laoliu 或角色名/码含 laoliu 的形式存在（各库不完全一致），
-- 两种都授；库里没有该用户/角色时这段是空操作，不报错。
-- 注意：派生表的列名取自 UNION 的第一支（`role_id`），别名 `src` 后必须写 `src.role_id`
-- （第一版写 `r.id` 直接报 `column r.id does not exist`，实测踩过）。
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
       '20000000-0000-0000-0000-000000000940','20000000-0000-0000-0000-000000000946',
       '20000000-0000-0000-0000-000000000941','20000000-0000-0000-0000-000000000942',
       '20000000-0000-0000-0000-000000000943','20000000-0000-0000-0000-000000000944',
       '20000000-0000-0000-0000-000000000945',
       '20000000-0000-0000-0000-000000000950','20000000-0000-0000-0000-000000000951',
       '20000000-0000-0000-0000-000000000952','20000000-0000-0000-0000-000000000953',
       '20000000-0000-0000-0000-000000000954','20000000-0000-0000-0000-000000000955',
       '20000000-0000-0000-0000-000000000956','20000000-0000-0000-0000-000000000957',
       '20000000-0000-0000-0000-000000000960','20000000-0000-0000-0000-000000000961',
       '20000000-0000-0000-0000-000000000962','20000000-0000-0000-0000-000000000963',
       '20000000-0000-0000-0000-000000000964','20000000-0000-0000-0000-000000000965',
       '20000000-0000-0000-0000-000000000966','20000000-0000-0000-0000-000000000967',
       '20000000-0000-0000-0000-000000000968','20000000-0000-0000-0000-000000000969',
       '20000000-0000-0000-0000-000000000970','20000000-0000-0000-0000-000000000971',
       '20000000-0000-0000-0000-000000000972','20000000-0000-0000-0000-000000000973',
       '20000000-0000-0000-0000-000000000974','20000000-0000-0000-0000-000000000975',
       '20000000-0000-0000-0000-000000000976','20000000-0000-0000-0000-000000000977',
       '20000000-0000-0000-0000-000000000978'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = src.role_id AND rm.menu_id = m.id
   );

-- ---------- ⑥ 自检 ----------
DO $$
DECLARE
    n_codes    integer;
    n_dirs     integer;
    n_master   integer;
    n_ledger   integer;
    n_buttons  integer;
    n_granted  integer;
    n_roles    integer;
    v_missing  text;
BEGIN
    -- 32 个权限码必须全部落地（卡面 24 + 只读 8）
    SELECT count(DISTINCT auth_code) INTO n_codes
      FROM sys_menu
     WHERE auth_code LIKE 'business:erp:%' AND deleted_at IS NULL;
    IF n_codes <> 32 THEN
        SELECT string_agg(code, ', ') INTO v_missing
          FROM (VALUES
                ('business:erp:warehouse:list'),('business:erp:warehouse:detail'),
                ('business:erp:warehouse:create'),('business:erp:warehouse:update'),
                ('business:erp:warehouse:delete'),
                ('business:erp:location:list'),('business:erp:location:detail'),
                ('business:erp:location:create'),('business:erp:location:update'),
                ('business:erp:location:delete'),
                ('business:erp:material:list'),('business:erp:material:detail'),
                ('business:erp:material:create'),('business:erp:material:update'),
                ('business:erp:material:delete'),
                ('business:erp:category:list'),('business:erp:category:detail'),
                ('business:erp:category:create'),
                ('business:erp:unit:list'),('business:erp:unit:create'),('business:erp:unit:update'),
                ('business:erp:supplier:list'),('business:erp:supplier:create'),
                ('business:erp:supplier:update'),
                ('business:erp:account:list'),('business:erp:period:list'),
                ('business:erp:seq:list'),('business:erp:purchase-order:list'),
                ('business:erp:purchase-order-item:list'),('business:erp:inventory:list'),
                ('business:erp:inventory-location:list'),('business:erp:inventory-txn:list')
               ) AS t(code)
         WHERE NOT EXISTS (
             SELECT 1 FROM sys_menu m
              WHERE m.auth_code = t.code AND m.deleted_at IS NULL
         );
        RAISE EXCEPTION 'V0141 自检失败：ERP 权限码应有 32 个，实际 %；缺失：%', n_codes, coalesce(v_missing, '无');
    END IF;

    -- 2 个目录 / 5 个主数据菜单 / 8 个台账菜单 / 19 个按钮
    SELECT count(*) INTO n_dirs FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000940'::uuid,
                  '20000000-0000-0000-0000-000000000946'::uuid)
       AND menu_type = 1 AND deleted_at IS NULL;
    SELECT count(*) INTO n_master FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000940'::uuid
       AND menu_type = 2 AND deleted_at IS NULL;
    SELECT count(*) INTO n_ledger FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000946'::uuid
       AND menu_type = 2 AND deleted_at IS NULL;
    SELECT count(*) INTO n_buttons FROM sys_menu
     WHERE menu_type = 3 AND auth_code LIKE 'business:erp:%' AND deleted_at IS NULL;
    IF n_dirs <> 2 OR n_master <> 5 OR n_ledger <> 8 OR n_buttons <> 19 THEN
        RAISE EXCEPTION 'V0141 自检失败：目录 %/2、主数据菜单 %/5、台账菜单 %/8、按钮 %/19',
            n_dirs, n_master, n_ledger, n_buttons;
    END IF;

    -- 超管必须拿到全部 34 个节点。
    -- 例外：**空库首启**时迁移先于 bootstrap 跑，此刻还没有任何角色（V0132 也是这么处理的），
    -- 授权语句是空操作；超管本来就按 role_code 短路（`#[nik::permission]` 全通），
    -- 因此这种情况只 NOTICE 不报错，等 admin 建出来之后重跑本迁移即可补齐 laoliu 的授权。
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0141：空库首启（尚无 SUPER_ADMIN 角色），跳过授权核对；建出角色后重跑本迁移即可补齐';
    ELSE
    SELECT count(*) INTO n_granted
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
     WHERE r.role_code = 'SUPER_ADMIN'
       AND rm.menu_id IN (
           '20000000-0000-0000-0000-000000000940','20000000-0000-0000-0000-000000000946',
           '20000000-0000-0000-0000-000000000941','20000000-0000-0000-0000-000000000942',
           '20000000-0000-0000-0000-000000000943','20000000-0000-0000-0000-000000000944',
           '20000000-0000-0000-0000-000000000945',
           '20000000-0000-0000-0000-000000000950','20000000-0000-0000-0000-000000000951',
           '20000000-0000-0000-0000-000000000952','20000000-0000-0000-0000-000000000953',
           '20000000-0000-0000-0000-000000000954','20000000-0000-0000-0000-000000000955',
           '20000000-0000-0000-0000-000000000956','20000000-0000-0000-0000-000000000957',
           '20000000-0000-0000-0000-000000000960','20000000-0000-0000-0000-000000000961',
           '20000000-0000-0000-0000-000000000962','20000000-0000-0000-0000-000000000963',
           '20000000-0000-0000-0000-000000000964','20000000-0000-0000-0000-000000000965',
           '20000000-0000-0000-0000-000000000966','20000000-0000-0000-0000-000000000967',
           '20000000-0000-0000-0000-000000000968','20000000-0000-0000-0000-000000000969',
           '20000000-0000-0000-0000-000000000970','20000000-0000-0000-0000-000000000971',
           '20000000-0000-0000-0000-000000000972','20000000-0000-0000-0000-000000000973',
           '20000000-0000-0000-0000-000000000974','20000000-0000-0000-0000-000000000975',
           '20000000-0000-0000-0000-000000000976','20000000-0000-0000-0000-000000000977',
           '20000000-0000-0000-0000-000000000978'
       );
    IF n_granted <> 34 THEN
        RAISE EXCEPTION 'V0141 自检失败：超管应拿到 34 个 ERP 节点，实际 %', n_granted;
    END IF;
    END IF;

    RAISE NOTICE 'V0141 完成：% 个 ERP 权限码、2 目录 / 5 主数据菜单 / 8 台账菜单 / 19 按钮，超管已全量授权',
        n_codes;
END $$;
