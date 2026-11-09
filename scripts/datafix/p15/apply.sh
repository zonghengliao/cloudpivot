#!/usr/bin/env bash
# ============================================================================
# P15 · 把「未分配桶」并进「默认库位桶」（E2E-FIX-6 ③ 存量口径）
#
# 用法（在目标库所在主机、仓库根目录执行）：
#   bash scripts/datafix/p15/apply.sh --check     # 只读：列出会被合并的 (物料, 仓库)
#   bash scripts/datafix/p15/apply.sh --backup    # 备份 3 张表 + 打印路径
#   bash scripts/datafix/p15/apply.sh --apply     # 真写库（事务内；守恒校验失败即回滚）
#
# 连接：默认走 127.0.0.1:5432、alun/alun、库 = NIK_DATABASE_NAME（与 scripts/verify 同口径）。
#   覆盖：DB_HOST / DB_PORT / DB_USER / DB_PASS / P15_DB
#
# 为什么要有这个脚本：
#   E2E-FIX-6 之前 `POST /serial/batch-generate` 不带库位 ⇒ 批量生成的库存落「未分配」桶，
#   而收货/期初落「默认库位桶」⇒ 同物料同仓两桶并存。代码已修（新数据不再分裂），
#   存量要么不管（测试数据），要么用本脚本并桶（真实数据）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DIR="$ROOT/scripts/datafix/p15"

DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-alun}"
DB_PASS="${DB_PASS:-alun}"
P15_DB="${P15_DB:-${NIK_DATABASE_NAME:-}}"

if [ -z "$P15_DB" ]; then
  echo "FATAL: 未指定目标库：给 P15_DB=<库名> 或 NIK_DATABASE_NAME=<库名>" >&2
  exit 1
fi

export PGPASSWORD="$DB_PASS"
psql_run() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$P15_DB" -v ON_ERROR_STOP=1 "$@"; }

case "${1:---check}" in
  --check)
    echo "== P15 只读报告（库 $P15_DB）=="
    psql_run -f "$DIR/10_report_two_bucket.up.sql"
    ;;
  --backup)
    STAMP="$(date +%Y%m%d-%H%M%S)"
    OUT="${P15_BACKUP_DIR:-$ROOT/evidence/p15-$STAMP}"
    mkdir -p "$OUT"
    echo "== 备份到 $OUT =="
    pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$P15_DB" \
      -t biz_inventory -t biz_inventory_location -t biz_material_serial \
      --data-only --column-inserts > "$OUT/p15_tables.sql" || { echo "FATAL: 备份失败" >&2; exit 1; }
    ls -l "$OUT/p15_tables.sql"
    echo "恢复：psql -d $P15_DB -f $OUT/p15_tables.sql（先 TRUNCATE 三张表，注意外键顺序）"
    ;;
  --apply)
    echo "== P15 应用（库 $P15_DB）：合并前快照 =="
    psql_run -f "$DIR/10_report_two_bucket.up.sql"
    echo "== 执行合并 =="
    psql_run -f "$DIR/20_merge_unassigned_into_default.up.sql"
    RC=$?
    echo "== 合并后复查（应当 0 行 = 没有未分配桶残留）=="
    psql_run -f "$DIR/10_report_two_bucket.up.sql"
    exit "$RC"
    ;;
  *)
    echo "用法: bash $0 --check|--backup|--apply" >&2
    exit 2
    ;;
esac
