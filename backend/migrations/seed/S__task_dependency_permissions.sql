-- ======================================================
-- 工作任务管理 · 依赖 / 甘特权限授权种子（批次 C+ · T4）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §四（依赖）、§五（甘特图）；
--       节点由版本化迁移 `migrations/business/V0110__task_dependency_constraints.up.sql` 创建。
-- 回滚：由 V0110 的 down 脚本统一清理（种子迁移没有独立 down —— 它的语义是
--       「指纹变化就重跑」，见 S__task_permissions.sql 的同款说明）。
--
-- ── 两段授权，口径故意不同 ────────────────────────────────────
--
-- ① 甘特页面（…0770）：**所有未软删角色**都授。
--    甘特是「看排期」的只读视图，和任务列表同一性质；数据隔离靠 `sys_role.data_scope`
--    与可见性谓词（scope ∪ 参与 id 数组），**不靠权限码**。只授超管 = 普通员工登录后
--    从侧边栏点不到甘特 = 功能没交付。
--
-- ② 依赖管理按钮（…0771 `business:task:dependency`）：**只授 SUPER_ADMIN**。
--    与 T3 的分派 / 协作人同款理由：依赖管理权的真实口径是「行级关系
--    （负责人 / 创建人 / 分派人）∪ 权限码」。若人手一个码，
--    「无关用户加依赖 → 403」这条验收永远不可能成立 —— 权限码一授予，隔离就没了。
--    需要跨任务理依赖的角色（项目协调 / 部门主管）由管理员在角色页显式勾选。
--
-- ── 时序 ──────────────────────────────────────────────────────
-- 种子迁移在 `S__role_data.sql` 之后执行（按文件名升序：config → dict → menu →
-- role → task_dependency → task_permissions），所以全新库上 SUPER_ADMIN 已经存在。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- `NOT EXISTS` 预判 + `ON CONFLICT (role_id, menu_id) DO NOTHING`：重复执行不报错。
-- ======================================================

-- ---------- ① 甘特页面：全员可见 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000770'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ② 依赖管理：只授超管 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000771'::uuid)) AS m(id)
 WHERE r.role_code = 'SUPER_ADMIN'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    page_gantt uuid := '20000000-0000-0000-0000-000000000770'::uuid;
    btn_dep    uuid := '20000000-0000-0000-0000-000000000771'::uuid;
    super_id   uuid;
    missing    int;
    leaked     int;
BEGIN
    -- 1) 每个未软删角色都必须能看到甘特页
    SELECT count(*) INTO missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = page_gantt
           );
    IF missing > 0 THEN
        RAISE EXCEPTION
            'S__task_dependency_permissions 自检失败：% 个未软删角色看不到甘特页（% 未授权）',
            missing, page_gantt;
    END IF;

    -- 2) 超管必须持有依赖管理码（角色树勾选状态正确）
    SELECT id INTO super_id FROM sys_role
     WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL LIMIT 1;

    IF super_id IS NULL THEN
        RAISE NOTICE 'S__task_dependency_permissions：未找到 SUPER_ADMIN，跳过依赖码自检';
        RETURN;
    END IF;

    SELECT count(*) INTO missing
      FROM (VALUES (btn_dep)) AS b(id)
     WHERE NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = super_id AND rm.menu_id = b.id
           );
    IF missing > 0 THEN
        RAISE EXCEPTION 'S__task_dependency_permissions 自检失败：SUPER_ADMIN 缺依赖管理权限';
    END IF;

    -- 3) 非超管被误授依赖码只提示（管理员显式勾选属预期，不阻断）
    SELECT count(*) INTO leaked
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id = btn_dep
       AND r.role_code <> 'SUPER_ADMIN';
    IF leaked > 0 THEN
        RAISE NOTICE
            'S__task_dependency_permissions 提示：% 行依赖管理授权给了非超管角色（管理员显式勾选，属预期）',
            leaked;
    END IF;
END
$$;
