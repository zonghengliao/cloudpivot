-- ======================================================
-- V0308：价格维护页（SO-34）—— 页面菜单 + 8 个权限码 + 销售角色授权
--
-- ## 为什么本卡要带迁移
--
-- ① `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
--    里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**（除超管外
--    任何人都调不动，而且不报错）。SO-34 新增 8 个接口 ⇒ 8 个码：
--
--      business:erp:cust-price:list/ create/update/delete   （客户协议价）
--      business:erp:price-tier:list/ create/update/delete   （阶梯/促销价）
--
--    与 `price_admin_handler.rs::PRICE_ADMIN_PERMISSIONS` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ② `system::menu::tests::n08` 是真库档硬门禁：`menu_type=2` 的 `path` 必须能在前端路由里
--    解析。本卡同时注册了路由 `business/erp/customer-price` ⇒ 本迁移可以**直接把 path 填上**
--    （先注册路由、再填菜单，顺序不能反）。V0307 那个隐藏的「价格查询」节点（…3071）
--    保持隐藏不动 —— 它承载的是取价只读码，与维护页是两个码。
--
-- ## 菜单树
--
--   销售管理（V0267 …2701，menu_type=1）
--     └── 价格管理（…3081，menu_type=2，auth_code=cust-price:list，
--         path=/business/erp/customer-price，component=business/erp/customer-price/index，sort 70）
--           ├── 协议价新建（…3082，menu_type=3）
--           ├── 协议价编辑（…3083）
--           ├── 协议价删除（…3084）
--           ├── 阶梯价查询（…3085）   ← 同一页面第二个 Tab，单独一个码
--           ├── 阶梯价新建（…3086）
--           ├── 阶梯价编辑（…3087）
--           └── 阶梯价删除（…3088）
--
-- ## 授权（**本迁移唯一替人做的决定，写在注释里**）
--
--   · SALES_SUPERVISOR（销售经理）：价格页 8 个码全给 —— 价格是销售经理的定价权；
--   · SALES_CLERK（销售员）：只给 `cust-price:list` + `price-tier:list`
--     （开单要看得见价，但不能改价；改价能力在页面上由 `:update/:create` 挡住）；
--   · 两个销售角色 + FINANCE_MANAGER 额外获得 V0307 的 `business:erp:price:resolve`
--     （**这是 SO-33 留下的缺口**：只读取价码此前没授给任何角色，销售员开单时
--     自动带价会 403；本迁移补平，只补"能取价"这一条，不给维护权）。
--   · WAREHOUSE_MANAGER 不给价格（仓管与定价无关）。
--
--   `ON CONFLICT (role_id, menu_id) DO NOTHING` 幂等；角色不存在时自然插入 0 行
--   （末尾自检会点出「应有授权却 0 行」的异常，不让它静默半套上线）。
--
-- ## 幂等与回滚
--
--   菜单按固定 uuid + `ON CONFLICT (id) DO UPDATE`（path/component 可反复补齐）；
--   授权按 `ON CONFLICT (role_id, menu_id) DO NOTHING`。
--   回滚：V0308__customer_price_menu.down.sql 先删授权、再删按钮、最后删页面节点。
-- ======================================================

-- ---------- ① 页面节点：价格管理 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003081'::uuid, '价格管理', 2,
     'business:erp:cust-price:list', '/business/erp/customer-price',
     '20000000-0000-0000-0000-000000002701'::uuid, 'business/erp/customer-price/index',
     70, 1, 'money', '客户协议价 + 阶梯/促销价维护（取价链见 ARCH-7 §15.3.3）',
     TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

-- ---------- ② 7 个按钮（menu_type=3，父 = 价格管理页） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003082'::uuid, '协议价新建', 3,
     'business:erp:cust-price:create', NULL, '20000000-0000-0000-0000-000000003081'::uuid,
     NULL, 10, 1, NULL, '新建客户协议价', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003083'::uuid, '协议价编辑', 3,
     'business:erp:cust-price:update', NULL, '20000000-0000-0000-0000-000000003081'::uuid,
     NULL, 20, 1, NULL, '编辑客户协议价', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003084'::uuid, '协议价删除', 3,
     'business:erp:cust-price:delete', NULL, '20000000-0000-0000-0000-000000003081'::uuid,
     NULL, 30, 1, NULL, '软删客户协议价（价格历史保留在库）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003085'::uuid, '阶梯价查询', 3,
     'business:erp:price-tier:list', NULL, '20000000-0000-0000-0000-000000003081'::uuid,
     NULL, 40, 1, NULL, '阶梯价 / 促销价列表（价格页第二个 Tab）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003086'::uuid, '阶梯价新建', 3,
     'business:erp:price-tier:create', NULL, '20000000-0000-0000-0000-000000003081'::uuid,
     NULL, 50, 1, NULL, '新建阶梯 / 促销档位', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003087'::uuid, '阶梯价编辑', 3,
     'business:erp:price-tier:update', NULL, '20000000-0000-0000-0000-000000003081'::uuid,
     NULL, 60, 1, NULL, '编辑阶梯 / 促销档位', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003088'::uuid, '阶梯价删除', 3,
     'business:erp:price-tier:delete', NULL, '20000000-0000-0000-0000-000000003081'::uuid,
     NULL, 70, 1, NULL, '软删阶梯 / 促销档位', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name       = EXCLUDED.name,
       menu_type  = EXCLUDED.menu_type,
       auth_code  = EXCLUDED.auth_code,
       parent_id  = EXCLUDED.parent_id,
       sort_order = EXCLUDED.sort_order,
       status     = EXCLUDED.status,
       title      = EXCLUDED.title,
       is_visible = EXCLUDED.is_visible,
       deleted_at = NULL,
       updated_at = now();

-- ---------- ③ 授权矩阵（见文件头「授权」） ----------
CREATE TEMP TABLE _cp_matrix (role_code text NOT NULL, menu_id uuid NOT NULL, PRIMARY KEY (role_code, menu_id));

INSERT INTO _cp_matrix (role_code, menu_id) VALUES
-- SALES_SUPERVISOR：价格页全功能（定价权）
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003081'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003082'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003083'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003084'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003085'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003086'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003087'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003088'::uuid),
-- SALES_CLERK：只读（开单要看得见价，改价权在经理）
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003081'::uuid),
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003085'::uuid),
-- 取价只读码（V0307 …3071）：销售员 / 销售经理 / 财务经理 —— 开单自动带价的前提
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003071'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003071'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003071'::uuid);

INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM _cp_matrix m
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

DROP TABLE _cp_matrix;

-- ---------- ④ 自检：8 个码各 1 行、页面可见且 path 已填、按钮父级正确、关键授权在位 ----------
DO $$
DECLARE
    v_missing   text;
    v_dupe      text;
    v_rows      integer;
    v_path      text;
    v_component text;
    v_visible   boolean;
    v_parent    uuid;
    v_grants    integer;
BEGIN
    -- 4.1 8 个权限码全部有承载行（缺一个 = 该码授不出去）
    SELECT string_agg(code, ', ') INTO v_missing
      FROM (VALUES
              ('business:erp:cust-price:list'),
              ('business:erp:cust-price:create'),
              ('business:erp:cust-price:update'),
              ('business:erp:cust-price:delete'),
              ('business:erp:price-tier:list'),
              ('business:erp:price-tier:create'),
              ('business:erp:price-tier:update'),
              ('business:erp:price-tier:delete')
           ) AS t(code)
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu sm
               WHERE sm.auth_code = t.code AND sm.deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0308 自检失败：以下权限码在 sys_menu 无承载行 → %', v_missing;
    END IF;

    -- 4.2 每个码只能有 1 条承载行（两条 = 授权面出现两个同名可勾项）
    SELECT string_agg(auth_code, ', ') INTO v_dupe
      FROM (SELECT auth_code FROM sys_menu
             WHERE deleted_at IS NULL
               AND (auth_code LIKE 'business:erp:cust-price:%'
                    OR auth_code LIKE 'business:erp:price-tier:%')
             GROUP BY auth_code HAVING count(*) <> 1) d;
    IF v_dupe IS NOT NULL THEN
        RAISE EXCEPTION 'V0308 自检失败：以下权限码承载行不是 1 条 → %', v_dupe;
    END IF;

    -- 4.3 页面节点存在、可见、path/component 与前端路由一致
    SELECT count(*), min(path), min(component), bool_and(is_visible)
      INTO v_rows, v_path, v_component, v_visible
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000003081'::uuid AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0308 自检失败：价格管理页面节点缺失（% 行）', v_rows;
    END IF;
    IF v_path IS DISTINCT FROM '/business/erp/customer-price' THEN
        RAISE EXCEPTION 'V0308 自检失败：页面 path 应为 /business/erp/customer-price，实际 %', v_path;
    END IF;
    IF v_component IS DISTINCT FROM 'business/erp/customer-price/index' THEN
        RAISE EXCEPTION 'V0308 自检失败：component 应为 business/erp/customer-price/index，实际 %', v_component;
    END IF;
    IF v_visible IS NOT TRUE THEN
        RAISE EXCEPTION 'V0308 自检失败：价格管理页面必须可见';
    END IF;

    -- 4.4 7 个按钮的父级必须是这个页面节点（否则是孤儿按钮）
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000003081'::uuid
       AND deleted_at IS NULL AND menu_type = 3;
    IF v_rows <> 7 THEN
        RAISE EXCEPTION 'V0308 自检失败：价格管理下应有 7 个按钮，实际 %', v_rows;
    END IF;
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE id BETWEEN '20000000-0000-0000-0000-000000003082'::uuid
                  AND '20000000-0000-0000-0000-000000003088'::uuid
       AND deleted_at IS NULL
       AND (parent_id IS DISTINCT FROM '20000000-0000-0000-0000-000000003081'::uuid OR menu_type <> 3);
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'V0308 自检失败：有 % 个按钮父级/类型不对（应为 3081 下的 menu_type=3）', v_rows;
    END IF;

    -- 4.5 关键授权在位：SALES_SUPERVISOR 8 个码全有、SALES_CLERK 有只读页 + 取价码
    WITH expected(role_code, menu_id) AS (VALUES
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003081'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003082'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003083'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003084'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003085'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003086'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003087'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003088'::uuid),
        ('SALES_CLERK', '20000000-0000-0000-0000-000000003081'::uuid),
        ('SALES_CLERK', '20000000-0000-0000-0000-000000003085'::uuid),
        ('SALES_CLERK', '20000000-0000-0000-0000-000000003071'::uuid),
        ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003071'::uuid),
        ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003071'::uuid)
    )
    SELECT count(*) INTO v_grants
      FROM expected e
      JOIN sys_role r ON r.role_code = e.role_code AND r.deleted_at IS NULL
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_role_menu rm
               WHERE rm.role_id = r.id AND rm.menu_id = e.menu_id);
    IF v_grants <> 0 THEN
        RAISE EXCEPTION 'V0308 自检失败：应有 % 条授权缺失（角色存在但未授上）', v_grants;
    END IF;
END $$;
