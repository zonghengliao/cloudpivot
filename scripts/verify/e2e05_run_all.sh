#!/usr/bin/env bash
# E2E-05 资产域演练：一键全量跑（重置 → A/B/C → 勾稽 SQL），日志落 $OUT
set -uo pipefail
OUT=${OUT:-/tmp/e2e05/logs}
mkdir -p "$OUT"
export PGPASSWORD=$(grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2)
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -q -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/scripts/verify/e2e05_reset.sql > "$OUT/reset.log" 2>&1 || { tail -5 "$OUT/reset.log"; exit 1; }
mkdir -p /tmp/e2e05; : > /tmp/e2e05/state.sh; : > /tmp/e2e05/asserts.log
rc=0
for s in a b c; do
  bash "/opt/cloudpivot/scripts/verify/e2e05_asset_$s.sh" > "$OUT/$s.log" 2>&1 || rc=1
  tail -2 "$OUT/$s.log"
done
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X \
  -f /opt/cloudpivot/scripts/verify/e2e05_reconcile.sql > "$OUT/reconcile.log" 2>&1
sed -n '1,40p' /tmp/e2e05/raw/gm_todo.json > "$OUT/todo_sample.json" 2>/dev/null
echo "== PASS=$(grep -c '^PASS' /tmp/e2e05/asserts.log) FAIL=$(grep -c '^FAIL' /tmp/e2e05/asserts.log)"
grep '^FAIL' /tmp/e2e05/asserts.log && rc=1
cp /tmp/e2e05/asserts.log "$OUT/asserts.log"
cp /tmp/e2e05/state.sh "$OUT/state.sh"
exit $rc
