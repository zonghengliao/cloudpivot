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
                    ('20000000-0000-0000-0000-000000000764'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 2) 自检（失败即启动失败） ----------
-- **每个**未软删角色都必须持有 4 个按钮：缺一个就是「有人登录后点不到工作任务」。
-- 宁可在启动期炸掉，也不要等上线后被用户发现。
DO $$
DECLARE
    btn_read   uuid := '20000000-0000-0000-0000-000000000761'::uuid;
    btn_create uuid := '20000000-0000-0000-0000-000000000762'::uuid;
    btn_update uuid := '20000000-0000-0000-0000-000000000763'::uuid;
    btn_delete uuid := '20000000-0000-0000-0000-000000000764'::uuid;
    missing    int;
BEGIN
    SELECT count(*) INTO missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND EXISTS (
               SELECT 1 FROM (VALUES (btn_read), (btn_create), (btn_update), (btn_delete)) AS b(id)
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
