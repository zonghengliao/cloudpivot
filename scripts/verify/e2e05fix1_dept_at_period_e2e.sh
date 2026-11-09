#!/usr/bin/env bash
# E2E05-FIX-1（卡 t_abb4bb0b）真机验收：**补提历史折旧按「期间归属部门」记费用**
#
# 环境：145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033（真 token curl）
#
# 场景（与卡面验收 ①~⑤ 一一对应）：
#
#   A 台：2029-01 在「演练设备部」，2029-02-01 转「采购演练部」
#   B 台：从未转移，一直在「采购演练部」
#
#   ① 先做转移（卡片当前部门 = 采购演练部），再补跑 2029-01
#   ② 2029-01 凭证借方 1900 必须记**演练设备部**（修前记采购演练部 —— E2E-05 Q3）
#   ③ 2029-02 记采购演练部（1900 + B 的 950）
#   ④ 无转移的 B 台两个月行为不变（都是采购演练部 / 950）
#   ⑤ DEPT 辅助账（部门损益的取数口径）按期间各归各位
#
# 期间命名空间：**2029-01 / 2029-02**（本卡独占；E2E-05 演练用 2027-12~2028-06）。
# 号段：分类 E2E5F1_* / 资产名 E2E05FIX1* / 部门 E2E5F1-D1、E2E5F1-D2。
# 重复跑：期间已开账会跳过开账，批次已存在时断言直接复用（幂等）。
#
# 用法：bash scripts/verify/e2e05fix1_dept_at_period_e2e.sh
# 退出码：0 = 全绿；非 0 = 有 FAIL（明细见 $LOG）
source /opt/cloudpivot/scripts/verify/e2e05_asset_common.sh

YEAR=${YEAR:-2029}
P1="$YEAR-01"
P2="$YEAR-02"
cat_of() { SQ "select id from biz_asset_category where category_code='$1' and deleted_at is null limit 1"; }
asset_of() { SQ "select id from biz_fixed_asset where asset_name='$1' and deleted_at is null limit 1"; }

say "===== F0 登录 + 前置（部门 / 科目 / 期间 $P1~$P2） ====="
login_admin
DEPT1=$(SQ "select id from sys_dept where dept_name='演练设备部' and deleted_at is null limit 1")
DEPT2=$(SQ "select id from sys_dept where dept_name='采购演练部' and deleted_at is null limit 1")
if [ -z "$DEPT1" ]; then
  api mk_d1 POST /system/dept "$T_ADMIN" '{"dept_code":"E2E5F1-D1","dept_name":"演练设备部","status":1,"sort_order":90}'
  check "建部门 演练设备部 http" "$HTTP" "200"
  DEPT1=$(data '.data.id')
fi
if [ -z "$DEPT2" ]; then
  api mk_d2 POST /system/dept "$T_ADMIN" '{"dept_code":"E2E5F1-D2","dept_name":"采购演练部","status":1,"sort_order":91}'
  check "建部门 采购演练部 http" "$HTTP" "200"
  DEPT2=$(data '.data.id')
fi
checkne "部门 A（演练设备部）" "$DEPT1" ""
checkne "部门 B（采购演练部）" "$DEPT2" ""
ACC_ASSET=$(acc 1601); ACC_ACCUM=$(acc 1602); ACC_EXP=$(acc 5602.01)
checkne "科目 1601/1602/5602.01 齐备" "${#ACC_ASSET}${#ACC_ACCUM}${#ACC_EXP}" "108"
save DEPT1 "$DEPT1"; save DEPT2 "$DEPT2"

have=$(SQ "select count(*) from biz_accounting_period where period_code in ('$P1','$P2')")
if [ "$have" != 2 ]; then
  api per_open POST /business/erp/period/batch-open "$T_ADMIN" \
    "{\"from\":\"$P1\",\"to\":\"$P2\",\"remark\":\"E2E05-FIX-1 期间归属部门验收\"}"
  check "批量开账 $P1~$P2 http" "$HTTP" "200"
  check "批量开账 code" "$CODE" "0"
fi
check "验收期间齐备（2 期）" "$(SQ "select count(*) from biz_accounting_period where period_code in ('$P1','$P2')")" "2"

say
say "===== F1 建分类 + 两台资产（A 在设备部 / B 在采购部） ====="
CAT=$(cat_of E2E5F1_60)
if [ -z "$CAT" ]; then
  api mk_cat POST /business/erp/asset-category "$T_ADMIN" \
    "{\"category_code\":\"E2E5F1_60\",\"category_name\":\"E2E05FIX1 60月设备\",\"useful_life_months\":60,\"salvage_rate\":0.05,\"asset_account_id\":\"$ACC_ASSET\",\"accum_account_id\":\"$ACC_ACCUM\",\"expense_account_id\":\"$ACC_EXP\",\"status\":1,\"remark\":\"E2E05-FIX-1 验收分类\"}"
  check "建分类 http" "$HTTP" "200"
  check "建分类 code" "$CODE" "0"
  CAT=$(data '.data.id')
