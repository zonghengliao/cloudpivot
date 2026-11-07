#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 真机验收：409 报文策略标签 + 未编预算不再冒充超支
#
# 跑法（在 145 上）：bash /opt/cloudpivot/docs/evidence/20261107-fix5-budget-flag/21_e2e.sh
# 前置：10_prepare_145.sh + 11_start_instance.sh（8046 在跑，连拷贝库）
#
# 报销人用 emp_drill（P0-05 建的演练账号，隶属「演练费用部」，有直属上级 lead_drill）——
# 用 admin 提交会撞 FIX-1/ARCH-8 的「制单人不可自审」内控（422 65018），那是另一张卡的口径。
#
# 场景（每条都留 raw/*.json 原始响应）：
#   ① 额度内：差旅费报 9500（额度 10000）→ 200，两个标记都是 0、over_amount=null
#   ② P0-06 ⑤ 回归：再报 1000（9500+1000 > 10000，槽级 BLOCK）→ 409，
#      报文必须说「槽级策略 BLOCK」+ 槽级已用/额度/超支，且**不许**再说「标记并强制审批」
#   ③ 真超支仍被拦：② 之后单据仍 DRAFT、0 占用行、0 流程实例（拦截没被改坏）
#   ④ 未编预算：通讯费类科目（无预算）报 800（全局 WARN）→ 200，
#      over_budget=0 / missing_budget=1 / over_amount=null（单据 + 占用行同口径）
#   ⑤ 列表接口能区分：GET /expense/page 里该行 missing_budget=true、over_budget=false
#   ⑥ BLOCK 下未编预算仍拦：把该科目的策略改成槽级 BLOCK → 409，第一句是
#      「涉及的预算科目未编制」，而不是「超出预算额度」（强管控不许因为修判定而松掉）
# ============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$HERE/00_env.sh"

echo "=========== FIX-5 E2E @ $(date '+%F %T') ==========="
echo "API=$API  DB=$DBNAME"

ADMIN=$(login admin "$ADMIN_PASS")
REPORTER=$(login emp_drill "${DRILL_PWD:-Drill@123456}")
if [ -z "$ADMIN" ] || [ -z "$REPORTER" ]; then echo "FATAL: 登录失败（admin/emp_drill）"; exit 1; fi
echo "$ADMIN" > "$(tokf admin)"
echo "$REPORTER" > "$(tokf emp_drill)"
echo "token ok (admin len=${#ADMIN} / emp_drill len=${#REPORTER})"

DEPT=$(sq "select dept_id from sys_user where username='emp_drill'")
DEPT_NAME=$(psqlc -tAc "select dept_name from sys_dept where id='$DEPT'::uuid")
REPORTER_NAME=$(psqlc -tAc "select real_name from sys_user where username='emp_drill'")
ACC_TRAVEL=$(sq "select id from biz_account where account_code='5602.02' and deleted_at is null limit 1")
ACC_OTHER=$(sq "select id from biz_account where account_code='5602.09' and deleted_at is null limit 1")
# ⑥ 用的「干净槽」：没预算、也没任何占用（不能用 5602.09 —— ④ 已经给它写了占用）
ACC_FRESH=$(sq "select id from biz_account where account_code='5602.06' and deleted_at is null limit 1")
ACC_TRAVEL_NAME=$(psqlc -tAc "select account_name from biz_account where id='$ACC_TRAVEL'")
ACC_OTHER_NAME=$(psqlc -tAc "select account_name from biz_account where id='$ACC_OTHER'")
ACC_FRESH_NAME=$(psqlc -tAc "select account_name from biz_account where id='$ACC_FRESH'")
echo "DEPT=$DEPT（$DEPT_NAME）  REPORTER=$REPORTER_NAME"
echo "ACC_TRAVEL=$ACC_TRAVEL $ACC_TRAVEL_NAME"
echo "ACC_OTHER=$ACC_OTHER $ACC_OTHER_NAME"
echo "ACC_FRESH=$ACC_FRESH $ACC_FRESH_NAME"
[ -n "$DEPT" ] && [ -n "$ACC_TRAVEL" ] && [ -n "$ACC_OTHER" ] && [ -n "$ACC_FRESH" ] || { echo "FATAL: 夹具前置数据缺失"; exit 1; }

