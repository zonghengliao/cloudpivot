#!/usr/bin/env bash
# ============================================================================
# SUPERCONV（P0-FIX-2B-C）· 第 0 步：备份
# ----------------------------------------------------------------------------
# 目标目录：/root/datafix-backup-superconv-<YYYYmmdd-HHMMSS>/
#   full.dump           整库（pg_dump -Fc，恢复用 pg_restore）
#   schema.sql          仅表结构（对照用）
#   sys_user.sql        sys_user 全表数据（token_version 会被推进，回滚要用）
#   sys_role.sql        sys_role 全表数据（OPS_ADMIN 是本批次新建的）
#   sys_role_menu.sql   角色授权（OPS_ADMIN 的 72 行在这里）
#   sys_user_role.sql   角色绑定（本卡摘的就是它）
#   sys_menu.sql        菜单（OPS_ADMIN 的权限码来源）
#   affected_rows.txt   4 个账号逐行绑定 + 超管集合 + 3 个「不该变」的计数 + 2 张 PENDING 实例
#   MANIFEST.txt        文件清单 + md5 + 库信息 + 执行时刻 + 行数快照
#
# 用法：bash scripts/datafix/superconv/00_backup.sh [标签]
#       SUPERCONV_DB=<库名> bash scripts/datafix/superconv/00_backup.sh drill   # 演练库/隔离库
# ============================================================================
set -euo pipefail

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . 环境文件）}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-${ALUN_DATABASE_HOST:-127.0.0.1}}"
DB_PORT="${NIK_DATABASE_PORT:-${ALUN_DATABASE_PORT:-5432}}"
DB_USER="${NIK_DATABASE_USER:-${ALUN_DATABASE_USER:-cloudpivot}}"
DB_NAME="${SUPERCONV_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

TS="${1:-$(date +%Y%m%d-%H%M%S)}"
DIR="/root/datafix-backup-superconv-$TS"

TABLES=(sys_user sys_role sys_role_menu sys_user_role sys_menu)

mkdir -p "$DIR"
echo "==> 备份到 $DIR（库 $DB_NAME@$DB_HOST:$DB_PORT）"

pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc -f "$DIR/full.dump"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --schema-only -f "$DIR/schema.sql"
for t in "${TABLES[@]}"; do
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
            --data-only --table="public.$t" -f "$DIR/$t.sql"
done

psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q > "$DIR/affected_rows.txt" <<'SQL'
\pset border 2
\echo '===== 超管集合（摘权前后对照；目标 = 只剩 admin）====='
SELECT u.username, r.role_code, r.data_scope, ur.expire_time
  FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
  JOIN sys_role r ON r.id = ur.role_id
 WHERE r.role_code = 'SUPER_ADMIN' ORDER BY u.username;
\echo '===== 4 个账号的全部角色（摘权前后对照）====='
SELECT u.username, r.role_code, r.data_scope, ur.id AS user_role_id, ur.expire_time
  FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
  JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','henry.liao')
 ORDER BY u.username, r.role_code;
\echo '===== 账号状态（本卡不许动 status / is_locked / 有效期）====='
SELECT u.username, u.status, u.is_locked, u.token_version, u.user_type,
       coalesce(d.dept_code, 'NULL') AS dept_code
  FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','henry.liao') ORDER BY u.username;
\echo '===== 不该变的计数（摘权前后必须完全相同）====='
SELECT 'sys_user_role 行数                = ' || count(*) FROM sys_user_role;
SELECT 'sys_user 行数                     = ' || count(*) FROM sys_user;
SELECT 'sys_role 行数                     = ' || count(*) FROM sys_role WHERE deleted_at IS NULL;
SELECT 'biz_task.dept_id 非空行数         = ' || count(*) FROM biz_task WHERE dept_id IS NOT NULL;
SELECT 'biz_project.dept_id 非空行数      = ' || count(*) FROM biz_project WHERE dept_id IS NOT NULL;
SELECT 'biz_flow_task PENDING 行数        = ' || count(*) FROM biz_flow_task WHERE status = 'PENDING';
\echo '===== D13：2 张 PENDING 实例的待办（摘权不改它们一行）====='
SELECT t.id, i.instance_no, t.node_key, t.status, u.username AS assignee, t.created_at
  FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id
  LEFT JOIN sys_user u ON u.id = t.assignee_id
 WHERE i.instance_no IN ('FLW-20261102-94417B93','FLW-20261105-5D6B0905')
 ORDER BY i.instance_no, t.created_at, t.node_key;
SQL

{
    echo "SUPERCONV (P0-FIX-2B-C) datafix backup"
    echo "time      : $(date -Is)"
    echo "database  : $DB_NAME on $DB_HOST:$DB_PORT (user $DB_USER)"
    echo "created by: scripts/datafix/superconv/00_backup.sh"
    echo
    echo "== md5 =="
    (cd "$DIR" && md5sum ./* | sort -k2)
    echo
    echo "== 受影响表的行数快照 =="
    for t in "${TABLES[@]}"; do
        n=$(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Atc "SELECT count(*) FROM public.$t")
        echo "$t = $n"
    done
} > "$DIR/MANIFEST.txt"

echo "==> 完成，清单："
sed -n '1,12p' "$DIR/MANIFEST.txt"
echo "目录：$DIR"
