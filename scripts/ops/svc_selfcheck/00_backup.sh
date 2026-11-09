#!/bin/bash
# ============================================================================
# 00_backup.sh —— 动手前的取证备份（sys_user / sys_role / sys_role_menu / sys_user_role）
#
# 为什么备份这四张表：卡 t_882d48c8 只**新增**两类主体（账号 + 只读角色）和两张关系表的行，
# 但一旦权限模型写错（例如角色挂歪、菜单勾多），要么回滚 SQL 要么按备份行复原；
# 备份里带 sha256 清单，事后能证明「当时是什么样」。
#
# 用法：
#   bash 00_backup.sh [--out DIR] [--env-file PATH]
#   默认 --out /root/svc-selfcheck-backup-<ts>，--env-file /etc/cloudpivot/cloudpivot.env
# 退出码：0 = 备份可用（dump > 1KB 且清单齐全）；1 = 失败
# ============================================================================
set -uo pipefail

SELF_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ENV_FILE=/etc/cloudpivot/cloudpivot.env
OUT=""

say() { printf '%s\n' "$*"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT=$2; shift 2 ;;
    --env-file) ENV_FILE=$2; shift 2 ;;
    -h|--help) sed -n '2,18p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) say "✗ 未知参数：$1"; exit 2 ;;
  esac
done

[ -n "$OUT" ] || OUT="/root/svc-selfcheck-backup-$(date +%Y%m%d-%H%M%S)"

# env 文件里只取库连接（不 source 整个文件，避免把无关口令灌进本进程环境）
_env_get() {
  local key=$1 line
  [ -r "$ENV_FILE" ] || return 0
  line=$(grep -m1 "^[[:space:]]*${key}=" "$ENV_FILE" 2>/dev/null) || return 0
  line=${line#*=}
  line=$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'$//")
  printf '%s' "$line"
}

DB_HOST=${NIK_DATABASE_HOST:-$(_env_get NIK_DATABASE_HOST)}
DB_PORT=${NIK_DATABASE_PORT:-$(_env_get NIK_DATABASE_PORT)}
DB_USER=${NIK_DATABASE_USER:-$(_env_get NIK_DATABASE_USER)}
DB_NAME=${NIK_DATABASE_NAME:-$(_env_get NIK_DATABASE_NAME)}
DB_PASS=${NIK_DATABASE_PASSWORD:-$(_env_get NIK_DATABASE_PASSWORD)}
DB_HOST=${DB_HOST:-127.0.0.1}; DB_PORT=${DB_PORT:-5432}; DB_NAME=${DB_NAME:-cloudpivot}

export PGPASSWORD="$DB_PASS"
PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

say "════ sys_user / sys_role / sys_role_menu / sys_user_role 备份 $(date '+%F %T') ════"
say "  目标库: $DB_USER@$DB_HOST:$DB_PORT/$DB_NAME"
say "  备份目录: $OUT"
mkdir -p "$OUT" || { say "✗ 建目录失败"; exit 1; }
chmod 700 "$OUT"

# 1) 四张表（结构 + 数据，--no-owner 便于跨环境还原）
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --no-owner --no-privileges \
  -t public.sys_user -t public.sys_role -t public.sys_role_menu -t public.sys_user_role \
  --format=plain > "$OUT/auth_tables.sql" 2> "$OUT/auth_tables.err"
if [ $? -ne 0 ]; then say "✗ pg_dump 失败：$(head -3 "$OUT/auth_tables.err")"; exit 1; fi
SIZE=$(stat -c %s "$OUT/auth_tables.sql")
if [ "$SIZE" -lt 1024 ]; then say "✗ dump 只有 ${SIZE}B（<1KB 视为空壳）"; exit 1; fi

# 2) 目标账号/角色的当前状态（人可读；不存在也留痕）
"${PSQL[@]}" -At -F'|' -c "
  SELECT 'sys_user|'||username||'|'||user_type||'|status='||status||'|must_change='||must_change_password
       ||'|locked='||is_locked||'|fails='||login_fail_count||'|dept='||coalesce(dept_id::text,'-')
       ||'|deleted='||coalesce(deleted_at::text,'-')
    FROM sys_user WHERE username IN ('svc_selfcheck','admin') ORDER BY username
  UNION ALL
  SELECT 'sys_role|'||role_code||'|scope='||data_scope||'|status='||status
       ||'|menus='||(SELECT count(*) FROM sys_role_menu rm WHERE rm.role_id = r.id)
       ||'|deleted='||coalesce(deleted_at::text,'-')
    FROM sys_role r WHERE role_code = 'OPS_SELFCHECK'
  UNION ALL
  SELECT 'counts|sys_user='||(SELECT count(*) FROM sys_user)
       ||' sys_role='||(SELECT count(*) FROM sys_role)
       ||' sys_role_menu='||(SELECT count(*) FROM sys_role_menu)
       ||' sys_user_role='||(SELECT count(*) FROM sys_user_role)
" > "$OUT/pre_state.txt" 2>> "$OUT/auth_tables.err" || true

# 3) 清单（sha256 + 大小），事后可校验
{
  echo "# svc_selfcheck 备份清单"
  echo "# created_at=$(date '+%F %T %z')"
  echo "# db=$DB_NAME host=$DB_HOST port=$DB_PORT"
  echo "# tables=sys_user,sys_role,sys_role_menu,sys_user_role"
  ( cd "$OUT" && sha256sum auth_tables.sql pre_state.txt 2>/dev/null )
} > "$OUT/MANIFEST.txt"

say "  ✅ dump ${SIZE}B  $(sha256sum "$OUT/auth_tables.sql" | cut -c1-16)…"
say "  ✅ 还原：psql -h \$H -p \$P -U \$U -d \$D -f $OUT/auth_tables.sql"
say "════ 备份完成 ════"