# ---------- 费用类型（幂等：先查后建） ----------
ensure_type() { # $1=code $2=name $3=account_id → id
  local code="$1" name="$2" account="$3" list id body
  list=$(api GET "/business/erp/expense-type/list" "$ADMIN")
  id=$(echo "$list" | jq -r --arg c "$code" '[.data[]?|select(.type_code==$c)][0].id // empty')
  if [ -z "$id" ]; then
    body=$(jq -nc --arg c "$code" --arg n "$name" --arg a "$account" \
      '{type_code:$c,type_name:$n,account_id:$a,tax_deductible:false,budget_controlled:true,need_invoice_no:false,amount_limit:"0",sort_order:95,remark:"FIX-5 取证"}')
    id=$(api POST "/business/erp/expense-type" "$ADMIN" "$body" | jq -r '.data.id // empty')
  fi
  printf '%s' "$id"
}
TYPE_TRAVEL=$(ensure_type FIX5_TRAVEL "差旅费(FIX5)" "$ACC_TRAVEL")
TYPE_OTHER=$(ensure_type FIX5_OTHER "其他费用(FIX5)" "$ACC_OTHER")
TYPE_FRESH=$(ensure_type FIX5_FRESH "培训费(FIX5)" "$ACC_FRESH")
echo "TYPE_TRAVEL=$TYPE_TRAVEL  TYPE_OTHER=$TYPE_OTHER  TYPE_FRESH=$TYPE_FRESH"
[ -n "$TYPE_TRAVEL" ] && [ -n "$TYPE_OTHER" ] && [ -n "$TYPE_FRESH" ] || { echo "FATAL: 费用类型没建起来"; exit 1; }

# ---------- 预算夹具：报销人部门 × 5602.02 × 2026，槽级 BLOCK 10000，11 月 10000，立即生效 ----------
# effective_from = NULL（对 2026-11 生效）；只给 11 月分解 10000（月闸先于年闸）。
# 5602.09 故意**不编预算**（未编预算场景），且清空该槽占用让算术确定。
psqlc -v ON_ERROR_STOP=1 -q <<SQL
delete from biz_budget_period where budget_id = '01a1b6f0-0000-4000-8000-00000000f501'::uuid;
delete from biz_budget_period where budget_id in (
  select id from biz_budget
   where dept_id='$DEPT'::uuid and account_id='$ACC_TRAVEL'::uuid and fiscal_year=2026
     and status='ACTIVE' and deleted_at is null);
delete from biz_budget
 where id = '01a1b6f0-0000-4000-8000-00000000f501'::uuid;
delete from biz_budget
 where dept_id='$DEPT'::uuid and account_id='$ACC_TRAVEL'::uuid and fiscal_year=2026
   and status='ACTIVE' and deleted_at is null;
delete from biz_budget_usage
 where dept_id='$DEPT'::uuid and fiscal_year=2026
   and account_id in ('$ACC_TRAVEL'::uuid, '$ACC_OTHER'::uuid);

insert into biz_budget (id, budget_no, fiscal_year, dept_id, dept_name, account_id, account_code,
                        account_name, total_amount, control_mode, version_no, change_type, status,
                        effective_from, created_at, updated_at)
values ('01a1b6f0-0000-4000-8000-00000000f501'::uuid, 'BDG-FIX5-000001', 2026, '$DEPT'::uuid, '$DEPT_NAME',
        '$ACC_TRAVEL'::uuid, '5602.02', '$ACC_TRAVEL_NAME', 10000, 'BLOCK', 1, 'ORIGINAL', 'ACTIVE',
        null, now(), now());

