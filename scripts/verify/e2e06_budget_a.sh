#!/usr/bin/env bash
# E2E-06 阶段 A：① 费用类型 ② 预算编制（DRAFT→SUBMITTED→ACTIVE）③ 预算科目映射
# 跑在 145（真后端 127.0.0.1:8033 + cloudpivot_dev）。断言累计到 /tmp/e2e06/asserts.log。
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e06_budget_common.sh"

say "==================== E2E-06 A：费用类型 + 预算编制 + 科目映射 ===================="
login_all
ensure_fixtures
login_rest
ensure_periods

ACC_TRAVEL=$(acc 5602.02); ACC_OTHER=$(acc 5602.09); ACC_OFFICE=$(acc 5602.03); ACC_TRAIN=$(acc 5602.06)
save ACC_TRAVEL "$ACC_TRAVEL"; save ACC_OTHER "$ACC_OTHER"; save ACC_OFFICE "$ACC_OFFICE"; save ACC_TRAIN "$ACC_TRAIN"
say "  · 科目：5602.02=$ACC_TRAVEL 5602.09=$ACC_OTHER 5602.03=$ACC_OFFICE 5602.06=$ACC_TRAIN"

# ============ ① 费用类型 ============
say
say "① 费用类型（建类型 → 关科目 / 可抵扣 / 受预算）"
api et_page GET "/business/erp/expense-type/page?page=1&page_size=50" "$T_ADMIN"
check "费用类型列表 http" "$HTTP" "200"
check "费用类型列表 code" "$CODE" "0"
say "  · 现有类型 $(data '.data.list|length') 条"

api et_create POST /business/erp/expense-type "$T_ADMIN" \
  "{\"type_code\":\"E2E06_TRAVEL\",\"type_name\":\"E2E06 差旅费（演练）\",\"account_id\":\"$ACC_TRAVEL\",\"tax_deductible\":true,\"budget_controlled\":true,\"need_invoice_no\":true,\"amount_limit\":\"0\",\"sort_order\":90,\"status\":1,\"remark\":\"E2E-06 演练类型\"}"
check "建费用类型 http" "$HTTP" "200"
check "建费用类型 code" "$CODE" "0"
TYPE_ID=$(data '.data.id'); save TYPE_OTHER_ID "$TYPE_ID"
check "类型落库（科目/可抵扣/受预算）" \
  "$(SQ "select account_code||'|'||tax_deductible||'|'||budget_controlled||'|'||need_invoice_no from biz_expense_type where id='$TYPE_ID'::uuid")" \
  "5602.02|1|1|1"

api et_dup POST /business/erp/expense-type "$T_ADMIN" \
  "{\"type_code\":\"E2E06_TRAVEL\",\"type_name\":\"重码\",\"account_id\":\"$ACC_TRAVEL\",\"tax_deductible\":true,\"budget_controlled\":true,\"need_invoice_no\":true,\"amount_limit\":\"0\",\"sort_order\":91,\"status\":1}"
check "重复编码 $HTTP" "$HTTP" "409"

api et_update PUT "/business/erp/expense-type/$TYPE_ID" "$T_ADMIN" \
  "{\"type_code\":null,\"type_name\":\"E2E06 差旅费（改名）\",\"account_id\":\"$ACC_TRAVEL\",\"tax_deductible\":true,\"budget_controlled\":true,\"need_invoice_no\":false,\"amount_limit\":\"5000\",\"sort_order\":90,\"status\":1,\"remark\":\"E2E-06 演练类型（已改）\"}"
check "改费用类型 http" "$HTTP" "200"
check "改动落库（名称/上限/发票号非必填）" \
  "$(SQ "select type_name||'|'||amount_limit||'|'||need_invoice_no from biz_expense_type where id='$TYPE_ID'::uuid")" \
  "E2E06 差旅费（改名）|5000.0000|0"

api et_leaf POST /business/erp/expense-type "$T_ADMIN" \
  "{\"type_code\":\"E2E06_BAD\",\"type_name\":\"非末级科目\",\"account_id\":\"$(SQ "select id from biz_account where account_code='5602'")\",\"tax_deductible\":true,\"budget_controlled\":true,\"need_invoice_no\":true,\"amount_limit\":\"0\",\"sort_order\":99,\"status\":1}"
check "非末级科目被拒 $HTTP" "$HTTP" "400"

# ============ ② 预算编制 ============
say
say "② 预算编制（部门×科目×期间 → 审批 → 生效）"

