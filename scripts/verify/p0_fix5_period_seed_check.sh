#!/usr/bin/env bash
# ============================================================================
# P0-FIX-5 复验脚本：新库自动开账（V0221 种子 + 启动保底）
#
# 做两件事，全部打在**隔离库**上，不碰生产 cloudpivot：
#
#   ① 全新库：从零起实例（真引擎跑迁移）→ 当年 12 期 → 不变量 F2 绿
#      → 再跑 up/down/up 验幂等 → 删掉一期再重启验「启动保底」
#   ② 存量库：把评审用的库（默认 cloudpivot_audit，含 5 张无期间凭证）整库复制
#      → 起实例应用 V0220/V0221 → F2 从 WARN 转绿、40/40 全绿
#
# 用法：
#   bash scripts/verify/p0_fix5_period_seed_check.sh              # 默认库名/端口
#   bash scripts/verify/p0_fix5_period_seed_check.sh fresh_db 18121
#   SRC_DB=cloudpivot_audit bash scripts/verify/p0_fix5_period_seed_check.sh
#
# 前置：backend/target/debug/cloudpivot 已是最新（cargo build --bin cloudpivot）；
#       psql 用 alun/alun 连 127.0.0.1:5432（与 scripts/verify/d1_migrate.sh 同口径）。
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
export PGPASSWORD=${PGPASSWORD:-alun}

ROOT=/opt/cloudpivot
FRESH_DB=${1:-cp_p5_verify}
PORT=${2:-18121}
SRC_DB=${SRC_DB:-cloudpivot_audit}
F2_DB="${FRESH_DB}_f2"
BIN="$ROOT/backend/target/debug/cloudpivot"
MIG_DIR="$ROOT/backend/migrations/business"
LOG_DIR=${LOG_DIR:-/root/.cloudpivot/p0fix5}
mkdir -p "$LOG_DIR"

PSQL="psql -h 127.0.0.1 -U alun -v ON_ERROR_STOP=1"
FAIL=0

step() { printf '\n\033[1m==== %s ====\033[0m\n' "$*"; }
ok()   { printf '  ✅ %s\n' "$*"; }
bad()  { printf '  ❌ %s\n' "$*"; FAIL=$((FAIL + 1)); }
expect_eq() { # 期望 实际 说明
  if [ "$1" = "$2" ]; then ok "$3（= $2）"; else bad "$3：期望 $1，实际 $2"; fi
}

q() { $PSQL -d "$1" -Atc "$2"; }

newdb() { # 用 postgres 建库（alun 无 CREATEDB 权限）
  su postgres -c "dropdb --if-exists $1" >/dev/null 2>&1
  su postgres -c "createdb -O alun $1" || return 1
}

# 起实例并把迁移跑完；$1=库 $2=端口 $3=日志
boot() {
  local db="$1" port="$2" log="$3"
  rm -f "$log"
  ( cd "$ROOT/backend" || exit 1
    export NIK_PROFILE=test \
           NIK_DATABASE_NAME="$db" NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
           NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
           NIK_SERVER_LISTEN="127.0.0.1:$port" \
           NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)" \
           CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)" \
           NIK_ADMIN_PASSWORD='Admin@123456'
    exec "$BIN" >>"$log" 2>&1 < /dev/null ) &
  echo $! >"/tmp/p0fix5.pid"
  local pid
  pid=$(cat /tmp/p0fix5.pid)
  for _ in $(seq 1 180); do
    grep -q "数据库迁移完成" "$log" 2>/dev/null && return 0
    kill -0 "$pid" 2>/dev/null || { echo "实例提前退出，看 $log"; return 1; }
    sleep 1
  done
  echo "等待迁移超时，看 $log"
  return 1
}

stop() {
  local pid
  pid=$(cat /tmp/p0fix5.pid 2>/dev/null) || return 0
  kill "$pid" 2>/dev/null
  for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
  rm -f /tmp/p0fix5.pid
}

