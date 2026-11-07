#!/usr/bin/env bash
# ============================================================================
# FIX-2（t_e2ee3d12）145 真机验收：预算控制三连 + usages + budget_mode
#   ① DRAFT 预算（BLOCK）→ 超支报销放行 + 「预算未生效」提示
#   ② ACTIVE 预算（effective_from = 次月 1 日 = 2026-12-01）→ 11 月单据不受新额度约束
#      （反证：12 月单据受约束 → 409）
#   ③ ACTIVE 预算（effective_from 已过）→ 正常占用 + 超支 409 拦截
#   ④ 预算详情 usages[] 能看见报销产生的占用（source_type / source_id / 金额）
#   ⑤ 槽级 BLOCK 的单据 budget_mode 记 BLOCK（不是全局默认 WARN）
#   ⑥ 执行报表与控制的生效口径一致（未生效行 effective=false）
#
# 运行位置：145 本机（隔离实例 127.0.0.1:8039 / 库 cloudpivot_fix2c）
# 说明：②③ 的「审批通过 → ACTIVE + effective_from=次月 1 日」用 SQL 夹具直接落库
#      （审批链本身已由 P0-06 演练证明；本卡验的是**判定口径**，不是审批流）
# ============================================================================
set -uo pipefail
BASE="http://127.0.0.1:8039"
API="$BASE/api"
EV=/root/fix2/evidence
mkdir -p "$EV"
DB=cloudpivot_fix2c
PGP="${PGP:-$(grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2)}"
psqlat() { PGPASSWORD="$PGP" psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -A -t -P pager=off -c "$1" 2>&1 | sed -n '1p' | tr -d '\r'; }
psqlq()  { PGPASSWORD="$PGP" psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -P pager=off -c "$1"; }

login() { curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"$1\",\"password\":\"$2\"}" | jq -r '.data.access_token // .data.token // empty'; }

api() { # METHOD PATH TOKEN [BODY] → stdout=body, $HTTP=status
  local method="$1" path="$2" tok="$3" body="${4:-}" code
  if [ -n "$body" ]; then
    code=$(curl -sS -o /tmp/_f2_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    code=$(curl -sS -o /tmp/_f2_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$code"
  cat /tmp/_f2_body.json
}

step() { echo; echo "===== $* ====="; }
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); echo "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  ❌ $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $3"; else bad "$1 期望 [$3] 实际 [$2]"; fi; }

# ---------------------------------------------------------------- 0. 登录
step "0. 登录（admin 首登强制改密 + 演示账号 demo_sales）"
ADMIN_PWD_INIT=$(grep -m1 ^NIK_ADMIN_PASSWORD /root/fix2/env | cut -d= -f2-)
ADMIN_PWD='Admin@Fix2x9'
T=$(login admin "$ADMIN_PWD")
if [ -z "$T" ]; then
  T0=$(login admin "$ADMIN_PWD_INIT")
  if [ -n "$T0" ]; then
    curl -sS -o /dev/null -X PUT "$API/system/auth/password" -H "Authorization: Bearer $T0" \
      -H 'Content-Type: application/json' \
      -d "{\"old_password\":\"$ADMIN_PWD_INIT\",\"new_password\":\"$ADMIN_PWD\"}" >/dev/null
    T=$(login admin "$ADMIN_PWD")
  fi
fi
DEMO_PWD=$(grep -m1 ^NIK_DEMO_PASSWORD /root/fix2/env | cut -d= -f2-)
REP_PWD='Rep@Fix2x9b'
REP=$(login demo_sales "$REP_PWD")
if [ -z "$REP" ]; then
  # 演示账号首登强制改密（CODE-06）：先拿初始口令的令牌，改密后重登
  R0=$(login demo_sales "$DEMO_PWD")
  if [ -n "$R0" ]; then
    curl -sS -o /dev/null -X PUT "$API/system/auth/password" -H "Authorization: Bearer $R0" \
      -H 'Content-Type: application/json' \
      -d "{\"old_password\":\"$DEMO_PWD\",\"new_password\":\"$REP_PWD\"}" >/dev/null
    REP=$(login demo_sales "$REP_PWD")
  fi
fi
echo "  admin token=${#T}  demo_sales token=${#REP}"
[ -n "$T" ] && [ -n "$REP" ] || { echo "FATAL 登录失败"; exit 1; }

