#!/usr/bin/env bash
# ============================================================================
# E2ERES / 00：删之前的全量备份（flow 9 表 + biz_status_log 审计表 + biz_task）
# ----------------------------------------------------------------------------
# 产出目录 /root/datafix-backup-e2eres-<YYYYmmdd-HHMMSS>/
#   flow_tables.dump       9 张 flow 表的 pg_dump -Fc（整表兜底，可 pg_restore 回来）
#   biz_status_log.dump    biz_status_log 整表 dump（**审计证据表**，只备份不删）
#   biz_task.dump          biz_task 整表 dump（有 flow_instance_id 指向实例）
#   target/<tbl>.csv       每张表「即将被删的行」逐行列出（E2E_*/D2_SMOKE 口径）
#   target/biz_status_log_residue.csv
#                          biz_status_log 里 entity_type/entity_no/biz_ref_no 命中
#                          E2E_*/D2_SMOKE 的行（2026-11-04 实测 0 行 —— 留档就是为了
#                          证明「0 行」，而不是「忘了导」）
#   table_counts.txt       每张表删除前总量 / 命中量
#   MANIFEST               库身份 + 行数 + md5 + git HEAD
#
# 用法（生产 = 144）：
#   ssh root@172.18.10.144
#   cd /opt/cloudpivot && bash scripts/datafix/e2e_residue/00_backup.sh
#
# 环境：
#   E2ERES_ENV_FILE     默认 /etc/cloudpivot/cloudpivot.env
#   E2ERES_BACKUP_ROOT  默认 /root
#   E2ERES_DB           目标库（默认 NIK_DATABASE_NAME）
# ============================================================================
set -euo pipefail

ENV_FILE="${E2ERES_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
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
DB_NAME="${E2ERES_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

ROOT="${E2ERES_BACKUP_ROOT:-/root}"
TS="$(date '+%Y%m%d-%H%M%S')"
OUT="$ROOT/datafix-backup-e2eres-$TS"
mkdir -p "$OUT/target"
chmod 700 "$OUT"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -X)

echo "==> 备份目标：$OUT"
echo "==> 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（user=$DB_USER）"

# 口径与 scripts/verify/biz_type_domain.py::is_test_residue 逐字一致：
#   biz_type 以 'E2E_' 开头  或  biz_type = 'D2_SMOKE'
RESIDUE_SQL="(left(biz_type, 4) = 'E2E_' OR biz_type = 'D2_SMOKE')"

# ---------------------------------------------------------------- 整表 dump
echo "==> ① pg_dump 9 张 flow 表 + 审计表 + biz_task"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc \
    -t biz_flow_def -t biz_flow_def_version -t biz_flow_node -t biz_flow_node_assignee \
    -t biz_flow_edge -t biz_flow_instance -t biz_flow_instance_node -t biz_flow_task \
    -t biz_flow_cc \
    -f "$OUT/flow_tables.dump"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc \
    -t biz_status_log -f "$OUT/biz_status_log.dump"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc \
    -t biz_task -f "$OUT/biz_task.dump"

# ---------------------------------------------------------------- 删除前计数
echo "==> ② 删除前计数"
{
  echo "db=$DB_NAME host=$DB_HOST port=$DB_PORT user=$DB_USER ts=$TS"
  "${PSQL[@]}" -At -F'|' <<SQL
select 'biz_flow_def|total='||count(*)||'|residue='||count(*) filter (where $RESIDUE_SQL)
  from biz_flow_def;
select 'biz_flow_instance|total='||count(*)||'|residue='||count(*) filter (where $RESIDUE_SQL)
  from biz_flow_instance;
select 'biz_flow_task|total='||count(*) from biz_flow_task;
select 'biz_status_log|total='||count(*) from biz_status_log;
select 'alive_flow_def|'||count(*) from biz_flow_def where deleted_at is null;
SQL
} | tee "$OUT/table_counts.txt"

