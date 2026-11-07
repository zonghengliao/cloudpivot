#!/usr/bin/env bash
# t_c3c2793a N3（干净副本重做）：处置没记账 ⇒ 1602 仍挂余额、卡片 DISPOSED 被排除 ⇒ K5/K5b 必须红
set -uo pipefail
cd /opt/cloudpivot || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
OUT=/root/k5
CP=cp_k5t3
PSQL="psql -h 127.0.0.1 -U cloudpivot -q"
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
su - postgres -c "psql -q -c 'CREATE DATABASE $CP OWNER cloudpivot'" >/dev/null || exit 1
pg_dump -h 127.0.0.1 -U cloudpivot cloudpivot_rev_demo | $PSQL -d "$CP" >/dev/null || exit 1
echo "副本 $CP 建好：DISPOSED=$($PSQL -d $CP -tAc "select count(*) from biz_fixed_asset where status='DISPOSED'")"
echo "---- 删掉该资产的处置凭证（模拟「处置审批通过但凭证没落」）"
$PSQL -d "$CP" -c "delete from biz_voucher where source_type='FIXED_ASSET_DISPOSAL';" >/dev/null
$PSQL -d "$CP" -c "select count(*) as disposal_vouchers_left from biz_voucher where source_type='FIXED_ASSET_DISPOSAL'"
NIK_DATABASE_NAME="$CP" python3 scripts/verify/erp_invariants.py > "$OUT/N3_clean_no_disposal_voucher_${CP}.txt" 2>&1
echo "exit=$?"
grep -E "^结果：|K5 |K5b |覆盖：在役" "$OUT/N3_clean_no_disposal_voucher_${CP}.txt" | head -8
sed -n "/必须立刻修/,\$p" "$OUT/N3_clean_no_disposal_voucher_${CP}.txt" | head -6
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
echo "dropped $CP"
