#!/usr/bin/env bash
# ============================================================================
# FLOWCLEAN · 00 备份（清 prod 之前的第一件事，不许省）
# ----------------------------------------------------------------------------
# 目标：删掉 6 个测试/演示审批流之前，把「整个 biz_flow_* 家族」留一份可回放的备份。
#
# 产出目录：/root/datafix-backup-flowclean-<YYYYmmdd-HHMMSS>（或用 FLOWCLEAN_BACKUP_DIR 指定）
#   flow_tables.dump     9 张 flow 表合成一份 custom-format（pg_restore 用）
#   <表>.sql             逐表 pg_dump --no-owner --no-privileges（可单独回放到空库）
#   <表>.rows            该表总行数（psql count(*)）
#   target/<表>.csv      该表**只含 6 个待清理流程**的行（csv header，人工可读）
#   keeper/<表>.csv      该表**只含 6 个保留流程**的行（事后校对「没动过」）
#   MANIFEST.txt         库名 / 主机 / 迁移版本 / 逐表行数 / md5 / 目标范围快照
#
# 用法：
#   bash scripts/datafix/flowclean/00_backup.sh              # 自动时间戳
#   bash scripts/datafix/flowclean/00_backup.sh drill        # 演练库用固定标签
#   FLOWCLEAN_DB=cloudpivot_flowclean_drill bash .../00_backup.sh drill
#
# 连接口径与生产一致：set -a; . /etc/cloudpivot/cloudpivot.env; set +a
# ============================================================================
set -euo pipefail

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi

DB_PASSWORD="${NIK_DATABASE_PASSWORD:-${ALUN_DATABASE_PASSWORD:-}}"
: "${DB_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . /etc/cloudpivot/cloudpivot.env）}"
export PGPASSWORD="$DB_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-${ALUN_DATABASE_HOST:-127.0.0.1}}"
DB_PORT="${NIK_DATABASE_PORT:-${ALUN_DATABASE_PORT:-5432}}"
DB_USER="${NIK_DATABASE_USER:-${ALUN_DATABASE_USER:-cloudpivot}}"
DB_NAME="${FLOWCLEAN_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

TABLES=(biz_flow_def biz_flow_def_version biz_flow_node biz_flow_node_assignee
        biz_flow_edge biz_flow_instance biz_flow_instance_node biz_flow_task biz_flow_cc)

# 6 个待清理（精确匹配，不用 LIKE）
DEL_CODES="'acc_tasknode_141800','acc_tn_141831','d5_281866','d5_282996','e2e_cs_084913','p12_canvas_demo'"
# 6 个保留
KEEP_CODES="'task_accept','stock_transfer','stock_in_cancel','payable_pay','po_approve','stock_in_post'"

TAG="${1:-$(date +%Y%m%d-%H%M%S)}"
DIR="${FLOWCLEAN_BACKUP_DIR:-/root/datafix-backup-flowclean-$TAG}"
mkdir -p "$DIR" "$DIR/target" "$DIR/keeper"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At -v ON_ERROR_STOP=1)

echo "== FLOWCLEAN 备份：$DB_NAME @ $DB_HOST:$DB_PORT → $DIR =="

# ---- 1) 逐表 pg_dump（纯 SQL）+ 行数 ----
for t in "${TABLES[@]}"; do
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
            --no-owner --no-privileges --table "public.$t" > "$DIR/$t.sql"
    "${PSQL[@]}" -c "select count(*) from public.$t" > "$DIR/$t.rows"
done

# ---- 2) 合成一份 custom-format（pg_restore 恢复用）----
PGS=()
for t in "${TABLES[@]}"; do PGS+=(-t "public.$t"); done
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
        --no-owner --no-privileges -Fc "${PGS[@]}" > "$DIR/flow_tables.dump"

# ---- 3) 目标范围 / 保留范围的 CSV（每个表都导一份）----
copy_csv() {  # copy_csv <sql> <out>
    "${PSQL[@]}" -c "\\copy ($1) to '$2' csv header"
}

