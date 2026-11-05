#!/usr/bin/env bash
# ============================================================================
# ORGLANDING（P0-FIX-2B-A）· 第 0 步：备份
# ----------------------------------------------------------------------------
# 目标目录：/root/datafix-backup-orglanding-<YYYYmmdd-HHMMSS>/
#   full.dump           整库（pg_dump -Fc，恢复用 pg_restore）
#   schema.sql          仅表结构（对照用）
#   sys_user.sql        sys_user 全表数据（-Fp，可直接 psql -f 单表回放）
#   sys_dept.sql        sys_dept 全表数据（组织树是 dept_code 解析的依据）
#   sys_user_role.sql   角色绑定（本卡禁止动它，备份只为对账）
#   affected_rows.txt   本卡 4 个账号的逐列快照（落位前原值）+ 不动名单 + 三个「不该变」的计数
#   MANIFEST.txt        文件清单 + md5 + 库信息 + 执行时刻 + 行数快照
#
# 用法：bash scripts/datafix/org_landing/00_backup.sh [标签]
#       ORGLANDING_DB=<库名> bash scripts/datafix/org_landing/00_backup.sh drill   # 演练库/隔离库
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
DB_NAME="${ORGLANDING_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

TS="${1:-$(date +%Y%m%d-%H%M%S)}"
DIR="/root/datafix-backup-orglanding-$TS"

TABLES=(sys_user sys_dept sys_user_role)

mkdir -p "$DIR"
echo "==> 备份到 $DIR（库 $DB_NAME@$DB_HOST:$DB_PORT）"

pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc -f "$DIR/full.dump"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --schema-only -f "$DIR/schema.sql"
for t in "${TABLES[@]}"; do
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
            --data-only --table="public.$t" -f "$DIR/$t.sql"
done

# 落位对象的逐列快照（回滚核对 / 事后取证用）
psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q > "$DIR/affected_rows.txt" <<'SQL'
\pset border 2
\echo '===== 落位对象（4 个账号，落位前的原值）====='
SELECT u.username, u.status, u.is_locked, u.user_type,
       coalesce(d.dept_code, 'NULL') AS dept_code,
       coalesce(u.dept_id::text, 'NULL') AS dept_id,
       u.updated_at
  FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE u.username IN ('admin', 'laoliu', 'laoliu_test', 'cl_43870')
 ORDER BY u.username;
\echo '===== 不动名单（落位后必须仍无部门）====='
SELECT u.username, u.user_type, u.is_locked,
       coalesce(d.dept_code, 'NULL') AS dept_code
  FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE u.username IN ('henry.liao', 'verify-ai', 'qa.viewer')
 ORDER BY u.username;
\echo '===== 组织树（dept_code 解析依据；leader_id 一律 NULL 才符合 D-6）====='
SELECT dept_code, dept_name, parent_id IS NULL AS is_root,
       coalesce(leader_id::text, 'NULL') AS leader_id, status
  FROM sys_dept WHERE deleted_at IS NULL ORDER BY dept_code;
\echo '===== 三个「不该变」的计数（落位前后必须完全相同）====='
SELECT 'sys_user_role 行数            = ' || count(*) FROM sys_user_role;
SELECT 'biz_task.dept_id 非空行数     = ' || count(*) FROM biz_task WHERE dept_id IS NOT NULL;
SELECT 'biz_project.dept_id 非空行数  = ' || count(*) FROM biz_project WHERE dept_id IS NOT NULL;
SELECT 'sys_dept.leader_id 非空行数   = ' || count(*) FROM sys_dept WHERE leader_id IS NOT NULL;
SQL

{
    echo "ORGLANDING (P0-FIX-2B-A) datafix backup"
    echo "time      : $(date -Is)"
    echo "database  : $DB_NAME on $DB_HOST:$DB_PORT (user $DB_USER)"
    echo "created by: scripts/datafix/org_landing/00_backup.sh"
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
