#!/usr/bin/env bash
# t_255fb28e 证据：两条代码变异反证（断言必须有牙）+ 还原后复绿
set -uo pipefail
U='postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@172.18.10.145:5432/cloudpivot_test_fxm109'
OUT=/root/t255
SRC=/opt/cloudpivot/backend/app/src/business/erp/report/fx_report_tests.rs
BAK=/root/t255_bak.rs
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend/app
export ERP_DB_URL="$U" SYS_DB_URL="$U" TASK_DB_URL="$U" P0FIX_DB_URL="$U"

residue() {
  psql "$U" -Atc "select 'M109夹具凭证='||count(*) from biz_voucher where summary='M109 夹具'"
  psql "$U" -Atc "select 'VOUCHER孤儿日志='||count(*) from biz_status_log where entity_type='VOUCHER' and entity_id not in (select id from biz_voucher)"
}

# 0) 起手还原到「已提交形态」并记 sha
python3 /root/t255_mutate.py restore "$SRC" > "$OUT/08_teeth_precheck.out" 2>&1
sha256sum "$SRC" >> "$OUT/08_teeth_precheck.out"

# 1) 变异 A：清场只删凭证，不删状态日志
{
  echo "=== 变异 A：purge_fixtures 里「删 VOUCHER 状态日志」那条腿的定位词改成永不匹配"
  python3 /root/t255_mutate.py A "$SRC"
  sha256sum "$SRC"
  echo "--- 跑收尾用例（期望 RED：日志 6 条删不掉）"
  cargo test -p cloudpivot --lib -- --test-threads=1 \
    business::erp::report::fx_report_tests::zz_teardown_purges_fixtures 2>&1
} > "$OUT/09_teeth_a_logs_leg.out" 2>&1

python3 /root/t255_mutate.py restore "$SRC" >> "$OUT/09_teeth_a_logs_leg.out" 2>&1

# 2) 变异 B：清场删凭证的判据改错（永不匹配）
{
  echo "=== 变异 B：purge_fixtures 里「删夹具凭证」的 voucher_word 改成永不匹配"
  python3 /root/t255_mutate.py B "$SRC"
  sha256sum "$SRC"
  echo "--- 跑收尾用例（期望 RED：清场删 0 张）"
  cargo test -p cloudpivot --lib -- --test-threads=1 \
    business::erp::report::fx_report_tests::zz_teardown_purges_fixtures 2>&1
} > "$OUT/10_teeth_b_voucher_leg.out" 2>&1

python3 /root/t255_mutate.py restore "$SRC" >> "$OUT/10_teeth_b_voucher_leg.out" 2>&1

# 3) 还原后复绿 + 残留 0
{
  echo "=== 还原后（sha 与备份逐字节一致）整模块复绿"
  sha256sum "$SRC" "$BAK"
  cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::report::fx_report_tests 2>&1
  residue
  echo "--- fmt / clippy"
  cargo fmt -p cloudpivot -- --check && echo "fmt rc=0"
  cargo clippy -p cloudpivot --lib --tests -- -D warnings 2>&1 | tail -3
} > "$OUT/11_after_restore_green.out" 2>&1

echo DONE_B
