#!/usr/bin/env bash
# E2E-06 预算+报销域演练公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）
# 被 e2e06_budget_a.sh / e2e06_budget_b.sh source。
# 状态落 /tmp/e2e06/state.sh，报文落 /tmp/e2e06/raw/，断言追加 /tmp/e2e06/asserts.log。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e06}; RAW=$WORK/raw; STATE=$WORK/state.sh; LOG=$WORK/asserts.log
mkdir -p "$RAW"; touch "$LOG"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
SQ()   { PSQLQ -c "$1" 2>&1 | tr '\n' ' ' | sed 's/  */ /g; s/ *$//'; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf 'PASS\t%s\n' "$*" >> "$LOG"; say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); printf 'FAIL\t%s\n' "$*" >> "$LOG"; say "  ❌ $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
checkne() { if [ "$2" != "$3" ]; then ok "$1 = $2（≠ $3）"; else bad "$1 = $2（不应等于 $3）"; fi; }
chknum() { # 金额容差 0.005
  if awk -v a="${2:-x}" -v b="${3:-x}" 'BEGIN{ if (a=="x"||b=="x") exit 1; d=a-b; if(d<0)d=-d; exit !(d<0.005) }'; then
    ok "$1 = $2（期望 $3）"; else bad "$1 = $2（期望 $3）"; fi
}
summary() { say; say "== PASS=$PASS FAIL=$FAIL"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1"="$2"; }
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null | head -c 400; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  if [ "$HTTP" = 401 ] && [ -n "$token" ]; then
    local fresh
    fresh=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
      -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
    if [ -n "$fresh" ]; then
      T_ADMIN=$fresh; save T_ADMIN "$fresh"
      local -a retry=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path" --oauth2-bearer "$fresh")
      [ -n "$body" ] && retry+=(-H 'Content-Type: application/json' -d "$body")
      HTTP=$(curl "${retry[@]}")
    fi
  fi
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}

login() { # login <user> <pass> <tag>
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then data '.data.access_token'
  else say "FATAL: 登录失败 $1 http=$HTTP $(show)" >&2; return 1; fi
}

login_all() {
  T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1; save T_ADMIN "$T_ADMIN"
  say "  · admin（SUPER_ADMIN）登录成功"
}

# 夹具用户建好之后再登录其余账号
login_rest() {
  T_APPR=$(login e2e01_appr 'E2e01@Appr123' appr) || exit 1; save T_APPR "$T_APPR"
  say "  · e2e01_appr（DEPT_LEADER+FM+GM，预算/报销审批人）登录成功"
  T_CLERK=$(login e2e06_clerk 'Drill@123456' clerk) || exit 1; save T_CLERK "$T_CLERK"
  say "  · e2e06_clerk（DRILL_CLERK，报销人）登录成功"
  T_MGR=$(login e2e06_mgr 'Drill@123456' mgr) || exit 1; save T_MGR "$T_MGR"
  say "  · e2e06_mgr（FINANCE_MANAGER，部门主管=直属上级）登录成功"
  T_P3A=$(login p3a_0002 'E2e01@Appr123' p3a) || exit 1; save T_P3A "$T_P3A"
  say "  · p3a_0002（DEPT_LEADER，预算编制人）登录成功"
}

acc() { SQ "select id from biz_account where account_code='$1' and deleted_at is null"; }

# 上传附件（multipart）。规则表 biz_attachment_rule: EXPENSE_REPORT 在 SUBMIT 阶段要求 ≥1 个（file_role 不限）
upfile() { # upfile <tag> <doc_type> <doc_id> <file_role> <local_path>
  # 用 admin 上传：DRILL_CLERK 没有 business:attachment:upload 权限（403），
  # 而附件只是「提交前的单据材料」，不代表审批人意见。
  local name=$1 dt=$2 did=$3 role=$4 path=$5 out="$RAW/$1.json"
  HTTP=$(curl -s -m 60 -o "$out" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
    --oauth2-bearer "$T_ADMIN" -F "doc_type=$dt" -F "doc_id=$did" \
    -F "file_role=$role" -F "remark=E2E-06 演练附件" -F "file=@$path")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}

