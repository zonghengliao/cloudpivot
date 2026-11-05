#!/usr/bin/env bash
# P1-FIX-8B 报表项维护：隔离库 + 夹具（只在 99 本机，绝不碰生产 144 / 本机 cloudpivot）
#
#   bash scripts/verify/fix8b_report_item_db.sh recreate   # 1) 重建空库（随后起一次实例跑迁移 + bootstrap）
#   bash scripts/verify/fix8b_report_item_instance.sh start
#   bash scripts/verify/fix8b_report_item_db.sh seed       # 2) 插夹具（需要表已迁移）
#
# 为什么要夹具：迁移种出来的 53 个报表项在生产口径下全是 0（没有任何凭证），
# 「改一个报表项 → 报表金额跟着变」这条验收标准就验不出来。这里造一张平衡凭证：
#   借 1405 库存商品 1000 / 贷 2202 应付账款 1000（2026-11）
# 于是资产负债表上 存货 = 1000、应付账款 = 1000、资产 = 负债 = 1000（勾稽平）。
set -uo pipefail

DB=${DB:-cloudpivot_ri8}

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

-- 会计期间（2026-10 / 2026-11 都开账，报表要能取上期）
INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status, remark)
VALUES
  (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN', 'P1-FIX-8B 夹具'),
  (2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'OPEN', 'P1-FIX-8B 夹具')
ON CONFLICT (period_code) DO NOTHING;

-- 一张平衡凭证：借 1405 库存商品 1000 / 贷 2202 应付账款 1000
INSERT INTO biz_voucher
  (id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
   summary, total_debit, total_credit, entry_count, status, is_red)
VALUES
  (md5('fix8b:voucher:1')::uuid, '记', '记-202611-0001', '2026-11', '2026-11-05', 'MANUAL', 'MANUAL',
   'P1-FIX-8B 夹具：报表项取数验证', 1000, 1000, 2, 'POSTED', 0)
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_voucher_entry
  (id, voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT md5('fix8b:entry:1')::uuid, md5('fix8b:voucher:1')::uuid, 1, 'P1-FIX-8B 夹具 购入存货',
       a.id, a.account_code, a.account_name, 1000, 0
  FROM biz_account a WHERE a.account_code = '1405' AND a.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_voucher_entry
  (id, voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
SELECT md5('fix8b:entry:2')::uuid, md5('fix8b:voucher:1')::uuid, 2, 'P1-FIX-8B 夹具 应付货款',
       a.id, a.account_code, a.account_name, 0, 1000
  FROM biz_account a WHERE a.account_code = '2202' AND a.deleted_at IS NULL
ON CONFLICT (id) DO NOTHING;

COMMIT;

-- 自检：凭证平衡 + 报表项 53 行
SELECT v.voucher_no, v.total_debit, v.total_credit, v.entry_count,
       (SELECT count(*) FROM biz_voucher_entry e WHERE e.voucher_id = v.id) AS entries
  FROM biz_voucher v WHERE v.id = md5('fix8b:voucher:1')::uuid;

SELECT report_code, count(*) AS items, count(*) FILTER (WHERE is_active = 1) AS active
  FROM biz_report_item GROUP BY report_code ORDER BY report_code;
SQL
}

case "${1:-}" in
  recreate) recreate ;;
  seed) seed ;;
  *) echo "用法: $0 {recreate|seed}"; exit 1 ;;
esac
