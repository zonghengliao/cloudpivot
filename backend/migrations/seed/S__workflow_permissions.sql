-- ======================================================
-- 审批流程定义 · 权限授权种子（批次 D · D1）
--
-- ⚠️ 文件名前缀必须是 `S__workflow_*`（`w` > `r`）：种子按**文件名字典序**执行，
--    `S__flow_permissions.sql` 会排在 `S__role_data.sql` **之前**，空库上
--    `INSERT … SELECT FROM sys_role` 命中 0 行（T7c 实测踩过，见其验证证据 §3）。
--
-- 节点由版本化迁移 `migrations/business/V0121__flow_def_permissions.up.sql` 创建：
--   …0780 页面节点（隐藏，D6 放出来）+ …0781 read / …0782 create / …0783 update /
--   …0784 delete / …0785 publish
--
-- 回滚：由 V0121 的 down 脚本统一清理（种子迁移没有独立 down）。
--
-- ── 授权口径（两档，刻意不对称） ──────────────────────────────
-- 1. `business:flow:read` → **所有未软删角色**
--    流程定义列表是「我这类单据走哪个流程」的查询入口，人人可读；
--    改图/发布是管理动作，靠下面三个码隔离。
-- 2. `create` / `update` / `delete` / `publish` → **只授 SUPER_ADMIN**
--    与 V0109 / V0113 / V0115 的 assign / member / comment:delete / project:update 同口径：
--    「改别人的配置」的加项权限。默认全员授予 = 没有隔离。
--
-- ── 幂等 ────────────────────────────────────────────────────
-- `NOT EXISTS` 预判 + `ON CONFLICT DO NOTHING`：重复执行不报错。
-- ======================================================

-- ---------- 1) 读：授予所有未软删角色 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, '20000000-0000-0000-0000-000000000781'::uuid, CURRENT_TIMESTAMP
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id
              AND granted.menu_id = '20000000-0000-0000-0000-000000000781'::uuid
       )
ON CONFLICT DO NOTHING;

-- ---------- 2) 写四码：只授 SUPER_ADMIN ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000782'::uuid),
                    ('20000000-0000-0000-0000-000000000783'::uuid),
                    ('20000000-0000-0000-0000-000000000784'::uuid),
                    ('20000000-0000-0000-0000-000000000785'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT DO NOTHING;

-- ---------- 3) 页面节点的可见性：保持迁移里登记的状态（隐藏），此处不动 ----------
-- D6 建好前端页面后执行：UPDATE sys_menu SET is_visible = TRUE
--   WHERE id = '20000000-0000-0000-0000-000000000780';

-- ---------- 4) 自检 ----------
DO $$
DECLARE
    btn_read   uuid := '20000000-0000-0000-0000-000000000781'::uuid;
    btn_create uuid := '20000000-0000-0000-0000-000000000782'::uuid;
    btn_update uuid := '20000000-0000-0000-0000-000000000783'::uuid;
    btn_delete uuid := '20000000-0000-0000-0000-000000000784'::uuid;
    btn_pub    uuid := '20000000-0000-0000-0000-000000000785'::uuid;
    missing    int;
    super_lack int;
    super_read int;
BEGIN
    -- 每个未软删角色都必须有 read
    SELECT count(*) INTO missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = btn_read
           );
    IF missing > 0 THEN
        RAISE EXCEPTION
            'S__workflow_permissions 自检失败：还有 % 个未软删角色缺少 business:flow:read 授权', missing;
    END IF;

    -- 超管必须有四个写码
    SELECT count(*) INTO super_lack
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND r.role_code = 'SUPER_ADMIN'
       AND EXISTS (
               SELECT 1
                 FROM (VALUES (btn_create), (btn_update), (btn_delete), (btn_pub)) AS b(id)
                WHERE NOT EXISTS (
                          SELECT 1 FROM sys_role_menu rm
                           WHERE rm.role_id = r.id AND rm.menu_id = b.id
                      )
           );
    IF super_lack > 0 THEN
        RAISE EXCEPTION
            'S__workflow_permissions 自检失败：SUPER_ADMIN 缺少 create/update/delete/publish 授权';
    END IF;

    -- 超管也必须能读（否则「能改不能看」这种配置会把人卡死）
    SELECT count(*) INTO super_read
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND r.role_code = 'SUPER_ADMIN'
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = btn_read
           );
    IF super_read > 0 THEN
        RAISE EXCEPTION 'S__workflow_permissions 自检失败：SUPER_ADMIN 缺少 business:flow:read 授权';
    END IF;
END
$$;
