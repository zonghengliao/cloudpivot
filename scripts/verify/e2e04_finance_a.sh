#!/usr/bin/env bash
# E2E-04 财务域演练 · A 段：① 会计科目 ② 会计期间 ③ 期初余额 ④ 记账凭证
# 在 145 上执行：bash scripts/verify/e2e04_finance_a.sh
set -uo pipefail
cd "$(dirname "$0")"
. ./e2e04_finance_common.sh

say "=============================================================="
say "A 段 step0：登录 + 科目字典夹具（2022 演练年度）"
say "=============================================================="
login
# 审核人：FINANCE_MANAGER（erp.separation_of_duties.approve=true → 制单人不能自审）
api a0_fin POST /system/user "$T_ADMIN" '{"username":"e2e04_fin","password":"E2e04@Fin123456","real_name":"E2E04 财务审核人","status":1,"role_ids":["20000000-0000-4000-8000-0000000f0005"],"remark":"E2E-04 财务演练审核人"}'
if [ "$CODE" = 0 ]; then FIN_UID=$(data '.data.id')
else FIN_UID=$(PSQLQ -c "select id from sys_user where username='e2e04_fin' and deleted_at is null;");
     api a0_fin_pwd PATCH "/system/user/$FIN_UID/password" "$T_ADMIN" '{"password":"E2e04@Fin123456"}'; fi
save FIN_UID "$FIN_UID"
PSQLQ -c "update sys_user set must_change_password=false, is_locked=false, login_fail_count=0 where username in ('admin','e2e04_fin');" >/dev/null
T_FIN=$(login_as e2e04_fin 'E2e04@Fin123456' a0f) || exit 1
save T_FIN "$T_FIN"
say "  · 财务审核人 e2e04_fin 登录（$FIN_UID）"

YEAR=2022
api a0_acc_page GET "/business/erp/account/page?page=1&page_size=400" "$T_ADMIN"
check "科目分页 http" "$HTTP" "200"
ACC_TOTAL=$(data '.data.total')
checkne "科目总数 > 60" "$([ "${ACC_TOTAL:-0}" -gt 60 ] && echo yes || echo no)" "no"

acc_id() { data ".data.list[]? | select(.account_code==\"$1\") | .id" | head -1; }
for code in 1001 1002 1122 1405 2202 2221.01 2221.02 3103 3104 3001 5001 5403 5602.03 6401; do
  v=$(acc_id "$code")
  var="ACC_$(printf '%s' "$code" | tr -d '.')"
  if [ -z "$v" ]; then bad "科目 $code 未找到"; else save "$var" "$v"; fi
done
say "  · 已取到 14 个科目 id（1002=${ACC_1002:0:8}… 2221.01=${ACC_222101:0:8}…）"

say
say "=============================================================="
say "① 会计科目（科目树 / 辅助核算）"
say "=============================================================="
api a1_leaf GET "/business/erp/account/page?page=1&page_size=400&is_leaf=true" "$T_ADMIN"
LEAF_N=$(data '.data.list | length')
checkne "末级科目数 > 0" "$([ "${LEAF_N:-0}" -gt 0 ] && echo yes || echo no)" "no"
PARENT_N=$(PSQLQ -c "select count(*) from biz_account where deleted_at is null and is_leaf=0;")
checkne "非末级（树父节点）数 > 0" "$([ "${PARENT_N:-0}" -gt 0 ] && echo yes || echo no)" "no"
check "树：末级+父节点 = 全量" "$((LEAF_N + PARENT_N))" "$ACC_TOTAL"

api a1_pl GET "/business/erp/account/page?page=1&page_size=400&account_category=PROFIT_LOSS&is_leaf=true" "$T_ADMIN"
check "损益类过滤后非空" "$([ "$(data '.data.list|length')" -gt 0 ] && echo yes || echo no)" "yes"
check "损益类过滤无越界" "$(data '[.data.list[]|select(.account_category!="PROFIT_LOSS")]|length')" "0"

api a1_detail GET "/business/erp/account/$ACC_1002" "$T_ADMIN"
check "科目详情 account_code" "$(data '.data.account_code')" "1002"
check "科目详情 balance_direction" "$(data '.data.balance_direction')" "DEBIT"

