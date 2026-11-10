#!/usr/bin/env bash
# V0371（归集四表）真库往返演练 —— 本卡实测证据
# 用法：bash 02_up_down_drill.sh <db> <outfile>
#   <db> 需已跑过 V0370（biz_cost_object 存在），例如空库全链重放过一次的业务库
set -uo pipefail
DB="${1:?db}"
OUT="${2:?out}"
ROOT=/opt/cloudpivot
UP="$ROOT/backend/migrations/business/V0371__cost_collect_four_tables.up.sql"
DOWN="$ROOT/backend/migrations/business/V0371__cost_collect_four_tables.down.sql"
export PGPASSWORD=alun
export PGHOST=127.0.0.1 PGPORT=5432 PGUSER=alun

say() { printf '\n===== %s =====\n' "$*"; }
q()   { psql -d "$DB" -X -q -v ON_ERROR_STOP=1 -tAc "$1"; }
runf(){ psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1 | grep -Ev 'already exists, skipping' ; echo "exit=${PIPESTATUS[0]}"; }

exec > >(tee "$OUT") 2>&1

say "0. 环境"
echo "db=$DB"; psql -d "$DB" -X -tAc "select version()" | cut -c1-40
q "select 'biz_cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')||'（V0370 前置）'"
# 清掉上一次演练可能残留（本脚本可重复执行）
q "delete from biz_cost_collect where remark='V0371-SELFCHECK'"
q "delete from biz_cost_labor where remark='V0371-SELFCHECK'"
q "delete from biz_cost_overhead where remark='V0371-SELFCHECK'"
q "delete from biz_cost_rate where remark='V0371-SELFCHECK'"
q "delete from biz_cost_object where object_no in ('DRILL-B2-CB','SELFCHECK-B2-CB')"

say "1. 首轮 up（本库首次应用）"
runf "$UP"

say "2. 第二、三轮 up（重复执行必须结果不变）"
runf "$UP"
runf "$UP"

say "3. 幂等证据：探针零残留 + 表/索引/约束数量不变"
q "select 'rows_collect='||count(*) from biz_cost_collect"
q "select 'rows_labor='||count(*) from biz_cost_labor"
q "select 'rows_overhead='||count(*) from biz_cost_overhead"
q "select 'rows_rate='||count(*) from biz_cost_rate"
q "select 'probe_leftover='||((select count(*) from biz_cost_collect where remark='V0371-SELFCHECK')+(select count(*) from biz_cost_labor where remark='V0371-SELFCHECK')+(select count(*) from biz_cost_overhead where remark='V0371-SELFCHECK')+(select count(*) from biz_cost_rate where remark='V0371-SELFCHECK')+(select count(*) from biz_cost_object where object_no='SELFCHECK-B2-CB'))"
q "select 'idx='||count(*) from pg_indexes where schemaname='public' and tablename in ('biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate')"
q "select 'checks='||count(*) from pg_constraint where conrelid in ('biz_cost_collect'::regclass,'biz_cost_labor'::regclass,'biz_cost_overhead'::regclass,'biz_cost_rate'::regclass) and contype='c'"
q "select 'fks='||count(*) from pg_constraint where conrelid in ('biz_cost_collect'::regclass,'biz_cost_labor'::regclass,'biz_cost_overhead'::regclass,'biz_cost_rate'::regclass) and contype='f'"

say "4. 快照：唯一索引谓词（必须带软删条件）"
q "select indexname||' :: '||indexdef from pg_indexes where schemaname='public' and indexname in ('uk_biz_cost_collect_src','uk_biz_cost_rate')"

say "5. 快照：外键及其删除动作（confdeltype 必须是 r=RESTRICT）"
q "select conrelid::regclass||' -> '||confrelid::regclass||' confdeltype='||confdeltype::text||' :: '||pg_get_constraintdef(oid) from pg_constraint where contype='f' and conrelid in ('biz_cost_collect'::regclass,'biz_cost_labor'::regclass,'biz_cost_overhead'::regclass,'biz_cost_rate'::regclass)"

say "6. 快照：金额 / 数量列精度（设计 §3.1：金额数量 18,4、单价费率 18,6）"
q "select table_name||'.'||column_name||' = '||data_type||'('||numeric_precision||','||numeric_scale||')' from information_schema.columns where table_schema='public' and table_name in ('biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate') and data_type='numeric' order by 1"

say "7. 卡面「金额 numeric(20,2)」的反证：scale=2 装不下 4 位小数（设计取 18,4 的依据）"
q "insert into biz_cost_object(id,object_no,product_id,target_warehouse_id,plan_qty,workshop_dept_id,workshop_dept_name,start_date) values ('00000000-0000-0000-0000-0000000c05c2','DRILL-B2-CB',gen_random_uuid(),gen_random_uuid(),10,gen_random_uuid(),'演练车间',date '2026-01-01')"
q "create table zz_prec_probe (hours numeric(10,2) not null, rate numeric(18,6) not null, amount numeric(20,2) not null, constraint ck_zz_amount check (amount = round(hours * rate, 4)))"
echo "→ 同一份代码、只把 amount 换成 numeric(20,2)：工资率 33.333333 的工时行（期望 check 违反）"
psql -d "$DB" -X -q -tAc "insert into zz_prec_probe(hours,rate,amount) values (1, 33.333333, 33.3333)" 2>&1 | head -2
q "select 'scale2_table_rows='||count(*)||'（期望 0 ⇒ 20,2 下真实工资率的工时行插不进去）' from zz_prec_probe"
q "drop table zz_prec_probe"
echo "→ 同样的数据写进 numeric(18,4) 的 biz_cost_labor（期望成功）："
q "insert into biz_cost_labor(period_code,work_date,cost_object_id,employee_name,dept_id,dept_name,hours,machine_hours,rate,amount,status,remark) values ('2026-02',date '2026-02-02','00000000-0000-0000-0000-0000000c05c2','精度验证',gen_random_uuid(),'精度车间',1,0,33.333333,33.3333,'DRAFT','V0371-SELFCHECK')" \
  && echo "  → 18,4 落库成功"
