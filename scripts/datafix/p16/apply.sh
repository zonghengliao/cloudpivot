#!/usr/bin/env bash
# ============================================================================
# P16 · E2E-FIX-7 全库既有差异清账（145 dev 库的夹具/演练残留）
#
# 用法（在目标库所在主机、仓库根目录执行）：
#   bash scripts/datafix/p16/apply.sh --check     # 只读：差异清单 + 目标清单
#   bash scripts/datafix/p16/apply.sh --backup    # 备份 8 张表 + 「将被删的每一行」JSON 快照
#   bash scripts/datafix/p16/apply.sh --apply     # 真写库：20 清理 + 30 补流水 + 40 验收
#   bash scripts/datafix/p16/apply.sh --verify    # 只跑 40 验收门
#   bash scripts/datafix/p16/apply.sh --drop-helper   # 删掉 datafix_p16_target 表与视图（收尾可选）
#
# 连接：优先用 NIK_DATABASE_*（与 scripts/verify 同口径），否则 DB_HOST/DB_PORT/DB_USER/DB_PASS + P16_DB。
#   145 上标准用法：
#     set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
#     P16_DB=$NIK_DATABASE_NAME bash scripts/datafix/p16/apply.sh --check
#
# 为什么要有这个脚本：
#   E2E-03 等演练在 cloudpivot_dev 里留下大量**只写了一半**的夹具（成本账/库位账/流水/序列号
#   四张表缺一半），把「库存 vs 流水 11 行 / 逐桶 10 行 / 一物一码 1 条」的全量巡检噪点
#   永久留在基线里，真问题被淹没。代码侧根因（E2E-FIX-6）已修，这里只清存量。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DIR="$ROOT/scripts/datafix/p16"

DB_HOST="${NIK_DATABASE_HOST:-${DB_HOST:-127.0.0.1}}"
DB_PORT="${NIK_DATABASE_PORT:-${DB_PORT:-5432}}"
DB_USER="${NIK_DATABASE_USER:-${DB_USER:-alun}}"
DB_PASS="${NIK_DATABASE_PASSWORD:-${DB_PASS:-alun}}"
P16_DB="${P16_DB:-${NIK_DATABASE_NAME:-}}"

if [ -z "$P16_DB" ]; then
  echo "FATAL: 未指定目标库：给 P16_DB=<库名> 或 NIK_DATABASE_NAME=<库名>" >&2
  exit 1
fi

export PGPASSWORD="$DB_PASS"
psql_run() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$P16_DB" -v ON_ERROR_STOP=1 "$@"; }

case "${1:---check}" in
  --check)
    echo "== P16 只读盘点（库 $P16_DB）=="
    psql_run -f "$DIR/10_report.up.sql"
    ;;

  --backup)
    STAMP="$(date +%Y%m%d-%H%M%S)"
    OUT="${P16_BACKUP_DIR:-$ROOT/evidence/p16-$STAMP}"
    mkdir -p "$OUT"
    echo "== 备份到 $OUT =="
    # ① 结构+数据整表 dump（8 张表：4 张库存账 + 4 张单据头 + 单据明细）
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$P16_DB" \
      --data-only --column-inserts \
      -t biz_inventory -t biz_inventory_location -t biz_inventory_txn -t biz_material_serial \
      -t biz_stock_in -t biz_stock_out -t biz_stock_take -t biz_stock_transfer \
      -t biz_stock_in_item -t biz_stock_out_item -t biz_stock_take_item -t biz_stock_transfer_item \
      > "$OUT/p16_tables.sql" || { echo "FATAL: pg_dump 失败" >&2; exit 1; }
    ls -l "$OUT/p16_tables.sql"
    # ② 逐行快照（将被删的每一行，jsonb）；需要先建目标表/视图
    psql_run -q -f "$DIR/05_target.up.sql" >/dev/null 2>&1 || true
    psql_run -c "\\copy (SELECT tbl, row FROM datafix_p16_doomed_rows ORDER BY 1) TO '$OUT/doomed_rows.tsv' WITH (FORMAT csv, DELIMITER E'\\t')" \
      && ls -l "$OUT/doomed_rows.tsv" || echo "WARN: 逐行快照导出失败（整表 dump 仍在）"
    echo "恢复：psql -d $P16_DB -f $OUT/p16_tables.sql（按外键顺序：先删库里现有行再灌）"
    ;;

  --apply)
    echo "== P16 步骤 1/3：建/刷新目标清单 =="
    psql_run -q -f "$DIR/05_target.up.sql" || { echo "FATAL: 目标清单失败" >&2; exit 1; }
    echo
    echo "== P16 步骤 2/3：清理夹具残留（G1/G2/G3 三闸不过即整单回滚）=="
    psql_run -f "$DIR/20_purge_fixture_residue.up.sql" || { echo "FATAL: 清理失败（已回滚）" >&2; exit 1; }
    echo
    echo "== P16 步骤 3/3：补录差额流水（class C）+ 验收 =="
    psql_run -f "$DIR/30_restore_missing_txn.up.sql" || { echo "FATAL: 补流水失败（已回滚）" >&2; exit 1; }
    echo
    psql_run -f "$DIR/40_verify.up.sql"
    ;;

  --verify)
    psql_run -f "$DIR/40_verify.up.sql"
    ;;

  --drop-helper)
    psql_run -c "DROP VIEW IF EXISTS datafix_p16_doomed_rows; DROP TABLE IF EXISTS datafix_p16_target;"
    echo "已删除 datafix_p16_target / datafix_p16_doomed_rows（清理台账也随之删除；需要留痕就别跑这一步）"
    ;;

  *)
    echo "用法: bash $0 --check|--backup|--apply|--verify|--drop-helper" >&2
    exit 2
    ;;
esac
