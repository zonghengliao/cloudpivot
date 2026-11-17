-- M3-19F 演练夹具：3 张返利结算单（草稿）
-- S1 rebate=1000 → 正常开票；S2 rebate=0 → 66094；S3 rebate=500 → 开票日 2026-10（已结账）→ 66095
BEGIN;

-- 政策（页面「适用政策」列展示快照，policy_id 是弱引用，这里种一条真政策更贴近现场）
INSERT INTO biz_sales_rebate_policy
    (id, policy_no, policy_name, customer_id, rule_type, rate_unit, settle_period,
     effective_from, status, remark)
SELECT '11111111-1111-7111-8111-000000000001'::uuid,
       'BR-20261101-000101', 'M3-19F 演练·按额 1%', c.id, 'BY_AMOUNT', 'PERCENT', 'MONTH',
       DATE '2026-01-01', 1, 'M3-19F 演练夹具（开票入口验收）'
  FROM (SELECT id FROM biz_customer WHERE deleted_at IS NULL ORDER BY code LIMIT 1) c
 WHERE NOT EXISTS (SELECT 1 FROM biz_sales_rebate_policy WHERE policy_no = 'BR-20261101-000101');

-- 三张草稿结算单（不同客户，避开 uk_biz_srs_period）
INSERT INTO biz_sales_rebate_settlement
    (id, settlement_no, customer_id, settle_period, period_code, period_from, period_to,
     policy_id, policy_no, policy_name, rule_type, rate_unit,
     policy_effective_from, policy_snapshot,
     order_count, order_amount, order_qty, return_count, return_amount, return_qty,
     base_amount, base_qty, tier_seq, tier_from, tier_to, rate, rebate_amount, status, remark)
SELECT v.id::uuid, v.settlement_no, c.id, 'MONTH', '2026-11', DATE '2026-11-01', DATE '2026-12-01',
       '11111111-1111-7111-8111-000000000001'::uuid,
       'BR-20261101-000101', 'M3-19F 演练·按额 1%', 'BY_AMOUNT', 'PERCENT',
       DATE '2026-01-01',
       '{"policy_no":"BR-20261101-000101","policy_name":"M3-19F 演练·按额 1%","rule_type":"BY_AMOUNT","rate_unit":"PERCENT","effective_from":"2026-01-01","effective_to":null,"tiers":[{"seq":1,"tier_from":"0","tier_to":null,"rate":"1","remark":"全档 1%"}]}'::jsonb,
       v.order_count, v.base_amount, v.order_qty, 0, 0, 0,
       v.base_amount, v.order_qty, 1, 0, NULL, 1, v.rebate_amount, 'DRAFT', v.remark
  FROM (VALUES
        ('11111111-1111-7111-8111-0000000000a1', 'RB-20261117-900001', 1, 1, 100000.0000, 10.0000, 1000.0000, 'M3-19F 验收：正常开票'),
        ('11111111-1111-7111-8111-0000000000a2', 'RB-20261117-900002', 2, 0,      0.0000,  0.0000,    0.0000, 'M3-19F 验收：应返为 0（66094）'),
        ('11111111-1111-7111-8111-0000000000a3', 'RB-20261117-900003', 3, 1,  50000.0000,  5.0000,  500.0000, 'M3-19F 验收：开票日撞已结账期间（66095）')
       ) AS v(id, settlement_no, cust_pos, order_count, base_amount, order_qty, rebate_amount, remark)
  JOIN (SELECT id, row_number() OVER (ORDER BY code) AS pos
          FROM biz_customer WHERE deleted_at IS NULL) c ON c.pos = v.cust_pos
 WHERE NOT EXISTS (SELECT 1 FROM biz_sales_rebate_settlement s WHERE s.settlement_no = v.settlement_no);

-- 2026-10 结账（66095 的触发条件：开票日落在已结账期间）
UPDATE biz_accounting_period
   SET status = 'CLOSED', closed_at = now()
 WHERE period_code = '2026-10' AND status <> 'CLOSED';

COMMIT;

SELECT settlement_no, status, rebate_amount, customer_id FROM biz_sales_rebate_settlement ORDER BY settlement_no;
SELECT period_code, status FROM biz_accounting_period WHERE period_code IN ('2026-10','2026-11');
