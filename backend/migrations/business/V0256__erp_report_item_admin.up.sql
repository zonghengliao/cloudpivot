-- ============================================================
-- V0256：报表项维护页 + 权限码（卡 t_a100a01a / P1-FIX-8B）
--
-- 依据：`backend/src/business/erp/report_item_admin.rs`（HTTP 管理面）
--       `backend/src/business/erp/report_item_model.rs`（数据访问）
--       `frontend/src/views/business/erp/report/report-item/index.vue`（维护页）
--
-- # 这条迁移做什么（只有菜单与权限，**不动 biz_report_item 任何一行**）
--
--   ① 页面节点：`/business/erp/report/report-item` → `business/erp/report/report-item/index`，
--      挂在「财务管理」目录（`…0e02`）下，`sort_order = 48`
--      （资产负债表 45 / 利润表 46 / 应付账龄 47 之后，紧跟报表组）。
--   ② 两个权限码（与 `report_item_admin.rs` 的 `#[nik::permission]` 注解**逐字一致**）：
--        business:erp:report-item:list    读报表项 / 字典 / 取数预览
--        business:erp:report-item:manage  新增 / 编辑 / 排序 / 启停
--   ③ 授权：`list` 授 SUPER_ADMIN + FINANCE_MANAGER + FINANCE_DIRECTOR + GENERAL_MANAGER
--      （看报表口径的人和看报表的人一致）；`manage` **只授 SUPER_ADMIN + FINANCE_MANAGER**
--      —— 改报表项 = 改对外披露的口径，不是普通主数据。
--
-- # 为什么不种任何报表项
--
--   `biz_report_item` 的 53 行标准结构是 V0183（资产负债表 41 行）/ V0184（利润表 12 行）
--   种的**会计准则基线**。本迁移只负责让这 53 行「可维护」，不替客户改口径。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（不覆盖运维改过的标题 / 排序）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。空库首启时 `sys_role` 为空 → 授权是空操作
--   （bootstrap 之后重放本迁移即可补齐，与 V0192/V0197/V0202/V0206/V0253 同口径）。
--
-- # 回滚
--
--   `V0256__erp_report_item_admin.down.sql` 删 3 个节点 + 授权行；
--   **不动** `biz_report_item`（回滚维护页不该丢任何已生效的报表口径）。
-- ============================================================

-- ---------- ① 页面节点 + 2 个权限码 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 页面：报表项维护（读权限 = list）
    ('20000000-0000-0000-0000-000000001700'::uuid, '报表项维护', 2,
     'business:erp:report-item:list', '/business/erp/report/report-item',
     '20000000-0000-0000-0000-000000000e02'::uuid, 'business/erp/report/report-item/index',
     48, 1, 'chart', '报表项维护（报表项 ↔ 科目映射 / 取数公式 / 排序）', TRUE, 'business'),
    -- 按钮：查看（与页面同码，供只授页面的角色组合）
    ('20000000-0000-0000-0000-000000001701'::uuid, 'business:erp:report-item:list', 3,
     'business:erp:report-item:list', NULL, '20000000-0000-0000-0000-000000001700'::uuid, NULL,
     10, 1, NULL, '查看报表项 / 取数预览', FALSE, 'business'),
    -- 按钮：新增 / 编辑 / 排序 / 启停
    ('20000000-0000-0000-0000-000000001702'::uuid, 'business:erp:report-item:manage', 3,
     'business:erp:report-item:manage', NULL, '20000000-0000-0000-0000-000000001700'::uuid, NULL,
     20, 1, NULL, '新增 / 编辑 / 排序 / 启停报表项', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 授权 ----------
-- 读：报表使用者 + 财务
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001700'::uuid),
        ('20000000-0000-0000-0000-000000001701'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                       'GENERAL_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- 写：只给超级管理员与财务主管
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001702'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = '20000000-0000-0000-0000-000000001702'::uuid
   );

-- ---------- ③ 自检（失败即整笔回滚 ⇒ 启动失败） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000001700'::uuid;
    n_found   int;
    n_wrong   int;
    n_roles   int;
BEGIN
    SELECT count(*) INTO n_found
      FROM sys_menu
     WHERE id IN (page_menu,
                  '20000000-0000-0000-0000-000000001701'::uuid,
                  '20000000-0000-0000-0000-000000001702'::uuid)
       AND deleted_at IS NULL;
    IF n_found <> 3 THEN
        RAISE EXCEPTION 'V0256 自检失败：报表项维护节点只存在 % 个（应为 3）', n_found;
    END IF;

    -- 页面节点：类型 2、可见、挂在「财务管理」目录、component 与前端路由逐字一致
    SELECT count(*) INTO n_wrong
      FROM sys_menu
     WHERE id = page_menu
       AND (menu_type <> 2
            OR is_visible <> TRUE
            OR auth_code <> 'business:erp:report-item:list'
            OR path <> '/business/erp/report/report-item'
            OR component <> 'business/erp/report/report-item/index'
            OR parent_id <> '20000000-0000-0000-0000-000000000e02'::uuid
            OR module_code <> 'business');
    IF n_wrong > 0 THEN
        RAISE EXCEPTION 'V0256 自检失败：页面节点的类型 / 可见性 / auth_code / path / component / 父节点不对';
    END IF;

    -- 两个按钮：类型 3、父节点是页面、状态 1
    SELECT count(*) INTO n_wrong
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001701'::uuid,
                  '20000000-0000-0000-0000-000000001702'::uuid)
       AND (menu_type <> 3 OR parent_id IS DISTINCT FROM page_menu OR status <> 1);
    IF n_wrong > 0 THEN
        RAISE EXCEPTION 'V0256 自检失败：% 个按钮节点的类型 / 父节点 / 状态不对', n_wrong;
    END IF;

    -- 两个权限码必须在
    SELECT count(*) INTO n_wrong
      FROM (VALUES ('business:erp:report-item:list'), ('business:erp:report-item:manage')) AS t(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
    IF n_wrong <> 0 THEN
        RAISE EXCEPTION 'V0256 自检失败：% 个权限码缺失', n_wrong;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0256：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    END IF;

    RAISE NOTICE 'V0256 完成：报表项维护页 + 2 个权限码（读：财务/总经理；写：超管/财务主管）';
END $$;