invariants() { # $1=库
  NIK_DATABASE_NAME="$1" NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
    python3 "$ROOT/scripts/verify/erp_invariants.py" 2>&1
}

trap stop EXIT

# ══════════════════════════════════════════════════════════════════════
step "① 全新隔离库 $FRESH_DB —— 真引擎迁移"
# ══════════════════════════════════════════════════════════════════════
newdb "$FRESH_DB" || { echo "建库失败"; exit 1; }
boot "$FRESH_DB" "$PORT" "$LOG_DIR/fresh-migrate.log" || { tail -20 "$LOG_DIR/fresh-migrate.log"; exit 1; }
tail -3 "$LOG_DIR/fresh-migrate.log"

YEAR=$(q "$FRESH_DB" "select extract(year from current_date)::int")
MAXV=$(q "$FRESH_DB" "select max(version::int) from schema_migrations")
echo "  ℹ️  schema_migrations 最大版本 = $MAXV（共享工作区里别人的迁移也在跑，本卡只锚 V0221）"
expect_eq "1" "$(q "$FRESH_DB" "select count(*) from schema_migrations where version='221' and name like '%accounting_period_year%'")" "V0221 已应用并记录在 schema_migrations"
expect_eq "12" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "当年 $YEAR 期间行数"
expect_eq "12" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR and status='OPEN'")" "当年期间全部 OPEN"
expect_eq "0" "$(q "$FRESH_DB" "select count(*) from generate_series(1,12) m where not exists (select 1 from biz_accounting_period p where p.period_year=$YEAR and p.period_month=m and p.period_code=$YEAR||'-'||lpad(m::text,2,'0'))")" "12 个月编码齐备"
expect_eq "0" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR and (extract(day from end_date) < 28 or start_date <> make_date($YEAR, period_month, 1))")" "区间是自然月（起始 1 号、月末 ≥28）"
grep -q "会计期间保底" "$LOG_DIR/fresh-migrate.log" && ok "启动保底日志出现：$(grep -o '会计期间保底[^\"]*' "$LOG_DIR/fresh-migrate.log" | head -1)" || bad "启动保底日志缺失"

invariants "$FRESH_DB" > "$LOG_DIR/fresh-invariants-round.txt" 2>&1
tail -4 "$LOG_DIR/fresh-invariants-round.txt"
grep -q "F2 凭证期间必有期间记录" "$LOG_DIR/fresh-invariants-round.txt" && \
  grep -A1 "F2 凭证期间必有期间记录" "$LOG_DIR/fresh-invariants-round.txt" | grep -q "✅" \
  && ok "不变量 F2 绿（全新库）" || bad "不变量 F2 不是绿"
grep -q "BLOCKER 0" "$LOG_DIR/fresh-invariants-round.txt" && ok "BLOCKER 0" || bad "出现 BLOCKER"

# ══════════════════════════════════════════════════════════════════════
step "①b 迁移脚本 up / down / up 幂等（同一库，psql 直跑迁移文件）"
# ══════════════════════════════════════════════════════════════════════
UP="$MIG_DIR/V0221__erp_seed_accounting_period_year.up.sql"
DOWN="$MIG_DIR/V0221__erp_seed_accounting_period_year.down.sql"
$PSQL -q -d "$FRESH_DB" -f "$UP"   2>&1 | sed 's/^/    /'
expect_eq "12" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "第 2 次 up 后行数（幂等）"
$PSQL -q -d "$FRESH_DB" -f "$DOWN" 2>&1 | sed 's/^/    /'
expect_eq "1" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "down 后只剩 V0137 的 $YEAR-10 种子行"
expect_eq "1" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_code='$YEAR-10'")" "V0137 字面 id 的种子行没被误删"
$PSQL -q -d "$FRESH_DB" -f "$DOWN" 2>&1 | sed 's/^/    /'
expect_eq "1" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "第 2 次 down 后行数（幂等）"
$PSQL -q -d "$FRESH_DB" -f "$UP"   2>&1 | sed 's/^/    /'
expect_eq "12" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "再 up 后回到 12 行"

