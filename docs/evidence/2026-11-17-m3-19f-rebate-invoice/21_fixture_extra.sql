-- M3-19F 追加夹具：S4（并发 409 演示）+ S5（干净成功 + toast 截图）
BEGIN;
INSERT INTO biz_sales_rebate_settlement
    (id, settlement_no, customer_id, settle_period, period_code, period_from, period_to,
     policy_id, policy_no, policy_name, rule_type, rate_unit,
     policy_effective_from, policy_snapshot,
     order_count, order_amount, order_qty, return_count, return_amount, return_qty,
     base_amount, base_qty, tier_seq, tier_from, tier_to, rate, rebate_amount, status, remark)
SELECT v.id::uuid, v.settlement_no, c.id, 'MONTH', '2026-12', DATE '2026-12-01', DATE '2027-01-01',
       '11111111-1111-7111-8111-000000000001'::uuid,
       'BR-20261101-000101', 'M3-19F 演练·按额 1%', 'BY_AMOUNT', 'PERCENT',
       DATE '2026-01-01',
       '{"policy_no":"BR-20261101-000101","policy_name":"M3-19F 演练·按额 1%","rule_type":"BY_AMOUNT","rate_unit":"PERCENT","effective_from":"2026-01-01","effective_to":null,"tiers":[{"seq":1,"tier_from":"0","tier_to":null,"rate":"1","remark":"全档 1%"}]}'::jsonb,
       v.order_count, v.base_amount, v.order_qty, 0, 0, 0,
       v.base_amount, v.order_qty, 1, 0, NULL, 1, v.rebate_amount, 'DRAFT', v.remark
  FROM (VALUES
        ('11111111-1111-7111-8111-0000000000a4', 'RB-20261117-900004', 1, 1, 200000.0000, 20.0000, 2000.0000, 'M3-19F 验收：并发开票 → 409 人话'),
        ('11111111-1111-7111-8111-0000000000a5', 'RB-20261117-900005', 2, 1,  30000.0000,  3.0000,  300.0000, 'M3-19F 验收：干净开票（成功 toast）')
       ) AS v(id, settlement_no, cust_pos, order_count, base_amount, order_qty, rebate_amount, remark)
  JOIN (SELECT id, row_number() OVER (ORDER BY code) AS pos
          FROM biz_customer WHERE deleted_at IS NULL) c ON c.pos = v.cust_pos
 WHERE NOT EXISTS (SELECT 1 FROM biz_sales_rebate_settlement s WHERE s.settlement_no = v.settlement_no);
COMMIT;
SELECT settlement_no, status, rebate_amount FROM biz_sales_rebate_settlement WHERE settlement_no >= 'RB-20261117-900004' ORDER BY 1;
