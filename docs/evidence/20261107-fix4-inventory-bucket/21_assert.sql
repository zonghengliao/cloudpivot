-- FIX-4 / V0324 存量数据迁移断言（在 20_seed_legacy.sql → V0324 up.sql 之后跑）
-- 输出每行 `PASS/FAIL <断言>`，任一行 FAIL 就让调用方 exit 1（脚本里 grep -c FAIL）。
\set ON_ERROR_STOP on

\echo '--- 迁移后：该仓该物料的库存桶分布 ---'
SELECT COALESCE(l.location_code, '(未分配桶)') AS bucket,
       il.qty AS loc_qty,
       i.qty AS cost_qty,
       i.amount AS cost_amount
  FROM biz_inventory_location il
  LEFT JOIN biz_location l ON l.id = il.location_id
  LEFT JOIN biz_inventory i
         ON i.material_id = il.material_id AND i.warehouse_id = il.warehouse_id
        AND i.location_id = il.id
 WHERE il.material_id = 'f4f40002-0000-4000-8000-000000000001'
 ORDER BY 1;

\echo '--- 断言 ---'
SELECT CASE WHEN COUNT(*) = 0 THEN 'PASS ① 有默认库位的仓库不再有「未分配桶」库位账行'
            ELSE 'FAIL ① 仍有 ' || COUNT(*) || ' 条未分配桶行' END
  FROM biz_inventory_location
 WHERE material_id = 'f4f40002-0000-4000-8000-000000000001'
   AND location_id IS NULL;

SELECT CASE WHEN COUNT(*) = 1 THEN 'PASS ② 只剩一个桶（默认库位桶）'
            ELSE 'FAIL ② 还有 ' || COUNT(*) || ' 个桶' END
  FROM biz_inventory_location
 WHERE material_id = 'f4f40002-0000-4000-8000-000000000001';

SELECT CASE WHEN qty = 50 AND amount = 500 AND avg_cost = 10
            THEN 'PASS ③ 默认库位桶成本账 = 40+10 = 50 / 金额 500 / 均价 10'
            ELSE 'FAIL ③ 成本账 qty=' || qty || ' amount=' || amount || ' avg_cost=' || avg_cost END
  FROM biz_inventory
 WHERE material_id = 'f4f40002-0000-4000-8000-000000000001'
   AND warehouse_id = 'f4f40001-0000-4000-8000-000000000001';

SELECT CASE WHEN SUM(qty) = 50 THEN 'PASS ④ 库位账合计 = 50（总数量不变）'
            ELSE 'FAIL ④ 库位账合计 = ' || SUM(qty) END
  FROM biz_inventory_location
 WHERE material_id = 'f4f40002-0000-4000-8000-000000000001';

SELECT CASE WHEN COUNT(*) = 0 THEN 'PASS ⑤ 两本账逐 (物料,仓,桶) 相等（0 行差异）'
            ELSE 'FAIL ⑤ 有 ' || COUNT(*) || ' 个桶两本账不等' END
  FROM (SELECT il.id, il.qty AS loc_qty, COALESCE(i.qty, 0) AS cost_qty
          FROM biz_inventory_location il
          LEFT JOIN biz_inventory i
                 ON i.material_id = il.material_id AND i.warehouse_id = il.warehouse_id
                AND i.location_id = il.id
         WHERE il.material_id = 'f4f40002-0000-4000-8000-000000000001') t
 WHERE loc_qty <> cost_qty;

SELECT CASE WHEN COUNT(*) = 1 THEN 'PASS ⑥ 序列号的库位引用已改指向目标库位账行（FK 不被打破）'
            ELSE 'FAIL ⑥ 序列号引用仍有 ' || COUNT(*) || ' 条指向已删的未分配桶行' END
  FROM biz_material_serial s
  JOIN biz_inventory_location il ON il.id = s.location_id
 WHERE s.id = 'f4f40005-0000-4000-8000-000000000001'
   AND il.location_id = 'f4f40003-0000-4000-8000-0000000000d1';

SELECT CASE WHEN location_row_id IS NOT NULL
            THEN 'PASS ⑦ 期初台账已回填库位桶（重录/取消可冲回同一个桶）'
            ELSE 'FAIL ⑦ 期初台账 location_row_id 仍为 NULL' END
  FROM biz_opening_inventory
 WHERE period_code = '2033-11'
   AND material_id = 'f4f40002-0000-4000-8000-000000000001';

\echo '--- 全库对账：库存表 = 流水累计（(物料,仓库) 聚合口径，差异应 0 行）---'
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1, 2)
SELECT COUNT(*) AS diff_rows
  FROM inv
 WHERE EXISTS (
   SELECT 1 FROM (
     SELECT material_id, warehouse_id,
            SUM(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END) AS qty
       FROM biz_inventory_txn GROUP BY 1, 2) t
    WHERE t.material_id = inv.material_id AND t.warehouse_id = inv.warehouse_id
      AND t.qty <> inv.qty);
