#!/usr/bin/env bash
# V0295（SO-19 销售发票）回滚演练：克隆 → down → 断言 → up → 断言 → 第三轮探针（psql 直灌路径）
# 用法：bash rollback-drill.sh
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
SRC=${SRC:-cloudpivot_so19}
DB=${DB:-cloudpivot_so19_drill}
export PGPASSWORD=${PGPASSWORD:-alun}
PSQL="psql -h 127.0.0.1 -U alun -v ON_ERROR_STOP=1"
EV="$REPO/docs/evidence/20261106-so19-invoice"

q() { $PSQL -d "$1" -Atc "$2"; }
state() {
  local db=$1 tag=$2
  echo "$tag table=$(q "$db" "select coalesce(to_regclass('public.biz_invoice')::text,'<null>')") \
inv_seq=$(q "$db" "select count(*) from biz_seq where seq_type='IV'") \
checks=$(q "$db" "select count(*) from pg_constraint where conrelid=to_regclass('public.biz_invoice') and contype='c'") \
idx=$(q "$db" "select count(*) from pg_indexes where tablename='biz_invoice'") \
receivable=$(q "$db" "select coalesce(to_regclass('public.biz_receivable')::text,'<null>')") \
other_seq=$(q "$db" "select string_agg(seq_type,',' order by seq_type) from biz_seq where deleted_at is null")"
}

echo "===== [0] 准备：从 $SRC 克隆出 $DB（模板克隆，不动源库）====="
su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
su - postgres -c "createdb -O alun -T $SRC $DB" || { echo "克隆失败（源库还有连接？）"; exit 1; }
state "$DB" "[0] 演练前"

echo
echo "===== [1] 执行 down.sql ====="
$PSQL -d "$DB" -f "$REPO/backend/migrations/business/V0295__sales_invoice.down.sql" 2>&1 | grep -E "NOTICE|WARNING|ERROR" || true
state "$DB" "[1] down 后"

echo
echo "===== [2] 重放 up.sql（幂等 + 自检第二次跑）====="
$PSQL -d "$DB" -f "$REPO/backend/migrations/business/V0295__sales_invoice.up.sql" 2>&1 | grep -E "V0295 自检|ERROR" || true
state "$DB" "[2] up 后"

echo
echo "===== [3] 第三轮探针（psql 直灌路径的库）====="
$PSQL -d "$DB" -f "$EV/constraint-probes.sql" 2>&1 | tail -3
echo "[3] 探针后残留：invoice=$(q "$DB" "select count(*) from biz_invoice") receivable_probe=$(q "$DB" "select count(*) from biz_receivable where ar_no like 'AR-PROBE%'")"

echo
echo "===== [4] 幂等重放：up.sql 再跑一遍（DDL 全 already exists）====="
$PSQL -d "$DB" -f "$REPO/backend/migrations/business/V0295__sales_invoice.up.sql" 2>&1 | grep -E "V0295 自检|ERROR|already exists" | tail -3 || true
state "$DB" "[4] 二次重放后"

echo
echo "===== [5] 清理演练库 ====="
su - postgres -c "dropdb --force $DB" && echo "dropped: $DB"
