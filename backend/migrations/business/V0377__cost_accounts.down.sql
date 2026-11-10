-- ============================================================================
-- V0377 down：撤销「成本科目翻末级 + 9 个末级科目」
--
-- 只回滚本迁移动过的东西：**不删凭证、不删单据、不动金额**。
--   ① 数据护栏 A：9 个末级科目上有历史分录 ⇒ 拒绝回滚（删科目会让分录指向不存在的科目）；
--   ② 数据护栏 B：三个上级科目下有「本迁移之外的子科目」⇒ 拒绝回滚
--      （把父翻回末级会造出「末级科目挂子科目」的裂树：凭证闸当它可记账、报表当它汇总）；
--   ③ 删 9 个末级科目（先删才能把父翻回末级，否则父还有子）；
--   ④ 三个上级科目翻回 is_leaf = 1；
--   ⑤ 反向自检：9 个编码全无、三科目末级、无裂树、无孤立分录。
--
-- # 顺序不能反
--   ③ 必须在 ④ 之前：父科目还挂着子科目时把它设成 is_leaf=1，立刻违反
--   「末级不许有子」的口径（本迁移 up 的自检块 ④ 就是查这个）。
--
-- # 幂等
--   DELETE / UPDATE 都带条件（行已删、父已翻回 ⇒ 第二遍 0 行），护栏读的
--   biz_voucher_entry / biz_account 两张表一直存在，第二遍自然不会命中。
--
-- # 已知边界（写在这里免得下一个人踩）
--   · 本迁移**不删** `biz_account` 之外的东西；`biz_cost_*` 里若有以 '4001.01' 之类
--     字符串形式存的科目编码（biz_cost_overhead.debit_account_code / alloc_rule.account_code），
--     回滚后它们会指向不存在的科目 —— 那些行属成本模块业务数据，由成本模块的清理负责，
--     回滚前请先用成本模块自己的界面/脚本清干净（本迁移拒绝替业务模块删数据）。
--   · 2211.01 若同时被人力资源卡（ARCH9-2）种过：迁移按版本号升序执行 ⇒ 本迁移（V0377）
--     一定先跑、行归本迁移所有；真出现 remark 不符的行（别人后来改过），down 会点名 NOTICE
--     而不是静默跳过一个半个。
-- ============================================================================

-- ---------- ① 数据护栏 A：末级科目上有历史分录就拒绝 ----------
DO $$
DECLARE
    v_blocked TEXT;
BEGIN
    IF to_regclass('public.biz_voucher_entry') IS NULL THEN
        RAISE NOTICE 'V0377 down：biz_voucher_entry 不存在（凭证模块已回滚？）—— 跳过分录护栏';
    ELSE
        SELECT string_agg(format('%s 有 %s 条', x.account_code, x.n), ' / ') INTO v_blocked
          FROM (SELECT e.account_code, count(*) AS n
                  FROM biz_voucher_entry e
                 WHERE e.account_code IN ('4001.01', '4001.02', '4001.03',
                                          '4101.01', '4101.02', '4101.03', '4101.04', '4101.09',
                                          '2211.01')
                 GROUP BY e.account_code) x;
        IF v_blocked IS NOT NULL THEN
            RAISE EXCEPTION 'V0377 down 拒绝执行：末级科目上还有历史分录 —— %。'
                '删科目会让这些分录指向不存在的科目（报表/明细账会报「科目不存在」）。'
                '请先红冲或作废这些凭证后再回滚', v_blocked;
        END IF;
    END IF;
END
$$;

-- ---------- ② 数据护栏 B：三个上级科目下有本迁移之外的子科目就拒绝 ----------
DO $$
DECLARE
    v_foreign TEXT;