DEPT=$(psqlat "select id from sys_dept where dept_code = 'DEPT_DEMO' and deleted_at is null")
A1=$(psqlat "select id from biz_account where account_code = '5602.02' and deleted_at is null")
A2=$(psqlat "select id from biz_account where account_code = '5602.03' and deleted_at is null")
A3=$(psqlat "select id from biz_account where account_code = '5602.05' and deleted_at is null")
echo "  dept(DEPT_DEMO)=$DEPT"
echo "  A1(5602.02)=$A1  A2(5602.03)=$A2  A3(5602.05)=$A3"
[ -n "$DEPT" ] && [ -n "$A1" ] && [ -n "$A2" ] && [ -n "$A3" ] || { echo "FATAL 主数据缺失"; exit 1; }
RUN=$(date +%m%d%H%M%S)

step "0b. 夹具：把 business:erp:expense% 的一组权限授给 demo_sales 的角色（SALES_CLERK）"
psqlq "insert into sys_role_menu (id, role_id, menu_id, created_at)
       select gen_random_uuid(), r.id, m.id, now()
         from sys_role r, sys_menu m
        where r.role_code = 'SALES_CLERK'
          and m.auth_code like 'business:erp:expense%'
          and not exists (select 1 from sys_role_menu x where x.role_id = r.id and x.menu_id = m.id)"
psqlat "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='SALES_CLERK' and m.auth_code like 'business:erp:expense%'" | sed 's/^/  demo_sales 现有 expense 权限条数: /'
REP=$(login demo_sales "$REP_PWD"); [ -n "$REP" ] || REP=$(login demo_sales "$DEMO_PWD")
check "0b. demo_sales 令牌非空" "$([ -n "$REP" ] && echo yes || echo no)" "yes"

# ---------------------------------------------------------------- 1. 费用类型
step "1. 建 3 个受预算控制的费用类型（分别绑 5602.02 / .03 / .05）"
mktype() { # $1=code $2=name $3=account_id
  api POST "/business/erp/expense-type" "$T" \
    "{\"type_code\":\"$1\",\"type_name\":\"$2\",\"account_id\":\"$3\",\"tax_deductible\":false,\"budget_controlled\":true,\"need_invoice_no\":false,\"status\":1}" > "/tmp/_f2_type_$1.json"
  local id; id=$(jq -r '.data.id // empty' "/tmp/_f2_type_$1.json")
  echo "  $1 http=$HTTP $(jq -c '{code,msg,account_code:.data.account_code,budget_controlled:.data.budget_controlled}' "/tmp/_f2_type_$1.json")" >&2
  printf '%s' "$id"
}
TY1=$(mktype "F2_DRAFT_$RUN"   "FIX2 草稿槽" "$A1")
TY2=$(mktype "F2_PENDING_$RUN" "FIX2 未生效槽" "$A2")
TY3=$(mktype "F2_ACTIVE_$RUN"  "FIX2 已生效槽" "$A3")
echo "  TY1=$TY1 TY2=$TY2 TY3=$TY3"
[ -n "$TY1" ] && [ -n "$TY2" ] && [ -n "$TY3" ] || { echo "FATAL 费用类型没建起来"; exit 1; }

