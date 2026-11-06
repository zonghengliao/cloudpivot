-- V0311 回滚：撤销 SO-36 计提配套的库层改动。
--
-- ⚠ 基数列恢复 `>= 0` 前必须先清掉负基数行（退货冲回明细）；有存活数据时本迁移
--   会因 CHECK 校验失败而整体回滚 —— 这是**故意的**：宁可回滚失败，也不要静默
--   把冲回行改成正数（那会把「冲回」变成「再计一次」）。

-- ---------- ④ 索引 ----------
DROP INDEX IF EXISTS idx_biz_cmitem_settlement_status;

-- ---------- ③ 基数列恢复非负 ----------
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE conname = 'ck_biz_cmitem_base'
                  AND conrelid = 'public.biz_commission_item'::regclass) THEN
        ALTER TABLE biz_commission_item DROP CONSTRAINT ck_biz_cmitem_base;
    END IF;
    ALTER TABLE biz_commission_item
        ADD CONSTRAINT ck_biz_cmitem_base CHECK (base_amount >= 0);
END $$;

COMMENT ON COLUMN biz_commission_item.base_amount IS
    '佣金基数：该应收「已核销」金额（按收款计佣，不是开单金额）';

-- ---------- ② 号段 ----------
DELETE FROM biz_seq WHERE seq_type = 'CM';

-- ---------- ① 科目 ----------
-- 只在「没有任何凭证明细用过它」时才删，避免把总账引用打断。
DELETE FROM biz_account
 WHERE account_code = '2241.02'
   AND NOT EXISTS (SELECT 1 FROM biz_voucher_entry e WHERE e.account_code = '2241.02');
