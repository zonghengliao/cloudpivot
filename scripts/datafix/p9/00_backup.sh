#!/usr/bin/env bash
# P9 数据修复 · 第 0 步：备份
#
# 目标：/root/datafix-backup-<YYYYmmdd-HHMMSS>/
#   full.dump           整库（pg_dump -Fc，恢复用 pg_restore）
#   schema.sql          仅表结构（对照用）
#   <table>.sql         受影响表的全量数据（-Fp，可直接 psql -f 回放单表）
#   MANIFEST.txt        文件清单 + md5 + 库信息 + 执行时刻
#
# 用法：bash scripts/datafix/p9/00_backup.sh [标签]
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
DB_NAME="${P9_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"   # P9_DB 优先：演练库 / 隔离库用

TS="${1:-$(date +%Y%m%d-%H%M%S)}"
DIR="/root/datafix-backup-$TS"

TABLES=(
    biz_stock_in biz_stock_in_item
    biz_inventory biz_inventory_location biz_inventory_txn
    biz_voucher biz_voucher_entry biz_voucher_rule
    biz_payable biz_seq biz_stock_transfer biz_account biz_accounting_period
)

mkdir -p "$DIR"
echo "==> 备份到 $DIR（库 $DB_NAME@$DB_HOST:$DB_PORT）"

pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc \
        -f "$DIR/full.dump"
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" --schema-only \
        -f "$DIR/schema.sql"

for t in "${TABLES[@]}"; do
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
            --data-only --table="public.$t" -f "$DIR/$t.sql"
done

{
    echo "P9 datafix backup"
    echo "time      : $(date -Is)"
    echo "database  : $DB_NAME on $DB_HOST:$DB_PORT (user $DB_USER)"
    echo "created by: scripts/datafix/p9/00_backup.sh"
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
