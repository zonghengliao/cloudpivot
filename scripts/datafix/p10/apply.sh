#!/usr/bin/env bash
# P10 数据修复 · 应用（幂等；每个脚本一个事务）
#
# 用法：
#   bash scripts/datafix/p10/apply.sh              # 应用 10
#   bash scripts/datafix/p10/apply.sh --rollback   # 倒序回滚
#   bash scripts/datafix/p10/apply.sh --check      # 只看现状（不写库）
#
# 前置：00_backup.sh 已经跑过（找不到最近一份备份就拒绝继续，除非 FORCE=1）
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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
DB_NAME="${P10_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"   # P10_DB 优先：演练库 / 隔离库用

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

ORDER=(10_m_w1a_004_fixture_residue)

snapshot() {
    echo "---- 三账快照（$(date '+%H:%M:%S')）----"
    "${PSQL[@]}" -At <<'SQL'
SELECT '库存金额合计        = ' || coalesce(sum(amount),0)::text FROM biz_inventory;
SELECT '库存数量合计        = ' || coalesce(sum(qty),0)::text FROM biz_inventory;
SELECT '库位账数量合计      = ' || coalesce(sum(qty),0)::text FROM biz_inventory_location;
SELECT '1405 科目净额       = ' || coalesce(sum(coalesce(debit_amount,0) - coalesce(credit_amount,0)),0)::text
  FROM biz_voucher_entry e
  JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL
  JOIN biz_account a ON a.id = e.account_id
 WHERE a.account_code LIKE '1405%';
SELECT 'M-W1A-004 库存行数   = ' || count(*)::text
  FROM biz_inventory i
  JOIN biz_material m ON m.id = i.material_id AND m.material_code = 'M-W1A-004';
SELECT 'M-W1A-004 在库号数   = ' || count(*)::text
  FROM biz_material_serial s
  JOIN biz_material m ON m.id = s.material_id AND m.material_code = 'M-W1A-004'
 WHERE s.deleted_at IS NULL AND s.status = 'IN_STOCK';
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
        # 取**最新**的一份（按 mtime，不按名字：演练库用的 `drill` 标签会排在日期后面），
        # 并且要求 MANIFEST 里的库名与本次目标一致 —— 拿别人的库备份当挡箭牌是没有意义的。
        latest=""
        while IFS= read -r dir; do
            [ -s "$dir/full.dump" ] || continue
            if grep -q "database  : $DB_NAME on " "$dir/MANIFEST.txt" 2>/dev/null; then
                latest="$dir"
                break
            fi
        done < <(ls -1dt /root/datafix-backup-p10-* 2>/dev/null || true)
        if [ -z "$latest" ]; then
            echo "✗ 找不到本库（$DB_NAME）的备份 full.dump：先跑 00_backup.sh（或用 FORCE=1 跳过）" >&2
            exit 1
        fi
        echo "==> 使用备份：$latest"
    fi
    for name in "${ORDER[@]}"; do
        echo "==> 应用 $name"
        "${PSQL[@]}" -q -1 -f "$HERE/$name.up.sql"
    done
else
    for ((i=${#ORDER[@]}-1; i>=0; i--)); do
        name="${ORDER[$i]}"
        echo "==> 回滚 $name"
        "${PSQL[@]}" -q -1 -f "$HERE/$name.down.sql"
    done
fi

snapshot
echo "== 完成（$mode）=="
