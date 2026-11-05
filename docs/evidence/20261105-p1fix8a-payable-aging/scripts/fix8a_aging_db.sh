#!/usr/bin/env bash
# P1-FIX-8A 应付账龄：隔离库 + 夹具（只在 99 本机，绝不碰生产 144 / 本机 cloudpivot）
#
#   bash scripts/verify/fix8a_aging_db.sh recreate   # 1) 重建空库（随后起一次实例跑迁移 + bootstrap）
#   bash scripts/verify/fix8a_aging_instance.sh start
#   bash scripts/verify/fix8a_aging_db.sh seed       # 2) 插夹具（需要表已迁移）
#
# 为什么用空库而不是生产快照副本：
#   快照副本里的 admin 口令是生产的（未知），隔离实例登不进去；空库由 bootstrap 用
#   NIK_ADMIN_PASSWORD 建 admin，才能真机登录。生产快照的 9 张应付单全部「未到期」，
#   对分档验证本来也没有信息量 —— 边界覆盖靠这里的夹具。
#
# 夹具（基准日 2026-11-05）：
#   9 张纳入，覆盖 5 个档位的**每一个边界**（-15 / 0 / 1 / 30 / 31 / 60 / 61 / 90 / 91 天）
#   3 张排除样本：SETTLED（未付 0）/ CANCELLED（状态）/ period_code=2026-12（基准日之后）
set -uo pipefail

DB=${DB:-cloudpivot_aging8}

recreate() {
  echo "== 重建空库 $DB"
  runuser -u postgres -- psql -q -c "DROP DATABASE IF EXISTS $DB" || exit 1
  runuser -u postgres -- psql -q -c "CREATE DATABASE $DB OWNER alun" || exit 1
  echo "   $DB 已重建（空库，等实例跑迁移）"
}

