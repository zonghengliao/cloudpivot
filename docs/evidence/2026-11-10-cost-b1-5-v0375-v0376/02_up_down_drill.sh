#!/usr/bin/env bash
# ============================================================================
# V0375（出库 + cost_object_id）+ V0376（入库 PRODUCTION + 两列）真库往返演练
# COST-B1-5 实测证据（卡 t_b2c9aa3d）
#
# 用法：bash 02_up_down_drill.sh <db> <outfile>
#   <db> 需已含 biz_stock_out / biz_stock_in / biz_cost_object（即已跑到 V0374 的库）
#
# 覆盖：
#   0. 造「存量」探针行（4 种 in_type / 4 种 out_type）+ 存量指纹
#   1. up 首轮 / ×3 幂等（NOTICE 全量留痕）
#   2. 结构快照：列 / 约束定义 / 索引
#   3. ★ 存量数据不受影响：指纹逐字节相等（显式列，不含新列）
#   4. ★ 卡面验收：in_type 允许 PRODUCTION（活体插入）
#   5. ★ 已知开放问题：无采购单的完工入库被 ck_biz_si_sourceless_pair 拦
#   6. down 数据护栏：有 PRODUCTION / 有 cost_object_id 时拒绝回滚并点名
#   7. down 正常回滚 + 幂等（×2）+ 存量指纹仍相等
#   8. 回滚后重放 up，存量指纹仍相等
# ============================================================================
set -uo pipefail
DB="${1:?db}"
OUT="${2:?out}"
ROOT=/opt/cloudpivot
UP5="$ROOT/backend/migrations/business/V0375__stock_out_cost_object.up.sql"
DN5="$ROOT/backend/migrations/business/V0375__stock_out_cost_object.down.sql"
UP6="$ROOT/backend/migrations/business/V0376__stock_in_production.up.sql"
DN6="$ROOT/backend/migrations/business/V0376__stock_in_production.down.sql"
export PGPASSWORD=alun
export PGHOST=127.0.0.1 PGPORT=5432 PGUSER=alun

say() { printf '\n===== %s =====\n' "$*"; }
q()   { psql -d "$DB" -X -q -v ON_ERROR_STOP=1 -tAc "$1"; }
runf(){ psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1 \
        | grep -Ev 'already exists, skipping'; echo "  exit=${PIPESTATUS[0]}"; }
expect_fail() { # $1=描述 $2=SQL
  echo "→ 期望失败：$1"
  psql -d "$DB" -X -q -tAc "$2" 2>&1 | grep -E 'ERROR|错误|违反|violates' | head -2
}
# 存量指纹：只用**本卡之前就存在**的列，新列不进指纹（否则加列后必然不等）
FP_IN="select md5(string_agg(concat_ws('|', in_no, in_type, coalesce(supplier_id::text,'-'),
        warehouse_id, in_date, status, total_qty, total_amount, total_tax, total_with_tax,
        is_sourceless, coalesce(source_order_id::text,'-'), coalesce(remark,'-')), ',' order by in_no))
        from biz_stock_in where in_no like 'LEGACY-B15-%'"
FP_OUT="select md5(string_agg(concat_ws('|', out_no, out_type, warehouse_id, out_date, status,
        total_qty, total_amount, coalesce(remark,'-')), ',' order by out_no))
        from biz_stock_out where out_no like 'LEGACY-B15-%'"

exec > >(tee "$OUT") 2>&1

say "0. 环境"
echo "db=$DB"; psql -d "$DB" -X -tAc "select version()" | cut -c1-40
q "select 'biz_stock_in='||coalesce(to_regclass('public.biz_stock_in')::text,'NULL')"
q "select 'biz_stock_out='||coalesce(to_regclass('public.biz_stock_out')::text,'NULL')"
q "select 'biz_cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')||'（V0370 前置）'"
q "select 'biz_cost_period='||coalesce(to_regclass('public.biz_cost_period')::text,'NULL')||'（V0373 前置）'"

