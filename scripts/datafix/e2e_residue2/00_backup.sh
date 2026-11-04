#!/usr/bin/env bash
# ============================================================================
# E2ERES2 / 00：删之前的全量备份（flow 9 表 + biz_status_log 审计表 + biz_task）
# ----------------------------------------------------------------------------
# 产出目录 /root/datafix-backup-e2eres2-<YYYYmmdd-HHMMSS>/
#   flow_tables.dump       9 张 flow 表的 pg_dump -Fc（整表兜底，可 pg_restore 回来）
#   biz_status_log.dump    biz_status_log 整表 dump（**审计证据表**，只备份不删）
#   biz_task.dump          biz_task 整表 dump（有 flow_instance_id 指向实例）
#   target/<tbl>.csv       每张表「即将被删的行」逐行列出（SET A-1 定义 + SET A-2 实例口径）
#   target/classification.csv
#                          **判决表**：每一条目标定义/实例的 create_by / flow_name /
#                          created_at / 业务单据引用计数 —— 证明「不是按前缀猜垃圾」
#   target/biz_status_log_target_refs.csv
#                          biz_status_log 里指向目标实例的行（实测 0 行 —— 留档就是为了
#                          证明「0 行」，而不是「忘了导」）
#   table_counts.txt       每张表删除前总量 / 命中量
#   MANIFEST               库身份 + 行数 + md5 + git HEAD
#
# 用法（生产 = 144）：
#   ssh root@172.18.10.144
#   cd /opt/cloudpivot && bash scripts/datafix/e2e_residue2/00_backup.sh
#
# 环境：
#   E2ERES2_ENV_FILE     默认 /etc/cloudpivot/cloudpivot.env
#   E2ERES2_BACKUP_ROOT  默认 /root
#   E2ERES2_DB           目标库（默认 NIK_DATABASE_NAME）
# ============================================================================
set -euo pipefail

ENV_FILE="${E2ERES2_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
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
DB_NAME="${E2ERES2_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

ROOT="${E2ERES2_BACKUP_ROOT:-/root}"
TS="$(date '+%Y%m%d-%H%M%S')"
OUT="$ROOT/datafix-backup-e2eres2-$TS"
mkdir -p "$OUT/target"
chmod 700 "$OUT"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1 -X)

echo "==> 备份目标：$OUT"
echo "==> 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（user=$DB_USER）"

# ---------------------------------------------------------------- 判据（与 10_precheck.sql 逐字一致）
# SET A-1：已软删 + biz_type 是业务值（上一张卡的口径不碰）+ flow_code 命中前缀枚举
TGT_DEF="(select d.id from biz_flow_def d
           where d.deleted_at is not null
             and not (left(d.biz_type, 4) = 'E2E_' or d.biz_type = 'D2_SMOKE')
             and d.flow_code ~ '^(e2e|diag|diag2|vrf|d5|acc|tn)_')"
# SET A-2：活实例 + 父定义已软删（孤儿实例）
TGT_INST="(select i.id from biz_flow_instance i
            where i.deleted_at is null and i.def_id in $TGT_DEF)"

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
select 'SET_A1_biz_flow_def|'||count(*) from biz_flow_def where id in $TGT_DEF;
select 'SET_A2_biz_flow_instance|'||count(*) from biz_flow_instance where id in $TGT_INST;
select 'all_biz_flow_def|total='||count(*)||'|soft_deleted='||count(*) filter (where deleted_at is not null)
  from biz_flow_def;
select 'all_biz_flow_instance|total='||count(*)||'|pending='||count(*) filter (where status='PENDING')
  from biz_flow_instance;
select 'alive_flow_def|'||count(*) from biz_flow_def where deleted_at is null;
select 'biz_status_log|'||count(*) from biz_status_log;
select 'biz_flow_task|'||count(*) from biz_flow_task;
select 'biz_flow_cc|'||count(*) from biz_flow_cc;
SQL
} | tee "$OUT/table_counts.txt"

# ---------------------------------------------------------------- 逐表导出待删行
echo "==> ③ 导出「即将被删的行」（CSV，含表头）"
copy_out() { # <file> <select-sql>
    "${PSQL[@]}" -c "\\copy ($2) to '$OUT/target/$1' with (format csv, header true)"
    echo "    target/$1 -> $(wc -l < "$OUT/target/$1") 行（含表头）"
}

copy_out biz_flow_def.csv \
  "select * from biz_flow_def where id in $TGT_DEF order by flow_code"
copy_out biz_flow_def_version.csv \
  "select * from biz_flow_def_version where def_id in $TGT_DEF order by def_id, version_no"
