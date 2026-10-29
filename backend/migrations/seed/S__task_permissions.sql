-- ======================================================
-- 工作任务管理 · 权限授权种子（`business:task:*` 授给所有未软删角色）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §七 + §十一 + §十三 T1 行；
--       节点由版本化迁移 `migrations/business/V0103__seed_task_permissions.up.sql` 创建。
-- 回滚：由 V0103 的 down 脚本统一清理（本文件不单独回滚 —— 种子迁移的语义是
--       「每次指纹变化就重跑」，它没有独立的 down）。
--
-- ── 为什么授权必须在**种子**里而不是版本化迁移里 ──────────────
-- 实测的时序事实：版本化迁移（V\\d+）先跑、种子（S__*.sql）后跑 ——
-- 空库实测 `schema_migrations.applied_seq` 的最后两条是
-- `menu_data=60` / `role_data=61`，而 V0103 的 applied_seq 更小。
--
-- 于是「V0103 里 `INSERT … SELECT FROM sys_role`」在**全新库**上命中 0 行：
-- 那时 SUPER_ADMIN 角色还没被 S__role_data.sql 创建出来。
-- 老库升级没问题、新装库会漏 —— 这种「只在全新环境复现」的权限缺口最难发现：
-- 超管因 `role_code` 短路仍能用接口，但**前端菜单树里没有它的授权行**，
-- 角色授权页勾选状态是错的（V0087 的公告读权限也踩过同一时序，只是没人查）。
--
-- 种子在 role_data 之后执行（按文件名升序：config → dict → menu → role → task），
-- 且对全新库是该文件的首次执行 → 两种情况都能覆盖。
--
-- ── 授权口径：**所有未软删角色**都授 ─────────────────────────
-- 任务管理是人人要用的入口（和公告栏同一性质）：普通员工要建自己的任务、
-- 拖自己的卡片；部门主管看本部门（数据范围由 `sys_role.data_scope` 控制）。
-- 只授超管 = 普通员工登录后点不到「工作任务」= 模块没交付。
-- **数据隔离靠 scope，不靠权限码** —— 别把这两件事混在一起。
--
-- ── 幂等 ──────────────────────────────────────────────────
-- 固定 UUID + `NOT EXISTS` 预判 + `ON CONFLICT DO NOTHING`：重复执行不报错。
-- ======================================================

-- ---------- 1) 授予所有未软删角色（含 SUPER_ADMIN，便于角色树勾选状态正确） ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000760'::uuid),
                    ('20000000-0000-0000-0000-000000000761'::uuid),
                    ('20000000-0000-0000-0000-000000000762'::uuid),
                    ('20000000-0000-0000-0000-000000000763'::uuid),
                    ('20000000-0000-0000-0000-000000000764'::uuid),
                    -- T2 追加（V0108 建节点）：改状态 / 改进度 / 建子任务
                    ('20000000-0000-0000-0000-000000000765'::uuid),
                    ('20000000-0000-0000-0000-000000000766'::uuid),
                    ('20000000-0000-0000-0000-000000000767'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 2) 自检（失败即启动失败） ----------
-- **每个**未软删角色都必须持有 7 个按钮：缺一个就是「有人登录后点不到工作任务 / 推不动状态」。
-- 宁可在启动期炸掉，也不要等上线后被用户发现。
DO $$
DECLARE
    btn_read     uuid := '20000000-0000-0000-0000-000000000761'::uuid;
    btn_create   uuid := '20000000-0000-0000-0000-000000000762'::uuid;
    btn_update   uuid := '20000000-0000-0000-0000-000000000763'::uuid;
    btn_delete   uuid := '20000000-0000-0000-0000-000000000764'::uuid;
    btn_status   uuid := '20000000-0000-0000-0000-000000000765'::uuid;
    btn_progress uuid := '20000000-0000-0000-0000-000000000766'::uuid;
    btn_subtask  uuid := '20000000-0000-0000-0000-000000000767'::uuid;
    missing      int;
BEGIN
    SELECT count(*) INTO missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND EXISTS (
               SELECT 1
                 FROM (VALUES (btn_read), (btn_create), (btn_update), (btn_delete),
                              (btn_status), (btn_progress), (btn_subtask)) AS b(id)
                WHERE NOT EXISTS (
                          SELECT 1 FROM sys_role_menu rm
                           WHERE rm.role_id = r.id AND rm.menu_id = b.id
                      )
           );

    IF missing > 0 THEN
        RAISE EXCEPTION 'S__task_permissions 自检失败：还有 % 个未软删角色缺少 business:task:* 授权', missing;
    END IF;
END
$$;

-- ---------- 3) T3：分派 / 协作人权限（**只授超管**） ----------
--
-- 这两个码**刻意不进上面的「全员授予」清单**：
--
--   分派权的真实口径是「行级关系（创建人 / 当前负责人 / 分派人）∪ 权限码」。
--   如果人人都有 `business:task:assign`，那「无关用户来分派 → 403」这条验收
--   永远不可能成立 —— 权限码一授予，隔离就没了。
--
-- 所以：默认只授 SUPER_ADMIN（超管本来就短路全部权限，授它是为了让角色树的
-- 勾选状态正确）；需要跨任务派活的角色（部门主管 / 将来的「AI 团队主管」）
-- 由管理员在角色页显式勾选 —— 这就是修正 11「默认不能、可按角色授予」的落地方式。
--
-- 时序：本文件在 `S__role_data.sql` 之后执行（按文件名升序：
-- config → dict → menu → role → task），所以全新库上 SUPER_ADMIN 已经存在。
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000768'::uuid),
                    ('20000000-0000-0000-0000-000000000769'::uuid)) AS m(id)
 WHERE r.role_code = 'SUPER_ADMIN'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- 自检：**若**超管角色存在，两个码必须都持有；且**不许**有非超管角色被误授
-- （误授 = 隔离失效，比漏授危险得多）。
DO $$
DECLARE
    btn_assign uuid := '20000000-0000-0000-0000-000000000768'::uuid;
    btn_member uuid := '20000000-0000-0000-0000-000000000769'::uuid;
    super_id   uuid;
    missing    int;
    leaked     int;
BEGIN
    SELECT id INTO super_id FROM sys_role
     WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL LIMIT 1;

    IF super_id IS NULL THEN
        RAISE NOTICE 'S__task_permissions 第 3 段：未找到 SUPER_ADMIN 角色，跳过分派权自检';
        RETURN;
    END IF;

    SELECT count(*) INTO missing
      FROM (VALUES (btn_assign), (btn_member)) AS b(id)
     WHERE NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = super_id AND rm.menu_id = b.id
           );
    IF missing > 0 THEN
        RAISE EXCEPTION 'S__task_permissions 自检失败：SUPER_ADMIN 缺 % 个分派 / 协作人权限', missing;
    END IF;

    SELECT count(*) INTO leaked
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id IN (btn_assign, btn_member)
       AND r.role_code <> 'SUPER_ADMIN';
    IF leaked > 0 THEN
        RAISE NOTICE 'S__task_permissions 提示：% 行分派 / 协作人授权给了非超管角色（管理员显式勾选的，属预期）', leaked;
    END IF;
END
$$;