say "0b. 造存量探针行（本卡之前就合法的 4 种 in_type / 4 种 out_type）"
q "delete from biz_stock_in where in_no like 'LEGACY-B15-%'" >/dev/null
q "delete from biz_stock_out where out_no like 'LEGACY-B15-%'" >/dev/null
# in_type: PURCHASE(有采购单+供应商) / RETURN(有采购单无双) / OTHER(无单无供，is_sourceless=TRUE) / SOURCELESS(无单有供)
q "insert into biz_stock_in (in_no,in_type,warehouse_id,in_date,status,supplier_id,source_order_id,is_sourceless,total_qty,total_amount,total_tax,total_with_tax,remark) values
   ('LEGACY-B15-I1','PURCHASE', gen_random_uuid(),date '2026-09-01','POSTED', gen_random_uuid(), gen_random_uuid(), false, 10, 100, 13, 113, '存量-采购'),
   ('LEGACY-B15-I2','RETURN',   gen_random_uuid(),date '2026-09-02','POSTED', null, gen_random_uuid(), false, 2, 20, 2.6, 22.6, '存量-退货'),
   ('LEGACY-B15-I3','OTHER',    gen_random_uuid(),date '2026-09-03','DRAFT',  null, null, true, 3, 30, 3.9, 33.9, '存量-其他'),
   ('LEGACY-B15-I4','SOURCELESS',gen_random_uuid(),date '2026-09-04','POSTED',gen_random_uuid(), null, true, 4, 40, 5.2, 45.2, '存量-无源')"
q "insert into biz_stock_out (out_no,out_type,warehouse_id,out_date,status,total_qty,total_amount,remark) values
   ('LEGACY-B15-O1','ISSUE',gen_random_uuid(),date '2026-09-05','POSTED',1,10,'存量-领料'),
   ('LEGACY-B15-O2','SCRAP',gen_random_uuid(),date '2026-09-06','POSTED',2,20,'存量-报废'),
   ('LEGACY-B15-O3','LOSS', gen_random_uuid(),date '2026-09-07','DRAFT', 3,30,'存量-盘亏'),
   ('LEGACY-B15-O4','OTHER',gen_random_uuid(),date '2026-09-08','POSTED',4,40,'存量-其他')" \
  && echo "  → 8 行存量探针已写入"
IN_FP0=$(q "$FP_IN"); OUT_FP0=$(q "$FP_OUT")
echo "存量指纹(前)  in=$IN_FP0"
echo "存量指纹(前) out=$OUT_FP0"
q "select 'stock_in_rows='||count(*) from biz_stock_in"
q "select 'stock_out_rows='||count(*) from biz_stock_out"

say "1. up 首轮（V0375 出库加列 → V0376 入库 PRODUCTION + 两列）"
runf "$UP5"
runf "$UP6"

say "2. 第二、三轮 up（幂等：重复执行结果不变）"
runf "$UP5"; runf "$UP6"; runf "$UP5"; runf "$UP6"

say "3. 结构快照：新列"
q "select table_name||'.'||column_name||' :: '||data_type||' nullable='||is_nullable||' default='||coalesce(column_default,'<无>')
     from information_schema.columns
    where table_schema='public' and ((table_name='biz_stock_out' and column_name='cost_object_id')
       or (table_name='biz_stock_in' and column_name in ('cost_object_id','cost_period_id')))
    order by 1"

say "4. 结构快照：in_type 值域 + 完工入库约束 + 未动的供应商约束"
q "select conname||' :: '||pg_get_constraintdef(oid)
     from pg_constraint
    where conrelid='public.biz_stock_in'::regclass and conname in
          ('ck_biz_si_type','ck_biz_si_production','ck_biz_si_supplier_scope','ck_biz_si_supplier','ck_biz_si_sourceless_pair')
    order by 1"

say "5. 结构快照：三条局部索引"
q "select indexname||' :: '||indexdef from pg_indexes
    where schemaname='public' and indexname in ('idx_biz_so_cost_obj','idx_biz_si_cost_obj','idx_biz_si_cost_period')
    order by 1"

