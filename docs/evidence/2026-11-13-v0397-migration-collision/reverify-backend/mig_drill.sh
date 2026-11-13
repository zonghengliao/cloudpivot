#!/usr/bin/env bash
# ===========================================================================
# 独立复跑夹具（backend-developer · 卡 t_b4cc9e22）
#
# 目的：不依赖 architect 的 run.sh，用自己的库名/端口/目录，从零复核：
#   A) 空库首启（仓库树，system/V0399 + business/V0397）→ 能起来、迁移跑完
#   B) 二次启动 → 0 待应用
#   C) 「已应用旧号 397」的库（用旧文件名树先迁满，再换回仓库树）
#      → 不 FATAL、不重跑 palette、business V0397/V0398 正常补跑、再启 0 待应用
#   D) 门禁：干净树绿 / 注入重复号红 / 撤掉再绿（在 scratch 副本上做，不动仓库）
#
# 用法：bash mig_drill.sh A|B|C1|C2|C3|D1|D2|D3|clean
# ===========================================================================
set -uo pipefail

BACKEND=/opt/cloudpivot/backend
BIN="$BACKEND/target/debug/cloudpivot"
BASE=/root/.hermes/profiles/backend-developer/cache/scratch/mig-drill
OUT="$BASE/out"
OLD_TREE="$BASE/oldtree"
GATE_TREE="$BASE/gate_tree"
mkdir -p "$OUT"

PGH=127.0.0.1
PGPORT=5432
PGUSER=alun
PGPW=alun
DB_EMPTY=cp_mig_b4cc_neg   # 空库（本卡 attempt 1 建的，public schema 为空）
DB_OLD=cp_mig_devcopy      # 145 cloudpivot_dev 的真实 dump 副本（palette 记录 @397 旧指纹）
PORT_EMPTY=18073
PORT_OLD=18074

JWT_SECRET="migrv-jwt-secret-0123456789abcdefghijklmnopqrstuvwxyz-xyz"
STORAGE_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

say() { printf '%s\n' "$*"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 2; }

q() { PGPASSWORD="$PGPW" psql -h "$PGH" -p "$PGPORT" -U "$PGUSER" -d "$1" -tAc "$2" 2>&1; }
qadm() { PGPASSWORD="$PGPW" psql -h "$PGH" -p "$PGPORT" -U "$PGUSER" -d postgres -tAc "$1" 2>&1; }

recreate_db() {
  qadm "DROP DATABASE IF EXISTS $1" >/dev/null 2>&1
  local created
  created=$(qadm "CREATE DATABASE $1" 2>&1)
  say "CREATE DATABASE $1 -> ${created:-ok}"
  [ "$(qadm "SELECT count(*) FROM pg_database WHERE datname='$1'" | tr -d '[:space:]')" = "1" ] || die "建库失败：$1"
}

# start_inst <cwd> <db> <port> <logfile>
start_inst() {
  local cwd="$1" db="$2" port="$3" log="$4"
  local pidfile="$BASE/pid.$port"
  rm -f "$pidfile"
  ( cd "$cwd" && exec env \
      NIK_PROFILE=test \
      NIK_SERVER_LISTEN="127.0.0.1:$port" \
      NIK_DATABASE_HOST="$PGH" \
      NIK_DATABASE_PORT="$PGPORT" \
      NIK_DATABASE_USER="$PGUSER" \
      NIK_DATABASE_PASSWORD="$PGPW" \
      NIK_DATABASE_NAME="$db" \
      NIK_ADMIN_PASSWORD="Admin@12345" \
      NIK_MIDDLEWARE_AUTH_JWT_SECRET="$JWT_SECRET" \
      CLOUDPIVOT_STORAGE_MASTER_KEY="$STORAGE_KEY" \
      X_CONFIG_SECRET_KEY="migrv-config-secret" \
      "$BIN" >"$log" 2>&1 ) &
  local pid=$!
  echo "$pid" >"$pidfile"
  local base="http://127.0.0.1:$port" code
  for i in $(seq 1 120); do
    code=$(curl -s -o /dev/null -m 3 -w '%{http_code}' "$base/health" 2>/dev/null || true)
    if [ "$code" = "200" ]; then
      say "STARTED pid=$pid db=$db port=$port health=200 after $((i*2))s"
      return 0
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid" 2>/dev/null
      say "EXITED rc=$? db=$db port=$port（/health 未起）"
      return 3
    fi
    sleep 2
  done
  say "TIMEOUT 240s db=$db pid=$pid"
  return 4
}

