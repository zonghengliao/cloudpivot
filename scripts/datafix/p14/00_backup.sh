#!/usr/bin/env bash
# ============================================================================
# P14 · 00 备份（清 prod 之前的第一件事，不许省）
# ----------------------------------------------------------------------------
# 产出目录：/root/datafix-backup-p14-<YYYYmmdd-HHMMSS>（也可用 P14_BACKUP_DIR 指定）
#   <表>.sql        逐表 pg_dump（含 CREATE TABLE，可单独回放到空库）
#   <表>.rows       行数（psql count(*)）
#   target_tables.dump  6 张表合成一份 custom-format（pg_restore 用）
#   MANIFEST.txt    表名 / 行数 / md5 / 库名 / 迁移版本 / 目标范围快照 / 全库对账基线
#
# 用法：
#   bash scripts/datafix/p14/00_backup.sh                # 自动时间戳
#   bash scripts/datafix/p14/00_backup.sh drill          # 演练库用固定标签
#   P14_DB=cloudpivot_p14drill bash .../00_backup.sh drill
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
DB_NAME="${P14_DB:-${NIK_DATABASE_NAME:-${ALUN_DATABASE_NAME:-cloudpivot}}}"

TABLES=(biz_material_serial biz_inventory biz_inventory_txn biz_inventory_location biz_voucher biz_voucher_entry)

TAG="${1:-$(date +%Y%m%d-%H%M%S)}"
DIR="${P14_BACKUP_DIR:-/root/datafix-backup-p14-$TAG}"
mkdir -p "$DIR"

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -At -v ON_ERROR_STOP=1)

echo "== P14 备份：$DB_NAME @ $DB_HOST:$DB_PORT → $DIR =="

# ---- 1) 逐表 pg_dump（纯 SQL，便于人工阅读 + 回放） ----
for t in "${TABLES[@]}"; do
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
            --no-owner --no-privileges --table "public.$t" > "$DIR/$t.sql"
    "${PSQL[@]}" -c "select count(*) from public.$t" > "$DIR/$t.rows"
done

# ---- 2) 合成一份 custom-format（pg_restore 恢复用） ----
PGS=()
for t in "${TABLES[@]}"; do PGS+=(-t "public.$t"); done
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
        --no-owner --no-privileges -Fc "${PGS[@]}" > "$DIR/target_tables.dump"

# ---- 3) 目标范围快照（清之前这一刻的真相，人工核对用） ----
q() { "${PSQL[@]}" -F'|' -c "$1"; }

q "select '## 序列号（PRODUCE/无来源/henry.liao/16:17 窗口）',
          s.serial_no, s.status, s.warehouse_id, coalesce(s.location_id::text,'(null)'),
          s.created_at, coalesce(s.deleted_at::text,'(null)')
     from biz_material_serial s
    where s.source_type='PRODUCE' and s.source_id is null
      and s.create_by=(select id from sys_user where username='henry.liao')
      and s.created_at >= timestamptz '2026-10-31 16:17:00+08'
      and s.created_at <  timestamptz '2026-10-31 16:18:00+08'
    order by s.serial_no" > "$DIR/scope_serials.txt"

q "select '## 成本账 M-W1A-004/WH01', i.qty, i.amount, i.avg_cost, i.last_txn_at
     from biz_inventory i
     join biz_material m  on m.id=i.material_id  and m.material_code='M-W1A-004'
     join biz_warehouse w on w.id=i.warehouse_id and w.warehouse_code='WH01'" > "$DIR/scope_inventory.txt"

q "select '## 该物料全部流水', t.doc_no, t.txn_type, t.biz_type, t.doc_type, t.qty, t.unit_cost, t.amount,
          t.before_qty, t.after_qty, t.created_at
     from biz_inventory_txn t
     join biz_material m on m.id=t.material_id and m.material_code='M-W1A-004'
    order by t.created_at" > "$DIR/scope_txn.txt"

q "select '## 库位账 M-W1A-004/WH01', count(*)
     from biz_inventory_location l
     join biz_material m  on m.id=l.material_id  and m.material_code='M-W1A-004'
     join biz_warehouse w on w.id=l.warehouse_id and w.warehouse_code='WH01'" > "$DIR/scope_location.txt"

q "select '## 凭证（source_no 含 SN-20261031-000011）', count(*)
     from biz_voucher where source_no like '%SN-20261031-000011%'" > "$DIR/scope_voucher.txt"

# ---- 4) 全库对账基线（修前 / 修后对比用） ----
q "select 'inv_rows',      count(*)                                  from biz_inventory
   union all select 'inv_qty',    coalesce(sum(qty),0)::numeric(18,4)      from biz_inventory
   union all select 'inv_amount', coalesce(sum(amount),0)::numeric(18,4)   from biz_inventory
   union all select 'loc_qty',    coalesce(sum(qty),0)::numeric(18,4)      from biz_inventory_location
   union all select 'txn_rows',   count(*)                                 from biz_inventory_txn
   union all select 'voucher_rows', count(*) from biz_voucher where deleted_at is null
   union all select 'serial_alive', count(*) from biz_material_serial where deleted_at is null
   union all select 'gl_1405_net',
             coalesce(sum(coalesce(e.debit_amount,0)-coalesce(e.credit_amount,0)),0)::numeric(18,4)
        from biz_voucher_entry e
        join biz_voucher v on v.id=e.voucher_id and v.deleted_at is null
        join biz_account a on a.id=e.account_id
       where a.account_code like '1405%'" > "$DIR/baseline.txt"

MIG_MAX=$("${PSQL[@]}" -c "select coalesce(max(version::int),0) from schema_migrations")
GIT_HEAD="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." 2>/dev/null && git rev-parse --short HEAD 2>/dev/null || echo '(n/a)')"

# ---- 5) MANIFEST ----
{
    echo "P14 数据修复备份清单（清理 prod 零成本测试数据）"
    echo "generated_at  : $(date '+%Y-%m-%d %H:%M:%S %Z')"
    echo "host          : $(hostname)"
    echo "database  : $DB_NAME on $DB_HOST:$DB_PORT"
    echo "db_user       : $DB_USER"
    echo "migration_max : $MIG_MAX"
    echo "git_head      : $GIT_HEAD"
    echo "backup_dir    : $DIR"
    echo
    printf '%-28s %10s %12s  %s\n' "table" "rows" "bytes" "md5(sql)"
    printf '%-28s %10s %12s  %s\n' "----------------------------" "----------" "------------" "--------------------------------"
    for t in "${TABLES[@]}"; do
        printf '%-28s %10s %12s  %s\n' "$t" "$(cat "$DIR/$t.rows")" "$(stat -c %s "$DIR/$t.sql")" "$(md5sum "$DIR/$t.sql" | cut -d' ' -f1)"
    done
    printf '%-28s %10s %12s  %s\n' "target_tables.dump" "-" "$(stat -c %s "$DIR/target_tables.dump")" "$(md5sum "$DIR/target_tables.dump" | cut -d' ' -f1)"
    echo
    echo "================ 目标范围快照 ================"
    cat "$DIR/scope_serials.txt" "$DIR/scope_inventory.txt" "$DIR/scope_txn.txt" "$DIR/scope_location.txt" "$DIR/scope_voucher.txt"
    echo
    echo "================ 全库对账基线 ================"
    cat "$DIR/baseline.txt"
} > "$DIR/MANIFEST.txt"

echo "== 备份完成 → $DIR =="
echo "---- MANIFEST.txt ----"
cat "$DIR/MANIFEST.txt"