api a1_stopped GET "/business/erp/account/page?page=1&page_size=400&status=0" "$T_ADMIN"
check "停用科目过滤（应为 0 或全是停用）" "$(data '[.data.list[]|select(.status!=0)]|length')" "0"

# 辅助核算：科目上挂的维度（5602.03 → DEPT）
api a1_aux_acc GET "/business/erp/account/page?page=1&page_size=400&keyword=5602.03" "$T_ADMIN"
check "5602.03 辅助核算维度" "$(data '.data.list[0].aux_dimensions')" "DEPT"
api a1_aux_bal GET "/business/erp/ledger/aux-balance?aux_type=DEPT&period_from=2026-11&period_to=2026-11" "$T_ADMIN"
check "辅助账(DEPT) http" "$HTTP" "200"
check "辅助账带 unassigned 覆盖度自证" "$(data '.data|has("unassigned")')" "true"
AUX_ROWS=$(data '.data.rows|length')
say "  · 2026-11 部门辅助账行数 = $AUX_ROWS，未挂维度笔数 = $(data '.data.unassigned.entry_count')"
api a1_aux_bad GET "/business/erp/ledger/aux-balance?aux_type=NOPE&period_from=2026-11" "$T_ADMIN"
check "非法 aux_type → 400" "$HTTP" "400"

say
say "=============================================================="
say "② 会计期间（月度期间 / 开账关账）"
say "=============================================================="
api a2_page GET "/business/erp/period/page?period_year=$YEAR&page=1&page_size=50" "$T_ADMIN"
check "2022 期间行数" "$(data '.data.list|length')" "12"
check "2022 期间全 OPEN" "$(data '[.data.list[]|select(.status!="OPEN")]|length')" "0"
P2022_01=$(data '.data.list[]|select(.period_code=="2022-01")|.id')
save P2022_01 "$P2022_01"

api a2_open2023 POST /business/erp/period/batch-open "$T_ADMIN" '{"from":"2023-01","to":"2023-12","remark":"E2E-04 演练：为年结预开 2023 年度"}'
check "批量开账 2023 http" "$HTTP" "200"
check "2023 新建+跳过 = 12（幂等可重跑）" "$(( $(data '.data.created_count') + $(data '.data.skipped|length') ))" "12"
check "DB 2023 期间 12 行全 OPEN" "$(PSQLQ -c "select count(*) from biz_accounting_period where period_code like '2023-%' and status='OPEN';")" "12"
api a2_open2023b POST /business/erp/period/batch-open "$T_ADMIN" '{"from":"2023-01","to":"2023-12"}'
check "重复批量开账幂等（created=0）" "$(data '.data.created_count')" "0"
check "重复批量开账跳过 12" "$(data '.data.skipped|length')" "12"

api a2_bad_order POST /business/erp/period/batch-open "$T_ADMIN" '{"from":"2024-06","to":"2024-01"}'
check "from>to → 400" "$HTTP" "400"
api a2_bad_span POST /business/erp/period/batch-open "$T_ADMIN" '{"from":"2024-01","to":"2036-12"}'
check "一次超 120 个月 → 400" "$HTTP" "400"
check "超限未落库（2024 期间 0 行）" "$(PSQLQ -c "select count(*) from biz_accounting_period where period_code like '2024-%';")" "0"

api a2_dup POST /business/erp/period "$T_ADMIN" '{"period_year":2023,"period_month":3}'
check "重复建期间 → 409" "$HTTP" "409"
api a2_bad_month POST /business/erp/period "$T_ADMIN" '{"period_year":2023,"period_month":13}'
check "月份 13 → 400" "$HTTP" "400"
api a2_closecheck GET "/business/erp/period/$P2022_01/close-check" "$T_ADMIN"
check "结账预检 http" "$HTTP" "200"
check "空月份 2022-01 可结账" "$(data '.data.can_close')" "true"

say
say "=============================================================="
say "③ 期初余额（录入 / 试算平衡）"
say "=============================================================="
api a3_get0 GET "/business/erp/opening-balance?period=2022-01" "$T_ADMIN"
check "期初总览 http" "$HTTP" "200"
A3_STATE=$(data '.data.state')
check "期初状态合法（NONE/ENTERED，重跑幂等）" "$([ "$A3_STATE" = "NONE" ] || [ "$A3_STATE" = "ENTERED" ] && echo ok || echo "$A3_STATE")" "ok"
check "未录时借贷平衡（0=0）" "$(data '.data.balanced')" "true"