seed() {
  echo "== 插夹具到 $DB"
  PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -q <<'SQL'
BEGIN;

-- 三家测试供应商（幂等：按 id 跳过）
INSERT INTO biz_supplier (id, supplier_code, supplier_name, credit_days, credit_limit, status, remark)
VALUES
  ('aa000000-0000-4000-8000-000000000001', 'FIX-AG-01', '甲方材料（账龄测试）', 30, 0, 1, 'P1-FIX-8A 夹具'),
  ('aa000000-0000-4000-8000-000000000002', 'FIX-AG-02', '乙方设备（账龄测试）', 60, 0, 1, 'P1-FIX-8A 夹具'),
  ('aa000000-0000-4000-8000-000000000003', 'FIX-AG-03', '丙方服务（账龄测试）', 90, 0, 1, 'P1-FIX-8A 夹具')
ON CONFLICT (id) DO NOTHING;

-- 12 张测试应付单（基准日 2026-11-05 → 逾期天数见行尾注释）
INSERT INTO biz_payable
  (id, payable_no, supplier_id, source_type, source_id, source_no, currency_code,
   amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, status, remark)
VALUES
  -- 未到期：15 天 / 到期当天（0 天，边界）
  (md5('fix8a:AP-FIX8A-000001')::uuid, 'AP-FIX8A-000001', 'aa000000-0000-4000-8000-000000000001', 'STOCK_IN',
   md5('fix8a:src:000001')::uuid, 'SRC-FIX8A-01', 'CNY', 12000, 0, 12000, 30, '2026-11-20', '2026-10', 'OPEN', '账龄夹具 未到期'),
  (md5('fix8a:AP-FIX8A-000002')::uuid, 'AP-FIX8A-000002', 'aa000000-0000-4000-8000-000000000001', 'STOCK_IN',
   md5('fix8a:src:000002')::uuid, 'SRC-FIX8A-02', 'CNY', 5000, 0, 5000, 0, '2026-11-05', '2026-10', 'OPEN', '账龄夹具 到期当天=未到期'),
  -- 1-30：1 天 / 30 天（边界）
  (md5('fix8a:AP-FIX8A-000003')::uuid, 'AP-FIX8A-000003', 'aa000000-0000-4000-8000-000000000001', 'STOCK_IN',
   md5('fix8a:src:000003')::uuid, 'SRC-FIX8A-03', 'CNY', 3000, 0, 3000, 0, '2026-11-04', '2026-10', 'OPEN', '账龄夹具 逾期1天'),
  (md5('fix8a:AP-FIX8A-000004')::uuid, 'AP-FIX8A-000004', 'aa000000-0000-4000-8000-000000000002', 'STOCK_IN',
   md5('fix8a:src:000004')::uuid, 'SRC-FIX8A-04', 'CNY', 8000, 0, 8000, 30, '2026-10-06', '2026-10', 'OPEN', '账龄夹具 逾期30天'),
  -- 31-60：31 天（部分已付）/ 60 天（边界）
  (md5('fix8a:AP-FIX8A-000005')::uuid, 'AP-FIX8A-000005', 'aa000000-0000-4000-8000-000000000002', 'STOCK_IN',
   md5('fix8a:src:000005')::uuid, 'SRC-FIX8A-05', 'CNY', 9000, 3000, 6000, 30, '2026-10-05', '2026-10', 'PARTIAL_PAID', '账龄夹具 逾期31天 部分已付'),
  (md5('fix8a:AP-FIX8A-000006')::uuid, 'AP-FIX8A-000006', 'aa000000-0000-4000-8000-000000000002', 'STOCK_IN',
   md5('fix8a:src:000006')::uuid, 'SRC-FIX8A-06', 'CNY', 11000, 0, 11000, 60, '2026-09-06', '2026-09', 'OPEN', '账龄夹具 逾期60天'),
  -- 61-90：61 天 / 90 天（边界）
  (md5('fix8a:AP-FIX8A-000007')::uuid, 'AP-FIX8A-000007', 'aa000000-0000-4000-8000-000000000003', 'STOCK_IN',
   md5('fix8a:src:000007')::uuid, 'SRC-FIX8A-07', 'CNY', 15000, 0, 15000, 90, '2026-09-05', '2026-09', 'OPEN', '账龄夹具 逾期61天'),
  (md5('fix8a:AP-FIX8A-000008')::uuid, 'AP-FIX8A-000008', 'aa000000-0000-4000-8000-000000000003', 'STOCK_IN',
   md5('fix8a:src:000008')::uuid, 'SRC-FIX8A-08', 'CNY', 22000, 0, 22000, 90, '2026-08-07', '2026-08', 'OPEN', '账龄夹具 逾期90天'),
  -- 90+：91 天（部分已付）
  (md5('fix8a:AP-FIX8A-000009')::uuid, 'AP-FIX8A-000009', 'aa000000-0000-4000-8000-000000000003', 'STOCK_IN',
   md5('fix8a:src:000009')::uuid, 'SRC-FIX8A-09', 'CNY', 40000, 10000, 30000, 90, '2026-08-06', '2026-08', 'PARTIAL_PAID', '账龄夹具 逾期91天 部分已付'),
  -- 排除样本：已结清（未付 0）/ 已作废 / 基准日之后的期间
  (md5('fix8a:AP-FIX8A-000010')::uuid, 'AP-FIX8A-000010', 'aa000000-0000-4000-8000-000000000001', 'STOCK_IN',
   md5('fix8a:src:000010')::uuid, 'SRC-FIX8A-10', 'CNY', 7777, 7777, 0, 30, '2026-09-01', '2026-09', 'SETTLED', '账龄夹具 已结清应排除'),
  (md5('fix8a:AP-FIX8A-000011')::uuid, 'AP-FIX8A-000011', 'aa000000-0000-4000-8000-000000000002', 'STOCK_IN',
   md5('fix8a:src:000011')::uuid, 'SRC-FIX8A-11', 'CNY', 8888, 0, 8888, 30, '2026-09-01', '2026-09', 'CANCELLED', '账龄夹具 已作废应排除'),
  (md5('fix8a:AP-FIX8A-000012')::uuid, 'AP-FIX8A-000012', 'aa000000-0000-4000-8000-000000000003', 'STOCK_IN',
   md5('fix8a:src:000012')::uuid, 'SRC-FIX8A-12', 'CNY', 99999, 0, 99999, 30, '2027-01-15', '2026-12', 'OPEN', '账龄夹具 基准日之后的期间应排除')
ON CONFLICT (id) DO NOTHING;

COMMIT;

-- 自检：12 张 = 9 纳入 + 3 排除样本
SELECT
  count(*) FILTER (WHERE status IN ('OPEN','PARTIAL_PAID','PENDING_CANCEL') AND unpaid_amount > 0
                     AND period_code <= '2026-11')                                   AS sheet_rows,
  count(*) FILTER (WHERE status IN ('SETTLED','CANCELLED') OR unpaid_amount = 0)      AS excluded_status,
  count(*) FILTER (WHERE period_code = '2026-12')                                     AS excluded_period,
  COALESCE(SUM(unpaid_amount) FILTER (WHERE status IN ('OPEN','PARTIAL_PAID','PENDING_CANCEL')
                                        AND unpaid_amount > 0 AND period_code <= '2026-11'), 0) AS unpaid_total
  FROM biz_payable
 WHERE payable_no LIKE 'AP-FIX8A-%' AND deleted_at IS NULL;
SQL

  echo "== 夹具分布（基准日 2026-11-05）"
  PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -P pager=off -c "
SELECT p.payable_no, s.supplier_name, p.due_date, (date '2026-11-05' - p.due_date) AS overdue_days,
       p.amount, p.paid_amount, p.unpaid_amount, p.status, p.period_code
  FROM biz_payable p LEFT JOIN biz_supplier s ON s.id = p.supplier_id
 WHERE p.payable_no LIKE 'AP-FIX8A-%' AND p.deleted_at IS NULL
 ORDER BY p.payable_no"
}

case "${1:-seed}" in
  recreate) recreate ;;
  seed) seed ;;
  *) echo "用法：$0 {recreate|seed}"; exit 2 ;;
esac
