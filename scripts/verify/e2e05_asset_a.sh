#!/usr/bin/env bash
# E2E-05 ① 资产分类 + ② 资产卡片（建卡/购入凭证/改卡/转移/使用人/停用启用/通知/删除闸）
# 环境：145 cloudpivot_dev，真后端；演练期间 2027-12 ~ 2028-06（本卡独占，全库无其他资产）
source /opt/cloudpivot/scripts/verify/e2e05_asset_common.sh

say "===== A0 登录 + 前置（部门 / 科目 / 期间） ====="
login_admin
DEPT1=$(SQ "select id from sys_dept where dept_name='演练设备部' and deleted_at is null limit 1")
DEPT2=$(SQ "select id from sys_dept where dept_name='采购演练部' and deleted_at is null limit 1")
U_ADMIN=$(SQ "select id from sys_user where username='admin'")
U_NAME=$(SQ "select coalesce(nullif(real_name,''),username) from sys_user where username='admin'")
ACC_ASSET=$(acc 1601); ACC_ACCUM=$(acc 1602); ACC_EXP=$(acc 5602.01)
save DEPT1 "$DEPT1"; save DEPT2 "$DEPT2"; save U_ADMIN "$U_ADMIN"
checkne "演练部门 A（设备部）存在" "$DEPT1" ""
checkne "演练部门 B（采购部）存在" "$DEPT2" ""
checkne "科目 1601/1602/5602.01 齐备" "${#ACC_ASSET}${#ACC_ACCUM}${#ACC_EXP}" "108"
ensure_periods

say
say "===== A1 资产分类页：分页 / 下拉 / 预览编码 ====="
api cat_page GET "/business/erp/asset-category/page?page=1&page_size=50" "$T_ADMIN"
check "分类分页 http" "$HTTP" "200"
check "分类分页 code" "$CODE" "0"
CATTOT=$(data '.data.total')
say "  · 现有分类 $CATTOT 个"
api cat_list GET "/business/erp/asset-category/list" "$T_ADMIN"
check "分类下拉 http" "$HTTP" "200"
check "下拉只给启用分类" "$(data '[.data[]|select(.status!=1)]|length')" "0"
api cat_next GET "/business/erp/asset-category/next-code" "$T_ADMIN"
check "预览编码 http" "$HTTP" "200"
NEXTCODE=$(data '.data.code // .data.next_code // .data')
say "  · 下一个可用分类编码：$NEXTCODE"
checkne "预览编码非空" "$NEXTCODE" ""

say
say "===== A2 建分类：60 期 / 3 期 / 12 期 + 自动取号 ====="
MK_ID=""
mkcat() { # mkcat <tag> <code> <name> <months> → 结果写全局 MK_ID
  local code_json=""
  [ -n "$2" ] && code_json="\"category_code\":\"$2\","
  api "$1" POST /business/erp/asset-category "$T_ADMIN" \
    "{$code_json\"category_name\":\"$3\",\"useful_life_months\":$4,\"salvage_rate\":0.05,\"asset_account_id\":\"$ACC_ASSET\",\"accum_account_id\":\"$ACC_ACCUM\",\"expense_account_id\":\"$ACC_EXP\",\"status\":1,\"remark\":\"E2E-05 演练分类\"}"
  check "建分类 $1 http" "$HTTP" "200"
  check "建分类 $1 code" "$CODE" "0"
  MK_ID=$(data '.data.id')
}
mkcat cat_eq E2E5_EQ "E2E05 演练设备（60月）" 60; CAT_EQ=$MK_ID
mkcat cat_3m E2E5_3M "E2E05 演练设备（3月尾差）" 3; CAT_3M=$MK_ID
mkcat cat_dis E2E5_DIS "E2E05 演练设备（12月处置）" 12; CAT_DIS=$MK_ID
mkcat cat_auto "" "E2E05 自动取号分类" 24; CAT_AUTO=$MK_ID
AUTO_CODE=$(SQ "select category_code from biz_asset_category where id='$CAT_AUTO'")
save CAT_EQ "$CAT_EQ"; save CAT_3M "$CAT_3M"; save CAT_DIS "$CAT_DIS"; save CAT_AUTO "$CAT_AUTO"
checkne "三个手工分类落库" "${#CAT_EQ}${#CAT_3M}${#CAT_DIS}" "108"
checkne "分类自动取号（空编码 → AC-xx）" "$AUTO_CODE" ""
say "  · 自动取号得到：$AUTO_CODE"
check "分类落库 60 期 / 残值率" "$(SQ "select useful_life_months||'/'||salvage_rate from biz_asset_category where id='$CAT_EQ'")" "60/0.0500"