insert into biz_budget_period (id, budget_id, period_code, period_month, amount)
select gen_random_uuid(), '01a1b6f0-0000-4000-8000-00000000f501'::uuid,
       '2026-' || lpad(m::text, 2, '0'), m, case when m = 11 then 10000 else 0 end
  from generate_series(1, 12) as m;

update sys_config set config_value='WARN', updated_at=now() where config_key='erp.budget.control_mode';
SQL
echo "预算夹具：$(psqlc -tAc "select budget_no||' / '||total_amount||' / '||control_mode||' / '||status from biz_budget where id='01a1b6f0-0000-4000-8000-00000000f501'")"
echo "全局策略：$(psqlc -tAc "select config_value from sys_config where config_key='erp.budget.control_mode' and deleted_at is null")"

# ---------- 通用：建单 ----------
create_report() { # $1=type_id $2=amount $3=title $4=tag → stdout report_id
  local body out
  body=$(jq -nc --arg t "$3" --arg d "$DEPT" --arg ty "$1" --arg am "$2" \
    --arg inv "INV-FIX5-$(date +%s%N | tail -c 10)" \
    '{title:$t, expense_date:"2026-11-01", dept_id:$d, remark:"FIX-5 取证",
      items:[{expense_date:"2026-11-01", expense_type_id:$ty, with_tax_amount:$am, tax_rate:"0",
              summary:"FIX-5 取证", invoice_no:$inv, invoice_date:"2026-11-01", invoice_type:"VAT_NORMAL"}]}')
  out=$(api POST "/business/erp/expense" "$REPORTER" "$body")
  echo "$out" > "$EV/raw/$4_create.json"
  echo "$out" | jq -r '.data.report.id // empty'
}

flags_of() { # $1=report_id → "over|missing|amount"
  psqlc -tAc "select over_budget||'|'||missing_budget||'|'||coalesce(over_amount::text,'NULL') from biz_expense_report where id='$1'::uuid"
}
usage_of() { # $1=report_id → "rows|over|amount|status"
  psqlc -tAc "select count(*)||'|'||coalesce(max(over_budget)::text,'-')||'|'||coalesce(max(over_amount::text),'NULL')||'|'||coalesce(max(status),'-') from biz_budget_usage where source_id='$1'::uuid"
}

echo
echo "=========== ① 额度内：差旅费报 9500（额度 10000）==========="
R1=$(create_report "$TYPE_TRAVEL" "9500" "FIX-5 额度内差旅 9500" "r1_9500")
echo "report=$R1"
[ -n "$R1" ] || { echo "FATAL: 建单失败"; cat "$EV/raw/r1_9500_create.json"; exit 1; }
api POST "/business/erp/expense/$R1/submit" "$REPORTER" > "$EV/raw/r1_9500_submit.json"
echo "submit_http=$HTTP $(jq -c '{code,msg}' "$EV/raw/r1_9500_submit.json")"
check_eq "200" "$HTTP" "① 9500 在额度内，提交放行"
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R1")"
check_eq "0 0 NULL" "$OV $MB $AMT" "① 单据标记（over_budget / missing_budget / over_amount）"
IFS='|' read -r ROWS UOV UAMT UST <<<"$(usage_of "$R1")"
check_eq "1|0|NULL|OCCUPIED" "$ROWS|$UOV|$UAMT|$UST" "① 占用行（1 行 / 未超支 / NULL / OCCUPIED）"

