#!/usr/bin/env bash
# ============================================================================
# V0374（二期三表：标准成本卡 / 卡明细 / 差异明细）真库往返演练 —— COST-B1-4 实测证据
#
# 用法：bash 02_up_down_drill.sh <db> <outfile>
#   <db> 需已跑过 V0370（biz_cost_object 存在），例如空库全链重放过的业务库
#        （scripts/verify/mig_replay_fresh.sh，或 B1-3 留下的 mig_replay_costb1_01）
#
# 覆盖：up×3 幂等 / 表·索引·约束计数 / 唯一键谓词快照 / 列精度快照 /
#       ★ 六类差异符号约定活体验证 / ★ §4.9.2 两因素恒等式（算例 C）/
#       外键删除动作（CASCADE vs RESTRICT）/ down×2 幂等 / 回滚后重放 /
#       下游 FK 护栏反证 / 探针零残留
# ============================================================================
set -uo pipefail
DB="${1:?db}"
OUT="${2:?out}"
ROOT=/opt/cloudpivot
UP="$ROOT/backend/migrations/business/V0374__cost_standard_tables.up.sql"
DN="$ROOT/backend/migrations/business/V0374__cost_standard_tables.down.sql"
export PGPASSWORD="${PGPASSWORD:-alun}"
export PGHOST="${PGHOST:-127.0.0.1}" PGPORT="${PGPORT:-5432}" PGUSER="${PGUSER:-alun}"

say() { printf '\n===== %s =====\n' "$*"; }
q()   { psql -d "$DB" -X -q -v ON_ERROR_STOP=1 -tAc "$1"; }
runf(){ psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1 \
        | grep -Ev 'already exists, skipping'; echo "exit=${PIPESTATUS[0]}"; }
expect_fail() { # $1=描述 $2=SQL
  echo "→ 期望失败：$1"
  psql -d "$DB" -X -q -tAc "$2" 2>&1 | head -2
}

exec > >(tee "$OUT") 2>&1

say "0. 环境"
echo "db=$DB"; psql -d "$DB" -X -tAc "select version()" | cut -c1-40
q "select 'biz_cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')||'（V0370 前置）'"
q "select 'biz_cost_standard='||coalesce(to_regclass('public.biz_cost_standard')::text,'NULL')||'（本卡目标）'"
# 清掉上一次演练可能的残留（本脚本可重复执行）
q "delete from biz_cost_variance where remark like 'DRILL-B4%' or remark like 'V0374-SELFCHECK'" >/dev/null
q "delete from biz_cost_standard where remark like 'DRILL-B4%' or remark like 'V0374-SELFCHECK'" >/dev/null
q "delete from biz_cost_object where object_no like 'DRILL-B4%' or object_no like 'SELFCHECK-B4-%'" >/dev/null

say "1. 首轮 up"
runf "$UP"

say "2. 第二、三轮 up（重复执行必须结果不变）"
runf "$UP"
runf "$UP"

say "3. 幂等证据：表 / 索引 / 约束计数 + 探针零残留"
q "select 'tables='||count(*)||'（期望 3）' from pg_tables where schemaname='public' and tablename in ('biz_cost_standard','biz_cost_standard_item','biz_cost_variance')"
q "select 'idx='||count(*)||'（期望 7 = 4 业务索引 + 3 PK）' from pg_indexes where schemaname='public' and tablename in ('biz_cost_standard','biz_cost_standard_item','biz_cost_variance')"
q "select 'checks='||count(*)||'（期望 12 = 5+2+5）' from pg_constraint where contype='c' and conrelid in ('biz_cost_standard'::regclass,'biz_cost_standard_item'::regclass,'biz_cost_variance'::regclass)"
q "select 'fks='||count(*)||'（期望 3 = 1 明细→卡头 + 2 差异→(对象,卡头)）' from pg_constraint where contype='f' and conrelid in ('biz_cost_standard'::regclass,'biz_cost_standard_item'::regclass,'biz_cost_variance'::regclass)"
q "select 'probe_leftover='||((select count(*) from biz_cost_standard where remark like 'V0374-SELFCHECK')+(select count(*) from biz_cost_variance where remark like 'V0374-SELFCHECK')+(select count(*) from biz_cost_object where object_no like 'SELFCHECK-B4-%'))||'（期望 0）'"

