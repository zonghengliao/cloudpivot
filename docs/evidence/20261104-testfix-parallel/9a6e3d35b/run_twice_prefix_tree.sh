#!/usr/bin/env bash
# 卡 t_9a6e3d35：干净隔离库上全量 cargo test -j 1 连跑 2 次（同一库，第二轮带第一轮残留）
# 被测树：/root/25926f63-tree = HEAD(0528788) + t_f404258c 的 5 个测试侧文件
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TREE=/root/25926f63-tree
export CARGO_TARGET_DIR=/root/9a6e3d35-target
export CARGO_BUILD_JOBS=2
DB=${DB:-cloudpivot_9a6e3d35b}
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB"
export SYS_DB_URL="$ERP_DB_URL"
export TASK_DB_URL="$ERP_DB_URL"
export NIK_PROFILE=test
OUT=/root/9a6e3d35b
mkdir -p "$OUT"

psql_q() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "$1"; }
fingerprint() {
  {
    echo "----- fingerprint [$1] db=$DB @ $(date '+%F %T %Z')"
    echo "  schema_migrations(max|count) = $(psql_q "select max(version::int)||'|'||count(*) from schema_migrations")"
    echo "  public 表数 = $(psql_q "select count(*) from information_schema.tables where table_schema='public'")"
    echo "  超管人数 = $(psql_q "select count(distinct u.id) from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN' and u.deleted_at is null")"
    echo "  admin = $(psql_q "select count(*) from sys_user where username='admin' and deleted_at is null")"
    echo "  biz_status_log = $(psql_q "select count(*) from biz_status_log")"
    echo "  biz_flow_instance = $(psql_q "select count(*) from biz_flow_instance")"
    echo "  biz_purchase_order = $(psql_q "select count(*) from biz_purchase_order")"
  } | tee -a "$OUT/fingerprints.txt"
}

cd "$TREE/backend" || exit 1

fingerprint before-run1

echo "[$(date '+%F %T')] 预编译测试二进制 cargo test -j 1 --no-run"
cargo test -j 1 --no-run > "$OUT/compile.log" 2>&1
echo "compile exit=$? $(tail -2 "$OUT/compile.log" | tr '\n' ' ')"

for i in 1 2; do
  t0=$(date +%s)
  cargo test -j 1 > "$OUT/run$i.log" 2>&1
  rc=$?
  t1=$(date +%s)
  line=$(grep -E '^test result:' "$OUT/run$i.log" | tail -1)
  nres=$(grep -c '^test result:' "$OUT/run$i.log")
  skip=$(grep -c '^SKIP' "$OUT/run$i.log")
  fails=$(grep -c -E '^test .* FAILED' "$OUT/run$i.log")
  {
    echo "round=$i exit=$rc elapsed=$((t1-t0))s start=$(date -d @$t0 '+%F %T') end=$(date -d @$t1 '+%F %T') result_lines=$nres SKIP=$skip FAILED_lines=$fails"
    echo "  $line"
  } | tee -a "$OUT/summary.tsv"
  grep -E '^test .* FAILED' "$OUT/run$i.log" > "$OUT/run$i.failures.txt" || true
  fingerprint "after-run$i"
done

{ echo "== 失败集合对比 =="
  if [ -s "$OUT/run1.failures.txt" ] || [ -s "$OUT/run2.failures.txt" ]; then
    echo "run1 失败 $(wc -l < "$OUT/run1.failures.txt") 条；run2 失败 $(wc -l < "$OUT/run2.failures.txt") 条"
    echo "-- 共同红 --"; comm -12 <(sort "$OUT/run1.failures.txt") <(sort "$OUT/run2.failures.txt")
    echo "-- 仅 run1 红 --"; comm -23 <(sort "$OUT/run1.failures.txt") <(sort "$OUT/run2.failures.txt")
    echo "-- 仅 run2 红 --"; comm -13 <(sort "$OUT/run1.failures.txt") <(sort "$OUT/run2.failures.txt")
  else
    echo "两轮均无 FAILED 行"
  fi
} | tee -a "$OUT/summary.tsv"

echo "DONE_TWO_ROUNDS"
