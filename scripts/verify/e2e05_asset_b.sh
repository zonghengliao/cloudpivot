#!/usr/bin/env bash
# E2E-05 ③ 折旧计提：月度批次 → 过账 → 凭证（按部门汇总）/ 红冲 → 重跑 → 尾差 → 提足即停
source /opt/cloudpivot/scripts/verify/e2e05_asset_common.sh
[ -n "${T_ADMIN:-}" ] || login_admin

say "===== B0 期间闸 + 参数校验（负例） ====="
api dep_badfmt POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-13"}'
check "期间 2028-13 http" "$HTTP" "400"
api dep_closed POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2026-09"}'
check "已结账期间 http" "$HTTP" "409"
check "已结账期间错误码" "$CODE" "66038"
api dep_noopen POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2032-01"}'
check "未开账期间 http" "$HTTP" "409"
check "未开账期间错误码" "$CODE" "66038"

say
say "===== B1 计提 2028-01（4 台，含 4 种口径） ====="
api dep01 POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-01","remark":"E2E-05 演练 2028-01"}'
check "计提 2028-01 http" "$HTTP" "200"
check "计提结果 outcome" "$(data '.data.outcome')" "POSTED"
check "计提台数" "$(data '.data.asset_count')" "4"
chknum "批次合计 = 1900+316.6667+4750+237.50" "$(data '.data.total_amount')" 7204.1667
B01=$(SQ "select id from biz_depreciation_batch where period_code='2028-01'")
V01=$(SQ "select voucher_id from biz_depreciation_batch where period_code='2028-01'")
save B01 "$B01"; save V01 "$V01"
check "批次状态" "$(SQ "select status from biz_depreciation_batch where id='$B01'")" "POSTED"
check "明细行数" "$(SQ "select count(*) from biz_depreciation where batch_id='$B01'")" "4"
chknum "A 首月折旧 1900" "$(SQ "select amount from biz_depreciation where batch_id='$B01' and asset_no='$NO_A'")" 1900
chknum "B 首月折旧 316.6667" "$(SQ "select amount from biz_depreciation where batch_id='$B01' and asset_id='$AS_B'")" 316.6667
chknum "C 首月折旧 4750" "$(SQ "select amount from biz_depreciation where batch_id='$B01' and asset_id='$AS_C'")" 4750
chknum "D 首月折旧 237.50" "$(SQ "select amount from biz_depreciation where batch_id='$B01' and asset_id='$AS_D'")" 237.5
check "A 期后累计 = 本期" "$(SQ "select accumulated_after||'|'||net_value_after from biz_depreciation where batch_id='$B01' and asset_no='$NO_A'")" "1900.0000|118100.0000"
check "A 卡片缓存回写（累计/月数/期间）" "$(SQ "select accumulated_depreciation||'|'||depreciation_months_done||'|'||last_depreciation_period||'|'||net_value from biz_fixed_asset where asset_no='$NO_A'")" "1900.0000|1|2028-01|118100.0000"
check "B 明细带部门维度" "$(SQ "select dept_name||'|'||expense_account_code from biz_depreciation where batch_id='$B01' and asset_id='$AS_B'")" "演练设备部|5602.01"
check "明细状态全部 POSTED" "$(SQ "select count(*) from biz_depreciation where batch_id='$B01' and status='POSTED'")" "4"

