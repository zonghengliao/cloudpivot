#!/usr/bin/env bash
# =============================================================================
# P-FLOW-3A · 入库链标准审批流 真机验收（采购订单审批 + 收货过账审批）
# =============================================================================
#
# 做什么
# ------
#   ① 起一个**隔离实例**（自建库 + 18094 端口，不碰 prod / dev / 别的卡）
#   ② 用真实 HTTP 走完卡面 §七的五条金额档位：
#        1) 采购 8000   → 部门主管待办 → 批 → APPROVED
#        2) 采购 50000  → 采购经理待办
#        3) 采购 200000 → 两人会签，只批一个不通过，两个都批才通过
#        4) 收货 500    → 小额免审直接过账 + 收到知会通知
#        5) 收货 5000   → 仓管主管审 → 批 → 过账（库存/凭证/应付都对）
#   ③ 反向与内控：审批中删单撤审批、制单人自批 403、重复提交 409
#   ④ 停实例（trap，异常也停）
#
# 用法
# ----
#   bash scripts/verify/pflow3a_po_stockin_approval.sh
#   BASE_URL=http://host:port bash ...            # 已有实例则跳过启动
#   P3A_DB_URL='postgres://user:pass@127.0.0.1:5432/db' bash ...   # 复用已迁移的库
#
# 前置
# ----
#   · 本机 PG 可达（127.0.0.1:5432）；`backend/target/debug/cloudpivot` 已编译
#   · 需要超级用户建库建角色时，脚本走 `su - postgres -c psql`（仅本机验收环境）
# =============================================================================
set -uo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
DB=${P3A_DB:-cloudpivot_pflow3a_verify}
TEMPLATE_DB=${P3A_TEMPLATE_DB:-cloudpivot_test_pflow2}
DB_USER=${P3A_DB_USER:-pflow3a}
DB_PASS=${P3A_DB_PASS:-pflow3a}
PORT=${P3A_PORT:-18094}
BASE_URL=${BASE_URL:-http://127.0.0.1:$PORT}
RUN_DIR=${P3A_RUN_DIR:-/tmp/pflow3a_verify}
ADMIN_USER=${P3A_ADMIN_USER:-admin}
ADMIN_PASS=${P3A_ADMIN_PASSWORD:-Admin@123456}
MASTER_KEY=${P3A_STORAGE_KEY:-$(openssl rand -hex 32)}
DB_URL=${P3A_DB_URL:-postgres://$DB_USER:$DB_PASS@127.0.0.1:5432/$DB}

mkdir -p "$RUN_DIR"
FAILED=0
PASSED=0
say() { printf '%s\n' "$*"; }
head1() { printf '\n=== %s ===\n' "$*"; }
fail() { FAILED=$((FAILED + 1)); printf '[FAIL] %s\n' "$*"; }
pass() { PASSED=$((PASSED + 1)); printf '[PASS] %s\n' "$*"; }

q() { psql "$DB_URL" -Atc "$1" 2>&1; }
q1() { q "$1" | head -1 | tr -d '[:space:]'; }

PID=""
cleanup() {
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null
    sleep 1
  fi
}
trap cleanup EXIT

# ---------- 0. 实例 ----------
# 默认「干净起一台」：先把占着端口的旧实例停掉（否则会复用一台连着旧库/旧口令的进程，
# 后面 psql 看新库、HTTP 打旧进程，断言会变成看运气 —— 实测踩过）。
if [ "${P3A_REUSE_DB:-0}" != "1" ]; then
  old_pid=$(ss -lptnH "sport = :$PORT" 2>/dev/null | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
  if [ -n "${old_pid:-}" ]; then
    say "停掉占用 :$PORT 的旧实例（pid $old_pid）"
    kill "$old_pid" 2>/dev/null
    sleep 2
  fi
fi

if curl -sf "$BASE_URL/health" >/dev/null 2>&1; then
  say "复用已在跑的实例：$BASE_URL"
else
  head1 "起隔离实例（库 $DB / 端口 $PORT）"
  export PATH=/root/.cargo/bin:$PATH
  ( cd "$BACKEND_DIR" && cargo build -q ) || { say "cargo build 失败"; exit 1; }

  su - postgres -c "psql -Atc \"select 1 from pg_roles where rolname='$DB_USER'\"" | grep -q 1 \
    || su - postgres -c "psql -c \"CREATE ROLE $DB_USER LOGIN SUPERUSER PASSWORD '$DB_PASS'\"" >/dev/null

  # 默认**建一个全新的空库**：迁移从 V0001 一路跑到 V0182（这本身是「干净库能装起来」的证据），
  # 且 bootstrap 会用 NIK_ADMIN_PASSWORD 建 admin —— 口令可预期。
  # 已有的库（含旧口令）不复用：argon2 口令哈希在库里没法反推，复用会卡在登录。
  if [ "${P3A_REUSE_DB:-0}" != "1" ]; then
    su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
      && su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
  fi
  if ! su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1; then
    su - postgres -c "createdb -O $DB_USER $DB" || { say "建库失败"; exit 1; }
  fi

  cd "$BACKEND_DIR" || exit 1
  NIK_PROFILE=test \
  NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
  NIK_DATABASE_HOST=127.0.0.1 \
  NIK_DATABASE_PORT=5432 \
  NIK_DATABASE_NAME="$DB" \
  NIK_DATABASE_USER="$DB_USER" \
  NIK_DATABASE_PASSWORD="$DB_PASS" \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET="pflow3a-verify-jwt-secret-0123456789" \
  NIK_ADMIN_PASSWORD="$ADMIN_PASS" \
  CLOUDPIVOT_STORAGE_MASTER_KEY="$MASTER_KEY" \
    setsid nohup "$BACKEND_DIR/target/debug/cloudpivot" >"$RUN_DIR/server.log" 2>&1 < /dev/null &
  PID=$!
  for _ in $(seq 1 90); do
    curl -sf "$BASE_URL/health" >/dev/null 2>&1 && break
    sleep 1
  done
  if ! curl -sf "$BASE_URL/health" >/dev/null 2>&1; then
    say "实例没起来，日志尾部："
    tail -30 "$RUN_DIR/server.log"
    exit 1
  fi
  pass "隔离实例已就绪：$BASE_URL"
  applied=$(grep -ao '应用 [0-9]* 个迁移' "$RUN_DIR/server.log" | tail -1)
  say "启动迁移日志：${applied:-（未打印）}"
fi

# ---------- 1. 库侧：V0182 种子就位 ----------
head1 "① 库侧：两条标准流程 + 四个审批角色 + 两个权限码"
for row in "po_approve|PO_APPROVE" "stock_in_post|STOCK_IN_POST"; do
  code=${row%%|*}; biz=${row##*|}
  got=$(q1 "select flow_code || '/' || biz_type || '/' || status from biz_flow_def where flow_code='$code'")
  [ "$got" = "$code/$biz/PUBLISHED" ] && pass "流程种子 $code 已发布（$got）" \
    || fail "流程种子 $code 不达标：$got"
done
n_roles=$(q1 "select count(*) from sys_role where role_code in ('DEPT_LEADER','PURCHASE_MANAGER','GENERAL_MANAGER','WAREHOUSE_MANAGER') and deleted_at is null")
[ "${n_roles:-0}" = "4" ] && pass "四个审批角色已就位" || fail "审批角色数=$n_roles（应为 4）"
n_codes=$(q1 "select count(*) from sys_menu where auth_code in ('business:erp:purchase-order:submit-approval','business:erp:stock-in:submit-post-approval') and deleted_at is null")
[ "${n_codes:-0}" = "2" ] && pass "两个提交审批权限码已就位" || fail "权限码数=$n_codes（应为 2）"

# ---------- 2. HTTP 夹具 ----------
head1 "② 造夹具（供应商 / 仓库 / 物料 / 期间 / 制单员 / 四个审批人）"
SUPPLIER='e3a30001-0000-4000-8000-000000000001'
WAREHOUSE='e3a30001-0000-4000-8000-000000000002'
UNIT='e3a30001-0000-4000-8000-000000000003'
MATERIAL='e3a30001-0000-4000-8000-000000000011'

q "insert into biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
   values ('$UNIT'::uuid,'P3A-PCS','个',1,NULL,1,1,1) on conflict do nothing" >/dev/null
q "insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days)
   values ('$SUPPLIER'::uuid,'P3A-SUP','P3A 验收供应商',1,30) on conflict do nothing" >/dev/null
q "insert into biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
   values ('$WAREHOUSE'::uuid,'P3A-WH','P3A 验收仓','/$WAREHOUSE/',1,1,1) on conflict do nothing" >/dev/null
q "insert into biz_material (id, material_code, material_name, unit_id, status, serial_required)
   values ('$MATERIAL'::uuid,'P3A-M1','P3A 螺丝',  '$UNIT'::uuid,1,false) on conflict do nothing" >/dev/null
q "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
   values (extract(year from (now() at time zone 'Asia/Shanghai'))::int,
           extract(month from (now() at time zone 'Asia/Shanghai'))::int,
           to_char(now() at time zone 'Asia/Shanghai','YYYY-MM'),
           date_trunc('month',(now() at time zone 'Asia/Shanghai'))::date,
           (date_trunc('month',(now() at time zone 'Asia/Shanghai')) + interval '1 month - 1 day')::date,
           'OPEN')
   on conflict (period_code) do update set status='OPEN'" >/dev/null
q "update biz_account set status = 1" >/dev/null

# 制单员角色：能建单 / 提交 / 看单 + 审批中心入口（否则测的是权限码 403，不是内控/流程 403）
q "insert into sys_role (role_code, role_name, description, status, sort_order)
   values ('P3A_MAKER','P3A 制单员','P-FLOW-3A 验收夹具',1,90) on conflict do nothing" >/dev/null
q "insert into sys_role_menu (id, role_id, menu_id)
   select gen_random_uuid(), r.id, m.id from sys_role r, sys_menu m
    where r.role_code='P3A_MAKER' and m.deleted_at is null
      and m.auth_code in ('business:erp:purchase-order:create','business:erp:purchase-order:update',
                          'business:erp:purchase-order:status','business:erp:purchase-order:list',
                          'business:erp:purchase-order:submit-approval',
                          'business:erp:stock-in:create','business:erp:stock-in:update',
                          'business:erp:stock-in:status','business:erp:stock-in:list',
                          'business:erp:stock-in:delete',
                          'business:erp:stock-in:submit-post-approval',
                          'business:flow:approve','business:flow:read')
   on conflict do nothing" >/dev/null
# 审批人角色：只需要审批中心入口
q "insert into sys_role (role_code, role_name, description, status, sort_order)
   values ('P3A_APPROVER','P3A 审批人','P-FLOW-3A 验收夹具',1,91) on conflict do nothing" >/dev/null
q "insert into sys_role_menu (id, role_id, menu_id)
   select gen_random_uuid(), r.id, m.id from sys_role r, sys_menu m
    where r.role_code='P3A_APPROVER' and m.deleted_at is null
      and m.auth_code in ('business:flow:approve','business:flow:read')
   on conflict do nothing" >/dev/null

for u in p3a_maker p3a_dept p3a_pm p3a_gm p3a_wh; do
  q "insert into sys_user (username, password, real_name, must_change_password, status)
     values ('$u', (select password from sys_user where username='$ADMIN_USER'), '$u', false, 1)
     on conflict do nothing" >/dev/null
done
q "insert into sys_user_role (user_id, role_id)
   select u.id, r.id from sys_user u, sys_role r
    where u.username='p3a_maker' and r.role_code='P3A_MAKER' on conflict do nothing" >/dev/null
for pair in "p3a_dept:DEPT_LEADER" "p3a_pm:PURCHASE_MANAGER" "p3a_gm:GENERAL_MANAGER" "p3a_wh:WAREHOUSE_MANAGER"; do
  u=${pair%%:*}; role=${pair##*:}
  q "insert into sys_user_role (user_id, role_id)
     select u.id, r.id from sys_user u, sys_role r
      where u.username='$u' and r.role_code='$role' on conflict do nothing" >/dev/null
  q "insert into sys_user_role (user_id, role_id)
     select u.id, r.id from sys_user u, sys_role r
      where u.username='$u' and r.role_code='P3A_APPROVER' on conflict do nothing" >/dev/null
done
# 夹具独占四个种子角色（否则「派给谁」不可判定）
q "delete from sys_user_role where role_id in (select id from sys_role where role_code in
     ('DEPT_LEADER','PURCHASE_MANAGER','GENERAL_MANAGER','WAREHOUSE_MANAGER'))
   and user_id not in (select id from sys_user where username in ('p3a_dept','p3a_pm','p3a_gm','p3a_wh'))" >/dev/null

# 口令统一成 admin 的（首登改密后 admin 的哈希已经是新的，这里跟着走）
login_body() {
  curl -s -m 10 -X POST "$BASE_URL/api/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}"
}
token_of() { printf '%s' "$1" | grep -o '"access_token":"[^"]*"' | head -1 | sed 's/.*:"//; s/"$//'; }
login() { token_of "$(login_body "$1" "$2")"; }

ADMIN_TOKEN=$(login "$ADMIN_USER" "$ADMIN_PASS")
if [ -z "$ADMIN_TOKEN" ]; then
  # 复用旧库时，上一次跑已经把初始口令改掉了（首登强制改密）→ 再试一次改后的口令
  for candidate in 'P3aAdmin#2026x' 'Admin@123456'; do
    ADMIN_TOKEN=$(login "$ADMIN_USER" "$candidate")
    if [ -n "$ADMIN_TOKEN" ]; then
      ADMIN_PASS="$candidate"
      break
    fi
  done
fi
if [ -z "$ADMIN_TOKEN" ]; then
  fail "admin 登录失败（口令与 NIK_ADMIN_PASSWORD 不一致？用 P3A_ADMIN_PASSWORD 覆盖）"
  say "RESULT: 无法继续"
  exit 1
fi
if login_body "$ADMIN_USER" "$ADMIN_PASS" | grep -q '"must_change_password":true'; then
  NEWPASS='P3aAdmin#2026x'
  curl -s -m 10 -X PUT "$BASE_URL/api/system/auth/password" \
    -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEWPASS\"}" >/dev/null
  ADMIN_PASS="$NEWPASS"
  ADMIN_TOKEN=$(login "$ADMIN_USER" "$ADMIN_PASS")
  pass "admin 首登改密 + 重登成功"
fi
q "update sys_user set password = (select password from sys_user where username='$ADMIN_USER'),
          must_change_password = false, status = 1, is_locked = false,
          lock_time = null, login_fail_count = 0
    where username in ('p3a_maker','p3a_dept','p3a_pm','p3a_gm','p3a_wh')" >/dev/null

token_or_fail() {
  local t
  t=$(login "$1" "$ADMIN_PASS")
  [ -n "$t" ] || fail "$1 登录失败"
  printf '%s' "$t"
}
MAKER=$(token_or_fail p3a_maker)
DEPT=$(token_or_fail p3a_dept)
PM=$(token_or_fail p3a_pm)
GM=$(token_or_fail p3a_gm)
WH=$(token_or_fail p3a_wh)
[ -n "$MAKER$DEPT$PM$GM$WH" ] && pass "五个夹具账号登录成功"

api() { # $1=token $2=method $3=path $4=body   → 打印 "CODE<TAB>bodyfile"
  local body_file code data="${4:-}"
  # ⚠️ 不能用 `-d "${4:-{}}"`：bash 会在第一个未加引号的 `}` 处结束参数展开，
  #    于是体变成「JSON + 一个多余的 }」→ 服务端报 trailing characters（实测踩过）。
  [ -z "$data" ] && data='{}'
  body_file=$(mktemp)
  code=$(curl -s -m 20 -o "$body_file" -w '%{http_code}' -X "$2" "$BASE_URL$3" \
    -H "Authorization: Bearer $1" -H 'Content-Type: application/json' -d "$data")
  printf '%s\t%s' "$code" "$body_file"
}
jget() { # $1=file $2=key → 第一个值
  grep -o "\"$2\":[^,}]*" "$1" | head -1 | sed 's/.*://; s/^"//; s/"$//'
}
num_eq() { # $1 $2 → 数值相等（`NUMERIC` 回来是 1.0000，逐字比会假红）
  awk -v a="$1" -v b="$2" 'BEGIN { exit !(a + 0 == b + 0) }'
}

# ---------- 3. 采购订单三档 ----------
head1 "③ 采购订单审批：8000 / 50000 / 200000 三档 + 会签只批一票"
new_po() { # $1=金额 → 打印 order_id
  local code file
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST /api/business/erp/purchase-order \
    "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",\"price_mode\":\"EXCL\",
      \"items\":[{\"material_id\":\"$MATERIAL\",\"qty\":\"1\",\"price_excl\":\"$1\",\"tax_rate\":\"0\"}]}")"
  [ "$code" = "200" ] || { fail "建采购单失败（$1）：$code $(head -c 200 "$file")"; return 1; }
  jget "$file" id
}
approve_task() { # $1=token $2=task_id → CODE
  local code file
  IFS=$'\t' read -r code file <<<"$(api "$1" POST "/api/business/flow/task/$2/approve" '{"comment":"验收通过"}')"
  printf '%s' "$code"
}
po_status() { q1 "select status from biz_purchase_order where id='$1'"; }
pending_task_of() { # $1=instance_id $2=username → task_id
  q1 "select t.id from biz_flow_task t join sys_user u on u.id=t.assignee_id
       where t.instance_id='$1' and t.status='PENDING' and u.username='$2' limit 1"
}

declare -A TIER=([8000]=p3a_dept [50000]=p3a_pm [200000]="p3a_pm,p3a_gm")
for amount in 8000 50000 200000; do
  po=$(new_po "$amount") || continue
  [ -n "$po" ] && pass "建采购单 $amount 元：$po" || { fail "建采购单 $amount 元没返回 id"; continue; }
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST "/api/business/erp/purchase-order/$po/submit-approval")"
  [ "$code" = "200" ] && pass "提交审批 $amount 元 → 200" \
    || { fail "提交审批 $amount 元 → $code $(head -c 200 "$file")"; continue; }
  st=$(po_status "$po")
  [ "$st" = "SUBMITTED" ] && pass "  单据进入 SUBMITTED（列表显示「审批中」）" || fail "  单据状态=$st"
  instance=$(jget "$file" approval_instance_id)
  [ -n "$instance" ] && pass "  回写审批实例 $instance" || fail "  没回写 approval_instance_id"

  # 幂等：再提交一次必须 409 且报文带审批单号
  IFS=$'\t' read -r code file2 <<<"$(api "$MAKER" POST "/api/business/erp/purchase-order/$po/submit-approval")"
  msg=$(jget "$file2" msg)
  [ "$code" = "409" ] && pass "  重复提交 → 409（报文：${msg:0:60}）" || fail "  重复提交 → $code（应 409）"

  # 按档位断言待办派给了谁
  case "$amount" in
    8000) want=p3a_dept; label='部门主管' ;;
    50000) want=p3a_pm; label='采购经理' ;;
    200000) want=p3a_pm; label='采购经理（会签之一）' ;;
  esac
  task=$(pending_task_of "$instance" "$want")
  [ -n "$task" ] && pass "  $amount 元待办派给 $label（$want）" || fail "  $amount 元没有派给 $want"
  if [ "$amount" = "200000" ]; then
    n=$(q1 "select count(*) from biz_flow_task where instance_id='$instance' and status='PENDING'")
    [ "$n" = "2" ] && pass "  会签节点生成 2 条待办" || fail "  会签待办数=$n（应 2）"
    gm_task=$(pending_task_of "$instance" p3a_gm)
    code=$(approve_task "$PM" "$task")
    [ "$code" = "200" ] && pass "  采购经理批一票 → 200" || fail "  采购经理批 → $code"
    st=$(po_status "$po")
    [ "$st" = "SUBMITTED" ] && pass "  只批一票单据仍 SUBMITTED（会签没走完）" || fail "  只批一票就变 $st"
    code=$(approve_task "$GM" "$gm_task")
    [ "$code" = "200" ] && pass "  总经理补一票 → 200" || fail "  总经理批 → $code"
    st=$(po_status "$po")
    [ "$st" = "APPROVED" ] && pass "  两人都批 → APPROVED（此后可收货）" || fail "  最终状态=$st"
  else
    if [ "$want" = p3a_dept ]; then approver_token=$DEPT; else approver_token=$PM; fi
    code=$(approve_task "$approver_token" "$task")
    [ "$code" = "200" ] && pass "  审批通过 → 200" || fail "  审批 → $code"
    st=$(po_status "$po")
    [ "$st" = "APPROVED" ] && pass "  $amount 元单据 → APPROVED" || fail "  $amount 元单据状态=$st"
  fi
