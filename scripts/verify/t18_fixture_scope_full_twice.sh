#!/usr/bin/env bash
# 卡 t_18dcb777 自证 ③：干净隔离库上**全库** `cargo test -j 1` 连跑 2 轮，第 2 轮在**同一库的残留**上。
#
# 关键点：
#  * SYS_DB_URL 必须与 ERP_DB_URL 同值（system::* 的真库用例只认 SYS_DB_URL，
#    缺它 = 打印 SKIP 后 return = 假绿）。
#  * 测试线程数用 libtest 默认值（= CPU 数，本机 4）：卡面明确禁止拿 -j 1 / --test-threads=1 当修复手段，
#    `-j 1` 只是编译并行度。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
REPO=${REPO:-/opt/cloudpivot}
DB=${ERP_DB:-cloudpivot_18dcb777}
ROUNDS=${ROUNDS:-2}
RECREATE=${RECREATE:-1}
LOGDIR=${LOGDIR:-/root/18dcb777/full}

mkdir -p "$LOGDIR"
if [ "$RECREATE" = "1" ]; then
  ERP_DB="$DB" RECREATE=1 LOG="$LOGDIR/clean-db.log" PIDFILE="$LOGDIR/clean-db.pid" \
    bash "$REPO/scripts/verify/erp_clean_db.sh" > "$LOGDIR/clean-db.out" 2>&1 || {
      echo "建库失败，见 $LOGDIR/clean-db.out"; exit 1; }
  grep -E "schema_migrations|超管人数" "$LOGDIR/clean-db.out"
fi

export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/${DB}"
export SYS_DB_URL="$ERP_DB_URL"
cd "$REPO/backend" || exit 1

fail=0
for i in $(seq 1 "$ROUNDS"); do
  log="$LOGDIR/run$i.log"
  start=$(date +%s)
  cargo test -j 1 > "$log" 2>&1
  code=$?
  dur=$(( $(date +%s) - start ))
  line=$(grep -E "^test result:" "$log" | tail -1)
  skips=$(grep -c "^SKIP" "$log")
  echo "round=$i exit=$code 耗时=${dur}s lib: ${line:-<无 test result 行>} SKIP=$skips" | tee -a "$LOGDIR/summary.txt"
  grep -E "^test .* FAILED" "$log" | sed 's/^/   FAILED: /' | tee -a "$LOGDIR/summary.txt"
  [ "$code" -ne 0 ] && fail=1
done
echo "=== $([ "$fail" -eq 0 ] && echo "PASS：${ROUNDS} 轮 0 failed" || echo "FAIL：见 $LOGDIR")" | tee -a "$LOGDIR/summary.txt"
exit "$fail"