say "4. ★ 快照：唯一索引谓词（uk_biz_cost_std 必须带软删条件）"
q "select indexname||' :: '||indexdef from pg_indexes where schemaname='public' and indexname in ('uk_biz_cost_std','uk_biz_cost_std_item','idx_biz_cost_var_period','idx_biz_cost_var_obj') order by 1"

say "5. 快照：列精度（标准价/费率 18,6；差异金额 18,4；标准工时 18,4）"
q "select table_name||'.'||column_name||' = '||data_type||'('||numeric_precision||','||numeric_scale||')' from information_schema.columns where table_schema='public' and table_name in ('biz_cost_standard','biz_cost_standard_item','biz_cost_variance') and data_type='numeric' order by 1"

say "6. 快照：外键删除动作（item→standard 必须 c=CASCADE；variance→(object,standard) 必须 r=RESTRICT）"
q "select conrelid::regclass||' -> '||confrelid::regclass||' confdeltype='||confdeltype::text from pg_constraint where contype='f' and conrelid in ('biz_cost_standard'::regclass,'biz_cost_standard_item'::regclass,'biz_cost_variance'::regclass) order by 1"

say "7. 业务语义验真：标准卡唯一 / 卡头三要素自洽 / 明细金额自洽 / 差异符号约定"
q "insert into biz_cost_object(id,object_no,product_id,target_warehouse_id,plan_qty,workshop_dept_id,workshop_dept_name,start_date) values ('00000000-0000-0000-0000-0000000d0401','DRILL-B4-OBJ',gen_random_uuid(),gen_random_uuid(),800,gen_random_uuid(),'演练车间',date '2026-01-01')" \
  && echo "  → 探针成本对象已建（算例 C：完工 800 件）"
echo "  标准卡是**单位**口径：料 1.1×12=13.20、工 0.10h×50=5.00、合计 18.20；"
echo "  乘完工 800 件 ⇒ 材料 10560 / 人工 4000（与 §4.9.2 算例 C 一致）"
q "insert into biz_cost_standard(id,material_id,version_no,effective_from,std_material_cost,std_labor_cost,std_overhead_cost,std_total_cost,std_labor_hours,std_labor_rate,remark) values ('00000000-0000-0000-0000-0000000d0402','00000000-0000-0000-0000-0000000d0410',1,date '2026-01-01',13.20,5.00,0,18.20,0.10,50.00,'DRILL-B4')" \
  && echo "  → 标准卡插入成功（单位标准成本 13.20 / 5.00 / 0 = 18.20）"
expect_fail "同物料同生效日第二张活卡（uk_biz_cost_std）" \
  "insert into biz_cost_standard(material_id,version_no,effective_from,remark) values ('00000000-0000-0000-0000-0000000d0410',2,date '2026-01-01','DRILL-B4')"
expect_fail "卡头总计 ≠ 料+工+费（ck_biz_cost_std_sum：料 100 工 0 费 0 却写 200）" \
  "insert into biz_cost_standard(material_id,version_no,effective_from,std_material_cost,std_labor_cost,std_total_cost,remark) values (gen_random_uuid(),1,date '2026-02-01',100,0,200,'DRILL-B4')"
expect_fail "标准人工成本 ≠ 工时×工资率（ck_biz_cost_std_labor）" \
  "insert into biz_cost_standard(material_id,version_no,effective_from,std_labor_cost,std_total_cost,std_labor_hours,std_labor_rate,remark) values (gen_random_uuid(),1,date '2026-02-01',70,70,2,30,'DRILL-B4')"