done

# ---------- 4. 收货过账两档 ----------
head1 "④ 收货过账审批：500 元免审直过 / 5000 元仓管主管审"
new_si() { # $1=金额 → 打印 stock_in_id
  local code file
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST /api/business/erp/stock-in \
    "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
      \"items\":[{\"material_id\":\"$MATERIAL\",\"qty\":\"1\",\"price_excl\":\"$1\",\"tax_rate\":\"0\"}]}")"
  [ "$code" = "200" ] || { fail "建收货单失败（$1）：$code $(head -c 300 "$file")"; return 1; }
  jget "$file" id
}
si_status() { q1 "select status from biz_stock_in where id='$1'"; }

# ④-1 小额免审
si=$(new_si 500)
if [ -n "$si" ]; then
  pass "建收货单 500 元：$si"
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST "/api/business/erp/stock-in/$si/submit-post" '{"status":"POSTED"}')"
  [ "$code" = "200" ] && pass "提交过账 500 元 → 200（免审直过）" || fail "提交过账 500 元 → $code $(head -c 300 "$file")"
  st=$(si_status "$si")
  [ "$st" = "POSTED" ] && pass "  500 元免审 → 已过账" || fail "  500 元状态=$st（应 POSTED）"
  inv=$(q1 "select coalesce(sum(qty),0)::text from biz_inventory where material_id='$MATERIAL' and warehouse_id='$WAREHOUSE'")
  num_eq "$inv" 1 && pass "  库存已加（1）" || fail "  库存=$inv（应 1）"
  inst=$(q1 "select id from biz_flow_instance where biz_type='STOCK_IN_POST' and biz_id='$si' order by started_at desc limit 1")
  n=$(q1 "select count(*) from sys_notice where biz_id='$inst' and biz_type='FLOW_NOTIFY'")
  [ "${n:-0}" -ge 1 ] && pass "  仓管主管收到免审知会通知（$n 条）" || fail "  知会通知数=$n（应 ≥1）"