stop_inst() {
  local pidfile="$BASE/pid.$1"
  [ -f "$pidfile" ] || { say "STOPPED port=$1 (no pidfile)"; return 0; }
  local pid; pid=$(cat "$pidfile")
  kill -TERM "$pid" 2>/dev/null
  for _ in $(seq 1 40); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  kill -KILL "$pid" 2>/dev/null
  rm -f "$pidfile"
  say "STOPPED port=$1 pid=$pid"
}

mig_line() { grep -oE "数据库迁移完成: 应用 [0-9]+ 个迁移" "$1" | tail -1; }
mi_count() { q "$1" "SELECT count(*) FROM schema_migrations" | tr -d '[:space:]'; }
palette_versions() { q "$1" "SELECT string_agg(kind||':'||name||'@'||version, ' , ') FROM schema_migrations WHERE name LIKE 'preference_palette_ids_gallery77%'"; }
palette_ids() { q "$1" "SELECT cardinality(string_to_array(regexp_replace(validator,'^oneof:',''), ',')) FROM sys_preference_def WHERE pref_key='ui.palette'" | tr -d '[:space:]'; }
last_versions() { q "$1" "SELECT kind||'|'||name||'|'||version FROM schema_migrations ORDER BY applied_seq DESC LIMIT $2"; }

