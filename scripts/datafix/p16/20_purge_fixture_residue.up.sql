-- ============================================================================
-- P16 · ③ 清理夹具 / 演练残留（class A）
-- ----------------------------------------------------------------------------
-- 只做一件事：把 05_target 里那些「四张表只写了一半」的 (物料, 仓库) 整体清掉，
-- 连同它们的流水、序列号、库位账行，以及**主数据已被硬删的孤儿库位账行**。
--
-- 三条硬闸（任一不成立 → 整单回滚，绝不半删）：
--   G1 目标库位账行不得被 `biz_opening_inventory.location_row_id` 引用（期初指着的桶不能删）
--   G2 目标物料不得有任何**已过账/已完成**的单据明细（删了流水会让
--      「已过账单据必须有流水」变红，那是把库存单打成孤儿）
--   G3 目标序列号不得被 `biz_stock_transfer_item_serial.serial_id` 引用
--
-- 删除顺序（被硬外键钉死，不能换）：
--   序列号 location_id 解引用 → 调拨明细 from/to_location_id 解引用
--   → biz_material_serial → biz_inventory_txn → biz_inventory（成本账）
--   → biz_inventory_location（库位账）。成本账对库位账行有硬外键，反过来删 23503。
--
-- 幂等：目标集合是固定的；已清过的组行数为 0，重跑只会删到 0 行。
-- ============================================================================
\set ON_ERROR_STOP on
BEGIN;

-- ---------- G1：期初库存引用了目标库位账行？ ----------
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n
      FROM biz_opening_inventory o
     WHERE o.location_row_id IN (
             SELECT l.id FROM biz_inventory_location l
               JOIN biz_material  m ON m.id = l.material_id
               JOIN biz_warehouse w ON w.id = l.warehouse_id
               JOIN datafix_p16_target t
                 ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code);
    IF n > 0 THEN
        RAISE EXCEPTION 'P16 中止（G1）：目标库位账行被期初库存引用 % 行 —— 这不是夹具残留，请人工确认', n;
    END IF;
END $$;

-- ---------- G2：目标物料有已过账/已完成单据明细？ ----------
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n FROM (
        SELECT 1 FROM biz_stock_in_item i JOIN biz_stock_in h ON h.id = i.stock_in_id
          JOIN biz_material m ON m.id = i.material_id
          JOIN datafix_p16_target t ON t.material_code = m.material_code
         WHERE h.status = 'POSTED' AND h.deleted_at IS NULL
        UNION ALL
        SELECT 1 FROM biz_stock_out_item i JOIN biz_stock_out h ON h.id = i.stock_out_id
          JOIN biz_material m ON m.id = i.material_id
          JOIN datafix_p16_target t ON t.material_code = m.material_code
         WHERE h.status = 'POSTED' AND h.deleted_at IS NULL
        UNION ALL
        SELECT 1 FROM biz_stock_take_item i JOIN biz_stock_take h ON h.id = i.take_id
          JOIN biz_material m ON m.id = i.material_id
          JOIN datafix_p16_target t ON t.material_code = m.material_code
         WHERE h.status = 'POSTED' AND h.deleted_at IS NULL
        UNION ALL
        SELECT 1 FROM biz_stock_transfer_item i JOIN biz_stock_transfer h ON h.id = i.transfer_id
          JOIN biz_material m ON m.id = i.material_id
          JOIN datafix_p16_target t ON t.material_code = m.material_code
         WHERE h.status = 'DONE' AND h.deleted_at IS NULL
    ) x;
    IF n > 0 THEN
        RAISE EXCEPTION 'P16 中止（G2）：目标物料有 % 条已过账/已完成单据明细 —— 先作废单据再清，或人工确认', n;
    END IF;
END $$;

-- ---------- G3：目标序列号被调拨明细引用？ ----------
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n
      FROM biz_stock_transfer_item_serial r
     WHERE r.serial_id IN (
             SELECT s.id FROM biz_material_serial s
               JOIN biz_material  m ON m.id = s.material_id
               JOIN biz_warehouse w ON w.id = s.warehouse_id
               JOIN datafix_p16_target t
                 ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code);
    IF n > 0 THEN
        RAISE EXCEPTION 'P16 中止（G3）：目标序列号被调拨明细引用 % 行', n;
    END IF;
END $$;

\echo '== P16 清理前：目标组待删行数 =='
SELECT sum(inv_rows) AS 成本账, sum(loc_rows) AS 库位账,
       sum(txn_rows) AS 流水, sum(serial_rows) AS 序列号
  FROM datafix_p16_target;

-- ---------- ① 解除序列号 → 库位账行（硬外键）----------
UPDATE biz_material_serial s
   SET location_id = NULL, updated_at = now()
 WHERE s.location_id IN (
         SELECT l.id FROM biz_inventory_location l
           JOIN biz_material  m ON m.id = l.material_id
           JOIN biz_warehouse w ON w.id = l.warehouse_id
           JOIN datafix_p16_target t
             ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code);
