#!/usr/bin/env bash
# E2E-05 ②③ 处置全流程：前置闸（FIX-3 回归 66048）→ 当月计提 → 提交处置 → GM 审批 →
#        3 张处置凭证（清理/收款/结转损益）→ 卡片 DISPOSED → 作废处置回滚 → 次日照提
source /opt/cloudpivot/scripts/verify/e2e05_asset_common.sh
[ -n "${T_ADMIN:-}" ] || login_admin
[ -n "${T_GM:-}" ] || login_gm

say "===== C1 FIX-3 回归：处置当月未计提 → 409 66048 ====="
api dis_gate POST "/business/erp/asset/$AS_C/dispose" "$T_ADMIN" \
  '{"disposal_type":"SALE","disposal_date":"2028-05-20","reason":"E2E-05 演练：未计提先处置（应被拦）","sale_price":"50000","output_tax":"6500"}'
check "未计提就处置 http" "$HTTP" "409"
check "未计提就处置错误码" "$CODE" "66048"
check "被拦时未创建审批实例" "$(SQ "select count(*) from biz_flow_instance where biz_no='$NO_C'")" "0"
check "被拦时卡片仍 IN_USE" "$(SQ "select status from biz_fixed_asset where id='$AS_C'")" "IN_USE"
check "被拦时未写处置留痕" "$(SQ "select count(*) from biz_fixed_asset_change where asset_id='$AS_C' and change_type='DISPOSAL'")" "0"

say
say "===== C2 计提 2028-05（处置当月照提） ====="
api dep05 POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-05","remark":"E2E-05 演练 2028-05（处置当月）"}'
check "计提 2028-05 http" "$HTTP" "200"
check "计提台数（A/C/D）" "$(data '.data.asset_count')" "3"
chknum "C 处置当月折旧 4750" "$(SQ "select amount from biz_depreciation where period_code='2028-05' and asset_id='$AS_C'")" 4750
check "C 当月明细 POSTED" "$(SQ "select status from biz_depreciation where period_code='2028-05' and asset_id='$AS_C'")" "POSTED"
chknum "C 累计（5 期）= 23750" "$(SQ "select accumulated_depreciation from biz_fixed_asset where id='$AS_C'")" 23750
chknum "C 净值 = 36250" "$(SQ "select net_value from biz_fixed_asset where id='$AS_C'")" 36250
B05=$(SQ "select id from biz_depreciation_batch where period_code='2028-05'")
save B05 "$B05"

say
say "===== C3 提交处置审批（出售 50000 + 销项 6500） ====="
api dis_submit POST "/business/erp/asset/$AS_C/dispose" "$T_ADMIN" \
  '{"disposal_type":"SALE","disposal_date":"2028-05-20","reason":"E2E-05 演练：出售 C 设备","sale_price":"50000","output_tax":"6500"}'
check "提交处置 http" "$HTTP" "200"
check "提交处置 code" "$CODE" "0"
INST=$(SQ "select id from biz_flow_instance where biz_no='$NO_C' and biz_type='ASSET_DISPOSAL' order by created_at desc limit 1")
save INST "$INST"
checkne "审批实例已创建" "$INST" ""
check "实例状态（待审批）" "$(SQ "select status from biz_flow_instance where id='$INST'")" "RUNNING"
check "审批中卡片仍 IN_USE（账未动）" "$(SQ "select status from biz_fixed_asset where id='$AS_C'")" "IN_USE"
check "审批中未生成处置凭证" "$(SQ "select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and source_no='$NO_C'")" "0"

say
say "===== C4 GM 审批通过 → 处置落账 ====="
api gm_todo GET "/business/flow/task/todo?page=1&page_size=50" "$T_GM"
check "GM 待办 http" "$HTTP" "200"
TASK=$(data ".data.list[]|select(.biz_no==\"$NO_C\")|.id" | head -1)
save TASK "$TASK"
checkne "GM 收到处置待办" "$TASK" ""
api dis_approve POST "/business/flow/task/$TASK/approve" "$T_GM" '{"comment":"E2E-05 演练：同意出售处置"}'
check "审批 http" "$HTTP" "200"
check "审批 code" "$CODE" "0"
sleep 2
check "实例 APPROVED" "$(SQ "select status from biz_flow_instance where id='$INST'")" "APPROVED"
check "卡片状态 DISPOSED" "$(SQ "select status from biz_fixed_asset where id='$AS_C'")" "DISPOSED"
check "处置方式/日期落库" "$(SQ "select disposal_type||'|'||disposal_date from biz_fixed_asset where id='$AS_C'")" "SALE|2028-05-20"
checkne "处置凭证 id 回写" "$(SQ "select disposal_voucher_id from biz_fixed_asset where id='$AS_C'")" ""
check "处置留痕（IN_USE→DISPOSED）" "$(SQ "select from_status||'→'||to_status from biz_fixed_asset_change where asset_id='$AS_C' and change_type='DISPOSAL'")" "IN_USE→DISPOSED"
check "处置留痕挂审批实例" "$(SQ "select (approval_instance_id is not null) from biz_fixed_asset_change where asset_id='$AS_C' and change_type='DISPOSAL'")" "t"
check "状态日志 DISPOSED" "$(SQ "select count(*) from biz_status_log where entity_no='$NO_C' and to_status='DISPOSED'")" "1"
check "FIXED_ASSET/DISPOSED 通知（FIX-3 规则）" "$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET' and biz_id='$AS_C' and idempotency_key like 'LNK:FIXED_ASSET:DISPOSED:%'")" "1"
check "处置不影响累计折旧缓存（=已提 5 期）" "$(SQ "select accumulated_depreciation||'|'||depreciation_months_done from biz_fixed_asset where id='$AS_C'")" "23750.0000|5"

