#!/usr/bin/env bash
# ============================================================
# ARCH-2 采购/入库勾稽设计稿 · 一键重跑（真库验证）
#
# 做什么：建库 → V0137+V0145（V0194 的前置真实状态）→ 造存量基线 → V0194 up ×2（幂等）
#          → 结构/存量核对 → down（档 A）→ 再 up → 巡检 SQL 演练 → Q1 性能量测
# 产出：同目录 apply.log / bench_q1.log / bench_q1b.log（可 diff、可归档）
#
# ⚠ 会 DROP/CREATE 库 arch2_bridge_poc —— 严禁把 DB 变量指向生产库。
# ⚠ 需要一个有 CREATEDB 权限的角色：本机实测 pflow3a 可以，alun 没有。
# ============================================================
set -uo pipefail

DB=${DB:-arch2_bridge_poc}
PSQL_ROLE=${PSQL_ROLE:-pflow3a}
EV=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$EV/../../.." && pwd)
MIG=$REPO/backend/migrations/business
UP=$EV/V0194__erp_po_contract_bridge.up.sql
DOWN=$EV/V0194__erp_po_contract_bridge.down.sql
PSQL="psql -h 127.0.0.1 -U $PSQL_ROLE -v ON_ERROR_STOP=1"

if [ -z "${PGPASSWORD:-}" ]; then
  echo "请先 export PGPASSWORD=<本机 dev 库口令>" >&2; exit 2
fi

fail=0
step() { echo; echo "===== $* ====="; }

{
step "0. 重建临时库 $DB（角色 $PSQL_ROLE）"
$PSQL -d postgres -c "DROP DATABASE IF EXISTS $DB" 2>&1 | tail -1
$PSQL -d postgres -c "CREATE DATABASE $DB" 2>&1 | tail -1 || exit 3

step "1. 前置真实状态：V0137（主数据+仓储 13 表）+ V0145（采购金额口径+收货两表）"
$PSQL -d $DB -f $MIG/V0137__erp_master_data_and_warehouse.up.sql 2>&1 | grep -E "NOTICE|ERROR" | sed 's/^/  /'
$PSQL -d $DB -f $MIG/V0145__purchase_order_amounts_and_stock_in.up.sql 2>&1 | grep -E "NOTICE|ERROR" | sed 's/^/  /'
$PSQL -d $DB -tAc "select '前置 biz_ 表数=' || count(*) from pg_tables where schemaname='public' and tablename like 'biz_%'"

step "2. 造「V0194 之前」的存量基线：3 张历史采购单 + 2 张入库单（1 有源 / 1 无源）"
$PSQL -d $DB <<'SQL'
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status,
                                total_qty, total_amount, total_tax, total_with_tax)
SELECT ('a0000000-0000-4000-8000-00000000010' || g)::uuid, 'PO-LEGACY-000' || g,
       'd1370005-0000-4000-8000-000000000001'::uuid, DATE '2026-09-01' + (g - 1),
       'd1370001-0000-4000-8000-000000000002'::uuid, 'RECEIVED', 10, 100, 13, 113
  FROM generate_series(1, 3) g;

INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status,
                          total_qty, total_amount, total_tax, total_with_tax)
SELECT ('b0000000-0000-4000-8000-00000000020' || g)::uuid, 'IN-LEGACY-000' || g, 'PURCHASE',
       CASE WHEN g = 1 THEN 'a0000000-0000-4000-8000-000000000101'::uuid ELSE NULL END,
       'd1370005-0000-4000-8000-000000000001'::uuid,
       'd1370001-0000-4000-8000-000000000002'::uuid, DATE '2026-09-01' + (g - 1), 'POSTED', 5, 50, 6.5, 56.5
  FROM generate_series(1, 2) g;
SQL
$PSQL -d $DB -tAc "select '存量基线：采购单 ' || count(*) || ' 张 / 入库单 ' || (select count(*) from biz_stock_in) || ' 张（其中无来源 ' || (select count(*) from biz_stock_in where source_order_id is null) || ' 张）' from biz_purchase_order"

step "3. 应用 V0194（up 第 1 遍）：存量行必须自动落 SPOT"
$PSQL -d $DB -f "$UP" 2>&1 | grep -E "NOTICE|ERROR|ALTER|CREATE INDEX" | sed 's/^/  /'

step "4. 幂等重放（up 第 2 遍，必须同样通过）"
$PSQL -d $DB -f "$UP" >/tmp/arch2_up2.log 2>&1; echo "  exit=$?"
grep -E "NOTICE:  V0194" /tmp/arch2_up2.log | sed 's/^/  /'

