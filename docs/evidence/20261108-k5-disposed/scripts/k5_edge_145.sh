#!/usr/bin/env bash
# t_c3c2793a 边界：作废处置 → 重新处置，K5/K5b 不许假红（K5b 只认未红冲蓝字）
set -uo pipefail
cd /opt/cloudpivot || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
OUT=/root/k5
CP=cp_k5t4
PSQL="psql -h 127.0.0.1 -U cloudpivot -q -v ON_ERROR_STOP=1"
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
su - postgres -c "psql -q -c 'CREATE DATABASE $CP OWNER cloudpivot'" >/dev/null || exit 1
pg_dump -h 127.0.0.1 -U cloudpivot cloudpivot_rev_demo | $PSQL -d "$CP" >/dev/null || exit 1
echo "副本 $CP：$($PSQL -d $CP -tAc "select count(*) from biz_fixed_asset where status='DISPOSED'") 张 DISPOSED"

echo
echo "==== E1 作废处置（红冲 + 卡片回 IN_USE）"
$PSQL -d "$CP" -f /root/k5_cancel.sql || exit 1
$PSQL -d "$CP" -c "select (select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and is_red=1) as 红字, (select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and is_red=0 and reversed_by is null) as 有效蓝字, (select count(*) from biz_fixed_asset where status='DISPOSED') as 已处置卡"
NIK_DATABASE_NAME="$CP" python3 scripts/verify/erp_invariants.py --group 固资 > "$OUT/E1_cancelled_${CP}.txt" 2>&1
echo "exit=$?"
grep -E "^结果：|K5 |K5b |覆盖：在役" "$OUT/E1_cancelled_${CP}.txt" | head -8

echo
echo "==== E2 作废后重新处置（再一套蓝字 + 卡片回 DISPOSED）"
$PSQL -d "$CP" -f /root/k5_redispose.sql || exit 1
$PSQL -d "$CP" -c "select (select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and is_red=0) as 蓝字总数, (select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and is_red=0 and reversed_by is null) as 未红冲蓝字, (select count(*) from biz_fixed_asset where status='DISPOSED') as 已处置卡"
NIK_DATABASE_NAME="$CP" python3 scripts/verify/erp_invariants.py --group 固资 > "$OUT/E2_redisposed_${CP}.txt" 2>&1
echo "exit=$?"
grep -E "^结果：|K5 |K5b |覆盖：在役" "$OUT/E2_redisposed_${CP}.txt" | head -8
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
echo "dropped $CP"
