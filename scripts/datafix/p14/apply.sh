#!/usr/bin/env bash
# ============================================================================
# P14 · 应用 / 回滚 / 现状（每个 SQL 脚本一个事务；幂等；可逆）
# ----------------------------------------------------------------------------
# 用法：
#   bash scripts/datafix/p14/apply.sh              # 应用 10 → 20 → 30 自检（全绿才退出 0）
#   bash scripts/datafix/p14/apply.sh --rollback   # 倒序回滚 20 → 10（再跑一遍现状快照）
#   bash scripts/datafix/p14/apply.sh --check      # 只看现状，不写库
#
# 环境：
#   P14_DB=<db>           目标库（默认取 NIK_DATABASE_NAME，生产中 = cloudpivot）
#   FORCE=1               跳过「必须存在本库备份」的保护（演练库里也建议先跑 00_backup.sh）
#   CLOUDPIVOT_ENV_FILE   默认 /etc/cloudpivot/cloudpivot.env
#
# 安全：
#   * 应用前必须能在 /root/datafix-backup-p14-* 里找到**本库**的 full 备份
#     （校验 MANIFEST.txt 里的 `database  : <db> on `，避免拿别的库的备份当挡箭牌）
#   * 所有前置校验写在 SQL 里（行数/qty/amount 不符就 RAISE EXCEPTION 整脚本回滚）
#   * 30_verify.sql 出现任何 FAIL → 退出码 1
# ============================================================================
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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
DB_NAME="${P14_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -v ON_ERROR_STOP=1)

APPLY_ORDER=(10_void_serials 20_reverse_inventory)
ROLLBACK_ORDER=(20_reverse_inventory 10_void_serials)

snapshot() {
    echo "---- 现状快照（$(date '+%H:%M:%S')）----"
    "${PSQL[@]}" -At -F'|' <<'SQL'
select '序列号存活(P14 判据)  = ' || count(*)
  from biz_material_serial s
 where s.deleted_at is null and s.source_type='PRODUCE' and s.source_id is null
   and s.create_by=(select id from sys_user where username='henry.liao')
   and s.created_at >= timestamptz '2026-10-31 16:17:00+08'
   and s.created_at <  timestamptz '2026-10-31 16:18:00+08';
select '成本账 qty/amount/avg_cost = ' || coalesce(max(qty),0)::text || ' / ' || coalesce(max(amount),0)::text
       || ' / ' || coalesce(max(avg_cost),0)::text || '（行数 ' || count(*) || '）'
  from biz_inventory i
  join biz_material m  on m.id=i.material_id  and m.material_code='M-W1A-004'
  join biz_warehouse w on w.id=i.warehouse_id and w.warehouse_code='WH01';
select '库位账 qty 合计       = ' || coalesce(sum(l.qty),0)::text
  from biz_inventory_location l
  join biz_material m  on m.id=l.material_id  and m.material_code='M-W1A-004'
  join biz_warehouse w on w.id=l.warehouse_id and w.warehouse_code='WH01';
select '流水累计(该组)        = ' || coalesce(sum(case when txn_type='IN' then qty else -qty end),0)::text
  from biz_inventory_txn t
  join biz_material m on m.id=t.material_id and m.material_code='M-W1A-004';
select 'P14 冲销流水          = ' || count(*) || ' 条'
  from biz_inventory_txn where doc_no='DATAFIX-20261101-P14';
select '全库 inv_qty/loc_qty  = ' || (select coalesce(sum(qty),0)::numeric(18,4) from biz_inventory)::text
       || ' / ' || (select coalesce(sum(qty),0)::numeric(18,4) from biz_inventory_location)::text;
select '全库 inv_amount/1405  = ' || (select coalesce(sum(amount),0)::numeric(18,4) from biz_inventory)::text
       || ' / ' || (select coalesce(sum(coalesce(e.debit_amount,0)-coalesce(e.credit_amount,0)),0)::numeric(18,4)
                      from biz_voucher_entry e
                      join biz_voucher v on v.id=e.voucher_id and v.deleted_at is null
                      join biz_account a on a.id=e.account_id
                     where a.account_code like '1405%')::text;
SQL
}

verify() {
    local out rc
    out="$(mktemp)"
    echo "---- 30_verify.sql 自检 ----"
    "${PSQL[@]}" -At -F'|' -f "$HERE/30_verify.sql" | tee "$out"
    if grep -q '^FAIL' "$out"; then
        echo "✗ 自检有 FAIL —— 不许当成功交付" >&2
        rm -f "$out"
        return 1
    fi
    rm -f "$out"
    echo "✓ 自检全绿"
    return 0
}

mode="apply"
case "${1:-}" in
    --rollback) mode="rollback" ;;
    --check)    mode="check" ;;
    "") ;;
    *) echo "未知参数：$1（可用 --rollback / --check）" >&2; exit 2 ;;
esac

echo "== P14 目标库：$DB_NAME @ $DB_HOST:$DB_PORT（$mode）=="
snapshot

if [ "$mode" = "check" ]; then
    verify || true
    exit 0
fi

if [ "$mode" = "apply" ]; then
    if [ "${FORCE:-0}" != "1" ]; then
        latest=""
        while IFS= read -r dir; do
            [ -s "$dir/target_tables.dump" ] || continue
            if grep -q "database  : $DB_NAME on " "$dir/MANIFEST.txt" 2>/dev/null; then
                latest="$dir"; break
            fi
        done < <(ls -1dt /root/datafix-backup-p14-* 2>/dev/null || true)
        if [ -z "$latest" ]; then
            echo "✗ 找不到本库（$DB_NAME）的备份 MANIFEST：先跑 00_backup.sh（或用 FORCE=1 跳过）" >&2
            exit 1
        fi
        echo "==> 使用备份：$latest"
    fi
    for name in "${APPLY_ORDER[@]}"; do
        echo "==> 应用 $name.up.sql"
        "${PSQL[@]}" -q -1 -f "$HERE/$name.up.sql"
    done
else
    for name in "${ROLLBACK_ORDER[@]}"; do
        echo "==> 回滚 $name.down.sql"
        "${PSQL[@]}" -q -1 -f "$HERE/$name.down.sql"
    done
fi

snapshot
if [ "$mode" = "apply" ]; then
    verify
fi
echo "== 完成（$mode）=="
