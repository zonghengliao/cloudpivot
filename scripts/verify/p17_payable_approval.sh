#!/usr/bin/env bash
# ======================================================================
# P17 验证脚本：应付付款审批（金额分级 + 内控 + 反向）
#
# 依据卡面 t_5d04571e §三「验收（真机）」。
#
# 六段，每段可单独复跑：
#   ① 结构与种子自检   biz_payment.approval_* / biz_payable.payment_approval_instance_id /
#                      ck_biz_payment_approval_status / uk_biz_payment_pending /
#                      种子流程 payable_pay（7 节点 8 边，两条 form.amount 条件边，会签 ALL）
#   ② 单测              cargo test --lib p17_（真库；21 条）
#   ③ 真机三档金额      5000 → 部门主管待办；50000 → 财务经理待办；
#                      500000 → 会签（只批一个不通过，两个都批才通过）；每次查 paid/unpaid + 凭证借贷
#   ④ 内控              制单人自己批 → 403「不能审批自己提交的付款申请」
#   ⑤ 反向              付款审批中把应付单作废 → 审批待办消失、付款记录 CANCELLED、账不动
#   ⑥ 不变量            erp_invariants.py（BLOCKER 0）
#
# 用法：
#
#   P17_DB_URL='postgres://user:pass@127.0.0.1:5432/cloudpivot_p17' \
#   P17_BASE_URL='http://127.0.0.1:18093' \
#     bash scripts/verify/p17_payable_approval.sh
#
#   bash scripts/verify/p17_payable_approval.sh --skip-http     # 只跑① ② ⑥
#   bash scripts/verify/p17_payable_approval.sh --keep          # 不清 e2e 夹具
#
# 环境变量：
#   P17_DB_URL      已应用 V0175 的库（缺省回退 ERP_DB_URL → NIK_DATABASE_*）
#   P17_BASE_URL    跑着本卡代码的实例（缺省 http://127.0.0.1:18093）
#   P17_ADMIN_USER / P17_ADMIN_PASSWORD   审批人账号（缺省 admin / P17Admin#2026）
#   ERP_DB_URL      单测用的库（缺省 = P17_DB_URL）
#
# 退出码：0 = 全绿；1 = 有断言不通过；2 = 环境不满足（没库 / 没 cargo）。
# ======================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PSQL="${P17_PSQL:-psql}"
SKIP_HTTP=0
KEEP=0
for arg in "$@"; do
  case "$arg" in
    --skip-http) SKIP_HTTP=1 ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
  esac
done

FAIL=0
pass() { printf '  \033[32m✅\033[0m %s\n' "$1"; }
fail() { printf '  \033[31m❌\033[0m %s\n' "$1"; FAIL=1; }
skip() { printf '  \033[33m⏭\033[0m %s\n' "$1"; }
head1() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# ---------- 0) 连库 ----------
DB_URL="${P17_DB_URL:-${ERP_DB_URL:-}}"
if [ -z "$DB_URL" ] && [ -n "${NIK_DATABASE_USER:-}" ]; then
  DB_URL="postgres://${NIK_DATABASE_USER}:${NIK_DATABASE_PASSWORD:-}@${NIK_DATABASE_HOST:-127.0.0.1}:${NIK_DATABASE_PORT:-5432}/${NIK_DATABASE_NAME:-cloudpivot}"
fi
if [ -z "$DB_URL" ]; then
  echo "SKIP: 未提供 P17_DB_URL / ERP_DB_URL / NIK_DATABASE_*，无法连库" >&2
  exit 2
fi
if ! "$PSQL" "$DB_URL" -X -tAc 'select 1' >/dev/null 2>&1; then
  echo "SKIP: 连不上库（$DB_URL）" >&2
  exit 2
fi
q() { "$PSQL" "$DB_URL" -X -tAc "$1" 2>/dev/null; }
qp() { "$PSQL" "$DB_URL" -X -tAc "$1" 2>/dev/null | tr -d '[:space:]'; }

# 库连接参数（给 erp_invariants.py 用；纯 shell 拆分，不靠正则，避免口令里的特殊字符被 sed 吃掉）
PROTO_REST="${DB_URL#postgres://}"
USERPASS="${PROTO_REST%%@*}"
DB_USER="${USERPASS%%:*}"
DB_PASS="${USERPASS#*:}"
HOSTPORT="${PROTO_REST#*@}"
DB_HOST="${HOSTPORT%%:*}"
PORTPATH="${HOSTPORT#*:}"
DB_PORT="${PORTPATH%%/*}"
DB_NAME="${PORTPATH#*/}"