# ══════════════════════════════════════════════════════════════════════
step "①c 跨年通道：删掉一期 → 重启 → 启动保底补回（不必等明年）"
# ══════════════════════════════════════════════════════════════════════
q "$FRESH_DB" "delete from biz_accounting_period where period_code='$YEAR-06'" >/dev/null
expect_eq "11" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "人为删掉 $YEAR-06 后行数"
stop
boot "$FRESH_DB" "$PORT" "$LOG_DIR/fresh-reboot.log" || { tail -20 "$LOG_DIR/fresh-reboot.log"; exit 1; }
expect_eq "12" "$(q "$FRESH_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "重启后行数（启动保底补回）"
grep -q "已补齐当年缺失的会计期间" "$LOG_DIR/fresh-reboot.log" && ok "重启日志确认补建：$(grep -o '会计期间保底[^\"]*' "$LOG_DIR/fresh-reboot.log" | head -1)" || bad "重启日志没有补建记录"
stop

# ══════════════════════════════════════════════════════════════════════
step "② 存量库（$SRC_DB 的副本 $F2_DB）—— 5 张无期间凭证能否转绿"
# ══════════════════════════════════════════════════════════════════════
newdb "$F2_DB" || { echo "建库失败"; exit 1; }
pg_dump -h 127.0.0.1 -U alun --no-owner --no-privileges "$SRC_DB" | $PSQL -q -d "$F2_DB" >"$LOG_DIR/f2-restore.log" 2>&1 \
  || { echo "复制 $SRC_DB 失败，看 $LOG_DIR/f2-restore.log"; exit 1; }
echo "  复制前：期间 $(q "$F2_DB" "select count(*) from biz_accounting_period") 行 / 凭证 $(q "$F2_DB" "select count(*) from biz_voucher where deleted_at is null") 张 / 迁移 $(q "$F2_DB" "select max(version::int) from schema_migrations")"
echo "  复制前无期间凭证：$(q "$F2_DB" "select count(*) from biz_voucher v where v.deleted_at is null and not exists (select 1 from biz_accounting_period p where p.period_code=v.period)") 张"
boot "$F2_DB" "$((PORT + 1))" "$LOG_DIR/f2-migrate.log" || { tail -20 "$LOG_DIR/f2-migrate.log"; exit 1; }
stop
echo "  ℹ️  存量库 schema_migrations 最大版本 = $(q "$F2_DB" "select max(version::int) from schema_migrations")"
expect_eq "1" "$(q "$F2_DB" "select count(*) from schema_migrations where version='221' and name like '%accounting_period_year%'")" "存量库已应用 V0221"
expect_eq "12" "$(q "$F2_DB" "select count(*) from biz_accounting_period where period_year=$YEAR")" "存量库当年期间行数"
expect_eq "0" "$(q "$F2_DB" "select count(*) from biz_voucher v where v.deleted_at is null and not exists (select 1 from biz_accounting_period p where p.period_code=v.period)")" "无期间凭证数（F2 判据）"

invariants "$F2_DB" > "$LOG_DIR/f2-invariants-round.txt" 2>&1
tail -4 "$LOG_DIR/f2-invariants-round.txt"
grep -A1 "F2 凭证期间必有期间记录" "$LOG_DIR/f2-invariants-round.txt" | grep -q "✅" \
  && ok "不变量 F2 绿（存量库 5 张凭证已挂上期间）" || bad "存量库 F2 仍不是绿"
grep -q "BLOCKER 0" "$LOG_DIR/f2-invariants-round.txt" && ok "BLOCKER 0（存量库）" || bad "存量库出现 BLOCKER"

# ══════════════════════════════════════════════════════════════════════
step "结果"
# ══════════════════════════════════════════════════════════════════════
if [ "$FAIL" -eq 0 ]; then
  echo "RESULT: PASS（0 项失败） · 日志目录 $LOG_DIR"
else
  echo "RESULT: FAIL（$FAIL 项失败） · 日志目录 $LOG_DIR"
  exit 1
fi
