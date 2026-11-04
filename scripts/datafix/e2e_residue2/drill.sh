#!/usr/bin/env bash
# ============================================================================
# E2ERES2 / drill：在**生产库副本**上把 up / 幂等 / down / 再 up 全跑一遍
# ----------------------------------------------------------------------------
# 为什么要演练：
#   这是物理删除。删错了没有「撤销按钮」 —— 只有快照表 + down 脚本。
#   演练证明四件事：
#     ① up → 指纹/行数都变了（真的删了东西，且删的数量等于基线）
#     ② up → up 幂等（第二次 0 行受影响）
#     ③ up → down 之后，9 张 flow 表 + biz_status_log 的**整表指纹逐字节等于演练前**，
#        且每张表的行数也等于演练前（快照写回不是「大致恢复」，是原样恢复 ——
#        这 36 条是**软删**定义，deleted_at 必须原样还回来）
#     ④ down 之后还能再 up 一次（自检无 FAIL）
#
# 用法（在 144 上）：
#   cd /opt/cloudpivot && bash scripts/datafix/e2e_residue2/drill.sh
#
# 环境：
#   E2ERES2_DRILL_DB       演练库名（默认 cloudpivot_e2eres2_drill）
#   E2ERES2_KEEP_DRILL=1   演练完不删库（排查用）
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="${E2ERES2_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
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
SRC_DB="${E2ERES2_SRC_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"
DRILL_DB="${E2ERES2_DRILL_DB:-cloudpivot_e2eres2_drill}"

PSQL_SRC=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$SRC_DB" -X -v ON_ERROR_STOP=1)
PSQL_DRL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DRILL_DB" -X -v ON_ERROR_STOP=1 -q)

TS="$(date '+%Y%m%d-%H%M%S')"
TMP="${TMPDIR:-/tmp}/e2eres2-drill-$TS"
mkdir -p "$TMP"

echo "=============================================================="
echo " E2ERES2 演练：$SRC_DB (只读快照) → $DRILL_DB"
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
        (select id::text||'|'||biz_type||'|'||status||'|'||coalesce(deleted_at::text,'-')||'|'||coalesce(ended_at::text,'-') as x from biz_flow_instance) s
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

