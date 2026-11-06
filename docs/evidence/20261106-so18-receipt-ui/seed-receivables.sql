-- SO-18 前端页手工验收用种子：每轮 3 张应收（CUS-0001 两张、CUS-0002 一张）
--   用法：psql -v round=1 -f seed-receivables.sql
--   目的：让核销面板有「该客户所有未收应收」可选，并能验证「核到别家客户 → 400」。
--   幂等/可重跑：每轮用不同的 ar_no（AR-SO18-R1-001…），旧轮的应收已被核销过
--   （biz_receipt_allocation FK RESTRICT），所以不删旧行、只追加本轮。
INSERT INTO biz_receivable
  (ar_no, customer_id, source_type, source_id, source_no, amount, received_amount,
   unsettled_amount, credit_days, due_date, period_code, status, remark)
VALUES
  ('AR-SO18-R' || :round || '-001', 'd0260000-0000-4000-8000-000000000001', 'MANUAL', gen_random_uuid(),
   'SO18-R' || :round || '-SRC-001', 600.0000, 0, 600.0000, 0, '2026-11-05', '2026-11', 'OPEN',
   'SO-18 验收种子 第' || :round || '轮'),
  ('AR-SO18-R' || :round || '-002', 'd0260000-0000-4000-8000-000000000001', 'MANUAL', gen_random_uuid(),
   'SO18-R' || :round || '-SRC-002', 400.0000, 0, 400.0000, 0, '2026-11-20', '2026-11', 'OPEN',
   'SO-18 验收种子 第' || :round || '轮'),
  ('AR-SO18-R' || :round || '-003', 'd0260000-0000-4000-8000-000000000002', 'MANUAL', gen_random_uuid(),
   'SO18-R' || :round || '-SRC-003', 500.0000, 0, 500.0000, 0, '2026-11-25', '2026-11', 'OPEN',
   'SO-18 验收种子 第' || :round || '轮（别家客户）');

SELECT ar_no, customer_id, amount, unsettled_amount, status
  FROM biz_receivable WHERE ar_no LIKE 'AR-SO18-R' || :round || '-%' ORDER BY ar_no;