api a3_save POST /business/erp/opening-balance "$T_ADMIN" '{"period":"2022-01","entries":[
  {"account_code":"1002","debit":"500000","remark":"E2E-04 演练期初：银行存款"},
  {"account_code":"3104","credit":"500000","remark":"E2E-04 演练期初：利润分配"}]}'
check "保存科目期初 http" "$HTTP" "200"
check "保存后 state=ENTERED" "$(data '.data.state')" "ENTERED"
check "合并口径借贷平衡" "$(data '.data.balanced')" "true"
check "借方合计" "$(money '.data.combined_debit_total')" "500000.00"
check "贷方合计" "$(money '.data.combined_credit_total')" "500000.00"
check "期初凭证分录数" "$(data '.data.voucher.entry_count')" "2"
save OPG_VOUCHER "$(data '.data.voucher.id')"
check "DB 期初行数" "$(PSQLQ -c "select count(*) from biz_opening_balance where period_code='2022-01';")" "2"
check "DB 期初凭证类型 OPENING" "$(PSQLQ -c "select voucher_type from biz_voucher where id='$OPG_VOUCHER';")" "OPENING"
check "DB 期初凭证借贷相等" "$(PSQLQ -c "select (total_debit=total_credit)::text from biz_voucher where id='$OPG_VOUCHER';")" "true"

api a3_unbal POST /business/erp/opening-balance "$T_ADMIN" '{"period":"2022-01","entries":[
  {"account_code":"1002","debit":"100"},{"account_code":"3104","credit":"90"}]}'
check "不平衡期初 → 400" "$HTTP" "400"
check "不平衡业务码 66012" "$CODE" "66012"
printf '%s' "$BODY" | jq -r '.msg' | grep -q "差" && ok "不平衡文案点出差额" || bad "不平衡文案未点差额：$(data '.msg')"
check "不平衡未落库（仍 2 行）" "$(PSQLQ -c "select count(*) from biz_opening_balance where period_code='2022-01';")" "2"

api a3_badacc POST /business/erp/opening-balance "$T_ADMIN" '{"period":"2022-01","entries":[
  {"account_code":"9999","debit":"1"},{"account_code":"3104","credit":"1"}]}'
check "不存在科目 → 400" "$HTTP" "400"
api a3_locked POST /business/erp/opening-balance "$T_ADMIN" '{"period":"2026-09","entries":[
  {"account_code":"1002","debit":"1"},{"account_code":"3104","credit":"1"}]}'
check "已结账期间录期初 → 409" "$HTTP" "409"
check "已结账业务码 10003" "$CODE" "10003"

api a3_get1 GET "/business/erp/opening-balance?period=2022-01" "$T_ADMIN"
check "回读期初行数" "$(data '.data.entries|length')" "2"
api a3_tb GET "/business/erp/ledger/trial-balance?period=2022-01" "$T_ADMIN"
check "2022-01 试算：借方合计=贷方合计" "$(money '.data.total_period_debit')" "$(money '.data.total_period_credit')"
check "2022-01 试算借方差额 0" "$(printf '%s' "$BODY" | jq -r '((.data.total_period_debit|tonumber)-(.data.total_period_credit|tonumber))|tostring')" "0"