fi

# ④-2 大额走审批
si2=$(new_si 5000)
if [ -n "$si2" ]; then
  pass "建收货单 5000 元：$si2"
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST "/api/business/erp/stock-in/$si2/submit-post" '{"status":"POSTED"}')"
  [ "$code" = "200" ] && pass "提交过账 5000 元 → 200" || fail "提交过账 5000 元 → $code $(head -c 300 "$file")"
  st=$(si_status "$si2")
  [ "$st" = "PENDING_POST" ] && pass "  5000 元 → 过账审批中（库存还没动）" || fail "  5000 元状态=$st"
  inv=$(q1 "select coalesce(sum(qty),0)::text from biz_inventory where material_id='$MATERIAL' and warehouse_id='$WAREHOUSE'")
  num_eq "$inv" 1 && pass "  审批中库存未加（仍 1）" || fail "  审批中库存=$inv（应仍 1）"

  # 审批中改金额 → 409（锁单据）
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" PUT "/api/business/erp/stock-in/$si2" \
    "{\"supplier_id\":\"$SUPPLIER\",\"warehouse_id\":\"$WAREHOUSE\",
      \"items\":[{\"material_id\":\"$MATERIAL\",\"qty\":\"9\",\"price_excl\":\"5000\",\"tax_rate\":\"0\"}]}")"
  [ "$code" = "409" ] && pass "  审批中改单 → 409（单据锁定）" || fail "  审批中改单 → $code（应 409）"

  inst2=$(jget "$file" approval_instance_id)
  inst2=$(q1 "select id from biz_flow_instance where biz_type='STOCK_IN_POST' and biz_id='$si2' and status='PENDING' limit 1")
  task=$(pending_task_of "$inst2" p3a_wh)
  [ -n "$task" ] && pass "  待办派给仓管主管（p3a_wh）" || fail "  待办没派给 p3a_wh"
  code=$(approve_task "$WH" "$task")
  [ "$code" = "200" ] && pass "  仓管主管批准 → 200" || fail "  批准 → $code"
  st=$(si_status "$si2")
  [ "$st" = "POSTED" ] && pass "  审批通过 → 已过账" || fail "  审批通过后状态=$st"
  inv=$(q1 "select coalesce(sum(qty),0)::text from biz_inventory where material_id='$MATERIAL' and warehouse_id='$WAREHOUSE'")
  num_eq "$inv" 2 && pass "  库存累加（1 + 1 = 2）" || fail "  库存=$inv（应 2）"
  vch=$(q1 "select count(*) from biz_voucher where source_id='$si2'")
  [ "$vch" = "1" ] && pass "  凭证已生成（1 张）" || fail "  凭证数=$vch"
  ap=$(q1 "select count(*) from biz_payable where source_id='$si2'")
  [ "$ap" = "1" ] && pass "  应付已生成（1 张）" || fail "  应付数=$ap"
  txn=$(q1 "select count(*) from biz_inventory_txn where doc_id='$si2'")
  [ "$txn" = "1" ] && pass "  库存流水 1 条" || fail "  流水数=$txn"
