#!/usr/bin/env bash
# t_c3c2793a N4：**没有处置的库**（把副本还原成「从未处置」）+ 人为制造 1602 与卡片不符
# 期望：还原后 K 组全绿；篡改 1602 +1000（凭证仍平）后 K5 必须报红（不放水）
set -uo pipefail
cd /opt/cloudpivot || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
OUT=/root/k5
CP=cp_k5t5
PSQL="psql -h 127.0.0.1 -U cloudpivot -q -v ON_ERROR_STOP=1"
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
su - postgres -c "psql -q -c 'CREATE DATABASE $CP OWNER cloudpivot'" >/dev/null || exit 1
pg_dump -h 127.0.0.1 -U cloudpivot cloudpivot_rev_demo | $PSQL -d "$CP" >/dev/null || exit 1

echo "==== 还原成「从未处置」：删处置凭证 + 卡片回 IN_USE（1602 余额 = 3166.6667 = 卡片累计）"
$PSQL -d "$CP" -c "delete from biz_voucher where source_type='FIXED_ASSET_DISPOSAL';
                   update biz_fixed_asset set status='IN_USE', disposal_type=NULL, disposal_date=NULL,
                          disposal_voucher_id=NULL where status='DISPOSED';" >/dev/null
$PSQL -d "$CP" -c "select (select count(*) from biz_fixed_asset where status='DISPOSED') as 已处置卡,
                          (select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL') as 处置凭证"
NIK_DATABASE_NAME="$CP" python3 scripts/verify/erp_invariants.py --group 固资 > "$OUT/N4a_no_disposal_clean_${CP}.txt" 2>&1
echo "exit=$?"; grep -E "^结果：|K5 |K5b |覆盖：在役" "$OUT/N4a_no_disposal_clean_${CP}.txt" | head -6

echo
echo "==== 在「没有处置的库」上篡改：PZ…000009 的 1602 贷方 +1000、5602.01 借方 +1000、凭证头同步"
$PSQL -d "$CP" -c "update biz_voucher_entry set debit_amount = debit_amount + 1000
                    where voucher_id = (select id from biz_voucher where voucher_no like '%000009') and account_code like '5602%';
                   update biz_voucher_entry set credit_amount = credit_amount + 1000
                    where voucher_id = (select id from biz_voucher where voucher_no like '%000009') and account_code = '1602';
                   update biz_voucher set total_debit = total_debit + 1000, total_credit = total_credit + 1000
                    where voucher_no like '%000009';" >/dev/null
NIK_DATABASE_NAME="$CP" python3 scripts/verify/erp_invariants.py --group 固资 > "$OUT/N4b_no_disposal_tamper_${CP}.txt" 2>&1
echo "exit=$?"; grep -E "^结果：|K5 |K5b " "$OUT/N4b_no_disposal_tamper_${CP}.txt" | head -6
sed -n "/必须立刻修/,\$p" "$OUT/N4b_no_disposal_tamper_${CP}.txt" | head -4
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
echo "dropped $CP"
