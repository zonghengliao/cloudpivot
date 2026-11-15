-- ============================================================================
-- V0380.down.sql —— 回滚存量批号回填（对应 V0380__cost_lot_backfill）
--
-- 只删**回填行**：source_doc_type='BACKFILL' AND source_type='MANUAL'。
-- 业务/客户自建的批次（source_doc_type 是 STOCK_IN / COST_PERIOD / 空）一行不碰。
--
-- 护栏与提示：
--   · 已被成本对象引用（biz_cost_object.lot_no）的回填批次：**仍删**，但打 WARNING 点名数量
--     —— 回填行只是「历史批号的索引」，不是业务事实来源；删掉等于回到"没有批次主数据"的旧状态。
--   · source_type 已被改成 PURCHASE/PRODUCTION 的行视为已人工确认过，**不删**（保护人工成果），
--     down 报告里点名数量。
--
-- 幂等：重复执行删 0 行，不报错。
-- ============================================================================

DO $$
DECLARE
    n_back      INT;
    n_kept      INT;
    n_referenced INT;
BEGIN
    SELECT count(*) INTO n_back
      FROM biz_lot WHERE source_doc_type = 'BACKFILL' AND source_type = 'MANUAL';
    SELECT count(*) INTO n_kept
      FROM biz_lot WHERE source_doc_type = 'BACKFILL' AND source_type <> 'MANUAL';

    IF n_back > 0 THEN
        SELECT count(*) INTO n_referenced
          FROM biz_lot l JOIN biz_cost_object o ON o.lot_no = l.lot_no
         WHERE l.source_doc_type = 'BACKFILL' AND l.source_type = 'MANUAL'
           AND o.deleted_at IS NULL;
        IF n_referenced > 0 THEN
            RAISE WARNING 'V0380.down：% 条回填批次已被成本对象引用（biz_cost_object.lot_no），'
                          '删除后这些成本对象只剩批号文本、没有批次主数据行', n_referenced;
        END IF;

        DELETE FROM biz_lot WHERE source_doc_type = 'BACKFILL' AND source_type = 'MANUAL';
    END IF;

    RAISE NOTICE 'V0380.down：已删除 % 条回填批次；保留 % 条已人工改过来源类型的回填行', n_back, n_kept;
END $$;