fi
checkne "分类 E2E5F1_60" "$CAT" ""
save CAT "$CAT"

MK_ID=""
mkasset() { # mkasset <tag> <asset_name> <dept> <original_value> → 结果写全局 MK_ID
  local tag=$1 name=$2 dept=$3 value=$4 id
  id=$(asset_of "$name")
  if [ -z "$id" ]; then
    api "mk_asset_$tag" POST /business/erp/asset "$T_ADMIN" \
      "{\"asset_name\":\"$name\",\"category_id\":\"$CAT\",\"use_dept_id\":\"$dept\",\"original_value\":\"$value\",\"tax_amount\":\"0\",\"purchase_date\":\"2028-12-10\",\"remark\":\"E2E05-FIX-1 验收\"}"
    check "建卡 $name http" "$HTTP" "200"
    check "建卡 $name code" "$CODE" "0"
    id=$(data '.data.id')
  fi
  MK_ID=$id
}
mkasset a "E2E05FIX1 A 台" "$DEPT1" 120000; AS_A=$MK_ID
mkasset b "E2E05FIX1 B 台（无转移）" "$DEPT2" 60000; AS_B=$MK_ID
checkne "资产 A" "$AS_A" ""
checkne "资产 B" "$AS_B" ""
save AS_A "$AS_A"; save AS_B "$AS_B"
M_A=$(SQ "select monthly_depreciation from biz_fixed_asset where id='$AS_A'")
M_B=$(SQ "select monthly_depreciation from biz_fixed_asset where id='$AS_B'")
chknum "A 月折旧 = 114000/60" "$M_A" 1900
chknum "B 月折旧 = 57000/60" "$M_B" 950
check "A 起提日 = 购入次月 1 日" "$(SQ "select start_depreciation_date from biz_fixed_asset where id='$AS_A'")" "$P1-01"

say
say "===== F2 关键前置：A 在【补跑 $P1 之前】先转采购部（bug 现场） ====="
api as_tr POST "/business/erp/asset/$AS_A/transfer" "$T_ADMIN" \
  "{\"to_dept_id\":\"$DEPT2\",\"change_date\":\"$P2-01\",\"reason\":\"E2E05-FIX-1 验收：跨部门转移\"}"
check "转账 http" "$HTTP" "200"
check "转移留痕（from=设备部 / to=采购部 / 日期=$P2-01）" \
  "$(SQ "select from_dept_name||'->'||to_dept_name||'|'||change_date from biz_fixed_asset_change where asset_id='$AS_A' and change_type='TRANSFER' order by created_at desc limit 1")" \
  "演练设备部->采购演练部|$P2-01"
check "卡片当前部门已变成采购演练部（bug 触发条件）" \
  "$(SQ "select use_dept_id from biz_fixed_asset where id='$AS_A'")" "$DEPT2"

say
say "===== F3 补跑 $P1：借方必须记【演练设备部】而不是卡片当前部门 ====="
api dep_p1 POST /business/erp/depreciation/run "$T_ADMIN" \
  "{\"period_code\":\"$P1\",\"remark\":\"E2E05-FIX-1 验收：补提 $P1\"}"
check "计提 $P1 http" "$HTTP" "200"
OUTCOME=$(data '.data.outcome')
if [ "$OUTCOME" = "EXISTS" ]; then
  say "  · 期间 $P1 已有批次（重复跑）：直接复用现有批次做断言"
else
  check "计提 $P1 结果" "$OUTCOME" "POSTED"
  check "计提台数（A + B）" "$(data '.data.asset_count')" "2"
  chknum "批次合计 1900 + 950" "$(data '.data.total_amount')" 2850
fi
B01=$(SQ "select id from biz_depreciation_batch where period_code='$P1'")
V01=$(SQ "select voucher_id from biz_depreciation_batch where period_code='$P1'")
save B01 "$B01"; save V01 "$V01"
checkne "批次 $P1" "$B01" ""
check "批次状态 POSTED" "$(SQ "select status from biz_depreciation_batch where id='$B01'")" "POSTED"

say "  -- ⑤ DEPT 辅助账（部门损益取数口径）--"
api aux_d1_p1 GET "/business/erp/ledger/aux-balance?aux_type=DEPT&aux_id=$DEPT1&period_from=$P1&period_to=$P1" "$T_ADMIN"
check "辅助账（设备部 $P1）http" "$HTTP" "200"
chknum "辅助账 设备部 $P1 本期借方 = 1900（只有 A）" "$(data '.data.total_period_debit')" 1900
api aux_d2_p1 GET "/business/erp/ledger/aux-balance?aux_type=DEPT&aux_id=$DEPT2&period_from=$P1&period_to=$P1" "$T_ADMIN"
check "辅助账（采购部 $P1）http" "$HTTP" "200"
chknum "辅助账 采购部 $P1 本期借方 = 950（只有 B）" "$(data '.data.total_period_debit')" 950

