-- ============================================================================
-- V0267 down：撤销「销售管理」目录 + 六个页面归位 + 本迁移授出的 47 对授权
--
-- 语义（与 up 逐条镜像，且**只撤本迁移的东西**）：
--   ① 授权行：按与 up 逐字相同的确定性 id 公式（md5(role|menu|'v0267-sales-menu')）精确删除
--      —— 历史上由别的迁移 / 数据修复授过的同码行 id 不同，**一行不碰**；
--   ② 六个页面：parent_id 还原 NULL，sort_order 还原 up 之前的 76/77/78/79/80/85
--      （实测值取自施工前快照，见 up 头注释第一节）；
--   ③ 目录节点 …2701 删除（先断言它下面已经没有活着的子节点，否则报警不删 —— 说明有人
--      后来把别的菜单挂进来了，此时强行删会把孤儿菜单留在库里）；
--   ④ 角色 SALES_CLERK：只在「无用户绑定」时删；有绑定 → NOTICE 保留（与 V0281 down 同口径：
--      回滚不该动在岗人员的组织关系）。
--
-- 幂等：重放删 0 行；重复执行时 ② 的 `IS DISTINCT FROM` 命中 0 行，③ 不存在则跳过。
-- ============================================================================

DROP TABLE IF EXISTS _sm_matrix;
CREATE TEMP TABLE _sm_matrix (
    role_code text NOT NULL,
    auth_code text NOT NULL,
    PRIMARY KEY (role_code, auth_code)
);

