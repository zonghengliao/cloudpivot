#!/usr/bin/env bash
# ==============================================================================
# so39_boot_health.sh —— 「已迁到最新」的库再起一次实例，证明 V0272 是幂等收尾
#
# 判据（references/pre-submit-traps.md §1b）：
#   ① 日志必须出现 `database connection established … url=…/<库>`（跑错目录时没有这行）
#   ② `数据库迁移完成: 应用 0 个迁移`（V0272 已记录 ⇒ 不应重放，也就不会因探针重跑而漂移）
#   ③ /health 返回 200
#   ④ schema_migrations 的 V0272 仍是一条 kind=versioned / applied_via=executed / success=t
#
# 用法：bash scripts/verify/so39_boot_health.sh [库名]
# ==============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

DB=${1:-cloudpivot_so39_clean}
PORT=${PORT:-18073}
REPO=${REPO:-/opt/cloudpivot}
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/so39-boot.log}
PIDFILE=${PIDFILE:-/root/so39-boot.pid}
DB_USER=${DB_USER:-alun}
DB_PASS=${DB_PASS:-alun}

fail=0
ok()  { echo "  [OK]   $1"; }
bad() { echo "  [FAIL] $1"; fail=1; }
q() { PGPASSWORD="$DB_PASS" psql -h 127.0.0.1 -U "$DB_USER" -d "$DB" -Atc "$1"; }

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER="$DB_USER"
export NIK_DATABASE_PASSWORD="$DB_PASS"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD='Admin@123456'

: > "$LOG"
cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"

ready=0
for _ in $(seq 1 120); do
  grep -q "数据库迁移完成" "$LOG" 2>/dev/null && { ready=1; break; }
  kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
  sleep 1
done

health_code=$(curl -s -o /tmp/so39_health.json -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
kill "$(cat "$PIDFILE")" 2>/dev/null
sleep 1

echo "==> 启动日志关键行"
grep -E "database connection established|数据库迁移完成" "$LOG" | sed 's/^/    /' | head -4

if grep -q "database connection established" "$LOG"; then
  ok "连到目标库（不是跑错目录）"
else
  bad "日志里没有 database connection established（检查 cd backend / 库名）"
fi

if [ "$ready" = "1" ] && grep -q "数据库迁移完成: 应用 0 个迁移" "$LOG"; then
  ok "应用 0 个迁移（V0272 已记账，不重放）"
else
  bad "迁移行不是「应用 0 个迁移」：$(grep '数据库迁移完成' "$LOG" | tail -1)"
fi

if [ "$health_code" = "200" ]; then
  ok "/health = 200（$(cat /tmp/so39_health.json 2>/dev/null | head -c 120)）"
else
  bad "/health = ${health_code:-<无响应>}"
fi

rec=$(q "select version||'|'||kind||'|'||applied_via||'|'||success from schema_migrations where version='272'")
if [ "$rec" = "272|versioned|executed|true" ]; then
  ok "schema_migrations V0272 = $rec"
else
  bad "schema_migrations V0272 = ${rec:-<空>}（期望 272|versioned|executed|true）"
fi

n=$(q "select count(*) from biz_currency where is_base")
if [ "$n" = "1" ]; then ok "重启后本位币仍恰好 1 行"; else bad "重启后本位币行数 = $n"; fi

echo ""
if [ "$fail" = "0" ]; then
  echo "===== boot health PASS ====="
  exit 0
else
  echo "===== boot health FAIL ====="
  exit 1
fi
