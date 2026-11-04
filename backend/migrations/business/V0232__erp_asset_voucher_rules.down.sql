-- ======================================================
-- V0232 down：删 4 条固定资产凭证规则；附件 CHECK 退回 5 值
--
-- 附件 CHECK 退回前必须先确认没有 FIXED_ASSET 的附件行（否则 ADD CONSTRAINT 会失败，
-- 报错信息也说不清是哪张单挂的）。自己先数、先 RAISE。
-- ======================================================
DO $$
DECLARE
    n int;
BEGIN
    SELECT count(*) INTO n FROM biz_doc_attachment WHERE doc_type = 'FIXED_ASSET';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0232 回滚被拒绝：还有 % 个资产附件（先把附件解绑或删掉再回滚）', n;
    END IF;
END $$;

ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
ALTER TABLE biz_doc_attachment
    ADD CONSTRAINT ck_biz_doc_attachment_type
    CHECK (doc_type IN ('CONTRACT','PURCHASE_ORDER','STOCK_IN','PAYABLE','PAYMENT'));

DELETE FROM biz_voucher_rule
 WHERE rule_code IN ('FIXED_ASSET_ACQ_DEFAULT','FIXED_ASSET_ACQ_CASH',
                     'FIXED_ASSET_ACQ_STOCK','DEPRECIATION_DEFAULT');
