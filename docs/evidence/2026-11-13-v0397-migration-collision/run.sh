#!/usr/bin/env bash
# ============================================================================
# 探针：V0397 重复版本号事故的实测夹具（architect 卡 t_83977c43）
#
# 目的：把卡面里「需要实测确认」的四件事变成可复现的原始输出——
#   ① 空库首启是否真 FATAL（复现）；
#   ② 已迁移库重启是否也 FATAL（⇒ 生产 144 一旦发布 main 就停机）；
#   ③ 失败发生在「连库之前」还是「连库之后」（判据：拿不可达 host 也能报同一条错）；
#   ④ 改号后空库首启 / 二次启动是否都绿；已应用旧号的库是否需要「指纹处置」。
#
# 用法：bash probe_mig.sh <step>      step ∈ t1 t2 t3a t3b t4 t4b t5 t6
# 环境：WT（被测工作树，默认 /root/arch-v0399/wt）、BIN、DBPW、DBHOST
# ============================================================================
set -uo pipefail

WORKSPACE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
OUT="$WORKSPACE/raw"   # 复跑产物（*.log，仓库 .gitignore 忽略）；本目录的 *.out 是策展过的原始输出
mkdir -p "$OUT"

WT=${WT:-/root/arch-v0399/wt}
BACKEND="$WT/backend"
BIN=${BIN:-/opt/cloudpivot/backend/target/release/cloudpivot}
DBHOST=${DBHOST:-172.18.10.145}
DBPORT=${DBPORT:-5432}
DBUSER=${DBUSER:-cloudpivot}
DBPW=${DBPW:-$(cat /root/.cp_dbpw_145 2>/dev/null)}
PORT=${PORT:-18397}
RUN="$WORKSPACE/run"
mkdir -p "$RUN"

JWT_SECRET="v0397-probe-jwt-secret-0123456789abcdefghijklmnopqrstuvwxyz"
STORAGE_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

say() { printf '%s\n' "$*"; }
die() { printf 'FATAL: %s\n' "$*" >&2; exit 2; }

[ -x "$BIN" ] || die "二进制不存在：$BIN"

