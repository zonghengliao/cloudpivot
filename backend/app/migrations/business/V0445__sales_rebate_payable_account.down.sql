-- ============================================================================
-- V0445 down：撤掉「其他应付款-销售返利」末级科目 2241.05
--
-- 【护栏：有凭证分录用过它就不删】
--   删掉科目会让历史凭证分录的 `account_id` 指空（`biz_voucher_entry` 还有
--   `account_code / account_name` 快照，但按 id 取实时名的路径会 500）。
--   真有返利凭证落在 2241.05 上，说明业务已经在用「返利负债」这个口径 —— 回滚迁移
--   不等于业务没发生过。宁可拒绝，让人先红冲 / 调账再回来 down。
--
-- 【只删自己种的那一行】
--   认领标记是 `remark LIKE 'V0445%'` **且** 名称逐字相符：客户手工建的同码科目
--   （名称不同）不会被这条 down 误删。
--
-- 【幂等】所有语句都带 IF EXISTS / WHERE，第二次执行 down 不报错。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 护栏：2241.05 已被凭证分录使用 ⇒ 拒绝回滚
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n bigint;
BEGIN
    IF to_regclass('public.biz_account') IS NULL THEN
        RAISE NOTICE 'V0445 down：biz_account 不存在，跳过';
        RETURN;
    END IF;

    IF EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.05')
       AND to_regclass('public.biz_voucher_entry') IS NOT NULL THEN
        EXECUTE $q$
            SELECT count(*)
              FROM biz_voucher_entry e
              JOIN biz_account a ON a.id = e.account_id
             WHERE a.account_code = '2241.05'
        $q$ INTO n;

        IF n > 0 THEN
            RAISE EXCEPTION 'V0445 down 拒绝回滚：科目 2241.05 已被 % 条凭证分录使用，'
                '删掉会让历史凭证指空。请先红冲 / 调整这些凭证，再回滚本迁移', n;
        END IF;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ② 删科目（只认领本迁移种的那一行）
-- ---------------------------------------------------------------------------
DELETE FROM biz_account
 WHERE account_code = '2241.05'
   AND account_name = '其他应付款-销售返利'
   AND remark LIKE 'V0445%';

-- ---------------------------------------------------------------------------
-- ③ 自检：科目没了、佣金科目与 2241 的末级位都没被带走
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n       integer;
    v_name  text;
    v_leaf  integer;
BEGIN
    SELECT count(*) INTO n
      FROM biz_account
     WHERE account_code = '2241.05' AND deleted_at IS NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0445 down 失败：科目 2241.05 仍有 % 条存活行（remark 不是 V0445%% 的'
            '手工科目不会被本迁移删除，请人工确认）', n;
    END IF;

    SELECT account_name INTO v_name
      FROM biz_account
     WHERE account_code = '2241.02' AND deleted_at IS NULL;
    IF v_name IS DISTINCT FROM '其他应付款-佣金' THEN
        RAISE EXCEPTION 'V0445 down 失败：佣金科目 2241.02 名称是「%」，应为「其他应付款-佣金」',
            coalesce(v_name, '(不存在)');
    END IF;

    SELECT is_leaf INTO v_leaf
      FROM biz_account
     WHERE account_code = '2241' AND deleted_at IS NULL;
    IF v_leaf IS DISTINCT FROM 0 THEN
        RAISE EXCEPTION 'V0445 down 失败：父科目 2241 的末级位是 %（它挂着 2241.01~.04 子科目，应为 0）',
            coalesce(v_leaf::text, '(不存在)');
    END IF;

    RAISE NOTICE 'V0445 down 完成：2241.05 其他应付款-销售返利 已移除；2241.02 佣金 / 2241 汇总科目完好';
END $$;
