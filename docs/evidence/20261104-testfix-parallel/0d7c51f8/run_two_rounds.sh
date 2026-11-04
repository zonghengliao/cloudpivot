#!/usr/bin/env bash
# 卡 t_0d7c51f8 收口复验：HEAD(64b73fb) + t_f404258c 5 文件（工作树逐字节一致）
#   1) 新建干净库 cloudpivot_0d7c51f8b（树内 release 二进制自举：迁移 + bootstrap）
#   2) 同一库上 cargo test -j 1 连跑 2 轮（第 2 轮带第 1 轮残留）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TREE=/root/25926f63-tree
export CARGO_TARGET_DIR=/root/9a6e3d35-target
export CARGO_BUILD_JOBS=2
DB=cloudpivot_0d7c51f8b
OUT=/root/0d7c51f8
mkdir -p "$OUT"

echo "== 0) 被测树指纹 =="
cd "$TREE" && git -C /opt/cloudpivot rev-parse --short HEAD | sed 's/^/base_commit=/'
sha256sum /root/9a6e3d35-target/release/cloudpivot | tee "$OUT/release-bin.sha256"
for f in contract_approval_tests.rs fixture_hygiene.rs period_items_tests.rs period_test_support.rs purchase_handler.rs; do
  cmp -s "/opt/cloudpivot/backend/src/business/erp/$f" "$TREE/backend/src/business/erp/$f" \
    && echo "tree_file=$f SAME_AS_WORKTREE" || echo "tree_file=$f DIFF"
done | tee "$OUT/tree-files.txt"

echo "== 1) 重建干净库 $DB =="
ERP_DB="$DB" RECREATE=1 REPO="$TREE" BIN=/root/9a6e3d35-target/release/cloudpivot \
  LOG="$OUT/clean-db.log" PIDFILE="$OUT/clean-db.pid" PORT=18126 \
  bash "$TREE/scripts/verify/erp_clean_db.sh" > "$OUT/clean-db.out" 2>&1
echo "clean_db_exit=$?" | tee -a "$OUT/summary.tsv"
grep -E "created:|迁移完成|schema_migrations|超管人数" "$OUT/clean-db.out" | head -6

export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB"
export SYS_DB_URL="$ERP_DB_URL"
export TASK_DB_URL="$ERP_DB_URL"
export NIK_PROFILE=test

psql_q() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "$1"; }
fp() {
  { echo "----- [$1] @ $(date '+%F %T %Z')"
    echo "  migrations=$(psql_q "select max(version::int)||'|'||count(*) from schema_migrations") tables=$(psql_q "select count(*) from information_schema.tables where table_schema='public'") admin=$(psql_q "select count(*) from sys_user where username='admin' and deleted_at is null") supers=$(psql_q "select count(distinct u.id) from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN' and u.deleted_at is null")"
    echo "  biz_task=$(psql_q "select count(*) from biz_task") biz_flow_instance=$(psql_q "select count(*) from biz_flow_instance") biz_purchase_order=$(psql_q "select count(*) from biz_purchase_order") biz_status_log=$(psql_q "select count(*) from biz_status_log")"
  } | tee -a "$OUT/fingerprints.txt"
}

cd "$TREE/backend" || exit 1
fp before-run1
for i in 1 2; do
  t0=$(date +%s)
  cargo test -j 1 > "$OUT/run$i.log" 2>&1
  rc=$?
  t1=$(date +%s)
  {
    echo "round=$i exit=$rc elapsed=$((t1-t0))s start=$(date -d @$t0 '+%F %T') end=$(date -d @$t1 '+%F %T') targets=$(grep -c '^test result:' "$OUT/run$i.log") SKIP=$(grep -c '^SKIP' "$OUT/run$i.log")"
    grep -E '^test result:' "$OUT/run$i.log"
  } | tee -a "$OUT/summary.tsv"
  grep -E '^test .* FAILED' "$OUT/run$i.log" > "$OUT/run$i.failures.txt" || true
  fp "after-run$i"
done
echo "== FAILED 行（两轮）=="
cat "$OUT/run1.failures.txt" "$OUT/run2.failures.txt" 2>/dev/null | sort | uniq -c | tee -a "$OUT/summary.tsv"
echo ALL_DONE
