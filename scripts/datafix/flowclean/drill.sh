#!/usr/bin/env bash
# ============================================================================
# FLOWCLEAN · drill：在「prod 库的副本」上把 up / 幂等 / down / 再 up 全跑一遍
# ----------------------------------------------------------------------------
# 目的：上生产之前，用**同一份数据**证明三件事 ——
#   ① up 能干净跑完，30_verify 全绿；
#   ② 幂等：第二遍 up = 0 行受影响，verify 仍全绿；
#   ③ 可逆：down 之后 10 张 flow 表的**整表行指纹**与 drill 之前逐字节相同（含
#      biz_task.flow_instance_id 被解引用那一列），再 up 一遍仍能绿。
#
# 用法：
#   bash scripts/datafix/flowclean/drill.sh                 # 复制生产库 → 演练 → 自动删演练库
#   KEEP_DRILL=1 bash scripts/datafix/flowclean/drill.sh    # 保留演练库（人工查看）
#
# 环境：
#   FLOWCLEAN_DRILL_DB=cloudpivot_flowclean_drill  演练库名
#   SOURCE_DB（默认 NIK_DATABASE_NAME，即生产库）
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi

DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . /etc/cloudpivot/cloudpivot.env）}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
SRC_DB="${SOURCE_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"
DRILL_DB="${FLOWCLEAN_DRILL_DB:-cloudpivot_flowclean_drill}"
DRILL_BACKUP="/root/datafix-backup-flowclean-drill"

q() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$1" -At -v ON_ERROR_STOP=1 -c "$2"; }

TABLES=(biz_flow_def biz_flow_def_version biz_flow_node biz_flow_node_assignee
        biz_flow_edge biz_flow_instance biz_flow_instance_node biz_flow_task biz_flow_cc biz_task)

digest() {  # digest <db> <outfile>
    : > "$2"
    for t in "${TABLES[@]}"; do
        local rows md5
        rows=$(q "$1" "select count(*) from public.$t")
        md5=$(q "$1" "select md5(coalesce(string_agg(to_jsonb(x)::text, '' order by to_jsonb(x)::text), '')) from public.$t x")
        printf '%-26s rows=%-8s md5=%s\n' "$t" "$rows" "$md5" >> "$2"
    done
}

echo "== FLOWCLEAN drill：$SRC_DB → $DRILL_DB @ $DB_HOST:$DB_PORT =="

# ---- 0) 建演练库（owner 与业务表 owner 保持一致，否则应用账号没权限）----
OWNER=$(q "$SRC_DB" "select tableowner from pg_tables where schemaname='public' and tablename='biz_flow_def'")
echo "==> 业务表 owner = $OWNER"
q postgres "drop database if exists $DRILL_DB" 2>/dev/null || \
    su postgres -c "psql -q -c 'drop database if exists $DRILL_DB'"
su postgres -c "psql -q -c \"create database $DRILL_DB owner $OWNER\"" || \
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d postgres -q -c "create database $DRILL_DB owner $OWNER"

# ---- 1) 复制生产数据（全库，含全部非 flow 表：外键才不炸）----
echo "==> pg_dump $SRC_DB | psql -d $DRILL_DB"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$SRC_DB" --no-owner --no-privileges \
    | psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DRILL_DB" -q -v ON_ERROR_STOP=1 > /dev/null

digest "$DRILL_DB" /tmp/flowclean-drill-before.txt
echo "---- drill 前整表指纹 ----"; cat /tmp/flowclean-drill-before.txt

# ---- 2) 演练库备份 ----
FLOWCLEAN_DB="$DRILL_DB" FLOWCLEAN_BACKUP_DIR="$DRILL_BACKUP" \
    bash "$HERE/00_backup.sh" drill > /tmp/flowclean-drill-backup.log 2>&1
echo "==> 演练库备份：$DRILL_BACKUP"

# ---- 3) 第一遍 up ----
echo ""
echo "===== ① 第一遍 apply ====="
FLOWCLEAN_DB="$DRILL_DB" FLOWCLEAN_BACKUP_DIR="$DRILL_BACKUP" \
    bash "$HERE/apply.sh" 2>&1 | tee /tmp/flowclean-drill-apply1.log | tail -30

# ---- 4) 第二遍 up（幂等）----
echo ""
echo "===== ② 第二遍 apply（幂等验证）====="
FLOWCLEAN_DB="$DRILL_DB" FLOWCLEAN_BACKUP_DIR="$DRILL_BACKUP" \
    bash "$HERE/apply.sh" 2>&1 | tee /tmp/flowclean-drill-apply2.log | tail -30
if ! grep -q '幂等跳过（本步 0 行受影响）' /tmp/flowclean-drill-apply2.log; then
    echo "✗ 幂等验证失败：第二遍没有走「0 行受影响」分支" >&2
    exit 1
fi
echo "✓ 幂等：第二遍 0 行受影响"

# ---- 5) down（可逆）----
echo ""
echo "===== ③ rollback ====="
FLOWCLEAN_DB="$DRILL_DB" bash "$HERE/apply.sh" --rollback 2>&1 | tee /tmp/flowclean-drill-rollback.log | tail -30

digest "$DRILL_DB" /tmp/flowclean-drill-after-rollback.txt
echo "---- 回滚后整表指纹 ----"; cat /tmp/flowclean-drill-after-rollback.txt
if ! diff -u /tmp/flowclean-drill-before.txt /tmp/flowclean-drill-after-rollback.txt; then
    echo "✗ 回滚后指纹与 drill 之前不一致 —— down 不是精确可逆" >&2
    exit 1
fi
echo "✓ 可逆：回滚后 10 张表逐字节等于 drill 之前"

# ---- 6) 再 up 一遍（证明回滚后还能重新应用）----
echo ""
echo "===== ④ 回滚后再 apply ====="
FLOWCLEAN_DB="$DRILL_DB" FLOWCLEAN_BACKUP_DIR="$DRILL_BACKUP" \
    bash "$HERE/apply.sh" 2>&1 | tee /tmp/flowclean-drill-apply3.log | tail -25

echo ""
echo "===== ⑤ 收尾核对 ====="
q "$DRILL_DB" "select count(*) from biz_flow_def where deleted_at is null" | sed 's/^/存活流程定义 = /'
q "$DRILL_DB" "select string_agg(flow_code, ',' order by flow_code) from biz_flow_def where deleted_at is null" \
    | sed 's/^/存活代码 = /'

if [ "${KEEP_DRILL:-0}" != "1" ]; then
    su postgres -c "psql -q -c 'drop database if exists $DRILL_DB'"
    echo "==> 已删除演练库 $DRILL_DB"
else
    echo "==> KEEP_DRILL=1：保留演练库 $DRILL_DB"
fi

echo "== drill 完成：日志 /tmp/flowclean-drill-{apply1,apply2,rollback,apply3}.log =="