INSERT INTO _sm_matrix (role_code, auth_code) VALUES
    ('SALES_SUPERVISOR', 'business:erp:customer:list'),
    ('SALES_SUPERVISOR', 'business:erp:customer:create'),
    ('SALES_SUPERVISOR', 'business:erp:customer:update'),
    ('SALES_SUPERVISOR', 'business:erp:customer:delete'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:list'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:create'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:update'),
    ('SALES_SUPERVISOR', 'business:erp:sale-order:status'),
    ('SALES_SUPERVISOR', 'business:erp:shipment:list'),
    ('SALES_SUPERVISOR', 'business:erp:receivable:list'),
    ('SALES_SUPERVISOR', 'business:erp:receivable:detail'),
    ('SALES_SUPERVISOR', 'business:erp:receivable:aging'),
    ('SALES_CLERK', 'business:erp:customer:list'),
    ('SALES_CLERK', 'business:erp:sale-order:list'),
    ('SALES_CLERK', 'business:erp:sale-order:create'),
    ('SALES_CLERK', 'business:erp:sale-order:status'),
    ('WAREHOUSE_MANAGER', 'business:erp:sale-order:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:shipment:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:shipment:create'),
    ('WAREHOUSE_MANAGER', 'business:erp:shipment:status'),
    ('FINANCE_MANAGER', 'business:erp:customer:list'),
    ('FINANCE_MANAGER', 'business:erp:sale-order:list'),
    ('FINANCE_MANAGER', 'business:erp:shipment:list'),
    ('FINANCE_MANAGER', 'business:erp:receivable:list'),
    ('FINANCE_MANAGER', 'business:erp:receivable:detail'),
    ('FINANCE_MANAGER', 'business:erp:receivable:aging'),
    ('FINANCE_MANAGER', 'business:erp:receivable:create'),
    ('FINANCE_MANAGER', 'business:erp:receivable:write-off'),
    ('FINANCE_MANAGER', 'business:erp:receipt:list'),
    ('FINANCE_MANAGER', 'business:erp:receipt:detail'),
    ('FINANCE_MANAGER', 'business:erp:receipt:create'),
    ('FINANCE_MANAGER', 'business:erp:receipt:allocate'),
    ('FINANCE_MANAGER', 'business:erp:receipt:status'),
    ('FINANCE_MANAGER', 'business:erp:invoice:list'),
    ('FINANCE_MANAGER', 'business:erp:invoice:detail'),
    ('FINANCE_MANAGER', 'business:erp:invoice:create'),
    ('FINANCE_MANAGER', 'business:erp:invoice:status'),
    ('GENERAL_MANAGER', 'business:erp:customer:list'),
    ('GENERAL_MANAGER', 'business:erp:sale-order:list'),
    ('GENERAL_MANAGER', 'business:erp:shipment:list'),
    ('GENERAL_MANAGER', 'business:erp:receivable:list'),
    ('GENERAL_MANAGER', 'business:erp:receivable:detail'),
    ('GENERAL_MANAGER', 'business:erp:receivable:aging'),
    ('GENERAL_MANAGER', 'business:erp:receipt:list'),
    ('GENERAL_MANAGER', 'business:erp:receipt:detail'),
    ('GENERAL_MANAGER', 'business:erp:invoice:list'),
    ('GENERAL_MANAGER', 'business:erp:invoice:detail');

DO $$
DECLARE
    v_dir    uuid := '20000000-0000-0000-0000-000000002701'::uuid;
    v_pages  uuid[] := ARRAY['20000000-0000-0000-0000-000000002601'::uuid,
                             '20000000-0000-0000-0000-000000002651'::uuid,
                             '20000000-0000-0000-0000-000000002841'::uuid,
                             '20000000-0000-0000-0000-000000002880'::uuid,
                             '20000000-0000-0000-0000-000000002890'::uuid,
                             '20000000-0000-0000-0000-000000002895'::uuid];
    v_rest   int[]  := ARRAY[76, 77, 78, 79, 80, 85];
    v_n_del  integer := 0;
    v_n_rest integer := 0;
    v_n_child integer;
    v_role_id uuid;
    v_in_use integer;
    i        integer;
BEGIN
    -- ---------- ① 删本迁移插入的授权行（确定性 id 精确匹配） ----------
    WITH RECURSIVE hit AS (
        SELECT mx.role_code, r.id AS role_id, m.id AS menu_id
          FROM _sm_matrix mx
          JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
          JOIN sys_menu m ON m.auth_code = mx.auth_code AND m.deleted_at IS NULL
    ),
    anc AS (
        SELECT h.role_code, h.role_id, p.id AS menu_id
          FROM hit h
          JOIN sys_menu c ON c.id = h.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
         WHERE coalesce(p.auth_code, '') = ''
        UNION
        SELECT a.role_code, a.role_id, p.id AS menu_id
          FROM anc a
          JOIN sys_menu c ON c.id = a.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
         WHERE coalesce(p.auth_code, '') = ''
    ),
    sel AS (SELECT DISTINCT role_id, menu_id FROM (SELECT * FROM hit UNION ALL SELECT * FROM anc) u)
    DELETE FROM sys_role_menu rm
     WHERE rm.id IN (
             SELECT (
                      SELECT (substr(h, 1, 8) || '-' || substr(h, 9, 4) || '-4' || substr(h, 14, 3)
                              || '-8' || substr(h, 18, 3) || '-' || substr(h, 21, 12))::uuid
                        FROM (SELECT md5(s.role_id::text || '|' || s.menu_id::text || '|v0267-sales-menu') AS h) _h
                    )
               FROM sel s
           );
    GET DIAGNOSTICS v_n_del = ROW_COUNT;

    -- ---------- ② 六个页面还原：父级 NULL + 原 sort_order ----------
    FOR i IN 1..array_length(v_pages, 1) LOOP
        UPDATE sys_menu
           SET parent_id = NULL, sort_order = v_rest[i], updated_at = now()
         WHERE id = v_pages[i]
           AND (parent_id IS NOT NULL OR sort_order IS DISTINCT FROM v_rest[i]);
        GET DIAGNOSTICS v_n_child = ROW_COUNT;
        v_n_rest := v_n_rest + v_n_child;
    END LOOP;

    -- ---------- ③ 目录节点：确认无子节点才删 ----------
    SELECT count(*) INTO v_n_child
      FROM sys_menu WHERE parent_id = v_dir AND deleted_at IS NULL;
    IF v_n_child = 0 THEN
        DELETE FROM sys_menu WHERE id = v_dir;
        IF FOUND THEN
            RAISE NOTICE 'V0267 down：已删除「销售管理」目录节点';
        END IF;
    ELSE
        RAISE WARNING 'V0267 down：目录 …2701 下仍有 % 个活子节点，保留目录不删（请人工确认）', v_n_child;
    END IF;

    -- ---------- ④ 角色 SALES_CLERK：无绑定才删 ----------
    SELECT id INTO v_role_id FROM sys_role
     WHERE role_code = 'SALES_CLERK' AND deleted_at IS NULL
       AND id = '20000000-0000-4000-8000-0000000f0008'::uuid;
    IF v_role_id IS NULL THEN
        RAISE NOTICE 'V0267 down：角色 SALES_CLERK 不存在（或 UUID 不是本迁移种的），跳过';
    ELSE
        SELECT count(*) INTO v_in_use FROM sys_user_role WHERE role_id = v_role_id;
        IF v_in_use > 0 THEN
            RAISE NOTICE 'V0267 down：SALES_CLERK 仍有 % 个用户绑定，保留不删', v_in_use;
        ELSE
            DELETE FROM sys_role_menu WHERE role_id = v_role_id;
            DELETE FROM sys_role WHERE id = v_role_id;
            RAISE NOTICE 'V0267 down：已删除角色 SALES_CLERK（无用户绑定）';
        END IF;
    END IF;

    RAISE NOTICE 'V0267 down 完成：删除授权 % 行，还原页面 % 行', v_n_del, v_n_rest;
END $$;

DROP TABLE IF EXISTS _sm_matrix;
