#!/usr/bin/env bash
# P10 数据修复 · 幂等证明
#
# 做法：连跑两遍 apply.sh，**第二遍之后**受影响表的数据段必须与第一遍之后逐字节一致。
#   如果第二遍又改了行，说明脚本里有「无条件 UPDATE」或「不判重的 INSERT」。
#
# 用法：P10_DB=<演练库> bash scripts/datafix/p10/verify_idempotent.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
: "${NIK_DATABASE_PASSWORD:?需要 NIK_DATABASE_PASSWORD}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_NAME="${P10_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

TABLES=(biz_inventory biz_inventory_location biz_inventory_txn biz_material_serial
        biz_stock_transfer biz_stock_transfer_item biz_stock_transfer_item_serial)

dump_all() {  # $1 = 输出目录
    mkdir -p "$1"
    # 过滤 pg_dump 18 的随机 `\restrict <token>` 行（每次 dump 都不同，不是数据）
    for t in "${TABLES[@]}"; do
        pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
                --data-only --table="public.$t" \
            | grep -vE '^\\(un)?restrict ' > "$1/$t.sql"
    done
}

W="$(mktemp -d /tmp/p10-idem.XXXXXX)"

echo "== 第 1 遍 =="
FORCE=1 P10_DB="$DB_NAME" bash "$HERE/apply.sh" > "$W/run1.log" 2>&1
dump_all "$W/after1"

echo "== 第 2 遍（同库再跑一次）=="
FORCE=1 P10_DB="$DB_NAME" bash "$HERE/apply.sh" > "$W/run2.log" 2>&1
dump_all "$W/after2"

echo "== 对比两遍之后的受影响表数据段 =="
if diff -r "$W/after1" "$W/after2" > "$W/diff.txt" 2>&1; then
    echo "PASS：第二遍零改动（幂等）"
    echo "报告目录：$W"
    exit 0
fi
echo "FAIL：第二遍改变了数据，脚本不是幂等的：" >&2
sed -n '1,60p' "$W/diff.txt" >&2
echo "报告目录：$W" >&2
exit 1