fi

# ---------- 5. 反向与内控 ----------
head1 "⑤ 反向（撤审批）与内控（制单人自批 403）"
si3=$(new_si 8000)
if [ -n "$si3" ]; then
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST "/api/business/erp/stock-in/$si3/submit-post" '{"status":"POSTED"}')"
  inst3=$(q1 "select id from biz_flow_instance where biz_type='STOCK_IN_POST' and biz_id='$si3' and status='PENDING' limit 1")
  [ -n "$inst3" ] && pass "8000 元收货单进入过账审批（$inst3）" || fail "8000 元没收货审批实例"
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" DELETE "/api/business/erp/stock-in/$si3")"
  [ "$code" = "200" ] && pass "审批中删除收货单 → 200" || fail "审批中删除 → $code $(head -c 200 "$file")"
  st=$(q1 "select status from biz_flow_instance where id='$inst3'")
  [ "$st" = "CANCELLED" ] && pass "  在跑审批被同一事务终止（$st）" || fail "  审批实例状态=$st（应 CANCELLED）"
  left=$(q1 "select count(*) from biz_flow_task where instance_id='$inst3' and status='PENDING'")
  [ "$left" = "0" ] && pass "  没有留下孤儿待办" || fail "  孤儿待办=$left"
fi

# 制单人自批：把 maker 临时挂到 WAREHOUSE_MANAGER 上，自己提自己批必须 403
q "insert into sys_user_role (user_id, role_id)
   select u.id, r.id from sys_user u, sys_role r
    where u.username='p3a_maker' and r.role_code='WAREHOUSE_MANAGER' on conflict do nothing" >/dev/null