echo
echo "=========== ② P0-06 ⑤ 回归：再报 1000（9500+1000 > 10000，槽级 BLOCK）==========="
R2=$(create_report "$TYPE_TRAVEL" "1000" "FIX-5 超支差旅 1000" "r2_1000")
echo "report=$R2"
api POST "/business/erp/expense/$R2/submit" "$REPORTER" > "$EV/raw/r2_1000_submit.json"
MSG=$(jq -r '.msg // empty' "$EV/raw/r2_1000_submit.json")
CODE=$(jq -r '.code // empty' "$EV/raw/r2_1000_submit.json")
echo "submit_http=$HTTP code=$CODE"
echo "---- 409 原始报文 ----"
echo "$MSG"
echo "---------------------"
check_eq "409" "$HTTP" "② 真超支被拦（P0-06 ⑤ 回归）"
check_eq "66037" "$CODE" "② 错误码 ERP_BUDGET_EXCEEDED"
check_contains "$MSG" "超出预算额度" "② 第一句说「超出预算额度」"
check_contains "$MSG" "槽级策略 BLOCK" "② 报文点明「槽级策略 BLOCK」"
check_contains "$MSG" "科目「5602.02 $ACC_TRAVEL_NAME」2026年11月" "② 报文点名科目与期间"
check_contains "$MSG" "月度已用 9500 / 月度预算 10000" "② 报文带上槽级已用/额度"
check_contains "$MSG" "本次 1000" "② 报文带上本次金额"
check_contains "$MSG" "超支 500" "② 报文带上超支额"
check_absent "$MSG" "标记并强制审批（WARN）" "② 不许再用全局标签解释槽级阻断"
check_absent "$MSG" "当前策略是 BLOCK" "② 不许再写笼统的「当前策略是 BLOCK」"

echo
echo "=========== ③ 拦截没被改坏：② 的单据/占用/流程都没落地 ==========="
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R2")"
check_eq "0 0 NULL" "$OV $MB $AMT" "③ 单据停在草稿态标记（无脏写）"
check_eq "DRAFT" "$(psqlc -tAc "select status from biz_expense_report where id='$R2'::uuid")" "③ 单据仍 DRAFT"
check_eq "0" "$(sq "select count(*) from biz_budget_usage where source_id='$R2'::uuid")" "③ 0 占用行（事务回滚）"
check_eq "0" "$(sq "select count(*) from biz_flow_instance where biz_id='$R2'::uuid")" "③ 0 流程实例"

echo
echo "=========== ④ 未编预算：5602.09（无任何预算）报 800，全局 WARN ==========="
R3=$(create_report "$TYPE_OTHER" "800" "FIX-5 未编预算 800" "r3_800")
echo "report=$R3"
api POST "/business/erp/expense/$R3/submit" "$REPORTER" > "$EV/raw/r3_800_submit.json"
echo "submit_http=$HTTP $(jq -c '{code,msg}' "$EV/raw/r3_800_submit.json")"
check_eq "200" "$HTTP" "④ 未编预算不阻断（全局 WARN）"
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R3")"
check_eq "0 1 NULL" "$OV $MB $AMT" "④ over_budget=0 / missing_budget=1 / over_amount=null"
IFS='|' read -r ROWS UOV UAMT UST <<<"$(usage_of "$R3")"
check_eq "1|0|NULL|OCCUPIED" "$ROWS|$UOV|$UAMT|$UST" "④ 占用行同口径（0 / NULL / OCCUPIED）"

echo
echo "=========== ⑤ 列表接口：over_budget=false / missing_budget=true 都在 ==========="
# ⚠️ 用 admin 令牌查列表：非超管（emp_drill）带**任何筛选**时后端 500，这是本卡之前就有的缺陷
#    （`scope::paginate` 要求 base_sql 带 `{FILTER}` 标记，报销分页的 base_sql 没有；
#    8033 现役实例上同样可复现，见 raw/list_page_emp_500.json 与 README）。与 FIX-5 无关。
api GET "/business/erp/expense/page?page=1&page_size=50&keyword=FIX-5" "$REPORTER" > "$EV/raw/list_page_emp_500.json"
echo "（旁证）emp_drill 带筛选查列表：http=$HTTP $(jq -c '{code,msg}' "$EV/raw/list_page_emp_500.json")"
api GET "/business/erp/expense/page?page=1&page_size=50&keyword=FIX-5" "$ADMIN" > "$EV/raw/list_page.json"
LIST_HTTP="$HTTP"
ROW=$(jq -c --arg id "$R3" '[.data.list[]?|select(.id==$id)][0] // {}' "$EV/raw/list_page.json")
echo "list_http=$LIST_HTTP"
echo "该行=$(echo "$ROW" | jq -c '{report_no,over_budget,missing_budget,over_amount,budget_mode}')"
check_eq "200" "$LIST_HTTP" "⑤ 列表接口 200"
check_eq "true" "$(echo "$ROW" | jq -r '.missing_budget|tostring')" "⑤ 列表行 missing_budget=true"
check_eq "false" "$(echo "$ROW" | jq -r '.over_budget|tostring')" "⑤ 列表行 over_budget=false"
check_eq "null" "$(echo "$ROW" | jq -r '.over_amount|tostring')" "⑤ 列表行 over_amount=null"
ROW1=$(jq -c --arg id "$R1" '[.data.list[]?|select(.id==$id)][0] // {}' "$EV/raw/list_page.json")
echo "额度内行=$(echo "$ROW1" | jq -c '{report_no,over_budget,missing_budget,over_amount}')"
check_eq "false" "$(echo "$ROW1" | jq -r '.missing_budget|tostring')" "⑤ 额度内单 missing_budget=false（不误标）"

