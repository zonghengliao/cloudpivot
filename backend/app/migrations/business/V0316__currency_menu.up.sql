-- ======================================================
-- V0316：币种 / 汇率维护页（SO-42）—— 两个页面节点 + 10 个权限码 + 销售/财务角色授权
--
-- ## 为什么本卡要带迁移
--
-- ① `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
--    里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**（除超管外
--    任何人都调不动，而且不报错）。SO-42 新增 10 个接口 ⇒ 10 个码：
--
--      business:erp:currency:list / create / update / base       （币种维护）
--      business:erp:fx-rate:list / create / update / delete /
--      batch / quote                                             （汇率维护）
--
--    与 `currency_admin_handler.rs::CURRENCY_ADMIN_PERMISSIONS` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ② `system::menu::tests::n08` 是真库档硬门禁：`menu_type=2` 的 `path` 必须能在前端路由里
--    解析。本卡同时注册了 `business/erp/currency` 与 `business/erp/exchange-rate` 两条路由
--    ⇒ 本迁移可以**直接把 path 填上**（先注册路由、再填菜单，顺序不能反）。
--
-- ## 菜单树（挂在「销售管理」…2701 下）
--
--   销售管理（V0267 …2701，menu_type=1）
--     ├── 价格管理（V0308 …3081，sort 70，不动）
--     ├── 币种管理（…3161，menu_type=2，auth_code=currency:list，
--     │   path=/business/erp/currency，component=business/erp/currency/index，sort 80）
--     │     ├── 币种新建（…3162，menu_type=3）
--     │     ├── 币种编辑 / 启停（…3163）
--     │     └── 立为本位币（…3164）
--     └── 汇率管理（…3168，menu_type=2，auth_code=fx-rate:list，
--         path=/business/erp/exchange-rate，component=business/erp/exchange-rate/index，sort 90）
--           ├── 汇率新建（…3169）
--           ├── 汇率编辑（…3170）
--           ├── 汇率删除（…3171）
--           ├── 按月批量录入（…3172）
--           └── 汇率预览（…3173）  ← 开单选币种时查，销售员也要有
--
-- ## 授权（**本迁移唯一替人做的决定，写在注释里**）
--
--   · SALES_SUPERVISOR（销售经理）：10 个码全给 —— 外币单的币种口径由销售经理把关；
--   · FINANCE_MANAGER（财务经理）：10 个码全给 —— 汇率是财务的牌价维护权；
--   · SALES_CLERK（销售员）：只给 `currency:list` + `fx-rate:list` + `fx-rate:quote`
--     （开单要能选币种、看得见锁定汇率；维护权在经理 / 财务）；
--   · WAREHOUSE_MANAGER（仓管）：不给 —— 仓储与币种无关（库存账不分币种，SO-41 已钉住）。
--
--   `ON CONFLICT (role_id, menu_id) DO NOTHING` 幂等；角色不存在时自然插入 0 行
--   （末尾自检会点出「应有授权却 0 行」的异常，不让它静默半套上线）。
--
-- ## 幂等与回滚
--
--   菜单按固定 uuid + `ON CONFLICT (id) DO UPDATE`（path/component 可反复补齐）；
--   授权按 `ON CONFLICT (role_id, menu_id) DO NOTHING`。
--   回滚：V0316__currency_menu.down.sql 先删授权、再删按钮、最后删页面节点。
-- ======================================================

-- ---------- ① 页面节点：币种管理 / 汇率管理 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003161'::uuid, '币种管理', 2,
     'business:erp:currency:list', '/business/erp/currency',
     '20000000-0000-0000-0000-000000002701'::uuid, 'business/erp/currency/index',
     80, 1, 'money', '币种主数据 + 本位币标记（唯一；ARCH-7 §15.6.3 ①）',
     TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003168'::uuid, '汇率管理', 2,
     'business:erp:fx-rate:list', '/business/erp/exchange-rate',
     '20000000-0000-0000-0000-000000002701'::uuid, 'business/erp/exchange-rate/index',
     90, 1, 'currency-exchange', '即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）',
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

-- ---------- ② 8 个按钮（menu_type=3） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003162'::uuid, '币种新建', 3,
     'business:erp:currency:create', NULL, '20000000-0000-0000-0000-000000003161'::uuid,
     NULL, 10, 1, NULL, '新增币种（三位 ISO 4217 大写码）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003163'::uuid, '币种编辑', 3,
     'business:erp:currency:update', NULL, '20000000-0000-0000-0000-000000003161'::uuid,
     NULL, 20, 1, NULL, '编辑名称 / 符号 / 小数位 / 备注，或启用·停用（本位币不能停用）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003164'::uuid, '立为本位币', 3,
     'business:erp:currency:base', NULL, '20000000-0000-0000-0000-000000003161'::uuid,
     NULL, 30, 1, NULL, '把某币种立为账套本位币（全库只能一个，库层部分唯一索引兜底）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003169'::uuid, '汇率新建', 3,
     'business:erp:fx-rate:create', NULL, '20000000-0000-0000-0000-000000003168'::uuid,
     NULL, 10, 1, NULL, '新增一条汇率（外币 → 本位币，值必须 > 0）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003170'::uuid, '汇率编辑', 3,
     'business:erp:fx-rate:update', NULL, '20000000-0000-0000-0000-000000003168'::uuid,
     NULL, 20, 1, NULL, '编辑汇率值 / 类型 / 生效日 / 备注', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003171'::uuid, '汇率删除', 3,
     'business:erp:fx-rate:delete', NULL, '20000000-0000-0000-0000-000000003168'::uuid,
     NULL, 30, 1, NULL, '物理删除录错的汇率行（该表无软删列）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003172'::uuid, '按月批量录入', 3,
     'business:erp:fx-rate:batch', NULL, '20000000-0000-0000-0000-000000003168'::uuid,
     NULL, 40, 1, NULL, '选月份 + 多币种一次性 upsert 到月末日', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003173'::uuid, '汇率预览', 3,
     'business:erp:fx-rate:quote', NULL, '20000000-0000-0000-0000-000000003168'::uuid,
     NULL, 50, 1, NULL, '按币种 + 日期查最近一条 SPOT（开单页选币种时用，只读）', TRUE, 'business')
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

-- ---------- ③ 角色授权矩阵 ----------
CREATE TEMP TABLE _cur_matrix (role_code text NOT NULL, menu_id uuid NOT NULL, PRIMARY KEY (role_code, menu_id));

INSERT INTO _cur_matrix (role_code, menu_id) VALUES
-- SALES_SUPERVISOR：全功能（外币单的币种口径由销售经理把关）
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003161'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003162'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003163'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003164'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003168'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003169'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003170'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003171'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003172'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003173'::uuid),
-- FINANCE_MANAGER：全功能（汇率是财务的牌价维护权）
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003161'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003162'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003163'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003164'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003168'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003169'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003170'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003171'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003172'::uuid),
    ('FINANCE_MANAGER', '20000000-0000-0000-0000-000000003173'::uuid),
-- SALES_CLERK：只看 + 预览（开单要能选币种、看得见锁定汇率，不能改牌价）
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003161'::uuid),
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003168'::uuid),
    ('SALES_CLERK', '20000000-0000-0000-0000-000000003173'::uuid);

INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM _cur_matrix m
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

DROP TABLE _cur_matrix;

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    v_missing   text;
    v_dupe      text;
    v_rows      integer;
    v_path      text;
    v_component text;
    v_visible   boolean;
    v_parent    uuid;
BEGIN
    -- ① 10 个码必须各有且仅有 1 行承载
    SELECT string_agg(code, ', ') INTO v_missing
      FROM (VALUES
            ('business:erp:currency:list'), ('business:erp:currency:create'),
            ('business:erp:currency:update'), ('business:erp:currency:base'),
            ('business:erp:fx-rate:list'), ('business:erp:fx-rate:create'),
            ('business:erp:fx-rate:update'), ('business:erp:fx-rate:delete'),
            ('business:erp:fx-rate:batch'), ('business:erp:fx-rate:quote')
           ) AS expected(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = expected.code AND m.deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'SO-42: 以下权限码在 sys_menu 里没有承载行：%', v_missing;
    END IF;

    SELECT string_agg(code, ', ') INTO v_dupe
      FROM (SELECT auth_code AS code, count(*) AS n FROM sys_menu
             WHERE deleted_at IS NULL
               AND auth_code IN ('business:erp:currency:list', 'business:erp:currency:create',
                                 'business:erp:currency:update', 'business:erp:currency:base',
                                 'business:erp:fx-rate:list', 'business:erp:fx-rate:create',
                                 'business:erp:fx-rate:update', 'business:erp:fx-rate:delete',
                                 'business:erp:fx-rate:batch', 'business:erp:fx-rate:quote')
             GROUP BY auth_code HAVING count(*) > 1) AS dup;
    IF v_dupe IS NOT NULL THEN
        RAISE EXCEPTION 'SO-42: 以下权限码在 sys_menu 里有多行承载（会造成授权歧义）：%', v_dupe;
    END IF;

    -- ② 两个页面节点：path / component / 可见 / 父级 都在位
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000003161'::uuid,
                  '20000000-0000-0000-0000-000000003168'::uuid)
       AND menu_type = 2 AND deleted_at IS NULL;
    IF v_rows <> 2 THEN
        RAISE EXCEPTION 'SO-42: 页面节点数 = %（期望 2：币种管理 + 汇率管理）', v_rows;
    END IF;

    SELECT path, component, is_visible, parent_id
      INTO v_path, v_component, v_visible, v_parent
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000003161'::uuid;
    IF v_path <> '/business/erp/currency'
       OR v_component <> 'business/erp/currency/index'
       OR v_visible IS NOT TRUE
       OR v_parent <> '20000000-0000-0000-0000-000000002701'::uuid THEN
        RAISE EXCEPTION 'SO-42: 币种管理节点字段不对（path=%, component=%, visible=%, parent=%）',
            v_path, v_component, v_visible, v_parent;
    END IF;

    SELECT path, component, is_visible, parent_id
      INTO v_path, v_component, v_visible, v_parent
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000003168'::uuid;
    IF v_path <> '/business/erp/exchange-rate'
       OR v_component <> 'business/erp/exchange-rate/index'
       OR v_visible IS NOT TRUE
       OR v_parent <> '20000000-0000-0000-0000-000000002701'::uuid THEN
        RAISE EXCEPTION 'SO-42: 汇率管理节点字段不对（path=%, component=%, visible=%, parent=%）',
            v_path, v_component, v_visible, v_parent;
    END IF;

    -- ③ 8 个按钮的父级必须正确（按钮挂错父级 = 授权矩阵表达错层级）
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000003162'::uuid,
                  '20000000-0000-0000-0000-000000003163'::uuid,
                  '20000000-0000-0000-0000-000000003164'::uuid)
       AND parent_id = '20000000-0000-0000-0000-000000003161'::uuid AND menu_type = 3;
    IF v_rows <> 3 THEN
        RAISE EXCEPTION 'SO-42: 币种按钮数 = %（期望 3）', v_rows;
    END IF;

    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000003169'::uuid,
                  '20000000-0000-0000-0000-000000003170'::uuid,
                  '20000000-0000-0000-0000-000000003171'::uuid,
                  '20000000-0000-0000-0000-000000003172'::uuid,
                  '20000000-0000-0000-0000-000000003173'::uuid)
       AND parent_id = '20000000-0000-0000-0000-000000003168'::uuid AND menu_type = 3;
    IF v_rows <> 5 THEN
        RAISE EXCEPTION 'SO-42: 汇率按钮数 = %（期望 5）', v_rows;
    END IF;

    -- ④ 关键授权在位（角色存在时；角色不存在时该角色 0 授权属正常）
    SELECT count(*) INTO v_rows
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE r.role_code IN ('SALES_SUPERVISOR', 'FINANCE_MANAGER')
       AND rm.menu_id IN ('20000000-0000-0000-0000-000000003161'::uuid,
                          '20000000-0000-0000-0000-000000003168'::uuid);
    IF v_rows = 0 AND EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'FINANCE_MANAGER' AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'SO-42: FINANCE_MANAGER / SALES_SUPERVISOR 一个页面权限都没拿到';
    END IF;

    RAISE NOTICE 'SO-42: 币种 / 汇率菜单与 10 个权限码自检通过';
END $$;