q "insert into biz_cost_standard_item(standard_id,line_no,component_material_id,std_qty,std_price,std_amount) values ('00000000-0000-0000-0000-0000000d0402',1,'00000000-0000-0000-0000-0000000d0411',1.1,12.00,13.20)" \
  && echo "  → 卡明细插入成功（标准用量 1.1 × 12.00 = 13.20）"
expect_fail "明细金额 ≠ 用量×单价（ck_biz_cost_std_item_cal）" \
  "insert into biz_cost_standard_item(standard_id,line_no,component_material_id,std_qty,std_price,std_amount) values ('00000000-0000-0000-0000-0000000d0402',2,gen_random_uuid(),1.1,12.00,13.50)"
expect_fail "标准用量 = 0（ck_biz_cost_std_item_num）" \
  "insert into biz_cost_standard_item(standard_id,line_no,component_material_id,std_qty,std_price,std_amount) values ('00000000-0000-0000-0000-0000000d0402',2,gen_random_uuid(),0,12.00,0)"

say "8. ★ 卡面/设计 §4.9.2：六类差异落表 + 两因素恒等式（算例 C）"
echo "  标准材料 800×1.1×12=10560；实际 1200×12.50=15000；价差 +600、量差 +3840，合计 4440 = 15000−10560"
q "insert into biz_cost_variance(period_code,cost_object_id,standard_id,variance_type,element,material_id,actual_qty,std_qty,actual_price,std_price,amount,direction,remark) values ('2026-01','00000000-0000-0000-0000-0000000d0401','00000000-0000-0000-0000-0000000d0402','MAT_PRICE','MATERIAL','00000000-0000-0000-0000-0000000d0411',1200,880,12.50,12.00,600,'UNFAVORABLE','DRILL-B4'),('2026-01','00000000-0000-0000-0000-0000000d0401','00000000-0000-0000-0000-0000000d0402','MAT_QTY','MATERIAL','00000000-0000-0000-0000-0000000d0411',1200,880,12.50,12.00,3840,'UNFAVORABLE','DRILL-B4'),('2026-01','00000000-0000-0000-0000-0000000d0401','00000000-0000-0000-0000-0000000d0402','LABOR_RATE','LABOR',null,90,80,48.00,50.00,-180,'FAVORABLE','DRILL-B4'),('2026-01','00000000-0000-0000-0000-0000000d0401','00000000-0000-0000-0000-0000000d0402','LABOR_EFF','LABOR',null,90,80,48.00,50.00,500,'UNFAVORABLE','DRILL-B4')" \
  && echo "  → 四条差异插入成功（材料价差+600 / 量差+3840 / 人工率差−180 / 效差+500）"
q "select '材料两因素之和='||sum(amount)||'（必须 = 15000−10560 = 4440）' from biz_cost_variance where remark='DRILL-B4' and element='MATERIAL'"
q "select '人工两因素之和='||sum(amount)||'（必须 = 4320−4000 = 320）' from biz_cost_variance where remark='DRILL-B4' and element='LABOR'"
q "select case when (select sum(amount) from biz_cost_variance where remark='DRILL-B4' and element='MATERIAL') = (15000-10560) then '材料恒等式 PASS' else '材料恒等式 FAIL' end"
q "select case when (select sum(amount) from biz_cost_variance where remark='DRILL-B4' and element='LABOR') = (90*48-800*0.10*50) then '人工恒等式 PASS' else '人工恒等式 FAIL' end"
expect_fail "direction=UNFAVORABLE 但 amount<0（ck_biz_cost_var_sign）" \
  "insert into biz_cost_variance(period_code,variance_type,element,amount,direction,remark) values ('2026-01','MAT_QTY','MATERIAL',-100,'UNFAVORABLE','DRILL-B4')"
expect_fail "六类之外的差异类型 MAT_MIX（ck_biz_cost_var_type）" \
  "insert into biz_cost_variance(period_code,variance_type,element,amount,direction,remark) values ('2026-01','MAT_MIX','MATERIAL',100,'UNFAVORABLE','DRILL-B4')"