say
say "===== A3 分类参数校验（负例） ====="
api cat_dup POST /business/erp/asset-category "$T_ADMIN" \
  "{\"category_code\":\"E2E5_EQ\",\"category_name\":\"重复编码\",\"useful_life_months\":12}"
check "重复编码 http" "$HTTP" "409"
api cat_badrate POST /business/erp/asset-category "$T_ADMIN" \
  '{"category_code":"E2E5_BAD","category_name":"残值率越界","useful_life_months":12,"salvage_rate":1.5}'
check "残值率 1.5 http" "$HTTP" "400"
check "残值率 1.5 错误码" "$CODE" "10001"
api cat_badmonth POST /business/erp/asset-category "$T_ADMIN" \
  '{"category_code":"E2E5_BAD2","category_name":"月数越界","useful_life_months":0}'
check "月数 0 http" "$HTTP" "400"
check "月数 0 错误码" "$CODE" "10001"

say
say "===== A4 改分类 / 删分类（无卡片可删） ====="
api cat_upd PUT "/business/erp/asset-category/$CAT_AUTO" "$T_ADMIN" \
  '{"category_name":"E2E05 自动取号分类（改名）","useful_life_months":24,"salvage_rate":0.03}'
check "改分类 http" "$HTTP" "200"
check "改分类落库" "$(SQ "select category_name||'/'||salvage_rate from biz_asset_category where id='$CAT_AUTO'")" "E2E05 自动取号分类（改名）/0.0300"
api cat_del DELETE "/business/erp/asset-category/$CAT_AUTO" "$T_ADMIN"
check "删空分类 http" "$HTTP" "200"
check "删空分类已软删" "$(SQ "select count(*) from biz_asset_category where id='$CAT_AUTO' and deleted_at is not null")" "1"

say
say "===== A5 建卡（购入 → 建卡即入账 + 购入凭证） ====="
MK_ID=""
mkasset() { # mkasset <tag> <cat> <name> <value> <purchase_date> <spec> → 结果写全局 MK_ID
  api "$1" POST /business/erp/asset "$T_ADMIN" \
    "{\"asset_name\":\"$3\",\"category_id\":\"$2\",\"spec\":\"$6\",\"use_dept_id\":\"$DEPT1\",\"location\":\"演练车间 1 号位\",\"original_value\":\"$4\",\"tax_amount\":\"0\",\"purchase_date\":\"$5\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"E2E-05 演练资产\"}"
  check "建卡 $1 http" "$HTTP" "200"
  check "建卡 $1 code" "$CODE" "0"
  MK_ID=$(data '.data.id')
}
mkasset mk_a "$CAT_EQ" "E2E05 设备A" 120000 2027-12-01 E2E05-A; AS_A=$MK_ID
mkasset mk_b "$CAT_3M" "E2E05 设备B" 1000 2027-12-01 E2E05-B; AS_B=$MK_ID
mkasset mk_c "$CAT_DIS" "E2E05 设备C" 60000 2027-12-01 E2E05-C; AS_C=$MK_ID
mkasset mk_d "$CAT_DIS" "E2E05 设备D" 3000 2027-12-01 E2E05-D; AS_D=$MK_ID
mkasset mk_e "$CAT_3M" "E2E05 设备E（待删）" 500 2028-06-01 E2E05-E; AS_E=$MK_ID
save AS_A "$AS_A"; save AS_B "$AS_B"; save AS_C "$AS_C"; save AS_D "$AS_D"; save AS_E "$AS_E"
NO_A=$(SQ "select asset_no from biz_fixed_asset where id='$AS_A'")
NO_C=$(SQ "select asset_no from biz_fixed_asset where id='$AS_C'")
save NO_A "$NO_A"; save NO_C "$NO_C"
checkne "五张卡片 id 齐备" "${#AS_A}${#AS_B}${#AS_C}${#AS_D}${#AS_E}" "180"

