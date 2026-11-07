#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 取证 · 步骤 4：迁移幂等 + 全新库首启自检
#
# 跑法（在 145 上）：bash .../13_fresh_and_idempotency.sh
#
# ① 幂等：重启隔离实例（同一个库）→ 日志必须「应用 0 个迁移」，
#    `schema_migrations` 的 (version, applied_at) 指纹逐字不变。
# ② 全新库：建一个空库 → 首启跑到最新（V0325/V0326 的自检在空 schema 上也要过）
#    → 断言列 / 约束 / 可空性都在 → kill 掉（不留进程）。
# ============================================================================
set -euo pipefail

EV="${EV:-/opt/cloudpivot/docs/evidence/20261107-fix5-budget-flag}"
DB=$(cat /root/backup/.fix5_db)
ENVF=/etc/cloudpivot/cloudpivot-dev.env
set -a
# shellcheck disable=SC1090
. "$ENVF"
set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

psqlf() { psql -h 127.0.0.1 -U cloudpivot -d "$1" -X -P pager=off "${@:2}"; }

echo "=========== ① 迁移幂等（重启同库，期望「应用 0 个迁移」）==========="
BEFORE=$(psqlf "$DB" -tAc "select count(*)||'/'||coalesce(sum(hashtext(version||applied_at::text)),0) from schema_migrations")
BEFORE_MAX=$(psqlf "$DB" -tAc "select max(version) from schema_migrations")
echo "重启前：migrations=$BEFORE max_version=$BEFORE_MAX"

bash "$EV/12_restart_instance.sh" | tail -3

AFTER=$(psqlf "$DB" -tAc "select count(*)||'/'||coalesce(sum(hashtext(version||applied_at::text)),0) from schema_migrations")
AFTER_MAX=$(psqlf "$DB" -tAc "select max(version) from schema_migrations")
echo "重启后：migrations=$AFTER max_version=$AFTER_MAX"
if [ "$BEFORE" = "$AFTER" ] && [ "$BEFORE_MAX" = "$AFTER_MAX" ]; then
  echo "  ✅ 幂等：迁移指纹与版本号完全不变（重复启动 = 0 变更）"
else
  echo "  ❌ 幂等失败：重启前 [$BEFORE/$BEFORE_MAX] 重启后 [$AFTER/$AFTER_MAX]"
  exit 1
fi

echo
echo "=========== ② 全新库首启（空 schema 跑到最新，迁移自检必须过）==========="
TS=$(date +%Y%m%d-%H%M%S)
FRESH="cloudpivot_fix5fresh_${TS//-/}"
psqlf postgres -q -c "CREATE DATABASE $FRESH OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $FRESH OWNER cloudpivot\""
echo "fresh_db=$FRESH"

cd /opt/cloudpivot/backend
NIK_DATABASE_NAME="$FRESH" NIK_SERVER_LISTEN=127.0.0.1:8047 \
  setsid nohup ./target/debug/cloudpivot-fix5 > /root/backup/fix5_fresh.log 2>&1 < /dev/null &
FRESH_PID=$!
ok=0
for i in $(seq 1 120); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' http://127.0.0.1:8047/health || true)
  if [ "$code" = "200" ]; then ok=1; echo "  ✅ 空库首启 health=200（${i}s）"; break; fi
  sleep 1
done
if [ "$ok" != "1" ]; then
  echo "  ❌ 空库首启失败，日志尾部："; tail -30 /root/backup/fix5_fresh.log; kill "$FRESH_PID" || true; exit 1
fi
grep -E "迁移完成" /root/backup/fix5_fresh.log | tail -2

TOTAL=$(psqlf "$FRESH" -tAc "select count(*) from schema_migrations")
MAXV=$(psqlf "$FRESH" -tAc "select max(version) from schema_migrations")
COL=$(psqlf "$FRESH" -tAc "select count(*) from information_schema.columns where table_name='biz_expense_report' and column_name='missing_budget'")
CHK=$(psqlf "$FRESH" -tAc "select count(*) from pg_constraint where conname='ck_biz_exp_report_missing'")
NULLABLE=$(psqlf "$FRESH" -tAc "select count(*) from information_schema.columns where table_name in ('biz_expense_report','biz_budget_usage') and column_name='over_amount' and is_nullable='YES'")
DEFAULTED=$(psqlf "$FRESH" -tAc "select count(*) from information_schema.columns where table_name in ('biz_expense_report','biz_budget_usage') and column_name='over_amount' and column_default is not null")
echo "  migrations=$TOTAL max_version=$MAXV"
[ "$COL" = "1" ] && echo "  ✅ biz_expense_report.missing_budget 存在" || { echo "  ❌ missing_budget 缺失"; exit 1; }
[ "$CHK" = "1" ] && echo "  ✅ ck_biz_exp_report_missing 存在" || { echo "  ❌ 约束缺失"; exit 1; }
[ "$NULLABLE" = "2" ] && echo "  ✅ 两张表的 over_amount 都可空" || { echo "  ❌ over_amount 可空性不对（$NULLABLE/2）"; exit 1; }
[ "$DEFAULTED" = "0" ] && echo "  ✅ over_amount 无默认值（V0326）" || { echo "  ❌ over_amount 仍有 default（$DEFAULTED）"; exit 1; }

# 收尾：杀掉这个一次性实例（只用自己刚起的 PID）
kill "$FRESH_PID" 2>/dev/null || true
sleep 1
echo "fresh instance killed (pid=$FRESH_PID)"
echo
echo "RESULT=ALL PASS"
