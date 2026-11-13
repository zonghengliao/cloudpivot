-- ============================================================================
-- V0401.down：撤回「页面码 / 全量接口码解耦」（试点：人力资源 5 个页面行）
--
-- 顺序（与 up 相反）：
--   ① 撤掉 `*:list` 按钮行上的授权（否则删菜单行会撞 sys_role_menu 的外键）
--   ② 删 `*:list` 按钮行（只删「确实还是本迁移那一行」的：id + menu_type=3 + 本码）
--   ③ 页面行码改回 `*:list`（页面行 id 不动，所以第 ① 步撤掉的只是按钮授权，
--      页面本身的授权从来没被动过 ⇒ 回滚后侧边栏与回滚前一致）
--
-- # 语义与代价（别当无害操作）
--   回滚后「页面码 = 全量列表码」的老耦合**复活**：EMPLOYEE 只要仍有页面授权，
--   就会再次通过 `*:list` 拿到全量列表 ⇒ `t_be54535c` 的两处 handler 人肉闸必须同时恢复
--   （`salary_sheet_handler::ensure_full_list` / `leave_handler::scope_to_self_without_detail`），
--   否则员工立刻能拉全公司工资单。这是回滚点写在 ADR §回滚 里的原因。
--
-- # 幂等
--   每条都带完整识别条件 ⇒ 重放（或从未 up 过的库上跑）都是 0 行。
-- ============================================================================

-- ① 撤授权
DELETE FROM sys_role_menu
 WHERE menu_id = ANY (ARRAY[
        '20000000-0000-4000-8000-00000000e061','20000000-0000-4000-8000-00000000e062',
        '20000000-0000-4000-8000-00000000e063','20000000-0000-4000-8000-00000000e064',
        '20000000-0000-4000-8000-00000000e065'
       ]::uuid[]);

-- ② 删按钮行（逐个点名 id + 码，避免误删被人工改过含义的行）
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000e061'::uuid
   AND menu_type = 3 AND auth_code = 'business:hr:employee:list';
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000e062'::uuid
   AND menu_type = 3 AND auth_code = 'business:hr:attendance:list';
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000e063'::uuid
   AND menu_type = 3 AND auth_code = 'business:hr:leave:list';
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000e064'::uuid
   AND menu_type = 3 AND auth_code = 'business:hr:salary:list';
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000e065'::uuid
   AND menu_type = 3 AND auth_code = 'business:hr:salary-item:list';

-- ③ 页面码改回 `*:list`
UPDATE sys_menu SET auth_code = 'business:hr:employee:list', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e001'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:employee:page';
UPDATE sys_menu SET auth_code = 'business:hr:attendance:list', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e002'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:attendance:page';
UPDATE sys_menu SET auth_code = 'business:hr:leave:list', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e003'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:leave:page';
UPDATE sys_menu SET auth_code = 'business:hr:salary:list', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e004'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:salary:page';
UPDATE sys_menu SET auth_code = 'business:hr:salary-item:list', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e005'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:salary-item:page';

-- ④ 自检：回滚后必须是「页面行 = `*:list`，且没有 `*:page` 残行」
DO $$
DECLARE
    n_left INT;
    n_page_left INT;
BEGIN
    SELECT count(*) INTO n_page_left
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND auth_code LIKE 'business:hr:%:page';
    IF n_page_left > 0 THEN
        RAISE EXCEPTION 'V0401.down 自检失败：还有 % 行页面挂着 `*:page`', n_page_left;
    END IF;

    SELECT count(*) INTO n_left
      FROM sys_menu
     WHERE id = ANY (ARRAY[
            '20000000-0000-4000-8000-00000000e061','20000000-0000-4000-8000-00000000e062',
            '20000000-0000-4000-8000-00000000e063','20000000-0000-4000-8000-00000000e064',
            '20000000-0000-4000-8000-00000000e065'
           ]::uuid[]);
    IF n_left > 0 THEN
        RAISE EXCEPTION 'V0401.down 自检失败：还有 % 行 `*:list` 按钮没删掉', n_left;
    END IF;

    RAISE NOTICE 'V0401.down：页面码已回滚为 `*:list`（% 行仍无 `:page`）', n_page_left;
END $$;