say
say "===== C5 处置凭证三张（清理 / 收款 / 结转损益） ====="
check "处置凭证张数 = 3" "$(SQ "select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and source_no='$NO_C'")" "3"
check "三张全部借贷平衡" "$(SQ "select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and source_no='$NO_C' and total_debit=total_credit")" "3"
chknum "① 借 1602 累计折旧 = 23750" "$(SQ "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='1602'")" 23750
chknum "① 借 1606 清理 = 净值 36250" "$(SQ "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='1606' and e.debit_amount>0")" 36250
chknum "① 贷 1601 原值 = 60000" "$(SQ "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='1601'")" 60000
chknum "② 借 1002 价税合计 = 56500" "$(SQ "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='1002'")" 56500
chknum "② 贷 1606 售价 = 50000" "$(SQ "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='1606'")" 50000
chknum "② 贷 2221.02 销项税 = 6500" "$(SQ "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='2221.02'")" 6500
chknum "③ 结转：借 1606 = 13750" "$(SQ "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='1606' and e.debit_amount>0 and e.credit_amount=0")" 13750
chknum "③ 结转：贷 5301 营业外收入 = 13750" "$(SQ "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='5301'")" 13750
chknum "1606 清理科目净额归零" "$(SQ "select coalesce(sum(e.debit_amount-e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='1606'")" 0
chknum "处置损益 = 售价 50000 − 净值 36250" "$(SQ "select coalesce(sum(e.credit_amount-e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_C' and e.account_code='5301'")" 13750

say
say "===== C6 处置后卡片锁定（改 / 删 / 转移 / 再处置） ====="
api dis_upd PUT "/business/erp/asset/$AS_C" "$T_ADMIN" '{"asset_name":"E2E05 已处置改名"}'
check "已处置改卡 http" "$HTTP" "409"
api dis_del DELETE "/business/erp/asset/$AS_C" "$T_ADMIN"
check "已处置删卡 http" "$HTTP" "409"
api dis_tr POST "/business/erp/asset/$AS_C/transfer" "$T_ADMIN" "{\"to_dept_id\":\"$DEPT1\",\"reason\":\"已处置再转移\"}"
check "已处置转移 http" "$HTTP" "409"
api dis_again POST "/business/erp/asset/$AS_C/dispose" "$T_ADMIN" \
  '{"disposal_type":"SALE","disposal_date":"2028-05-21","reason":"重复处置","sale_price":"1","output_tax":"0"}'
check "已处置再处置 http" "$HTTP" "409"
check "已处置再处置错误码" "$CODE" "66040"

say
say "===== C7 资产 D：报废处置 → 作废处置（红冲 + 恢复在役） ====="
api dis_d_submit POST "/business/erp/asset/$AS_D/dispose" "$T_ADMIN" \
  '{"disposal_type":"SCRAP","disposal_date":"2028-05-25","reason":"E2E-05 演练：D 设备报废"}'
check "D 报废提交 http" "$HTTP" "200"
NO_D=$(SQ "select asset_no from biz_fixed_asset where id='$AS_D'")
save NO_D "$NO_D"
api gm_todo2 GET "/business/flow/task/todo?page=1&page_size=50" "$T_GM"
TASK_D=$(data ".data.list[]|select(.biz_no==\"$NO_D\")|.id" | head -1)
save TASK_D "$TASK_D"
checkne "D 处置待办" "$TASK_D" ""
api dis_d_approve POST "/business/flow/task/$TASK_D/approve" "$T_GM" '{"comment":"E2E-05 演练：同意报废"}'
check "D 报废审批 http" "$HTTP" "200"
sleep 2
check "D 状态 DISPOSED" "$(SQ "select status from biz_fixed_asset where id='$AS_D'")" "DISPOSED"
chknum "报废：借 1602 = 1187.50（5 期）" "$(SQ "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_D' and e.account_code='1602'")" 1187.5
chknum "报废：借 5711 营业外支出 = 净值 1812.50" "$(SQ "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_D' and e.account_code='5711'")" 1812.5
api dis_cancel POST "/business/erp/asset/$AS_D/dispose/cancel" "$T_ADMIN" '{"reason":"E2E-05 演练：作废处置，设备留用"}'
check "作废处置 http" "$HTTP" "200"
check "作废后状态恢复 IN_USE" "$(SQ "select status from biz_fixed_asset where id='$AS_D'")" "IN_USE"
check "作废后清 disposal 字段" "$(SQ "select coalesce(disposal_type,'NULL')||'|'||coalesce(disposal_date::text,'NULL') from biz_fixed_asset where id='$AS_D'")" "NULL|NULL"
check "作废留痕（DISPOSED→IN_USE）" "$(SQ "select from_status||'→'||to_status from biz_fixed_asset_change where asset_id='$AS_D' and change_type='ENABLE' order by created_at desc limit 1")" "DISPOSED→IN_USE"
check "作废生成红字凭证" "$(SQ "select count(*) from biz_voucher where source_type='FIXED_ASSET_DISPOSAL' and source_no='$NO_D' and is_red=1")" "1"
chknum "作废后该卡处置凭证净额归零（1601/1602/5711）" "$(SQ "select coalesce(sum(e.debit_amount-e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='FIXED_ASSET_DISPOSAL' and v.source_no='$NO_D' and e.account_code in ('1601','1602','5711','1606')")" 0
check "作废通知（IN_USE 事件）" "$(SQ "select count(*) from sys_notice where biz_type='FIXED_ASSET' and biz_id='$AS_D' and idempotency_key like 'LNK:FIXED_ASSET:IN_USE:%'")" "1"

say
say "===== C8 作废后继续计提 2028-06（恢复在役照提） ====="
api dep06 POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-06","remark":"E2E-05 演练 2028-06"}'
check "计提 2028-06 http" "$HTTP" "200"
check "计提台数（A + 恢复的 D；C 已处置剔除）" "$(data '.data.asset_count')" "2"
chknum "A 6 月折旧 1900" "$(SQ "select amount from biz_depreciation where period_code='2028-06' and asset_id='$AS_A'")" 1900
chknum "D 作废后 6 月照提 237.50" "$(SQ "select amount from biz_depreciation where period_code='2028-06' and asset_id='$AS_D'")" 237.5
check "C 在 6 月无明细（已处置）" "$(SQ "select count(*) from biz_depreciation where period_code='2028-06' and asset_id='$AS_C'")" "0"
check "全库超提行数 = 0" "$(SQ "select count(*) from biz_fixed_asset where accumulated_depreciation > depreciable_base")" "0"
check "全库累计 = 明细合计（K2）" "$(SQ "select count(*) from (select a.id from biz_fixed_asset a left join biz_depreciation d on d.asset_id=a.id and d.status='POSTED' group by a.id, a.accumulated_depreciation having a.accumulated_depreciation <> coalesce(sum(d.amount),0)) t")" "0"

say
say "===== C9 跨表勾稽（1601/1602 ↔ 卡片台账） ====="
chknum "1601 净额 = 在册卡片原值（未处置、未删除）" \
  "$(SQ "select coalesce((select sum(debit_amount-credit_amount) from biz_voucher_entry where account_code='1601'),0) - coalesce((select sum(original_value) from biz_fixed_asset where deleted_at is null and status<>'DISPOSED'),0)")" 0
chknum "1602 净额 = 在册卡片累计折旧（未处置、未删除）" \
  "$(SQ "select coalesce((select sum(debit_amount-credit_amount) from biz_voucher_entry where account_code='1602'),0) - coalesce((select sum(accumulated_depreciation) from biz_fixed_asset where deleted_at is null and status<>'DISPOSED'),0)")" 0
check "全库不平衡凭证 = 0" "$(SQ "select count(*) from biz_voucher where deleted_at is null and total_debit <> total_credit")" "0"
check "1606 清理科目全库净额 = 0" "$(SQ "select count(*) from biz_voucher_entry where account_code='1606' and voucher_id in (select id from biz_voucher where deleted_at is null) group by account_code having sum(debit_amount-credit_amount) <> 0")" ""
check "每张凭证 total = 分录合计" "$(SQ "select count(*) from (select v.id from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id where v.deleted_at is null group by v.id, v.total_debit, v.total_credit having v.total_debit <> sum(e.debit_amount) or v.total_credit <> sum(e.credit_amount)) t")" "0"

say
summary
