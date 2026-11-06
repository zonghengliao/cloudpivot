#!/usr/bin/env bash
# =============================================================================
# SO-32 价格两表 + 物料标准价：迁移验证（三阶段，全部真实执行）
# =============================================================================
#   bash scripts/verify/so32_price_migration.sh
#
# 阶段 1｜空库全量重放：新建 cloudpivot_so32（空库）→ 起隔离实例 → 迁移器把
#         V0270 连同全部历史迁移一起 APPLY_OK → 断言 schema_migrations 记录
#         （kind=versioned / applied_via=executed / success）与三张对象在位，
#         并用真实 INSERT 复验「唯一索引生效（同键第二条被拒）」。
# 阶段 2｜乱序补迁：从 cloudpivot_so31（已到 V0306、**没有** V0270）克隆一份
#         cloudpivot_so32_ooo → 起实例 → V0270 以「版本号低于已应用最大值」的
#         乱序补迁路径落库（日志有 乱序补迁 告警）→ 断言记录 appended 在末尾。
# 阶段 3｜回滚演习：down → up → 二次 up（幂等）在克隆库上跑，断言三张对象回来、
#         物料主数据行数一行未改、重复 up 不报错。
#
# 端口：18232（隔离段，18230=SO-30 / 18231=SO-31），不撞邻居卡。
# =============================================================================
set -uo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
EV=${EV:-$ROOT/docs/evidence/20261107-so32-price-migration}
DB1=${DB1:-cloudpivot_so32}
DB2=${DB2:-cloudpivot_so32_ooo}
DB3=${DB3:-cloudpivot_so32_drill}
SRC=${SRC:-cloudpivot_so31}
P1=${P1:-18232}
P2=${P2:-18233}
mkdir -p "$EV"
FAIL=0
ok()  { echo "  [OK]   $*"; }
bad() { echo "  [FAIL] $*"; FAIL=$((FAIL + 1)); }
chk() { # chk <说明> <实际> <期望>
  if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = [$2] 期望 [$3]"; fi
}
q() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$1" -tAc "$2" 2>&1; }
ins() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$1" -tAc "$2" 2>&1; }

upd() { # upd <db> <port>
  SO30_DB="$1" SO30_PORT="$2" SO30_LOG="/root/$(basename "$1")_server.log" \
    SO30_PIDF="/root/$(basename "$1")_server.pid" \
    bash "$ROOT/scripts/verify/so30_instance.sh" up
}
down() { # down <db> <port>
  SO30_DB="$1" SO30_PORT="$2" SO30_LOG="/root/$(basename "$1")_server.log" \
    SO30_PIDF="/root/$(basename "$1")_server.pid" \
    bash "$ROOT/scripts/verify/so30_instance.sh" down
}

echo "=============================================================="
echo "阶段 1｜空库全量重放 → $DB1 (:$P1)"
echo "=============================================================="
SO30_DB="$DB1" bash "$ROOT/scripts/verify/so30_db_setup.sh" || { echo "FATAL: 建库失败"; exit 1; }
upd "$DB1" "$P1" || { echo "FATAL: 实例未起"; exit 1; }
chk "health" "$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$P1/health")" "200"

chk "V0270 schema_migrations 记录(version|kind|applied_via|success)" \
    "$(q "$DB1" "select version||'|'||kind||'|'||applied_via||'|'||success from schema_migrations where name='sales_price' and kind='versioned'")" \
    "270|versioned|executed|true"
chk "versioned 迁移条数（历史 + 本卡）" \
    "$(q "$DB1" "select count(*) from schema_migrations where kind='versioned' and success")" \
    "$(ls "$ROOT/backend/migrations"/*/V*.up.sql | wc -l)"
chk "applied_via 非法值行数（只允许 executed/imported）" \
    "$(q "$DB1" "select count(*) from schema_migrations where applied_via not in ('executed','imported')")" "0"
chk "本卡迁移按版本号次序落在 V0267 与 V0280 之间（空库重放排序正确）" \
    "$(q "$DB1" "select (applied_seq > (select applied_seq from schema_migrations where version='267') and applied_seq < (select applied_seq from schema_migrations where version='280'))::text from schema_migrations where name='sales_price'")" "true"

