#!/usr/bin/env bash
# ============================================================================
# P17 · E2E-08 夹具清理漏删的存量残留清账（t_45bda7ad）
#
# 用法（在目标库所在主机、仓库根目录执行）：
#   bash scripts/datafix/p17/apply.sh --check     # 只读：孤儿库位账 / 孤儿批次凭证清单
#   bash scripts/datafix/p17/apply.sh --backup    # 备份 4 张表 + 将被删的每一行 JSON 快照
#   bash scripts/datafix/p17/apply.sh --apply     # 真写库：20 清理（三道门禁）+ 30 验收
#   bash scripts/datafix/p17/apply.sh --verify    # 只跑 30 验收门
#
# 连接：优先 NIK_DATABASE_*（与 scripts/verify 同口径），否则 DB_HOST/DB_PORT/DB_USER/DB_PASS + P17_DB。
#   145 上标准用法：
#     set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
#     P17_DB=$NIK_DATABASE_NAME bash scripts/datafix/p17/apply.sh --check
#
# 清什么（形态判据，不写死编码/凭证号）：
#   ① biz_inventory_location：material_id 或 warehouse_id 在 biz_material / biz_warehouse 里不存在
#   ② biz_voucher：source_type='SERIAL_BATCH' 且批次流水（txn.doc_id = voucher.source_id）已不存在
#      → 连带它的 biz_voucher_entry 与 biz_status_log(VOUCHER)
# 根因（同一提交已修）：scripts/verify/e2e08_master_common.sh::cleanup_own 删物料/仓库时不删库位账、
# 也不删 batch-generate 写的凭证 ⇒ 每跑一次 E2E-08 漏 1 行库位账 + 1 张凭证 + 1 条状态日志。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DIR="$ROOT/scripts/datafix/p17"

DB_HOST="${NIK_DATABASE_HOST:-${DB_HOST:-127.0.0.1}}"
DB_PORT="${NIK_DATABASE_PORT:-${DB_PORT:-5432}}"
DB_USER="${NIK_DATABASE_USER:-${DB_USER:-alun}}"
DB_PASS="${NIK_DATABASE_PASSWORD:-${DB_PASS:-alun}}"
P17_DB="${P17_DB:-${NIK_DATABASE_NAME:-}}"

if [ -z "$P17_DB" ]; then
  echo "FATAL: 未指定目标库：给 P17_DB=<库名> 或 NIK_DATABASE_NAME=<库名>" >&2
  exit 1
fi

export PGPASSWORD="$DB_PASS"
psql_run() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$P17_DB" -v ON_ERROR_STOP=1 "$@"; }

case "${1:---check}" in
  --check)
    echo "== P17 只读盘点（库 $P17_DB）=="
    psql_run -f "$DIR/10_report.up.sql"
    ;;

  --backup)
    STAMP="$(date +%Y%m%d-%H%M%S)"
    OUT="${P17_BACKUP_DIR:-$ROOT/evidence/p17-$STAMP}"
    mkdir -p "$OUT"
    echo "== 备份到 $OUT =="
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$P17_DB" \
      --data-only --column-inserts \
      -t biz_inventory_location -t biz_voucher -t biz_voucher_entry -t biz_status_log \
      > "$OUT/p17_tables.sql" || { echo "FATAL: pg_dump 失败" >&2; exit 1; }
    ls -l "$OUT/p17_tables.sql"
    # 逐行快照：把「将要删的每一行」先导成 jsonb（与 p16 同口径）
    psql_run -q <<'SQL' > "$OUT/doomed_rows.tsv" 2>/dev/null || true
\copy (WITH v AS (SELECT v.* FROM biz_voucher v WHERE v.source_type='SERIAL_BATCH' AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=v.source_id)), l AS (SELECT l.* FROM biz_inventory_location l WHERE NOT EXISTS (SELECT 1 FROM biz_material m WHERE m.id=l.material_id) OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id=l.warehouse_id)) SELECT 'biz_inventory_location' AS tbl, to_jsonb(x)::text AS row FROM l x UNION ALL SELECT 'biz_status_log', to_jsonb(s)::text FROM biz_status_log s WHERE s.entity_type='VOUCHER' AND s.entity_id IN (SELECT id FROM v) UNION ALL SELECT 'biz_voucher_entry', to_jsonb(e)::text FROM biz_voucher_entry e WHERE e.voucher_id IN (SELECT id FROM v) UNION ALL SELECT 'biz_voucher', to_jsonb(vv)::text FROM v vv) TO STDOUT WITH (FORMAT csv, DELIMITER E'\t')
SQL
    ls -l "$OUT/doomed_rows.tsv" 2>/dev/null || echo "WARN: 逐行快照失败（整表 dump 仍在）"
    echo "恢复：psql -d $P17_DB -f $OUT/p17_tables.sql"
    ;;

  --apply)
    echo "== P17 步骤 1/2：清理（G1/G2/G3 任一命中整单回滚）=="
    psql_run -f "$DIR/20_purge_orphan_batch.up.sql" || { echo "FATAL: 清理失败（已回滚）" >&2; exit 1; }
    echo
    echo "== P17 步骤 2/2：验收 =="
    psql_run -f "$DIR/30_verify.up.sql"
    ;;

  --verify)
    psql_run -f "$DIR/30_verify.up.sql"
    ;;

  *)
    echo "用法: $0 [--check|--backup|--apply|--verify]" >&2
    exit 2
    ;;
esac
