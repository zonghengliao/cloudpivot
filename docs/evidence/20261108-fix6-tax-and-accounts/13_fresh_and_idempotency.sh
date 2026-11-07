#!/usr/bin/env bash
# ============================================================================
# FIX-6 145 取证 · 步骤 3：幂等（同库再启「应用 0 个迁移」）+ 全新库首启（迁移链完整）
# ============================================================================
set -uo pipefail
DB=$(cat /root/backup/.fix6_db)
EV=/opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts
mkdir -p "$EV/raw"
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)

fp() { psql -h 127.0.0.1 -U cloudpivot -d "$1" -X -tAc \
  "select count(*) || '|' || md5(string_agg(kind||name||coalesce(version,'')||checksum, ',' order by kind,name)) from schema_migrations"; }

echo "=== A) 同库重启：应「应用 0 个迁移」，指纹不变 ===" | tee "$EV/raw/idempotency.log"
BEFORE=$(fp "$DB")
echo "before=$BEFORE" | tee -a "$EV/raw/idempotency.log"
bash "$EV/14_kill_instance.sh" >>"$EV/raw/idempotency.log" 2>&1
bash "$EV/11_start_instance.sh" 2>&1 | tee -a "$EV/raw/idempotency.log" | tail -4
AFTER=$(fp "$DB")
echo "after =$AFTER" | tee -a "$EV/raw/idempotency.log"
if [ "$BEFORE" = "$AFTER" ]; then
  echo "✅ IDEMPOTENT：迁移指纹与条数不变" | tee -a "$EV/raw/idempotency.log"
else
  echo "❌ 指纹变化" | tee -a "$EV/raw/idempotency.log"; exit 1
fi
grep -E "应用 [0-9]+ 个迁移" /root/backup/fix6_instance.log | tail -2 | tee -a "$EV/raw/idempotency.log"

echo | tee -a "$EV/raw/idempotency.log"
echo "=== B) 全新空库首启：迁移链（含 V0327/28/29）在空库上自检全过 ===" | tee -a "$EV/raw/idempotency.log"
FRESH="cloudpivot_fix6_fresh_$(date +%H%M%S)"
su - postgres -c "psql -X -q -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $FRESH OWNER cloudpivot\"" | tee -a "$EV/raw/idempotency.log"
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
cd /opt/cloudpivot/backend
NIK_DATABASE_NAME="$FRESH" NIK_SERVER_LISTEN=127.0.0.1:8049 setsid nohup ./target/debug/cloudpivot-fix6 \
  > /root/backup/fix6_fresh.log 2>&1 < /dev/null &
FRESH_PID=$!
for i in $(seq 1 120); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' http://127.0.0.1:8049/health || true)
  [ "$code" = "200" ] && break
  sleep 1
done
echo "fresh health=$code" | tee -a "$EV/raw/idempotency.log"
grep -E "数据库迁移完成" /root/backup/fix6_fresh.log | tail -2 | tee -a "$EV/raw/idempotency.log"
psql -h 127.0.0.1 -U cloudpivot -d "$FRESH" -X -c \
  "select name, version from schema_migrations where version in ('327','328','329') order by version::int" \
  | tee -a "$EV/raw/idempotency.log"
psql -h 127.0.0.1 -U cloudpivot -d "$FRESH" -X -c \
  "select (select count(*) from information_schema.columns where table_name='biz_invoice' and column_name='tax_rate_mode') as invoice_mode_col,
          (select count(*) from information_schema.columns where table_name='biz_report_item' and column_name='reclass_negative_to') as reclass_col,
          (select config_value from sys_config where config_key='inventory_opening_equity_account') as equity_cfg" \
  | tee -a "$EV/raw/idempotency.log"
kill "$FRESH_PID" 2>/dev/null
sleep 1
pkill -f 'cloudpivot-fix6' 2>/dev/null
echo "（空库实例已 kill；重启拷贝库实例供后续复核）" | tee -a "$EV/raw/idempotency.log"
bash "$EV/11_start_instance.sh" 2>&1 | tail -3 | tee -a "$EV/raw/idempotency.log"