say
say "===== B2 折旧凭证 2028-01（借 5602.01 按部门汇总 / 贷 1602） ====="
check "凭证来源类型" "$(SQ "select source_type from biz_voucher where id='$V01'")" "DEPRECIATION"
check "凭证期间 / 日期" "$(SQ "select period||'|'||voucher_date from biz_voucher where id='$V01'")" "2028-01|2028-01-31"
check "凭证明细 2 行（借 1 行 + 贷 1 行）" "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V01'")" "2"
chknum "借方 5602.01 合计 = 批次合计" "$(SQ "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id='$V01' and account_code='5602.01'")" 7204.1667
chknum "贷方 1602 = 批次合计" "$(SQ "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$V01' and account_code='1602'")" 7204.1667
check "凭证借贷平衡" "$(SQ "select (total_debit=total_credit) from biz_voucher where id='$V01'")" "t"
# E2E05-FIX-1（卡 t_abb4bb0b）修好后的口径：补提历史期间按**期间归属部门**归集，不再用卡片当前部门。
# A 已于 2028-02-01 转到采购部，但 2028-01 那一期它还在设备部 ⇒ 借方只剩「演练设备部」一行，
# 金额 = 全部四台（A+B+C+D）。修前这里断言的是「采购演练部 1900 / 设备部 5304.1667」（bug 现场）。
check "借方按部门只拆 1 行（补提按期间归属部门）" "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V01' and debit_amount>0")" "1"
chknum "借方 演练设备部（A+B+C+D）= 7204.1667" "$(SQ "select debit_amount from biz_voucher_entry where voucher_id='$V01' and debit_amount>0 and aux_dept_id='$DEPT1'")" 7204.1667
check "借方没有采购演练部的行（A 那一期还没转）" "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V01' and debit_amount>0 and aux_dept_id='$DEPT2'")" "0"
check "A 明细的部门 = 演练设备部（期间归属部门，不是卡片当前部门）" \
  "$(SQ "select dept_name from biz_depreciation where batch_id='$B01' and asset_no='$NO_A'")" "演练设备部"

say
say "===== B3 幂等：同期间二次计提 ====="
api dep01b POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-01"}'
check "二次计提 http" "$HTTP" "200"
check "二次计提 outcome" "$(data '.data.outcome')" "EXISTS"
check "期间仍只有 1 个批次" "$(SQ "select count(*) from biz_depreciation_batch where period_code='2028-01'")" "1"
check "不新增明细（仍 4 行）" "$(SQ "select count(*) from biz_depreciation where period_code='2028-01'")" "4"

say
say "===== B4 红冲 2028-01（红字凭证 + 明细 REVERSED + 卡片回滚） ====="
api dep01rev POST "/business/erp/depreciation/$B01/reverse" "$T_ADMIN" '{"reason":"E2E-05 演练：红冲 2028-01 校验回滚"}'
check "红冲 http" "$HTTP" "200"
check "批次状态" "$(SQ "select status from biz_depreciation_batch where id='$B01'")" "REVERSED"
checkne "红字凭证 id 已记" "$(SQ "select reverse_voucher_id from biz_depreciation_batch where id='$B01'")" ""
check "红字凭证 is_red" "$(SQ "select is_red||'|'||status from biz_voucher where id=(select reverse_voucher_id from biz_depreciation_batch where id='$B01')")" "1|POSTED"
check "红字凭证借贷平衡" "$(SQ "select (total_debit=total_credit) from biz_voucher where id=(select reverse_voucher_id from biz_depreciation_batch where id='$B01')")" "t"
check "明细全部 REVERSED" "$(SQ "select count(*) from biz_depreciation where batch_id='$B01' and status='REVERSED'")" "4"
check "A 卡片回滚（累计 0 / 月数 0）" "$(SQ "select accumulated_depreciation||'|'||depreciation_months_done from biz_fixed_asset where asset_no='$NO_A'")" "0.0000|0"
chknum "A 净值回原值" "$(SQ "select net_value from biz_fixed_asset where asset_no='$NO_A'")" 120000
chknum "本卡折旧凭证 1602 净额回到 0（红冲后）" "$(SQ "select coalesce(sum(debit_amount-credit_amount),0) from biz_voucher_entry where account_code='1602' and voucher_id in (select id from biz_voucher where source_type='DEPRECIATION' and source_id in (select id from biz_depreciation_batch where period_code between '2028-01' and '2028-06') union select reverse_voucher_id from biz_depreciation_batch where period_code between '2028-01' and '2028-06' and reverse_voucher_id is not null)")" 0

