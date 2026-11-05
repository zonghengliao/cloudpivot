#!/usr/bin/env bash
# 收口D 验收：144 生产只读复跑（不写库；PGOPTIONS 强制 default_transaction_read_only=on）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
export PGOPTIONS='-c default_transaction_read_only=on'
cd /opt/cloudpivot || exit 9
OUT=/tmp/d4-out; mkdir -p "$OUT"
PSQL="psql -X -q -h 127.0.0.1 -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME"

echo "########## 0. 环境与代码版本 ##########"
hostname; date '+%F %T %Z'; git log --oneline -1
git status --short -- scripts/ docs/architecture/ | head -5
echo "-- 巡检视图数量 --"
$PSQL -tAc "select 'c_di 视图数='||count(*) from pg_views where schemaname='di' and viewname like 'c_di%';" 2>&1
echo "-- DI-18 豁免台账 --"
$PSQL -c "select check_id, src_table, src_id, checked_on, expires_on, (expires_on > current_date) as active from di.v_poly_ref_waiver;" 2>&1
echo "-- 视图定义关键子串（确认线上口径 = 仓库 HEAD）--"
$PSQL -tAc "select 'c_di18 含已核查豁免#: '||(position('已核查豁免#' in pg_get_viewdef('di.c_di18'::regclass))>0)::text||' ; c_di18 含 CURRENT_DATE: '||(position('CURRENT_DATE' in pg_get_viewdef('di.c_di18'::regclass))>0)::text||' ; c_di19 含 PENDING: '||(position('PENDING' in pg_get_viewdef('di.c_di19'::regclass))>0)::text;" 2>&1

echo; echo "########## 1. 全量巡检（人读格式，只读）##########"
python3 scripts/verify/snapshot_audit.py --log-file "$OUT/audit.log" > "$OUT/01-full.txt" 2>&1; echo "exit=$?"
tail -6 "$OUT/01-full.txt"

echo; echo "########## 2. 多态引用组（卡面命令口径，只读）##########"
python3 scripts/verify/snapshot_audit.py --group 多态引用 --log-file "$OUT/audit.log" > "$OUT/02-polyref.txt" 2>&1; echo "exit=$?"
tail -6 "$OUT/02-polyref.txt"

echo; echo "########## 3. JSON 明细（供逐条 diff 用）##########"
python3 scripts/verify/snapshot_audit.py --json --out "$OUT/03-full.json" --log-file "$OUT/audit.log" > "$OUT/03-stdout.txt" 2>"$OUT/03-stderr.txt"; echo "exit=$?"
python3 /tmp/d4-summarize.py