say "  · 卡片状态 / 起提日 / 折旧派生值："
api asset_detail GET "/business/erp/asset/$AS_A" "$T_ADMIN"
check "卡片详情 http" "$HTTP" "200"
check "建卡即 IN_USE（激活入账）" "$(data '.data.asset.status')" "IN_USE"
check "起提日 = 购入次月 1 日" "$(data '.data.asset.start_depreciation_date')" "2028-01-01"
chknum "A 残值 = 120000×5%" "$(SQ "select residual_value from biz_fixed_asset where id='$AS_A'")" 6000
chknum "A 可折旧总额" "$(SQ "select depreciable_base from biz_fixed_asset where id='$AS_A'")" 114000
chknum "A 月折旧 = 114000/60" "$(SQ "select monthly_depreciation from biz_fixed_asset where id='$AS_A'")" 1900
chknum "B 月折旧 = 950/3" "$(SQ "select monthly_depreciation from biz_fixed_asset where id='$AS_B'")" 316.6667
chknum "C 月折旧 = 57000/12" "$(SQ "select monthly_depreciation from biz_fixed_asset where id='$AS_C'")" 4750
check "建卡累计折旧 = 0" "$(SQ "select accumulated_depreciation from biz_fixed_asset where id='$AS_A'")" "0.0000"
check "建卡净值 = 原值" "$(SQ "select net_value from biz_fixed_asset where id='$AS_A'")" "120000.0000"
check "卡片分类快照 = E2E5_EQ" "$(SQ "select category_code from biz_fixed_asset where id='$AS_A'")" "E2E5_EQ"
check "部门名快照" "$(SQ "select use_dept_name from biz_fixed_asset where id='$AS_A'")" "演练设备部"

say
say "===== A6 购入凭证（FIXED_ASSET_ACQ） ====="
acq_vid=$(SQ "select voucher_id from biz_fixed_asset where id='$AS_A'")
checkne "A 卡片回写购入凭证 id" "$acq_vid" ""
check "购入凭证类型" "$(SQ "select source_type from biz_voucher where id='$acq_vid'")" "FIXED_ASSET_ACQ"
check "购入凭证状态" "$(SQ "select status from biz_voucher where id='$acq_vid'")" "POSTED"
check "购入凭证日期 = 购入日" "$(SQ "select voucher_date from biz_voucher where id='$acq_vid'")" "2027-12-01"
check "购入凭证明细 2 行（借 1601 / 贷 应付）" "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$acq_vid'")" "2"
chknum "购入凭证 借 1601 = 原值" "$(SQ "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$acq_vid' and account_code='1601'")" 120000
check "购入凭证 贷方科目（赊购 → 2202）" "$(SQ "select account_code from biz_voucher_entry where voucher_id='$acq_vid' and credit_amount>0")" "2202"
check "全库不平凭证 = 0" "$(SQ "select count(*) from biz_voucher where total_debit <> total_credit and deleted_at is null")" "0"
check "五张建卡凭证全部 POSTED" "$(SQ "select count(*) from biz_voucher where source_type='FIXED_ASSET_ACQ' and source_no in (select asset_no from biz_fixed_asset where asset_name like 'E2E05%') and status='POSTED'")" "5"

say
say "===== A7 建卡负例 ====="
api mk_bad_dept POST /business/erp/asset "$T_ADMIN" \
  "{\"asset_name\":\"E2E05 坏卡\",\"category_id\":\"$CAT_EQ\",\"use_dept_id\":\"00000000-0000-4000-8000-000000000000\",\"original_value\":\"100\",\"purchase_date\":\"2027-12-01\"}"