# ---- ②-1 草稿（5602.02 差旅费，FY2027，12 万）----
PERIODS_A=$(jq -nc '[range(1;13) | {month: ., amount: "10000"}]')
api bg_draft POST /business/erp/budget "$T_P3A" \
  "{\"fiscal_year\":2027,\"dept_id\":\"$DEPT_ID\",\"account_id\":\"$ACC_TRAVEL\",\"total_amount\":\"120000\",\"periods\":$PERIODS_A,\"remark\":\"E2E-06 差旅费预算（WARN 继承全局）\"}"
check "建预算（草稿）http" "$HTTP" "200"
check "建预算 code" "$CODE" "0"
BG_A=$(data '.data.budget.id'); save BG_A "$BG_A"
check "草稿状态" "$(data '.data.budget.status')" "DRAFT"
check "月度分解 12 行" "$(data '.data.periods|length')" "12"
check "12 月合计=年度额度" "$(SQ "select sum(amount)::text from biz_budget_period where budget_id='$BG_A'::uuid")" "120000.0000"
check "单号形态 BDG-" "$(printf '%s' "$(data '.data.budget.budget_no')" | cut -c1-4)" "BDG-"
check "草稿无 effective_from（边距：DRAFT 不参与控制）" \
  "$(SQ "select coalesce(effective_from::text,'NULL') from biz_budget where id='$BG_A'::uuid")" "NULL"

# ---- ②-2 FIX-2 回归：DRAFT 预算不控制（只提示）----
say "  · FIX-2 回归：DRAFT 预算不参与控制，只在详情/提交里给 budget_warnings"
api ex_draft POST /business/erp/expense "$T_CLERK" \
  "{\"title\":\"E2E06 DRAFT 预算期消费\",\"expense_date\":\"2027-02-08\",\"dept_id\":\"$DEPT_ID\",\"items\":[{\"expense_type_id\":\"$TYPE_ID\",\"with_tax_amount\":\"3000\",\"tax_rate\":\"0.06\",\"summary\":\"DRAFT 期差旅\",\"invoice_no\":\"INV-E2E06-001\",\"invoice_type\":\"VAT_SPECIAL\"}]}"
check "建报销单（DRAFT 期）http" "$HTTP" "200"
EXP_D=$(data '.data.report.id'); save EXP_D "$EXP_D"
check "建单回包无预算告警字段也 200" "$CODE" "0"
exp_att "$EXP_D" draft
api ex_draft_sub POST "/business/erp/expense/$EXP_D/submit" "$T_CLERK" '{}'
check "DRAFT 预算下提交 http" "$HTTP" "200"
check "DRAFT 预算不阻断（不是 409）" "$CODE" "0"
check "占用行 over_budget=0（未生效预算不算超支）" \
  "$(SQ "select coalesce(string_agg(status||'/'||over_budget||'/'||coalesce(over_amount::text,'NULL'),',') ,'-') from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP_D'::uuid")" \
  "OCCUPIED/0/NULL"
WARN_CODES=$(curl -s -m 30 "$BASE/business/erp/expense/$EXP_D" --oauth2-bearer "$T_CLERK" | jq -r '[.data.budget_warnings[]?.code]|join(",")')
check "详情带回「预算未生效」提示" "$WARN_CODES" "BUDGET_NOT_EFFECTIVE"
check "提示里的预算状态是 DRAFT" \
  "$(curl -s -m 30 "$BASE/business/erp/expense/$EXP_D" --oauth2-bearer "$T_CLERK" | jq -r '[.data.budget_warnings[]?.status]|join(",")')" "DRAFT"
api ex_draft_wd POST "/business/erp/expense/$EXP_D/withdraw" "$T_CLERK" '{}'
check "撤回 http" "$HTTP" "200"
check "撤回后占用释放" \
  "$(SQ "select status from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP_D'::uuid")" "RELEASED"

# ---- ②-3 提交审批 → 部门主管（或签）→ 财务经理 → ACTIVE ----
api bg_submit POST "/business/erp/budget/$BG_A/submit" "$T_P3A" '{}'
check "预算提交 http" "$HTTP" "200"
check "提交后状态" "$(SQ "select status from biz_budget where id='$BG_A'::uuid")" "SUBMITTED"