# 定义与版本：直接按 flow_code / def_id 收敛
copy_csv "select * from biz_flow_def where flow_code in ($DEL_CODES)"        "$DIR/target/biz_flow_def.csv"
copy_csv "select * from biz_flow_def where flow_code in ($KEEP_CODES)"       "$DIR/keeper/biz_flow_def.csv"

for scope in target keeper; do
    if [ "$scope" = target ]; then CODES="$DEL_CODES"; else CODES="$KEEP_CODES"; fi
    copy_csv "select v.* from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id where d.flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_def_version.csv"
    copy_csv "select n.* from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_node.csv"
    copy_csv "select a.* from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_node_assignee.csv"
    copy_csv "select e.* from biz_flow_edge e join biz_flow_def_version v on v.id=e.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_edge.csv"
    copy_csv "select * from biz_flow_instance where flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_instance.csv"
    copy_csv "select ni.* from biz_flow_instance_node ni join biz_flow_instance i on i.id=ni.instance_id where i.flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_instance_node.csv"
    copy_csv "select t.* from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_task.csv"
    copy_csv "select c.* from biz_flow_cc c join biz_flow_instance i on i.id=c.instance_id where i.flow_code in ($CODES)" \
             "$DIR/$scope/biz_flow_cc.csv"
done

# 被 FOREIGN KEY ... ON DELETE SET NULL 牵连的外部行（biz_task.flow_instance_id）
copy_csv "select t.* from biz_task t where t.flow_instance_id in (select id from biz_flow_instance where flow_code in ($DEL_CODES))" \
         "$DIR/target/biz_task_ref.csv"

# ---- 4) MANIFEST ----
{
    echo "FLOWCLEAN backup manifest"
    echo "captured_at : $(date '+%Y-%m-%d %H:%M:%S %z')"
    echo "database    : $DB_NAME on $DB_HOST:$DB_PORT (user=$DB_USER)"
    echo "migrations  : $( "${PSQL[@]}" -c 'select max(version::int) from schema_migrations' )"
    echo "delete_codes: $(echo "$DEL_CODES" | tr -d "'")"
    echo "keep_codes  : $(echo "$KEEP_CODES" | tr -d "'")"
    echo ""
    echo "== 全表行数 / 文件 md5 =="
    for t in "${TABLES[@]}"; do
        printf '%-26s rows=%-8s md5=%s\n' "$t" "$(cat "$DIR/$t.rows")" "$(md5sum "$DIR/$t.sql" | cut -d' ' -f1)"
    done
    echo ""
    echo "== 目标范围（6 个待删流程）行数 =="
    for f in "$DIR"/target/*.csv; do
        printf '%-40s lines=%s\n' "$(basename "$f")" "$(( $(wc -l < "$f") - 1 ))"
    done
    echo ""
    echo "== 保留范围（6 个业务流）行数 =="
    for f in "$DIR"/keeper/*.csv; do
        printf '%-40s lines=%s\n' "$(basename "$f")" "$(( $(wc -l < "$f") - 1 ))"
    done
    echo ""
    echo "== 全库基线（verify 对账用）=="
    "${PSQL[@]}" -F'|' -c "
      select 'alive_flow_defs=' || count(*) from biz_flow_def where deleted_at is null
      union all select 'orphan_alive_instances=' || count(*) from biz_flow_instance i
        where i.deleted_at is null and not exists (select 1 from biz_flow_def d where d.flow_code=i.flow_code and d.deleted_at is null)
      union all select 'orphan_pending_tasks=' || count(*) from biz_flow_task ft
        where ft.status='PENDING' and not exists (
          select 1 from biz_flow_instance i join biz_flow_def d on d.flow_code=i.flow_code and d.deleted_at is null where i.id=ft.instance_id)"
} > "$DIR/MANIFEST.txt"

echo "-- MANIFEST --"
cat "$DIR/MANIFEST.txt"
echo "== 备份完成：$DIR（flow_tables.dump $(du -h "$DIR/flow_tables.dump" | cut -f1)）=="