copy_out biz_flow_node.csv \
  "select * from biz_flow_node where version_id in (select id from biz_flow_def_version where def_id in $TGT_DEF) order by version_id, node_key"
copy_out biz_flow_node_assignee.csv \
  "select * from biz_flow_node_assignee where node_id in (select id from biz_flow_node where version_id in (select id from biz_flow_def_version where def_id in $TGT_DEF))"
copy_out biz_flow_edge.csv \
  "select * from biz_flow_edge where version_id in (select id from biz_flow_def_version where def_id in $TGT_DEF) order by version_id, edge_key"
copy_out biz_flow_instance.csv \
  "select * from biz_flow_instance where id in $TGT_INST order by flow_code"
copy_out biz_flow_instance_node.csv \
  "select * from biz_flow_instance_node where instance_id in $TGT_INST order by instance_id, round, node_key"
copy_out biz_flow_task.csv \
  "select * from biz_flow_task where instance_id in $TGT_INST order by instance_id"
copy_out biz_flow_cc.csv \
  "select * from biz_flow_cc where instance_id in $TGT_INST"

# ---------------------------------------------------------------- 判决表（本卡的核心证据）
echo "==> ④ 判决表 classification.csv（逐条：谁建的 / 叫什么 / 什么时候 / 有没有单据引用）"
"${PSQL[@]}" -c "\\copy (
  select 'DEF' as kind, d.id, d.flow_code, d.flow_name, d.biz_type, d.status,
         coalesce(u.username,'-') as created_by,
         to_char(d.created_at,'YYYY-MM-DD HH24:MI:SS') as created_at,
         to_char(d.deleted_at,'YYYY-MM-DD HH24:MI:SS') as deleted_at,
         (select count(*) from biz_flow_instance i where i.def_id = d.id) as instances,
         (select count(*) from biz_flow_def_version v where v.def_id = d.id) as versions,
         0 as doc_refs
    from biz_flow_def d left join sys_user u on u.id = d.create_by
   where d.id in $TGT_DEF
  union all
  select 'INST', i.id, i.flow_code, i.flow_name, i.biz_type, i.status,
         coalesce(u.username,'-') as initiator,
         to_char(i.created_at,'YYYY-MM-DD HH24:MI:SS'),
         to_char(i.ended_at,'YYYY-MM-DD HH24:MI:SS') as ended_at,
         0, i.version_no,
         (select count(*) from biz_contract        x where x.approval_instance_id = i.id)
       + (select count(*) from biz_payable         x where x.cancel_instance_id = i.id or x.payment_approval_instance_id = i.id)
       + (select count(*) from biz_payment         x where x.approval_instance_id = i.id)
       + (select count(*) from biz_purchase_order  x where x.approval_instance_id = i.id)
       + (select count(*) from biz_stock_in        x where x.approval_instance_id = i.id)
       + (select count(*) from biz_stock_take      x where x.approval_instance_id = i.id)
       + (select count(*) from biz_stock_transfer  x where x.approval_instance_id = i.id)
       + (select count(*) from biz_task            x where x.flow_instance_id = i.id or x.approval_instance_id = i.id)
       + (select count(*) from biz_voucher         x where x.pending_reverse_instance_id = i.id) as doc_refs
    from biz_flow_instance i left join sys_user u on u.id = i.initiator_id
   where i.id in $TGT_INST
   order by kind, flow_code
) to '$OUT/target/classification.csv' with (format csv, header true)"
echo "    target/classification.csv -> $(wc -l < "$OUT/target/classification.csv") 行（含表头）"

# 审计证据表：把「指向目标实例的行」导出来留档（实测 0 行，导出即证明）
copy_out biz_status_log_target_refs.csv \
  "select * from biz_status_log where entity_id in $TGT_INST order by created_at"
# 审计表整表留档（删场前后都要能对账 113 行一行没动）
"${PSQL[@]}" -c "\\copy (select * from biz_status_log order by created_at) to '$OUT/target/biz_status_log_FULL.csv' with (format csv, header true)"

# ---------------------------------------------------------------- MANIFEST
echo "==> ⑤ MANIFEST（md5）"
{
  echo "# E2ERES2 备份清单"
  echo "# 生成时刻: $(date '+%F %T %Z')"
  echo "# 目标库  : $DB_NAME @ $DB_HOST:$DB_PORT (user=$DB_USER)"
  echo "# 口径    : SET A-1 = 已软删 + biz_type 业务值 + flow_code ~ ^(e2e|diag|diag2|vrf|d5|acc|tn)_"
  echo "#           SET A-2 = 活实例 + 父定义已软删（孤儿实例）"
  echo "#           判性质证据见 target/classification.csv（create_by / flow_name / created_at / doc_refs）"
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
