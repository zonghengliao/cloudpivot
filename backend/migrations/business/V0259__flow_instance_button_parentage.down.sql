-- ============================================================================
-- V0259 down：把 5 个实例侧按钮挂回旧父页 …0780（流程定义），并撤掉本迁移插入的补授行
--
-- 只做两件事：
--   ① UPDATE sys_menu.parent_id → …0780（仅限本迁移把父页改成 …0861/…0862 的那 5 行）；
--   ② DELETE 本迁移插入的 sys_role_menu 行 —— 按**同一条确定性 id 公式**重算后按 id 删，
--      人工勾选的行 id 是 `gen_random_uuid()`，公式 id 与它不可能碰撞 ⇒ 只删本迁移造的行。
--
-- 已知代价（写明白，别以为是漏洞）：
--   · 公式依赖「该角色仍持有这 5 个按钮之一」。若 up 之后有人把某个角色的按钮撤销了，
--     down 重算不出这个角色的补授行，会留下残行（生产 / 装配库 up 插入 0 行，无此问题）。
--   · 这是 `sys_role_menu` 没有「来源列」的通病，与 V0239 / V0258 down 同口径。
--   · 回滚只恢复结构，不恢复「归位后管理员在权限树里重新勾选」的结果 —— 归位本身
--     不改变任何角色的码集合，所以正常情况下回滚前后用户的可见性完全一致。
-- ============================================================================

DO $$
DECLARE
    v_btn_page   uuid := '20000000-0000-0000-0000-000000000862'::uuid;
    v_btn_start  uuid := '20000000-0000-0000-0000-000000000861'::uuid;
    v_old_parent uuid := '20000000-0000-0000-0000-000000000780'::uuid;
    n_back       int := 0;
    n_del        int := 0;
    n_ver        int := 0;
BEGIN
    -- 旧父页必须还在（软删也算在，V0121 是它的种源）
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = v_old_parent) THEN
        RAISE EXCEPTION 'V0259 down 失败：旧父页 …0780 不存在，无法还原';
    END IF;

    -- ---------- ① 结构回滚 ----------
    UPDATE sys_menu SET parent_id = v_old_parent, updated_at = now()
     WHERE id = '20000000-0000-0000-0000-000000000800'::uuid
       AND parent_id = v_btn_start;
    n_back := n_back + (SELECT count(*) FROM sys_menu
                         WHERE id = '20000000-0000-0000-0000-000000000800'::uuid
                           AND parent_id = v_old_parent);

    UPDATE sys_menu SET parent_id = v_old_parent, updated_at = now()
     WHERE id IN ('20000000-0000-0000-0000-000000000801'::uuid,
                  '20000000-0000-0000-0000-000000000802'::uuid,
                  '20000000-0000-0000-0000-000000000803'::uuid,
                  '20000000-0000-0000-0000-000000000804'::uuid)
       AND parent_id = v_btn_page;

    -- ---------- ② 撤掉本迁移的补授行（按确定性 id） ----------
    CREATE TEMP TABLE IF NOT EXISTS _v0259_down AS
    WITH RECURSIVE
    -- 本迁移可能补授过的角色集合：仍持有这 5 个按钮之一的在用角色
    holders AS (
        SELECT DISTINCT rm.role_id, t.new_parent
          FROM (VALUES
                  ('20000000-0000-0000-0000-000000000800'::uuid, v_btn_start),
                  ('20000000-0000-0000-0000-000000000801'::uuid, v_btn_page),
                  ('20000000-0000-0000-0000-000000000802'::uuid, v_btn_page),
                  ('20000000-0000-0000-0000-000000000803'::uuid, v_btn_page),
                  ('20000000-0000-0000-0000-000000000804'::uuid, v_btn_page)
               ) AS t(btn_id, new_parent)
          JOIN sys_role_menu rm ON rm.menu_id = t.btn_id
          JOIN sys_role r       ON r.id = rm.role_id AND r.deleted_at IS NULL
    ),
    hit AS (
        SELECT h.role_id, m.id AS menu_id
          FROM holders h
          JOIN sys_menu p ON p.id = h.new_parent AND p.deleted_at IS NULL
          JOIN sys_menu m ON m.deleted_at IS NULL AND m.auth_code = p.auth_code
    ),
    anc AS (
        SELECT h.role_id, p.id AS menu_id, coalesce(p.auth_code,'') AS code
          FROM hit h
          JOIN sys_menu c ON c.id = h.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
        UNION
        SELECT a.role_id, p.id, coalesce(p.auth_code,'')
          FROM anc a
          JOIN sys_menu c ON c.id = a.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
    )
    SELECT (substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3)
            || '-8' || substr(h,18,3) || '-' || substr(h,21,12))::uuid AS id,
           role_id, menu_id
      FROM (
        SELECT s.role_id, s.menu_id,
               md5(s.role_id::text || '|' || s.menu_id::text || '|v0259-orphan-parent') AS h
          FROM (SELECT role_id, menu_id FROM hit
                UNION
                SELECT role_id, menu_id FROM anc WHERE code = '') s
      ) x;

    DELETE FROM sys_role_menu rm
     USING _v0259_down d
     WHERE rm.id = d.id
       AND rm.role_id = d.role_id
       AND rm.menu_id = d.menu_id;
    GET DIAGNOSTICS n_del = ROW_COUNT;

    -- 撤权同样要推进 token_version（否则在线会话还带着旧码）
    IF n_del > 0 THEN
        UPDATE sys_user u
           SET token_version = u.token_version + 1, updated_at = now()
         WHERE u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_user_role ur
                        WHERE ur.user_id = u.id
                          AND ur.role_id IN (SELECT role_id FROM _v0259_down));
        GET DIAGNOSTICS n_ver = ROW_COUNT;
    END IF;

    -- ---------- ③ 回滚后断言：5 个节点必须回到旧父页 ----------
    IF (SELECT count(*) FROM sys_menu
         WHERE id IN ('20000000-0000-0000-0000-000000000800'::uuid,
                      '20000000-0000-0000-0000-000000000801'::uuid,
                      '20000000-0000-0000-0000-000000000802'::uuid,
                      '20000000-0000-0000-0000-000000000803'::uuid,
                      '20000000-0000-0000-0000-000000000804'::uuid)
           AND parent_id = v_old_parent) <> 5 THEN
        RAISE EXCEPTION 'V0259 down 断言失败：5 个实例侧按钮没有全部回到 …0780';
    END IF;

    RAISE NOTICE 'V0259 down 完成：5 个按钮回到 …0780 / 删除本迁移补授 % 行 / 推进 token_version % 个账号',
        n_del, n_ver;
END $$;
