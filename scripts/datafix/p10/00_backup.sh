#!/usr/bin/env bash
# P10 数据修复 · 第 0 步：备份（只备份受影响的 4 张表 + 整库）
#
# 目标：/root/datafix-backup-p10-<YYYYmmdd-HHMMSS>/
#   full.dump                 整库（pg_dump -Fc，恢复用 pg_restore）
#   schema.sql                仅表结构（对照用）
#   <table>.sql               受影响表的全量数据（-Fp，可直接 psql -f 回放单表）
#   m_w1a_004_rows.sql        本卡涉及的 4 张表里 M-W1A-004 的行（逐列、可读，回滚快照的来历）
#   MANIFEST.txt              文件清单 + md5 + 库信息 + 执行时刻 + 行数快照
#
# 用法：bash scripts/datafix/p10/00_backup.sh [标签]
set -euo pipefail

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
: "${NIK_DATABASE_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . 环境文件）}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_NAME="${P10_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"   # P10_DB 优先：演练库 / 隔离库用

TS="${1:-$(date +%Y%m%d-%H%M%S)}"
DIR="/root/datafix-backup-p10-$TS"

TABLES=(
    biz_inventory biz_inventory_location biz_inventory_txn
    biz_material_serial biz_stock_transfer biz_stock_transfer_item biz_stock_transfer_item_serial
)

mkdir -p "$DIR"
echo "==> 备份到 $DIR（库 $DB_NAME@$DB_HOST:$DB_PORT）"

pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc -f "$DIR/full.dump"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --schema-only -f "$DIR/schema.sql"

for t in "${TABLES[@]}"; do
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
            --data-only --table="public.$t" -f "$DIR/$t.sql"
done

# 修复对象的逐列快照（回滚脚本 10_…down.sql 里的 id / 时间戳就取自这里）
psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At -F'|' > "$DIR/m_w1a_004_rows.sql" <<'SQL'
\pset border 2
\echo '===== biz_inventory ====='
SELECT id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount,
       last_txn_at, created_at, updated_at
  FROM biz_inventory
 WHERE material_id = (SELECT id FROM biz_material WHERE material_code = 'M-W1A-004'
                       AND deleted_at IS NULL ORDER BY created_at LIMIT 1);
\echo '===== biz_inventory_location ====='
SELECT id, material_id, warehouse_id, location_id, qty, last_txn_at, created_at
  FROM biz_inventory_location
 WHERE material_id = (SELECT id FROM biz_material WHERE material_code = 'M-W1A-004'
                       AND deleted_at IS NULL ORDER BY created_at LIMIT 1);
\echo '===== biz_inventory_txn ====='
SELECT id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, qty, unit_cost,
       amount, before_qty, before_amount, before_cost, after_qty, after_amount, after_cost,
       period_code, operator_id, remark, created_at
  FROM biz_inventory_txn
 WHERE material_id = (SELECT id FROM biz_material WHERE material_code = 'M-W1A-004'
                       AND deleted_at IS NULL ORDER BY created_at LIMIT 1)
 ORDER BY created_at;
\echo '===== biz_material_serial（含已软删行）====='
SELECT id, serial_no, status, warehouse_id, location_id, source_type, source_id,
       inbound_at, outbound_at, created_at, updated_at, deleted_at
  FROM biz_material_serial
 WHERE material_id = (SELECT id FROM biz_material WHERE material_code = 'M-W1A-004'
                       AND deleted_at IS NULL ORDER BY created_at LIMIT 1)
 ORDER BY serial_no;
SQL

{
    echo "P10 datafix backup"
    echo "time      : $(date -Is)"
    echo "database  : $DB_NAME on $DB_HOST:$DB_PORT (user $DB_USER)"
    echo "created by: scripts/datafix/p10/00_backup.sh"
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
