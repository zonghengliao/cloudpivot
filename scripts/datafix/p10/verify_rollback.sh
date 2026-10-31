#!/usr/bin/env bash
# P10 数据修复 · 回滚往返演练
#
# 用法：P10_DB=<演练库> bash scripts/datafix/p10/verify_rollback.sh
# 前提：该库已经 apply 过（否则 down 是空操作，验不出东西）。
# 判据：回滚后 M-W1A-004 的 4 张表行数 / 在库号数 / 库存三值必须回到修复前的快照；
#       再 up 一遍必须回到「全清」—— 两个方向都逐字段对上才算往返成立。
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
DB_NAME="${P10_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

snap() {
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At <<'SQL'
SELECT '库存金额=' || coalesce(sum(i.amount),0)::text
     || ' 库存数量=' || coalesce(sum(i.qty),0)::text
     || ' M-W1A-004 库存行=' || count(*)::text
     || ' 库位账=' || (SELECT coalesce(sum(qty),0)::text FROM biz_inventory_location l
                        JOIN biz_material m ON m.id = l.material_id AND m.material_code = 'M-W1A-004')
     || ' 流水=' || (SELECT count(*)::text FROM biz_inventory_txn t
                      JOIN biz_material m ON m.id = t.material_id AND m.material_code = 'M-W1A-004')
     || ' 存活号=' || (SELECT count(*)::text FROM biz_material_serial s
                        JOIN biz_material m ON m.id = s.material_id AND m.material_code = 'M-W1A-004'
                       WHERE s.deleted_at IS NULL)
     || ' 在库号=' || (SELECT count(*)::text FROM biz_material_serial s
                        JOIN biz_material m ON m.id = s.material_id AND m.material_code = 'M-W1A-004'
                       WHERE s.deleted_at IS NULL AND s.status = 'IN_STOCK')
     || ' 1405=' || (SELECT coalesce(sum(coalesce(debit_amount,0)-coalesce(credit_amount,0)),0)::text
                       FROM biz_voucher_entry e
                       JOIN biz_voucher v ON v.id=e.voucher_id AND v.deleted_at IS NULL
                       JOIN biz_account a ON a.id=e.account_id
                      WHERE a.account_code LIKE '1405%')
  FROM biz_inventory i
  JOIN biz_material m ON m.id = i.material_id AND m.material_code = 'M-W1A-004';
SQL
}

echo "== 回滚前：$(snap)"
echo "== 执行回滚（倒序）=="
P10_DB="$DB_NAME" bash "$HERE/apply.sh" --rollback 2>&1 | grep -E "回滚|NOTICE|ERROR|完成" | head -20
echo "== 回滚后：$(snap)"
echo "== 再应用一遍（证明 down → up 可往返）=="
FORCE=1 P10_DB="$DB_NAME" bash "$HERE/apply.sh" 2>&1 | grep -E "应用|NOTICE|ERROR|完成" | head -12
echo "== 重新应用后：$(snap)"
