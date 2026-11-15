-- ============================================================================
-- V0356 down：撤销本迁移授出的「业务单据核心权限码」授权行（**只撤自己插的**）
--
-- 语义（与 up 逐条镜像）：
--   ① 按与 up **逐字相同**的确定性 id 公式
--      （`md5(role_id|menu_id|'v0356-doc-perm-grants')`）精确删除；
--      历史上由别的迁移 / 数据修复授过的同码行 id 不同，**一行不碰**；
--   ② 不删菜单、不删角色、不改 path/可见性 —— 授权面的事只回滚授权面
--      （菜单承载是 V0304 / V0295 / V0141 的成果，回滚它们不在本迁移职责内）；
--   ③ 末尾 NOTICE 报告删了几行，重放 = 0 行（幂等）。
-- ============================================================================

DROP TABLE IF EXISTS _dpg_matrix;
CREATE TEMP TABLE _dpg_matrix (
    role_code text NOT NULL,
    auth_code text NOT NULL,
    PRIMARY KEY (role_code, auth_code)
);

INSERT INTO _dpg_matrix (role_code, auth_code) VALUES
    ('SALES_CLERK',      'business:erp:sales-return:list'),
    ('SALES_CLERK',      'business:erp:sales-return:create'),
    ('SALES_SUPERVISOR', 'business:erp:sales-return:list'),
    ('SALES_SUPERVISOR', 'business:erp:sales-return:status'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:list'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:detail'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:create'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:allocate'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:status'),
    ('PURCHASE_MANAGER', 'business:erp:unit:create');

DO $$
DECLARE
    v_n_del integer := 0;
BEGIN
    WITH RECURSIVE hit AS (
        SELECT mx.role_code, r.id AS role_id, m.id AS menu_id
          FROM _dpg_matrix mx
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
    sel AS (SELECT DISTINCT role_id, menu_id
              FROM (SELECT * FROM hit UNION ALL SELECT * FROM anc) u)
    DELETE FROM sys_role_menu rm
     WHERE rm.id IN (
             SELECT (
                      SELECT (substr(h, 1, 8) || '-' || substr(h, 9, 4) || '-4' || substr(h, 14, 3)
                              || '-8' || substr(h, 18, 3) || '-' || substr(h, 21, 12))::uuid
                        FROM (SELECT md5(s.role_id::text || '|' || s.menu_id::text
                                         || '|v0356-doc-perm-grants') AS h) _h
                    )
               FROM sel s
           );
    GET DIAGNOSTICS v_n_del = ROW_COUNT;

    RAISE NOTICE 'V0356 down：已撤销本迁移插入的 % 行授权（其它迁移授的同码行未动）', v_n_del;
END $$;

DROP TABLE IF EXISTS _dpg_matrix;
