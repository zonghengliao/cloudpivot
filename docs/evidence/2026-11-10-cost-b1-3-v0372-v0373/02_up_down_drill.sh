#!/usr/bin/env bash
# ============================================================================
# V0372（分摊两表）+ V0373（期间两表）真库往返演练 —— COST-B1-3 实测证据
#
# 用法：bash 02_up_down_drill.sh <db> <outfile>
#   <db> 需已跑过 V0370/V0371（biz_cost_object / biz_cost_collect 存在），
#        例如空库全链重放过一次的业务库（scripts/verify/mig_replay_fresh.sh）
#
# 覆盖：up×3 幂等 / ★唯一键排除 REVERSED 的活体验证（卡面验收）/
#       外键删除动作 / 精度快照 / down×2 幂等 / 回滚后重放 / 下游 FK 护栏反证
# ============================================================================
set -uo pipefail
DB="${1:?db}"
OUT="${2:?out}"
ROOT=/opt/cloudpivot
UP1="$ROOT/backend/migrations/business/V0372__cost_alloc_tables.up.sql"
DN1="$ROOT/backend/migrations/business/V0372__cost_alloc_tables.down.sql"
UP2="$ROOT/backend/migrations/business/V0373__cost_period_tables.up.sql"
DN2="$ROOT/backend/migrations/business/V0373__cost_period_tables.down.sql"
export PGPASSWORD=alun
export PGHOST=127.0.0.1 PGPORT=5432 PGUSER=alun

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
q "select 'biz_cost_collect='||coalesce(to_regclass('public.biz_cost_collect')::text,'NULL')||'（V0371 前置）'"
# 清掉上一次演练可能残留（本脚本可重复执行）
q "delete from biz_cost_period_item where cost_object_id in (select id from biz_cost_object where object_no like 'DRILL-B3-%' or object_no like 'SELFCHECK-B3-%')" >/dev/null
q "delete from biz_cost_period where batch_no like 'DRILL-B3-%' or batch_no like 'SELFCHECK-B3-%'" >/dev/null
q "delete from biz_cost_alloc_result where cost_object_id in (select id from biz_cost_object where object_no like 'DRILL-B3-%' or object_no like 'SELFCHECK-B3-%')" >/dev/null
q "delete from biz_cost_alloc_rule where rule_code like 'DRILL-B3-%' or rule_code like 'SELFCHECK-B3-%'" >/dev/null
q "delete from biz_cost_object where object_no like 'DRILL-B3-%' or object_no like 'SELFCHECK-B3-%'" >/dev/null

say "1. 首轮 up（V0372 分摊两表 → V0373 期间两表）"
runf "$UP1"
runf "$UP2"

say "2. 第二、三轮 up（重复执行必须结果不变）"
runf "$UP1"
runf "$UP2"
runf "$UP1"
runf "$UP2"

say "3. 幂等证据：表 / 索引 / 约束数量 + 探针零残留"
q "select 'tables='||count(*)||'（期望 4）' from pg_tables where schemaname='public' and tablename in ('biz_cost_alloc_rule','biz_cost_alloc_result','biz_cost_period','biz_cost_period_item')"
q "select 'idx='||count(*)||'（期望 13 = 9 业务索引 + 4 PK）' from pg_indexes where schemaname='public' and tablename in ('biz_cost_alloc_rule','biz_cost_alloc_result','biz_cost_period','biz_cost_period_item')"
q "select 'checks='||count(*)||'（期望 24 = 5+3+9+7）' from pg_constraint where contype='c' and conrelid in ('biz_cost_alloc_rule'::regclass,'biz_cost_alloc_result'::regclass,'biz_cost_period'::regclass,'biz_cost_period_item'::regclass)"
q "select 'fks='||count(*)||'（期望 4 = 2 分摊 + 2 期间）' from pg_constraint where contype='f' and conrelid in ('biz_cost_alloc_rule'::regclass,'biz_cost_alloc_result'::regclass,'biz_cost_period'::regclass,'biz_cost_period_item'::regclass)"
q "select 'probe_leftover='||((select count(*) from biz_cost_alloc_rule where rule_code like 'SELFCHECK-B3-%')+(select count(*) from biz_cost_period where batch_no like 'SELFCHECK-B3-%')+(select count(*) from biz_cost_object where object_no like 'SELFCHECK-B3-%')+(select count(*) from biz_cost_alloc_result r join biz_cost_object o on o.id=r.cost_object_id where o.object_no like 'SELFCHECK-B3-%'))||'（期望 0）'"

say "4. ★ 快照：唯一索引谓词（uk_biz_cost_period 必须排除 REVERSED/CANCELLED）"
q "select indexname||' :: '||indexdef from pg_indexes where schemaname='public' and indexname in ('uk_biz_cost_rule_code','uk_biz_cost_alloc_result','uk_biz_cost_period','uk_biz_cost_period_no','uk_biz_cost_item') order by 1"