say
say "===== B5 红冲后重跑 2028-01（复用同一批次行） ====="
api dep01c POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-01","remark":"E2E-05 重跑"}'
check "重跑 http" "$HTTP" "200"
check "重跑 outcome（红冲后重跑专用码）" "$(data '.data.outcome')" "REVERSED_RERUN"
check "复用同一批次行（id 不变）" "$(SQ "select id from biz_depreciation_batch where period_code='2028-01'")" "$B01"
check "批次回到 POSTED" "$(SQ "select status from biz_depreciation_batch where id='$B01'")" "POSTED"
check "明细回到 POSTED" "$(SQ "select count(*) from biz_depreciation where batch_id='$B01' and status='POSTED'")" "4"
checkne "新凭证（与原凭证不同号）" "$(SQ "select voucher_id from biz_depreciation_batch where id='$B01'")" "$V01"
check "A 卡片重新计提（1900 / 1 期）" "$(SQ "select accumulated_depreciation||'|'||depreciation_months_done from biz_fixed_asset where asset_no='$NO_A'")" "1900.0000|1"

say
say "===== B6 D 卡停用（IDLE）→ 计提 2028-02：停用照提折旧 ====="
api st_idle_d POST "/business/erp/asset/$AS_D/status" "$T_ADMIN" \
  '{"target_status":"IDLE","change_date":"2028-02-01","reason":"E2E-05 演练：D 设备停用（验照提）"}'
check "D 停用 http" "$HTTP" "200"
check "D 状态 IDLE" "$(SQ "select status from biz_fixed_asset where id='$AS_D'")" "IDLE"
api dep02 POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-02","remark":"E2E-05 演练 2028-02（A 已转采购部 / D 闲置）"}'
check "计提 2028-02 http" "$HTTP" "200"
check "计提台数（D 闲置仍命中也含 B/C）" "$(data '.data.asset_count')" "4"
chknum "D 闲置期间照提 237.50" "$(SQ "select amount from biz_depreciation where period_code='2028-02' and asset_id='$AS_D'")" 237.5
check "D 闲置状态未被打回" "$(SQ "select status from biz_fixed_asset where id='$AS_D'")" "IDLE"
B02=$(SQ "select id from biz_depreciation_batch where period_code='2028-02'")
V02=$(SQ "select voucher_id from biz_depreciation_batch where period_code='2028-02'")
save B02 "$B02"; save V02 "$V02"
check "2 月借方按部门拆 2 行（A 已转采购部）" "$(SQ "select count(*) from biz_voucher_entry where voucher_id='$V02' and debit_amount>0")" "2"
chknum "借方 采购演练部（A）= 1900" "$(SQ "select debit_amount from biz_voucher_entry where voucher_id='$V02' and debit_amount>0 and aux_dept_id='$DEPT2'")" 1900
chknum "借方 演练设备部（B+C+D）= 5304.1667" "$(SQ "select debit_amount from biz_voucher_entry where voucher_id='$V02' and debit_amount>0 and aux_dept_id='$DEPT1'")" 5304.1667
chknum "贷方 1602 = 批次合计" "$(SQ "select coalesce(sum(credit_amount),0) from biz_voucher_entry where voucher_id='$V02' and account_code='1602'")" 7204.1667
ensure_asset_att "$AS_D" d
api st_use_d POST "/business/erp/asset/$AS_D/status" "$T_ADMIN" \
  '{"target_status":"IN_USE","change_date":"2028-02-20","reason":"E2E-05 演练：D 设备重新启用"}'
check "D 启用 http" "$HTTP" "200"
check "D 状态 IN_USE" "$(SQ "select status from biz_fixed_asset where id='$AS_D'")" "IN_USE"