# 卡面验收②：A 部门的预算，n_dept **只**派给 A 部门主管一人（修好前是全体 DEPT_LEADER 会签）
NDEPT_ROW=$(tasks_of BUDGET_APPROVE "$BG_A" PENDING | grep '|n_dept|' || true)
check "n_dept 恰有 1 条待办（不再全员会签）" "$(printf '%s\n' "$NDEPT_ROW" | grep -c .)" "1"
check "n_dept 待办归属本部门主管 e2e06_mgr" \
  "$(printf '%s' "$NDEPT_ROW" | awk -F'|' '{print $3}')" "e2e06_mgr"
check "其他 DEPT_LEADER 成员没有被派到 n_dept（部门维度不再错配）" \
  "$(SQ "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id join sys_user u on u.id=t.assignee_id join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where i.biz_id='$BG_A'::uuid and t.node_key='n_dept' and r.role_code='DEPT_LEADER' and u.username<>'e2e06_mgr'")" "0"
check "n_dept 审批人来源=INITIATOR_SUPERIOR（发起人所在部门）" \
  "$(SQ "select n.assignee_type from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='budget_approve' and v.is_current and n.node_key='n_dept'")" "INITIATOR_SUPERIOR"
check "n_dept 审批模式=ANY（或签，一人同意即通过）" \
  "$(SQ "select n.approval_mode from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code='budget_approve' and v.is_current and n.node_key='n_dept'")" "ANY"

api bg_self POST "/business/flow/task/$(tasks_of BUDGET_APPROVE "$BG_A" PENDING | head -1 | cut -d'|' -f1)/approve" "$T_P3A" '{"comment":"编制人自批"}'
check "编制人自批被内控拒绝 $HTTP" "$HTTP" "403"
check "编制人自批被内控拒绝 code=65019" "$CODE" "65019"

budget_active "$BG_A" bgA
check "预算实例终态" \
  "$(SQ "select status from biz_flow_instance where biz_type='BUDGET_APPROVE' and biz_id='$BG_A'::uuid")" "APPROVED"
check "预算状态 ACTIVE" "$(SQ "select status from biz_budget where id='$BG_A'::uuid")" "ACTIVE"
EF=$(SQ "select effective_from::text from biz_budget where id='$BG_A'::uuid")
save EF_A "$EF"
check "effective_from = 审批次月 1 日" "$EF" "$(date -d "$(date +%Y-%m-01) +1 month" +%Y-%m-01)"
check "预算表零凭证（审批不落账）" \
  "$(SQ "select count(*) from biz_voucher where source_type like 'BUDGET%'")" "0"

# ---- ②-4 BLOCK 槽（5602.09，1.2 万/年 = 1000/月）----
PERIODS_B=$(jq -nc '[range(1;13) | {month: ., amount: "1000"}]')
api bg_block POST /business/erp/budget "$T_P3A" \
  "{\"fiscal_year\":2027,\"dept_id\":\"$DEPT_ID\",\"account_id\":\"$ACC_OTHER\",\"total_amount\":\"12000\",\"control_mode\":\"BLOCK\",\"periods\":$PERIODS_B,\"remark\":\"E2E-06 其他费用（槽级 BLOCK）\"}"
check "建 BLOCK 预算 http" "$HTTP" "200"
BG_B=$(data '.data.budget.id'); save BG_B "$BG_B"
check "槽级策略落库 BLOCK" "$(SQ "select control_mode from biz_budget where id='$BG_B'::uuid")" "BLOCK"
api bgb_submit POST "/business/erp/budget/$BG_B/submit" "$T_P3A" '{}'
check "BLOCK 预算提交 http" "$HTTP" "200"
budget_active "$BG_B" bgB
check "BLOCK 预算 ACTIVE" "$(SQ "select status from biz_budget where id='$BG_B'::uuid")" "ACTIVE"

# ---- ②-5 effective_from 未到生效日（FY2023，同一账号 5602.02）----
api bg_eff POST /business/erp/budget "$T_P3A" \
  "{\"fiscal_year\":2023,\"dept_id\":\"$DEPT_ID\",\"account_id\":\"$ACC_TRAVEL\",\"total_amount\":\"60000\",\"periods\":$(jq -nc '[range(1;13)|{month:.,amount:"5000"}]'),\"remark\":\"E2E-06 生效日回归（FY2023）\"}"
