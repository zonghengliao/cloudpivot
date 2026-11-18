#!/usr/bin/env bash
# t_255fb28e 证据：模块单跑 / 跳过收尾的复现 / 还原后复绿（不改源码）
set -uo pipefail
U='postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@172.18.10.145:5432/cloudpivot_test_fxm109'
OUT=/root/t255
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend/app
export ERP_DB_URL="$U" SYS_DB_URL="$U" TASK_DB_URL="$U" P0FIX_DB_URL="$U"

residue() {
  echo "--- 残留核对（psql $U）"
  psql "$U" -Atc "select 'M109夹具凭证='||count(*) from biz_voucher where summary='M109 夹具'"
  psql "$U" -Atc "select 'ERPFX9凭证='||count(*) from biz_voucher where voucher_word='ERPFX9'"
  psql "$U" -Atc "select 'VOUCHER孤儿日志='||count(*) from biz_status_log where entity_type='VOUCHER' and entity_id not in (select id from biz_voucher)"
  psql "$U" -Atc "select '2064期间='||count(*) from biz_accounting_period where period_code like '2064-%'"
  psql "$U" -Atc "select '2064牌价='||count(*) from biz_exchange_rate where from_currency in ('USD','EUR','HKD') and to_currency='CNY' and effective_date >= DATE '2064-01-01'"
  psql "$U" -Atc "select coalesce(string_agg(voucher_no||':'||status, ',' order by voucher_no),'(空)') from biz_voucher where voucher_word='ERPFX9'"
}

{
  echo "=== cargo test ... business::erp::report::fx_report_tests --nocapture"
  cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::report::fx_report_tests --nocapture 2>&1
  residue
} > "$OUT/06_module_green.out" 2>&1

{
  echo "=== 反证（复现修前症状）：跳过收尾用例 ⇒ 最后一条用例造的 6 张必然留在库里"
  echo "=== cargo test ... business::erp::report::fx_report_tests --skip zz_teardown_purges_fixtures"
  cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::report::fx_report_tests \
    --skip zz_teardown_purges_fixtures 2>&1
  residue
} > "$OUT/07_skip_zz_repro.out" 2>&1

{
  echo "=== 复绿：把收尾用例放回来再跑一次同模块（应把上一步的 6 张清干净）"
  cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::report::fx_report_tests 2>&1
  residue
} > "$OUT/07b_after_repro_clean.out" 2>&1

echo DONE_A