chk "biz_customer_price 列数" "$(q "$DB1" "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_customer_price'")" "14"
chk "biz_price_tier 列数"     "$(q "$DB1" "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_price_tier'")" "16"
chk "biz_material.standard_price 类型" \
    "$(q "$DB1" "select data_type||'('||numeric_precision||','||numeric_scale||')' from information_schema.columns where table_schema='public' and table_name='biz_material' and column_name='standard_price'")" \
    "numeric(18,4)"
chk "uk_biz_cust_price 是存活行唯一索引" \
    "$(q "$DB1" "select (indexdef like '%UNIQUE%' and indexdef like '%deleted_at IS NULL%')::text from pg_indexes where schemaname='public' and indexname='uk_biz_cust_price'")" "true"
chk "uk_biz_price_tier 带 NULLS NOT DISTINCT" \
    "$(q "$DB1" "select (indexdef like '%NULLS NOT DISTINCT%')::text from pg_indexes where schemaname='public' and indexname='uk_biz_price_tier'")" "true"

# 唯一索引行为复验（真实 INSERT）：同一 (客户,物料,起订量,生效日) 第二条必须被拒
CUS='f0320000-0000-4000-8000-000000000001'
MAT='f0320000-0000-4000-8000-000000000002'
ins "$DB1" "insert into biz_customer (id, code, name) values ('$CUS','__SO32_EV_CUS__','SO32 证据客户')" >/dev/null
ins "$DB1" "insert into biz_material (id, material_code, material_name, unit_id) values ('$MAT','__SO32_EV_MAT__','SO32 证据物料', gen_random_uuid())" >/dev/null
r1=$(ins "$DB1" "insert into biz_customer_price (customer_id, material_id, price) values ('$CUS','$MAT',88.0000) returning 'inserted'" | head -1)
r2=$(ins "$DB1" "insert into biz_customer_price (customer_id, material_id, price) values ('$CUS','$MAT',99.0000) returning 'inserted'" | head -1)
chk "第一条协议价插入" "$r1" "inserted"
case "$r2" in
  *uk_biz_cust_price*unique*|*duplicate\ key*uk_biz_cust_price*) ok "同键第二条被 uk_biz_cust_price 拒绝（唯一索引生效）: ${r2%%$'\n'*}" ;;
  *) bad "同键第二条未被拒绝（实际返回: $r2）" ;;
esac
r3=$(ins "$DB1" "insert into biz_customer_price (customer_id, material_id, price) values ('$CUS','$MAT',0) returning 'inserted'" | head -1)
case "$r3" in
  *ck_biz_cust_price_price*) ok "价格 = 0 被 ck_biz_cust_price_price 拒绝（价格必须 > 0）" ;;
  *) bad "价格 = 0 未被拒绝（实际返回: $r3）" ;;
esac
r4=$(ins "$DB1" "update biz_material set standard_price = 0 where id = '$MAT' returning 'updated'" | head -1)
case "$r4" in
  *ck_biz_material_standard_price*) ok "standard_price = 0 被 CHECK 拒绝" ;;
  *) bad "standard_price = 0 未被拒绝（实际返回: $r4）" ;;
esac
# 清场
ins "$DB1" "delete from biz_customer_price where customer_id='$CUS'" >/dev/null
ins "$DB1" "delete from biz_price_tier where material_id='$MAT'" >/dev/null
ins "$DB1" "delete from biz_material where id='$MAT'" >/dev/null
ins "$DB1" "delete from biz_customer where id='$CUS'" >/dev/null
chk "证据探针清场后残留" \
    "$(q "$DB1" "select (select count(*) from biz_customer_price where customer_id='$CUS' or material_id='$MAT') + (select count(*) from biz_customer where id='$CUS') + (select count(*) from biz_material where id='$MAT')")" "0"

down "$DB1" "$P1" >/dev/null

echo
echo "=============================================================="
echo "阶段 2｜乱序补迁 → $DB2（克隆 $SRC，已到 V0306 且无 V0270）:$P2"
echo "=============================================================="
chk "克隆源 $SRC 里 V0270 应不存在" \
    "$(q "$SRC" "select count(*) from schema_migrations where name='sales_price'")" "0"
chk "克隆源 $SRC 最大版本" \
    "$(q "$SRC" "select max(version::int) from schema_migrations where kind='versioned' and version ~ '^[0-9]+$'")" "306"