expect_fail "期间格式 2026-13（ck_biz_cost_var_period）" \
  "insert into biz_cost_variance(period_code,variance_type,element,amount,direction,remark) values ('2026-13','MAT_QTY','MATERIAL',100,'UNFAVORABLE','DRILL-B4')"

say "9. 外键 RESTRICT：有差异记录时删标准卡 / 删成本对象都必须被拦"
expect_fail "删标准卡（variance→standard RESTRICT）" \
  "delete from biz_cost_standard where id='00000000-0000-0000-0000-0000000d0402'"
expect_fail "删成本对象（variance→cost_object RESTRICT）" \
  "delete from biz_cost_object where id='00000000-0000-0000-0000-0000000d0401'"

say "10. CASCADE：删标准卡 ⇒ 卡明细跟着走"
q "insert into biz_cost_standard(id,material_id,version_no,effective_from,remark) values ('00000000-0000-0000-0000-0000000d0403',gen_random_uuid(),1,date '2026-03-01','DRILL-B4')" >/dev/null
q "insert into biz_cost_standard_item(standard_id,line_no,component_material_id,std_qty,std_price,std_amount) values ('00000000-0000-0000-0000-0000000d0403',1,gen_random_uuid(),2,5.00,10.00)" >/dev/null
q "select 'before_cascade='||count(*) from biz_cost_standard_item where standard_id='00000000-0000-0000-0000-0000000d0403'"
q "delete from biz_cost_standard where id='00000000-0000-0000-0000-0000000d0403'" >/dev/null
q "select 'items_after_cascade='||count(*)||'（期望 0）' from biz_cost_standard_item where standard_id='00000000-0000-0000-0000-0000000d0403'"

say "11. 清理演练行"
q "delete from biz_cost_variance where remark like 'DRILL-B4%'" >/dev/null
q "delete from biz_cost_standard where remark like 'DRILL-B4%'" >/dev/null
q "delete from biz_cost_object where object_no like 'DRILL-B4%'" >/dev/null
q "select 'leftover='||((select count(*) from biz_cost_standard)+(select count(*) from biz_cost_standard_item)+(select count(*) from biz_cost_variance)+(select count(*) from biz_cost_variance where remark like 'DRILL%'))||'（期望 0）'"

say "12. down 回滚"
runf "$DN"

say "13. down 后零残留断言"
q "select 'standard='||coalesce(to_regclass('public.biz_cost_standard')::text,'NULL')||' item='||coalesce(to_regclass('public.biz_cost_standard_item')::text,'NULL')||' variance='||coalesce(to_regclass('public.biz_cost_variance')::text,'NULL')||' cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')||'（最后一项必须还在）'"
q "select 'idx_left='||count(*)||'（期望 0）' from pg_indexes where schemaname='public' and (indexname like '%biz_cost_std%' or indexname like '%biz_cost_var%' or tablename in ('biz_cost_standard','biz_cost_standard_item','biz_cost_variance'))"

say "14. down 再跑一次（幂等）"
runf "$DN"

say "15. 回滚后重新 up（可重放）"
runf "$UP"
q "select 'replay tables='||count(*)||'（期望 3）' from pg_tables where schemaname='public' and tablename in ('biz_cost_standard','biz_cost_standard_item','biz_cost_variance')"

say "16. 下游护栏反证：外部表 FK 到标准卡时 down 必须拒绝执行"
q "create table zz_downstream (id uuid primary key, std_id uuid references biz_cost_standard(id))"
echo "→ 期望 V0374 down 被拒（提示先回滚下游迁移）："
psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$DN" 2>&1 | head -3
q "drop table zz_downstream"

say "17. 收尾（保持三表在位，供二期 S-* 卡使用）"
q "select 'final tables='||count(*) from pg_tables where schemaname='public' and tablename in ('biz_cost_standard','biz_cost_standard_item','biz_cost_variance')"
echo "== DRILL END =="
