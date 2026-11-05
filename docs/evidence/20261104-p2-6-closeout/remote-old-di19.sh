#!/usr/bin/env bash
# 收口D ②：生产上「重放改前 DI-19 口径」+ 导出改后 DI-19 行，供逐条对比（全程只读）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
export PGOPTIONS='-c default_transaction_read_only=on'
OUT=/tmp/d4-out
PSQL="psql -X -q -h 127.0.0.1 -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME"

echo "########## 改前口径（7f0fe5c^）的 DI-19 SELECT 在生产上只读重放 ##########"
echo "-- 语句只读确认 --"
$PSQL -tAc "show default_transaction_read_only;"
$PSQL -f "$OUT/old-di19.sql" > "$OUT/old-di19-rows.tsv" 2>"$OUT/old-di19-err.txt"
echo "exit=$?"
wc -l "$OUT/old-di19-rows.tsv"
head -3 "$OUT/old-di19-rows.tsv"
cat "$OUT/old-di19-err.txt"

echo; echo "########## 改后口径（线上视图 di.c_di19）##########"
$PSQL -c "select * from di.c_di19 order by entity_no;" > "$OUT/new-di19-rows.tsv" 2>&1
echo "exit=$?"
wc -l "$OUT/new-di19-rows.tsv"
head -3 "$OUT/new-di19-rows.tsv"

echo; echo "########## 行数守恒（同一条 = entity_type+entity_id）##########"
$PSQL -tAc "select '改后: count='||count(*)||' distinct_id='||count(distinct entity_id) from di.c_di19;"