check "部门不存在 http" "$HTTP" "400"
api mk_bad_val POST /business/erp/asset "$T_ADMIN" \
  "{\"asset_name\":\"E2E05 坏卡2\",\"category_id\":\"$CAT_EQ\",\"use_dept_id\":\"$DEPT1\",\"original_value\":\"0\",\"purchase_date\":\"2027-12-01\"}"
check "原值 0 http" "$HTTP" "400"
api cat_del_used DELETE "/business/erp/asset-category/$CAT_EQ" "$T_ADMIN"
check "删有卡片的分类 http" "$HTTP" "409"

say
say "===== A8 改卡片：白名单字段生效、金额不可改 ====="
api as_upd PUT "/business/erp/asset/$AS_A" "$T_ADMIN" \
  '{"asset_name":"E2E05 设备A（改名）","spec":"E2E05-A2","location":"演练车间 2 号位","original_value":"999999","remark":"E2E-05 改卡"}'
check "改卡 http" "$HTTP" "200"
check "改名生效" "$(SQ "select asset_name from biz_fixed_asset where id='$AS_A'")" "E2E05 设备A（改名）"
check "改地点生效" "$(SQ "select location from biz_fixed_asset where id='$AS_A'")" "演练车间 2 号位"
chknum "原值不可改（请求 999999 被忽略）" "$(SQ "select original_value from biz_fixed_asset where id='$AS_A'")" 120000

say
say "===== A9 部门转移 + 使用人变更（无凭证，只留痕） ====="
api as_tr POST "/business/erp/asset/$AS_A/transfer" "$T_ADMIN" \
  "{\"to_dept_id\":\"$DEPT2\",\"change_date\":\"2028-02-01\",\"reason\":\"E2E-05 演练：设备调拨到采购部\"}"
check "转移 http" "$HTTP" "200"
check "转移后部门 id" "$(SQ "select use_dept_id from biz_fixed_asset where id='$AS_A'")" "$DEPT2"
check "转移后部门名快照" "$(SQ "select use_dept_name from biz_fixed_asset where id='$AS_A'")" "采购演练部"
check "转移留痕（TRANSFER）" "$(SQ "select change_type||'|'||change_date||'|'||from_dept_name||'→'||to_dept_name from biz_fixed_asset_change where asset_id='$AS_A' and change_type='TRANSFER'")" "TRANSFER|2028-02-01|演练设备部→采购演练部"
checkne "转移留痕带操作人姓名快照" "$(SQ "select operator_name from biz_fixed_asset_change where asset_id='$AS_A' and change_type='TRANSFER'")" ""
api as_cus POST "/business/erp/asset/$AS_A/custodian" "$T_ADMIN" \
  "{\"to_user_id\":\"$U_ADMIN\",\"change_date\":\"2028-02-02\",\"reason\":\"E2E-05 演练：使用人变更\"}"
check "使用人变更 http" "$HTTP" "200"
check "使用人 id" "$(SQ "select custodian_id from biz_fixed_asset where id='$AS_A'")" "$U_ADMIN"
check "使用人姓名快照" "$(SQ "select custodian_name from biz_fixed_asset where id='$AS_A'")" "$U_NAME"
check "使用人留痕（CUSTODIAN）" "$(SQ "select change_type||'|'||to_user_name from biz_fixed_asset_change where asset_id='$AS_A' and change_type='CUSTODIAN'")" "CUSTODIAN|$U_NAME"
check "转移/使用人不动累计折旧" "$(SQ "select accumulated_depreciation||'/'||depreciation_months_done from biz_fixed_asset where id='$AS_A'")" "0.0000/0"

say
say "===== A10 停用 / 启用（IN_USE ⇄ IDLE）+ FIXED_ASSET 联动通知 ====="
N0=$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET'")
api st_idle POST "/business/erp/asset/$AS_A/status" "$T_ADMIN" \
  '{"target_status":"IDLE","change_date":"2028-02-01","reason":"E2E-05 演练：设备停用"}'
