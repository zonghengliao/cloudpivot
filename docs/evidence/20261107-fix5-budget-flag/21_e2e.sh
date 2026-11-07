#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 真机验收：409 报文策略标签 + 未编预算不再冒充超支
#
# 跑法（在 145 上）：bash /opt/cloudpivot/docs/evidence/20261107-fix5-budget-flag/21_e2e.sh
# 前置：10_prepare_145.sh + 11_start_instance.sh（8046 在跑，连拷贝库）
#
# 场景（每条都留 raw/*.json 原始响应）：
#   ① P0-06 ⑤ 回归：差旅费槽 BLOCK 10000 / 已用 9500，再报 1000 → 409，
#      报文必须说「槽级策略 BLOCK」+ 槽级数据，且**不许**再说「标记并强制审批」
#   ② 真超支仍被拦：409 后单据仍 DRAFT、0 占用行、0 流程实例（拦截没被改坏）
#   ③ 未编预算：通讯费（无预算）报 800 → 200，over_budget=0 / missing_budget=1 / over_amount=null
#   ④ 列表接口能区分：GET /expense/page 里该行带 missing_budget=true、over_budget=false
#   ⑤ 额度内正常单：提交 200、两个标记都是 0、over_amount=null（不再一律「超预算」）
#   ⑥ 口径说明（非验收必须）：全局 BLOCK 下「未编预算」不再按超支拦（over 判据是「超了额度」）
# ============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$HERE/00_env.sh"

echo "=========== FIX-5 E2E @ $(date '+%F %T') ==========="
echo "API=$API  DB=$DBNAME"

ADMIN=$(login admin "$ADMIN_PASS")
if [ -z "$ADMIN" ]; then echo "FATAL: admin 登录失败"; exit 1; fi
echo "$ADMIN" > "$(tokf admin)"
echo "admin token ok (len=${#ADMIN})"

DEPT=$(sq "select id from sys_dept where dept_name='演练设备部' and deleted_at is null limit 1")
ACC_TRAVEL=$(sq "select id from biz_account where account_code='5602.02' and deleted_at is null limit 1")
ACC_TEL=$(sq "select id from biz_account where account_code='5602.05' and deleted_at is null limit 1")
ACC_TRAVEL_NAME=$(psqlc -tAc "select account_name from biz_account where id='$ACC_TRAVEL'")
ACC_TEL_NAME=$(psqlc -tAc "select account_name from biz_account where id='$ACC_TEL'")
echo "DEPT=$DEPT（$DEPT 演练设备部）"
echo "ACC_TRAVEL=$ACC_TRAVEL $ACC_TRAVEL_NAME"
echo "ACC_TEL=$ACC_TEL $ACC_TEL_NAME"
[ -n "$DEPT" ] && [ -n "$ACC_TRAVEL" ] && [ -n "$ACC_TEL" ] || { echo "FATAL: 夹具前置数据缺失"; exit 1; }

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
TYPE_TEL=$(ensure_type FIX5_TEL "通讯费(FIX5)" "$ACC_TEL")
echo "TYPE_TRAVEL=$TYPE_TRAVEL  TYPE_TEL=$TYPE_TEL"
[ -n "$TYPE_TRAVEL" ] && [ -n "$TYPE_TEL" ] || { echo "FATAL: 费用类型没建起来"; exit 1; }

# ---------- 预算夹具：演练设备部 × 5602.02 × 2026，BLOCK 10000，11 月 10000，立即生效 ----------
# effective_from = NULL（对 2026-11 生效）；只给 11 月分解 10000，其余月 0（月闸先于年闸）
psqlc -v ON_ERROR_STOP=1 -q <<SQL
delete from biz_budget_period where budget_id in (
  select id from biz_budget
   where dept_id='$DEPT'::uuid and account_id='$ACC_TRAVEL'::uuid and fiscal_year=2026
     and status='ACTIVE' and deleted_at is null);
delete from biz_budget
 where dept_id='$DEPT'::uuid and account_id='$ACC_TRAVEL'::uuid and fiscal_year=2026
   and status='ACTIVE' and deleted_at is null;
delete from biz_budget_usage
 where dept_id='$DEPT'::uuid and account_id='$ACC_TRAVEL'::uuid and fiscal_year=2026;

insert into biz_budget (id, budget_no, fiscal_year, dept_id, dept_name, account_id, account_code,
                        account_name, total_amount, control_mode, version_no, change_type, status,
                        effective_from, created_at, updated_at)
values ('01a1b6f0-0000-4000-8000-00000000f501'::uuid, 'BDG-FIX5-000001', 2026, '$DEPT'::uuid, '演练设备部',
        '$ACC_TRAVEL'::uuid, '5602.02', '$ACC_TRAVEL_NAME', 10000, 'BLOCK', 1, 'ORIGINAL', 'ACTIVE',
        null, now(), now());