# ---------------------------------------------------------------- 2. 三份预算
step "2. 编制三份预算（槽级策略 BLOCK；全局策略保持默认 WARN）"
GLOBAL_CFG=$(psqlat "select coalesce((select config_value from sys_config where config_key='erp.budget.control_mode' and deleted_at is null),'<无故 no row>')")
echo "  全局控制策略 config = $GLOBAL_CFG（无行 = 代码缺省 WARN）"
mkbudget() { # $1=tag $2=account $3=total $4=m11 $5=m12
  local periods="[{\"month\":1,\"amount\":\"0\"},{\"month\":2,\"amount\":\"0\"},{\"month\":3,\"amount\":\"0\"},{\"month\":4,\"amount\":\"0\"},{\"month\":5,\"amount\":\"0\"},{\"month\":6,\"amount\":\"0\"},{\"month\":7,\"amount\":\"0\"},{\"month\":8,\"amount\":\"0\"},{\"month\":9,\"amount\":\"0\"},{\"month\":10,\"amount\":\"0\"},{\"month\":11,\"amount\":\"$4\"},{\"month\":12,\"amount\":\"$5\"}]"
  api POST "/business/erp/budget" "$T" \
    "{\"fiscal_year\":2026,\"dept_id\":\"$DEPT\",\"account_id\":\"$2\",\"total_amount\":\"$3\",\"control_mode\":\"BLOCK\",\"periods\":$periods,\"remark\":\"FIX-2 $1\"}" > "/tmp/_f2_b_$1.json"
  local id; id=$(jq -r '.data.budget.id // empty' "/tmp/_f2_b_$1.json")
  echo "  $1 http=$HTTP $(jq -c '{code,msg,no:.data.budget.budget_no,status:.data.budget.status,mode:.data.budget.control_mode,total:.data.budget.total_amount}' "/tmp/_f2_b_$1.json")" >&2
  printf '%s' "$id"
}
B1=$(mkbudget draft "$A1" 10000 10000 0)
B2=$(mkbudget pending "$A2" 20000 10000 10000)
B3=$(mkbudget active "$A3" 10000 10000 0)
echo "  B1(DRAFT)=$B1  B2(未到生效日)=$B2  B3(已生效)=$B3"
[ -n "$B1" ] && [ -n "$B2" ] && [ -n "$B3" ] || { echo "FATAL 预算没建起来"; exit 1; }

step "2b. 夹具：B2 → ACTIVE + effective_from=2026-12-01（次月 1 日）；B3 → ACTIVE + effective_from=2026-01-01"
psqlq "update biz_budget set status='ACTIVE', approved_at=now(), effective_from='2026-12-01' where id='$B2'::uuid" >/dev/null
psqlq "update biz_budget set status='ACTIVE', approved_at=now(), effective_from='2026-01-01' where id='$B3'::uuid" >/dev/null
psqlq "select budget_no, status, control_mode, total_amount, effective_from from biz_budget where id in ('$B1'::uuid,'$B2'::uuid,'$B3'::uuid) order by budget_no"

# ---------------------------------------------------------------- 3. 报销单工厂
mkexpense() { # $1=tag $2=amount $3=day $4=type_id $5=title
  local body="{\"title\":\"$5\",\"expense_date\":\"$3\",\"dept_id\":\"$DEPT\",\"remark\":\"FIX-2 验收\",\"items\":[{\"expense_date\":\"$3\",\"expense_type_id\":\"$4\",\"with_tax_amount\":\"$2\",\"tax_rate\":\"0\",\"summary\":\"$5\",\"invoice_type\":\"NONE\"}]}"
  api POST "/business/erp/expense" "$REP" "$body" > "/tmp/_f2_e_$1_create.json"
  local id; id=$(jq -r '.data.report.id // empty' "/tmp/_f2_e_$1_create.json")
  echo "  [$1] create http=$HTTP $(jq -c '{code,msg,no:.data.report.report_no}' "/tmp/_f2_e_$1_create.json")" >&2
  if [ -z "$id" ]; then echo "  [$1] 建单失败" >&2; return 1; fi
  api POST "/business/erp/expense/$id/submit" "$REP" > "/tmp/_f2_e_$1_submit.json"
  echo "  [$1] submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,budget_mode:.data.report.budget_mode,warnings:[(.data.budget_warnings // [])[]|{code,status,status_label}],usages:((.data.budget_usages // [])|length)}' "/tmp/_f2_e_$1_submit.json")" >&2
  printf '%s' "$id"
}

