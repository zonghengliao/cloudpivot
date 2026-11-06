-- ======================================================
-- V0311：佣金计提 / 结算（SO-36，ARCH-7 §15.4.3）配套的库层改动
--
-- V0309 已把三张表建好（规则 / 结算单 / 明细），本迁移只补「计提真正跑起来」需要的四件事：
--
--   ① `2241.02 其他应付款-佣金` 末级科目 —— 计提凭证的贷方
--   ② `biz_seq(seq_type='CM')` 结算单取号规则（CM-yyyyMM-000001，按月滚动）
--   ③ 放宽 `ck_biz_cmitem_base`：佣金基数**允许为负**（退货 / 退款冲回）
--   ④ 佣金明细按「应收 → 已认领基数」的聚合查询索引（计提主路径）
--
-- ── ① 为什么是 2241.02 而不是卡面的 2241 ─────────────────────
-- 卡面（ARCH-7 §15.4.3 ④）写「贷 2241 其他应付款-佣金」，但 V0226 已把
-- `2241` 从末级翻成**非末级（仅汇总）**科目（它下面挂了 `2241.01 员工垫付`）。
-- `voucher::post_voucher` 有硬闸：分录落在 `is_leaf = 0` 的科目上直接拒绝
-- （见 `voucher::non_leaf_codes` 的「记账科目口径」）。所以按仓内既有做法
-- 种一个末级子科目 `2241.02 其他应付款-佣金`，计提凭证贷记它 —— 与
-- V0226 种 `2241.01`、V0229 种 `5602.01 折旧费` 同一口径。
--
-- ── ① 借方为什么是 5601 而不是卡面的 6601 ───────────────────
-- 库里**没有 66xx 科目**：本仓科目集是「小企业会计制度核心科目」
-- （V0183 种子，`select count(*) from biz_account where account_code like '66%'` = 0），
-- 销售费用 = `5601`（V0226 头注释、V0229 头注释都记过这笔账）。
-- 新种 6601 会让「销售费用」在利润表（`income_statement` 的 SELLING_EXPENSE = 5601）
-- 与预算科目树里凭空消失一份。故借方落 `5601 销售费用`（末级，直接用），
-- 摘要里写明「佣金计提」，报表按摘要 / 摘要+凭证来源可下钻。
-- 本迁移**不**把 5601 翻成非末级（那会连带改利润表与预算映射，属独立专项）。
--
-- ── ③ 为什么基数必须能是负数（与 V0309 的 `base_amount >= 0` 相反）───
-- V0309 的 CHECK 假设「明细行只表示正向计提」。SO-36 落地后必须表达
-- 「退货 / 收款退款 ⇒ 佣金按比例扣回」（ARCH-7 §15.4.3 ⑥）：
--
--   * 一张应收的「已认领基数」= 该应收在所有**存活**结算单里 `base_amount` 之和；
--   * 本期间可计佣基数 = 应收的 `received_amount`（已核销 = 真收到钱）**减去**已认领基数；
--   * 收到钱 → 差为正 → 正计提；发生退款 / 退货导致已收下降 → 差为负 → **负计提（冲回）**。
--
-- 若基数只能非负，「已认领基数」就记不住冲回，下一期会把同一笔钱再次冲回（越冲越错）。
-- 所以基数列改成**有符号**：正 = 本期新增已收款，负 = 本期冲回。`commission` 本来
-- 就允许负（V0309 头注释 5.8k 的探针行），保持不动。
-- 新 CHECK 只拦「0」：基数为 0 的明细行没有任何业务含义（既没计提也没冲回），
-- 落库只会让对账多出一堆噪音行。
--
-- ── 幂等 ──────────────────────────────────────────────
-- 科目 / 号段用 `WHERE NOT EXISTS` 守卫；约束改动用 DO 块做存在性判断后 DROP + ADD。
-- 文件内**不写** BEGIN/COMMIT（迁移器自己开事务）。
-- ======================================================

-- ---------- ① 贷方科目 2241.02 其他应付款-佣金 ----------
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, aux_dimensions, remark)
SELECT '2241.02', '其他应付款-佣金',
       (SELECT id FROM biz_account WHERE account_code = '2241' AND deleted_at IS NULL),
       2, 'LIABILITY', 'CREDIT', 1, 1, NULL,
       'SO-36：销售佣金计提贷记（借 5601 销售费用 / 贷 2241.02），'
       '发放时借本科目 / 贷 1002 银行存款。父科目 2241 由 V0226 置为非末级'
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.02')
   AND EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241' AND deleted_at IS NULL);

-- ---------- ② 结算单取号规则（CM-yyyyMM-000001） ----------
-- `date_pattern` 只是给配置页展示的声明；真正的周期键由 commission_model 按月生成
-- （`biz_seq.period_key` 存 `yyyyMM`，跨月才重置），与 RC/SH 的按天滚动是两种规则，
-- 故不复用 `voucher::next_doc_no`（它把周期键写死成 CST 天）。
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no,
                     step, separator, status, remark)
SELECT gen_random_uuid(), 'CM', '佣金结算单号', 'CM', 'yyyyMM', '', 0, 1, '-', 1,
       '佣金结算单号：CM-yyyyMM-000001（按月滚动；SO-36）'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'CM' AND deleted_at IS NULL);

