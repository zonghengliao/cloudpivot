-- ============================================================================
-- V0384 down：撤销「工资 9 科目 + 父科目翻非末级 + 2 条凭证规则」
--
-- 只回滚本迁移动过的东西：**不删凭证、不删单据、不动金额**。
--   ① 数据护栏 A：本迁移种的末级科目上有历史分录 ⇒ 拒绝回滚
--      （删科目会让分录指向不存在的科目，报表/明细账直接报「科目不存在」）；
--   ② 数据护栏 B：回滚会把库弄成裂树 / 删父留子 ⇒ 拒绝回滚
--      （父科目已是裂树，或本迁移种的科目下又被加了子科目）；
--   ③ 删 2 条凭证规则（`biz_voucher` 不存 rule_id，凭证里是科目/金额快照 ⇒ 无外键残留）；
--   ④ 删本迁移种的科目；
--   ⑤ 父科目**仅在已无任何未删子科目时**才翻回 `is_leaf = 1`；
--   ⑥ 反向自检：编码全无 / 规则全无 / 无裂树 / 无孤立分录。
--
-- # 顺序不能反
--   ④ 必须在 ⑤ 之前：父科目还挂着子科目时把它设成 `is_leaf = 1`，立刻违反
--   「末级不许有子」的口径（up 的自检块 ④ 查的就是这个）。
--
-- # ★ 与设计 §7.3 的唯一差异（有意为之，理由写在这里免得下一个人以为写错了）
--   设计 §7.3 写「`down` 删 9 个新科目；父科目 `is_leaf` 不回翻」。
--   实现上是两条收窄：
--   (a) **`2211.01` 通常不归本迁移**：V0377（成本核算，版本号更小、必然先跑）已经种了
--       `2211.01`（`remark` = 'ARCH-9 成本核算升级 D-14 播种：成本末级科目（V0377）'）。
--       本迁移的 INSERT 走 `ON CONFLICT DO NOTHING` 复用它。只回滚 V0384（V0377 仍处于
--       已应用状态）时把它一起删掉，会让成本结算的人工计提凭证（贷 `2211.01`，见
--       `cost_settle_voucher::ACCOUNT_PAYROLL`）失去科目 ⇒ 回滚反而造出一个更强的故障。
--       所以：`2211.01` **只在它确由本迁移种下（`remark` 是本迁移的标记）时才删**，
--       否则保留并 `RAISE NOTICE` 点名。其余 8 个编码只有本迁移种，无条件删（带分录护栏）。
--   (b) **父科目「确实已无子科目」时翻回末级**：设计担心「有历史分录风险」，但把父翻回
--       末级只在「它没有任何末级子科目」时才做 —— 此时它本来就是该记账的科目，
--       翻回末级是**恢复原状**（不是放宽）。不这么做，只回滚 V0384 会在老库里留下
--       「既无子科目、又不能记账」的空壳科目（`2241` / `5602` 在未跑过 V0226 的老库里
--       就是这种情况，§7.5 实测过）。有子科目时（含 V0377 的 `2211.01`）一律保持非末级，
--       与设计口径一致。
--   (c) **`aux_dimensions = 'DEPT'` 不回滚**：保留的 `2211.01` 上还留着本迁移补写的部门维度。
--       这是良性的：DEPT 只影响**手工凭证**写入时的必填校验（`voucher_write::check_aux_dimensions`
--       仅被 `manual_in_tx` 调用），而「工资类科目按部门记账」正是 §4.6.2 规则 3 要的口径；
--       且 down 无法区分「本迁移补的」与「客户本来配的」，回滚时清掉反而可能抹掉客户配置。
--
-- # 幂等
--   DELETE / UPDATE 都带条件（行已删、父已翻回 ⇒ 第二遍 0 行），
--   护栏读的 `biz_voucher_entry` / `biz_account` 两张表一直存在，第二遍自然不会命中。
-- ============================================================================

-- ---------- ① 数据护栏 A：待删科目上有历史分录就拒绝 ----------
DO $$
DECLARE
    v_blocked TEXT;
