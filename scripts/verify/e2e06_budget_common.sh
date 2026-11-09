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
  # 预算编制人：必须是**被编预算那个部门**的成员 —— 修好 n_dept（t_1fafa2ae）之后，
  # 「部门主管」按**发起人所在部门**的 leader_id 解析，编制人不在部门里就解析不出人
  # （会退化成超管兜底）。这里用专门的 e2e06_planner（DEPT_LEADER + E2E06-BDG）。
  T_P3A=$(login e2e06_planner 'Drill@123456' planner) || exit 1; save T_P3A "$T_P3A"
  say "  · e2e06_planner（DEPT_LEADER，预算编制人，属 E2E06-BDG）登录成功"
}

acc() { SQ "select id from biz_account where account_code='$1' and deleted_at is null"; }

# 上传附件（multipart）。规则表 biz_attachment_rule: EXPENSE_REPORT 在 SUBMIT 阶段要求 ≥1 个（file_role 不限）
upfile() { # upfile <tag> <doc_type> <doc_id> <file_role> <local_path>
  # 用**报销人本人**（e2e06_clerk，DRILL_CLERK + EMPLOYEE 身份角色）上传。
  # t_1fafa2ae 起 EMPLOYEE 持有 business:attachment:upload / list（种子
  # S__employee_self_service_permissions.sql），所以「员工自己传发票 → 提交报销」
  # 这条自助链路可以按真实角色跑；此前这里用 admin 代传，正好把 403 缺口盖住了。
  local name=$1 dt=$2 did=$3 role=$4 path=$5 out="$RAW/$1.json"
  HTTP=$(curl -s -m 60 -o "$out" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
    --oauth2-bearer "$T_CLERK" -F "doc_type=$dt" -F "doc_id=$did" \
    -F "file_role=$role" -F "remark=E2E-06 演练附件（报销人本人上传）" -F "file=@$path")
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

  # ---- 预算编制人（e2e06_planner）：DEPT_LEADER + **属于 E2E06-BDG** ----
  # 为什么必须新开一个号而不是复用 p3a_0002：
  #   修好 budget_approve 的 n_dept 之后（t_1fafa2ae / V0355），部门主管按**发起人所在部门**
  #   解析。p3a_0002 在库里没有部门（dept_id IS NULL）→ 解析 0 人 → 落 FALLBACK_ADMIN，
  #   演练就会变成「超管批预算」，测不到「A 部门预算只派 A 部门主管」。而且 DEPT_LEADER
  #   的 data_scope=3，把它硬塞进演练部门会连带改掉别的卡对它的可见性预期。
  local role_leader got
  role_leader=$(SQ "select id from sys_role where role_code='DEPT_LEADER' and deleted_at is null")
  if [ -z "$(SQ "select id from sys_user where username='e2e06_planner'")" ]; then
    api fx_planner POST /system/user "$T_ADMIN" \
      "{\"username\":\"e2e06_planner\",\"password\":\"Drill@123456\",\"real_name\":\"E2E06 预算编制人\",\"dept_id\":\"$dept_id\",\"status\":1,\"role_ids\":[\"$role_leader\"],\"remark\":\"E2E-06 预算编制人（属演练部门）\"}"
    check "建用户 e2e06_planner" "$CODE" "0"
  fi
  got=$(SQ "select coalesce(d.dept_code,'-') from sys_user u left join sys_dept d on d.id=u.dept_id where u.username='e2e06_planner'")
  check "预算编制人属演练部门" "$got" "E2E06-BDG"

  # ---- 报销人挂 EMPLOYEE 身份角色（附件上传权限的载体）----
  # 种子只给 EMPLOYEE 授 attachment:upload/list（管理岗 6 个角色本来就有）。
  # DRILL_CLERK 是「只报销、不带审批」的演练岗位，两个角色叠加 = 真实的「员工 + 岗位」。
  # 用 SQL 直接落绑定（等价于后台「用户 → 角色」勾选；幂等）。
  if [ -n "$(SQ "select id from sys_role where role_code='EMPLOYEE' and deleted_at is null")" ]; then
    SQ "insert into sys_user_role (id, user_id, role_id)
          select gen_random_uuid(), u.id, r.id
            from sys_user u, sys_role r
           where u.username='e2e06_clerk' and r.role_code='EMPLOYEE' and r.deleted_at is null
             and not exists (select 1 from sys_user_role x where x.user_id=u.id and x.role_id=r.id)" >/dev/null
    got=$(SQ "select count(*) from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where u.username='e2e06_clerk' and r.role_code='EMPLOYEE'")
    check "报销人持有 EMPLOYEE（附件上传权限载体）" "$got" "1"
  fi

  # ---- 口令指纹修复（卡面验收：sys_user.password 必须都是 argon2 PHC 串）----
  # 共享库里有一批历史夹具账号的 password 是占位符（实测 1 字节 'x'）：
  #   · 登录校验直接 Err —— 改造前报 500，现已按 401 处理（auth/handler.rs）；
  #   · 它们被种在审批角色里时会让**会签节点永远凑不齐票**（本卡的根因之一）。
  # 演练库统一把这类行重置成演练口令的哈希（这些号原本就登不进去，无信息损失）；
  # 生产请走「系统管理 → 用户 → 重置密码」，不要跑这条脚本。
  local badpw appr_hash
  badpw=$(SQ "select count(*) from sys_user where deleted_at is null and password not like '\$argon2id\$%'")
  if [ "${badpw:-0}" != "0" ]; then
    appr_hash=$(SQ "select u.password from sys_user u where u.username='e2e01_appr'")
    SQ "update sys_user set password='$appr_hash' where deleted_at is null and password not like '\$argon2id\$%'" >/dev/null
    FIX_PW=$badpw; save FIX_PW "$badpw"
    say "  · 夹具修复：$badpw 个账号的 password 不是 argon2 指纹（登录必然失败）→ 统一重置为演练口令哈希"
  fi
  got=$(SQ "select count(*) from sys_user where deleted_at is null and password not like '\$argon2id\$%'")
  check "全库口令指纹均为 argon2id" "$got" "0"
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
  # t_1fafa2ae 之后 n_dept 是「发起人所在部门主管 + 或签」，所以：
  #   n_dept  → 派给 e2e06_mgr（演练部门负责人），用 T_MGR 批；
  #   n_fin* / n_gm* → 角色（FINANCE_MANAGER / GENERAL_MANAGER / …）候选可能多人，
  #                    只批 e2e01_appr 名下那条（或签：一人同意即通过，兄弟待办随后 CANCELLED）；
  #   兜底：万一是 FALLBACK_ADMIN 出来的待办（部门没负责人 / 编制人被内控排除），用 T_ADMIN 批。
  # 别人的待办一律跳过 —— 代批等于伪造审批意见。
  local bid=$1 tag=$2 tried=0 row tid user
  while [ "$tried" -lt 8 ]; do
    local pending
    pending=$(tasks_of BUDGET_APPROVE "$bid" PENDING)
    [ -z "$pending" ] && break
    while read -r row; do
      [ -z "$row" ] && continue
      tid=${row%%|*}; user=$(printf '%s' "$row" | awk -F'|' '{print $3}')
      case "$user" in
        e2e06_mgr)  api "${tag}_dept_$tried" POST "/business/flow/task/$tid/approve" "$T_MGR"  '{"comment":"部门主管同意（E2E-06）"}' ;;
        e2e01_appr) api "${tag}_fin_$tried"  POST "/business/flow/task/$tid/approve" "$T_APPR" '{"comment":"预算审批通过（E2E-06）"}' ;;
        admin)      api "${tag}_adm_$tried"  POST "/business/flow/task/$tid/approve" "$T_ADMIN" '{"comment":"兜底超管同意（E2E-06）"}' ;;
        *) say "  · 跳过非本卡账号的待办（$user，或签兄弟待办，节点由已批的那票推进）"; continue ;;
      esac
      check "$tag 审批 $user http" "$HTTP" "200"
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
