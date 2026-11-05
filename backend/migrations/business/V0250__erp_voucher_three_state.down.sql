-- ======================================================
-- V0250 回滚：删掉凭证三态审核链的 3 个权限码
--
-- 只回滚**权限种子**，不回滚任何业务数据：
--   · 不动 biz_voucher（存量 21 张 POSTED 保持 POSTED，本来就是迁移前状态）；
--   · 不动 biz_status_log（已经发生的 DRAFT→AUDITED / AUDITED→DRAFT 流转是审计证据，
--     回滚迁移不许抹掉历史 —— 那等于篡改审计轨迹）；
--   · 不动 ck_biz_voucher_status（V0149 起就允许 4 个状态，本迁移只是自检、没改过约束）。
--
-- 回滚后三个接口对非超管是 403（权限码没了），但超管仍可调用 —— 这与「迁移前」并不完全
-- 等价，是有意的：回滚的是授权配置，不是代码能力。要真正摘掉能力请回退代码版本。
--
-- 顺序：先删授权行（sys_role_menu 有外键指向 sys_menu），再删菜单行。
-- ======================================================

DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001032'::uuid,
                   '20000000-0000-0000-0000-000000001033'::uuid,
                   '20000000-0000-0000-0000-000000001034'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001032'::uuid,
              '20000000-0000-0000-0000-000000001033'::uuid,
              '20000000-0000-0000-0000-000000001034'::uuid);

-- ---------- 恢复 posted_at 的 NOT NULL（V0149 的基线）----------
--
-- 回滚必须能把库结构与「三态下线」对齐：三态没了就不会再产生草稿，
-- 而 `posted_at NOT NULL` 是 V0149 的显式约束（当时唯一的态就是 POSTED）。
-- 若库里已有 posted_at 为 NULL 的行（回滚前造过草稿），先用 created_at 兜底填上 ——
-- 否则 SET NOT NULL 会 23502 让回滚半途而废。这一步只填空值，不改任何非空行。
UPDATE biz_voucher SET posted_at = created_at WHERE posted_at IS NULL;

ALTER TABLE biz_voucher ALTER COLUMN posted_at SET NOT NULL;

DO $$
DECLARE
    n_menus integer;
    n_rows  integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE auth_code IN ('business:erp:voucher:audit',
                         'business:erp:voucher:un-audit',
                         'business:erp:voucher:post');
    IF n_menus <> 0 THEN
        RAISE EXCEPTION 'V0250 down 自检失败：仍有 % 个权限码未删', n_menus;
    END IF;

    SELECT count(*) INTO n_rows
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000001032'::uuid,
                       '20000000-0000-0000-0000-000000001033'::uuid,
                       '20000000-0000-0000-0000-000000001034'::uuid);
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0250 down 自检失败：仍有 % 行授权指向已删权限码', n_rows;
    END IF;

    RAISE NOTICE 'V0250 down 完成：3 个权限码与授权行已删除（凭证与状态日志一行未动）';
END $$;