insert into biz_budget_period (id, budget_id, period_code, period_month, amount)
select gen_random_uuid(), '01a1b6f0-0000-4000-8000-00000000f501'::uuid,
       '2026-' || lpad(m::text, 2, '0'), m, case when m = 11 then 10000 else 0 end
  from generate_series(1, 12) as m;
SQL
echo "预算夹具：$(psqlc -tAc "select budget_no||' / '||total_amount||' / '||control_mode||' / '||status from biz_budget where id='01a1b6f0-0000-4000-8000-00000000f501'")"

# ---------- 通用：建单 ----------
create_report() { # $1=type_id $2=amount $3=title $4=tag → RPT
  local body out
  body=$(jq -nc --arg t "$3" --arg d "$DEPT" --arg ty "$1" --arg am "$2" \
    --arg inv "INV-FIX5-$(date +%s%N | tail -c 10)" \
    '{title:$t, expense_date:"2026-11-01", dept_id:$d, remark:"FIX-5 取证",
      items:[{expense_date:"2026-11-01", expense_type_id:$ty, with_tax_amount:$am, tax_rate:"0",
              summary:"FIX-5 取证", invoice_no:$inv, invoice_date:"2026-11-01", invoice_type:"VAT_NORMAL"}]}')
  out=$(api POST "/business/erp/expense" "$ADMIN" "$body")
  echo "$out" > "$EV/raw/$4_create.json"
  RPT=$(echo "$out" | jq -r '.data.report.id // empty')
  echo "$RPT"
}

flags_of() { # $1=report_id → "over|missing|amount"
  psqlc -tAc "select over_budget||'|'||missing_budget||'|'||coalesce(over_amount::text,'NULL') from biz_expense_report where id='$1'::uuid"
}
usage_of() { # $1=report_id → "rows|over|amount|status"
  psqlc -tAc "select count(*)||'|'||coalesce(max(over_budget)::text,'-')||'|'||coalesce(max(over_amount::text),'-')||'|'||coalesce(max(status),'-') from biz_budget_usage where source_id='$1'::uuid"
}

echo
echo "=========== ① 额度内：差旅费报 9500（额度 10000）==========="
R1=$(create_report "$TYPE_TRAVEL" "9500" "FIX-5 额度内差旅 9500" "r1_9500")
api POST "/business/erp/expense/$R1/submit" "$ADMIN" > "$EV/raw/r1_9500_submit.json"
echo "report=$R1 submit_http=$HTTP $(jq -c '{code,msg}' "$EV/raw/r1_9500_submit.json")"
check_eq "200" "$HTTP" "① 9500 在额度内，提交放行"
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R1")"
check_eq "0 0 NULL" "$OV $MB $AMT" "① 单据标记（over_budget / missing_budget / over_amount）"
echo "占用行：$(usage_of "$R1")"

echo
echo "=========== ② P0-06 ⑤ 回归：再报 1000（9500+1000 > 10000，槽级 BLOCK）==========="
R2=$(create_report "$TYPE_TRAVEL" "1000" "FIX-5 超支差旅 1000" "r2_1000")
api POST "/business/erp/expense/$R2/submit" "$ADMIN" > "$EV/raw/r2_1000_submit.json"
MSG=$(jq -r '.msg // empty' "$EV/raw/r2_1000_submit.json")
CODE=$(jq -r '.code // empty' "$EV/raw/r2_1000_submit.json")
echo "report=$R2 submit_http=$HTTP code=$CODE"
echo "---- 409 原始报文 ----"
echo "$MSG"
echo "---------------------"
check_eq "409" "$HTTP" "② 真超支被拦（P0-06 ⑤ 回归）"
check_eq "66037" "$CODE" "② 错误码 ERP_BUDGET_EXCEEDED"
check_contains "$MSG" "槽级策略 BLOCK" "② 报文点明「槽级策略 BLOCK」"
check_contains "$MSG" "科目「5602.02 $ACC_TRAVEL_NAME」2026年11月" "② 报文点名科目与期间"
check_contains "$MSG" "月度已用 9500 / 月度预算 10000" "② 报文带上槽级已用/额度"
check_contains "$MSG" "本次 1000" "② 报文带上本次金额"
check_contains "$MSG" "超支 500" "② 报文带上超支额"
check_absent "$MSG" "标记并强制审批" "② 不许再出现全局标签「标记并强制审批」"
check_absent "$MSG" "当前策略是 BLOCK" "② 不许再用笼统的「当前策略是 BLOCK」解释槽级阻断"
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R2")"
check_eq "0 0 NULL" "$OV $MB $AMT" "② 被拦单据停在初始标记（没有脏写）"
check_eq "DRAFT" "$(psqlc -tAc "select status from biz_expense_report where id='$R2'::uuid")" "② 单据仍 DRAFT"
check_eq "0" "$(sq "select count(*) from biz_budget_usage where source_id='$R2'::uuid")" "② 0 占用行（事务回滚）"
check_eq "0" "$(sq "select count(*) from biz_flow_instance where biz_id='$R2'::uuid")" "② 0 流程实例"