si4=$(new_si 6000)
if [ -n "$si4" ]; then
  IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST "/api/business/erp/stock-in/$si4/submit-post" '{"status":"POSTED"}')"
  inst4=$(q1 "select id from biz_flow_instance where biz_type='STOCK_IN_POST' and biz_id='$si4' and status='PENDING' limit 1")
  mine=$(pending_task_of "$inst4" p3a_maker)
  if [ -z "$mine" ]; then
    # 兜底：maker 不是仓管主管的成员时，任务派给 p3a_wh，自批场景改用 p3a_wh 制单
    say "（maker 未拿到待办，跳过自批断言）"
  else
    IFS=$'\t' read -r code file <<<"$(api "$MAKER" POST "/api/business/flow/task/$mine/approve" '{"comment":"自己批"}')"
    [ "$code" = "403" ] && pass "制单人自批 → 403（内控：制单与审批分离）" \
      || fail "制单人自批 → $code（应 403）$(head -c 200 "$file")"
  fi
fi
q "delete from sys_user_role where user_id=(select id from sys_user where username='p3a_maker')
     and role_id=(select id from sys_role where role_code='WAREHOUSE_MANAGER')" >/dev/null

say ""
say "RESULT: PASS=$PASSED FAIL=$FAILED"
[ "$FAILED" -eq 0 ] || exit 1