say "5. ★ 卡面验收活体验证：同期间一条 REVERSED + 一条 SETTLED → 不冲突"
q "insert into biz_cost_object(id,object_no,product_id,target_warehouse_id,plan_qty,workshop_dept_id,workshop_dept_name,start_date) values ('00000000-0000-0000-0000-0000000c0b31','DRILL-B3-OBJ',gen_random_uuid(),gen_random_uuid(),10,gen_random_uuid(),'演练车间',date '2026-01-01')" \
  && echo "  → 探针成本对象已建"
q "insert into biz_cost_period(id,period_code,batch_no,status,approval_instance_id,settled_at,finish_voucher_id,reversed_at,reverse_reason,remark) values ('00000000-0000-0000-0000-0000000c0b32','2026-01','DRILL-B3-CS-0101','REVERSED','00000000-0000-0000-0000-0000000c0b3a',now(),'00000000-0000-0000-0000-0000000c0b3b',now(),'演练红冲','DRILL-B3')" \
  && echo "  → ① REVERSED 行（2026-01）插入成功"
q "insert into biz_cost_period(id,period_code,batch_no,status,approval_instance_id,settled_at,finish_voucher_id,remark) values ('00000000-0000-0000-0000-0000000c0b33','2026-01','DRILL-B3-CS-0102','SETTLED','00000000-0000-0000-0000-0000000c0b3a',now(),'00000000-0000-0000-0000-0000000c0b3b','DRILL-B3')" \
  && echo "  → ② 同期间 SETTLED 行插入成功 ⇒ 不冲突（验收通过）"
q "select 'period_2026_01_rows='||count(*)||'（期望 2：一条 REVERSED + 一条 SETTLED）' from biz_cost_period where period_code='2026-01' and batch_no like 'DRILL-B3-%'"
expect_fail "同期间第三条**活跃**批次（唯一键必须拦，否则同月双结算）" \
  "insert into biz_cost_period(period_code,batch_no,status,remark) values ('2026-01','DRILL-B3-CS-0103','DRAFT','DRILL-B3')"
q "insert into biz_cost_period(period_code,batch_no,status,approval_instance_id,settled_at,finish_voucher_id,reversed_at,reverse_reason,remark) values ('2026-01','DRILL-B3-CS-0104','REVERSED','00000000-0000-0000-0000-0000000c0b3a',now(),'00000000-0000-0000-0000-0000000c0b3b',now(),'演练二次红冲','DRILL-B3')" \
  && echo "  → ③ 同期间第二条 REVERSED 插入成功（红冲留痕可多条）"
q "select 'period_2026_01_rows_now='||count(*)||'（期望 3 = 2 REVERSED + 1 SETTLED）' from biz_cost_period where period_code='2026-01' and batch_no like 'DRILL-B3-%'"
echo "→ 期望失败：SETTLED 不带 settled_at / finish_voucher_id（假结算死单防线）"
psql -d "$DB" -X -q -tAc "insert into biz_cost_period(period_code,batch_no,status,approval_instance_id,remark) values ('2026-05','DRILL-B3-CS-0501','SETTLED','00000000-0000-0000-0000-0000000c0b3a','DRILL-B3')" 2>&1 | head -2

say "6. 快照：外键删除动作（period_item→period 必须 c=CASCADE；→cost_object 必须 r=RESTRICT）"
q "select conrelid::regclass||' -> '||confrelid::regclass||' confdeltype='||confdeltype::text from pg_constraint where contype='f' and conrelid in ('biz_cost_alloc_rule'::regclass,'biz_cost_alloc_result'::regclass,'biz_cost_period'::regclass,'biz_cost_period_item'::regclass) order by 1"

say "7. 快照：金额 / 数量列精度（设计 §3.1：18,4；单价费率 18,6；完工进度 5,2）"
q "select table_name||'.'||column_name||' = '||data_type||'('||numeric_precision||','||numeric_scale||')' from information_schema.columns where table_schema='public' and table_name in ('biz_cost_alloc_rule','biz_cost_alloc_result','biz_cost_period','biz_cost_period_item') and data_type='numeric' order by 1"

say "8. 业务语义验真：分摊结果唯一 / 基数 0 不许摊钱 / 计算单成本守恒 / 批次删除级联"
q "insert into biz_cost_alloc_rule(id,rule_code,rule_name,source_dept_id,source_dept_name,account_code,basis,effective_from) values ('00000000-0000-0000-0000-0000000c0b34','DRILL-B3-R1','演练分摊规则',gen_random_uuid(),'演练车间','4101.01','LABOR_HOURS',date '2026-01-01')" && echo "  → 分摊规则建成功"
q "insert into biz_cost_alloc_result(period_code,rule_id,cost_object_id,dept_id,dept_name,account_code,basis_value,basis_total,pool_amount,alloc_amount) values ('2026-01','00000000-0000-0000-0000-0000000c0b34','00000000-0000-0000-0000-0000000c0b31',gen_random_uuid(),'演练车间','4101.01',60,100,1000,600)" && echo "  → 分摊结果插入成功"
expect_fail "同 (期间,规则,对象,科目) 第二条分摊结果" \
  "insert into biz_cost_alloc_result(period_code,rule_id,cost_object_id,dept_id,dept_name,account_code,basis_value,basis_total,pool_amount,alloc_amount) values ('2026-01','00000000-0000-0000-0000-0000000c0b34','00000000-0000-0000-0000-0000000c0b31',gen_random_uuid(),'演练车间','4101.01',60,100,1000,600)"