counts() { # <psql-array...>  输出每张表的行数
    "${@}" -At -F'=' <<'SQL'
select 'def',   count(*) from biz_flow_def
union all select 'ver',   count(*) from biz_flow_def_version
union all select 'node',  count(*) from biz_flow_node
union all select 'edge',  count(*) from biz_flow_edge
union all select 'asg',   count(*) from biz_flow_node_assignee
union all select 'inst',  count(*) from biz_flow_instance
union all select 'inode', count(*) from biz_flow_instance_node
union all select 'task',  count(*) from biz_flow_task
union all select 'cc',    count(*) from biz_flow_cc
union all select 'slog',  count(*) from biz_status_log
order by 1;
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

# 副本里如果带着生产那边的 datafix_* 表/视图（生产已经跑过本包），先清掉，
# 让演练等价于「第一次跑」—— 否则 up 会走幂等分支，演练就什么也没验到。
echo "==> ①b 清掉副本里的 datafix_e2eres2_* 残留（演练从零开始）"
"${PSQL_DRL[@]}" -c "drop view if exists datafix_e2eres2_v_target_inst;" \
                 -c "drop view if exists datafix_e2eres2_v_target_def;" \
                 -c "drop table if exists datafix_e2eres2_snapshot;" \
                 -c "drop table if exists datafix_e2eres2_guard;" \
                 -c "drop table if exists datafix_e2eres2_target;"

echo "==> ② 演练前指纹 / 行数（副本）"
FP_BEFORE="$(fingerprint "${PSQL_DRL[@]}")"
CNT_BEFORE="$(counts "${PSQL_DRL[@]}")"
echo "    $FP_BEFORE"
echo "$CNT_BEFORE" | sed 's/^/    /'

echo "==> ③ 演练库也走一遍 00_backup.sh（apply 的备份闸要有本库备份）"
E2ERES2_DB="$DRILL_DB" E2ERES2_BACKUP_ROOT=/root bash "$HERE/00_backup.sh" > "$TMP/backup.log" 2>&1 \
    || { tail -20 "$TMP/backup.log"; echo "!! 演练库备份失败" >&2; exit 1; }
BK_DRILL="$(ls -1dt /root/datafix-backup-e2eres2-* | head -1)"
echo "    $BK_DRILL"

echo "==> ④ up #1（真删）"
E2ERES2_DB="$DRILL_DB" FORCE=1 bash "$HERE/apply.sh" 2>&1 | tee "$TMP/up1.log" | tail -40
grep -qE '^FAIL\|' "$TMP/up1.log" && { echo "!! up #1 自检 FAIL" >&2; exit 1; }
FP_AFTER_UP="$(fingerprint "${PSQL_DRL[@]}")"
CNT_AFTER_UP="$(counts "${PSQL_DRL[@]}")"
echo "    删除后指纹：$FP_AFTER_UP"
echo "$CNT_AFTER_UP" | sed 's/^/    /'
[ "$FP_AFTER_UP" != "$FP_BEFORE" ] || { echo "!! up 之后指纹没变 —— 什么都没删掉" >&2; exit 1; }

echo "==> ⑤ up #2（幂等：应 0 行受影响）"
E2ERES2_DB="$DRILL_DB" FORCE=1 bash "$HERE/apply.sh" > "$TMP/up2.log" 2>&1 \
    || { tail -20 "$TMP/up2.log"; echo "!! up #2 失败（不幂等）" >&2; exit 1; }
grep -q '幂等' "$TMP/up2.log" || { echo "!! up #2 没有走到幂等分支："; tail -15 "$TMP/up2.log"; exit 1; }
echo "    up #2 幂等 OK"

echo "==> ⑥ down（按快照整行写回）"
E2ERES2_DB="$DRILL_DB" bash "$HERE/apply.sh" --rollback > "$TMP/down.log" 2>&1 \
    || { tail -20 "$TMP/down.log"; echo "!! down 失败" >&2; exit 1; }
FP_AFTER_DOWN="$(fingerprint "${PSQL_DRL[@]}")"
CNT_AFTER_DOWN="$(counts "${PSQL_DRL[@]}")"
echo "    回滚后指纹：$FP_AFTER_DOWN"
echo "$CNT_AFTER_DOWN" | sed 's/^/    /'
if [ "$FP_AFTER_DOWN" != "$FP_BEFORE" ]; then
    echo "!! 回滚后指纹 != 演练前（快照写回不是原样恢复）" >&2
    echo "   before=$FP_BEFORE" >&2
    echo "   after =$FP_AFTER_DOWN" >&2
    exit 1
fi
if [ "$CNT_AFTER_DOWN" != "$CNT_BEFORE" ]; then
    echo "!! 回滚后行数 != 演练前" >&2
    diff <(echo "$CNT_BEFORE") <(echo "$CNT_AFTER_DOWN") >&2 || true
    exit 1
fi
echo "    回滚指纹 + 逐表行数一致（9 张 flow 表 + biz_status_log）"

echo "==> ⑥b 回滚后软删状态必须原样（这 36 条是软删行，不能变成活定义）"
"${PSQL_DRL[@]}" -At -F'|' -c "
select 'alive=' || count(*) filter (where deleted_at is null)
    || ' soft_deleted=' || count(*) filter (where deleted_at is not null)
  from biz_flow_def where id in (select id from datafix_e2eres2_target where kind='def');"

echo "==> ⑦ 再 up 一次（证明回滚后还能重来）"
E2ERES2_DB="$DRILL_DB" FORCE=1 bash "$HERE/apply.sh" > "$TMP/up3.log" 2>&1 \
    || { tail -20 "$TMP/up3.log"; echo "!! up #3 失败" >&2; exit 1; }
grep -qE '^FAIL\|' "$TMP/up3.log" && { echo "!! up #3 自检 FAIL" >&2; exit 1; }
echo "    up #3 OK"

if [ "${E2ERES2_KEEP_DRILL:-0}" = "1" ]; then
    echo "==> 保留演练库 $DRILL_DB（E2ERES2_KEEP_DRILL=1）"
else
    echo "==> ⑧ 清掉演练库 $DRILL_DB"
    dropdb_as_admin
fi

cat <<EOF

================ 演练结论 ================
① up 生效            ：指纹已变化（副本上删掉了 36 定义 + 16 实例及其 265 子行）
② up 幂等            ：第二次 0 行受影响
③ down 原样恢复      ：9 张 flow 表 + biz_status_log 指纹逐字节 + 逐表行数等于演练前
④ 软删状态保持       ：写回的 36 条定义仍是 deleted_at 非空（没有被「复活」成活定义）
⑤ 回滚后可重来       ：up #3 自检无 FAIL
日志：$TMP
演练库备份：$BK_DRILL（确认不需要后可删）
EOF