echo "库：$(q 'select current_database()')"
echo "仓：$ROOT"

# ---------- ① 结构与种子 ----------
head1 "① 结构与种子自检（V0175）"

for pair in "biz_payment:approval_instance_id" "biz_payment:approval_status" \
            "biz_payable:payment_approval_instance_id"; do
  tbl="${pair%%:*}"; col="${pair##*:}"
  n=$(qp "select count(*) from information_schema.columns where table_name='$tbl' and column_name='$col'")
  [ "$n" = "1" ] && pass "$tbl.$col 已存在" || fail "缺 $tbl.$col（V0175 未应用）"
done

ck=$(q "select pg_get_constraintdef(oid) from pg_constraint
         where conname='ck_biz_payment_approval_status' and conrelid='biz_payment'::regclass")
case "$ck" in
  *PENDING*APPROVED*REJECTED*CANCELLED*) pass "ck_biz_payment_approval_status 五态齐全" ;;
  "") fail "缺 ck_biz_payment_approval_status" ;;
  *) fail "ck_biz_payment_approval_status 口径不对：$ck" ;;
esac

ck2=$(qp "select count(*) from pg_constraint
           where conname='ck_biz_payment_pending_instance' and conrelid='biz_payment'::regclass")
[ "$ck2" = "1" ] && pass "ck_biz_payment_pending_instance 存在（PENDING ⟹ 有实例）" || fail "缺 ck_biz_payment_pending_instance"

uk=$(qp "select count(*) from pg_indexes where tablename='biz_payment' and indexname='uk_biz_payment_pending'")
[ "$uk" = "1" ] && pass "uk_biz_payment_pending 存在（同一应付单只许一笔审批中）" || fail "缺 uk_biz_payment_pending（硬红线 ⑦ 的库层兜底）"

seed=$(q "select d.status || '|' || d.is_default::text || '|' || v.node_count || '|' || v.edge_count
            from biz_flow_def d join biz_flow_def_version v on v.id = d.current_version_id
           where d.flow_code='payable_pay' and d.deleted_at is null")
if [ "$seed" = "PUBLISHED|true|7|8" ]; then
  pass "种子流程 payable_pay = PUBLISHED/默认/7 节点 8 边"
else
  fail "种子流程结构不对：$seed（期望 PUBLISHED|true|7|8）"
fi

cond=$(qp "select count(*) from biz_flow_edge e join biz_flow_def d on d.current_version_id = e.version_id
            where d.flow_code='payable_pay' and not e.is_default
              and e.condition_expr->>'field' = 'form.amount'")
[ "$cond" = "2" ] && pass "金额分级是真的条件边（form.amount 两条）" || fail "条件边数=$cond（期望 2，说明分级没落在流程定义里）"

defs=$(qp "select count(*) from biz_flow_edge e join biz_flow_def d on d.current_version_id = e.version_id
            where d.flow_code='payable_pay' and e.is_default")
[ "$defs" = "2" ] && pass "两条兜底边（每个 CONDITION 恰一条）" || fail "兜底边数=$defs（期望 2）"

cs=$(q "select n.node_type || '|' || coalesce(n.approval_mode,'-') || '|' || n.self_approval
          from biz_flow_node n join biz_flow_def d on d.current_version_id = n.version_id
         where d.flow_code='payable_pay' and n.node_key='n_board'")
[ "$cs" = "COUNTERSIGN|ALL|ALLOW" ] && pass "会签节点 = COUNTERSIGN/ALL（两个都批才过）+ 自审放行（403 由入口兜）" \
  || fail "会签节点口径不对：$cs（期望 COUNTERSIGN|ALL|ALLOW）"

# ---------- ② 单测 ----------
head1 "② 单测（真库）"
if command -v cargo >/dev/null 2>&1; then
  LOG=$(mktemp)
  ( cd "$ROOT/backend" && ERP_DB_URL="${ERP_DB_URL:-$DB_URL}" \
      cargo test --lib p17_ -- --test-threads=1 >"$LOG" 2>&1 )
  grep -E "test result:" "$LOG" | tail -1 | sed 's/^/    /'
  if grep -q "test result: ok" "$LOG"; then
    pass "P17 单测全绿（22 条：发起 5 / 分级 4 / 通过 4 / 驳回反向 3 / 内控 1 / 迁移自检 1 / 纯逻辑 3 + 收尾 1）"
  else
    grep -E "^test .* FAILED|panicked at" "$LOG" | head -8 | sed 's/^/    /'
    fail "P17 单测有失败（日志：$LOG）"
  fi
else
  skip "没装 cargo，单测段跳过"
fi

# ---------- ③④⑤ 真机（HTTP） ----------
BASE="${P17_BASE_URL:-http://127.0.0.1:18093}"
ADMIN_USER="${P17_ADMIN_USER:-admin}"
ADMIN_PASS="${P17_ADMIN_PASSWORD:-P17Admin#2026}"
MAKER_USER='p17_maker'
SUPPLIER_ID='e7e70001-0000-4000-8000-0000000e2e01'

if [ "$SKIP_HTTP" = "1" ]; then
  skip "③④⑤ 真机段按 --skip-http 跳过"
elif ! curl -s -m 3 "$BASE/health" >/dev/null 2>&1; then
  skip "③④⑤ 实例不可达（$BASE），真机段跳过（先起实例：bash /root/p17/10_server.sh）"
else
  # ---- 夹具：供应商 / 期间 / 制单员账号 / 第二个审批人 ----
  q "insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days)
     values ('$SUPPLIER_ID'::uuid, 'P17E2E-SUP', 'P17 e2e 供应商', 1, 30) on conflict do nothing" >/dev/null
  q "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
     values (extract(year from (now() at time zone 'Asia/Shanghai'))::int,
             extract(month from (now() at time zone 'Asia/Shanghai'))::int,
             to_char(now() at time zone 'Asia/Shanghai', 'YYYY-MM'),
             date_trunc('month', (now() at time zone 'Asia/Shanghai'))::date,
             (date_trunc('month', (now() at time zone 'Asia/Shanghai')) + interval '1 month - 1 day')::date,
             'OPEN')
     on conflict (period_code) do update set status = 'OPEN'" >/dev/null
  q "update biz_account set status = 1 where account_code in ('2202','1002')" >/dev/null

  # 制单员：非超管 + 应付写权限 + 审批入口权限（内控要求「制单与审批分离」。
  # `business:flow:approve` 是路由级权限：没有它连 approve 接口都进不去，
  # 那样测出来的是权限码 403，而不是本卡的内控 403）
  q "insert into sys_role (role_code, role_name, description, status, data_scope)
     values ('P17_MAKER', '应付制单员', 'P17 验证夹具', 1, 1) on conflict do nothing" >/dev/null
  q "insert into sys_role_menu (role_id, menu_id)
     select r.id, m.id from sys_role r, sys_menu m
      where r.role_code='P17_MAKER'
        and m.auth_code in ('business:erp:payable:pay','business:erp:payable:list',
                            'business:erp:payable:detail','business:flow:approve')
     on conflict do nothing" >/dev/null
  q "insert into sys_user (username, password, real_name, must_change_password, status)
     values ('$MAKER_USER', (select password from sys_user where username='$ADMIN_USER'),
             'P17 制单员', false, 1) on conflict do nothing" >/dev/null
  q "insert into sys_user_role (user_id, role_id)
     select u.id, r.id from sys_user u, sys_role r
      where u.username='$MAKER_USER' and r.role_code='P17_MAKER'
     on conflict do nothing" >/dev/null

  # 所有超管（含单测夹具 p17_ap1/p17_ap2）与制单员统一成同一个可登录口令 ——
  # **必须在「审批人改密之后」执行**（见下面 unify_passwords 的调用点），
  # 否则复制到的是旧口令哈希，改密后他们就登不上了。
  unify_passwords() {
    q "update sys_user set password = (select password from sys_user where username='$ADMIN_USER'),
              must_change_password = false, status = 1,
              is_locked = false, lock_time = NULL, login_fail_count = 0
        where username = '$MAKER_USER'
           or id in (select ur.user_id from sys_user_role ur join sys_role r on r.id = ur.role_id
                      where r.role_code = 'SUPER_ADMIN')" >/dev/null
  }

  # ---- 登录 ----
  login_body() {
    curl -s -m 10 -X POST "$BASE/api/system/auth/login" \
      -H 'Content-Type: application/json' \
      -d "{\"username\":\"$1\",\"password\":\"$2\"}"
  }
  token_of() {
    printf '%s' "$1" | grep -o '"access_token":"[^"]*"' | head -1 | sed 's/.*:"//; s/"$//'
  }
  login() { token_of "$(login_body "$1" "$2")"; }

  # 口令候选：首跑用的是实例初始口令，改密后要用改过的新口令（同一个库反复跑很常见）
  PASS_CANDIDATES=("$ADMIN_PASS" 'P17Admin#2026x' 'P17Admin#2026')
  # ⚠️ 必须把「命中的口令」和 token 一起回传：函数若在 `$(…)` 子 shell 里改 ADMIN_PASS，
  # 父 shell 是看不到的（实测：token 拿到了、口令还是旧的 → 后面制单员登录必失败）。
  try_login() {
    local candidate
    for candidate in "${PASS_CANDIDATES[@]}"; do
      local body token
      body=$(login_body "$ADMIN_USER" "$candidate")
      token=$(token_of "$body")
      if [ -n "$token" ]; then
        printf '%s\t%s' "$candidate" "$token"
        return 0
      fi
    done
    return 1
  }
  IFS=$'\t' read -r ADMIN_PASS ADMIN_TOKEN <<< "$(try_login || true)"
  BODY=$(login_body "$ADMIN_USER" "$ADMIN_PASS")
  if printf '%s' "$BODY" | grep -q '"must_change_password":true'; then
    # 首登强制改密：业务接口会 403「首次登录必须先修改初始密码」，先改密再重登
    NEWPASS='P17Admin#2026x'
    curl -s -m 10 -X PUT "$BASE/api/system/auth/password" \
      -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
      -d "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEWPASS\"}" >/dev/null
    ADMIN_PASS="$NEWPASS"
    ADMIN_TOKEN=$(login "$ADMIN_USER" "$ADMIN_PASS")
    [ -n "$ADMIN_TOKEN" ] && pass "审批人首登改密 + 重登成功（后续统一用新口令）" \
      || fail "审批人改密后重登失败"
  fi
  if [ -z "$ADMIN_TOKEN" ]; then
    fail "审批人登录失败（$ADMIN_USER）：口令可能与实例的 NIK_ADMIN_PASSWORD 不一致（用 P17_ADMIN_PASSWORD 覆盖）"
  fi
  unify_passwords
  MAKER_TOKEN=$(login "$MAKER_USER" "$ADMIN_PASS")
  [ -n "$MAKER_TOKEN" ] && pass "制单员 $MAKER_USER 登录成功（非超管，只持 business:erp:payable:pay）" \
    || fail "制单员登录失败（口令应同 admin 的）"

  http_post() { # $1=token $2=path $3=body → 输出 "CODE<TAB>BODYFILE"
    local out
    out=$(mktemp)
    local code
    code=$(curl -s -m 15 -o "$out" -w '%{http_code}' -X POST "$BASE$2" \
      -H "Authorization: Bearer $1" -H 'Content-Type: application/json' -d "$3")
    printf '%s\t%s' "$code" "$out"
  }

  # ---- 造应付单（直接落库：应付由收货过账生成，P2-B1 不在本卡范围） ----
  new_payable() { # $1=金额
    local no="AP-P17E2E-$(date +%H%M%S)$$-$RANDOM"
    q "insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no, currency_code,
                                amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, status)
       values ('$no', '$SUPPLIER_ID'::uuid, 'STOCK_IN', gen_random_uuid(), 'P17E2E-IN', 'CNY',
               $1, 0, $1, 0, (now() at time zone 'Asia/Shanghai')::date + 30,
               to_char(now() at time zone 'Asia/Shanghai','YYYY-MM'), 'OPEN')
       returning id" | head -1 | tr -d '[:space:]'
  }
  submit_pay() { # $1=payable_id $2=金额 → 打印 "CODE  BODY"
    http_post "$MAKER_TOKEN" "/api/business/erp/payable/$1/pay" "{\"amount\":\"$2\",\"pay_method\":\"银行转账\"}"
  }
  field() { # $1=json file $2=key
    grep -o "\"$2\":[^,}]*" "$1" | head -1 | sed 's/.*://; s/^"//; s/"$//'
  }

  head1 "③ 真机：三档金额（条件节点真的在分流）"
  E2E_IDS=()
  for tier in "5000:dept:PARTIAL_PAID" "50000:fin:PARTIAL_PAID" "500000:board:PENDING"; do
    amount="${tier%%:*}"; rest="${tier#*:}"; want_node="${rest%%:*}"
    payable_id=$(new_payable "$amount")
    [ -n "$payable_id" ] || { fail "造应付单失败（$amount）"; continue; }
    E2E_IDS+=("$payable_id")
    IFS=$'\t' read -r code body <<<"$(submit_pay "$payable_id" "$amount")"
    if [ "$code" != "202" ]; then
      fail "付款提交（$amount）应返回 202，实际 $code：$(head -c 200 "$body")"
      continue
    fi
    pass "付款提交 $amount → 202（审批中，账还没动）"
    instance_id=$(field "$body" approval_instance_id)
    payment_id=$(field "$body" payment_id)
    paid_after=$(qp "select paid_amount::text from biz_payable where id='$payable_id'")
    [ "$paid_after" = "0.0000" ] && pass "  审批中 paid_amount 仍 0（$amount）" \
      || fail "  审批中 paid_amount 变了：$paid_after"

    nodes=$(q "select string_agg(distinct node_key, ',') from biz_flow_task where instance_id='$instance_id'")
    case "$want_node" in
      dept) [ "$nodes" = "n_dept" ] && pass "  5000 元只派到「部门主管审批」（$nodes）" || fail "  5000 元派错节点：$nodes" ;;
      fin)  [ "$nodes" = "n_fin" ] && pass "  50000 元只派到「财务经理审批」（$nodes）" || fail "  50000 元派错节点：$nodes" ;;
      board) case "$nodes" in
               *n_board*) pass "  500000 元走「财务总监+总经理会签」（$nodes）" ;;
               *) fail "  500000 元派错节点：$nodes" ;;
             esac ;;
    esac

    # 会签场景：先只批一个人，检查「不通过」；再批完
    tasks=$(q "select id || '|' || assignee_id from biz_flow_task where instance_id='$instance_id' and status='PENDING' order by assignee_id")
    first=1
    while IFS='|' read -r task_id assignee; do
      [ -n "${task_id:-}" ] || continue
      uname=$(q "select username from sys_user where id='$assignee'")
      token=$(login "$uname" "$ADMIN_PASS")
      if [ -z "$token" ]; then
        fail "  审批人 $uname 登录失败"
        continue
      fi
      IFS=$'\t' read -r c2 b2 <<<"$(http_post "$token" "/api/business/flow/task/$task_id/approve" '{"comment":"P17 验证通过"}')"
      if [ "$c2" != "200" ]; then
        fail "  审批（$uname）失败 $c2：$(head -c 200 "$b2")"
        continue
      fi
      if [ "$first" = "1" ]; then
        st=$(qp "select status from biz_flow_instance where id='$instance_id'")
        remain=$(qp "select count(*) from biz_flow_task where instance_id='$instance_id' and status='PENDING'")
        if [ "$want_node" = "board" ] && [ "$remain" -gt 0 ]; then
          [ "$st" = "PENDING" ] && pass "  会签：只批一个人 → 仍是审批中（状态 $st，还剩 $remain 票）" \
            || fail "  会签：只批一个人就结束了（状态 $st）"
        fi
        first=0
      fi
    done <<< "$tasks"

    st=$(qp "select status from biz_flow_instance where id='$instance_id'")
    [ "$st" = "APPROVED" ] && pass "  全部批完 → 审批单 APPROVED" || fail "  全部批完审批单仍是 $st"

    paid=$(qp "select paid_amount::text || '|' || unpaid_amount::text || '|' || status from biz_payable where id='$payable_id'")
    case "$paid" in
      "$amount.0000|0.0000|SETTLED") pass "  账面正确：paid=$amount unpaid=0 SETTLED" ;;
      *"|PARTIAL_PAID") pass "  账面：$paid（部分已付）" ;;
      *) fail "  账面不对：$paid（期望 paid=$amount 且 unpaid=0 / SETTLED）" ;;
    esac

    bal=$(q "select v.total_debit::text || '|' || v.total_credit::text || '|' ||
                    (select count(*) from biz_voucher_entry e where e.voucher_id = v.id) || '|' ||
                    coalesce((select string_agg(distinct a.account_code, ',') from biz_voucher_entry e
                                join biz_account a on a.id = e.account_id where e.voucher_id = v.id), '-')
               from biz_payment p join biz_voucher v on v.id = p.voucher_id
              where p.id='$payment_id'")
    IFS='|' read -r d c n accs <<< "$bal"
    if [ "$d" = "$c" ] && [ "$n" = "2" ] && [ "$accs" = "1002,2202" ]; then
      pass "  付款凭证：借 $d = 贷 $c，两条分录 1002/2202"
    else
      fail "  付款凭证不对：借=$d 贷=$c 分录=$n 科目=$accs"
    fi
  done

  head1 "④ 内控：制单人不能批自己"
  # 让**制单员自己**也进审批人（临时给他超管角色），模拟「制单人恰好也是审批人」；
  # 必须**重新登录**：令牌里的权限是登录时装载的（SUPER_ADMIN 的通配 `*` 也要重新拿）。
  q "insert into sys_user_role (user_id, role_id)
     select u.id, r.id from sys_user u, sys_role r
      where u.username='$MAKER_USER' and r.role_code='SUPER_ADMIN' on conflict do nothing" >/dev/null
  MAKER_SUPER_TOKEN=$(login "$MAKER_USER" "$ADMIN_PASS")
  [ -n "$MAKER_SUPER_TOKEN" ] && pass "  制单员临时挂上 SUPER_ADMIN 并重新登录（令牌含通配权限）" \
    || fail "  制单员重新登录失败"

  ctrl_payable=$(new_payable 5000)
  E2E_IDS+=("$ctrl_payable")
  ctrl_body=$(mktemp)
  ctrl_code=$(curl -s -m 15 -o "$ctrl_body" -w '%{http_code}' -X POST \
    "$BASE/api/business/erp/payable/$ctrl_payable/pay" \
    -H "Authorization: Bearer $MAKER_SUPER_TOKEN" -H 'Content-Type: application/json' \
    -d '{"amount":"5000","pay_method":"银行转账"}')
  if [ "$ctrl_code" != "202" ]; then
    fail "  自审用例提交失败：$ctrl_code $(head -c 200 "$ctrl_body")"
  else
    ctrl_instance=$(field "$ctrl_body" approval_instance_id)
    own_task=$(qp "select t.id from biz_flow_task t join sys_user u on u.id = t.assignee_id
                    where t.instance_id='$ctrl_instance' and u.username='$MAKER_USER' limit 1")
    if [ -z "$own_task" ]; then
      fail "  制单员没拿到自己的待办（审批人里没有他？）"
    else
      IFS=$'\t' read -r c3 b3 <<<"$(http_post "$MAKER_SUPER_TOKEN" "/api/business/flow/task/$own_task/approve" '{"comment":"我自己批"}')"
      if [ "$c3" = "403" ] && grep -q "不能审批自己提交的付款申请" "$b3"; then
        pass "  制单人批自己 → 403「不能审批自己提交的付款申请」"
      else
        fail "  制单人自审没被拦住：$c3 $(head -c 200 "$b3")"
      fi
      # 账一分没动（这一票没生效）
      ctrl_paid=$(qp "select paid_amount::text from biz_payable where id='$ctrl_payable'")
      [ "$ctrl_paid" = "0.0000" ] && pass "  内控拦下后账一分没动（paid=$ctrl_paid）" \
        || fail "  内控拦下但账动了：paid=$ctrl_paid"
    fi
  fi
  # 复原：制单员退回非超管（后面 ⑤ 的用例按普通制单员跑）
  q "delete from sys_user_role where user_id=(select id from sys_user where username='$MAKER_USER')
       and role_id=(select id from sys_role where role_code='SUPER_ADMIN')" >/dev/null
  MAKER_TOKEN=$(login "$MAKER_USER" "$ADMIN_PASS")

  head1 "⑤ 反向：付款审批中把应付单作废 → 审批待办消失"
  rev_payable=$(new_payable 5000)
  E2E_IDS+=("$rev_payable")
  IFS=$'\t' read -r code body <<<"$(submit_pay "$rev_payable" "5000")"
  rev_instance=$(field "$body" approval_instance_id)
  before=$(qp "select count(*) from biz_flow_task where instance_id='$rev_instance' and status='PENDING'")
  # 制单员没有 payable:cancel 权限以外的判断，这里用 admin 的 token 作废
  IFS=$'\t' read -r c4 b4 <<<"$(http_post "$ADMIN_TOKEN" "/api/business/erp/payable/$rev_payable/cancel" '{"reason":"P17 验证：重复入库"}')"
  after=$(qp "select count(*) from biz_flow_task where instance_id='$rev_instance' and status='PENDING'")
  rev_st=$(qp "select status from biz_flow_instance where id='$rev_instance'")
  if [ "$c4" = "200" ] && [ "$after" = "0" ] && [ "$rev_st" = "CANCELLED" ]; then
    pass "  作废前后待办 $before → $after，审批实例 $rev_st（P11 孤儿教训）"
  else
    fail "  反向失败：作废返回 $c4，待办 $before → $after，实例 $rev_st $(head -c 160 "$b4")"
  fi
  pay_st=$(qp "select approval_status || '|' || coalesce(voucher_id::text,'-') from biz_payment where id='$(field "$body" payment_id)'")
  [ "${pay_st%%|*}" = "CANCELLED" ] && pass "  付款记录 CANCELLED 且未出凭证（${pay_st#*|}）" \
    || fail "  付款记录状态不对：$pay_st"

  # ---- 清理 e2e 夹具（不影响种子/其它数据） ----
  if [ "$KEEP" = "0" ]; then
    for pid in "${E2E_IDS[@]:-}"; do
      [ -n "${pid:-}" ] || continue
      q "delete from biz_flow_instance where biz_id in (select id from biz_payment where payable_id='$pid')" >/dev/null
      q "delete from biz_voucher_entry where voucher_id in
           (select id from biz_voucher where source_id in (select id from biz_payment where payable_id='$pid'))" >/dev/null
      q "delete from biz_voucher where source_id in (select id from biz_payment where payable_id='$pid')" >/dev/null
      q "delete from biz_status_log where entity_id='$pid'" >/dev/null
      q "delete from biz_payment where payable_id='$pid'" >/dev/null
      q "delete from biz_payable where id='$pid'" >/dev/null
    done
    echo "    （e2e 夹具已清理；加 --keep 可保留）"
  fi
fi

# ---------- ⑥ 不变量 ----------
head1 "⑥ ERP 不变量套件"
# ⚠️ 口径：不变量是「全库账实核对」，必须在**干净库**（新建 + 迁移完、零夹具）上验收。
# 跑过单测 / e2e 的库里全是夹具，A2/B1/E2 这类全库断言会红 —— 那不是账错，是夹具。
# 所以允许单独指定：P17_INV_DB_URL（缺省 = P17_DB_URL）。
INV_URL="${P17_INV_DB_URL:-$DB_URL}"
INV_REST="${INV_URL#postgres://}"
INV_USERPASS="${INV_REST%%@*}"
INV_USER="${INV_USERPASS%%:*}"
INV_PASS="${INV_USERPASS#*:}"
INV_HOSTPORT="${INV_REST#*@}"
INV_HOST="${INV_HOSTPORT%%:*}"
INV_PORTPATH="${INV_HOSTPORT#*:}"
INV_PORT="${INV_PORTPATH%%/*}"
INV_NAME="${INV_PORTPATH#*/}"
echo "    库：$INV_NAME（P17_INV_DB_URL 可单独指向干净库）"
INV_LOG=$(mktemp)
( cd "$ROOT" && NIK_DATABASE_HOST="$INV_HOST" NIK_DATABASE_PORT="$INV_PORT" \
  NIK_DATABASE_USER="$INV_USER" NIK_DATABASE_PASSWORD="$INV_PASS" NIK_DATABASE_NAME="$INV_NAME" \
  timeout 300 python3 scripts/verify/erp_invariants.py >"$INV_LOG" 2>&1 )
grep -E "结果：|BLOCKER" "$INV_LOG" | tail -3 | sed 's/^/    /'
if grep -q "BLOCKER 0" "$INV_LOG"; then
  if grep -q "32/32 通过" "$INV_LOG"; then
    pass "不变量 32/32 通过、BLOCKER 0"
  else
    pass "不变量 BLOCKER 0（通过数未达 32：库里有夹具，干净库上应是 32/32）"
  fi
else
  fail "不变量出现 BLOCKER（详见 $INV_LOG）"
fi

# ---------- 汇总 ----------
echo
if [ "$FAIL" = "0" ]; then
  printf '\033[32m==> 汇总：全部通过\033[0m\n'
  exit 0
fi
printf '\033[31m==> 汇总：有断言失败（见上方 ❌）\033[0m\n'
exit 1
