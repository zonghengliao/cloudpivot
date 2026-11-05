#!/usr/bin/env bash
# ============================================================================
# BIZPERM · 第 0 步：备份（授权变更前）
# ----------------------------------------------------------------------------
# 目录：/root/datafix-backup-bizperm-<YYYYmmdd-HHMMSS>/
#   full.dump          整库（pg_dump -Fc，兜底 pg_restore）
#   schema.sql         仅结构
#   sys_role_menu.sql  授权表数据（本卡改的就是它）
#   sys_menu.sql       菜单表数据（本卡新建 3 条按钮码）
#   sys_user.sql       token_version 会被推进
#   before_rows.txt    施工前逐角色/逐页面/逐账号的可读镜像（对平基准）
#   MANIFEST.txt       清单 + md5 + 库信息
#
# 用法：bash 00_backup.sh [标签]
#       BIZPERM_DB=<库> bash 00_backup.sh drill     # 演练库
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

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_NAME="${BIZPERM_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

TS="${1:-$(date +%Y%m%d-%H%M%S)}"
DIR="/root/datafix-backup-bizperm-$TS"
TABLES=(sys_role sys_role_menu sys_menu sys_user sys_user_role)

mkdir -p "$DIR"
echo "==> 备份到 $DIR（库 $DB_NAME@$DB_HOST:$DB_PORT）"

pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc -f "$DIR/full.dump"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --schema-only -f "$DIR/schema.sql"
for t in "${TABLES[@]}"; do
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
            --data-only --table="public.$t" -f "$DIR/$t.sql"
done

psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q > "$DIR/before_rows.txt" <<'SQL'
\pset border 2
\echo '===== 逐角色授权行数（施工前）====='
SELECT r.role_code, r.data_scope,
       count(*) FILTER (WHERE m.menu_type = 1) AS dirs,
       count(*) FILTER (WHERE m.menu_type = 2) AS pages,
       count(*) FILTER (WHERE m.menu_type = 3) AS buttons,
       count(*) AS total
  FROM sys_role r LEFT JOIN sys_role_menu rm ON rm.role_id = r.id
  LEFT JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
 GROUP BY 1,2 ORDER BY 1;
\echo '===== 5 角色并集覆盖的业务页（施工前）====='
WITH biz AS (SELECT id, path FROM sys_menu WHERE menu_type = 2 AND deleted_at IS NULL),
     g AS (SELECT DISTINCT rm.menu_id FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
            WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'))
SELECT count(*) AS covered_pages FROM biz JOIN g ON g.menu_id = biz.id;
\echo '===== 不该变的计数 ====='
SELECT 'sys_user 行数            = ' || count(*) FROM sys_user;
SELECT 'sys_user_role 行数       = ' || count(*) FROM sys_user_role;
SELECT 'sys_role_menu 行数       = ' || count(*) FROM sys_role_menu;
SELECT 'sys_menu 行数            = ' || count(*) FROM sys_menu WHERE deleted_at IS NULL;
SELECT 'SUPER_ADMIN 成员         = ' || count(*) FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id WHERE r.role_code='SUPER_ADMIN';
SELECT 'biz_task 行数            = ' || count(*) FROM biz_task;
SELECT 'biz_task.dept_id 非空    = ' || count(*) FROM biz_task WHERE dept_id IS NOT NULL;
SELECT 'biz_flow_task PENDING    = ' || count(*) FROM biz_flow_task WHERE status = 'PENDING';
SQL

{
    echo "BIZPERM (P0-FIX-2B-C 跟进) datafix backup"
    echo "time      : $(date -Is)"
    echo "database  : $DB_NAME on $DB_HOST:$DB_PORT (user $DB_USER)"
    echo "created by: scripts/datafix/bizperm/00_backup.sh"
    echo
    echo "== md5 =="
    (cd "$DIR" && md5sum ./* 2>/dev/null | sort -k2)
    echo
    echo "== 行数快照 =="
    for t in "${TABLES[@]}"; do
        echo "$t = $(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Atc "SELECT count(*) FROM public.$t")"
    done
    echo
    echo "== 还原方式 =="
    echo "pg_restore -h $DB_HOST -p $DB_PORT -U $DB_USER -d <目标库> --clean --if-exists $DIR/full.dump"
} > "$DIR/MANIFEST.txt"

echo "==> 完成，清单："
sed -n '1,12p' "$DIR/MANIFEST.txt"
echo "目录：$DIR"
