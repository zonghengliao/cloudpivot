SELECT table_name FROM information_schema.tables
 WHERE table_schema = 'public' AND (table_name LIKE '%flow%' OR table_name LIKE '%state_edge%')
 ORDER BY 1;
\echo '--- state edge 列 ---'
SELECT column_name FROM information_schema.columns WHERE table_name LIKE '%state_edge%' ORDER BY ordinal_position;
\echo '--- 全局对账基线（逐物料仓差异行数）---'
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT count(*) AS diff_rows, COALESCE(SUM(COALESCE(inv.qty,0)-COALESCE(txn.qty,0)),0) AS diff_qty
  FROM inv FULL JOIN txn USING (material_id, warehouse_id)
 WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0);