-- ---------- ③ 基数允许为负（退货 / 退款冲回） ----------
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE conname = 'ck_biz_cmitem_base'
                  AND conrelid = 'public.biz_commission_item'::regclass) THEN
        ALTER TABLE biz_commission_item DROP CONSTRAINT ck_biz_cmitem_base;
    END IF;
    ALTER TABLE biz_commission_item
        ADD CONSTRAINT ck_biz_cmitem_base CHECK (base_amount <> 0);
END $$;

COMMENT ON COLUMN biz_commission_item.base_amount IS
    '佣金基数（**有符号**，V0311 起）：正 = 本期新增已核销（收到钱）金额；'
    '负 = 退货 / 收款退款导致已收下降的冲回金额；0 被 ck_biz_cmitem_base 拒绝。'
    '按收款计佣，不是开单金额（ARCH-7 §15.4.3）';
COMMENT ON COLUMN biz_commission_settlement.total_sales IS
    '期间佣金基数合计 = 已核销（收到钱）的应收金额增量之和（ARCH-7 §15.4.3）。'
    '纯冲回期间为负时按 0 落库（ck_biz_cmset_total_sales 要求非负），'
    '冲回金额体现在 total_commission（负）与明细负 commission 上';

-- ---------- ④ 计提主路径索引 ----------
-- 「某张应收已被认领多少基数」是每次计提的必经聚合：
--   SELECT receivable_id, SUM(base_amount) FROM biz_commission_item i
--     JOIN biz_commission_settlement t ON t.id = i.settlement_id
--    WHERE t.deleted_at IS NULL GROUP BY receivable_id
-- V0309 的 idx_biz_cmitem_receivable 是 `(receivable_id) WHERE receivable_id IS NOT NULL`，
-- 已能覆盖本聚合的索引扫描，故此处不重复建索引；
-- 唯一新增的是「按结算单状态扫明细」的辅助索引（重算 / 对账按状态筛台账时用）。
CREATE INDEX IF NOT EXISTS idx_biz_cmitem_settlement_status
    ON biz_commission_item (settlement_id, commission);

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    n      INT;
    d      TEXT;
    probe_settle UUID := 'd0311000-0000-4000-8000-000000000001'::uuid;
BEGIN
    -- 5.1 贷方科目：存在 + 末级 + 挂了 2241 父
    SELECT count(*) INTO n FROM biz_account
     WHERE account_code = '2241.02' AND deleted_at IS NULL
       AND is_leaf = 1 AND level = 2 AND balance_direction = 'CREDIT'
       AND account_category = 'LIABILITY'
       AND parent_id IS NOT DISTINCT FROM
           (SELECT id FROM biz_account WHERE account_code = '2241' AND deleted_at IS NULL);
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0311 自检失败：2241.02 其他应付款-佣金 未按末级子科目就位（命中 %）', n;
    END IF;

    -- 5.2 借方 5601 必须仍是可记账末级（本迁移刻意不动它）
    SELECT is_leaf::text INTO d FROM biz_account WHERE account_code = '5601' AND deleted_at IS NULL;
    IF d IS NULL OR d <> '1' THEN
        RAISE EXCEPTION 'V0311 自检失败：5601 销售费用不是末级科目（is_leaf=%），佣金计提借方无处落账', d;
    END IF;

    -- 5.3 号段
    SELECT count(*) INTO n FROM biz_seq
     WHERE seq_type = 'CM' AND prefix = 'CM' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0311 自检失败：biz_seq(seq_type=CM) 取号规则行缺失';
    END IF;

    -- 5.4 基数列约束：负值可插入、0 被拒
    INSERT INTO biz_commission_settlement (id, settlement_no, salesperson_id, period_code,
                                           total_sales, total_commission)
    SELECT probe_settle, '__V0311_PROBE__', u.id, '2099-01', 0, 0
      FROM sys_user u WHERE u.deleted_at IS NULL LIMIT 1;
    IF FOUND THEN
        INSERT INTO biz_commission_item (settlement_id, base_amount, rate, commission)
        VALUES (probe_settle, -100, 5, -5);
        SELECT base_amount::text INTO d FROM biz_commission_item WHERE settlement_id = probe_settle;
        IF d <> '-100.0000' THEN
            RAISE EXCEPTION 'V0311 自检失败：负基数落库 = %（期望 -100.0000）', d;
        END IF;
        BEGIN
            INSERT INTO biz_commission_item (settlement_id, base_amount, rate, commission)
            VALUES (probe_settle, 0, 5, 0);
            RAISE EXCEPTION 'V0311 自检失败：base_amount = 0 未被 ck_biz_cmitem_base 拒绝';
        EXCEPTION WHEN check_violation THEN NULL;
        END;
        DELETE FROM biz_commission_settlement WHERE id = probe_settle;
    ELSE
        RAISE NOTICE 'V0311 自检：库里没有 sys_user 行，跳过负基数探针';
    END IF;

    -- 5.5 清理探针
    DELETE FROM biz_commission_settlement WHERE settlement_no = '__V0311_PROBE__';
    SELECT count(*) INTO n FROM biz_commission_item i
      LEFT JOIN biz_commission_settlement t ON t.id = i.settlement_id
     WHERE t.id IS NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0311 自检失败：明细出现 % 行孤儿', n;
    END IF;

    RAISE NOTICE 'V0311 完成：2241.02 + biz_seq(CM) + 有符号基数列就位';
END $$;
