#!/usr/bin/env bash
# FIN-2 迁移 up → down → up 三轮演练（ARCH-6 §7.6 第 2 条：down 写不好等于没有回滚）。
#
#   bash scripts/verify/fin2_migrate_drill.sh [库名]
#
# 判据（每轮）：
#   1) down 之后四张业务表 + 3 条规则全部消失、菜单/权限码清干净；
#   2) 再 up 之后表 / 分类种子 / 规则 / 流程节点数全部回来；
#   3) 全程 graph_hash 与行一致（p22b_rows_hash.py drifted = 0）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=${1:-cloudpivot_fin2asset}
MIG=/opt/cloudpivot/backend/migrations/business
ROOT=/opt/cloudpivot
export PGPASSWORD=alun

psql_q() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' -v ON_ERROR_STOP=1 -c "$1"; }
run_file() { psql -h 127.0.0.1 -U alun -d "$DB" -q -v ON_ERROR_STOP=1 -f "$1" > /dev/null; }

assert_eq() { # label actual expected
  if [ "$2" = "$3" ]; then echo "    ✅ $1 = $2"; else echo "    🚨 $1 = $2（期望 $3）"; FAIL=1; fi
}

FAIL=0
for round in 1 2 3; do
  echo "──────── 第 $round 轮 ────────"
  # down（逆序）
  for f in V0232__erp_asset_voucher_rules V0231__erp_asset_disposal_flow \
           V0230__erp_depreciation_tables V0229__erp_asset_tables; do
    run_file "$MIG/$f.down.sql" || { echo "    🚨 down $f 失败"; FAIL=1; }
  done
  assert_eq "资产表已删" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_asset_category','biz_fixed_asset','biz_fixed_asset_change','biz_depreciation_batch','biz_depreciation')")" "0"
  assert_eq "固资凭证规则已删" "$(psql_q "select count(*) from biz_voucher_rule where rule_code like 'FIXED_ASSET%' or rule_code='DEPRECIATION_DEFAULT'")" "0"
  assert_eq "asset 权限码已删" "$(psql_q "select count(*) from sys_menu where auth_code like 'business:erp:asset%' or auth_code like 'business:erp:depreciation:%'")" "0"
  assert_eq "asset_disposal 流程已删" "$(psql_q "select count(*) from biz_flow_def where flow_code='asset_disposal'")" "0"

  # up（正序）
  for f in V0229__erp_asset_tables V0230__erp_depreciation_tables \
           V0231__erp_asset_disposal_flow V0232__erp_asset_voucher_rules; do
    run_file "$MIG/$f.up.sql" || { echo "    🚨 up $f 失败"; FAIL=1; }
  done
  assert_eq "五张表已建" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_asset_category','biz_fixed_asset','biz_fixed_asset_change','biz_depreciation_batch','biz_depreciation')")" "5"
  assert_eq "分类种子" "$(psql_q "select count(*) from biz_asset_category where deleted_at is null")" "5"
  assert_eq "固资凭证规则" "$(psql_q "select count(*) from biz_voucher_rule where rule_code in ('FIXED_ASSET_ACQ_DEFAULT','FIXED_ASSET_ACQ_CASH','FIXED_ASSET_ACQ_STOCK','DEPRECIATION_DEFAULT')")" "4"
  assert_eq "asset 权限码 14 个" "$(psql_q "select count(*) from sys_menu where auth_code like 'business:erp:asset%' or auth_code like 'business:erp:depreciation:%'")" "14"
  assert_eq "asset_disposal 节点数" "$(psql_q "select count(*) from biz_flow_node where version_id='20000000-0000-4000-8000-0000000c0231'")" "3"
  assert_eq "asset_disposal 边数" "$(psql_q "select count(*) from biz_flow_edge where version_id='20000000-0000-4000-8000-0000000c0231'")" "2"
  DRIFT=$(python3 "$ROOT/scripts/verify/p22b_rows_hash.py" "$DB" 20000000-0000-4000-8000-0000000c0230 | tail -1)
  case "$DRIFT" in
    *"drifted = 0"*) echo "    ✅ graph_hash 一致（$DRIFT）" ;;
    *) echo "    🚨 graph_hash 漂移（$DRIFT）"; FAIL=1 ;;
  esac
done

echo
if [ "$FAIL" = "0" ]; then echo "★ 三轮 up/down/up 全部通过"; else echo "🚨 演练有失败项"; fi
exit "$FAIL"
