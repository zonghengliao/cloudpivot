#!/usr/bin/env bash
# V0292 回滚演练：down → 断言 → up → 断言 → 探针（第三轮，psql 直灌路径）
# 用法：bash rollback-drill.sh
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
SRC=${SRC:-cloudpivot_so16b}
DB=${DB:-cloudpivot_so16_drill}
export PGPASSWORD=${PGPASSWORD:-alun}
PSQL="psql -h 127.0.0.1 -U alun -v ON_ERROR_STOP=1"

q() { $PSQL -d "$1" -Atc "$2"; }

echo "===== [0] 准备：从 $SRC 克隆出 $DB（模板克隆，不动源库）====="
su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
su - postgres -c "createdb -O alun -T $SRC $DB" || { echo "克隆失败（源库还有连接？）"; exit 1; }
echo "[0] 演练前状态：table=$(q "$DB" "select to_regclass('public.biz_receipt')" | tr -d ' ') alloc=$(q "$DB" "select to_regclass('public.biz_receipt_allocation')" | tr -d ' ') rc_seq=$(q "$DB" "select count(*) from biz_seq where seq_type='RC'") checks=$(q "$DB" "select count(*) from pg_constraint where conrelid in ('public.biz_receipt'::regclass,'public.biz_receipt_allocation'::regclass) and contype='c'") idx=$(q "$DB" "select count(*) from pg_indexes where tablename in ('biz_receipt','biz_receipt_allocation')")"

echo
echo "===== [1] 执行 down.sql ====="
$PSQL -d "$DB" -f "$REPO/backend/migrations/business/V0292__sales_receipt.down.sql" 2>&1 | grep -E "NOTICE|WARNING|ERROR" || true
echo "[1] down 后状态：receipt=$(q "$DB" "select coalesce(to_regclass('public.biz_receipt')::text,'<null>')") alloc=$(q "$DB" "select coalesce(to_regclass('public.biz_receipt_allocation')::text,'<null>')") rc_seq=$(q "$DB" "select count(*) from biz_seq where seq_type='RC'") receivable=$(q "$DB" "select coalesce(to_regclass('public.biz_receivable')::text,'<null>')") other_seq=$(q "$DB" "select string_agg(seq_type,',' order by seq_type) from biz_seq where deleted_at is null")"

echo
echo "===== [2] 重放 up.sql（幂等 + 自检第二次跑）====="
$PSQL -d "$DB" -f "$REPO/backend/migrations/business/V0292__sales_receipt.up.sql" 2>&1 | grep -E "V0292 自检|ERROR" || true
echo "[2] up 后状态：table=$(q "$DB" "select to_regclass('public.biz_receipt')" | tr -d ' ') alloc=$(q "$DB" "select to_regclass('public.biz_receipt_allocation')" | tr -d ' ') rc_seq=$(q "$DB" "select count(*) from biz_seq where seq_type='RC'") checks=$(q "$DB" "select count(*) from pg_constraint where conrelid in ('public.biz_receipt'::regclass,'public.biz_receipt_allocation'::regclass) and contype='c'") idx=$(q "$DB" "select count(*) from pg_indexes where tablename in ('biz_receipt','biz_receipt_allocation')")"

echo
echo "===== [3] 第三轮探针（psql 直灌路径的库）====="
$PSQL -d "$DB" -f "$REPO/docs/evidence/20261106-so16-receipt/constraint-probes.sql" 2>&1 | grep -cE "PASS" | sed 's/^/[3] PASS 行数 = /'
echo "[3] 探针后残留：receipt=$(q "$DB" "select count(*) from biz_receipt") alloc=$(q "$DB" "select count(*) from biz_receipt_allocation")"