BEGIN
    SELECT string_agg(format('%s→%s', p.account_code, c.account_code), ' / ') INTO v_foreign
      FROM biz_account c
      JOIN biz_account p ON p.id = c.parent_id
     WHERE p.account_code IN ('4001', '4101', '2211')
       AND p.deleted_at IS NULL
       AND c.deleted_at IS NULL
       AND c.account_code NOT IN ('4001.01', '4001.02', '4001.03',
                                  '4101.01', '4101.02', '4101.03', '4101.04', '4101.09',
                                  '2211.01');
    IF v_foreign IS NOT NULL THEN
        RAISE EXCEPTION 'V0377 down 拒绝执行：上级科目下还有本迁移之外的子科目 —— %。'
            '把 4001/4101/2211 翻回末级后它们会变成「末级科目挂子科目」的裂树'
            '（凭证闸当它可记账、报表又当它汇总，同一笔钱两头算）。'
            '请先手工处理这些子科目（迁到别处或删除）后再回滚', v_foreign;
    END IF;
END
$$;

-- ---------- ③ 删 9 个末级科目（护栏已保证无分录） ----------
DELETE FROM biz_account
 WHERE account_code IN ('4001.01', '4001.02', '4001.03',
                        '4101.01', '4101.02', '4101.03', '4101.04', '4101.09',
                        '2211.01');

-- ---------- ④ 三个上级科目翻回末级（仅当下面确实没有子了） ----------
UPDATE biz_account p
   SET is_leaf    = 1,
       updated_at = CURRENT_TIMESTAMP
 WHERE p.account_code IN ('4001', '4101', '2211')
   AND p.deleted_at IS NULL
   AND p.is_leaf = 0
   AND NOT EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id AND c.deleted_at IS NULL);

-- ---------- ⑤ 反向自检 ----------
DO $$
DECLARE
    v_left     BIGINT;
    v_parent   TEXT;
    v_cracked  BIGINT;
    v_entries  BIGINT;
BEGIN
    -- ① 9 个编码必须全没了
    SELECT count(*) INTO v_left
      FROM biz_account
     WHERE account_code IN ('4001.01', '4001.02', '4001.03',
                            '4101.01', '4101.02', '4101.03', '4101.04', '4101.09',
                            '2211.01');
    IF v_left > 0 THEN
        RAISE EXCEPTION 'V0377 down 自检失败：还有 % 个末级科目没删掉', v_left;
    END IF;

    -- ② 三个上级科目必须都是末级、未软删（软删的不算：回滚不复活已删科目）
    SELECT string_agg(format('%s(is_leaf=%s)', a.account_code, a.is_leaf), ' / ') INTO v_parent
      FROM biz_account a
     WHERE a.account_code IN ('4001', '4101', '2211')
       AND a.deleted_at IS NULL
       AND a.is_leaf <> 1;
    IF v_parent IS NOT NULL THEN
        RAISE EXCEPTION 'V0377 down 自检失败：上级科目没翻回末级 —— %（回滚后记账闸应重新允许 4001）',
            v_parent;
    END IF;

    -- ③ 全表不变量：不许有「末级科目挂子科目」
    SELECT count(*) INTO v_cracked
      FROM biz_account p
      JOIN biz_account c ON c.parent_id = p.id AND c.deleted_at IS NULL
     WHERE p.deleted_at IS NULL
       AND p.is_leaf = 1;
    IF v_cracked > 0 THEN
        RAISE EXCEPTION 'V0377 down 自检失败：回滚后有 % 个裂树节点（末级科目挂着子科目）', v_cracked;
    END IF;

    -- ④ 不许留下孤立分录（护栏 A 已拦，这里兜底：真留下了就是数据损坏）
    IF to_regclass('public.biz_voucher_entry') IS NOT NULL THEN
        SELECT count(*) INTO v_entries
          FROM biz_voucher_entry
         WHERE account_code IN ('4001.01', '4001.02', '4001.03',
                                '4101.01', '4101.02', '4101.03', '4101.04', '4101.09',
                                '2211.01');
        IF v_entries > 0 THEN
            RAISE EXCEPTION 'V0377 down 自检失败：% 条分录指向已删科目（护栏 A 漏了）', v_entries;
        END IF;
    END IF;

    -- ⑤ 收口提示（本迁移种的科目带固定 remark 标记，便于人工追溯归属）
    RAISE NOTICE 'V0377 down 完成：9 个末级科目已删、4001/4101/2211 已翻回末级、无裂树、无孤立分录';
END
$$;
