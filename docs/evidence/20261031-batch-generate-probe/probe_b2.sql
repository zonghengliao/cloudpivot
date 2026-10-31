BEGIN;
-- 现状：库里 1405 余额 vs 库存金额（测试库口径）
SELECT 'B2 库存金额' k, coalesce(sum(amount),0) v FROM biz_inventory
UNION ALL SELECT 'B2 1405余额',
  coalesce(sum(coalesce(e.debit_amount,0)-coalesce(e.credit_amount,0)),0)
  FROM biz_voucher_entry e JOIN biz_voucher v ON v.id=e.voucher_id AND v.deleted_at IS NULL
  JOIN biz_account a ON a.id=e.account_id WHERE a.account_code LIKE '1405%';

-- 模拟：批量生成 5 件 @10 写成本账 amount=50，但**不生成凭证**
INSERT INTO biz_unit (id, unit_code, unit_name) VALUES ('33333333-3333-4333-8333-333333333333','PCS-R','件') ON CONFLICT DO NOTHING;
INSERT INTO biz_material (id, material_code, material_name, unit_id, serial_required)
VALUES ('a0000000-0000-4000-8000-00000000a003','PROBE-M3','B2取证物料','33333333-3333-4333-8333-333333333333', true) ON CONFLICT DO NOTHING;
INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount, last_txn_at)
VALUES (gen_random_uuid(),'a0000000-0000-4000-8000-00000000a003','b0000000-0000-4000-8000-00000000b001',5,0,10.000000,50.00,now());

\echo '=== 写 50 元库存、不记账凭证之后 ==='
SELECT 'B2 库存金额' k, coalesce(sum(amount),0) v FROM biz_inventory
UNION ALL SELECT 'B2 1405余额',
  coalesce(sum(coalesce(e.debit_amount,0)-coalesce(e.credit_amount,0)),0)
  FROM biz_voucher_entry e JOIN biz_voucher v ON v.id=e.voucher_id AND v.deleted_at IS NULL
  JOIN biz_account a ON a.id=e.account_id WHERE a.account_code LIKE '1405%';
ROLLBACK;
