-- V0149 down：删除凭证三表 + 回收本迁移补种的科目（只回收自己种的）
--
-- 顺序：分录 → 规则 → 凭证（分录有 FK ON DELETE CASCADE，但显式按序删更清楚）。
-- 科目回收只针对 `2221` / `2221.01` 两条种子，且**仅当没有分录引用它们**时
-- （引用即说明已经记过账，删科目会让历史凭证的 account_id 悬空）。
DO $$
DECLARE
    n_entry  BIGINT;
    n_vouch  BIGINT;
    n_ref    BIGINT;
BEGIN
    IF to_regclass('public.biz_voucher_entry') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_voucher_entry' INTO n_entry;
    END IF;
    IF to_regclass('public.biz_voucher') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_voucher' INTO n_vouch;
    END IF;
    RAISE NOTICE 'V0149 down：凭证 % 张 / 分录 % 行', COALESCE(n_vouch, 0), COALESCE(n_entry, 0);
END $$;

DROP TABLE IF EXISTS biz_voucher_entry;
DROP TABLE IF EXISTS biz_voucher_rule;
DROP TABLE IF EXISTS biz_voucher;

-- 回收科目种子：先探引用（分录表已删，这里探的是其它可能的引用面：科目自身子节点）
DO $$
DECLARE
    n_ref BIGINT;
BEGIN
    SELECT count(*) INTO n_ref
      FROM biz_account
     WHERE parent_id = (SELECT id FROM biz_account WHERE account_code = '2221' AND deleted_at IS NULL)
       AND account_code <> '2221.01';
    IF n_ref > 0 THEN
        RAISE NOTICE 'V0149 down：2221 下还有 % 个非本迁移创建的子科目，保留科目种子', n_ref;
        RETURN;
    END IF;

    DELETE FROM biz_account
     WHERE account_code IN ('2221.01', '2221')
       AND deleted_at IS NULL;
    RAISE NOTICE 'V0149 down：科目种子 2221 / 2221.01 已回收';
END $$;

DO $$
BEGIN
    IF to_regclass('public.biz_voucher') IS NOT NULL
       OR to_regclass('public.biz_voucher_entry') IS NOT NULL
       OR to_regclass('public.biz_voucher_rule') IS NOT NULL THEN
        RAISE EXCEPTION 'V0149 down 失败：凭证三表仍有残留';
    END IF;
    RAISE NOTICE 'V0149 down 完成：凭证三表零残留';
END $$;