\echo '  ① 序列号解除库位行引用：' :ROW_COUNT ' 行'

-- ---------- ② 解除调拨明细 → 库位账行（硬外键）----------
UPDATE biz_stock_transfer_item i
   SET from_location_id = NULL
 WHERE i.from_location_id IN (
         SELECT l.id FROM biz_inventory_location l
           JOIN biz_material  m ON m.id = l.material_id
           JOIN biz_warehouse w ON w.id = l.warehouse_id
           JOIN datafix_p16_target t
             ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code);
\echo '  ② 调拨明细 from_location_id 解引用：' :ROW_COUNT ' 行'

UPDATE biz_stock_transfer_item i
   SET to_location_id = NULL
 WHERE i.to_location_id IN (
         SELECT l.id FROM biz_inventory_location l
           JOIN biz_material  m ON m.id = l.material_id
           JOIN biz_warehouse w ON w.id = l.warehouse_id
           JOIN datafix_p16_target t
             ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code);
\echo '  ② 调拨明细 to_location_id 解引用：' :ROW_COUNT ' 行'

-- ---------- ③ 序列号 ----------
DELETE FROM biz_material_serial s
 USING biz_material m, biz_warehouse w, datafix_p16_target t
 WHERE m.id = s.material_id AND w.id = s.warehouse_id
   AND t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code;
\echo '  ③ 序列号：' :ROW_COUNT ' 行'

-- ---------- ④ 流水 ----------
DELETE FROM biz_inventory_txn x
 USING biz_material m, biz_warehouse w, datafix_p16_target t
 WHERE m.id = x.material_id AND w.id = x.warehouse_id
   AND t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code;
\echo '  ④ 流水：' :ROW_COUNT ' 行'

-- ---------- ⑤ 成本账（必须先于库位账：biz_inventory.location_id 是硬外键）----------
DELETE FROM biz_inventory i
 USING biz_material m, biz_warehouse w, datafix_p16_target t
 WHERE m.id = i.material_id AND w.id = i.warehouse_id
   AND t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code;
\echo '  ⑤ 成本账：' :ROW_COUNT ' 行'

-- ---------- ⑥ 库位账：目标组 + 孤儿行（主数据已硬删）----------
DELETE FROM biz_inventory_location l
 USING biz_material m, biz_warehouse w, datafix_p16_target t
 WHERE m.id = l.material_id AND w.id = l.warehouse_id
   AND t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code;
\echo '  ⑥ 库位账（目标组）：' :ROW_COUNT ' 行'

DELETE FROM biz_inventory_location l
 WHERE NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
    OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id);
\echo '  ⑥ 库位账（孤儿行）：' :ROW_COUNT ' 行'

-- ---------- 记账：done_at + 复核行数归零 ----------
UPDATE datafix_p16_target t SET
    inv_rows = (SELECT count(*) FROM biz_inventory i
                  JOIN biz_material m ON m.id = i.material_id
                  JOIN biz_warehouse w ON w.id = i.warehouse_id
                 WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code),
    loc_rows = (SELECT count(*) FROM biz_inventory_location l
                  JOIN biz_material m ON m.id = l.material_id
                  JOIN biz_warehouse w ON w.id = l.warehouse_id
                 WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code),
    txn_rows = (SELECT count(*) FROM biz_inventory_txn x
                  JOIN biz_material m ON m.id = x.material_id
                  JOIN biz_warehouse w ON w.id = x.warehouse_id
                 WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code),
    serial_rows = (SELECT count(*) FROM biz_material_serial s
                     JOIN biz_material m ON m.id = s.material_id
                     JOIN biz_warehouse w ON w.id = s.warehouse_id
                    WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code),
    done_at = coalesce(t.done_at, now());

DO $$
DECLARE r record;
BEGIN
    FOR r IN SELECT * FROM datafix_p16_target WHERE inv_rows + loc_rows + txn_rows + serial_rows > 0 LOOP
        RAISE EXCEPTION 'P16 中止（自检）：%(%) 清理后仍有 成本账 % / 库位账 % / 流水 % / 序列号 % 行',
            r.material_code, r.warehouse_code, r.inv_rows, r.loc_rows, r.txn_rows, r.serial_rows;
    END LOOP;
    RAISE NOTICE 'P16 自检通过：全部目标组的 成本账/库位账/流水/序列号 均为 0 行';
END $$;

\echo '== P16 清理后：目标组行数（应全 0）=='
SELECT material_code, warehouse_code, inv_rows, loc_rows, txn_rows, serial_rows,
       to_char(done_at,'MM-DD HH24:MI') AS 处理时间
  FROM datafix_p16_target ORDER BY material_code, warehouse_code;

COMMIT;
