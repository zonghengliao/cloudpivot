-- ======================================================
-- V0290 回滚：应收单去掉 CANCELLED 状态（SO-14 遗留 · t_b33adc45）
--
-- ⚠ 回滚前提：库里**不能有** CANCELLED 的应收行 —— 4 值域约束会把它们顶成 23514。
--   这里显式检查并给出可读的失败原因（不是闷声炸约束），让人先决定怎么处理那批单据
--   （典型做法：确认出库单作废与三本账回滚无误后，按数据修复脚本软删或改回 OPEN）。
-- ======================================================

DO $$
DECLARE
    n BIGINT;
BEGIN
    SELECT count(*) INTO n FROM biz_receivable WHERE status = 'CANCELLED';
    IF n > 0 THEN
        RAISE EXCEPTION
            'V0290 回滚被拒：库里还有 % 张 status=CANCELLED 的应收单。'
            '请先决定这批单据的去向（软删 / 改回 OPEN），再回滚本迁移', n;
    END IF;
END $$;

ALTER TABLE biz_receivable DROP CONSTRAINT IF EXISTS ck_biz_ar_cancelled;

ALTER TABLE biz_receivable DROP CONSTRAINT IF EXISTS ck_biz_ar_status;
ALTER TABLE biz_receivable
    ADD CONSTRAINT ck_biz_ar_status
    CHECK (status IN ('OPEN', 'PARTIAL', 'SETTLED', 'WRITTEN_OFF'));

COMMENT ON COLUMN biz_receivable.status IS
    '状态机：OPEN/PARTIAL/SETTLED/WRITTEN_OFF（ARCH-7 §4.3；payable 的 PARTIAL_PAID 对应本表 PARTIAL）';
COMMENT ON COLUMN biz_receivable.settled_at IS
    '结清 / 坏账核销时间（SETTLED、WRITTEN_OFF 时落值，由 SO-13 写入）';