say
say "=============================================================="
say "④ 记账凭证（手工凭证 12 个月 + 状态机 + 红冲 + 自动凭证查询）"
say "=============================================================="
# 状态机负向：新建 = DRAFT（文档里写的是「直接 POSTED」，实测以代码为准）
if [ -z "${A4_SM_DONE:-}" ]; then
api a4_draft POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2022-01-31\",\"summary\":\"E2E-04 演练：资金内部划转\",\"entries\":[
  {\"account_id\":\"$ACC_1001\",\"debit\":\"1500\"},{\"account_id\":\"$ACC_1002\",\"credit\":\"1500\"}]}"
check "手工建单 http" "$HTTP" "200"
DRAFT_ID=$(data '.data.id')
check "手工建单状态 DRAFT" "$(data '.data.status')" "DRAFT"
api a4_post_early POST "/business/erp/voucher/$DRAFT_ID/post" "$T_ADMIN" '{}'
check "DRAFT 直接记账被拦（409/400）" "$([ "$HTTP" = "400" ] || [ "$HTTP" = "409" ] && echo blocked || echo "$HTTP")" "blocked"
api a4_sod POST "/business/erp/voucher/$DRAFT_ID/audit" "$T_ADMIN" '{}'
check "制单人自审被拦（65019）" "$CODE" "65019"
api a4_unaudit_bad POST "/business/erp/voucher/$DRAFT_ID/un-audit" "$T_ADMIN" '{"reason":"短"}'
check "未审核就反审核被拦（409/400）" "$([ "$HTTP" = "400" ] || [ "$HTTP" = "409" ] && echo blocked || echo "$HTTP")" "blocked"
api a4_audit POST "/business/erp/voucher/$DRAFT_ID/audit" "$T_FIN" '{"remark":"E2E-04 审核"}'
check "审核（他人）→ AUDITED" "$(PSQLQ -c "select status from biz_voucher where id='$DRAFT_ID';")" "AUDITED"
api a4_unaudit POST "/business/erp/voucher/$DRAFT_ID/un-audit" "$T_FIN" '{"reason":"E2E-04 演练：反审核验证"}'
check "反审核 → DRAFT" "$(PSQLQ -c "select status from biz_voucher where id='$DRAFT_ID';")" "DRAFT"
api a4_audit2 POST "/business/erp/voucher/$DRAFT_ID/audit" "$T_FIN" '{}'
check "再审核 → AUDITED" "$(PSQLQ -c "select status from biz_voucher where id='$DRAFT_ID';")" "AUDITED"
api a4_post POST "/business/erp/voucher/$DRAFT_ID/post" "$T_FIN" '{}'
check "记账 → POSTED" "$(PSQLQ -c "select status from biz_voucher where id='$DRAFT_ID';")" "POSTED"
check "记账后凭证借贷相等" "$(PSQLQ -c "select (total_debit=total_credit)::text from biz_voucher where id='$DRAFT_ID';")" "true"

# 红冲：voucher_reverse 流程已发布 → 必须走审批；显式 skip 被拒；审批通过后出红字凭证
api a4_rev_skip POST "/business/erp/voucher/$DRAFT_ID/reverse" "$T_ADMIN" '{"reason":"E2E-04 演练：红冲资金划转","skip_approval":true,"skip_reason":"E2E-04 演练：核对红冲分录与净额"}'
check "已发布流程时显式跳过审批被拒（10001）" "$CODE" "10001"
api a4_rev1 POST "/business/erp/voucher/$DRAFT_ID/reverse" "$T_ADMIN" '{"reason":"E2E-04 演练：红冲资金划转"}'
check "红冲送审 http" "$HTTP" "200"
check "红冲 outcome=PENDING_APPROVAL" "$(data '.data.outcome')" "PENDING_APPROVAL"
REV_IID=$(data '.data.pending_reverse_instance_id')
checkne "红冲审批实例已挂" "${REV_IID:-none}" "none"
check "送审后凭证仍 POSTED（账未动）" "$(PSQLQ -c "select status from biz_voucher where id='$DRAFT_ID';")" "POSTED"
check "送审后 1001 净额仍 1500" "$(acc_net 1001 2022-01-01 2022-01-31)" "1500.00"
api a4_rev_withdraw POST "/business/erp/voucher/$DRAFT_ID/withdraw-reverse" "$T_ADMIN" '{"reason":"E2E-04 演练：撤回红冲"}'
check "撤回红冲 http" "$HTTP" "200"
check "撤回后 pending 清空" "$(PSQLQ -c "select coalesce(pending_reverse_instance_id::text,'') from biz_voucher where id='$DRAFT_ID';")" ""
api a4_rev2 POST "/business/erp/voucher/$DRAFT_ID/reverse" "$T_ADMIN" '{"reason":"E2E-04 演练：红冲资金划转"}'
check "再次送审 http" "$HTTP" "200"
REV_IID=$(data '.data.pending_reverse_instance_id')
approve_as_any "$REV_IID" a4r
save A4_REV_IID "$REV_IID"
check "审批实例终态 APPROVED" "$(flow_state "$REV_IID")" "APPROVED"
RED_ID=$(PSQLQ -c "select id from biz_voucher where reverse_of='$DRAFT_ID' and deleted_at is null;")
checkne "红字凭证已生成" "${RED_ID:-none}" "none"
check "红字凭证 POSTED 且 is_red" "$(PSQLQ -c "select status||'/'||is_red::text from biz_voucher where id='$RED_ID';")" "POSTED/1"
check "红字凭证落当前开放期间（不回溯改历史账：2026-11）" "$(PSQLQ -c "select period from biz_voucher where id='$RED_ID';")" "2026-11"
check "红字分录 = 原分录镜像" "$(PSQLQ -c "select string_agg(e.account_code||':'||e.debit_amount||'/'||e.credit_amount,',' order by e.account_code) from biz_voucher_entry e where e.voucher_id='$RED_ID';")" "1001:0.0000/1500.0000,1002:1500.0000/0.0000"
check "蓝+红两凭证 1001 净额归零" "$(PSQLQ -c "select coalesce(sum(e.debit_amount-e.credit_amount),0)::numeric(20,2)::text from biz_voucher_entry e where e.voucher_id in ('$DRAFT_ID','$RED_ID') and e.account_code='1001';")" "0.00"
check "蓝+红两凭证 1002 净额归零" "$(PSQLQ -c "select coalesce(sum(e.debit_amount-e.credit_amount),0)::numeric(20,2)::text from biz_voucher_entry e where e.voucher_id in ('$DRAFT_ID','$RED_ID') and e.account_code='1002';")" "0.00"
check "原凭证已标记被红冲" "$(PSQLQ -c "select coalesce(reversed_by::text,'') from biz_voucher where id='$DRAFT_ID';")" "$RED_ID"
save A4_SM_DONE 1
else
  say "  · 状态机/红冲段已跑过（state 有 A4_SM_DONE），跳过"
fi

# 12 个月凭证（每月 5 张，全部 建单→审核→记账）
MANUAL_N=$(PSQLQ -c "select count(*) from biz_voucher where period like '2022-%' and deleted_at is null and voucher_type='MANUAL';")
if [ "${MANUAL_N:-0}" -ge 61 ]; then
  say "  · 2022 手工凭证已存在 $MANUAL_N 张，跳过生成"
else
  say "  · 开始生成 2022 年 12 个月凭证（每月 5 张，建单→审核→记账）…"
  for m in 01 02 03 04 05 06 07 08 09 10 11 12; do
    mkm "2022-$m" "$m" "2022-$m" || bad "2022-$m 凭证生成有失败项"
  done
  # 12 月大额采购：制造「留抵税额」（进项 > 销项）用于 FIX-6 负数重分类回归
  mkpost v12x "2022-12-29" "E2E-04 2022-12 大额采购（留抵）" "[
    {\"account_id\":\"$ACC_1405\",\"debit\":\"500000\",\"summary\":\"库存商品\"},
    {\"account_id\":\"$ACC_222101\",\"debit\":\"65000\",\"summary\":\"进项税额\"},
    {\"account_id\":\"$ACC_2202\",\"credit\":\"565000\",\"summary\":\"应付货款\"}]" >/dev/null
fi

check "2022 年 POSTED 凭证张数（60 月度 + 大额采购 + 划转 + 红字）" "$(PSQLQ -c "select count(*) from biz_voucher where period like '2022-%' and deleted_at is null and status='POSTED';")" "63"
check "2022 年凭证全部借贷平衡（不平张数）" "$(vch_unbalanced)" "0"

api a4_autopage GET "/business/erp/voucher/page?page=1&page_size=5&voucher_type=AUTO" "$T_ADMIN"
check "自动凭证查询 http" "$HTTP" "200"
check "自动凭证类型过滤无越界" "$(data '[.data.list[]|select(.voucher_type!="AUTO")]|length')" "0"
checkne "库内存在自动生成凭证" "$(data '.data.total')" "0"
api a4_page GET "/business/erp/voucher/page?page=1&page_size=5&period=2022-12" "$T_ADMIN"
check "凭证列表按期间过滤" "$(data '[.data.list[]|select(.period!="2022-12")]|length')" "0"
api a4_page_bad GET "/business/erp/voucher/page?page=1&page_size=5&status=NOPE" "$T_ADMIN"
check "非法状态过滤 → 400" "$HTTP" "400"
api a4_detail GET "/business/erp/voucher/$OPG_VOUCHER" "$T_ADMIN"
check "凭证详情分录数" "$(data '.data.entries|length')" "2"

summary
