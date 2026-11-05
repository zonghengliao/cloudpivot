\pset footer off
\echo '== DI-26 六个分支在生产库的等价只读计数（不装 di 视图，直接对业务表）=='
SELECT
  (SELECT count(*) FROM (
      SELECT lower(btrim(supplier_name)) k FROM biz_supplier
       WHERE deleted_at IS NULL GROUP BY 1 HAVING count(*) > 1) t)                AS name_dup,
  (SELECT count(*) FROM (
      SELECT lower(btrim(short_name)) k FROM biz_supplier
       WHERE deleted_at IS NULL AND coalesce(btrim(short_name),'') <> ''
       GROUP BY 1 HAVING count(*) > 1) t)                                          AS short_dup,
  (SELECT count(*) FROM biz_supplier a
     JOIN biz_supplier b ON b.id <> a.id AND b.deleted_at IS NULL
      AND lower(btrim(b.supplier_name)) = lower(btrim(a.short_name))
    WHERE a.deleted_at IS NULL AND coalesce(btrim(a.short_name),'') <> '')         AS short_shadows_name,
  (SELECT count(*) FROM biz_purchase_order o JOIN biz_supplier s ON s.id = o.supplier_id AND s.deleted_at IS NULL
    WHERE o.deleted_at IS NULL AND coalesce(btrim(o.supplier_name_snapshot),'') <> ''
      AND lower(btrim(o.supplier_name_snapshot)) <> lower(btrim(s.supplier_name))
      AND NOT (o.purchase_type = 'SPOT' AND o.created_at > s.updated_at))          AS snapshot_drift_po,
  (SELECT count(*) FROM biz_contract c JOIN biz_supplier s ON s.id = c.party_b_id AND s.deleted_at IS NULL
    WHERE c.deleted_at IS NULL AND coalesce(btrim(c.party_b_name),'') <> ''
      AND lower(btrim(c.party_b_name)) <> lower(btrim(s.supplier_name)))            AS snapshot_drift_contract,
  (SELECT count(*) FROM biz_flow_instance f
     JOIN biz_supplier s ON s.id::text = btrim(f.form_data ->> 'supplier_id') AND s.deleted_at IS NULL
    WHERE f.deleted_at IS NULL AND coalesce(btrim(f.form_data ->> 'supplier_name'),'') <> ''
      AND lower(btrim(f.form_data ->> 'supplier_name')) <> lower(btrim(s.supplier_name))) AS snapshot_drift_flow,
  (SELECT count(*) FROM biz_purchase_order o JOIN biz_supplier s ON s.id = o.supplier_id AND s.deleted_at IS NULL
    WHERE o.deleted_at IS NULL AND o.supplier_id IS NOT NULL
      AND coalesce(btrim(o.supplier_name_snapshot),'') = '')                        AS snapshot_missing,
  (SELECT count(*) FROM biz_purchase_order o JOIN biz_supplier s ON s.id = o.supplier_id
    WHERE o.deleted_at IS NULL AND coalesce(btrim(o.supplier_name_snapshot),'') <> ''
      AND lower(btrim(o.supplier_name_snapshot)) <> lower(btrim(s.supplier_name))
      AND o.purchase_type = 'SPOT' AND o.created_at > s.updated_at)                  AS snapshot_late_write;
