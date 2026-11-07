#!/usr/bin/env bash
# t_c3c2793a 回归：① rev_demo BLOCKER 0 ② 人为制造 1602 不符仍报红 ③ 无处置库不误伤 + 多库矩阵
set -uo pipefail
cd /opt/cloudpivot || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
OUT=/root/k5
mkdir -p "$OUT"
SRC=cloudpivot_rev_demo
CP=cp_k5t1
PSQL="psql -h 127.0.0.1 -U cloudpivot -q"

echo "############ 0) 造副本库 $CP（pg_dump | psql，不碰源库）"
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
su - postgres -c "psql -q -c 'CREATE DATABASE $CP OWNER cloudpivot'" >/dev/null || exit 1
pg_dump -h 127.0.0.1 -U cloudpivot "$SRC" | $PSQL -d "$CP" >/dev/null || exit 1
echo "copy ok: $($PSQL -d $CP -tAc "select count(*) from biz_fixed_asset where status='DISPOSED'") 张 DISPOSED"

run() { # run <db> <label> [extra args]
  local db="$1" label="$2"; shift 2
  echo "---- [$label] db=$db $*"
  NIK_DATABASE_NAME="$db" python3 scripts/verify/erp_invariants.py "$@" > "$OUT/${label}_${db}.txt" 2>&1
  local rc=$?
  grep -E "^结果：" "$OUT/${label}_${db}.txt"
  grep -E "K5 |K5b |覆盖：在役" "$OUT/${label}_${db}.txt" | head -6
  echo "exit=$rc"
  return 0
}

echo
echo "############ ① 含 1 张 DISPOSED 的 rev_demo（验收标准①）"
run "$SRC" "A1_revdemo"

echo
echo "############ N1 副本原样（应绿）"
run "$CP" "N1_copy_asis"

echo
echo "############ N2 人为制造「1602 与卡片不符」：PZ…000009 的 1602 贷方 +1000，对方科目借方同加，凭证仍平"
$PSQL -d "$CP" -c "update biz_voucher_entry set debit_amount = debit_amount + 1000
                    where voucher_id = (select id from biz_voucher where voucher_no like '%000009') and account_code like '5602%';
                   update biz_voucher_entry set credit_amount = credit_amount + 1000
                    where voucher_id = (select id from biz_voucher where voucher_no like '%000009') and account_code = '1602';
                   update biz_voucher set total_debit = total_debit + 1000, total_credit = total_credit + 1000
                    where voucher_no like '%000009';" >/dev/null
$PSQL -d "$CP" -c "select v.voucher_no, e.account_code, e.debit_amount, e.credit_amount from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.voucher_no like '%000009' order by e.line_no"
run "$CP" "N2_tamper1602"

echo
echo "############ N3 「处置没记账」：删掉该资产的 3 张处置凭证（卡片仍 DISPOSED）"
$PSQL -d "$CP" -c "delete from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and source_id = (select id from biz_fixed_asset where status='DISPOSED' limit 1);" >/dev/null
$PSQL -d "$CP" -c "select count(*) as disposal_vouchers_left from biz_voucher where source_type='FIXED_ASSET_DISPOSAL'"
run "$CP" "N3_no_disposal_voucher"

echo
echo "############ ③ 没有资产的库（cloudpivot_rev_admin，只有 admin）不误伤"
run cloudpivot_rev_admin "C1_rev_admin"

echo
echo "############ C2 其它含 DISPOSED 卡片的历史库（K 组逐库，看有无新的假红）"
for db in cloudpivot_fix9_20261108030024 cloudpivot_fix8_fresh_20261108022816 \
          cloudpivot_rev_20261108034818 cloudpivot_fix7_idem_20261108015948 \
          cloudpivot_fix6_suite_011439 cloudpivot_fix4legacy_20261107233725; do
  echo "---- [$db]"
  NIK_DATABASE_NAME="$db" python3 scripts/verify/erp_invariants.py --group 固资 > "$OUT/C2_${db}.txt" 2>&1
  echo "exit=$?"
  grep -E "^结果：|K5 |K5b |覆盖：在役" "$OUT/C2_${db}.txt" | head -6
done

echo
echo "############ 清理副本库"
su - postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $CP'" >/dev/null 2>&1
echo "dropped $CP: $($PSQL -d postgres -tAc "select count(*) from pg_database where datname='$CP'") 仍存在"
echo "== 证据目录 $OUT"; ls -l "$OUT"