BEGIN
    IF to_regclass('public.biz_voucher_entry') IS NULL THEN
        RAISE NOTICE 'V0384 down：biz_voucher_entry 不存在（凭证模块已回滚？）—— 跳过分录护栏';
    ELSE
        SELECT string_agg(format('%s 有 %s 条', x.account_code, x.n), ' / ') INTO v_blocked
          FROM (SELECT e.account_code, count(*) AS n
                  FROM biz_voucher_entry e
                 WHERE e.account_code IN ('2211.01', '2211.02', '2211.03',
                                          '2221.03',
                                          '2241.03', '2241.04',
                                          '5602.10', '5602.11', '5602.12')
                 GROUP BY e.account_code) x;
        IF v_blocked IS NOT NULL THEN
            RAISE EXCEPTION 'V0384 down 拒绝执行：末级科目上还有历史分录 —— %。'
                '删科目会让这些分录指向不存在的科目（报表/明细账会报「科目不存在」）。'
                '请先红冲或作废这些凭证后再回滚', v_blocked;
        END IF;
    END IF;
END
$$;

-- ---------- ② 数据护栏 B：不许在「裂树」或「删父留子」的库上继续回滚 ----------
--
--   ★ 修正一次设计直觉（第一版按 V0377 抄「父科目下有本迁移之外的子科目就拒绝」，
--     结果在真实库上直接把合法回滚拦死）：`2221` / `2241` / `5602` 下本来就有
--     V0226/V0311/V0149 种的子科目（`2221.01/02`、`2241.01/02`、`5602.01~09`），
--     它们**不是**本迁移种的、也**必须**保留 —— 一并拒绝等于「装过 HR 就再也回滚不了」。
--   真正会造裂树的只有两种情形，逐条拦：
--     (a) 4 个父科目里有人**当前 `is_leaf = 1` 却挂着子科目** —— 回滚前就是裂树
--         （说明 up 之后有人绕过闸改过库），这种库不该被继续改动；
--     (b) 本迁移的 9 个编码**自己挂上了子科目** —— 删它们会把子科目挂到不存在的父上
--         （客户在 `2211.01` 下再分层时才会发生）。
--   其余情况安全：删的都是末级科目（无子），父科目「有子就保持非末级、没子才翻回末级」
--   （步骤 ⑤ 的 `NOT EXISTS` 条件），两侧都不会裂。
DO $$
DECLARE
    v_cracked TEXT;
    v_orphan  TEXT;
BEGIN
    -- (a) 父科目裂树：is_leaf = 1 却挂着未删子科目
    SELECT string_agg(format('%s(挂了 %s 个子科目却仍是末级)', p.account_code, x.n), ' / ')
      INTO v_cracked
      FROM biz_account p
      JOIN LATERAL (SELECT count(*) AS n
                      FROM biz_account c
                     WHERE c.parent_id = p.id AND c.deleted_at IS NULL) x ON x.n > 0
     WHERE p.account_code IN ('2211', '2221', '2241', '5602')
       AND p.deleted_at IS NULL
       AND p.is_leaf = 1;
    IF v_cracked IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 down 拒绝执行：父科目已是裂树 —— %。'
            '记账闸会把它当可记账、报表又把它当汇总（同一笔钱两头算）。'
            '请先在「会计科目」里把这些父科目改成非末级（is_leaf = 0）再回滚', v_cracked;
    END IF;

    -- (b) 本迁移的科目被客户加了子科目：删父会留孤儿
    SELECT string_agg(format('%s→%s', p.account_code, c.account_code), ' / ') INTO v_orphan
      FROM biz_account c
      JOIN biz_account p ON p.id = c.parent_id
     WHERE p.account_code IN ('2211.01', '2211.02', '2211.03',
                              '2221.03',
                              '2241.03', '2241.04',
                              '5602.10', '5602.11', '5602.12')
       AND p.deleted_at IS NULL
       AND c.deleted_at IS NULL;
    IF v_orphan IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 down 拒绝执行：本迁移种的科目下又被加了下级 —— %。'
            '删父科目会把这些子科目挂到不存在的父上（科目树报「父级不存在」）。'
            '请先手工把下级科目迁到别处（或删除）后再回滚', v_orphan;
    END IF;
END
$$;

-- ---------- ③ 删 2 条凭证规则（口径登记，无外键引用） ----------
DELETE FROM biz_voucher_rule
 WHERE rule_code IN ('PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_PAYMENT_DEFAULT');

-- ---------- ④ 删本迁移种的末级科目 ----------
--   8 个编码只有本迁移种 ⇒ 无条件删（护栏 A 已保证无分录）；
--   2211.01 归 V0377（版本更小、必然先跑）⇒ 只删「确由本迁移种下」的那一行。
DELETE FROM biz_account
 WHERE deleted_at IS NULL
   AND (account_code IN ('2211.02', '2211.03',
                         '2221.03',
                         '2241.03', '2241.04',
                         '5602.10', '5602.11', '5602.12')
        OR (account_code = '2211.01' AND remark = 'ARCH-9-2 工资相关科目'));

