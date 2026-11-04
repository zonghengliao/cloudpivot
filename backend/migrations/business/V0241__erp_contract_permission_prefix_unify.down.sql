-- ============================================================================
-- V0241__erp_contract_permission_prefix_unify.down.sql
--
-- 回滚 V0241：把合同域 5 个权限码从 `business:erp:contract:*` 改回 `erp:contract:*`。
--
-- 与 up 一样**只改名、不删行、不碰授权表**：菜单 id 不变 ⇒ `sys_role_menu`
-- 一行不动，回滚也不会丢已配授权。
--
-- ⚠ 回滚后必须同时回滚代码（`contract_handler.rs` 的 5 个常量与注解、前端三处判权、
--   `shared/permission_alias.rs`）。否则路由注解仍在问 `business:erp:contract:*`，
--   而库里只剩 `erp:contract:*` —— 除超管外全员 403。别名表是双向的，能兜住
--   「注解新码 + 库里旧码」这个方向，但那只是兼容窗口，不该被当成稳态。
-- ============================================================================

DO $$
DECLARE
    v_ids          uuid[] := ARRAY[
        '20000000-0000-0000-0000-000000001100',
        '20000000-0000-0000-0000-000000001101',
        '20000000-0000-0000-0000-000000001102',
        '20000000-0000-0000-0000-000000001103',
        '20000000-0000-0000-0000-000000001104'
    ]::uuid[];
    n_grant_before bigint;
    n_grant_after  bigint;
    n_codes        integer;
BEGIN
    SELECT count(*) INTO n_grant_before FROM sys_role_menu WHERE menu_id = ANY (v_ids);

    UPDATE sys_menu
       SET auth_code = 'erp:' || substring(auth_code FROM length('business:erp:') + 1)
     WHERE auth_code LIKE 'business:erp:contract:%';

    UPDATE sys_menu
       SET name = auth_code
     WHERE menu_type = 3
       AND auth_code LIKE 'erp:contract:%'
       AND name LIKE 'business:erp:contract:%';

    SELECT count(DISTINCT auth_code) INTO n_codes
      FROM sys_menu
     WHERE auth_code LIKE 'erp:contract:%' AND deleted_at IS NULL AND status = 1;
    IF n_codes <> 5 THEN
        RAISE EXCEPTION 'V0241 回滚自检失败：erp:contract:* 应有 5 个启用码，实际 %', n_codes;
    END IF;

    SELECT count(*) INTO n_grant_after FROM sys_role_menu WHERE menu_id = ANY (v_ids);
    IF n_grant_after <> n_grant_before THEN
        RAISE EXCEPTION 'V0241 回滚自检失败：授权行数 前 % / 后 %（应相等）',
            n_grant_before, n_grant_after;
    END IF;

    -- ---------- ④ 回滚同样推进 token_version（理由同 up：让在途令牌换一批码） ----------
    UPDATE sys_user u
       SET token_version = token_version + 1,
           updated_at = now()
     WHERE u.deleted_at IS NULL
       AND u.id IN (
           SELECT ur.user_id
             FROM sys_user_role ur
             JOIN sys_role_menu rm ON rm.role_id = ur.role_id
            WHERE rm.menu_id = ANY (v_ids)
       );

    RAISE NOTICE 'V0241 回滚完成：5 个 erp:contract:* 恢复，授权行 % → %（零丢失）',
        n_grant_before, n_grant_after;
END $$;
