#!/usr/bin/env bash
# FIN-3 迁移 up → down → up 三轮演练（ARCH-6 §7.6 第 2 条：down 写不好等于没有回滚）。
#
#   bash scripts/verify/fin3_migrate_drill.sh [库名]
#
# ⚠️ 前置：库必须是**干净**的（先 `fin3_dbsetup.sh`）。V0243 的 down 有护栏——
#    只要还有 expense 审批实例（e2e 跑过就有），它会主动 RAISE 拒绝回滚，
#    这不是脚本 bug，是「流程定义不许在实例还活着时消失」的正确行为。
#
# 判据（每轮）：
#   1) down 之后四张业务表 / 2 条规则 / 12 个权限码 / expense 流程 / 两处 CHECK 放宽全部回退；
#   2) 再 up 之后表 / 8 条类型种子 / 规则 / 流程节点边数全部回来；
#   3) 全程 graph_hash 与行一致（p22b_rows_hash.py drifted = 0）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=${1:-cloudpivot_fin3}
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
  for f in V0244__erp_expense_voucher_rules V0243__erp_expense_approval_flow \
           V0242__erp_expense_tables; do
    run_file "$MIG/$f.down.sql" || { echo "    🚨 down $f 失败"; FAIL=1; }
  done
  assert_eq "报销四表已删" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_expense_type','biz_expense_report','biz_expense_item','biz_expense_payment')")" "0"
  assert_eq "报销凭证规则已删" "$(psql_q "select count(*) from biz_voucher_rule where rule_code like 'EXPENSE_%'")" "0"
  assert_eq "报销权限码已删" "$(psql_q "select count(*) from sys_menu where auth_code like 'business:erp:expense%'")" "0"
  assert_eq "expense 流程已删" "$(psql_q "select count(*) from biz_flow_def where flow_code='expense'")" "0"
  assert_eq "附件 CHECK 已收回 6 值" "$(psql_q "select count(*) from pg_constraint where conname='ck_biz_doc_attachment_type' and pg_get_constraintdef(oid) like '%EXPENSE_REPORT%'")" "0"
  assert_eq "提醒 CHECK 已收回 4 值" "$(psql_q "select count(*) from pg_constraint where conname='ck_biz_reminder_log_doc_type' and pg_get_constraintdef(oid) like '%EXPENSE_REPORT%'")" "0"

  # up（正序）
  for f in V0242__erp_expense_tables V0243__erp_expense_approval_flow \
           V0244__erp_expense_voucher_rules; do
    run_file "$MIG/$f.up.sql" || { echo "    🚨 up $f 失败"; FAIL=1; }
  done
  assert_eq "四张表已建" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_expense_type','biz_expense_report','biz_expense_item','biz_expense_payment')")" "4"
  assert_eq "8 条费用类型种子" "$(psql_q "select count(*) from biz_expense_type where deleted_at is null")" "8"
  assert_eq "2 条凭证规则" "$(psql_q "select count(*) from biz_voucher_rule where rule_code in ('EXPENSE_REPORT_DEFAULT','EXPENSE_PAY_DEFAULT') and is_active=1")" "2"
  assert_eq "12 个权限码（distinct）" "$(psql_q "select count(distinct auth_code) from sys_menu where auth_code like 'business:erp:expense%' and deleted_at is null")" "12"
  assert_eq "expense 节点数" "$(psql_q "select count(*) from biz_flow_node where version_id='20000000-0000-4000-8000-0000000d0250'")" "7"
  assert_eq "expense 边数" "$(psql_q "select count(*) from biz_flow_edge where version_id='20000000-0000-4000-8000-0000000d0250'")" "8"
  assert_eq "两条金额条件边" "$(psql_q "select count(*) from biz_flow_edge where version_id='20000000-0000-4000-8000-0000000d0250' and condition_expr is not null")" "2"
  DRIFT=$(python3 "$ROOT/scripts/verify/p22b_rows_hash.py" "$DB" 20000000-0000-4000-8000-0000000d0240 | tail -1)
  case "$DRIFT" in
    *"drifted = 0"*) echo "    ✅ graph_hash 一致（$DRIFT）" ;;
    *) echo "    🚨 graph_hash 漂移（$DRIFT）"; FAIL=1 ;;
  esac
done

echo
if [ "$FAIL" = "0" ]; then echo "★ 三轮 up/down/up 全部通过"; else echo "🚨 演练有失败项"; fi
exit "$FAIL"