step "5. 结构核对（列 / 约束 / 索引）"
$PSQL -d $DB -c "select column_name, data_type, is_nullable, column_default from information_schema.columns where table_name='biz_purchase_order' and column_name in ('contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot') order by 1"
$PSQL -d $DB -c "select conname, pg_get_constraintdef(oid) from pg_constraint where conname in ('ck_biz_po_purchase_type','ck_biz_po_contract_pair','ck_biz_si_supplier_scope') order by 1"

step "6. 存量行落位核对（历史单必须全部 SPOT，且四列自洽）"
$PSQL -d $DB -tAc "select 'purchase_type 分布: ' || string_agg(purchase_type || '=' || n, ', ') from (select purchase_type, count(*) n from biz_purchase_order group by 1 order by 1) t"
$PSQL -d $DB -tAc "select '违反双向钉死的行数=' || count(*) from biz_purchase_order where not (case purchase_type when 'CONTRACT' then contract_id is not null and contract_no_snapshot is not null when 'SPOT' then contract_id is null and contract_no_snapshot is null else false end)"
$PSQL -d $DB -tAc "select '存量 supplier_name_snapshot 为 NULL 的行数=' || count(*) from biz_purchase_order where supplier_name_snapshot is null"

step "7. 回滚（down 档 A）：结构回落、业务数据一行不少"
$PSQL -d $DB -f "$DOWN" 2>&1 | grep -E "NOTICE|ERROR|DROP" | sed 's/^/  /'
$PSQL -d $DB -tAc "select '回滚后：四列仍在=' || count(*) || ' / 本卡约束=' || (select count(*) from pg_constraint where conname in ('ck_biz_po_purchase_type','ck_biz_po_contract_pair','ck_biz_si_supplier_scope')) || ' / 本卡索引=' || (select count(*) from pg_indexes where indexname in ('idx_biz_po_contract','idx_biz_si_no_source')) from information_schema.columns where table_name='biz_purchase_order' and column_name in ('contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot')"
$PSQL -d $DB -tAc "select '回滚后：采购单=' || (select count(*) from biz_purchase_order) || ' 张 / 入库单=' || (select count(*) from biz_stock_in) || ' 张'"

step "8. 回滚后再上线（up 第二轮），证明可逆可反复"
$PSQL -d $DB -f "$UP" >/tmp/arch2_up3.log 2>&1; echo "  再上线 exit=$?"
$PSQL -d $DB -tAc "select '再上线后：约束=' || count(*) from pg_constraint where conname in ('ck_biz_po_purchase_type','ck_biz_po_contract_pair','ck_biz_si_supplier_scope')"

step "9. 巡检 SQL 真库演练（4 类问题各造 1 条，必须全部命中）"
$PSQL -d $DB -f $EV/patrol_demo_seed.sql | tail -2
echo "--- 巡检输出 ---"
$PSQL -d $DB -f $EV/patrol_snapshot_drift.sql 2>&1 | grep -vE "^\(|^$|^-+$" | sed 's/^/  /'
} 2>&1 | tee $EV/apply.log

# 性能量测单独出日志（10 万行造数，约 5–10s）
$PSQL -d $DB -f $EV/bench_q1.sql  > $EV/bench_q1.log  2>&1 || fail=1
$PSQL -d $DB -f $EV/bench_q1b.sql > $EV/bench_q1b.log 2>&1 || fail=1
$PSQL -d $DB -c "EXPLAIN (ANALYZE, SUMMARY ON) SELECT count(*) FROM biz_stock_in WHERE source_order_id IS NULL AND deleted_at IS NULL" >> $EV/bench_q1b.log 2>&1 || fail=1

echo
echo "===== 判据 ====="
n=$(grep -c "NOTICE:  V0194 自检通过" $EV/apply.log)
m=$(grep -c "NOTICE:  V0194 回滚完成" $EV/apply.log)
k=$(grep -c "NOTICE:  V0194 存量口径"    $EV/apply.log)
echo "  自检通过=${n}（期望 2：up 两遍各一次） / 回滚完成=${m}（期望 1） / 存量口径=${k}（期望 2）"
echo "  bench_q1 Execution Time 行数=$(grep -c 'Execution Time' $EV/bench_q1.log)（期望 5）"
echo "  bench_q1b Execution Time 行数=$(grep -c 'Execution Time' $EV/bench_q1b.log)（期望 6）"
if [ "$n" = 2 ] && [ "$m" = 1 ] && [ "$k" = 2 ] && [ "$fail" = 0 ]; then
  echo "PASS：V0194 可执行 / 幂等 / 可回滚 / 可再上线，巡检与量测日志已刷新"
else
  echo "FAIL：看 $EV/apply.log 与 bench 日志"; exit 1
fi