q "select 'labor_scale4_rows='||count(*) from biz_cost_labor where remark='V0371-SELFCHECK'"

say "8. 业务语义验真：软删后同来源行可重新归集 / 活行重号被拦 / 池行允许无对象"
q "insert into biz_cost_collect(cost_object_id,period_code,element,source_type,source_id,line_no,occur_date,material_id,qty,unit_price,amount,remark) values ('00000000-0000-0000-0000-0000000c05c2','2026-02','MATERIAL','STOCK_OUT','00000000-0000-0000-0000-0000000c05c3',1,date '2026-02-01',gen_random_uuid(),10,12.5,125,'V0371-SELFCHECK')" \
  && echo "→ 首次归集：OK"
q "update biz_cost_collect set deleted_at=now() where source_id='00000000-0000-0000-0000-0000000c05c3' and deleted_at is null"
q "insert into biz_cost_collect(cost_object_id,period_code,element,source_type,source_id,line_no,occur_date,material_id,qty,unit_price,amount,remark) values ('00000000-0000-0000-0000-0000000c05c2','2026-02','MATERIAL','STOCK_OUT','00000000-0000-0000-0000-0000000c05c3',1,date '2026-02-01',gen_random_uuid(),10,12.5,125,'V0371-SELFCHECK')" \
  && echo "→ 软删后重新归集：OK"
echo "→ 活行重号应报 unique_violation（期望失败）："
psql -d "$DB" -X -q -tAc "insert into biz_cost_collect(cost_object_id,period_code,element,source_type,source_id,line_no,occur_date,material_id,qty,unit_price,amount,remark) values ('00000000-0000-0000-0000-0000000c05c2','2026-02','MATERIAL','STOCK_OUT','00000000-0000-0000-0000-0000000c05c3',1,date '2026-02-01',gen_random_uuid(),10,12.5,125,'V0371-SELFCHECK')" 2>&1 | head -2
echo "→ 领料行不挂成本对象应报 check_violation（期望失败）："
psql -d "$DB" -X -q -tAc "insert into biz_cost_collect(period_code,element,source_type,occur_date,material_id,qty,amount,remark) values ('2026-02','MATERIAL','STOCK_OUT',date '2026-02-01',gen_random_uuid(),1,10,'V0371-SELFCHECK')" 2>&1 | head -2
q "insert into biz_cost_collect(period_code,element,source_type,occur_date,amount,dept_id,remark) values ('2026-02','OVERHEAD','DEPRECIATION',date '2026-02-28',88,gen_random_uuid(),'V0371-SELFCHECK')" \
  && echo "→ 车间费用池（折旧、不挂对象）允许：OK"
echo "→ 有归集行时删成本对象应被 RESTRICT 拦（期望失败）："
psql -d "$DB" -X -q -tAc "delete from biz_cost_object where id='00000000-0000-0000-0000-0000000c05c2'" 2>&1 | head -2

say "9. 清理演练行"
q "delete from biz_cost_collect where remark='V0371-SELFCHECK'"
q "delete from biz_cost_labor where remark='V0371-SELFCHECK'"
q "delete from biz_cost_object where object_no='DRILL-B2-CB'"
q "select 'leftover='||((select count(*) from biz_cost_collect)+(select count(*) from biz_cost_labor)+(select count(*) from biz_cost_overhead)+(select count(*) from biz_cost_rate)+(select count(*) from biz_cost_object where object_no like 'DRILL%'))"

say "10. down 回滚"
runf "$DOWN"

say "11. down 后零残留断言"
q "select 'collect='||coalesce(to_regclass('public.biz_cost_collect')::text,'NULL')||' labor='||coalesce(to_regclass('public.biz_cost_labor')::text,'NULL')||' overhead='||coalesce(to_regclass('public.biz_cost_overhead')::text,'NULL')||' rate='||coalesce(to_regclass('public.biz_cost_rate')::text,'NULL')||' cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')"
q "select 'idx_left='||count(*) from pg_indexes where schemaname='public' and (indexname like '%biz_cost_collect%' or indexname like '%biz_cost_labor%' or indexname like '%biz_cost_oh%' or indexname like '%biz_cost_rate%')"

say "12. down 再跑一次（幂等）"
runf "$DOWN"

say "13. 回滚后重新 up（可重放）"
runf "$UP"
q "select 'replay tables='||(select count(*) from pg_tables where schemaname='public' and tablename in ('biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate'))"

say "14. 下游护栏反证：下游表 FK 到本卡表时 down 必须拒绝"
q "create table zz_downstream (id uuid primary key, collect_id uuid references biz_cost_collect(id))"
echo "→ 期望 down 被拒："
psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$DOWN" 2>&1 | head -3
q "drop table zz_downstream"

say "15. 收尾（保持四表在位，供后续卡使用）"
q "select 'final tables='||(select count(*) from pg_tables where schemaname='public' and tablename in ('biz_cost_collect','biz_cost_labor','biz_cost_overhead','biz_cost_rate'))||' fks='||(select count(*) from pg_constraint where contype='f' and conrelid in ('biz_cost_collect'::regclass,'biz_cost_labor'::regclass,'biz_cost_overhead'::regclass) and confrelid='biz_cost_object'::regclass)"
echo "== DRILL END =="
