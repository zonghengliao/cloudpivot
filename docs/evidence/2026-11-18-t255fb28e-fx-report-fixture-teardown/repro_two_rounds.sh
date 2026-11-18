#!/usr/bin/env bash
# t_255fb28e 验收驱动：全新种子库 → 同一库连跑两轮全量 → 夹具残留核对
set -uo pipefail
U='postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@172.18.10.145:5432/cloudpivot_test_fxm109'
OUT=/root/t255
mkdir -p "$OUT"
export PATH=/root/.cargo/bin:$PATH

counts() {
  psql "$U" -AtF'|' -c "select table_name, (xpath('/row/c/text()', query_to_xml(format('select count(*) as c from %I.%I', table_schema, table_name), false, true, '')))[1]::text::int from information_schema.tables where table_schema='public' and table_type='BASE TABLE' order by 1"
}

residue() {
  psql "$U" -Atc "select 'M109夹具凭证='||count(*) from biz_voucher where summary='M109 夹具'"
  psql "$U" -Atc "select 'ERPFX9凭证='||count(*) from biz_voucher where voucher_word='ERPFX9'"
  psql "$U" -Atc "select 'VOUCHER孤儿日志='||count(*) from biz_status_log where entity_type='VOUCHER' and entity_id not in (select id from biz_voucher)"
  psql "$U" -Atc "select '2064期间='||count(*) from biz_accounting_period where period_code like '2064-%'"
  psql "$U" -Atc "select '2064USD/EUR/HKD牌价='||count(*) from biz_exchange_rate where from_currency in ('USD','EUR','HKD') and to_currency='CNY' and effective_date >= DATE '2064-01-01' and effective_date < DATE '2065-01-01'"
  psql "$U" -Atc "select '夹具凭证明细(应为空)='||coalesce(string_agg(voucher_no||':'||status, ',' order by voucher_no),'(空)') from biz_voucher where voucher_word='ERPFX9'"
}

cd /opt/cloudpivot
ERP_DB=cloudpivot_test_fxm109 PORT=18255 LOG="$OUT/dbseed.log" PIDFILE="$OUT/dbseed.pid" \
  bash scripts/verify/make_full_suite_db.sh > "$OUT/00_dbseed.out" 2>&1
echo "dbseed RC=$?" >> "$OUT/00_dbseed.out"

cd /opt/cloudpivot/backend/app
export ERP_DB_URL="$U" SYS_DB_URL="$U" TASK_DB_URL="$U" P0FIX_DB_URL="$U"

cargo test -p cloudpivot --lib -- --test-threads=1 > "$OUT/01_round1.out" 2>&1
echo "round1 RC=$?" >> "$OUT/01_round1.out"
counts > "$OUT/02_counts_after_round1.txt"
residue > "$OUT/02b_residue_after_round1.txt"

cargo test -p cloudpivot --lib -- --test-threads=1 > "$OUT/03_round2.out" 2>&1
echo "round2 RC=$?" >> "$OUT/03_round2.out"
counts > "$OUT/04_counts_after_round2.txt"
residue > "$OUT/05_residue_after_round2.txt"

echo "ALL_DONE"
