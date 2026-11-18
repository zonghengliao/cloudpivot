#!/usr/bin/env bash
# t_60c193ef 的验收复现：全新种子库 → 同一库连跑两轮全量 → 查 ERPR1 残留
#
# 用法（在 /opt/cloudpivot 下）：
#   bash docs/evidence/2026-11-18-t60c193ef-report-erpr1-fixture-teardown/repro_two_rounds.sh
#
# 前置：backend/target/debug/cloudpivot 已 cargo build；backend/app 已 cargo test --lib --no-run
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
REPO=/opt/cloudpivot
EV=$REPO/docs/evidence/2026-11-18-t60c193ef-report-erpr1-fixture-teardown
DB=cloudpivot_test_erpr1
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
export ERP_DB_URL="postgres://cloudpivot:${PGPASSWORD}@172.18.10.145:5432/${DB}"
export SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL"

residue() {
  psql -h 172.18.10.145 -U cloudpivot -d "$DB" -Atc "
    select 'ERPR1凭证=' || (select count(*) from biz_voucher where voucher_word='ERPR1')
        || ' ERPR1分录=' || (select count(*) from biz_voucher_entry e
                              where e.voucher_id in (select id from biz_voucher where voucher_word='ERPR1'))
        || ' 2019期间=' || (select count(*) from biz_accounting_period where period_code like '2019-%')
        || ' 2020期间=' || (select count(*) from biz_accounting_period where period_code like '2020-%')
        || ' VOUCHER孤儿日志=' || (select count(*) from biz_status_log
                                   where entity_type='VOUCHER'
                                     and entity_id not in (select id from biz_voucher))
        || ' biz_voucher总数=' || (select count(*) from biz_voucher)
        || ' biz_accounting_period总数=' || (select count(*) from biz_accounting_period)"
}

echo "== ① 全新种子库 =="
ERP_DB=$DB bash "$REPO/scripts/verify/make_full_suite_db.sh" > "$EV/00b_dbseed_final.out" 2>&1
echo "SEED_RC=$?" | tee -a "$EV/00b_dbseed_final.out"
tail -3 "$EV/00b_dbseed_final.out"

cd "$REPO/backend/app" || exit 1
for round in 1 2; do
  echo "== ② 第 $round 轮全量（--test-threads=1，CWD=backend/app）=="
  if [ "$round" = 1 ]; then out="$EV/01_round1_summary.out"; else out="$EV/03_round2_summary.out"; fi
  timeout 1500 cargo test -p cloudpivot --lib -- --test-threads=1 > "$out" 2>&1
  echo "ROUND${round}_RC=$?" | tee -a "$out"
  grep -E "^test result:" "$out" | tail -1
  if [ "$round" = 1 ]; then residue | tee "$EV/02_residue_after_round1.txt"; else residue | tee "$EV/05_residue_after_round2.txt"; fi
done
echo "== ③ 两轮 summary 对照 =="
grep -hE "^test result:" "$EV/01_round1_summary.out" "$EV/03_round2_summary.out"
