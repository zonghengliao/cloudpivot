#!/usr/bin/env bash
# 根卡 t_e3cc469f 独立复跑：从 V0192 基线建探针库 → 应用 V0193..V0197 两遍 → 结构断言 → 15 条巡检 → 约束反例
set -uo pipefail
DB=arch2_root_poc
ROOT=/opt/cloudpivot
OUT=$ROOT/docs/evidence/2026-11-02-root-verify
LOG="$OUT/repro.log"
mkdir -p "$OUT"
: > "$LOG"

# 模板库 cloudpivot_t06d 的对象属主是 alun；p17role 无 CREATE、也不是表属主，
# 故所有执行走 postgres 超级用户（本机 root 可 su - postgres）。顺带证明：迁移不依赖属主身份。
runpsql() { su - postgres -c "psql -X -v ON_ERROR_STOP=1 -d $DB $(printf '%q ' "$@")"; }
say() { echo "$@" | tee -a "$LOG"; }

say "== [0] 重建探针库（模板 cloudpivot_t06d = V0001..V0192）=="
su - postgres -c "psql -X -Atc \"DROP DATABASE IF EXISTS $DB\"" >>"$LOG" 2>&1
su - postgres -c "psql -X -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB TEMPLATE cloudpivot_t06d\"" >>"$LOG" 2>&1
say "createdb exit=$?"

say "== [0b] 基线事实（表数 / 迁移上限 / 触发器 / 外键）=="
runpsql -Atc "select 'tables='||count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'
  union all select 'migration_max='||max(version) from schema_migrations
  union all select 'triggers='||count(*) from pg_trigger where not tgisinternal
  union all select 'foreign_keys='||count(*) from pg_constraint where contype='f'" | tee -a "$LOG"

MIGS="V0193__erp_contract V0194__erp_po_contract_bridge V0195__biz_snapshot_drift V0196__biz_contract_party_change V0197__biz_doc_trace_views"
for round in 1 2; do
  for f in $MIGS; do
    say "== [$round] apply $f =="
    runpsql -f "$ROOT/docs/evidence/2026-11-02-arch2-synthesis/$f.up.sql" >>"$LOG" 2>&1
    say "$f round$round exit=$?"
  done
done

say "== [3] 结构断言 =="
runpsql -Atc "
select 'biz_contract cols='||count(*) from information_schema.columns where table_name='biz_contract'
union all select 'biz_contract checks='||count(*) from pg_constraint where conrelid='public.biz_contract'::regclass and contype='c'
union all select 'biz_snapshot_drift cols='||count(*) from information_schema.columns where table_name='biz_snapshot_drift'
union all select 'biz_contract_party_change cols='||count(*) from information_schema.columns where table_name='biz_contract_party_change'
union all select 'po_new_cols='||count(*) from information_schema.columns where table_name='biz_purchase_order' and column_name in ('contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot')
union all select 'views_v0197='||count(*) from information_schema.views where table_name in ('v_biz_doc_edge','v_biz_doc_ref')
union all select 'trace_edges_rows='||count(*) from v_biz_doc_edge
union all select 'trace_ref_rows='||count(*) from v_biz_doc_ref
union all select 'biz_contract_fk='||count(*) from pg_constraint where conrelid='public.biz_contract'::regclass and contype='f'" | tee -a "$LOG"

say "== [4] 安装巡检兼容层 + 15 条定义 + 执行器，并跑一轮 =="
for f in 00_install_compat 10_checks 20_runner_install; do
  runpsql -f "$ROOT/docs/architecture/snapshot-drift/$f.sql" >>"$LOG" 2>&1
  say "$f exit=$?"
done
runpsql -f "$ROOT/docs/architecture/snapshot-drift/21_run_once.sql" >>"$LOG" 2>&1
say "21_run_once exit=$?"

say "== [5] 15 条巡检逐条执行（空数据应 0 行且无报错）=="
FAIL=0
for i in 01 02 03 04 05 06 07 08 09 10 11 12 13 14 15; do
  N=$(runpsql -Atc "select count(*) from di.c_di$i" 2>>"$LOG")
  RC=$?
  [ $RC -ne 0 ] && FAIL=$((FAIL+1))
  say "DI-$i rows=${N:-ERR} rc=$RC"
done
say "check_errors=$FAIL"

say "== [6] 约束反例（9 条，期望全被拦）=="
runpsql -f "$ROOT/docs/evidence/2026-11-02-arch2-synthesis/constraint_negative_tests.sql" >"$OUT/constraint_negative_tests.log" 2>&1
say "negative_tests exit=$?"
say "negative_23514=$(grep -c '23514' "$OUT/constraint_negative_tests.log") negative_23505=$(grep -c '23505' "$OUT/constraint_negative_tests.log")"

say "== DONE =="
