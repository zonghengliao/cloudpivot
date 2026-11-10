#!/usr/bin/env bash
# V0370 真库往返演练（本卡实测证据）
# 用法：bash cost_b1_1_drill.sh <db> <outfile>
set -uo pipefail
DB="${1:?db}"
OUT="${2:?out}"
ROOT=/opt/cloudpivot
UP="$ROOT/backend/migrations/business/V0370__cost_lot_and_cost_object.up.sql"
DOWN="$ROOT/backend/migrations/business/V0370__cost_lot_and_cost_object.down.sql"
export PGPASSWORD=alun
export PGHOST=127.0.0.1 PGPORT=5432 PGUSER=alun

say() { printf '\n===== %s =====\n' "$*"; }
q()   { psql -d "$DB" -X -q -v ON_ERROR_STOP=1 -tAc "$1"; }
runf(){ psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; echo "exit=$?"; }

exec > >(tee "$OUT") 2>&1

say "0. 环境"
echo "db=$DB"; psql -d "$DB" -tAc "select version()" | cut -c1-40

say "1. 首轮 up（迁移引擎已在本库跑过一次）→ 再跑一次验幂等"
runf "$UP"

say "2. 第三轮 up（连跑两次，结果不变）"
runf "$UP"

say "3. 幂等证据：探针行零残留 + 表/索引/约束数量不变"
q "select 'biz_lot_rows='||count(*) from biz_lot"
q "select 'biz_cost_object_rows='||count(*) from biz_cost_object"
q "select 'idx_biz_lot='||count(*) from pg_indexes where schemaname='public' and tablename='biz_lot'"
q "select 'idx_biz_cost_object='||count(*) from pg_indexes where schemaname='public' and tablename='biz_cost_object'"
q "select 'checks='||count(*) from pg_constraint where conrelid in ('biz_lot'::regclass,'biz_cost_object'::regclass) and contype='c'"

say "4. 快照：唯一索引谓词"
q "select indexname||' :: '||indexdef from pg_indexes where schemaname='public' and indexname in ('uk_biz_lot_material_no','uk_biz_cost_object_no')"

say "5. 快照：CHECK 约束清单"
q "select conrelid::regclass||'.'||conname from pg_constraint where conrelid in ('biz_lot'::regclass,'biz_cost_object'::regclass) and contype='c' order by 1"

say "6. 业务语义验真：软删后同批号可重建 / 活行重号被拦 / 关批无时间戳被拦"
q "insert into biz_lot(lot_no,material_id) values ('DRILL-L1','11111111-1111-1111-1111-111111111111')"
q "update biz_lot set deleted_at=now() where lot_no='DRILL-L1'"
q "insert into biz_lot(lot_no,material_id) values ('DRILL-L1','11111111-1111-1111-1111-111111111111')"
echo "→ 软删后重建：OK（上面第二条 insert 未报错）"
echo "→ 活行重号应报 unique_violation（下面这条期望失败）："
psql -d "$DB" -X -q -tAc "insert into biz_lot(lot_no,material_id) values ('DRILL-L1','11111111-1111-1111-1111-111111111111')" 2>&1 | head -2
echo "→ 关批无 closed_at 应报 check_violation（期望失败）："
psql -d "$DB" -X -q -tAc "insert into biz_cost_object(object_no,product_id,target_warehouse_id,plan_qty,workshop_dept_id,workshop_dept_name,start_date,status) values ('DRILL-CB-X','11111111-1111-1111-1111-111111111111','22222222-2222-2222-2222-222222222222',10,'33333333-3333-3333-3333-333333333333','演练车间','2026-01-01','CLOSED')" 2>&1 | head -2
echo "→ 清理演练行"
q "delete from biz_cost_object where product_id='11111111-1111-1111-1111-111111111111'"
q "delete from biz_lot where material_id='11111111-1111-1111-1111-111111111111'"
q "select 'leftover='||(select count(*) from biz_lot)+(select count(*) from biz_cost_object)"

say "7. down 回滚"
runf "$DOWN"

say "8. down 后零残留断言"
q "select 'biz_lot='||coalesce(to_regclass('public.biz_lot')::text,'NULL')||' biz_cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')"
q "select 'idx_left='||count(*) from pg_indexes where schemaname='public' and (tablename in ('biz_lot','biz_cost_object') or indexname like 'uk_biz_lot%' or indexname like 'uk_biz_cost_object%' or indexname like 'idx_biz_lot%' or indexname like 'idx_biz_cost_object%')"

say "9. down 再跑一次（幂等）"
runf "$DOWN"

say "10. 回滚后重新 up（可重放）"
runf "$UP"
q "select 'biz_lot='||coalesce(to_regclass('public.biz_lot')::text,'NULL')||' biz_cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')"

say "11. 下游护栏反证（V0371 起表存在时 down 必须拒绝）"
q "create table if not exists biz_cost_collect(id uuid primary key)"
echo "→ 期望 down 被拒："
psql -d "$DB" -X -q -v ON_ERROR_STOP=1 --single-transaction -f "$DOWN" 2>&1 | head -3
q "drop table if exists biz_cost_collect"

say "12. 收尾"
q "select 'final biz_lot='||coalesce(to_regclass('public.biz_lot')::text,'NULL')||' final biz_cost_object='||coalesce(to_regclass('public.biz_cost_object')::text,'NULL')"
echo "== DRILL END =="