DO $$
DECLARE
    v_kept TEXT;
BEGIN
    SELECT string_agg(format('%s(归属：%s)', a.account_code, coalesce(a.remark, '<无 remark>')), ' / ')
      INTO v_kept
      FROM biz_account a
     WHERE a.account_code = '2211.01' AND a.deleted_at IS NULL;
    IF v_kept IS NOT NULL THEN
        RAISE NOTICE 'V0384 down 提示：% 保留未删 —— 它不是本迁移种的行（V0377/更早的卡种的），'
            '删了会让成本结算的人工计提凭证（贷 2211.01）失去科目。若确实要连它一起撤，'
            '请按版本序回滚到 V0377 之前', v_kept;
    END IF;
END
$$;

-- ---------- ⑤ 父科目翻回末级（仅当下面确实一个未删子科目都没有了） ----------
UPDATE biz_account p
   SET is_leaf    = 1,
       updated_at = CURRENT_TIMESTAMP
 WHERE p.account_code IN ('2211', '2221', '2241', '5602')
   AND p.deleted_at IS NULL
   AND p.is_leaf = 0
   AND NOT EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id AND c.deleted_at IS NULL);

-- ---------- ⑥ 反向自检 ----------
DO $$
DECLARE
    v_left     BIGINT;
    v_rules    BIGINT;
    v_cracked  BIGINT;
    v_entries  BIGINT;
    v_parent   TEXT;
BEGIN
    -- ① 8 个只归本迁移的编码必须全没了
    SELECT count(*) INTO v_left
      FROM biz_account
     WHERE account_code IN ('2211.02', '2211.03',
                            '2221.03',
                            '2241.03', '2241.04',
                            '5602.10', '5602.11', '5602.12');
    IF v_left > 0 THEN
        RAISE EXCEPTION 'V0384 down 自检失败：还有 % 个末级科目没删掉', v_left;
    END IF;

    -- ② 2 条规则必须没了（软删行不算：本迁移从不软删规则）
    SELECT count(*) INTO v_rules
      FROM biz_voucher_rule
     WHERE rule_code IN ('PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_PAYMENT_DEFAULT')
       AND deleted_at IS NULL;
    IF v_rules > 0 THEN
        RAISE EXCEPTION 'V0384 down 自检失败：还有 % 条工资凭证规则没删掉', v_rules;
    END IF;

    -- ③ 全表不变量：不许有「末级科目挂子科目」
    SELECT count(*) INTO v_cracked
      FROM biz_account p
      JOIN biz_account c ON c.parent_id = p.id AND c.deleted_at IS NULL
     WHERE p.deleted_at IS NULL
       AND p.is_leaf = 1;
    IF v_cracked > 0 THEN
        RAISE EXCEPTION 'V0384 down 自检失败：回滚后有 % 个裂树节点（末级科目挂着子科目）', v_cracked;
    END IF;

    -- ④ 不许留下孤立分录（护栏 A 已拦，这里兜底：真留下了就是数据损坏）
    IF to_regclass('public.biz_voucher_entry') IS NOT NULL THEN
        SELECT count(*) INTO v_entries
          FROM biz_voucher_entry
         WHERE account_code IN ('2211.02', '2211.03',
                                '2221.03',
                                '2241.03', '2241.04',
                                '5602.10', '5602.11', '5602.12');
        IF v_entries > 0 THEN
            RAISE EXCEPTION 'V0384 down 自检失败：% 条分录指向已删科目（护栏 A 漏了）', v_entries;
        END IF;
    END IF;

    -- ⑤ 收口提示：4 个父科目的最终末级位（有子科目的一定还在非末级，别当成回滚失败）
    SELECT string_agg(format('%s(is_leaf=%s)', a.account_code, a.is_leaf), ' / '
                      ORDER BY a.account_code)
      INTO v_parent
      FROM biz_account a
     WHERE a.account_code IN ('2211', '2221', '2241', '5602')
       AND a.deleted_at IS NULL;

    RAISE NOTICE 'V0384 down 完成：8 个末级科目已删（2211.01 视归属保留）、2 条规则已删、'
        '无裂树、无孤立分录；父科目最终末级位：%', coalesce(v_parent, '<无>');
END
$$;
