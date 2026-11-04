\pset footer off
\echo '== 0. 迁移版本 =='
SELECT max(version) AS max_version, count(*) AS applied FROM schema_migrations;
\echo '== 0b. 业务数据量 =='
SELECT (SELECT count(*) FROM biz_supplier) AS supplier,
       (SELECT count(*) FROM biz_purchase_order) AS po,
       (SELECT count(*) FROM biz_contract) AS contract,
       (SELECT count(*) FROM biz_stock_in) AS stock_in,
       (SELECT count(*) FROM biz_payable) AS payable,
       (SELECT count(*) FROM biz_flow_instance) AS flow_inst;

\echo '== 1. 活跃供应商同名（归一 trim+lower）=='
SELECT count(*) AS dup_name_groups, coalesce(sum(n), 0) AS rows_in_groups FROM (
  SELECT lower(btrim(supplier_name)) AS k, count(*) AS n
    FROM biz_supplier WHERE deleted_at IS NULL GROUP BY 1 HAVING count(*) > 1) t;

\echo '== 2. 活跃简称重复 =='
SELECT count(*) AS dup_short_groups, coalesce(sum(n), 0) AS rows_in_groups FROM (
  SELECT lower(btrim(short_name)) AS k, count(*) AS n
    FROM biz_supplier WHERE deleted_at IS NULL AND coalesce(btrim(short_name), '') <> ''
   GROUP BY 1 HAVING count(*) > 1) t;

\echo '== 3. 简称撞他人全名 =='
SELECT count(*) AS short_hits_name FROM biz_supplier a
  JOIN biz_supplier b ON b.id <> a.id AND b.deleted_at IS NULL
   AND lower(btrim(b.supplier_name)) = lower(btrim(a.short_name))
 WHERE a.deleted_at IS NULL AND coalesce(btrim(a.short_name), '') <> '';

\echo '== 4. 采购单快照 vs 现名 =='
SELECT count(*) AS po_total,
       count(*) FILTER (WHERE o.supplier_name_snapshot IS NULL) AS snap_null,
       count(*) FILTER (WHERE o.supplier_name_snapshot IS NOT NULL
                          AND s.id IS NOT NULL
                          AND btrim(o.supplier_name_snapshot) <> btrim(s.supplier_name)) AS snap_ne_current,
       count(*) FILTER (WHERE o.supplier_name_snapshot IS NOT NULL
                          AND s.id IS NOT NULL
                          AND btrim(o.supplier_name_snapshot) <> btrim(s.supplier_name)
                          AND o.purchase_type = 'SPOT') AS spot_snap_ne_current,
       count(*) FILTER (WHERE o.supplier_name_snapshot IS NOT NULL
                          AND s.id IS NOT NULL
                          AND btrim(o.supplier_name_snapshot) <> btrim(s.supplier_name)
                          AND o.purchase_type = 'CONTRACT') AS contract_snap_ne_current
  FROM biz_purchase_order o
  LEFT JOIN biz_supplier s ON s.id = o.supplier_id;

\echo '== 5. 晚写（SPOT 单建单晚于主档最后修改，却写了旧名）=='
SELECT count(*) AS late_write FROM biz_purchase_order o
  JOIN biz_supplier s ON s.id = o.supplier_id
 WHERE o.deleted_at IS NULL AND o.supplier_name_snapshot IS NOT NULL
   AND btrim(o.supplier_name_snapshot) <> btrim(s.supplier_name)
   AND o.purchase_type = 'SPOT' AND o.created_at > s.updated_at;

\echo '== 6. 合同 party_b_name vs 现名 =='
SELECT count(*) AS contract_total,
       count(*) FILTER (WHERE s.id IS NOT NULL AND btrim(c.party_b_name) <> btrim(s.supplier_name)) AS party_b_ne_current
  FROM biz_contract c LEFT JOIN biz_supplier s ON s.id = c.party_b_id;

\echo '== 7. 审批快照 form_data.supplier_name vs 现名 =='
SELECT count(*) AS instances_with_name,
       count(*) FILTER (WHERE s.id IS NOT NULL
                          AND btrim(f.form_data->>'supplier_name') <> btrim(s.supplier_name)) AS snap_ne_current
  FROM biz_flow_instance f
  LEFT JOIN biz_supplier s ON s.id::text = f.form_data->>'supplier_id'
 WHERE coalesce(btrim(f.form_data->>'supplier_name'), '') <> '';

\echo '== 8. 简称在单据/报表里的使用（应为 0 行）=='
SELECT count(*) AS short_name_in_docs FROM biz_purchase_order WHERE coalesce(btrim(supplier_name_snapshot),'') <> ''
   AND EXISTS (SELECT 1 FROM biz_supplier s WHERE s.id = biz_purchase_order.supplier_id
                AND lower(btrim(biz_purchase_order.supplier_name_snapshot)) = lower(btrim(s.short_name))
                AND lower(btrim(s.short_name)) <> lower(btrim(s.supplier_name)));
