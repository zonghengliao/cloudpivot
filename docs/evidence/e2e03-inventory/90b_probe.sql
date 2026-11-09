\echo '--- E2E-03 收货单 ---'
SELECT id, in_no, status, in_type, total_amount, approval_instance_id FROM biz_stock_in WHERE remark LIKE 'E2E-03%';
\echo '--- 相关凭证 ---'
SELECT source_type, source_id, voucher_no, status FROM biz_voucher WHERE created_at::date = current_date AND source_type IN ('STOCK_IN','SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP') ORDER BY created_at DESC LIMIT 12;
\echo '--- biz_status_log 列 ---'
SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name='biz_status_log';
\echo '--- MS 序列号/库存速览 ---'
SELECT s.serial_no, s.status FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id WHERE m.material_code='E2E03-MS' ORDER BY s.created_at;
SELECT 'inv' t, coalesce(sum(qty),0) q FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='E2E03-MS'
UNION ALL SELECT 'loc', coalesce(sum(qty),0) FROM biz_inventory_location i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='E2E03-MS';
\echo '--- 应付单是否被这些收货单带出 ---'
SELECT count(*) FROM biz_payable WHERE doc_no IN (SELECT in_no FROM biz_stock_in WHERE remark LIKE 'E2E-03%');
