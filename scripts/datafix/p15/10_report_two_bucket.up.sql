-- ============================================================
-- P15 数据修复 ①（**只读**）：找「同物料同仓两桶并存」的物料
--
-- 背景（E2E-FIX-6 / ADR-20261031）：
--   `POST /serial/batch-generate` 在 E2E-FIX-6 之前 **不接受也不透传库位**，
--   所以批量生成的成本账 / 库位账 / 序列号一律落「未分配桶」
--   （`biz_inventory.location_id IS NULL`、`biz_inventory_location.location_id IS NULL`）；
--   而收货入库 / 期初落「默认库位桶」（`biz_location.is_default = 1`）。
--   ⇒ 同一个物料在同一个仓库里必然出现两个桶：
--       · 库位库存页出现「(未分配)」行
--       · 扫码只看到仓库、看不到库位
--       · 巡检的「库位账合计 = 成本账合计」按仓求和是平的，但逐桶看是分裂的
--
-- 本脚本**只查不改**。判读口径：
--   · `has_unassigned = true`  该 (物料, 仓库) 有未分配桶且 qty/amount 不为 0
--   · `has_default_bucket`     同时已经有默认库位桶（NULL 桶 + 默认桶 = 真正的两桶并存）
--   · `has_default_location`   该仓库是否配了默认库位（false ⇒ 合并脚本会跳过它，
--                              因为「没有默认库位」本身就是收货也落未分配桶的正常形态）
--   · `null_serial_cnt`        落未分配桶的在库序列号件数（扫码看不到库位的那些）
--
-- 执行：
--   psql "$DSN" -v ON_ERROR_STOP=1 -f scripts/datafix/p15/10_report_two_bucket.up.sql
-- ============================================================

WITH unassigned AS (
    SELECT i.material_id,
           i.warehouse_id,
           i.qty   AS null_qty,
           i.amount AS null_amount,
           i.avg_cost AS null_avg_cost
      FROM biz_inventory i
     WHERE i.location_id IS NULL
       AND (i.qty <> 0 OR i.amount <> 0)
),
def_loc AS (
    SELECT l.warehouse_id,
           l.id           AS location_dict_id,
           l.location_code,
           l.location_name
      FROM biz_location l
     WHERE l.is_default = 1
       AND l.deleted_at IS NULL
),
bucket AS (
    SELECT i.material_id,
           i.warehouse_id,
           i.location_id,
           i.qty,
           i.amount
      FROM biz_inventory i
     WHERE i.location_id IS NOT NULL
),
null_serials AS (
    SELECT s.material_id,
           s.warehouse_id,
           COUNT(*) AS cnt
      FROM biz_material_serial s
     WHERE s.location_id IS NULL
       AND s.status = 'IN_STOCK'
       AND s.deleted_at IS NULL
     GROUP BY s.material_id, s.warehouse_id
)
SELECT m.material_code,
       m.material_name,
       m.serial_required,
       w.warehouse_code,
       w.warehouse_name,
       u.null_qty,
       u.null_amount,
       u.null_avg_cost,
       (d.location_dict_id IS NOT NULL) AS has_default_location,
       d.location_code                  AS default_location_code,
       EXISTS (
           SELECT 1
             FROM bucket b
             JOIN def_loc d2 ON d2.warehouse_id = b.warehouse_id
             JOIN biz_inventory_location il ON il.id = b.location_id
            WHERE b.material_id = u.material_id
              AND b.warehouse_id = u.warehouse_id
              AND il.location_id = d2.location_dict_id
       ) AS has_default_bucket,
       COALESCE(ns.cnt, 0) AS null_serial_cnt,
       -- 合并后应该落到的「默认库位桶」是否已存在（存在 = 合并是 加法；不存在 = 先建行）
       (SELECT il.id
          FROM biz_inventory_location il
          JOIN def_loc d3 ON d3.location_dict_id = il.location_id
         WHERE il.material_id = u.material_id
           AND il.warehouse_id = u.warehouse_id
           AND il.location_id = d3.location_dict_id
         LIMIT 1) AS target_ledger_row_hint
  FROM unassigned u
  JOIN biz_material  m ON m.id = u.material_id
  JOIN biz_warehouse w ON w.id = u.warehouse_id
  LEFT JOIN def_loc  d ON d.warehouse_id = u.warehouse_id
  LEFT JOIN null_serials ns
         ON ns.material_id = u.material_id
        AND ns.warehouse_id = u.warehouse_id
 ORDER BY w.warehouse_code, m.material_code;
