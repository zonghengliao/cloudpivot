#!/usr/bin/env bash
# V0211（盘点单）迁移 up / down / up / down / up 三轮来回（隔离库 cp_p13）
#
#   P13_DB_NAME=cp_p13 bash scripts/verify/p13_migration_roundtrip.sh
#
# 每一轮都核对：down 后零残留（表 / 权限节点 / 取号 / 审批流程），
# up 后结构齐全（2 表 / 8 索引 / 7 约束 / 6 权限节点 / 1 取号 / 1 流程 + 6 节点 6 边）。
set -uo pipefail
export PGPASSWORD=${PGPASSWORD:-alun}
DB=${P13_DB_NAME:-cp_p13}
UP=/opt/cloudpivot/backend/migrations/business/V0211__erp_stock_take.up.sql
DOWN=/opt/cloudpivot/backend/migrations/business/V0211__erp_stock_take.down.sql

run() { psql -h 127.0.0.1 -U alun -d "$DB" -X -q -v ON_ERROR_STOP=1 -f "$1" 2>&1 | sed 's/^/    /'; return ${PIPESTATUS[0]}; }

snapshot() {
  psql -h 127.0.0.1 -U alun -d "$DB" -X -q -tAc "
    SELECT 'tables=' || (SELECT count(*) FROM information_schema.tables
                          WHERE table_schema='public' AND table_name IN ('biz_stock_take','biz_stock_take_item'))
        || ' idx='    || (SELECT count(*) FROM pg_indexes WHERE schemaname='public'
                           AND indexname IN ('uk_biz_st_no','idx_biz_st_status','idx_biz_st_date',
                                             'idx_biz_st_warehouse','idx_biz_st_voucher','idx_biz_st_approval',
                                             'idx_biz_st_item_take','idx_biz_st_item_material'))
        || ' con='    || (SELECT count(*) FROM pg_constraint
                           WHERE conname IN ('ck_biz_st_status','ck_biz_st_type','ck_biz_st_totals','ck_biz_st_cancel',
                                             'uk_biz_st_item_line','ck_biz_st_item_qty','ck_biz_st_item_amount'))
        || ' menu='   || (SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:stock-take:%')
        || ' seq='    || (SELECT count(*) FROM biz_seq WHERE seq_type='STOCK_TAKE')
        || ' flow='   || (SELECT count(*) FROM biz_flow_def WHERE flow_code='stock_take_post' AND deleted_at IS NULL)
        || ' nodes='  || (SELECT count(*) FROM biz_flow_node WHERE version_id='20000000-0000-4000-8000-0000000c2111'::uuid)
        || ' edges='  || (SELECT count(*) FROM biz_flow_edge WHERE version_id='20000000-0000-4000-8000-0000000c2111'::uuid)
        || ' gencol=' || (SELECT count(*) FROM information_schema.columns
                           WHERE table_name='biz_stock_take_item' AND column_name='diff_qty' AND is_generated='ALWAYS')"
}

fail=0
for round in 1 2; do
  echo "== 第 ${round} 轮 down"
  run "$DOWN" || fail=1
  after_down=$(snapshot)
  echo "   状态：$after_down"
  case "$after_down" in
    *"tables=0 idx=0 con=0 menu=0 seq=0 flow=0 nodes=0 edges=0 gencol=0"*) echo "   零残留 OK" ;;
    *) echo "   ✗ down 后仍有残留"; fail=1 ;;
  esac
  echo "== 第 ${round} 轮 up"
  run "$UP" || fail=1
  after_up=$(snapshot)
  echo "   状态：$after_up"
  case "$after_up" in
    *"tables=2 idx=8 con=7 menu=6 seq=1 flow=1 nodes=6 edges=6 gencol=1"*) echo "   结构齐全 OK" ;;
    *) echo "   ✗ up 后结构不齐"; fail=1 ;;
  esac
done

echo "== 终态核对（应 tables=2 idx=8 con=7 menu=6 seq=1 flow=1 nodes=6 edges=6 gencol=1）"
final=$(snapshot)
echo "   $final"
case "$final" in
  *"tables=2 idx=8 con=7 menu=6 seq=1 flow=1 nodes=6 edges=6 gencol=1"*) echo "== 终态 OK" ;;
  *) echo "== 终态不符，失败"; fail=1 ;;
esac
exit $fail