su - postgres -c "dropdb --if-exists $DB2" >/dev/null 2>&1
su - postgres -c "createdb -O alun -T $SRC $DB2" || { echo "FATAL: 克隆失败"; exit 1; }
upd "$DB2" "$P2" || { echo "FATAL: 实例未起"; exit 1; }
chk "health" "$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$P2/health")" "200"
chk "V0270 在存量库上补迁成功" \
    "$(q "$DB2" "select version||'|'||applied_via||'|'||success from schema_migrations where name='sales_price'")" \
    "270|executed|true"
chk "补迁落在已应用最大值之后（applied_seq 递增，不重排历史）" \
    "$(q "$DB2" "select (applied_seq = (select max(applied_seq) from schema_migrations where kind='versioned'))::text from schema_migrations where name='sales_price'")" "true"
chk "历史 V0306 记录仍在" \
    "$(q "$DB2" "select count(*) from schema_migrations where version='306' and success")" "1"
chk "补迁前 max=306，补迁后 V0270 仍存在（号段 270 未被拒）" \
    "$(q "$DB2" "select count(*) from schema_migrations where version='270'")" "1"
if grep -q '乱序补迁' "/root/$(basename "$DB2")_server.log"; then
  ok "启动日志含「乱序补迁」告警（alun 对低版本补迁只告警不拒绝）"
  grep -o 'migration=sales_price[^"]*' "/root/$(basename "$DB2")_server.log" | head -2 >&2 || true
else
  bad "启动日志未见「乱序补迁」告警（补迁路径未被走到？）"
fi
chk "存量库上三张对象在位" \
    "$(q "$DB2" "select (to_regclass('public.biz_customer_price') is not null and to_regclass('public.biz_price_tier') is not null and exists (select 1 from information_schema.columns where table_name='biz_material' and column_name='standard_price'))::text")" "true"
down "$DB2" "$P2" >/dev/null

echo
echo "=============================================================="
echo "阶段 3｜回滚演习 down → up → 二次 up（库 $DB3，克隆 $DB2）"
echo "=============================================================="
su - postgres -c "dropdb --if-exists $DB3" >/dev/null 2>&1
su - postgres -c "createdb -O alun -T $DB2 $DB3" || { echo "FATAL: 克隆失败"; exit 1; }
MAT_CNT=$(q "$DB3" "select count(*) from biz_material")
MIG_SQL="$ROOT/backend/migrations/business"
run_sql() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB3" -v ON_ERROR_STOP=1 -f "$1" 2>&1; }
out=$(run_sql "$MIG_SQL/V0270__sales_price.down.sql"); rc=$?
chk "down 退出码" "$rc" "0"
chk "down 后两张表是否消失" "$(q "$DB3" "select (to_regclass('public.biz_customer_price') is null and to_regclass('public.biz_price_tier') is null)::text")" "true"
chk "down 后 standard_price 列是否消失" "$(q "$DB3" "select count(*) from information_schema.columns where table_name='biz_material' and column_name='standard_price'")" "0"
chk "down 不改物料主数据行数" "$(q "$DB3" "select count(*) from biz_material")" "$MAT_CNT"
out=$(run_sql "$MIG_SQL/V0270__sales_price.down.sql"); rc=$?
chk "down 二次执行（幂等）退出码" "$rc" "0"
out=$(run_sql "$MIG_SQL/V0270__sales_price.up.sql"); rc=$?
chk "down→up 退出码" "$rc" "0"
chk "down→up 后表回来" "$(q "$DB3" "select (to_regclass('public.biz_customer_price') is not null)::text")" "true"
out=$(run_sql "$MIG_SQL/V0270__sales_price.up.sql"); rc=$?
chk "二次 up（幂等，不再要求列/索引重建）退出码" "$rc" "0"
chk "二次 up 后 schema_migrations 记录未被复制" \
    "$(q "$DB3" "select count(*) from schema_migrations where name='sales_price'")" "1"
chk "二次 up 后物料主数据行数未变" "$(q "$DB3" "select count(*) from biz_material")" "$MAT_CNT"

echo
echo "=============================================================="
if [ "$FAIL" -eq 0 ]; then
  echo "结论：全部断言通过（FAIL=0）"
else
  echo "结论：$FAIL 条断言失败"
fi
echo "=============================================================="
exit "$FAIL"
