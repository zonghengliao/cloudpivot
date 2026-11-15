-- ======================================================
-- V0190 down：撤掉年结（ERP-R4）的结构与节点
--
-- # 边界（必读）
--
-- · 安全阀：库里还有 `voucher_type='YEAR_END'` 的结转凭证时**中止**。
--   把白名单收回会让这些凭证变成「非法行」，而删期初来源列又会让「哪些期初是年结生成的」
--   无从判断。正确做法是先反年结（业务动作），再回滚迁移。
-- · 期初**数据行**一行不动：`source` 列没了，行还在（它们本来就是账的一部分，
--   只是不再能区分来源）。
-- · 权限节点与授权行删掉；`sys_menu` 里运维改过标题的行也删（auth_code 是本迁移种的）。
--
-- 幂等：全部 IF EXISTS。
-- ======================================================

DO $$
DECLARE
    n_year_end INT;
BEGIN
    SELECT count(*) INTO n_year_end
      FROM biz_voucher
     WHERE voucher_type = 'YEAR_END' AND deleted_at IS NULL;
    IF n_year_end > 0 THEN
        RAISE EXCEPTION
            'V0190 down 中止：库里还有 % 张年末结转凭证（voucher_type=YEAR_END）。\n'
            '请先在业务上反年结（「年度结转」页 · 反年结）再回滚本迁移 —— '
            '否则收回 voucher_type 白名单会让它们在库层面变成非法行。', n_year_end;
    END IF;
END $$;

-- ① 权限节点（先删授权行，再删节点）
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001025'::uuid,
                   '20000000-0000-0000-0000-000000001026'::uuid,
                   '20000000-0000-0000-0000-000000001027'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001025'::uuid,
              '20000000-0000-0000-0000-000000001026'::uuid,
              '20000000-0000-0000-0000-000000001027'::uuid);

-- ② 结转凭证唯一索引
DROP INDEX IF EXISTS uk_biz_voucher_year_end_period;

-- ③ 凭证类型白名单收回 AUTO|MANUAL|OPENING（安全阀已保证没有 YEAR_END 行）
ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_type;
ALTER TABLE biz_voucher
    ADD CONSTRAINT ck_biz_voucher_type
    CHECK (voucher_type IN ('AUTO', 'MANUAL', 'OPENING'));

-- ④ 期初来源列（约束跟着列一起 drop）
ALTER TABLE biz_opening_balance DROP CONSTRAINT IF EXISTS ck_biz_opening_source;
ALTER TABLE biz_opening_balance DROP COLUMN IF EXISTS source;