say "6. ★ 存量数据不受影响：指纹对比（加列后必须逐字节相等）"
IN_FP1=$(q "$FP_IN"); OUT_FP1=$(q "$FP_OUT")
echo "存量指纹(后)  in=$IN_FP1  → $([ "$IN_FP0" = "$IN_FP1" ] && echo '相等 ✅' || echo '不等 ❌')"
echo "存量指纹(后) out=$OUT_FP1  → $([ "$OUT_FP0" = "$OUT_FP1" ] && echo '相等 ✅' || echo '不等 ❌')"
q "select 'legacy_cost_object_filled='||count(*)||'（期望 0：加列不许给存量填值）' from biz_stock_in where cost_object_id is not null"
q "select 'stock_in_rows='||count(*)||'（期望 4）' from biz_stock_in where in_no like 'LEGACY-B15-%'"

say "7. ★ 卡面验收：biz_stock_in.in_type 允许 'PRODUCTION'（活体插入 → 删除）"
q "insert into biz_stock_in (in_no,in_type,warehouse_id,in_date,status,cost_object_id,cost_period_id,source_order_id,is_sourceless,total_qty,total_amount,total_tax,total_with_tax,remark)
   values ('DRILL-B15-OK','PRODUCTION',gen_random_uuid(),date '2026-10-31','POSTED',gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),false,100,1234.5678,0,1234.5678,'演练-完工入库')" \
  && echo "  → PRODUCTION 入库单插入成功 ✅（验收通过）"
q "select 'production_rows='||count(*)||'（期望 1）' from biz_stock_in where in_type='PRODUCTION'"
q "select in_no||' in_type='||in_type||' cost_obj='||(cost_object_id is not null)::text||' cost_period='||(cost_period_id is not null)::text
     from biz_stock_in where in_no='DRILL-B15-OK'"

say "8. ★ 已知开放问题复现：无采购单的完工入库（结算口径）被 V0199 ck_biz_si_sourceless_pair 拦"
expect_fail "PRODUCTION + 无 source_order_id + is_sourceless=FALSE（§4.7 结算就是这么写的）" \
  "insert into biz_stock_in (in_no,in_type,warehouse_id,in_date,status,cost_object_id,cost_period_id,is_sourceless)
   values ('DRILL-B15-NOSRC','PRODUCTION',gen_random_uuid(),date '2026-10-31','POSTED',gen_random_uuid(),gen_random_uuid(),false)"
expect_fail "PRODUCTION + 供应商" \
  "insert into biz_stock_in (in_no,in_type,warehouse_id,in_date,status,cost_object_id,cost_period_id,supplier_id,source_order_id,is_sourceless)
   values ('DRILL-B15-SUP','PRODUCTION',gen_random_uuid(),date '2026-10-31','POSTED',gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),gen_random_uuid(),false)"
expect_fail "PRODUCTION + 缺 cost_period_id" \
  "insert into biz_stock_in (in_no,in_type,warehouse_id,in_date,status,cost_object_id,source_order_id,is_sourceless)
   values ('DRILL-B15-NOPER','PRODUCTION',gen_random_uuid(),date '2026-10-31','POSTED',gen_random_uuid(),gen_random_uuid(),false)"
expect_fail "PRODUCTION 当无源单（is_sourceless=TRUE）" \
  "insert into biz_stock_in (in_no,in_type,warehouse_id,in_date,status,cost_object_id,cost_period_id,is_sourceless)
   values ('DRILL-B15-SL','PRODUCTION',gen_random_uuid(),date '2026-10-31','POSTED',gen_random_uuid(),gen_random_uuid(),true)"

say "9. ★ down 数据护栏：库里还有 PRODUCTION 单时，回滚必须拒绝并点名"
runf "$DN6"

say "10. 出库侧护栏：有单据挂着成本对象时，V0375 down 必须拒绝并点名"
q "update biz_stock_out set cost_object_id=gen_random_uuid() where out_no='LEGACY-B15-O1'" >/dev/null
runf "$DN5"