echo
echo "=========== ③ 未编预算：通讯费（5602.05 无任何预算）报 800 ==========="
R3=$(create_report "$TYPE_TEL" "800" "FIX-5 未编预算通讯费 800" "r3_800")
api POST "/business/erp/expense/$R3/submit" "$ADMIN" > "$EV/raw/r3_800_submit.json"
echo "report=$R3 submit_http=$HTTP $(jq -c '{code,msg}' "$EV/raw/r3_800_submit.json")"
check_eq "200" "$HTTP" "③ 未编预算不阻断（全局 WARN）"
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R3")"
check_eq "0 1 NULL" "$OV $MB $AMT" "③ over_budget=0 / missing_budget=1 / over_amount=null"
IFS='|' read -r ROWS UOV UAMT UST <<<"$(usage_of "$R3")"
check_eq "1|0|NULL|OCCUPIED" "$ROWS|$UOV|$UAMT|$UST" "③ 占用行：1 行、over_budget=0、over_amount=null、OCCUPIED"

echo
echo "=========== ④ 列表接口：over_budget=false / missing_budget=true 都在 ==========="
api GET "/business/erp/expense/page?page=1&page_size=50&keyword=FIX-5" "$ADMIN" > "$EV/raw/list_page.json"
LIST_HTTP="$HTTP"
ROW=$(jq -c --arg id "$R3" '[.data.list[]?|select(.id==$id)][0] // {}' "$EV/raw/list_page.json")
echo "list_http=$LIST_HTTP row=$ROW"
check_eq "200" "$LIST_HTTP" "④ 列表接口 200"
check_eq "true" "$(echo "$ROW" | jq -r '.missing_budget // "absent"')" "④ 列表行带 missing_budget=true"
check_eq "false" "$(echo "$ROW" | jq -r '.over_budget|tostring')" "④ 列表行 over_budget=false"
check_eq "null" "$(echo "$ROW" | jq -r '.over_amount|tostring')" "④ 列表行 over_amount=null"
ROW_OVER=$(jq -c --arg id "$R1" '[.data.list[]?|select(.id==$id)][0] // {}' "$EV/raw/list_page.json")
check_eq "false" "$(echo "$ROW_OVER" | jq -r '.missing_budget|tostring')" "④ 额度内单 missing_budget=false（不误标）"

echo
echo "=========== ⑤ 口径说明：全局 BLOCK 下「未编预算」不再按超支拦 ==========="
OLD_MODE=$(psqlc -tAc "select coalesce(max(config_value),'(none)') from sys_config where config_key='erp.budget.control_mode' and deleted_at is null")
psqlc -q -c "update sys_config set config_value='BLOCK', updated_at=now() where config_key='erp.budget.control_mode'"
R4=$(create_report "$TYPE_TEL" "500" "FIX-5 全局BLOCK 未编预算 500" "r4_500")
api POST "/business/erp/expense/$R4/submit" "$ADMIN" > "$EV/raw/r4_500_submit.json"
echo "report=$R4 submit_http=$HTTP $(jq -c '{code,msg}' "$EV/raw/r4_500_submit.json")"
check_eq "200" "$HTTP" "⑤ 全局 BLOCK + 未编预算：不再按超支 409（判据是「超了额度」）"
IFS='|' read -r OV MB AMT <<<"$(flags_of "$R4")"
check_eq "0 1 NULL" "$OV $MB $AMT" "⑤ 单据标记仍是「未编预算」而不是超支"
# 还原全局策略
if [ "$OLD_MODE" = "(none)" ]; then
  psqlc -q -c "update sys_config set deleted_at=now() where config_key='erp.budget.control_mode'"
else
  psqlc -q -c "update sys_config set config_value='$OLD_MODE', updated_at=now() where config_key='erp.budget.control_mode'"
fi
echo "全局策略已还原为：$(psqlc -tAc "select config_value from sys_config where config_key='erp.budget.control_mode' and deleted_at is null")"

echo
echo "=========== 预算槽最终状态（勾稽）==========="
psqlc -c "select b.budget_no, b.control_mode, b.status,
                 (select coalesce(sum(amount),0) from biz_budget_usage u
                   where u.dept_id=b.dept_id and u.account_id=b.account_id and u.fiscal_year=b.fiscal_year
                     and u.status in ('OCCUPIED','CONSUMED') and u.deleted_at is null) as used
            from biz_budget b where b.id='01a1b6f0-0000-4000-8000-00000000f501'"

echo
echo "=========== 汇总 ==========="
echo "FAILS=$FAILS"
[ "$FAILS" -eq 0 ] && echo "RESULT=ALL PASS" || echo "RESULT=HAS FAILURES"
exit "$FAILS"
