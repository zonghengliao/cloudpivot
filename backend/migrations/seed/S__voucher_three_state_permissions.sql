-- ======================================================
-- 凭证三态审核链 · 权限授权种子（【P0-FIX-4A】）
--
-- 菜单行（3 个权限码）由版本化迁移 `migrations/business/V0250__erp_voucher_three_state.up.sql`
-- 创建；**授权**放在这里，因为种子按文件名字典序执行，本文件排在
-- `S__role_data.sql`（`r` < `v`）之后 —— 空库首启时角色行一定已经存在。
-- 反例：写在版本化迁移里会在空库上命中 0 个角色（迁移先于种子跑），
-- 实测证据见 `docs/evidence/20261105-p0fix4a/03_migration_drill.txt`
-- （第一次 up 后 SUPER_ADMIN 的授权行 = 0，重放同一份 SQL 才补上 3 行）。
--
-- ── 授权口径 ──────────────────────────────────────────────
-- 与 V0250 里那段 DO 块一致：**谁已经能制单（business:erp:voucher:create），
-- 就把 audit / un-audit / post 三个码给他**，外加 SUPER_ADMIN。
--
-- 这不是放宽内控：内控拦的是「同一个人审自己那张」（后端 `check_self_audit`
-- → 403 + 65019），判据是**人**不是角色 —— 张三制的单李四能审，角色集合相同
-- 照样实现双人复核。要收紧成「审核岗位 ≠ 制单岗位」，在角色授权页取消勾选即可。
--
-- 生产实测（2026-11-05）：只有 SUPER_ADMIN 持有 voucher:create，因此生产上
-- 就是 SUPER_ADMIN 独有；财务经理 / 财务总监要用这三个动作时由管理员授权。
--
-- ── 幂等 ──────────────────────────────────────────────────
-- `NOT EXISTS` 预判 + `ON CONFLICT DO NOTHING`：重复执行不报错。
-- ======================================================

INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000001032'::uuid),
                    ('20000000-0000-0000-0000-000000001033'::uuid),
                    ('20000000-0000-0000-0000-000000001034'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND (
        r.role_code = 'SUPER_ADMIN'
        OR EXISTS (
            SELECT 1
              FROM sys_role_menu rm
              JOIN sys_menu cm ON cm.id = rm.menu_id
             WHERE rm.role_id = r.id
               AND cm.auth_code = 'business:erp:voucher:create'
               AND cm.deleted_at IS NULL
        )
   )
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu granted
         WHERE granted.role_id = r.id AND granted.menu_id = m.id
   )
ON CONFLICT DO NOTHING;

-- ---------- 自检：SUPER_ADMIN 必须三码齐全（失败即启动失败）----------
DO $$
DECLARE
    n_granted integer;
BEGIN
    SELECT count(*) INTO n_granted
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
     WHERE r.role_code = 'SUPER_ADMIN'
       AND rm.menu_id IN ('20000000-0000-0000-0000-000000001032'::uuid,
                          '20000000-0000-0000-0000-000000001033'::uuid,
                          '20000000-0000-0000-0000-000000001034'::uuid);

    IF n_granted <> 3 THEN
        RAISE EXCEPTION
            'S__voucher_three_state_permissions 自检失败：SUPER_ADMIN 只拿到 % 个凭证审核链权限码（期望 3）',
            n_granted;
    END IF;

    RAISE NOTICE 'S__voucher_three_state_permissions：SUPER_ADMIN 已持有 audit / un-audit / post 三码';
END $$;