expect_fail "basis_total=0 却摊 100 元（凭空分摊）" \
  "insert into biz_cost_alloc_result(period_code,rule_id,cost_object_id,dept_id,dept_name,account_code,basis_value,basis_total,pool_amount,alloc_amount) values ('2026-01','00000000-0000-0000-0000-0000000c0b34','00000000-0000-0000-0000-0000000c0b31',gen_random_uuid(),'演练车间','4101.02',0,0,1000,100)"
q "insert into biz_cost_period_item(period_id,cost_object_id,product_id,completed_qty,wip_beg_cost,mat_cur,labor_cur,oh_cur,finished_cost,wip_end_cost,completion_pct,equiv_material,equiv_conversion,unit_material,unit_labor,unit_overhead,unit_finished_cost) values ('00000000-0000-0000-0000-0000000c0b33','00000000-0000-0000-0000-0000000c0b31',gen_random_uuid(),10,0,100,60,40,150,50,50,12,10,10,6,4,12.5)" \
  && echo "  → 计算单（料100+工60+费40=200=完工150+在制50）插入成功：守恒通过"
expect_fail "计算单不守恒（完工 150 + 在制 40 ≠ 200）" \
  "insert into biz_cost_period_item(period_id,cost_object_id,product_id,wip_beg_cost,mat_cur,labor_cur,oh_cur,finished_cost,wip_end_cost) values ('00000000-0000-0000-0000-0000000c0b32','00000000-0000-0000-0000-0000000c0b31',gen_random_uuid(),0,100,60,40,150,40)"
q "delete from biz_cost_period where id='00000000-0000-0000-0000-0000000c0b33'" \
  && echo "  → 删除批次（CASCADE 期望：计算单跟着走）"
q "select 'items_after_cascade='||count(*)||'（期望 0）' from biz_cost_period_item where cost_object_id='00000000-0000-0000-0000-0000000c0b31'"
echo "→ 期望失败：有分摊结果时删成本对象（RESTRICT）"
psql -d "$DB" -X -q -tAc "delete from biz_cost_object where id='00000000-0000-0000-0000-0000000c0b31'" 2>&1 | head -2

say "9. 清理演练行"
q "delete from biz_cost_alloc_result where cost_object_id='00000000-0000-0000-0000-0000000c0b31'" >/dev/null
q "delete from biz_cost_period_item where cost_object_id='00000000-0000-0000-0000-0000000c0b31'" >/dev/null
q "delete from biz_cost_period where batch_no like 'DRILL-B3-%'" >/dev/null
q "delete from biz_cost_alloc_rule where rule_code like 'DRILL-B3-%'" >/dev/null
q "delete from biz_cost_object where object_no like 'DRILL-B3-%'" >/dev/null
q "select 'leftover='||((select count(*) from biz_cost_alloc_rule)+(select count(*) from biz_cost_alloc_result)+(select count(*) from biz_cost_period)+(select count(*) from biz_cost_period_item)+(select count(*) from biz_cost_object where object_no like 'DRILL%'))||'（期望 0）'"

say "10. down 回滚（V0373 → V0372，逆序）"
runf "$DN2"
runf "$DN1"

say "11. down 后零残留断言"
q "select 'period='||coalesce(to_regclass('public.biz_cost_period')::text,'NULL')||' period_item='||coalesce(to_regclass('public.biz_cost_period_item')::text,'NULL')||' alloc_rule='||coalesce(to_regclass('public.biz_cost_alloc_rule')::text,'NULL')||' alloc_result='||coalesce(to_regclass('public.biz_cost_alloc_result')::text,'NULL')||' cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')||'（最后一项必须还在）'"
q "select 'idx_left='||count(*)||'（期望 0）' from pg_indexes where schemaname='public' and (indexname like '%biz_cost_alloc%' or indexname like '%biz_cost_period%' or indexname like '%biz_cost_item%')"

say "12. down 再跑一次（幂等）"
runf "$DN2"
runf "$DN1"

say "13. 回滚后重新 up（可重放）"
runf "$UP1"
runf "$UP2"
q "select 'replay tables='||count(*)||'（期望 4）' from pg_tables where schemaname='public' and tablename in ('biz_cost_alloc_rule','biz_cost_alloc_result','biz_cost_period','biz_cost_period_item')"

say "14. 下游护栏反证：外部表 FK 到期间表时 down 必须拒绝执行"
q "create table zz_downstream (id uuid primary key, period_id uuid references biz_cost_period(id))"
echo "→ 期望 V0373 down 被拒（提示先回滚下游迁移）："
psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$DN2" 2>&1 | head -3
q "drop table zz_downstream"

say "15. 收尾（保持四表在位，供后续卡使用）"
q "select 'final tables='||count(*) from pg_tables where schemaname='public' and tablename in ('biz_cost_alloc_rule','biz_cost_alloc_result','biz_cost_period','biz_cost_period_item')"
echo "== DRILL END =="