say "11. 清掉演练行，正常回滚（V0376 → V0375）"
q "delete from biz_stock_in where in_no like 'DRILL-B15-%'" >/dev/null
q "update biz_stock_out set cost_object_id=null where out_no like 'LEGACY-B15-%'" >/dev/null
runf "$DN6"
runf "$DN5"

say "12. down 幂等（再各来一遍）"
runf "$DN6"
runf "$DN5"

say "13. 回滚后结构：新列 / 新索引 / 新约束必须全没了，老约束回来"
q "select 'new_cols_left='||count(*)||'（期望 0）' from information_schema.columns
    where table_schema='public' and ((table_name='biz_stock_out' and column_name='cost_object_id')
       or (table_name='biz_stock_in' and column_name in ('cost_object_id','cost_period_id')))"
q "select 'new_idx_left='||count(*)||'（期望 0）' from pg_indexes
    where schemaname='public' and indexname in ('idx_biz_so_cost_obj','idx_biz_si_cost_obj','idx_biz_si_cost_period')"
q "select 'ck_production_left='||count(*)||'（期望 0）' from pg_constraint
    where conname='ck_biz_si_production' and conrelid='public.biz_stock_in'::regclass"
q "select conname||' :: '||pg_get_constraintdef(oid) from pg_constraint
    where conrelid='public.biz_stock_in'::regclass and conname in ('ck_biz_si_type','ck_biz_si_supplier_scope') order by 1"

say "14. 回滚后存量数据仍完好（指纹必须与最初相等）"
IN_FP2=$(q "$FP_IN"); OUT_FP2=$(q "$FP_OUT")
echo "存量指纹(回滚后)  in=$IN_FP2  → $([ "$IN_FP0" = "$IN_FP2" ] && echo '相等 ✅' || echo '不等 ❌')"
echo "存量指纹(回滚后) out=$OUT_FP2  → $([ "$OUT_FP0" = "$OUT_FP2" ] && echo '相等 ✅' || echo '不等 ❌')"
q "select 'stock_in_rows='||count(*)||'（期望 4：回滚只删列不删行）' from biz_stock_in where in_no like 'LEGACY-B15-%'"
q "select 'stock_out_rows='||count(*)||'（期望 4）' from biz_stock_out where out_no like 'LEGACY-B15-%'"

say "15. 回滚后重放（down → up 必须能重建，存量仍不受影响）"
runf "$UP5"
runf "$UP6"
IN_FP3=$(q "$FP_IN"); OUT_FP3=$(q "$FP_OUT")
echo "存量指纹(重放后)  in=$IN_FP3  → $([ "$IN_FP0" = "$IN_FP3" ] && echo '相等 ✅' || echo '不等 ❌')"
echo "存量指纹(重放后) out=$OUT_FP3  → $([ "$OUT_FP0" = "$OUT_FP3" ] && echo '相等 ✅' || echo '不等 ❌')"
q "select 'ck_biz_si_type 含 PRODUCTION='||(pg_get_constraintdef(oid) like '%PRODUCTION%')::text from pg_constraint
    where conname='ck_biz_si_type' and conrelid='public.biz_stock_in'::regclass"

say "16. 收尾：清演练数据并核对零残留"
q "delete from biz_stock_in where in_no like 'LEGACY-B15-%' or in_no like 'DRILL-B15-%' or in_no like 'V0376-PROBE-%'" >/dev/null
q "delete from biz_stock_out where out_no like 'LEGACY-B15-%'" >/dev/null
q "select 'residue_in='||count(*)||'（期望 0）' from biz_stock_in where in_no like 'LEGACY-B15-%' or in_no like 'DRILL-B15-%' or in_no like 'V0376-PROBE-%'"
q "select 'residue_out='||count(*)||'（期望 0）' from biz_stock_out where out_no like 'LEGACY-B15-%'"
echo
echo "== 演练结束（终态：V0375/V0376 已 up 在位）=="
