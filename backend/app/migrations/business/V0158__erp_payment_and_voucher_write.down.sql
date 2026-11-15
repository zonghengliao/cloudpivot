-- ============================================================
-- V0158 down：撤销「应付付款 + 凭证红冲」的建表与权限码（批次 2 · P7）
--
-- 顺序：先撤授权 → 再撤权限节点 → 最后撤表。
-- 补种的科目 1002 只有在**本迁移之后没有被任何凭证引用**时才删（有引用就保留并 NOTICE）：
-- 凭证分录是会计档案，删科目会让历史凭证的分录指向不存在的科目。
-- ============================================================

-- ---------- ① 授权 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001011'::uuid,
                   '20000000-0000-0000-0000-000000001012'::uuid,
                   '20000000-0000-0000-0000-000000001013'::uuid,
                   '20000000-0000-0000-0000-000000001014'::uuid);

-- ---------- ② 权限节点 ----------
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001011'::uuid,
              '20000000-0000-0000-0000-000000001012'::uuid,
              '20000000-0000-0000-0000-000000001013'::uuid,
              '20000000-0000-0000-0000-000000001014'::uuid);

-- ---------- ③ 付款记录表 ----------
DROP INDEX IF EXISTS idx_biz_payment_voucher;
DROP INDEX IF EXISTS idx_biz_payment_payable;
DROP INDEX IF EXISTS uk_biz_payment_no;
DROP TABLE IF EXISTS biz_payment;

-- ---------- ④ 补种的科目 1002（有凭证引用则保留） ----------
DO $$
DECLARE
    n_refs integer;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '1002') THEN
        RAISE NOTICE 'V0158 down：科目 1002 不存在，无需处理';
        RETURN;
    END IF;

    SELECT count(*) INTO n_refs
      FROM biz_voucher_entry e
      JOIN biz_account a ON a.id = e.account_id
     WHERE a.account_code = '1002';

    IF n_refs > 0 THEN
        RAISE NOTICE 'V0158 down：科目 1002 已被 % 条凭证分录引用，保留（会计档案不能指向空科目）', n_refs;
        RETURN;
    END IF;

    DELETE FROM biz_account WHERE account_code = '1002' AND remark LIKE 'P7 付款凭证贷方科目%';
    RAISE NOTICE 'V0158 down：已删除本迁移补种的科目 1002';
END $$;
