#!/usr/bin/env bash
# 卡 t_9a6e3d35 正式验收：重建干净库 → 全量 cargo test -j 1 × 2（第二轮带残留）
# 被测树 = HEAD(0528788) + t_f404258c 5 文件 + tests/batchT_t1_task.rs 的 DB_LOCK 改造
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TREE=/root/25926f63-tree
export CARGO_TARGET_DIR=/root/9a6e3d35-target
export CARGO_BUILD_JOBS=2
DB=${DB:-cloudpivot_9a6e3d35b}
OUT=/root/9a6e3d35b
mkdir -p "$OUT"

# ---- 1) 重建干净库（树内 release 二进制自举）----
ERP_DB="$DB" RECREATE=1 REPO="$TREE" BIN=/root/9a6e3d35-target/release/cloudpivot \
  LOG="$OUT/clean2-db.log" PIDFILE="$OUT/clean2-db.pid" PORT=18096 \
  bash "$TREE/scripts/verify/erp_clean_db.sh" > "$OUT/clean2-db.out" 2>&1
echo "clean_db_exit=$?"
grep -E "created:|schema_migrations|超管人数" "$OUT/clean2-db.out" | head -5

export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB"
export SYS_DB_URL="$ERP_DB_URL"
export TASK_DB_URL="$ERP_DB_URL"
export NIK_PROFILE=test

psql_q() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "$1"; }
fp() {
  { echo "----- [$1] @ $(date '+%F %T %Z')"
    echo "  migrations=$(psql_q "select max(version::int)||'|'||count(*) from schema_migrations") tables=$(psql_q "select count(*) from information_schema.tables where table_schema='public'") admin=$(psql_q "select count(*) from sys_user where username='admin' and deleted_at is null") supers=$(psql_q "select count(distinct u.id) from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN' and u.deleted_at is null")"
    echo "  biz_task=$(psql_q "select count(*) from biz_task") biz_flow_instance=$(psql_q "select count(*) from biz_flow_instance") biz_purchase_order=$(psql_q "select count(*) from biz_purchase_order") biz_status_log=$(psql_q "select count(*) from biz_status_log")"
  } | tee -a "$OUT/fingerprints2.txt"
}

cd "$TREE/backend" || exit 1
fp before-run3

for i in 3 4; do
  t0=$(date +%s)
  cargo test -j 1 > "$OUT/run$i.log" 2>&1
  rc=$?
  t1=$(date +%s)
  {
    echo "round=$i exit=$rc elapsed=$((t1-t0))s start=$(date -d @$t0 '+%F %T') end=$(date -d @$t1 '+%F %T') result_lines=$(grep -c '^test result:' "$OUT/run$i.log") SKIP=$(grep -c '^SKIP' "$OUT/run$i.log")"
    grep -E '^test result:' "$OUT/run$i.log"
  } | tee -a "$OUT/summary2.tsv"
  grep -E '^test .* FAILED' "$OUT/run$i.log" > "$OUT/run$i.failures.txt" || true
  fp "after-run$i"
done

{ echo "== run3/run4 FAILED 行 =="
  cat "$OUT/run3.failures.txt" "$OUT/run4.failures.txt" 2>/dev/null | sort | uniq -c
  echo "== 结束 =="
} | tee -a "$OUT/summary2.tsv"
echo "DONE_ACCEPT_ROUNDS"
