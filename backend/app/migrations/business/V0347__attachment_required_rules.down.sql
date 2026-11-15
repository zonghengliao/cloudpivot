-- ============================================================
-- V0347 回滚：附件必传规则表 + 附件类型 CHECK 收回 7 值（ATT-06）
--
-- 顺序：
--   ① 已有 5 类新单据的真实附件时 **主动 RAISE**（回滚不许静默丢数据）
--   ② 删规则表（含索引；表就是本迁移的全部产物）
--   ③ ck_biz_doc_attachment_type 收回 V0244 的 7 值
--
-- 幂等：DROP TABLE / INDEX IF EXISTS、DROP CONSTRAINT IF EXISTS。
-- ============================================================

DO $$
DECLARE
    n int;
BEGIN
    SELECT count(*) INTO n
      FROM biz_doc_attachment
     WHERE doc_type IN ('INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN');
    IF n > 0 THEN
        RAISE EXCEPTION
            'V0347 回滚被拒绝：biz_doc_attachment 还有 % 条新类型（INVOICE/RECEIPT/STOCK_TAKE/PURCHASE_RETURN/SALES_RETURN）附件，请先处理这些绑定再回滚', n;
    END IF;
END $$;

DROP TABLE IF EXISTS biz_attachment_rule;

ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
ALTER TABLE biz_doc_attachment
    ADD CONSTRAINT ck_biz_doc_attachment_type
    CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                        'FIXED_ASSET', 'EXPENSE_REPORT'));

COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT / FIXED_ASSET / EXPENSE_REPORT（与 attachment::model::DocKind 一一对应；V0347 回滚后为 7 值）';