check "停用 http" "$HTTP" "200"
check "停用后状态" "$(SQ "select status from biz_fixed_asset where id='$AS_A'")" "IDLE"
check "停用留痕（IN_USE→IDLE）" "$(SQ "select from_status||'→'||to_status from biz_fixed_asset_change where asset_id='$AS_A' and change_type='IDLE' order by created_at desc limit 1")" "IN_USE→IDLE"
check "状态日志落 biz_status_log" "$(SQ "select from_status||'→'||to_status from biz_status_log where entity_no='$NO_A' and to_status='IDLE' order by created_at desc limit 1")" "IN_USE→IDLE"
N1=$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET'")
check "FIXED_ASSET/IDLE 通知 +1（FIX-3 规则）" "$((N1-N0))" "1"
check "通知幂等键形态" "$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET' and idempotency_key like 'LNK:FIXED_ASSET:IDLE:%'")" "1"
api st_dup POST "/business/erp/asset/$AS_A/status" "$T_ADMIN" '{"target_status":"IDLE","reason":"重复停用"}'
check "重复停用 http" "$HTTP" "409"
check "重复停用错误码" "$CODE" "66046"
check "重复停用不重复通知" "$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET'")" "$N1"
api st_disposed POST "/business/erp/asset/$AS_A/status" "$T_ADMIN" '{"target_status":"DISPOSED","reason":"走错接口"}'
check "处置走 /status http" "$HTTP" "400"
api st_use POST "/business/erp/asset/$AS_A/status" "$T_ADMIN" '{"target_status":"IN_USE","change_date":"2028-02-15","reason":"E2E-05 演练：设备启用"}'
check "启用缺附件被拦（ATT 规则：ACTIVATE 要发票/验收单）" "$HTTP" "400"
ensure_asset_att "$AS_A" a
api st_use POST "/business/erp/asset/$AS_A/status" "$T_ADMIN" \
  '{"target_status":"IN_USE","change_date":"2028-02-15","reason":"E2E-05 演练：设备启用"}'
check "启用 http" "$HTTP" "200"
check "启用后状态" "$(SQ "select status from biz_fixed_asset where id='$AS_A'")" "IN_USE"
N2=$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET'")
check "FIXED_ASSET/IN_USE 通知 +1" "$((N2-N1))" "1"
check "通知幂等键（事件维度：可逆边第二次照发）" "$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET' and biz_id='$AS_A' and idempotency_key like 'LNK:FIXED_ASSET:%'")" "2"

say
say "===== A11 删除卡片闸门（未计提 / 已计提 / 已处置） ====="
# E 卡：2028-06 购入，尚未起提 → 「未计提」分支
api del_e DELETE "/business/erp/asset/$AS_E" "$T_ADMIN"
say "  · 删除未计提卡片 E：http=$HTTP code=$CODE（若 200 = 卡片被硬删但购入凭证仍在 → 见问题清单 Q1）"
if [ "$HTTP" = "200" ]; then
  bad "删除已生成购入凭证的卡片被放行（200）：1601 总账与卡片台账将永久不符"
  save DEL_E_LEAK 1
else
  ok "删除已生成购入凭证的卡片被拦（http=$HTTP code=$CODE）"
fi
check "E 卡片状态（200 则已软删）" "$(SQ "select case when deleted_at is null then 'ACTIVE' else 'DELETED' end from biz_fixed_asset where id='$AS_E'")" "$([ "${DEL_E_LEAK:-0}" = 1 ] && echo DELETED || echo ACTIVE)"
chknum "1601 净额 vs 在册卡片原值（差额 = 被删卡片的购入凭证）" \
  "$(SQ "select coalesce((select sum(debit_amount-credit_amount) from biz_voucher_entry where account_code='1601'),0) - coalesce((select sum(original_value) from biz_fixed_asset where deleted_at is null and status<>'DISPOSED'),0)")" 0

say
summary
