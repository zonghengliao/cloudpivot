#!/usr/bin/env bash
# ============================================================================
# E2ERES3 / 00：动手前的全量备份（**没有这份备份，apply.sh 拒绝执行**）
# ----------------------------------------------------------------------------
# 产出目录 /root/datafix-backup-e2eres3-<YYYYmmdd-HHMMSS>/
#   target_tables.dump   本包会碰的 10 张表整表 pg_dump -Fc（可 pg_restore 回来）
#   target/<tbl>.csv     即将被软删的行（逐行，含完整列）—— 人可读的「改前样貌」
#   users.csv            6 个测试账号改前的 status / is_locked / 角色
#   table_counts.txt     改动前每张表总量 + 命中量
#   MANIFEST             库身份 + 行数 + md5 + git HEAD（apply.sh 用它的「目标库」行做闸）
#
# 用法（生产 = 144）：
#   ssh root@172.18.10.144
#   cd /opt/cloudpivot && bash scripts/datafix/e2e_residue3/00_backup.sh
#
# 环境：
#   E2ERES3_ENV_FILE     默认 /etc/cloudpivot/cloudpivot.env
#   E2ERES3_BACKUP_ROOT  默认 /root
#   E2ERES3_DB           目标库（默认 NIK_DATABASE_NAME）
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${E2ERES3_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
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
DB_NAME="${E2ERES3_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

ROOT="${E2ERES3_BACKUP_ROOT:-/root}"
TS="$(date '+%Y%m%d-%H%M%S')"
OUT="$ROOT/datafix-backup-e2eres3-$TS"
mkdir -p "$OUT/target"
chmod 700 "$OUT"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -X -At)
TABLES=(biz_task biz_task_comment biz_flow_instance sys_notice sys_announcement
        sys_file sys_export_task sys_help_category biz_warehouse sys_user)

echo "==> 备份目标：$OUT"
echo "==> 目标库  ：$DB_NAME @ $DB_HOST:$DB_PORT（user=$DB_USER）"

# ---------------------------------------------------------------- ① 整表 dump
echo "==> ① pg_dump 10 张表（整表兜底）"
ARG=()
for t in "${TABLES[@]}"; do ARG+=(-t "$t"); done
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc "${ARG[@]}" \
    -f "$OUT/target_tables.dump"
echo "    target_tables.dump  $(stat -c%s "$OUT/target_tables.dump") bytes"

# ---------------------------------------------------------------- ② 命中行 CSV
echo "==> ② 逐表导出「即将被软删的行」"
"${PSQL[@]}" -v ON_ERROR_STOP=1 -c "\\copy (
  select 'biz_task' as tbl, id, task_no::text, title::text, status::text, create_by::text, created_at, deleted_at
    from biz_task where deleted_at is null
     and (title like '[E2E]%' or title like '[E9E2E]%')
  union all
  select 'biz_task_comment', id, 'comment', left(content,60)::text, 'x', create_by::text, created_at, deleted_at
    from biz_task_comment where deleted_at is null
     and task_id in (select id from biz_task where title like '[E2E]%' or title like '[E9E2E]%')
  union all
  select 'biz_flow_instance', id, flow_code::text, title::text, status::text, initiator_id::text, created_at, deleted_at
    from biz_flow_instance where deleted_at is null and title ilike '%E2E%'
  union all
  select 'sys_notice', id, notice_no::text, left(title,60)::text, status::text, create_by::text, created_at, deleted_at
    from sys_notice where deleted_at is null and title ilike '%E2E%'
  union all
  select 'sys_announcement', id, 'announcement', left(title,60)::text, status::text, create_by::text, created_at, deleted_at
    from sys_announcement where deleted_at is null and title ilike '%E2E%'
  union all
  select 'sys_file', id, file_name::text, original_name::text, 'x', create_by::text, created_at, deleted_at
    from sys_file where deleted_at is null
     and (original_name ilike 'e2e%' or original_name ilike 'e9-%')
  union all
  select 'sys_export_task', id, export_name::text, file_name::text, status::text, create_by::text, created_at, deleted_at
    from sys_export_task where deleted_at is null
     and (export_name ilike 'e2e%' or file_name ilike 'e2e%')
  union all
  select 'sys_help_category', id, code::text, name::text, 'x', 'system', created_at, deleted_at
    from sys_help_category where deleted_at is null
     and (code ilike 'e2e%' or name ilike '%E2E%')
  union all
  select 'biz_warehouse', id, warehouse_code::text, warehouse_name::text, status::text, create_by::text, created_at, deleted_at
    from biz_warehouse where deleted_at is null
     and (warehouse_code like 'E2E-%' or warehouse_name ilike '%E2E%')
  order by 1, 7
) to '$OUT/target/residue_rows.csv' with (format csv, header true)"
echo "    target/residue_rows.csv  $(($(wc -l < "$OUT/target/residue_rows.csv") - 1)) 行"