say "  -- 凭证借方按部门 --"
check "借方 2 行（设备部 / 采购部各 1）" \
  "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V01' and debit_amount>0")" "2"
chknum "借方 演练设备部（A）= 1900" \
  "$(SQ "select debit_amount from biz_voucher_entry where voucher_id='$V01' and debit_amount>0 and aux_dept_id='$DEPT1'")" 1900
chknum "借方 采购演练部（B）= 950" \
  "$(SQ "select debit_amount from biz_voucher_entry where voucher_id='$V01' and debit_amount>0 and aux_dept_id='$DEPT2'")" 950
check "凭证摘要点名期间与部门" \
  "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V01' and debit_amount>0 and summary like '$P1 折旧（演练设备部）'")" "1"
check "A 明细的部门 = 演练设备部（不是卡片当前部门）" \
  "$(SQ "select dept_name from biz_depreciation where batch_id='$B01' and asset_id='$AS_A'")" "演练设备部"
chknum "A 明细金额 1900" "$(SQ "select amount from biz_depreciation where batch_id='$B01' and asset_id='$AS_A'")" 1900

say "  -- 同一结论经 REST 接口（批次详情）--"
api dep_detail_p1 GET "/business/erp/depreciation/$B01" "$T_ADMIN"
check "批次详情 http" "$HTTP" "200"
check "接口返回 A 的部门名 = 演练设备部" \
  "$(printf '%s' "$BODY" | jq -r ".data.details[]|select(.asset_id==\"$AS_A\")|.dept_name")" "演练设备部"

say
say "===== F4 跑 $P2：A 已转 ⇒ 记采购演练部；B 行为不变 ====="
api dep_p2 POST /business/erp/depreciation/run "$T_ADMIN" \
  "{\"period_code\":\"$P2\",\"remark\":\"E2E05-FIX-1 验收：$P2\"}"
check "计提 $P2 http" "$HTTP" "200"
OUTCOME2=$(data '.data.outcome')
if [ "$OUTCOME2" = "EXISTS" ]; then
  say "  · 期间 $P2 已有批次（重复跑）：直接复用现有批次做断言"
else
  check "计提 $P2 结果" "$OUTCOME2" "POSTED"
fi
B02=$(SQ "select id from biz_depreciation_batch where period_code='$P2'")
V02=$(SQ "select voucher_id from biz_depreciation_batch where period_code='$P2'")
save B02 "$B02"; save V02 "$V02"
check "借方 1 行（都归采购部）" \
  "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V02' and debit_amount>0")" "1"
chknum "借方 采购演练部（A + B）= 2850" \
  "$(SQ "select debit_amount from biz_voucher_entry where voucher_id='$V02' and debit_amount>0 and aux_dept_id='$DEPT2'")" 2850
check "借方没有演练设备部的行（A 已转走 / B 本来不在）" \
  "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V02' and debit_amount>0 and aux_dept_id='$DEPT1'")" "0"
check "A 明细的部门 = 采购演练部" \
  "$(SQ "select dept_name from biz_depreciation where batch_id='$B02' and asset_id='$AS_A'")" "采购演练部"
api aux_d1_p2 GET "/business/erp/ledger/aux-balance?aux_type=DEPT&aux_id=$DEPT1&period_from=$P2&period_to=$P2" "$T_ADMIN"
check "辅助账（设备部 $P2）http" "$HTTP" "200"
chknum "辅助账 设备部 $P2 本期借方 = 0" "$(data '.data.total_period_debit')" 0
api aux_d2_p2 GET "/business/erp/ledger/aux-balance?aux_type=DEPT&aux_id=$DEPT2&period_from=$P2&period_to=$P2" "$T_ADMIN"
chknum "辅助账 采购部 $P2 本期借方 = 2850" "$(data '.data.total_period_debit')" 2850

say "  -- ④ 无转移的资产（B）：两个月都是采购演练部 / 950 --"
check "B 明细 $P1 部门" "$(SQ "select dept_name from biz_depreciation where batch_id='$B01' and asset_id='$AS_B'")" "采购演练部"
chknum "B 明细 $P1 金额" "$(SQ "select amount from biz_depreciation where batch_id='$B01' and asset_id='$AS_B'")" 950
check "B 明细 $P2 部门" "$(SQ "select dept_name from biz_depreciation where batch_id='$B02' and asset_id='$AS_B'")" "采购演练部"
chknum "B 明细 $P2 金额" "$(SQ "select amount from biz_depreciation where batch_id='$B02' and asset_id='$AS_B'")" 950

say
say "== PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
