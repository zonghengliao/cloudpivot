#!/usr/bin/env bash
# P9 数据修复 · 应用（幂等；每个脚本一个事务）
#
# 用法：
#   bash scripts/datafix/p9/apply.sh              # 按 10 → 20 → 30 → 40 → 50 顺序应用
#   bash scripts/datafix/p9/apply.sh --rollback   # 倒序回滚（50 → 40 → 30 → 20 → 10）
#   bash scripts/datafix/p9/apply.sh --check      # 只看当前状态（不写库）
#
# 前置：00_backup.sh 已经跑过（脚本会检查最近一份备份是否存在，否则拒绝继续，除非 FORCE=1）
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
if [ -f "$ENV_FILE" ]; then
    # shellcheck disable=SC1090
    set -a; . "$ENV_FILE"; set +a
fi
: "${NIK_DATABASE_PASSWORD:?需要 NIK_DATABASE_PASSWORD（先 . /etc/cloudpivot/cloudpivot.env）}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_NAME="${P9_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"   # P9_DB 优先：演练库 / 隔离库用

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

ORDER=(10_b1_location_ledger 20_d1_missing_voucher 30_e4_cancel_rollback 40_g2_transfer_stuck 50_b2_fixture_value)

snapshot() {
    echo "---- 三账快照（$(date '+%H:%M:%S')）----"
    "${PSQL[@]}" -At <<'SQL'
SELECT '库存金额合计      = ' || coalesce(sum(amount),0)::text FROM biz_inventory;
SELECT '库存数量合计      = ' || coalesce(sum(qty),0)::text FROM biz_inventory;
SELECT '库位账数量合计    = ' || coalesce(sum(qty),0)::text FROM biz_inventory_location;
SELECT '1405 科目净额     = ' || coalesce(sum(coalesce(debit_amount,0) - coalesce(credit_amount,0)),0)::text
  FROM biz_voucher_entry e
  JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL
  JOIN biz_account a ON a.id = e.account_id
 WHERE a.account_code LIKE '1405%';
SELECT '凭证张数          = ' || count(*)::text FROM biz_voucher WHERE deleted_at IS NULL;
SELECT '作废收货单冲销流水 = ' || count(*)::text FROM biz_inventory_txn WHERE biz_type = 'PURCHASE_IN_CANCEL';
SQL
}

mode="apply"
case "${1:-}" in
    --rollback) mode="rollback" ;;
    --check)    mode="check" ;;
    "")        ;;
    *) echo "未知参数：$1（可用 --rollback / --check）" >&2; exit 2 ;;
esac

echo "== 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（$mode）=="
snapshot

if [ "$mode" = "check" ]; then
    exit 0
fi

if [ "$mode" = "apply" ]; then
    if [ "${FORCE:-0}" != "1" ]; then
        latest=$(ls -1d /root/datafix-backup-* 2>/dev/null | sort | tail -1 || true)
        if [ -z "$latest" ] || [ ! -s "$latest/full.dump" ]; then
            echo "✗ 找不到 /root/datafix-backup-*/full.dump：先跑 00_backup.sh（或用 FORCE=1 跳过）" >&2
            exit 1
        fi
        echo "==> 使用备份：$latest"
    fi
    for name in "${ORDER[@]}"; do
        f="$HERE/$name.up.sql"
        echo "==> 应用 $name"
        "${PSQL[@]}" -q -1 -f "$f"
    done
else
    for ((i=${#ORDER[@]}-1; i>=0; i--)); do
        name="${ORDER[$i]}"
        f="$HERE/$name.down.sql"
        echo "==> 回滚 $name"
        "${PSQL[@]}" -q -1 -f "$f"
    done
fi

snapshot
echo "== 完成（$mode）=="
