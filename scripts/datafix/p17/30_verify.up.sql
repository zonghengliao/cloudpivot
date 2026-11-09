-- P17 验收门：孤儿库位账、孤儿 SERIAL_BATCH 凭证都必须为 0
-- 同时打印「存活批次」数与库存域关键计数，便于与 E2E-FIX-7 的基线对照。
\set ON_ERROR_STOP 1
\pset pager off

\echo '=== P17 验收 ==='
SELECT (SELECT count(*) FROM biz_inventory_location l
         WHERE NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
            OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id)) AS orphan_location_rows,
       (SELECT count(*) FROM biz_voucher v
         WHERE v.source_type = 'SERIAL_BATCH'
           AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = v.source_id)) AS orphan_batch_vouchers,
       (SELECT count(*) FROM biz_voucher v
         WHERE v.source_type = 'SERIAL_BATCH') AS serial_batch_vouchers_alive,
       (SELECT count(*) FROM biz_inventory_location) AS location_rows_total,
       (SELECT count(*) FROM biz_inventory)          AS cost_ledger_rows;

DO $$
DECLARE loc bigint; vch bigint; orphan_log bigint;
BEGIN
    SELECT count(*) INTO loc FROM biz_inventory_location l
     WHERE NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
        OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id);
    SELECT count(*) INTO vch FROM biz_voucher v
     WHERE v.source_type = 'SERIAL_BATCH'
       AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = v.source_id);
    SELECT count(*) INTO orphan_log FROM biz_status_log s
     WHERE s.entity_type = 'VOUCHER'
       AND NOT EXISTS (SELECT 1 FROM biz_voucher v WHERE v.id = s.entity_id);

    IF loc > 0 THEN
        RAISE EXCEPTION '验收失败：仍有 % 行孤儿库位账', loc;
    END IF;
    IF vch > 0 THEN
        RAISE EXCEPTION '验收失败：仍有 % 张无批次流水的 SERIAL_BATCH 凭证', vch;
    END IF;
    RAISE NOTICE 'P17 验收通过：孤儿库位账 0 / 孤儿批次凭证 0（存量 VOUCHER 状态日志孤儿 % 条，属别的卡，未在处理范围）', orphan_log;
END $$;

\echo '=== 1405/1901 余额（清理后，仅供与清账前对照）==='
SELECT e.account_code, sum(e.debit_amount)::numeric(18,4) AS debit, sum(e.credit_amount)::numeric(18,4) AS credit,
       sum(e.debit_amount - e.credit_amount)::numeric(18,4) AS balance
  FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
 WHERE v.status = 'POSTED' AND v.deleted_at IS NULL AND e.account_code IN ('1405', '1901')
 GROUP BY e.account_code ORDER BY e.account_code;

\echo '=== 库存成本账合计（应与 1405 借方可比；差异属 E2E-FIX-7 遗留口径，不在本卡范围）==='
SELECT coalesce(sum(amount), 0)::numeric(18,4) AS inv_amount FROM biz_inventory;