check "建 FY2023 预算 http" "$HTTP" "200"
BG_E=$(data '.data.budget.id'); save BG_E "$BG_E"
api bge_submit POST "/business/erp/budget/$BG_E/submit" "$T_P3A" '{}'
budget_active "$BG_E" bgE
check "FY2023 预算 ACTIVE" "$(SQ "select status from biz_budget where id='$BG_E'::uuid")" "ACTIVE"
check "FY2023 生效日 = 审批次月 1 日（> 2023 期间月末）" "$(SQ "select effective_from::text from biz_budget where id='$BG_E'::uuid")" "$EF"

# ---- ②-5b FIX-2 生效日回归：期间早于 effective_from 时预算「不生效、不控制」 ----
say "  · FIX-2 回归：2023-11 期间早于 effective_from（2026-12-01）→ 该版本对 2023-11 不生效"
api ex_eff POST /business/erp/expense "$T_CLERK" \
  "{\"title\":\"E2E06 生效日前的费用\",\"expense_date\":\"2023-11-20\",\"dept_id\":\"$DEPT_ID\",\"items\":[{\"expense_type_id\":\"$TYPE_ID\",\"with_tax_amount\":\"2000\",\"tax_rate\":\"0.06\",\"summary\":\"生效日前差旅（不应被预算管）\",\"invoice_no\":\"INV-E2E06-003\",\"invoice_type\":\"VAT_SPECIAL\"}]}"
check "建单（2023-11）http" "$HTTP" "200"
EXP_E=$(data '.data.report.id'); save EXP_E "$EXP_E"
exp_att "$EXP_E" eff >/dev/null
api ex_eff_sub POST "/business/erp/expense/$EXP_E/submit" "$T_CLERK" '{}'
check "提交（生效日前）http" "$HTTP" "200"
check "未到生效日 → missing_budget=true 且不标超支" \
  "$(SQ "select missing_budget::text||'|'||over_budget::text||'|'||coalesce(over_amount::text,'NULL') from biz_expense_report where id='$EXP_E'::uuid")" \
  "1|0|NULL"
EFF_WARN=$(curl -s -m 30 "$BASE/business/erp/expense/$EXP_E" --oauth2-bearer "$T_CLERK" | jq -r '[.data.budget_warnings[]?|"\(.status)|\(.status_label)"]|join(",")')
check "提示状态 ACTIVE + 「已生效（未到生效日）」" "$EFF_WARN" "ACTIVE|已生效（未到生效日）"
api ex_eff_wd POST "/business/erp/expense/$EXP_E/withdraw" "$T_CLERK" '{}'
check "撤回（生效日前单）http" "$HTTP" "200"

# 同一槽的第二版：ACTIVE 之后再调整一版 → 旧版 SUPERSEDED（红线 6 / 唯一索引兜底）
say "  · 调整：ACTIVE 后只能新增版本（旧版 SUPERSEDED）"
api bg_adjust POST "/business/erp/budget/$BG_A/adjust" "$T_P3A" \
  "{\"total_amount\":\"150000\",\"periods\":$(jq -nc '[range(1;13)|{month:.,amount:"12500"}]'),\"change_reason\":\"E2E-06 追加差旅额度（演练）\"}"
check "调整（新增版本）http" "$HTTP" "200"
check "调整返回新版本号 2" "$(data '.data.budget.version_no')" "2"
BG_A2=$(data '.data.budget.id'); save BG_A2 "$BG_A2"
check "调整后旧版仍 ACTIVE（新版还没批）" \
  "$(SQ "select status from biz_budget where id='$BG_A'::uuid")" "ACTIVE"
budget_active "$BG_A2" bgA2
check "新版 ACTIVE" "$(SQ "select status from biz_budget where id='$BG_A2'::uuid")" "ACTIVE"
check "旧版退位 SUPERSEDED" "$(SQ "select status from biz_budget where id='$BG_A'::uuid")" "SUPERSEDED"
check "同槽 ACTIVE 唯一（库级兜底）" \
  "$(SQ "select count(*) from biz_budget where dept_id='$DEPT_ID'::uuid and account_id='$ACC_TRAVEL'::uuid and fiscal_year=2027 and status='ACTIVE' and deleted_at is null")" "1"
check "ACTIVE 额度改不动（只剩新版 15 万）" \
  "$(SQ "select total_amount::text from biz_budget where id='$BG_A2'::uuid")" "150000.0000"

