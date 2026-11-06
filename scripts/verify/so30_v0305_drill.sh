#!/usr/bin/env bash
# =============================================================================
# SO-30：V0305 迁移演练（down 守卫 / down / up / 二次 up 幂等）
# =============================================================================
#   bash scripts/verify/so30_v0305_drill.sh
#
# 四段：
#   ① 有退货入库单时 down **必须拒绝**（不许静默丢业务事实）
#   ② 清掉退货入库单后 down 成功（索引 / 两列 / 发票来源白名单回退）
#   ③ up 重新落结构
#   ④ 二次 up 幂等（ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS）
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
DB=${SO30_DB:-cloudpivot_so30}
UP="$ROOT/backend/migrations/business/V0305__sales_return_posting.up.sql"
DOWN="$ROOT/backend/migrations/business/V0305__sales_return_posting.down.sql"
export PGPASSWORD=alun
PSQL() { psql -h 127.0.0.1 -U alun -d "$DB" "$@"; }

echo "== ① 有退货入库单时 down 必须拒绝 =="
cnt=$(PSQL -tAc "select count(*) from biz_stock_in where source_type='SALES_RETURN'")
echo "  当前退货入库单：$cnt 张"
if [ "$cnt" -gt 0 ]; then
  out=$(PSQL -v ON_ERROR_STOP=1 -f "$DOWN" 2>&1)
  case "$out" in
    *"down 拒绝执行"*) echo "  ✅ down 被守卫拒绝（文案点名业务事实）" ;;
    *) echo "  ❌ down 未被拒绝：$out"; exit 1 ;;
  esac
else
  echo "  ⚠ 库里没有退货入库单，跳过守卫段（先跑一次 e2e 才能验到）"
fi

echo "== ② 清掉退货入库单 / 红字发票后 down 成功 =="
PSQL -q -c "DELETE FROM biz_voucher_entry WHERE voucher_id IN
                (SELECT voucher_id FROM biz_invoice WHERE source_type='SALES_RETURN'
                 UNION SELECT voucher_id FROM biz_stock_in WHERE source_type='SALES_RETURN');
            DELETE FROM biz_voucher WHERE id IN
                (SELECT voucher_id FROM biz_invoice WHERE source_type='SALES_RETURN')
               OR summary LIKE '销售退货 %';
            DELETE FROM biz_invoice WHERE source_type='SALES_RETURN';
            DELETE FROM biz_inventory_txn WHERE biz_type='SALES_RETURN_IN';
            DELETE FROM biz_stock_in_item WHERE stock_in_id IN
                (SELECT id FROM biz_stock_in WHERE source_type='SALES_RETURN');
            DELETE FROM biz_stock_in WHERE source_type='SALES_RETURN';" || exit 1
PSQL -v ON_ERROR_STOP=1 -q -f "$DOWN" || { echo "  ❌ down 失败"; exit 1; }
cols=$(PSQL -tAc "select count(*) from information_schema.columns where table_name='biz_stock_in' and column_name in ('source_type','source_id')")
echo "  down 后 biz_stock_in 来源列数=$cols（期望 0）"
[ "$cols" = "0" ] || { echo "  ❌ down 没把列删掉"; exit 1; }
inv=$(PSQL -tAc "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_invoice_source'")
case "$inv" in
  *SALES_RETURN*) echo "  ❌ 发票来源白名单没回退：$inv"; exit 1 ;;
  *) echo "  ✅ 发票来源白名单已回退（不含 SALES_RETURN）" ;;
esac

echo "== ③ up 重新落结构 =="
PSQL -v ON_ERROR_STOP=1 -q -f "$UP" || { echo "  ❌ up 失败"; exit 1; }
cols=$(PSQL -tAc "select count(*) from information_schema.columns where table_name='biz_stock_in' and column_name in ('source_type','source_id')")
[ "$cols" = "2" ] && echo "  ✅ up 后两列回归（2）" || { echo "  ❌ up 后列数=$cols"; exit 1; }

echo "== ④ 二次 up 幂等 =="
PSQL -v ON_ERROR_STOP=1 -q -f "$UP" && echo "  ✅ 二次 up 通过（幂等）" || { echo "  ❌ 二次 up 失败"; exit 1; }
idx=$(PSQL -tAc "select count(*) from pg_indexes where tablename='biz_stock_in' and indexname='uk_biz_si_source_doc'")
[ "$idx" = "1" ] && echo "  ✅ uk_biz_si_source_doc 仍唯一存在（1）" || { echo "  ❌ 索引数=$idx"; exit 1; }

echo "== 演练完成：down 守卫 / down / up / 二次 up 全绿 =="
