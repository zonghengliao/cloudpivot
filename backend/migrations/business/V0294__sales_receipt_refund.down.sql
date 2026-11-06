-- ======================================================
-- V0294 down：撤销收款退款单两张表 + RR 取号规则
--
-- # 为什么「有已过账退款」时主动拒绝
--
-- `biz_receipt_refund.voucher_id` / `biz_voucher.source_id` 是**软引用**（无外键），
-- 直接 DROP TABLE 会留下「凭证说它是某张退款的过账凭证，而退款单不见了」的悬空，
-- 巡检（DI-17 凭证来源悬空）会红且无法在本迁移里自动修复。
-- 所以：库里有 `status = 'POSTED'` 的退款行 → 明确拒绝并给出人读的处置口径
-- （先在业务层处理这批退款，或单独写数据修复脚本），**不静默删账**。
-- 存在 DRAFT / CONFIRMED / CANCELLED 行时不阻塞（它们没有凭证，删表不产生悬空）。
-- ======================================================

DO $$
DECLARE
    posted INT;
BEGIN
    IF to_regclass('public.biz_receipt_refund') IS NOT NULL THEN
        SELECT count(*) INTO posted FROM biz_receipt_refund WHERE status = 'POSTED';
        IF posted > 0 THEN
            RAISE EXCEPTION
                'V0294 down 被拒绝：库里有 % 张已过账（POSTED）的收款退款单，'
                '它们各自挂着凭证（biz_receipt_refund.voucher_id → biz_voucher）。'
                '直接删表会让那些凭证的来源变成悬空（DI-17 红）。请先对这批退款做业务处置'
                '（或在红字凭证层冲销）并单独留档，再重跑本 down。', posted;
        END IF;
    END IF;
END $$;

-- 明细先删（对 biz_receivable 的 RESTRICT 也只在这张表上；DROP TABLE 会自动带走索引）
DROP TABLE IF EXISTS biz_receipt_refund_allocation;
DROP TABLE IF EXISTS biz_receipt_refund;

-- 取号规则行（biz_seq 是全局字典：只删本迁移种下的 RR 一行）
DELETE FROM biz_seq WHERE seq_type = 'RR';
