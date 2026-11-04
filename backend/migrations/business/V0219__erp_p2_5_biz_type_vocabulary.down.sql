-- ============================================================================
-- V0219__erp_p2_5_biz_type_vocabulary.down.sql
--
-- 回滚：把调拨单的状态日志类型改回旧值 `TRANSFER`。
--
-- ⚠️ 回滚**不是严格无损**的：down 只能按「entity_id 属于 biz_stock_transfer」把行改回去，
--    如果 up **之前**库里就已经有 `entity_type='STOCK_TRANSFER'` 的行（例如别处手工写的），
--    up 之后它们与调拨行无法区分，down 会把它们也算进调拨。
--    生产库 up 之前的实测数字记在 `docs/evidence/20261104-p2-5-biz-type/README.md`
--    的「迁移前后对比」里（`.entity_type` 的 distinct 值 + 行数），回滚前请先对一遍。
--
-- 只回滚「本迁移改的东西」。ADJUST 没有数据改动，按代码回滚
-- （`doc_kind::TxnDocType` 去掉 Adjust）即可，不需要 SQL。
-- ============================================================================

UPDATE biz_status_log
   SET entity_type = 'TRANSFER'
 WHERE entity_type = 'STOCK_TRANSFER'
   AND entity_id IN (SELECT id FROM biz_stock_transfer);

DO $$
DECLARE
    left_over bigint;
BEGIN
    SELECT count(*) INTO left_over
      FROM biz_status_log l
     WHERE l.entity_type = 'STOCK_TRANSFER'
       AND l.entity_id IN (SELECT id FROM biz_stock_transfer);
    IF left_over <> 0 THEN
        RAISE EXCEPTION 'V0219 down 自检失败：仍有 % 行调拨日志没回到 TRANSFER', left_over;
    END IF;
    RAISE NOTICE 'V0219 down：调拨状态日志已回到 entity_type = ''TRANSFER''';
END $$;
