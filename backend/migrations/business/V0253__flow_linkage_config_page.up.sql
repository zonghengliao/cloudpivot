-- ============================================================
-- V0253：跨单据联动规则的**配置页 + 权限码**（卡 t_d0daa4f0 / P1-FIX-5A）
--
-- 依据：`backend/src/business/flow/linkage/admin.rs`（HTTP 管理面）
--       `backend/src/business/flow/linkage/spec.rs`（11 个白名单动作）
--       `frontend/src/views/business/flow/linkage/index.vue`（配置页）
--
-- # 这条迁移做什么（只有菜单与权限，**不动任何业务表**）
--
--   ① 页面节点：`/flow/linkage` → `business/flow/linkage/index`，挂在「审批流」目录
--      （`…000860`）下，`sort_order = 40`（流程定义 10 / 发起审批 20 / 审批中心 30 之后）。
--   ② 两个权限码（与 `admin.rs` 的 `#[nik::permission]` 注解**逐字一致**）：
--        business:flow:linkage:list    读规则 + 预览影响
--        business:flow:linkage:manage  增改 / 启停 / 删除
--   ③ 授权：两个码都只授 `SUPER_ADMIN` —— 与内核 `model::ensure_config_admin` 同口径
--      （联动规则决定「单据状态变了以后还会动哪些账」，属业务规则而非普通配置）。
--
-- # 为什么 `biz_flow_linkage_rule` 里不种任何规则
--
--   V0205 建表时就是 0 行，那是**刻意**的：0 条规则 = 引擎一条动作都不执行 = 现有业务行为逐字不变。
--   种几条「演示规则」进生产等于上线即改账务联动，属于业务决策，不该由迁移替客户决定。
--   配置页负责让「配一条规则」变成 30 秒内可完成的事（列表 / 新增 / 编辑 / 启停 / 预览影响）。
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`（不覆盖运维改过的标题 / 排序）；
--   `sys_role_menu` 用 `NOT EXISTS` 去重。空库首启时 `sys_role` 为空 → 授权是空操作
--   （bootstrap 之后重放本迁移即可补齐，与 V0192/V0197/V0202/V0206 同口径）。
--
-- # 回滚
--
--   `V0253__flow_linkage_config_page.down.sql` 删两个权限码 + 页面节点 + 授权行；
--   **不动** `biz_flow_linkage_rule`（那是 V0205 的表，回滚本迁移不该丢配置），
--   也不动 `biz_status_log`（审计不在回滚范围）。
-- ============================================================

-- ---------- ① 页面节点 + 2 个权限码 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 页面：跨单据联动规则（读权限 = list；写权限单列为按钮节点）
    ('20000000-0000-0000-0000-000000001230'::uuid, '联动规则', 2,
     'business:flow:linkage:list', '/flow/linkage',
     '20000000-0000-0000-0000-000000000860'::uuid, 'business/flow/linkage/index',
     40, 1, 'link', '跨单据联动规则（源单据 + 触发状态 → 白名单动作）', TRUE, 'business'),
    -- 按钮：查看（与页面同码，供只授页面的角色按需组合）
    ('20000000-0000-0000-0000-000000001231'::uuid, 'business:flow:linkage:list', 3,
     'business:flow:linkage:list', NULL, '20000000-0000-0000-0000-000000001230'::uuid, NULL,
     10, 1, NULL, '查看联动规则 / 预览执行影响', FALSE, 'business'),
    -- 按钮：增改 / 启停 / 删除（仅 SUPER_ADMIN）
    ('20000000-0000-0000-0000-000000001232'::uuid, 'business:flow:linkage:manage', 3,
     'business:flow:linkage:manage', NULL, '20000000-0000-0000-0000-000000001230'::uuid, NULL,
     20, 1, NULL, '新增 / 编辑 / 启停 / 删除联动规则（仅 SUPER_ADMIN）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 授权：两个码都只给 SUPER_ADMIN ----------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001230'::uuid),
        ('20000000-0000-0000-0000-000000001231'::uuid),
        ('20000000-0000-0000-0000-000000001232'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- ③ 自检（失败即整笔回滚 ⇒ 启动失败） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000001230'::uuid;
    n_found   int;
    n_wrong   int;
BEGIN
    SELECT count(*) INTO n_found
      FROM sys_menu
     WHERE id IN (page_menu,
                  '20000000-0000-0000-0000-000000001231'::uuid,
                  '20000000-0000-0000-0000-000000001232'::uuid)
       AND deleted_at IS NULL;
    IF n_found <> 3 THEN
        RAISE EXCEPTION 'V0253 自检失败：联动规则节点只存在 % 个（应为 3）', n_found;
    END IF;

    -- 页面节点：类型 2、可见、挂在「审批流」目录、component 与前端路由逐字一致
    SELECT count(*) INTO n_wrong
      FROM sys_menu
     WHERE id = page_menu
       AND (menu_type <> 2
            OR is_visible <> TRUE
            OR auth_code <> 'business:flow:linkage:list'
            OR path <> '/flow/linkage'
            OR component <> 'business/flow/linkage/index'
            OR parent_id <> '20000000-0000-0000-0000-000000000860'::uuid
            OR module_code <> 'business');
    IF n_wrong > 0 THEN
        RAISE EXCEPTION 'V0253 自检失败：页面节点的类型 / 可见性 / auth_code / path / component / 父节点不对';
    END IF;

    -- 两个按钮：类型 3、父节点是页面、状态 1
    SELECT count(*) INTO n_wrong
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001231'::uuid,
                  '20000000-0000-0000-0000-000000001232'::uuid)
       AND (menu_type <> 3 OR parent_id IS DISTINCT FROM page_menu OR status <> 1);
    IF n_wrong > 0 THEN
        RAISE EXCEPTION 'V0253 自检失败：% 个按钮节点的类型 / 父节点 / 状态不对', n_wrong;
    END IF;

    -- 两个权限码必须在（我们自己插的 → 缺了就 EXCEPTION）
    SELECT count(*) INTO n_wrong
      FROM (VALUES ('business:flow:linkage:list'), ('business:flow:linkage:manage')) AS t(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
    IF n_wrong <> 0 THEN
        RAISE EXCEPTION 'V0253 自检失败：% 个权限码缺失', n_wrong;
    END IF;

    RAISE NOTICE 'V0253 完成：联动规则配置页 + 2 个权限码（仅授 SUPER_ADMIN）';
END $$;