echo
echo "=========== ⑥ BLOCK 下未编预算仍被拦（强管控不许因修判定而放松）==========="
# 用一个**干净槽**（5602.06 培训费：既没预算、也没任何占用）—— 这样 missing_budget 判据成立。
# 全局策略改成 BLOCK：没额度就不许花，但第一句要说「未编制」，不能说成「超出预算额度」。
psqlc -q -c "update sys_config set config_value='BLOCK', updated_at=now() where config_key='erp.budget.control_mode'"
R4=$(create_report "$TYPE_FRESH" "500" "FIX-5 全局BLOCK 未编预算 500" "r4_500")
echo "report=$R4"
api POST "/business/erp/expense/$R4/submit" "$REPORTER" > "$EV/raw/r4_500_submit.json"
MSG4=$(jq -r '.msg // empty' "$EV/raw/r4_500_submit.json")
echo "submit_http=$HTTP code=$(jq -r '.code // empty' "$EV/raw/r4_500_submit.json")"
echo "---- 409 原始报文 ----"
echo "$MSG4"
echo "---------------------"
check_eq "409" "$HTTP" "⑥ 全局 BLOCK + 未编预算：仍被拦（不放松强管控）"
check_contains "$MSG4" "涉及的预算科目未编制" "⑥ 第一句说「未编制预算」而不是「超出预算额度」"
check_contains "$MSG4" "全局策略 BLOCK" "⑥ 策略来源说对（全局）"
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R4")"
check_eq "0 0 NULL" "$OV $MB $AMT" "⑥ 被拦单据无脏写"
check_eq "0" "$(sq "select count(*) from biz_budget_usage where source_id='$R4'::uuid")" "⑥ 0 占用行"
# 还原全局策略
psqlc -q -c "update sys_config set config_value='WARN', updated_at=now() where config_key='erp.budget.control_mode'"
echo "全局策略已还原为：$(psqlc -tAc "select config_value from sys_config where config_key='erp.budget.control_mode' and deleted_at is null")"

echo
echo "=========== 预算槽最终状态（勾稽）==========="
psqlc -c "select b.budget_no, b.control_mode, b.status, b.total_amount,
                 (select coalesce(sum(amount),0) from biz_budget_usage u
                   where u.dept_id=b.dept_id and u.account_id=b.account_id and u.fiscal_year=b.fiscal_year
                     and u.status in ('OCCUPIED','CONSUMED') and u.deleted_at is null) as used
            from biz_budget b where b.id='01a1b6f0-0000-4000-8000-00000000f501'"
psqlc -c "select source_no, amount, status, over_budget, coalesce(over_amount::text,'NULL') as over_amount
            from biz_budget_usage
           where dept_id='$DEPT'::uuid and fiscal_year=2026 order by created_at"

echo
echo "=========== 汇总 ==========="
echo "FAILS=$FAILS"
[ "$FAILS" -eq 0 ] && echo "RESULT=ALL PASS" || echo "RESULT=HAS FAILURES"
exit "$FAILS"
