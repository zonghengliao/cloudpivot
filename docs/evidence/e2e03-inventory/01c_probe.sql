SELECT s.serial_no, s.status, s.location_id, s.source_type, s.source_id IS NOT NULL AS has_src, s.batch_no, s.created_at
  FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id
 WHERE m.material_code='E2E03-MS' ORDER BY s.created_at;
