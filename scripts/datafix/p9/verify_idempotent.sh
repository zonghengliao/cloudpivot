#!/usr/bin/env bash
# P9 数据修复 · 幂等证明（卡面 §五 ⑫ / 硬红线 4）
#
# 做法：连跑两遍 apply.sh，**第二遍之后**受影响表的数据段必须与第一遍之后逐字节一致。
#   如果第二遍又改了行，说明脚本里有「无条件 UPDATE」或「取号未判重」，
#   这在生产上意味着「多跑一次就多扣一次库存」。
#
# 用法：bash scripts/datafix/p9/verify_idempotent.sh
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
DB_NAME="${P9_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"   # P9_DB 优先：演练库 / 隔离库用

TABLES=(biz_inventory biz_inventory_location biz_inventory_txn biz_voucher biz_voucher_entry
        biz_payable biz_stock_in biz_stock_transfer biz_seq)

dump_all() {  # $1 = 输出目录
    mkdir -p "$1"
    # 过滤掉 pg_dump 18 的随机 `\restrict <token>` / `\unrestrict <token>` 行：
    # 那是每次 dump 都不同的会话安全令牌，不是数据（不过滤会永远 diff 成功 ≠ 数据变了）。
    for t in "${TABLES[@]}"; do
        pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
                --data-only --table="public.$t" \
            | grep -vE '^\\(un)?restrict ' > "$1/$t.sql"
    done
}

W="$(mktemp -d /tmp/p9-idem.XXXXXX)"

echo "== 第 1 遍 =="
FORCE=1 bash "$HERE/apply.sh" > "$W/run1.log" 2>&1
dump_all "$W/after1"

echo "== 第 2 遍（同库再跑一次）=="
FORCE=1 bash "$HERE/apply.sh" > "$W/run2.log" 2>&1
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
