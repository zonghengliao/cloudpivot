#!/usr/bin/env bash
# ============================================================================
# E2ERES / drill：在**生产库副本**上把 up / 幂等 / down / 再 up 全跑一遍
# ----------------------------------------------------------------------------
# 为什么要演练：
#   这是物理删除。删错了没有「撤销按钮」—— 只有快照表 + down 脚本。
#   演练证明两件事：
#     ① up → up 幂等（第二次 0 行受影响）
#     ② up → down 之后，9 张 flow 表 + biz_status_log 的**整表指纹逐字节等于演练前**
#        （快照写回不是「大致恢复」，是原样恢复）
#
# 用法（在 144 上）：
#   cd /opt/cloudpivot && bash scripts/datafix/e2e_residue/drill.sh
#
# 环境：
#   E2ERES_DRILL_DB       演练库名（默认 cloudpivot_e2eres_drill）
#   E2ERES_KEEP_DRILL=1   演练完不删库（排查用）
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="${E2ERES_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi

DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
SRC_DB="${NIK_DATABASE_NAME:-cloudpivot}"
DRILL_DB="${E2ERES_DRILL_DB:-cloudpivot_e2eres_drill}"

PSQL_SRC=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$SRC_DB" -X -v ON_ERROR_STOP=1)
PSQL_DRL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DRILL_DB" -X -v ON_ERROR_STOP=1 -q)

TS="$(date '+%Y%m%d-%H%M%S')"
TMP="${TMPDIR:-/tmp}/e2eres-drill-$TS"
mkdir -p "$TMP"

echo "=============================================================="
echo " E2ERES 演练：$SRC_DB (只读快照) → $DRILL_DB"
echo " 临时目录：$TMP"
echo "=============================================================="

# ---------------------------------------------------------------- 整表指纹
fingerprint() { # <psql-array...>  输出一行 md5
    "${@}" -At <<'SQL'
with rows as (
    select 'def' as t, md5(string_agg(x, E'\n' order by x)) as h from
        (select id::text||'|'||flow_code||'|'||biz_type||'|'||status||'|'||coalesce(deleted_at::text,'-') as x from biz_flow_def) s
    union all select 'ver', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||def_id::text||'|'||version_no||'|'||version_status||'|'||coalesce(graph_hash,'-') as x from biz_flow_def_version) s
    union all select 'node', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||version_id::text||'|'||node_key||'|'||node_type as x from biz_flow_node) s
    union all select 'edge', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||version_id::text||'|'||edge_key||'|'||from_node_id::text||'|'||to_node_id::text as x from biz_flow_edge) s
    union all select 'asg', md5(string_agg(x, E'\n' order by x)) from
        (select node_id::text||'|'||assignee_type||'|'||ref_id::text as x from biz_flow_node_assignee) s
    union all select 'inst', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||biz_type||'|'||status||'|'||coalesce(ended_at::text,'-') as x from biz_flow_instance) s
    union all select 'inode', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||instance_id::text||'|'||node_key||'|'||status as x from biz_flow_instance_node) s
    union all select 'task', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||instance_id::text||'|'||node_key||'|'||status as x from biz_flow_task) s
    union all select 'cc', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||instance_id::text||'|'||node_key as x from biz_flow_cc) s
    union all select 'slog', md5(string_agg(x, E'\n' order by x)) from
        (select id::text||'|'||entity_type||'|'||entity_id::text||'|'||to_status as x from biz_status_log) s
)
select string_agg(t || '=' || coalesce(h, '(empty)'), ' ' order by t) from rows;
SQL
}

# ---------------------------------------------------------------- 建/删演练库
# 生产库角色 cloudpivot 没有 CREATEDB 权限（rolcreatedb=f），所以建/删库走 postgres 系统账号。
createdb_as_admin() {
    if [ "$("${PSQL_SRC[@]}" -At -c "select rolcreatedb from pg_roles where rolname = current_user")" = "t" ]; then
        "${PSQL_SRC[@]}" -c "create database \"$DRILL_DB\" owner \"$DB_USER\"" >/dev/null
    else
        su - postgres -c "createdb -O $DB_USER $DRILL_DB"
    fi
}

dropdb_as_admin() {
    if [ "$("${PSQL_SRC[@]}" -At -c "select rolcreatedb from pg_roles where rolname = current_user")" = "t" ]; then
        "${PSQL_SRC[@]}" -c "drop database \"$DRILL_DB\"" >/dev/null
    else
        su - postgres -c "dropdb --if-exists $DRILL_DB"
    fi
}