# ================================================================ ① DRAFT
step "① DRAFT 预算（槽级 BLOCK）+ 超支报销 15000/10000 → 放行 + 「预算未生效」提示"
R1=$(mkexpense draft 15000 2026-11-03 "$TY1" "FIX2-①草稿预算超支")
check "① 单据状态（放行）" "$(jq -r '.data.report.status // "?"' /tmp/_f2_e_draft_submit.json)" "SUBMITTED"
check "① 提示条数" "$(jq -r '(.data.budget_warnings // [])|length' /tmp/_f2_e_draft_submit.json)" "1"
check "① 提示码" "$(jq -r '(.data.budget_warnings // [])[0].code // "?"' /tmp/_f2_e_draft_submit.json)" "BUDGET_NOT_EFFECTIVE"
check "① 提示里的预算状态" "$(jq -r '(.data.budget_warnings // [])[0].status // "?"' /tmp/_f2_e_draft_submit.json)" "DRAFT"
echo "  提示全文：$(jq -r '(.data.budget_warnings // [])[0].message // "-"' /tmp/_f2_e_draft_submit.json)"
check "① DB 单据状态" "$(psqlat "select status from biz_expense_report where id='$R1'::uuid")" "SUBMITTED"
check "① DB 占用已写（放行 = 照常占用）" "$(psqlat "select count(*) from biz_budget_usage where source_id='$R1'::uuid")" "1"
check "① 槽级 BLOCK 未生效 → 额度按 0，只标记不拦截（over_budget=1）" "$(psqlat "select over_budget from biz_budget_usage where source_id='$R1'::uuid")" "1"

# ================================================================ ② 未到生效日
step "② ACTIVE 预算 effective_from=2026-12-01 → 11 月单据不受新额度约束（超支额 = 全额）"
R2=$(mkexpense pending 2000 2026-11-04 "$TY2" "FIX2-②未到生效日")
check "② 单据状态（放行）" "$(jq -r '.data.report.status // "?"' /tmp/_f2_e_pending_submit.json)" "SUBMITTED"
check "② 提示里的预算状态" "$(jq -r '(.data.budget_warnings // [])[0].status // "?"' /tmp/_f2_e_pending_submit.json)" "ACTIVE"
check "② 提示状态中文（未到生效日）" "$(jq -r '(.data.budget_warnings // [])[0].status_label // "?"' /tmp/_f2_e_pending_submit.json)" "已生效（未到生效日）"
check "② 超支额 = 全额（12 月额度没被算进 11 月）" "$(jq -r '.data.report.over_amount // "?"' /tmp/_f2_e_pending_submit.json)" "2000.0000"
echo "  提示全文：$(jq -r '(.data.budget_warnings // [])[0].message // "-"' /tmp/_f2_e_pending_submit.json)"
check "② DB 占用 over_amount" "$(psqlat "select over_amount::text from biz_budget_usage where source_id='$R2'::uuid")" "2000.0000"

step "②反证 同槽 12 月单据 15000 → 生效日后额度开始管：年度 20000−已用 2000=18000、月 10000 → 超 5000 且 BLOCK → 409"
R2B=$(mkexpense pending_dec 15000 2026-12-05 "$TY2" "FIX2-②12月生效后超支")
check "②反证 无「未生效」提示" "$(jq -r '(.data.budget_warnings // [])|length' /tmp/_f2_e_pending_dec_submit.json)" "0"
check "②反证 超支错误码" "$(jq -r '.code // 0' /tmp/_f2_e_pending_dec_submit.json)" "66037"
check "②反证 单据仍 DRAFT" "$(psqlat "select status from biz_expense_report where id='$R2B'::uuid")" "DRAFT"

# ================================================================ ③ 已生效
step "③ ACTIVE 预算 effective_from=2026-01-01 → 正常占用（3000）+ 超支 20000 被 409 拦截"
R3=$(mkexpense active 3000 2026-11-06 "$TY3" "FIX2-③已生效额度内")
check "③ 额度内单据状态" "$(jq -r '.data.report.status // "?"' /tmp/_f2_e_active_submit.json)" "SUBMITTED"
check "③ 额度内无「未生效」提示" "$(jq -r '(.data.budget_warnings // [])|length' /tmp/_f2_e_active_submit.json)" "0"
check "③ 单据 budget_mode（槽级 BLOCK）" "$(jq -r '.data.report.budget_mode // "?"' /tmp/_f2_e_active_submit.json)" "BLOCK"
check "③ DB 占用 OCCUPIED/未超" "$(psqlat "select status || '/' || over_budget from biz_budget_usage where source_id='$R3'::uuid")" "OCCUPIED/0"
check "③ DB 单据 budget_mode" "$(psqlat "select budget_mode from biz_expense_report where id='$R3'::uuid")" "BLOCK"

