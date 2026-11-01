-- ======================================================
-- V0183 回滚：资产负债表（ERP-R1）
--
-- 顺序：权限码 → 报表项 → 报表项表 → 本迁移新种的科目
--
-- 【硬约束】只删**本迁移种的那 49 个**科目，且仅当它们既没有下级、也没有任何分录：
--   `192.168.x` 一条都不碰（7 个既有科目）。
--   若某个新科目已被业务用上（有 biz_voucher_entry 引用），**保留行并把 remark 标注回来**，
--   绝不级联删账 —— 宁可留一个空科目，也不能让历史凭证失去科目。
-- ======================================================

-- ---------- ① 权限码 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001015'::uuid,
                   '20000000-0000-0000-0000-000000001016'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001016'::uuid,
              '20000000-0000-0000-0000-000000001015'::uuid);

-- ---------- ② 报表项 ----------
DELETE FROM biz_report_item WHERE report_code = 'BALANCE_SHEET';

DROP TABLE IF EXISTS biz_report_item;

-- ---------- ③ 本迁移新种的科目（有引用则保留）----------
DO $$
DECLARE
    kept integer;
BEGIN
    DELETE FROM biz_account a
     WHERE a.account_code IN (
            '1001', '1012', '1121', '1122', '1123', '1131', '1132', '1221',
            '1401', '1402', '1403', '1404', '1408',
            '1601', '1602', '1604', '1605', '1606', '1701', '1702', '1801',
            '2101', '2203', '2211', '2221.02', '2231', '2241', '2401', '2501', '2701',
            '3001', '3101', '3102', '3103', '3104',
            '4001', '4101',
            '5001', '5051', '5111', '5301', '5401', '5402', '5403',
            '5601', '5602', '5603', '5711', '5801'
           )
       AND a.remark = 'V0183 标准科目（小企业会计制度核心科目，ERP-R1 补种）'
       AND NOT EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = a.id)
       AND NOT EXISTS (SELECT 1 FROM biz_voucher_entry e WHERE e.account_id = a.id);

    SELECT count(*) INTO kept
      FROM biz_account
     WHERE deleted_at IS NULL
       AND account_code IN ('1001', '1602', '3001', '5801');
    IF kept <> 0 THEN
        RAISE NOTICE 'V0183 回滚：仍有 % 个新科目因被凭证/下级引用而保留（保留行优先于删账）', kept;
    END IF;
END $$;
