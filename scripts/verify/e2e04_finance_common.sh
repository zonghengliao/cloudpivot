#!/usr/bin/env bash
# E2E-04 财务域演练公共库（145 测试机 · cloudpivot_dev · 真后端 :8033）
# 被 e2e04_finance_{a,b,c}.sh source。
# 状态落 /tmp/e2e04/state.sh，报文落 /tmp/e2e04/raw/
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e04}; RAW=$WORK/raw; STATE=$WORK/state.sh
mkdir -p "$RAW"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); say "  ❌ $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
checkne(){ if [ "$2" != "$3" ]; then ok "$1 = $2（≠ $3）"; else bad "$1 = $2（不应等于 $3）"; fi; }
summary(){ say; say "== PASS=$PASS FAIL=$FAIL"; [ "$FAIL" = 0 ] || say "== 有失败项"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1=$2"; }
money(){ printf '%s' "$BODY" | jq -r "($1) // 0" 2>/dev/null | awk '{printf "%.2f", $1+0}'; }
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
show() { printf '%s' "$BODY" | jq -c '{code,msg,detail:(.data//{}|keys?)}' 2>/dev/null | head -c 400; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  if [ -n "$body" ]; then args+=(-H 'Content-Type: application/json' -d "$body"); fi
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

login() {
  api login_admin POST /system/auth/login "" "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then
    T_ADMIN=$(data '.data.access_token'); save T_ADMIN "$T_ADMIN"
    say "  · 登录 admin 成功（token 长度 ${#T_ADMIN}）"
  else
    say "FATAL: admin 登录失败 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; exit 1
  fi
}

login_as() { # login_as <user> <pass> <tag> → 打印 token
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  if [ "$HTTP" = 200 ] && [ "$CODE" = 0 ]; then data '.data.access_token'
  else say "FATAL: 登录失败 $1 http=$HTTP $(printf '%s' "$BODY" | head -c 300)" >&2; return 1; fi
}

# 手工凭证：建单(DRAFT, admin 制单) → 审核(AUDITED, 财务经理) → 记账(POSTED, 财务经理)
# 制单/审核分离开关 erp.separation_of_duties.approve=true，所以审核必须是另一个账号。
mkpost() { # mkpost <tag> <date> <summary> <entries-json> → 打印凭证 id
  local tag=$1 date=$2 sum=$3 entries=$4 vid st
  api "${tag}_new" POST /business/erp/voucher "$T_ADMIN" \
    "{\"voucher_date\":\"$date\",\"summary\":\"$sum\",\"entries\":$entries}"
  if [ "$HTTP" != 200 ] || [ "$CODE" != 0 ]; then
    bad "建单 $tag 失败 http=$HTTP $(show)"; return 1
  fi
  vid=$(data '.data.id')
  st=$(PSQLQ -c "select status from biz_voucher where id='$vid';")
  [ "$st" = "DRAFT" ] || { bad "$tag 建单状态 = $st（期望 DRAFT）"; return 1; }
  api "${tag}_audit" POST "/business/erp/voucher/$vid/audit" "$T_FIN" '{}'
  [ "$CODE" = 0 ] || { bad "$tag 审核失败 http=$HTTP $(show)"; return 1; }
  st=$(PSQLQ -c "select status from biz_voucher where id='$vid';")
  [ "$st" = "AUDITED" ] || { bad "$tag 审核后状态 = $st（期望 AUDITED）"; return 1; }
  api "${tag}_post" POST "/business/erp/voucher/$vid/post" "$T_FIN" '{}'
  [ "$CODE" = 0 ] || { bad "$tag 记账失败 http=$HTTP $(show)"; return 1; }
  st=$(PSQLQ -c "select status from biz_voucher where id='$vid';")
  [ "$st" = "POSTED" ] || { bad "$tag 记账后状态 = $st（期望 POSTED）"; return 1; }
  printf '%s' "$vid"
}

# 单月 5 张凭证明细（金额口径见 docs/evidence/e2e04-finance/README.md）
mkm() { # mkm <yyyy-MM> <月序号 1..12>
  local p=$1 n=$2 d1="$1-05" d2="$1-10" d3="$1-20" d4="$1-25" d5="$1-28"
  local s="$3"
  mkpost "v$n""a" "$d1" "E2E-04 $s 销售确认收入" "[
    {\"account_id\":\"$ACC_1122\",\"debit\":\"113000\",\"summary\":\"应收货款\"},
    {\"account_id\":\"$ACC_5001\",\"credit\":\"100000\",\"summary\":\"主营业务收入\"},
    {\"account_id\":\"$ACC_222102\",\"credit\":\"13000\",\"summary\":\"销项税额\"}]" >/dev/null
  mkpost "v$n""b" "$d2" "E2E-04 $s 采购入库" "[
    {\"account_id\":\"$ACC_1405\",\"debit\":\"60000\",\"summary\":\"库存商品\"},
    {\"account_id\":\"$ACC_222101\",\"debit\":\"7800\",\"summary\":\"进项税额\"},
    {\"account_id\":\"$ACC_2202\",\"credit\":\"67800\",\"summary\":\"应付货款\"}]" >/dev/null
  mkpost "v$n""c" "$d3" "E2E-04 $s 结转销售成本" "[
    {\"account_id\":\"$ACC_6401\",\"debit\":\"60000\",\"summary\":\"主营业务成本\"},
    {\"account_id\":\"$ACC_1405\",\"credit\":\"60000\",\"summary\":\"库存商品\"}]" >/dev/null
  mkpost "v$n""d" "$d4" "E2E-04 $s 支付办公费用" "[
    {\"account_id\":\"$ACC_560203\",\"debit\":\"20000\",\"summary\":\"管理费用-办公费\"},
    {\"account_id\":\"$ACC_1002\",\"credit\":\"20000\",\"summary\":\"银行存款\"}]" >/dev/null
  mkpost "v$n""e" "$d5" "E2E-04 $s 收到货款" "[
    {\"account_id\":\"$ACC_1002\",\"debit\":\"113000\",\"summary\":\"银行存款\"},
    {\"account_id\":\"$ACC_1122\",\"credit\":\"113000\",\"summary\":\"应收账款\"}]" >/dev/null
}

# 全库凭证借贷平衡（返回不平张数）
vch_unbalanced() {
  PSQLQ -c "select count(*) from biz_voucher v where v.deleted_at is null and v.status='POSTED'
             and coalesce((select sum(debit_amount) from biz_voucher_entry e where e.voucher_id=v.id),0)
              <> coalesce((select sum(credit_amount) from biz_voucher_entry e where e.voucher_id=v.id),0);"
}
# 某科目某区间净额（借-贷，POSTED）
acc_net() { # acc_net <account_code> <from> <to>
  PSQLQ -c "select coalesce(sum(e.debit_amount-e.credit_amount),0)::numeric(20,2)::text
              from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id
             where v.deleted_at is null and v.status='POSTED' and e.account_code='$1'
               and v.voucher_date between '$2' and '$3';"
}
# 某科目全期间净额（借-贷，POSTED；红冲跨期间口径核对用）
acc_net_all() { # acc_net_all <account_code>
  PSQLQ -c "select coalesce(sum(e.debit_amount-e.credit_amount),0)::numeric(20,2)::text
              from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id
             where v.deleted_at is null and v.status='POSTED' and e.account_code='$1';"
}
# 某月分录借方合计（余额表「本期发生额」交叉核对用）
month_debit() { # month_debit <from> <to>
  PSQLQ -c "select coalesce(sum(e.debit_amount),0)::numeric(20,2)::text
              from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id
             where v.deleted_at is null and v.status='POSTED' and v.voucher_date between '$1' and '$2';"
}
period_id() { PSQLQ -c "select id from biz_accounting_period where period_code='$1';"; }

# 审批实例：用给定 token 逐节点审批直到没有待办（凭证红冲审批等）
approve_instance() { # approve_instance <token> <instance_id> <tag> [comment]
  local t=$1 iid=$2 tag=$3 comment=${4:-"E2E-04 财务演练审批通过"} n=0 tid node
  while [ $n -lt 8 ]; do
    api "${tag}_todo$n" GET "/business/flow/task/todo?page=1&page_size=200" "$t"
    if [ "$CODE" != 0 ]; then say "  ⚠ $tag 待办查询 code=$CODE $(show)"; return 1; fi
    tid=$(data ".data.list[]? | select(.instance_id==\"$iid\") | .id" | head -1)
    [ -z "$tid" ] && return 0
    node=$(data ".data.list[]? | select(.id==\"$tid\") | .current_node_name")
    api "${tag}_ap$n" POST "/business/flow/task/$tid/approve" "$t" "{\"comment\":\"$comment\"}"
    if [ "$CODE" = 0 ]; then ok "审批通过节点 [$node]"; else bad "审批节点 [$node] 失败 http=$HTTP $(show)"; return 1; fi
    n=$((n+1))
  done
  return 0
}

# 依次用财务经理 / admin 试审批（流程节点角色不同）
approve_as_any() { # approve_as_any <instance_id> <tag>
  local iid=$1 tag=$2
  api "${tag}_ftodo" GET "/business/flow/task/todo?page=1&page_size=200" "$T_FIN"
  if [ -n "$(data ".data.list[]? | select(.instance_id==\"$iid\") | .id" | head -1)" ]; then
    approve_instance "$T_FIN" "$iid" "${tag}f"; return $?
  fi
  approve_instance "$T_ADMIN" "$iid" "${tag}a"
}
flow_state() { PSQLQ -c "select status from biz_flow_instance where id='$1';"; }
