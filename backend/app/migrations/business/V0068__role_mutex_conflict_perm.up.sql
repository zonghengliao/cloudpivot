-- ======================================================
-- V0068：角色互斥 · 冲突查询权限（批次 B · B7，架构 §5.1 的「例外」条）
--
-- 依据：`docs/批次B-技术架构.md` §5.1 权限码表第 12 行
--       `system:role-mutex:conflict` 的授予策略说明：
--       「它只读、不改变态，且『授角色』的日常操作者需要它才知道为什么被拒。
--         若只授超管，非超管管理员点保存会拿到 409 但打不开冲突清单（因为清单接口 403）
--         —— 体验断裂。所以在所有**持有 `system:user:role` 的角色**上追加授予。」
--
-- 回滚：V0068__role_mutex_conflict_perm.down.sql（只收回非超管角色的该按钮）
--
-- ── 为什么这条权限可以给非超管 ────────────────────────────────
-- 它返回的是「哪些用户同时持有两个互斥角色」——这些人本来就该被处理掉，属于
-- SoD 落地必须让管理员看到的信息；且接口只读、不落库、不改任何权限状态。
--
-- ── 为什么用「持有 `system:user:role` 的角色」当锚点 ──────────
-- 能授角色的角色 = 会撞到 409 的角色。锚点取菜单授权（`sys_role_menu`）而不是
-- 「谁登录过」，因为权限来源只有一处：角色的菜单授权。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- 固定 UUID + `NOT EXISTS` + `ON CONFLICT (role_id, menu_id) DO NOTHING`：
-- 重复执行不报错，也不覆盖运维后续手工收回的授权（收回后重跑迁移会再授回来，
-- 这是版本化迁移的语义，不是 bug —— 需要长期收回就改本文件并出新版本）。
-- ======================================================

-- 1) 给所有「能授角色」（持 `system:user:role`）且未软删的角色补授冲突查询按钮。
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT DISTINCT rm.role_id,
                '20000000-0000-0000-0000-000000000624'::uuid,
                CURRENT_TIMESTAMP
  FROM sys_role_menu rm
 INNER JOIN sys_menu anchor
    ON anchor.id = rm.menu_id
   AND anchor.auth_code = 'system:user:role'
 INNER JOIN sys_role r
    ON r.id = rm.role_id
   AND r.deleted_at IS NULL
 WHERE NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = rm.role_id
              AND granted.menu_id = '20000000-0000-0000-0000-000000000624'::uuid
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- 2) 自检（失败即迁移失败）：`system:role-mutex:conflict` 节点必须存在，
--    且每个持 `system:user:role` 的角色都必须持它。缺一个就是「管理员被 409 拦住
--    却打不开清单」的体验断裂，宁可在迁移期炸掉也不要在生产上发现。
DO $$
DECLARE
    conflict_menu uuid := '20000000-0000-0000-0000-000000000624'::uuid;
    missing       int;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = conflict_menu AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0068 自检失败：权限节点 % 不存在（V0065 未应用？）', conflict_menu;
    END IF;

    SELECT count(*) INTO missing
      FROM (
            SELECT DISTINCT rm.role_id
              FROM sys_role_menu rm
             INNER JOIN sys_menu anchor
                ON anchor.id = rm.menu_id
               AND anchor.auth_code = 'system:user:role'
             INNER JOIN sys_role r
                ON r.id = rm.role_id
               AND r.deleted_at IS NULL
           ) AS grantors
     WHERE NOT EXISTS (
               SELECT 1 FROM sys_role_menu granted
                WHERE granted.role_id = grantors.role_id
                  AND granted.menu_id = conflict_menu
           );

    IF missing > 0 THEN
        RAISE EXCEPTION 'V0068 自检失败：还有 % 个持 system:user:role 的角色没有 system:role-mutex:conflict 授权', missing;
    END IF;
END
$$;