"${PSQL[@]}" -c "\\copy (
  select u.id, u.username, u.real_name, u.status, u.is_locked, u.user_type, u.last_login_at,
         coalesce(string_agg(r.role_code, ',' order by r.role_code), '-') as roles,
         u.created_at
    from sys_user u
    left join sys_user_role ur on ur.user_id = u.id
    left join sys_role r on r.id = ur.role_id and r.deleted_at is null
   where u.username in ('laoliu_test','qa.viewer','qa.limited','qa.mutex','cl_43870','verify-ai')
   group by u.id, u.username, u.real_name, u.status, u.is_locked, u.user_type, u.last_login_at, u.created_at
   order by u.username
) to '$OUT/users.csv' with (format csv, header true)"
echo "    users.csv  $(($(wc -l < "$OUT/users.csv") - 1)) 行"

# ---------------------------------------------------------------- ③ 改动前计数
echo "==> ③ 改动前计数"
{
  echo "db=$DB_NAME host=$DB_HOST port=$DB_PORT user=$DB_USER ts=$TS"
  "${PSQL[@]}" -F'|' -c "
    select 'marker_rows_alive|'||(select count(*) from biz_task where deleted_at is null and (title like '[E2E]%' or title like '[E9E2E]%'))
      ||'/'||(select count(*) from biz_task_comment where deleted_at is null and task_id in (select id from biz_task where title like '[E2E]%' or title like '[E9E2E]%'))
      ||'/'||(select count(*) from sys_notice where deleted_at is null and title ilike '%E2E%')
      ||'/'||(select count(*) from sys_file where deleted_at is null and (original_name ilike 'e2e%' or original_name ilike 'e9-%'))
      ||'/'||(select count(*) from sys_announcement where deleted_at is null and title ilike '%E2E%')
      ||'/'||(select count(*) from sys_export_task where deleted_at is null and (export_name ilike 'e2e%' or file_name ilike 'e2e%'))
      ||'/'||(select count(*) from sys_help_category where deleted_at is null and (code ilike 'e2e%' or name ilike '%E2E%'))
      ||'/'||(select count(*) from biz_flow_instance where deleted_at is null and title ilike '%E2E%')
      ||'/'||(select count(*) from biz_warehouse where deleted_at is null and (warehouse_code like 'E2E-%' or warehouse_name ilike '%E2E%'))
      ||' (task/comment/notice/file/announce/export/help/flow/warehouse)';
    select 'test_accounts_enabled|'||count(*) from sys_user
     where deleted_at is null and status=1 and username in ('laoliu_test','qa.viewer','qa.limited','qa.mutex','cl_43870','verify-ai');
    select 'accounting|voucher='||(select count(*) from biz_voucher)
      ||' entry='||(select count(*) from biz_voucher_entry)
      ||' payable='||(select count(*) from biz_payable)
      ||' payment='||(select count(*) from biz_payment)
      ||' inventory='||(select count(*) from biz_inventory)
      ||' txn='||(select count(*) from biz_inventory_txn)
      ||' period='||(select count(*) from biz_accounting_period)
      ||' balance='||(select count(*) from biz_opening_balance);
    select 'audit|status_log='||(select count(*) from biz_status_log)
      ||' operation_logs='||(select count(*) from operation_logs)
      ||' audit_logs='||(select count(*) from audit_logs);
    select 'tables|task='||(select count(*) from biz_task)
      ||' user='||(select count(*) from sys_user)
      ||' notice='||(select count(*) from sys_notice)
      ||' file='||(select count(*) from sys_file);"
} | tee "$OUT/table_counts.txt"

# ---------------------------------------------------------------- ④ MANIFEST
echo "==> ④ MANIFEST"
{
  echo "# E2ERES3 备份清单"
  echo "# 生成时刻: $(date '+%F %T %Z')"
  echo "# 目标库  : $DB_NAME @ $DB_HOST:$DB_PORT (user=$DB_USER)"
  echo "# 口径    : scripts/verify/e2e_marker.py::TABLE_SPECS 的 SQL 镜像"
  echo "#           （任务/评论/流程实例/通知/公告/文件/导出/帮助分类/仓库 九类标记行 + 6 个测试账号）"
  if git -C "$HERE/../../.." rev-parse HEAD >/dev/null 2>&1; then
      echo "# git HEAD: $(git -C "$HERE/../../.." rev-parse HEAD)"
  fi
  echo "# ---- table_counts.txt ----"
  cat "$OUT/table_counts.txt"
  echo "# ---- files (md5 / bytes) ----"
  ( cd "$OUT" && md5sum target_tables.dump target/*.csv users.csv \
      | while read -r sum file; do printf '%s  %-24s %s\n' "$sum" "$file" "$(stat -c%s "$OUT/$file")"; done )
} > "$OUT/MANIFEST"
chmod 600 "$OUT/MANIFEST"

echo "==> 备份完成：$OUT"
echo "    回滚兜底：pg_restore -d $DB_NAME --clean $OUT/target_tables.dump"
echo "    之后执行：bash $HERE/apply.sh"
