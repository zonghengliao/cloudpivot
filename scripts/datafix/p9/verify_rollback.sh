#!/usr/bin/env bash
# P9 数据修复 · 回滚往返演练（每个 up 都配套 down，且 down 之后三账回到修复前）
#
# 用法：P9_DB=<演练库> bash scripts/datafix/p9/verify_rollback.sh
# 前提：该库已经 apply 过（否则 down 是空操作，验不出东西）。
set -uo pipefail
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
DB_NAME="${P9_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

snap() {
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At <<'SQL'
SELECT '库存金额=' || coalesce(sum(amount),0)::text
     || ' 库存数量=' || coalesce(sum(qty),0)::text
     || ' 库位账=' || (SELECT coalesce(sum(qty),0)::text FROM biz_inventory_location)
     || ' 1405=' || (SELECT coalesce(sum(coalesce(debit_amount,0)-coalesce(credit_amount,0)),0)::text
                       FROM biz_voucher_entry e
                       JOIN biz_voucher v ON v.id=e.voucher_id AND v.deleted_at IS NULL
                       JOIN biz_account a ON a.id=e.account_id
                      WHERE a.account_code LIKE '1405%')
     || ' 凭证=' || (SELECT count(*)::text FROM biz_voucher WHERE deleted_at IS NULL)
     || ' 冲销流水=' || (SELECT count(*)::text FROM biz_inventory_txn WHERE biz_type='PURCHASE_IN_CANCEL')
     || ' PENDING调拨=' || (SELECT count(*)::text FROM biz_stock_transfer WHERE status='PENDING' AND deleted_at IS NULL)
  FROM biz_inventory;
SQL
}

echo "== 回滚前：$(snap)"
echo "== 执行回滚（倒序）=="
P9_DB="$DB_NAME" bash "$HERE/apply.sh" --rollback 2>&1 | grep -E "回滚|NOTICE|ERROR|完成" | head -20
echo "== 回滚后：$(snap)"
echo "== 再应用一遍（证明 down → up 可往返）=="
FORCE=1 P9_DB="$DB_NAME" bash "$HERE/apply.sh" 2>&1 | grep -E "应用|ERROR|完成" | head -12
echo "== 重新应用后：$(snap)"