q()    { PGPASSWORD="$DBPW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$1" -tAc "$2" 2>&1; }
qadm() { PGPASSWORD="$DBPW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
dbexist() { [ "$(qadm "SELECT count(*) FROM pg_database WHERE datname='$1'" | tr -d '[:space:]')" = "1" ]; }
recreate_db() {
  qadm "DROP DATABASE IF EXISTS $1" >/dev/null
  qadm "CREATE DATABASE $1" >/dev/null 2>&1
  dbexist "$1" || die "建库失败：$1"
}

# start_inst <db> <port> <logfile> [extra-env-assignment...]
#   起隔离实例，最多等 420s 的 /health；实例提前退出则把退出码写进 $RUN/rc
start_inst() {
  local db="$1" port="$2" log="$3"; shift 3
  local pidfile="$RUN/pid.$port"
  rm -f "$pidfile"
  local _owd="$PWD"
  cd "$BACKEND" || return 1
  env NIK_PROFILE=test \
      NIK_SERVER_LISTEN="127.0.0.1:$port" \
      NIK_DATABASE_HOST="$DBHOST" \
      NIK_DATABASE_PORT="$DBPORT" \
      NIK_DATABASE_USER="$DBUSER" \
      NIK_DATABASE_PASSWORD="$DBPW" \
      NIK_DATABASE_NAME="$db" \
      NIK_ADMIN_PASSWORD="Admin@12345" \
      NIK_MIDDLEWARE_AUTH_JWT_SECRET="$JWT_SECRET" \
      CLOUDPIVOT_STORAGE_MASTER_KEY="$STORAGE_KEY" \
      X_CONFIG_SECRET_KEY="v0397-probe-config-secret" \
      "$@" \
      "$BIN" >"$log" 2>&1 &
  local pid=$!
  cd "$_owd" || return 1
  echo "$pid" >"$pidfile"
  local base="http://127.0.0.1:$port"
  local code
  for i in $(seq 1 210); do
    code=$(curl -s -o /dev/null -m 3 -w '%{http_code}' "$base/health" 2>/dev/null || true)
    if [ "$code" = "200" ]; then
      echo "STARTED pid=$pid db=$db port=$port health=200 after $((i*2))s"
      return 0
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid" 2>/dev/null
      local rc=$?
      echo "EXITED rc=$rc db=$db port=$port (无 /health)"
      return 3
    fi
    sleep 2
  done
  echo "TIMEOUT 420s db=$db pid=$pid"
  return 4
}

stop_inst() {
  local port="$1"
  local pidfile="$RUN/pid.$port"
  [ -f "$pidfile" ] || return 0
  local pid; pid=$(cat "$pidfile")
  kill -TERM "$pid" 2>/dev/null
  for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  kill -KILL "$pid" 2>/dev/null
  rm -f "$pidfile"
  echo "STOPPED port=$port"
}

mi_count() { q "$1" "SELECT count(*) FROM schema_migrations" | tr -d '[:space:]'; }
mi_ver()   { q "$1" "SELECT kind||'|'||name||'|'||version FROM schema_migrations ORDER BY applied_seq DESC LIMIT $2"; }

case "${1:-}" in
  t1)  # 空库 + 重复版本号（main 现状）→ 期望立刻 FATAL，且库内一张表都没建
    recreate_db v0399_t1_empty
    say "--- T1 空库首启（main 原样，两条 V0397）port=$PORT"
    start_inst v0399_t1_empty "$PORT" "$OUT/t1_empty_db_start.log"; say "start_inst rc=$?"
    say "库内 schema_migrations 是否已建：$(q v0399_t1_empty "SELECT to_regclass('public.schema_migrations') IS NOT NULL" | tr -d '[:space:]')"
    say "--- 日志（前 12 行）"; sed -n '1,12p' "$OUT/t1_empty_db_start.log"
    stop_inst "$PORT"
    ;;

  t2)  # 不可达 host + 重复版本号 → 若仍报 duplicate，则证明「连库之前就炸」
    recreate_db v0399_t2_nohost >/dev/null
    say "--- T2 不可达库 host=203.0.113.1（TEST-NET-3，必然连不上）"
    DBHOST=203.0.113.1 start_inst v0399_t2_nohost "$((PORT+1))" "$OUT/t2_unreachable_db_start.log"; say "start_inst rc=$?"
    say "--- 日志（前 12 行）"; sed -n '1,12p' "$OUT/t2_unreachable_db_start.log"
    stop_inst "$((PORT+1))"
    ;;

  t3a) # 先把 397 的「system 主题」那条临时移出 → 能把库迁满（造出一个「已迁移库」）
    mv "$BACKEND/migrations/system/V0397__preference_palette_ids_gallery77.up.sql" \
       "$RUN/.hold_palette.up.sql" 2>/dev/null
    mv "$BACKEND/migrations/system/V0397__preference_palette_ids_gallery77.down.sql" \
       "$RUN/.hold_palette.down.sql" 2>/dev/null
    say "--- T3a 移出 system/V0397 后：空库迁满（造「已迁移库」v0399_t3_migrated）"
    recreate_db v0399_t3_migrated
    start_inst v0399_t3_migrated "$PORT" "$OUT/t3a_migrate_full.log"; say "start_inst rc=$?"
    say "schema_migrations 行数=$(mi_count v0399_t3_migrated)"
    say "397 记录："; mi_ver v0399_t3_migrated 20 | grep -E '\|39[0-9]$' || true
    stop_inst "$PORT"
    ;;

  t3b) # 把 system/V0397 放回（= 重复号复原）→ 对着「已迁移库」重启 → 期望仍 FATAL
    mv "$RUN/.hold_palette.up.sql"   "$BACKEND/migrations/system/V0397__preference_palette_ids_gallery77.up.sql" 2>/dev/null
    mv "$RUN/.hold_palette.down.sql" "$BACKEND/migrations/system/V0397__preference_palette_ids_gallery77.down.sql" 2>/dev/null
    say "--- T3b 已迁移库重启（重复号复原）db=v0399_t3_migrated"
    say "重启前 schema_migrations 行数=$(mi_count v0399_t3_migrated)"
    start_inst v0399_t3_migrated "$PORT" "$OUT/t3b_migrated_db_start.log"; say "start_inst rc=$?"
    say "重启后 schema_migrations 行数=$(mi_count v0399_t3_migrated)（未变 ⇒ 连库之前就炸）"
    say "--- 日志（前 12 行）"; sed -n '1,12p' "$OUT/t3b_migrated_db_start.log"
    stop_inst "$PORT"
    ;;

  t4)  # 改号后（system/V0397 → V0399）：空库首启 → 期望绿
    say "--- T4 改号后空库首启（system/V0399）db=v0399_t4_fixed"
    ls "$BACKEND"/migrations/system/V039*.up.sql | sed 's|.*/||'
    recreate_db v0399_t4_fixed
    start_inst v0399_t4_fixed "$((PORT+2))" "$OUT/t4_fixed_empty_start.log"; say "start_inst rc=$?"
    say "schema_migrations 行数=$(mi_count v0399_t4_fixed)"
    say "最后 6 条记录："; mi_ver v0399_t4_fixed 6
    say "palette 白名单 id 数：$(q v0399_t4_fixed "SELECT coalesce(cardinality(string_to_array(regexp_replace(validator,'^oneof:',''), ',')),-1) FROM sys_preference_def WHERE pref_key='ui.palette'" | tr -d '[:space:]')"
    stop_inst "$((PORT+2))"
    ;;

  t4b) # 二次启动（库已迁满）→ 期望 0 待应用、秒级 GREEN
    say "--- T4b 改号后二次启动（已迁移库）db=v0399_t4_fixed"
    start_inst v0399_t4_fixed "$((PORT+2))" "$OUT/t4b_fixed_restart.log"; say "start_inst rc=$?"
    say "--- 迁移相关日志"; grep -iE "migration|迁移|无待应用" "$OUT/t4b_fixed_restart.log" | head -6
    stop_inst "$((PORT+2))"
    ;;

  t5)  # 「已应用旧号」的库：把 V0399 记录改回旧名旧号（模拟 cloudpivot_dev 现状）→ 重启，看是否致命
    say "--- T5 已应用库残留旧号记录（模拟 145 的 cloudpivot_dev）"
    q v0399_t4_fixed "UPDATE schema_migrations SET name='preference_palette_ids_gallery77', version='397' WHERE name='preference_palette_ids_gallery77'" >/dev/null
    q v0399_t4_fixed "SELECT 1" >/dev/null
    # 把 palette 白名单先改坏，验证「补跑是否真的把数据修回来」
    q v0399_t4_fixed "UPDATE sys_preference_def SET validator='oneof:mist-blue' WHERE pref_key='ui.palette'" >/dev/null
    say "改坏后白名单 id 数=$(q v0399_t4_fixed "SELECT cardinality(string_to_array(regexp_replace(validator,'^oneof:',''), ',')) FROM sys_preference_def WHERE pref_key='ui.palette'" | tr -d '[:space:]')"
    say "迁移表里的 palette 记录：$(q v0399_t4_fixed "SELECT kind||'|'||name||'|'||version FROM schema_migrations WHERE name='preference_palette_ids_gallery77'" | tr -d '\n')"
    start_inst v0399_t4_fixed "$((PORT+2))" "$OUT/t5_stale_record_start.log"; say "start_inst rc=$?"
    say "重启后白名单 id 数=$(q v0399_t4_fixed "SELECT cardinality(string_to_array(regexp_replace(validator,'^oneof:',''), ',')) FROM sys_preference_def WHERE pref_key='ui.palette'" | tr -d '[:space:]')（=99 ⇒ 新号已幂等补跑）"
    say "迁移表里的 palette 记录：$(q v0399_t4_fixed "SELECT kind||'|'||name||'|'||version FROM schema_migrations WHERE name='preference_palette_ids_gallery77'" | tr -d '\n')"
    say "palette 记录条数=$(q v0399_t4_fixed "SELECT count(*) FROM schema_migrations WHERE name='preference_palette_ids_gallery77'" | tr -d '[:space:]')"
    stop_inst "$((PORT+2))"
    ;;

  t6)  # 门禁：干净树绿 / 注入重复号红 / 撤掉再绿
    say "--- T6 门禁（scripts/check.sh 的 L0 段）"
    bash "$WT/scripts/check.sh" --l0 >"$OUT/t6a_gate_clean.log" 2>&1; say "干净树 rc=$?（期望 0）"
    grep -iE "迁移版本号|全局唯一|migration version" "$OUT/t6a_gate_clean.log" | head -4
    cp "$BACKEND/migrations/system/V0399__preference_palette_ids_gallery77.up.sql" \
       "$BACKEND/migrations/business/V0399__dup_probe.up.sql"
    cp "$BACKEND/migrations/system/V0399__preference_palette_ids_gallery77.down.sql" \
       "$BACKEND/migrations/business/V0399__dup_probe.down.sql"
    bash "$WT/scripts/check.sh" --l0 >"$OUT/t6b_gate_dup.log" 2>&1; say "注入重复号 rc=$?（期望非 0）"
    grep -iE "迁移版本号|全局唯一|V0399|duplicate" "$OUT/t6b_gate_dup.log" | head -6
    rm -f "$BACKEND/migrations/business/V0399__dup_probe."*
    bash "$WT/scripts/check.sh" --l0 >"$OUT/t6c_gate_clean_again.log" 2>&1; say "撤掉后 rc=$?（期望 0）"
    ;;

  t5b) # 关键判据：迁移记录表的主键是 (kind,name)，**不含 version** ——
       # 所以「改号」若连内容一起改（哪怕只加一行注释），已应用旧号的库会因指纹不一致而中止。
    say "--- T5b 已应用库 + 内容被改过（记录指纹 = 旧内容）→ 预期 FATAL MigrationChecksum"
    OLD_CK=$(PGPASSWORD="$DBPW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d cloudpivot_dev -tAc \
      "SELECT checksum FROM schema_migrations WHERE name='preference_palette_ids_gallery77'" | tr -d '[:space:]')
    [ -n "$OLD_CK" ] || die "拿不到 cloudpivot_dev 里旧内容的指纹"
    say "cloudpivot_dev 记录的旧指纹 = $OLD_CK"
    q v0399_t4_fixed "UPDATE schema_migrations SET checksum='$OLD_CK', version='397' WHERE name='preference_palette_ids_gallery77'" >/dev/null
    start_inst v0399_t4_fixed "$((PORT+3))" "$OUT/t5b_content_changed_start.log"; say "start_inst rc=$?"
    say "--- 日志（关键行）"; grep -iE "checksum|指纹|divergent|FATAL" "$OUT/t5b_content_changed_start.log" | head -4
    stop_inst "$((PORT+3))"
    ;;

  t5c) # 复现 T5b 的对照组：只是往 V0399 里**多加一行注释**，已应用旧号的库立刻 FATAL
    say "--- T5c 改号时顺手改内容（加一行注释）→ 预期 FATAL MigrationChecksum"
    UPFILE="$BACKEND/migrations/system/V0399__preference_palette_ids_gallery77.up.sql"
    cp "$UPFILE" "$RUN/.palette.bak"
    printf -- '-- probe: 仅加一行注释，验证「改号不许改内容」\n' >>"$UPFILE"
    OLD_CK=$(PGPASSWORD="$DBPW" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d cloudpivot_dev -tAc \
      "SELECT checksum FROM schema_migrations WHERE name='preference_palette_ids_gallery77'" | tr -d '[:space:]')
    q v0399_t4_fixed "UPDATE schema_migrations SET checksum='$OLD_CK', version='397' WHERE name='preference_palette_ids_gallery77'" >/dev/null
    start_inst v0399_t4_fixed "$((PORT+3))" "$OUT/t5c_comment_added_start.log"; say "start_inst rc=$?"
    grep -iE "was modified after it was applied|FATAL" "$OUT/t5c_comment_added_start.log" | head -2
    cp "$RUN/.palette.bak" "$UPFILE"
    say "已还原 V0399 内容（cmp 复核 $(cmp -s "$RUN/.palette.bak" "$UPFILE" && echo OK || echo FAIL)）"
    stop_inst "$((PORT+3))"
    ;;

  clean) # 清库
    for db in v0399_t1_empty v0399_t2_nohost v0399_t3_migrated v0399_t4_fixed; do
      qadm "DROP DATABASE IF EXISTS $db" >/dev/null && echo "dropped $db"
    done
    ;;

  *) die "未知 step：${1:-<空>}（t1 t2 t3a t3b t4 t4b t5 t6 clean）" ;;
esac