case "${1:-}" in
  A)  # 空库首启（仓库树）
    say "=== A) 空库首启 · 仓库树 $BACKEND/migrations（HEAD=$(git -C /opt/cloudpivot rev-parse --short HEAD)）"
    ls "$BACKEND"/migrations/*/V039*.up.sql | sed 's|.*/migrations/||'
    say "库 $DB_EMPTY 现有 schema：$(q "$DB_EMPTY" "select coalesce(string_agg(nspname,','),'(none)') from pg_namespace where nspname not like 'pg_%' and nspname<>'information_schema'" | tr -d '\n')"
    say "库 $DB_EMPTY 有 schema_migrations 吗：$(q "$DB_EMPTY" "select to_regclass('public.schema_migrations') is not null" | tr -d '[:space:]')"
    start_inst "$BACKEND" "$DB_EMPTY" "$PORT_EMPTY" "$OUT/A_empty_first_start.log"; say "start_inst rc=$?"
    say "迁移结论：$(mig_line "$OUT/A_empty_first_start.log")"
    say "schema_migrations 行数=$(mi_count "$DB_EMPTY")"
    say "palette 记录=$(palette_versions "$DB_EMPTY")"
    say "ui.palette 白名单 id 数=$(palette_ids "$DB_EMPTY")（期望 99）"
    say "最后 4 条（applied_seq 倒序）："; q "$DB_EMPTY" "SELECT applied_seq||' '||kind||' '||name||' @'||coalesce(version,'-') FROM schema_migrations ORDER BY applied_seq DESC LIMIT 4"
    say "真实失败行数（行首 ERROR nik_web::app: 数据库迁移失败 / 行首 FATAL: 服务启动失败 / duplicate up migration for version）：$(grep -cE '^FATAL: 服务启动失败|ERROR nik_web::app: 数据库迁移失败|duplicate up migration for version [0-9]+:' "$OUT/A_empty_first_start.log")"
    stop_inst "$PORT_EMPTY"
    ;;

  R)  # 把 drill 空库重置回「空」（drop/recreate public schema）
    say "=== R) 重置 drill 库 db=$DB_EMPTY"
    q "$DB_EMPTY" "DROP SCHEMA public CASCADE" >/dev/null 2>&1
    q "$DB_EMPTY" "CREATE SCHEMA public" >/dev/null 2>&1
    say "schema_migrations 存在吗：$(q "$DB_EMPTY" "select to_regclass('public.schema_migrations') is not null" | tr -d '[:space:]')（f ⇒ 空库）"
    ;;

  B)  # 二次启动 → 0 待应用
    say "=== B) 二次启动（库已迁满）db=$DB_EMPTY"
    say "启动前行数=$(mi_count "$DB_EMPTY")"
    start_inst "$BACKEND" "$DB_EMPTY" "$PORT_EMPTY" "$OUT/B_restart.log"; say "start_inst rc=$?"
    say "迁移结论：$(mig_line "$OUT/B_restart.log")"
    say "启动后行数=$(mi_count "$DB_EMPTY")（应不变）"
    stop_inst "$PORT_EMPTY"
    ;;

  C1) # 「已应用旧号 397」的库基线：145 cloudpivot_dev 的真实 dump 副本
    say "=== C1) 已应用旧号库基线 db=$DB_OLD（145 cloudpivot_dev 的 dump 副本）"
    say "schema_migrations 行数=$(mi_count "$DB_OLD")"
    say "palette 记录=$(palette_versions "$DB_OLD")（旧号 397 + 旧指纹 = 改名前已应用的真实形态）"
    say "ui.palette 白名单 id 数=$(palette_ids "$DB_OLD")"
    say "迁移上限（磁盘）= $(ls "$BACKEND"/migrations/*/V*.up.sql | sed 's|.*/V0*||;s|__.*||' | sort -n | tail -1)"
    ;;

  C2) # 换成仓库树（新号 399 + business 397/398）→ 期望正常启动、不重跑 palette
    say "=== C2) 已应用旧号的库 + 仓库树（V0399）启动 —— 这是 145 上 cloudpivot_dev 的真实形态"
    say "启动前 palette 记录=$(palette_versions "$DB_OLD")"
    say "启动前 ui.palette 白名单 id 数=$(palette_ids "$DB_OLD")"
    say "启动前行数=$(mi_count "$DB_OLD")"
    start_inst "$BACKEND" "$DB_OLD" "$PORT_OLD" "$OUT/C2_repo_tree_on_oldnum_db.log"; say "start_inst rc=$?"
    say "迁移结论：$(mig_line "$OUT/C2_repo_tree_on_oldnum_db.log")（期望应用 2 个：business V0397/V0398）"
    say "FATAL 行数=$(grep -c FATAL "$OUT/C2_repo_tree_on_oldnum_db.log")"
    say "checksum 相关行："; grep -iE "modified after it was applied|checksum" "$OUT/C2_repo_tree_on_oldnum_db.log" | head -3
    say "启动后 palette 记录=$(palette_versions "$DB_OLD")（应仍 397 且只有 1 条）"
    say "启动后行数=$(mi_count "$DB_OLD")"
    say "启动后白名单 id 数=$(palette_ids "$DB_OLD")（=3 说明没重跑；=99 说明幂等补跑）"
    stop_inst "$PORT_OLD"
    ;;

  C3) # 再启一次 → 0 待应用
    say "=== C3) 旧号库再启一次 → 0 待应用"
    start_inst "$BACKEND" "$DB_OLD" "$PORT_OLD" "$OUT/C3_restart.log"; say "start_inst rc=$?"
    say "迁移结论：$(mig_line "$OUT/C3_restart.log")"
    stop_inst "$PORT_OLD"
    ;;

  D1|D2|D3)  # 门禁三态（在 scratch 副本上做，不动仓库）
    if [ "$1" = "D1" ]; then
      say "=== D1) 门禁：干净树副本"
      rm -rf "$GATE_TREE"; mkdir -p "$GATE_TREE"
      cp -r "$BACKEND/migrations" "$GATE_TREE/migrations"
      cp -r "$BACKEND/config" "$GATE_TREE/config" 2>/dev/null
      # 工具按 <root>/backend/migrations 找，故补一层
      mkdir -p "$GATE_TREE/backend"; mv "$GATE_TREE/migrations" "$GATE_TREE/backend/migrations"
      python3 /opt/cloudpivot/scripts/lint/migration_version_unique.py --root "$GATE_TREE"; say "rc=$?（期望 0）"
    elif [ "$1" = "D2" ]; then
      say "=== D2) 门禁：注入 business/V0399 重复号"
      cp "$GATE_TREE/backend/migrations/system/V0399__preference_palette_ids_gallery77.up.sql" \
         "$GATE_TREE/backend/migrations/business/V0399__dup_probe.up.sql"
      cp "$GATE_TREE/backend/migrations/system/V0399__preference_palette_ids_gallery77.down.sql" \
         "$GATE_TREE/backend/migrations/business/V0399__dup_probe.down.sql"
      python3 /opt/cloudpivot/scripts/lint/migration_version_unique.py --root "$GATE_TREE"; say "rc=$?（期望非 0）"
    else
      say "=== D3) 门禁：撤掉重复号"
      rm -f "$GATE_TREE"/backend/migrations/business/V0399__dup_probe.*.sql
      python3 /opt/cloudpivot/scripts/lint/migration_version_unique.py --root "$GATE_TREE"; say "rc=$?（期望 0）"
    fi
    ;;

  N)  # 负向对照：把 system/V0399 改回 V0397（复原撞号）→ 期望启动即 FATAL，且库内一张表都没建
    say "=== N) 负向对照：复原撞号（system/V0397 + business/V0397）db=$DB_EMPTY"
    rm -rf "$OLD_TREE"; mkdir -p "$OLD_TREE"
    cp -r "$BACKEND/config" "$OLD_TREE/config"
    cp -r "$BACKEND/migrations" "$OLD_TREE/migrations"
    mv "$OLD_TREE/migrations/system/V0399__preference_palette_ids_gallery77.up.sql" \
       "$OLD_TREE/migrations/system/V0397__preference_palette_ids_gallery77.up.sql"
    mv "$OLD_TREE/migrations/system/V0399__preference_palette_ids_gallery77.down.sql" \
       "$OLD_TREE/migrations/system/V0397__preference_palette_ids_gallery77.down.sql"
    say "撞号树 V0397 up 文件：$(ls "$OLD_TREE"/migrations/*/V0397__*.up.sql | sed 's|.*/migrations/||' | tr '\n' ' ')"
    q "$DB_EMPTY" "DROP SCHEMA public CASCADE" >/dev/null 2>&1
    q "$DB_EMPTY" "CREATE SCHEMA public" >/dev/null 2>&1
    say "重置后 schema_migrations 存在吗：$(q "$DB_EMPTY" "select to_regclass('public.schema_migrations') is not null" | tr -d '[:space:]')"
    start_inst "$OLD_TREE" "$DB_EMPTY" "$PORT_OLD" "$OUT/N_duplicate_number_fatal.log"; say "start_inst rc=$?（期望非 0）"
    say "--- 关键日志行 ---"
    grep -nE "duplicate up migration|identities must be globally unique|FATAL" "$OUT/N_duplicate_number_fatal.log" | head -6
    say "库内 schema_migrations 是否已建：$(q "$DB_EMPTY" "select to_regclass('public.schema_migrations') is not null" | tr -d '[:space:]')（f ⇒ 连库之前就炸，一张表没建）"
    stop_inst "$PORT_OLD"
    ;;

  clean)
    for db in "$DB_EMPTY" "$DB_OLD"; do
      qadm "DROP DATABASE IF EXISTS $db" >/dev/null 2>&1 && say "dropped $db"
    done
    rm -rf "$OLD_TREE" "$GATE_TREE"
    ;;

  *) die "未知 step：${1:-<空>}（A B C1 C2 C3 D1 D2 D3 clean）" ;;
esac