say
say "===== B7 计提 2028-03：B（3 期）末月尾差补齐 ====="
api dep03 POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-03","remark":"E2E-05 演练 2028-03（B 末月）"}'
check "计提 2028-03 http" "$HTTP" "200"
check "计提台数" "$(data '.data.asset_count')" "4"
chknum "B 末月金额 316.6666（尾差补齐）" "$(SQ "select amount from biz_depreciation where period_code='2028-03' and asset_id='$AS_B'")" 316.6666
check "B 末月标记 is_last_month" "$(SQ "select is_last_month from biz_depreciation where period_code='2028-03' and asset_id='$AS_B'")" "1"
chknum "B 累计 = 可折旧总额 950" "$(SQ "select accumulated_depreciation from biz_fixed_asset where id='$AS_B'")" 950
chknum "B 净值 = 残值 50" "$(SQ "select net_value from biz_fixed_asset where id='$AS_B'")" 50
check "B 已提 3 期" "$(SQ "select depreciation_months_done||'|'||last_depreciation_period from biz_fixed_asset where id='$AS_B'")" "3|2028-03"
B03=$(SQ "select id from biz_depreciation_batch where period_code='2028-03'")
save B03 "$B03"
chknum "3 月批次合计" "$(SQ "select total_amount from biz_depreciation_batch where id='$B03'")" 7204.1666
chknum "3 月借方三台合计（B 尾差）" "$(SQ "select coalesce(sum(debit_amount),0) from biz_voucher_entry where voucher_id=(select voucher_id from biz_depreciation_batch where id='$B03') and account_code='5602.01'")" 7204.1666

say
say "===== B8 计提 2028-04：提足即停（B 不再命中）+ 不能超提 ====="
api dep04 POST /business/erp/depreciation/run "$T_ADMIN" '{"period_code":"2028-04","remark":"E2E-05 演练 2028-04"}'
check "计提 2028-04 http" "$HTTP" "200"
check "计提台数（B 提足剔除）" "$(data '.data.asset_count')" "3"
check "B 在 4 月无明细" "$(SQ "select count(*) from biz_depreciation where period_code='2028-04' and asset_id='$AS_B'")" "0"
check "B 全生命周期明细行数 = 3" "$(SQ "select count(*) from biz_depreciation where asset_id='$AS_B'")" "3"
check "全库超提行数 = 0（累计 ≤ 可折旧总额）" "$(SQ "select count(*) from biz_fixed_asset where accumulated_depreciation > depreciable_base")" "0"
check "全库累计 = 明细合计（K2 缓存对账）" "$(SQ "select count(*) from (select a.id, a.accumulated_depreciation, coalesce(sum(d.amount),0) as detail from biz_fixed_asset a left join biz_depreciation d on d.asset_id=a.id and d.status='POSTED' group by a.id, a.accumulated_depreciation having a.accumulated_depreciation <> coalesce(sum(d.amount),0)) t")" "0"
check "全库折旧凭证 0 张不平" "$(SQ "select count(*) from biz_voucher where source_type='DEPRECIATION' and total_debit <> total_credit")" "0"
check "每期凭证金额 = 批次合计" "$(SQ "select count(*) from biz_depreciation_batch b join biz_voucher v on v.id=b.voucher_id where b.status='POSTED' and v.total_debit <> b.total_amount")" "0"

say
say "===== B9 批次页 / 明细页接口 ====="
api dep_page GET "/business/erp/depreciation/page?page=1&page_size=50&period_code=2028-03" "$T_ADMIN"
check "批次分页 http" "$HTTP" "200"
check "期间过滤只回 1 期" "$(data '.data.total')" "1"
api dep_det GET "/business/erp/depreciation/$B03" "$T_ADMIN"
check "批次详情 http" "$HTTP" "200"
check "详情明细 4 行（3 月在提 4 台）" "$(data '.data.details|length')" "4"
chknum "详情合计 = 批次金额" "$(data '.data.batch.total_amount')" 7204.1666

say
summary