# ---------------------------------------------------------------- 准备副本
echo "==> ① 建演练库 $DRILL_DB（从 $SRC_DB 全量 dump 还原）"
if "${PSQL_SRC[@]}" -At -c "select 1 from pg_database where datname = '$DRILL_DB'" | grep -q 1; then
    echo "    （库已存在 → 先删）"
    dropdb_as_admin
fi
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$SRC_DB" -Fc -f "$TMP/src.dump"
createdb_as_admin
pg_restore -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DRILL_DB" --no-owner --no-privileges "$TMP/src.dump"

echo "==> ② 演练前指纹（副本）"
FP_BEFORE="$(fingerprint "${PSQL_DRL[@]}")"
echo "    $FP_BEFORE"

echo "==> ③ 演练库也走一遍 00_backup.sh（apply 的备份闸要有本库备份）"
E2ERES_DB="$DRILL_DB" E2ERES_BACKUP_ROOT=/root bash "$HERE/00_backup.sh" > "$TMP/backup.log" 2>&1 \
    || { tail -20 "$TMP/backup.log"; echo "!! 演练库备份失败" >&2; exit 1; }
BK_DRILL="$(ls -1dt /root/datafix-backup-e2eres-* | head -1)"
echo "    $BK_DRILL"

echo "==> ④ up #1（真删）"
E2ERES_DB="$DRILL_DB" FORCE=1 bash "$HERE/apply.sh" 2>&1 | tee "$TMP/up1.log" | tail -30
grep -qE '^FAIL\|' "$TMP/up1.log" && { echo "!! up #1 自检 FAIL" >&2; exit 1; }
FP_AFTER_UP="$(fingerprint "${PSQL_DRL[@]}")"
echo "    删除后指纹：$FP_AFTER_UP"
[ "$FP_AFTER_UP" != "$FP_BEFORE" ] || { echo "!! up 之后指纹没变 —— 什么都没删掉" >&2; exit 1; }

echo "==> ⑤ up #2（幂等：应 0 行受影响）"
E2ERES_DB="$DRILL_DB" FORCE=1 bash "$HERE/apply.sh" > "$TMP/up2.log" 2>&1 \
    || { tail -20 "$TMP/up2.log"; echo "!! up #2 失败（不幂等）" >&2; exit 1; }
grep -q '幂等' "$TMP/up2.log" || { echo "!! up #2 没有走到幂等分支："; tail -15 "$TMP/up2.log"; exit 1; }
echo "    up #2 幂等 OK"

echo "==> ⑥ down（按快照整行写回）"
E2ERES_DB="$DRILL_DB" bash "$HERE/apply.sh" --rollback > "$TMP/down.log" 2>&1 \
    || { tail -20 "$TMP/down.log"; echo "!! down 失败" >&2; exit 1; }
FP_AFTER_DOWN="$(fingerprint "${PSQL_DRL[@]}")"
echo "    回滚后指纹：$FP_AFTER_DOWN"
if [ "$FP_AFTER_DOWN" != "$FP_BEFORE" ]; then
    echo "!! 回滚后指纹 != 演练前（快照写回不是原样恢复）" >&2
    echo "   before=$FP_BEFORE" >&2
    echo "   after =$FP_AFTER_DOWN" >&2
    exit 1
fi
echo "    回滚指纹逐表一致（9 张 flow 表 + biz_status_log）"

echo "==> ⑦ 再 up 一次（证明回滚后还能重来）"
E2ERES_DB="$DRILL_DB" FORCE=1 bash "$HERE/apply.sh" > "$TMP/up3.log" 2>&1 \
    || { tail -20 "$TMP/up3.log"; echo "!! up #3 失败" >&2; exit 1; }
grep -qE '^FAIL\|' "$TMP/up3.log" && { echo "!! up #3 自检 FAIL" >&2; exit 1; }
echo "    up #3 OK"

if [ "${E2ERES_KEEP_DRILL:-0}" = "1" ]; then
    echo "==> 保留演练库 $DRILL_DB（E2ERES_KEEP_DRILL=1）"
else
    echo "==> ⑧ 清掉演练库 $DRILL_DB"
    dropdb_as_admin
fi

cat <<EOF

================ 演练结论 ================
① up 生效            ：指纹已变化（副本上删掉了测试残留）
② up 幂等            ：第二次 0 行受影响
③ down 原样恢复      ：9 张 flow 表 + biz_status_log 指纹逐字节等于演练前
④ 回滚后可重来       ：up #3 自检无 FAIL
日志：$TMP
演练库备份：$BK_DRILL（确认不需要后可删）
EOF