R3B=$(mkexpense active_over 20000 2026-11-07 "$TY3" "FIX2-③超支拦截")
echo "  超支提交报文：$(jq -c '{code,msg}' /tmp/_f2_e_active_over_submit.json)"
check "③ 超支错误码（ERP_BUDGET_EXCEEDED）" "$(jq -r '.code // 0' /tmp/_f2_e_active_over_submit.json)" "66037"
check "③ 超支后单据仍是 DRAFT" "$(psqlat "select status from biz_expense_report where id='$R3B'::uuid")" "DRAFT"
check "③ 超支后零占用行（事务回滚）" "$(psqlat "select count(*) from biz_budget_usage where source_id='$R3B'::uuid")" "0"

# ================================================================ ④ usages[]
step "④ 预算详情 usages[] 能看到报销占用（source_type / source_id / 金额）"
api GET "/business/erp/budget/$B3" "$T" > "$EV/b3_detail.json"
echo "  http=$HTTP"
jq -c '{year_used:.data.year_used, year_available:.data.year_available, usages:[.data.usages[]|{source_type,source_type_label,source_id,source_no,amount,status}]}' "$EV/b3_detail.json"
check "④ usages 条数" "$(jq -r '.data.usages|length' "$EV/b3_detail.json")" "1"
check "④ source_type" "$(jq -r '.data.usages[0].source_type' "$EV/b3_detail.json")" "EXPENSE_REPORT"
check "④ source_type_label" "$(jq -r '.data.usages[0].source_type_label' "$EV/b3_detail.json")" "费用报销单"
check "④ source_id = 报销单 id" "$(jq -r '.data.usages[0].source_id' "$EV/b3_detail.json")" "$R3"
check "④ 金额" "$(jq -r '.data.usages[0].amount' "$EV/b3_detail.json")" "3000.0000"
check "④ year_used 与占用合计一致" "$(jq -r '.data.year_used' "$EV/b3_detail.json")" "3000.0000"
api GET "/business/erp/budget/$B1" "$T" > "$EV/b1_detail.json"
check "④ DRAFT 槽（B1）也能看到自己那笔报销占用" "$(jq -r '.data.usages|length' "$EV/b1_detail.json")" "1"
check "④ DRAFT 槽 usages[0].source_no" "$(jq -r '.data.usages[0].source_no' "$EV/b1_detail.json")" "$(psqlat "select report_no from biz_expense_report where id='$R1'::uuid")"

# ================================================================ ⑤ budget_mode
step "⑤ 槽级 BLOCK 的单据 budget_mode 记 BLOCK（不是全局默认 WARN）"
check "⑤ 全局策略 = WARN（槽级 BLOCK 才是实际生效策略）" "$GLOBAL_CFG" "WARN"
check "⑤ 单据 budget_mode（11 月额度内、槽级 BLOCK）" "$(psqlat "select budget_mode from biz_expense_report where id='$R3'::uuid")" "BLOCK"
check "⑤ 同槽 11 月占用 over_budget" "$(psqlat "select over_budget from biz_budget_usage where source_id='$R3'::uuid")" "0"
echo "  ⑤ 说明：修复前这里记的是全局 WARN；同槽超支时直接 409（③），不会落一张「记错策略」的单"

# ================================================================ ⑥ 报表
step "⑥ 执行报表：未到生效日 effective=false / 已生效 effective=true（与控制同口径）"
api GET "/business/erp/budget/report?fiscal_year=2026" "$T" > "$EV/report.json"
echo "  http=$HTTP"
jq -c '[.data[]|select(.budget_id=="'"$B2"'" or .budget_id=="'"$B3"'")|{account_code,effective,effective_from,total_amount,used,available}]' "$EV/report.json"
check "⑥ B2（未到生效日）effective" "$(jq -r '[.data[]|select(.budget_id=="'"$B2"'")][0].effective' "$EV/report.json")" "false"
check "⑥ B2 effective_from" "$(jq -r '[.data[]|select(.budget_id=="'"$B2"'")][0].effective_from' "$EV/report.json")" "2026-12-01"
check "⑥ B3（已生效）effective" "$(jq -r '[.data[]|select(.budget_id=="'"$B3"'")][0].effective' "$EV/report.json")" "true"

echo
echo "================ 汇总 ================"
echo "PASS=$PASS FAIL=$FAIL"
printf 'PASS=%s FAIL=%s\n' "$PASS" "$FAIL" > "$EV/summary.txt"
[ "$FAIL" = "0" ] || exit 1