# ---- ②-6 未编预算（5602.03）：不得误判为超预算（FIX-5）----
api ex_nobg POST /business/erp/expense "$T_CLERK" \
  "{\"title\":\"E2E06 未编预算科目\",\"expense_date\":\"2027-02-09\",\"dept_id\":\"$DEPT_ID\",\"items\":[{\"expense_type_id\":\"$(SQ "select id from biz_expense_type where type_code='OFFICE' and deleted_at is null")\",\"with_tax_amount\":\"800\",\"tax_rate\":\"0.06\",\"summary\":\"办公用品（未编预算）\",\"invoice_no\":\"INV-E2E06-002\",\"invoice_type\":\"VAT_NORMAL\"}]}"
check "未编预算科目建单 http" "$HTTP" "200"
EXP_N=$(data '.data.report.id'); save EXP_N "$EXP_N"
exp_att "$EXP_N" nobg
api ex_nobg_sub POST "/business/erp/expense/$EXP_N/submit" "$T_CLERK" '{}'
check "未编预算提交 http" "$HTTP" "200"
check "未编预算不误判超支（over_budget=0 / over_amount NULL）" \
  "$(SQ "select over_budget::text||'|'||coalesce(over_amount::text,'NULL') from biz_expense_report where id='$EXP_N'::uuid")" \
  "0|NULL"
check "missing_budget=true（只打「未编制」标记）" \
  "$(SQ "select missing_budget::text from biz_expense_report where id='$EXP_N'::uuid")" "1"
check "占用行仍写（WARN 模式占用 + 不标超支）" \
  "$(SQ "select status||'/'||over_budget||'/'||coalesce(over_amount::text,'NULL') from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP_N'::uuid")" \
  "OCCUPIED/0/NULL"
api ex_nobg_wd POST "/business/erp/expense/$EXP_N/withdraw" "$T_CLERK" '{}'
check "撤回未编预算单 http" "$HTTP" "200"

# ============ ③ 预算科目映射 ============
say
say "③ 预算科目映射（物料 → 预算科目）"
MAT=$(SQ "select id from biz_material where material_code='E2E04-MAT' and deleted_at is null")
MAT_CODE=$(SQ "select material_code from biz_material where id='$MAT'::uuid")
save MAT "$MAT"; save MAT_CODE "$MAT_CODE"
api mp_create POST /business/erp/budget/mapping "$T_APPR" \
  "{\"map_type\":\"MATERIAL\",\"ref_id\":\"$MAT\",\"ref_code\":\"$MAT_CODE\",\"account_id\":\"$ACC_TRAIN\",\"priority\":10,\"status\":1,\"remark\":\"E2E-06 物料→培训费\"}"
check "建映射 http" "$HTTP" "200"
check "建映射 code" "$CODE" "0"
MAP_ID=$(data '.data.id'); save MAP_ID "$MAP_ID"
check "映射落库（MATERIAL→5602.06）" \
  "$(SQ "select map_type||'|'||ref_code||'|'||(select account_code from biz_account a where a.id=m.account_id) from biz_budget_subject_map m where id='$MAP_ID'::uuid")" \
  "MATERIAL|$MAT_CODE|5602.06"

api mp_page GET "/business/erp/budget/mapping/page?page=1&page_size=20&status=MATERIAL" "$T_APPR"
check "映射分页 http" "$HTTP" "200"
check "按类型筛选命中（status 参数当 map_type 用）" \
  "$(printf '%s' "$BODY" | jq -r '.data.list|length')" "$(printf '%s' "$BODY" | jq -r '[.data.list[]|select(.map_type=="MATERIAL")]|length')"

api mp_update PUT "/business/erp/budget/mapping/$MAP_ID" "$T_APPR" \
  "{\"map_type\":\"MATERIAL\",\"ref_id\":\"$MAT\",\"ref_code\":\"$MAT_CODE\",\"account_id\":\"$ACC_TRAIN\",\"priority\":5,\"status\":1,\"remark\":\"E2E-06 物料→培训费（优先级 5）\"}"
check "改映射 http" "$HTTP" "200"
check "优先级落库 5" "$(SQ "select priority from biz_budget_subject_map where id='$MAP_ID'::uuid")" "5"

api mp_bad POST /business/erp/budget/mapping "$T_APPR" \
  "{\"map_type\":\"MATERIAL\",\"ref_id\":null,\"account_id\":\"$ACC_TRAIN\",\"priority\":1,\"status\":1}"
check "MATERIAL 缺目标 id 被拒 $HTTP" "$HTTP" "400"

summary
