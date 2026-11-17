-- M3-23 前端页面真机验证夹具（2026-11，可重复执行：全部 ON CONFLICT DO NOTHING）
-- 客户 3 家：C1（v2 覆盖 v1，实际低于预测）、C2（实际超出预测）、C3（没有预测 = 漏预测行）
-- 物料 2 个：M1（有物料级预测）、M2（只有实际 = 漏预测）
BEGIN;

INSERT INTO biz_customer (id, code, name, status) VALUES
  ('32300000-0000-4000-8000-000000000001', 'M323-C1', '北京华信科技', 'ACTIVE'),
  ('32300000-0000-4000-8000-000000000002', 'M323-C2', '上海联通贸易', 'ACTIVE'),
  ('32300000-0000-4000-8000-000000000003', 'M323-C3', '广州立创电子', 'ACTIVE')
ON CONFLICT DO NOTHING;

INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status, created_at, updated_at)
SELECT '32300000-0000-4000-8000-000000000011', 'M323-M1', '云服务器 X1',
       (SELECT id FROM biz_unit ORDER BY unit_code LIMIT 1), 'FINISHED', 1, now(), now()
ON CONFLICT DO NOTHING;

INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status, created_at, updated_at)
SELECT '32300000-0000-4000-8000-000000000012', 'M323-M2', '存储阵列 S2',
       (SELECT id FROM biz_unit ORDER BY unit_code LIMIT 1), 'FINISHED', 1, now(), now()
ON CONFLICT DO NOTHING;

-- 预测：C1 两版（v2 是当前版本，v1 是历史版本，不该进合计）
INSERT INTO biz_sales_forecast
  (id, forecast_no, customer_id, material_id, period_code, forecast_amount, forecast_qty, confidence, version_no, remark)
VALUES
  ('32300000-0000-4000-8000-000000000101', 'SFC-M323-0001', '32300000-0000-4000-8000-000000000001',
   NULL, '2026-11', 100000.00, 100.0000, 'MEDIUM', 1, 'M3-23 夹具'),
  ('32300000-0000-4000-8000-000000000102', 'SFC-M323-0002', '32300000-0000-4000-8000-000000000001',
   NULL, '2026-11', 120000.00, 120.0000, 'HIGH', 2, 'M3-23 夹具'),
  ('32300000-0000-4000-8000-000000000103', 'SFC-M323-0003', '32300000-0000-4000-8000-000000000002',
   NULL, '2026-11', 50000.00, 50.0000, 'MEDIUM', 1, 'M3-23 夹具'),
  ('32300000-0000-4000-8000-000000000104', 'SFC-M323-0004', NULL,
   '32300000-0000-4000-8000-000000000011', '2026-11', 80000.00, 80.0000, 'LOW', 1, 'M3-23 夹具')
ON CONFLICT DO NOTHING;

-- 订单（APPROVED = 有效订单口径）：C1 9 万 / C2 6 万 / C3 3 万（C3 无预测）
INSERT INTO biz_sales_order
  (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax,
   base_total_amount, base_total_tax, base_total_with_tax)
VALUES
  ('32300000-0000-4000-8000-000000000201', 'M323-SO-0001', '32300000-0000-4000-8000-000000000001',
   '2026-11-08', 'APPROVED', 90000, 0, 90000, 90000, 0, 90000),
  ('32300000-0000-4000-8000-000000000202', 'M323-SO-0002', '32300000-0000-4000-8000-000000000002',
   '2026-11-12', 'APPROVED', 60000, 0, 60000, 60000, 0, 60000),
  ('32300000-0000-4000-8000-000000000203', 'M323-SO-0003', '32300000-0000-4000-8000-000000000003',
   '2026-11-15', 'APPROVED', 30000, 0, 30000, 30000, 0, 30000)
ON CONFLICT DO NOTHING;

INSERT INTO biz_sales_order_item
  (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty)
VALUES
  ('32300000-0000-4000-8000-000000000201', 1, '32300000-0000-4000-8000-000000000011', 60, 1000, 60000, 0, 0, 0),
  ('32300000-0000-4000-8000-000000000201', 2, '32300000-0000-4000-8000-000000000012', 30, 1000, 30000, 0, 0, 0),
  ('32300000-0000-4000-8000-000000000202', 1, '32300000-0000-4000-8000-000000000011', 60, 1000, 60000, 0, 0, 0),
  ('32300000-0000-4000-8000-000000000203', 1, '32300000-0000-4000-8000-000000000012', 30, 1000, 30000, 0, 0, 0)
ON CONFLICT DO NOTHING;

COMMIT;