# 报销单提交前置：至少 1 个发票影像（ATT-06）。幂等：已有就不重复传。
exp_att() { # exp_att <report_id> <tag>
  local n
  n=$(SQ "select count(*) from biz_doc_attachment where doc_type='EXPENSE_REPORT' and doc_id='$1' and deleted_at is null")
  if [ "$n" -gt 0 ]; then ok "报销附件已存在（$2）"; return 0; fi
  [ -f "$WORK/drill-invoice.pdf" ] || printf '%%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<</Root 1 0 R>>\n%%%%EOF\n' > "$WORK/drill-invoice.pdf"
  upfile "att_$2" EXPENSE_REPORT "$1" INVOICE_SCAN "$WORK/drill-invoice.pdf"
  check "上传发票影像 $2 http" "$HTTP" "200"
  check "上传发票影像 $2 code" "$CODE" "0"
  check "附件落库（EXPENSE_REPORT / INVOICE_SCAN）" \
    "$(SQ "select count(*) from biz_doc_attachment where doc_type='EXPENSE_REPORT' and doc_id='$1' and file_role='INVOICE_SCAN' and deleted_at is null")" "1"
}

# ---------- 夹具（真实 API 建用户/部门，缺号修复用 SQL 并如实记录） ----------
ensure_fixtures() {
  local dept_id role_clerk role_mgr
  dept_id=$(SQ "select id from sys_dept where dept_code='E2E06-BDG' and deleted_at is null")
  if [ -z "$dept_id" ]; then
    api fx_dept POST /system/dept "$T_ADMIN" \
      '{"dept_code":"E2E06-BDG","dept_name":"E2E06 预算演练部","status":1,"sort_order":96}'
    check "建部门 E2E06-BDG http" "$HTTP" "200"
    check "建部门 E2E06-BDG code" "$CODE" "0"
    dept_id=$(SQ "select id from sys_dept where dept_code='E2E06-BDG' and deleted_at is null")
  fi
  save DEPT_ID "$dept_id"
  role_clerk=$(SQ "select id from sys_role where role_code='DRILL_CLERK'")
  role_mgr=$(SQ "select id from sys_role where role_code='FINANCE_MANAGER'")

  if [ -z "$(SQ "select id from sys_user where username='e2e06_clerk'")" ]; then
    api fx_clerk POST /system/user "$T_ADMIN" \
      "{\"username\":\"e2e06_clerk\",\"password\":\"Drill@123456\",\"real_name\":\"E2E06 报销员\",\"dept_id\":\"$dept_id\",\"status\":1,\"role_ids\":[\"$role_clerk\"],\"remark\":\"E2E-06 报销人\"}"
    check "建用户 e2e06_clerk" "$CODE" "0"
  fi
  if [ -z "$(SQ "select id from sys_user where username='e2e06_mgr'")" ]; then
    api fx_mgr POST /system/user "$T_ADMIN" \
      "{\"username\":\"e2e06_mgr\",\"password\":\"Drill@123456\",\"real_name\":\"E2E06 部门主管\",\"dept_id\":\"$dept_id\",\"status\":1,\"role_ids\":[\"$role_mgr\"],\"remark\":\"E2E-06 直属上级\"}"
    check "建用户 e2e06_mgr" "$CODE" "0"
  fi
  # 部门主管 = 报销的 INITIATOR_SUPERIOR 解析目标
  SQ "update sys_dept set leader_id=(select id from sys_user where username='e2e06_mgr') where id='$dept_id'::uuid" >/dev/null
  check "部门主管指向 e2e06_mgr" \
    "$(SQ "select u.username from sys_dept d join sys_user u on u.id=d.leader_id where d.id='$dept_id'::uuid")" "e2e06_mgr"

  # 已知缺陷修复（如实记录）：p3a_0002 的 password 列是占位符 'x'（非 argon2），登录 500；
  # 它被种进全局 DEPT_LEADER 角色，而预算审批「部门主管」节点是会签（approval_mode=ALL）
  # → 预算永远批不完。本卡把它重置成与 e2e01_appr 相同哈希，演练结束后保留（原值 'x' 无法登录）。
  local ph len
  ph=$(SQ "select password from sys_user where username='p3a_0002'")
  len=${#ph}
  if [ "$len" -lt 20 ]; then
    SQ "update sys_user set password=(select password from sys_user where username='e2e01_appr') where username='p3a_0002'" >/dev/null
    FIX_P3A=1; save FIX_P3A 1
    say "  · 夹具修复：p3a_0002 密码哈希是 ${len} 字符占位符（登录 500）→ 重置为 e2e01_appr 的哈希"
  fi
}

# 演练期间 2027-01..2027-06（FY2027，避开 2026 业务年 / 2022 年结年 / 2027-12~2028-06 资产段）
ensure_periods() {
  local from=2027-01 to=2027-06 have
  have=$(SQ "select count(*) from biz_accounting_period where period_code between '$from' and '$to'")
  say "  · 演练期间 $from..$to 现有 $have 期"
  if [ "$have" = 6 ]; then ok "演练期间齐备（6 期）"; return 0; fi
  api per_batch POST /business/erp/period/batch-open "$T_ADMIN" \
    "{\"from\":\"$from\",\"to\":\"$to\",\"remark\":\"E2E-06 预算/报销演练期间\"}"
  check "批量开账 http" "$HTTP" "200"
  check "批量开账 code" "$CODE" "0"
  check "演练期间数" "$(SQ "select count(*) from biz_accounting_period where period_code between '$from' and '$to'")" "6"
}

# ---------- 审批辅助 ----------
# tasks_of <biz_type> <biz_id> <status>：输出 task_id|node_key|username
tasks_of() {
  PSQLQ -c "select t.id||'|'||t.node_key||'|'||coalesce(u.username,'-') from biz_flow_task t
              join biz_flow_instance i on i.id=t.instance_id
              left join sys_user u on u.id=t.assignee_id
             where i.biz_type='$1' and i.biz_id='$2'::uuid and t.status='$3'
             order by t.created_at, t.node_key"
}
approve_tasks_of() { # approve_tasks_of <tag> <biz_type> <biz_id> <token> <comment>
  local tag=$1 biz=$2 bid=$3 tok=$4 cmt=${5:-同意} n=0 row tid
  while read -r row; do
    [ -z "$row" ] && continue
    tid=${row%%|*}
    api "${tag}_$n" POST "/business/flow/task/$tid/approve" "$tok" "{\"comment\":\"$cmt\"}"
    check "$tag 审批待办 $tid http" "$HTTP" "200"
    n=$((n+1))
  done < <(tasks_of "$biz" "$bid" PENDING)
  echo "$n"
}

budget_active() { # budget_active <budget_id> <tag>：把预算从 SUBMITTED 批到 ACTIVE
  # 只批 e2e01_appr 名下的待办：会签 n_dept 里编制人已被内控排除；n_fin 是 ANY（或签），
  # 同角色其他成员（e2e02_appr / e2e04_fin）名下也会各有一条 —— 那是别人的待办，不能代批，
  # 也不影响节点：任一人同意即通过，兄弟待办随后 CANCELLED。
  local bid=$1 tag=$2 tried=0 row tid
  while [ "$tried" -lt 6 ]; do
    local pending
    pending=$(tasks_of BUDGET_APPROVE "$bid" PENDING | grep '|e2e01_appr$' || true)
    [ -z "$pending" ] && break
    while read -r row; do
      [ -z "$row" ] && continue
      tid=${row%%|*}
      api "${tag}_$tried" POST "/business/flow/task/$tid/approve" "$T_APPR" '{"comment":"预算审批通过（E2E-06）"}'
      check "$tag 审批待办 http" "$HTTP" "200"
    done < <(printf '%s\n' "$pending")
    tried=$((tried+1))
  done
}

expense_approve() { # expense_approve <report_id> <tag>：直属上级(e2e06_mgr) → 财务经理(e2e01_appr)
  local rid=$1 tag=$2 tried=0 row tid user
  while [ "$tried" -lt 6 ]; do
    local pending
    pending=$(tasks_of EXPENSE "$rid" PENDING)
    [ -z "$pending" ] && break
    while read -r row; do
      [ -z "$row" ] && continue
      tid=${row%%|*}; user=$(printf '%s' "$row" | awk -F'|' '{print $3}')
      case "$user" in
        e2e06_mgr) api "${tag}_sup" POST "/business/flow/task/$tid/approve" "$T_MGR" '{"comment":"直属上级同意（E2E-06）"}' ;;
        e2e01_appr) api "${tag}_fin" POST "/business/flow/task/$tid/approve" "$T_APPR" '{"comment":"财务经理同意（E2E-06）"}' ;;
        *) say "  · 跳过非本卡账号的待办（$user，或签兄弟待办，节点由已批的那票推进）"; continue ;;
      esac
      check "$tag 审批 $user http" "$HTTP" "200"
    done < <(printf '%s\n' "$pending")
    tried=$((tried+1))
  done
}
