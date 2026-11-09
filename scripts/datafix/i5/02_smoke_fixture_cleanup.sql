-- I-5 · 冒烟夹具清场（145 cloudpivot_dev_clean）
-- 只删前缀 I5-SMOKE 的夹具；按 FK 顺序子表→主表；清完必须回到「0 业务行」的空基线。
-- 幂等：无夹具时各 DELETE 均为 0。
\pset pager off
\set ON_ERROR_STOP on

CREATE TEMP TABLE i5_mat AS SELECT id FROM biz_material WHERE material_code LIKE 'I5-SMOKE%';
CREATE TEMP TABLE i5_wh  AS SELECT id FROM biz_warehouse WHERE warehouse_code LIKE 'I5-SMOKE%';
CREATE TEMP TABLE i5_ser AS SELECT id FROM biz_material_serial WHERE material_id IN (SELECT id FROM i5_mat);
CREATE TEMP TABLE i5_doc AS SELECT DISTINCT doc_id AS id FROM biz_inventory_txn
                            WHERE material_id IN (SELECT id FROM i5_mat) AND doc_id IS NOT NULL;
CREATE TEMP TABLE i5_vou AS
  SELECT v.id FROM biz_voucher v
   WHERE v.source_id IN (SELECT id FROM i5_ser)                      -- SERIAL_OUT / SERIAL_SCRAP：source_id = 序列号 id
      OR (v.source_type IN ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP')
          AND v.source_id IN (SELECT id FROM i5_doc));               -- SERIAL_BATCH：source_id = 流水 doc_id

\echo '===== 清场目标（只允许 I5-SMOKE 前缀）====='
SELECT (SELECT count(*) FROM i5_mat) AS materials,
       (SELECT count(*) FROM i5_wh)  AS warehouses,
       (SELECT count(*) FROM i5_ser) AS serials,
       (SELECT count(*) FROM i5_doc) AS doc_ids,
       (SELECT count(*) FROM i5_vou) AS vouchers;

-- 门禁：目标凭证不得有红冲/被反冲关系（有就整单回滚，先人工看）
DO $$
DECLARE bad int;
BEGIN
  SELECT count(*) INTO bad FROM biz_voucher
   WHERE id IN (SELECT id FROM i5_vou)
     AND (is_red <> 0 OR reversed_by IS NOT NULL OR reverse_of IS NOT NULL);
  IF bad > 0 THEN RAISE EXCEPTION '目标凭证存在红冲关系 % 条，停止清场', bad; END IF;
END $$;

\echo '===== 删除（子表→主表）====='
DELETE FROM biz_voucher_entry   WHERE voucher_id IN (SELECT id FROM i5_vou);
DELETE FROM biz_status_log      WHERE entity_id IN (SELECT id FROM i5_vou)
                                   OR entity_id IN (SELECT id FROM i5_ser)
                                   OR entity_id IN (SELECT id FROM i5_mat)
                                   OR entity_no LIKE 'I5-SMOKE%';
DELETE FROM biz_voucher         WHERE id IN (SELECT id FROM i5_vou);
DELETE FROM biz_material_serial WHERE id IN (SELECT id FROM i5_ser);
DELETE FROM biz_inventory_txn   WHERE material_id IN (SELECT id FROM i5_mat);
DELETE FROM biz_inventory_location WHERE material_id IN (SELECT id FROM i5_mat);
DELETE FROM biz_inventory       WHERE material_id IN (SELECT id FROM i5_mat);
DELETE FROM biz_location        WHERE warehouse_id IN (SELECT id FROM i5_wh);
DELETE FROM biz_material        WHERE id IN (SELECT id FROM i5_mat);
DELETE FROM biz_warehouse       WHERE id IN (SELECT id FROM i5_wh);
DELETE FROM biz_unit            WHERE unit_code LIKE 'I5-SMOKE%';

-- 兜底：幂等重跑时序列号行已不在，SERIAL_OUT/SERIAL_SCRAP 凭证就成无主（source_id 指不到活行）。
-- 只在「库里完全没有序列号业务」时清，正常业务库一律跳过。
DO $$
DECLARE v_ids uuid[];
BEGIN
  IF NOT EXISTS (SELECT 1 FROM biz_material_serial)
     AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn WHERE biz_type LIKE 'SERIAL%') THEN
    SELECT array_agg(id) INTO v_ids FROM biz_voucher
     WHERE source_type IN ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP')
       AND deleted_at IS NULL AND is_red = 0
       AND reversed_by IS NULL AND reverse_of IS NULL;
    IF v_ids IS NOT NULL THEN
      DELETE FROM biz_voucher_entry WHERE voucher_id = ANY(v_ids);
      DELETE FROM biz_status_log    WHERE entity_id  = ANY(v_ids);
      DELETE FROM biz_voucher       WHERE id         = ANY(v_ids);
      RAISE NOTICE 'purged orphan serial vouchers: %', array_length(v_ids, 1);
    END IF;
  ELSE
    RAISE NOTICE 'skip orphan serial voucher sweep: 库里还有序列号业务';
  END IF;
END $$;

\echo '===== 清场后残留检查（全部应为 0）====='
SELECT (SELECT count(*) FROM biz_material WHERE material_code LIKE 'I5-SMOKE%') AS material_left,
       (SELECT count(*) FROM biz_warehouse WHERE warehouse_code LIKE 'I5-SMOKE%') AS warehouse_left,
       (SELECT count(*) FROM biz_unit WHERE unit_code LIKE 'I5-SMOKE%') AS unit_left,
       (SELECT count(*) FROM biz_material_serial) AS serial_all,
       (SELECT count(*) FROM biz_inventory) AS inv_all,
       (SELECT count(*) FROM biz_inventory_txn) AS txn_all,
       (SELECT count(*) FROM biz_inventory_location) AS inv_loc_all,
       (SELECT count(*) FROM biz_voucher) AS voucher_all,
       (SELECT count(*) FROM biz_voucher_entry) AS entry_all,
       (SELECT count(*) FROM biz_status_log) AS status_log_all;