# ---------------------------------------------------------------- 逐表导出待删行
echo "==> ③ 导出「即将被删的行」（CSV，含表头）"
copy_out() { # <file> <select-sql>
    "${PSQL[@]}" -c "\\copy ($2) to '$OUT/target/$1' with (format csv, header true)"
    echo "    target/$1 -> $(wc -l < "$OUT/target/$1") 行（含表头）"
}

copy_out biz_flow_def.csv \
  "select * from biz_flow_def where $RESIDUE_SQL order by biz_type"
copy_out biz_flow_def_version.csv \
  "select * from biz_flow_def_version where def_id in (select id from biz_flow_def where $RESIDUE_SQL) order by def_id, version_no"
copy_out biz_flow_node.csv \
  "select * from biz_flow_node where version_id in (select id from biz_flow_def_version where def_id in (select id from biz_flow_def where $RESIDUE_SQL)) order by version_id, node_key"
copy_out biz_flow_node_assignee.csv \
  "select * from biz_flow_node_assignee where node_id in (select id from biz_flow_node where version_id in (select id from biz_flow_def_version where def_id in (select id from biz_flow_def where $RESIDUE_SQL)))"
copy_out biz_flow_edge.csv \
  "select * from biz_flow_edge where version_id in (select id from biz_flow_def_version where def_id in (select id from biz_flow_def where $RESIDUE_SQL)) order by version_id, edge_key"
copy_out biz_flow_instance.csv \
  "select * from biz_flow_instance where $RESIDUE_SQL order by biz_type"
copy_out biz_flow_instance_node.csv \
  "select * from biz_flow_instance_node where instance_id in (select id from biz_flow_instance where $RESIDUE_SQL) order by instance_id, round, node_key"
copy_out biz_flow_task.csv \
  "select * from biz_flow_task where instance_id in (select id from biz_flow_instance where $RESIDUE_SQL) order by instance_id"
copy_out biz_flow_cc.csv \
  "select * from biz_flow_cc where instance_id in (select id from biz_flow_instance where $RESIDUE_SQL)"

# 审计证据表：把「会被删的那部分」导出来留档（实测 0 行，导出即证明）
copy_out biz_status_log_residue.csv \
  "select * from biz_status_log where entity_type like 'E2E\_%' or entity_type = 'D2_SMOKE' or entity_no like 'E2E\_%' or coalesce(biz_ref_no,'') like 'E2E\_%' order by created_at"
# 审计表整表留档（删场前后都要能对账 113 行一行没动）
"${PSQL[@]}" -c "\\copy (select * from biz_status_log order by created_at) to '$OUT/target/biz_status_log_FULL.csv' with (format csv, header true)"

# ---------------------------------------------------------------- MANIFEST
echo "==> ④ MANIFEST（md5）"
{
  echo "# E2ERES 备份清单"
  echo "# 生成时刻: $(date '+%F %T %Z')"
  echo "# 目标库  : $DB_NAME @ $DB_HOST:$DB_PORT (user=$DB_USER)"
  echo "# 口径    : biz_type 以 E2E_ 开头 或 = D2_SMOKE（= biz_type_domain.py::is_test_residue）"
  if git -C "$(dirname "$0")/../../.." rev-parse HEAD >/dev/null 2>&1; then
      echo "# git HEAD: $(git -C "$(dirname "$0")/../../.." rev-parse HEAD)"
  fi
  "${PSQL[@]}" -At -c "select '# server  : ' || version()"
  echo "# ---- table_counts.txt ----"
  cat "$OUT/table_counts.txt"
  echo "# ---- files (md5 / bytes) ----"
  ( cd "$OUT" && md5sum flow_tables.dump biz_status_log.dump biz_task.dump target/*.csv |
      while read -r sum file; do printf '%s  %-46s %s\n' "$sum" "$file" "$(stat -c%s "$OUT/$file")"; done )
} > "$OUT/MANIFEST"
chmod 600 "$OUT/MANIFEST"

echo "==> 备份完成：$OUT"
echo "    （回滚兜底：pg_restore -d $DB_NAME $OUT/flow_tables.dump）"
