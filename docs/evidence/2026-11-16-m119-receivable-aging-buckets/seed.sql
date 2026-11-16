-- M1-19 真接口验收夹具：应收账龄 30/60/90/180/365 七档
-- 目标库：cp_m119_api（cloudpivot_test 的克隆）—— 脚本自带清理，可重复跑。
-- 基准日固定 2026-11-05，各行到期日 = 基准日 − 逾期天数，正好落在每一档的边界上。

UPDATE biz_invoice SET receivable_id = NULL WHERE receivable_id IS NOT NULL;
DELETE FROM biz_receipt_allocation WHERE receivable_id IS NOT NULL;
DELETE FROM biz_receipt_refund_allocation WHERE receivable_id IS NOT NULL;
DELETE FROM biz_receivable;
DELETE FROM biz_customer WHERE code LIKE 'M119-%';

INSERT INTO biz_customer (id, code, name, status) VALUES
  ('b1190001-0000-4000-8000-000000000001'::uuid, 'M119-A', '甲客户', 'ACTIVE'),
  ('b1190001-0000-4000-8000-000000000002'::uuid, 'M119-B', '乙客户', 'ACTIVE');

INSERT INTO biz_receivable
  (ar_no, customer_id, source_type, source_id, source_no, currency_code,
   amount, received_amount, unsettled_amount, credit_days, due_date, period_code,
   status, settled_at, created_at, updated_at)
VALUES
  -- 甲
  ('AR-M119-0001', 'b1190001-0000-4000-8000-000000000001'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   100, 0, 100, 30, '2026-11-05'::date, '2026-11', 'OPEN', NULL, now(), now()),          -- 0 天 → 未到期
  ('AR-M119-0002', 'b1190001-0000-4000-8000-000000000001'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   200, 0, 200, 30, '2026-11-04'::date, '2026-11', 'OPEN', NULL, now(), now()),          -- 1 天 → 1-30
  ('AR-M119-0003', 'b1190001-0000-4000-8000-000000000001'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   350, 50, 300, 30, '2026-10-06'::date, '2026-10', 'PARTIAL', NULL, now(), now()),      -- 30 天 → 1-30（部分已收）
  ('AR-M119-0007', 'b1190001-0000-4000-8000-000000000001'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   700, 0, 700, 30, '2026-08-07'::date, '2026-08', 'OPEN', NULL, now(), now()),          -- 90 天 → 61-90
  ('AR-M119-000b', 'b1190001-0000-4000-8000-000000000001'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   1100, 0, 1100, 30, '2026-11-20'::date, '2026-11', 'OPEN', NULL, now(), now()),        -- −15 天 → 未到期
  ('AR-M119-000c', 'b1190001-0000-4000-8000-000000000001'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   1300, 0, 1300, 30, '2026-05-09'::date, '2026-05', 'OPEN', NULL, now(), now()),        -- 180 天 → 91-180（上界）
  ('AR-M119-000e', 'b1190001-0000-4000-8000-000000000001'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   1500, 0, 1500, 30, '2025-11-05'::date, '2025-11', 'OPEN', NULL, now(), now()),        -- 365 天 → 181-365（上界）
  -- 乙
  ('AR-M119-0004', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   400, 0, 400, 30, '2026-10-05'::date, '2026-10', 'OPEN', NULL, now(), now()),          -- 31 天 → 31-60
  ('AR-M119-0005', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   500, 0, 500, 30, '2026-09-06'::date, '2026-09', 'OPEN', NULL, now(), now()),          -- 60 天 → 31-60
  ('AR-M119-0006', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   600, 0, 600, 30, '2026-09-05'::date, '2026-09', 'OPEN', NULL, now(), now()),          -- 61 天 → 61-90
  ('AR-M119-0008', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   800, 0, 800, 30, '2026-08-06'::date, '2026-08', 'OPEN', NULL, now(), now()),          -- 91 天 → 91-180（下界）
  ('AR-M119-000d', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   1400, 0, 1400, 30, '2026-05-08'::date, '2026-05', 'OPEN', NULL, now(), now()),        -- 181 天 → 181-365（下界）
  ('AR-M119-000f', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   1600, 0, 1600, 30, '2025-11-04'::date, '2025-11', 'OPEN', NULL, now(), now()),        -- 366 天 → 365 天以上（下界）
  -- 不进账龄的两张终态（证明口径不是「把所有行都算上」）
  ('AR-M119-0009', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   50, 50, 0, 30, '2026-10-01'::date, '2026-10', 'SETTLED', now(), now(), now()),        -- 已收清
  ('AR-M119-000a', 'b1190001-0000-4000-8000-000000000002'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
   900, 0, 900, 30, '2026-10-01'::date, '2026-10', 'WRITTEN_OFF', now(), now(), now());  -- 坏账核销

SELECT count(*) AS rows, sum(unsettled_amount) AS unsettled FROM biz_receivable;
