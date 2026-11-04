#!/bin/bash
# ============================================================================
# t_a6e8aca3 真机验收：**全新安装的空库**上，部门主管 / 财务经理能否审掉一条预算审批。
#
# 前置：$S/boot_fresh.sh 已在 127.0.0.1:18161 起了连 cloudpivot_pg239_fresh 的实例
#       （180 个迁移含 V0239 全部执行完）。
#
# 断言链：
#   ① admin 登录（首登强制改密）
#   ② 建部门（预算 dept_id NOT NULL）
#   ③ 夹具：e2e_dept_leader→DEPT_LEADER、e2e_fin_mgr→FINANCE_MANAGER（口令同 admin）
#   ④ admin 建预算 → 提交 → 待办落在 n_dept
#   ⑤ 部门主管登录 → GET /task/todo（要 instance:read）→ 200 且含本任务
#   ⑥ 部门主管 POST /task/{id}/approve（要 approve）→ 200 → n_dept APPROVED → 推进到 n_fin
#   ⑦ 财务经理同样审 → 200 → 实例 APPROVED → 预算 ACTIVE
#   ⑧ 反向对照：down.sql 撤掉 V0239 授权 + 清权限缓存 → 同一账号对**新**待办再点 → 403
#      （证明 ⑥⑦ 的 200 是 V0239 给的，不是超管短路）
#   ⑨ 复原：重跑 up.sql（幂等）→ 同一账号再点 → 200
# ============================================================================
set -uo pipefail
S=/root/.hermes/profiles/backend-developer/cache/scratch
EV=/opt/cloudpivot/docs/evidence/20261105-t_a6e8aca3-approver-grants
BASE=http://127.0.0.1:18161
DB=cloudpivot_pg239_fresh
M=/opt/cloudpivot/backend/migrations/business
mkdir -p "$EV"
export PGPASSWORD="${ALUN_DATABASE_PASSWORD:-alun}"   # 本机 dev 库口令，允许用环境变量覆盖

q()  { psql -h 127.0.0.1 -U alun -d "$DB" -At -F'|' -c "$1" 2>/dev/null; }
qp() { q "$1" | head -1 | awk -F'|' '{print $1}'; }
PASS=0; FAIL=0
pass() { echo -e "  \033[32mPASS\033[0m $1"; PASS=$((PASS+1)); }
fail() { echo -e "  \033[31mFAIL\033[0m $1"; FAIL=$((FAIL+1)); }

NEWPASS='E2eGrant#2026'
ADMIN_PASS_INIT=$(grep '^NIK_ADMIN_PASSWORD=' $S/fresh.env | cut -d= -f2-)
if [ -z "$ADMIN_PASS_INIT" ]; then
  ADMIN_PASS_INIT=$(sed 's/\x1b\[[0-9;]*m//g' $S/fresh.log | grep -a '未注入管理员初始口令' | head -1 \
                      | grep -o 'password=[A-Za-z0-9]*' | cut -d= -f2)
fi

login_body() { curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
                 -d "{\"username\":\"$1\",\"password\":\"$2\"}"; }
token_of()  { printf '%s' "$1" | grep -o '"access_token":"[^"]*"' | head -1 | sed 's/.*:"//; s/"$//'; }
# 登录带重试：接口有「登录尝试过于频繁」限流（本机连续多次登录会命中）
login() {
  local u=$1 p=$2 i tok
  for i in 1 2 3; do
    tok=$(token_of "$(login_body "$u" "$p")")
    [ -n "$tok" ] && { printf '%s' "$tok"; return; }
    sleep 8
  done
}
# 主体/权限缓存是**进程内 LRU**（profile=test 的 [cache] type="local"，见 config/config.toml:67），
# 不是 Redis（本机 6379 未监听）。改授权后要么等 TTL(300s)，要么重启进程 ——
# 真实上线路径就是「迁移 + 重启」，所以这里用重启来复现「重启后按新授权生效」。
restart_instance() {
  local pid i
  # 按端口取真实 pid（`$!` 拿到的是 setsid 包装进程，kill 它杀不掉子进程 —— 实测踩过）
  pid=$(ss -lptnH 'sport = :18161' | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
  [ -n "${pid:-}" ] && kill "$pid" 2>/dev/null
  for i in $(seq 1 20); do
    ss -lntH 'sport = :18161' | grep -q . || break
    sleep 1
  done
  ( cd /opt/cloudpivot/backend && setsid env $(grep -v '^#' $S/fresh.env | tr '\n' ' ') \
      ./target/debug/cloudpivot >> $S/fresh.log 2>&1 & )
  for i in $(seq 1 60); do
    curl -s -m 1 "$BASE/health" >/dev/null 2>&1 && return 0
    sleep 1
  done
  return 1
}
flush_cache() { :; }
call() { # $1=token $2=method $3=path $4=body → "CODE\tFILE"
  local out; out=$(mktemp)
  local code; code=$(curl -s -m 20 -o "$out" -w '%{http_code}' -X "$2" "$BASE$3" \
      -H "Authorization: Bearer $1" -H 'Content-Type: application/json' ${4:+-d "$4"})
  printf '%s\t%s' "$code" "$out"
}
jsonid() { grep -o '"id":"[^"]*"' "$1" | head -1 | sed 's/.*:"//; s/"$//'; }

# ---------------------------------------------------------------- ① 登录
echo "==================== ① admin 登录 ===================="
flush_cache
TOKEN=""
for cand in "$ADMIN_PASS_INIT" "$NEWPASS"; do
  [ -z "$cand" ] && continue
  TOKEN=$(login admin "$cand"); [ -n "$TOKEN" ] && ADMIN_PASS_INIT="$cand" && break
done
[ -n "$TOKEN" ] || { fail "admin 登录失败"; exit 1; }
BODY=$(login_body admin "$ADMIN_PASS_INIT")
if printf '%s' "$BODY" | grep -q '"must_change_password":true'; then
  c=$(curl -s -m 10 -o /dev/null -w '%{http_code}' -X PUT "$BASE/api/system/auth/password" \
        -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        -d "{\"old_password\":\"$ADMIN_PASS_INIT\",\"new_password\":\"$NEWPASS\"}")
  [ "$c" = "200" ] && pass "admin 首登改密 200" || fail "admin 首登改密 $c"
  flush_cache; TOKEN=$(login admin "$NEWPASS")
fi
[ -n "$TOKEN" ] && pass "admin 登录成功" || { fail "admin 改密后登录失败"; exit 1; }

# ---------------------------------------------------------------- ② 部门
echo "==================== ② 部门 ===================="
DEPT_ID=$(qp "select id from sys_dept where dept_code='E2EFIN' and deleted_at is null limit 1")
if [ -z "$DEPT_ID" ]; then
  IFS=$'\t' read -r code f <<<"$(call "$TOKEN" POST /api/system/dept '{"dept_code":"E2EFIN","dept_name":"E2E 财务部","status":1,"sort_order":10}')"
  [ "$code" = "200" ] || { fail "建部门失败 $code: $(head -c 300 "$f")"; exit 1; }
  DEPT_ID=$(jsonid "$f"); pass "建部门 200（dept_id=$DEPT_ID）"
else
  pass "部门已存在，复用（dept_id=$DEPT_ID）"
fi

# ---------------------------------------------------------------- ③ 夹具
echo "==================== ③ 夹具账号 ===================="
mkuser() { # $1=username $2=realname $3=role_code → 打印 user_id
  q "insert into sys_user (username, password, real_name, must_change_password, status, dept_id)
     values ('$1', (select password from sys_user where username='admin'), '$2', false, 1, '$DEPT_ID'::uuid)
     on conflict (username) where deleted_at is null do update set password = excluded.password,
         must_change_password = false, status = 1, is_locked = false, lock_time = null,
         login_fail_count = 0, dept_id = excluded.dept_id" >/dev/null
  q "insert into sys_user_role (user_id, role_id)
     select u.id, r.id from sys_user u, sys_role r where u.username='$1' and r.role_code='$3'
     on conflict do nothing" >/dev/null
  qp "select id from sys_user where username='$1'"
}
LEADER=e2e_dept_leader; FINMGR=e2e_fin_mgr
LEADER_ID=$(mkuser "$LEADER" 'E2E 部门主管' DEPT_LEADER)
FINMGR_ID=$(mkuser "$FINMGR" 'E2E 财务经理' FINANCE_MANAGER)
if [ -n "$LEADER_ID" ] && [ -n "$FINMGR_ID" ]; then
  pass "夹具就绪：$LEADER→DEPT_LEADER，$FINMGR→FINANCE_MANAGER"
else
  fail "夹具创建失败"; exit 1
fi
echo "  approve 授权行数：DEPT_LEADER=$(qp "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where r.role_code='DEPT_LEADER' and rm.menu_id='20000000-0000-0000-0000-000000000803'")  FINANCE_MANAGER=$(qp "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where r.role_code='FINANCE_MANAGER' and rm.menu_id='20000000-0000-0000-0000-000000000803'")"

# ---------------------------------------------------------------- ④ 建预算
echo "==================== ④ 建预算 + 提交 ===================="
pick_account() { qp "select a.id from biz_account a where a.is_leaf=1 and a.status=1 and a.deleted_at is null
                      and not exists (select 1 from biz_budget b where b.account_id=a.id and b.dept_id='$DEPT_ID'::uuid
                                      and b.fiscal_year=2026 and b.deleted_at is null)
                      order by a.account_code limit 1"; }
new_budget() { # $1=amount → 打印 budget_id（失败打印空）
  local acc bf code
  acc=$(pick_account)
  [ -z "$acc" ] && { echo ""; return; }
  bf=$(mktemp)
  code=$(curl -s -m 20 -o "$bf" -w '%{http_code}' -X POST "$BASE/api/business/erp/budget" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
      -d "{\"fiscal_year\":2026,\"dept_id\":\"$DEPT_ID\",\"account_id\":\"$acc\",\"total_amount\":$1,\"remark\":\"t_a6e8aca3 真机验收\"}")
  if [ "$code" != "200" ]; then echo "ERR $code $(head -c 300 "$bf")" >&2; echo ""; return; fi
  echo "  科目 $(qp "select account_code from biz_account where id='$acc'")  额度 $1" >&2
  jsonid "$bf"
}
submit_budget() { # $1=budget_id → 打印 HTTP code
  IFS=$'\t' read -r code f <<<"$(call "$TOKEN" POST "/api/business/erp/budget/$1/submit" '{}')"
  [ "$code" = "200" ] && cp "$f" "$EV/04-budget-submit.json"
  echo "$code"
}
BUDGET_ID=$(new_budget 12000)
[ -n "$BUDGET_ID" ] || { fail "建预算失败"; exit 1; }
pass "建预算 200（budget_id=$BUDGET_ID，status=$(qp "select status from biz_budget where id='$BUDGET_ID'")）"
code=$(submit_budget "$BUDGET_ID")
[ "$code" = "200" ] && pass "提交审批 200（status=$(qp "select status from biz_budget where id='$BUDGET_ID'")）" \
                    || { fail "提交失败 $code"; exit 1; }
INSTANCE_ID=$(qp "select approval_instance_id from biz_budget where id='$BUDGET_ID'")
TASK1=$(qp "select id from biz_flow_task where instance_id='$INSTANCE_ID' and status='PENDING' and assignee_id='$LEADER_ID' limit 1")
echo "  instance=$INSTANCE_ID  待办：$(q "select t.node_key||'→'||coalesce(u.username,'(空)') from biz_flow_task t left join sys_user u on u.id=t.assignee_id where t.instance_id='$INSTANCE_ID' and t.status='PENDING'" | tr '\n' ' ')"
[ -n "$TASK1" ] && pass "待办落在部门主管（task=$TASK1）" || { fail "部门主管没有待办"; exit 1; }

# ---------------------------------------------------------------- ⑤ 待办列表
echo "==================== ⑤ 部门主管：待办列表（instance:read） ===================="
flush_cache
LTOKEN=$(login "$LEADER" "$NEWPASS")
[ -n "$LTOKEN" ] && pass "$LEADER 登录成功（非超管）" || { fail "$LEADER 登录失败"; exit 1; }
IFS=$'\t' read -r code f <<<"$(call "$LTOKEN" GET '/api/business/flow/task/todo?page=1&page_size=10')"
cp "$f" "$EV/05-leader-todo.json"
if [ "$code" = "200" ] && grep -q "$TASK1" "$f"; then
  pass "GET /api/business/flow/task/todo → 200 且含本任务（business:flow:instance:read 已授）"
else
  fail "GET /task/todo $code: $(head -c 300 "$f")"
fi

# ---------------------------------------------------------------- ⑥ 部门主管审
echo "==================== ⑥ 部门主管审批（approve） ===================="
IFS=$'\t' read -r code f <<<"$(call "$LTOKEN" POST "/api/business/flow/task/$TASK1/approve" '{"comment":"t_a6e8aca3 真机验收：部门主管同意"}')"
cp "$f" "$EV/06-leader-approve.json"
echo "  HTTP $code  $(head -c 300 "$f")"
[ "$code" = "200" ] && pass "POST /api/business/flow/task/$TASK1/approve → 200（不再 403 需要权限 business:flow:approve）" \
                    || fail "部门主管 approve 失败 $code"
echo "  n_dept 任务状态 = $(qp "select status from biz_flow_task where id='$TASK1'")"
TASK2=$(qp "select id from biz_flow_task where instance_id='$INSTANCE_ID' and status='PENDING' and assignee_id='$FINMGR_ID' limit 1")
[ -n "$TASK2" ] && pass "审批链推进到财务经理（task=$TASK2）" || fail "没推进到财务经理节点"

# ---------------------------------------------------------------- ⑦ 财务经理审
echo "==================== ⑦ 财务经理审批 ===================="
flush_cache
FTOKEN=$(login "$FINMGR" "$NEWPASS")
[ -n "$FTOKEN" ] && pass "$FINMGR 登录成功" || fail "$FINMGR 登录失败"
if [ -n "${TASK2:-}" ]; then
  IFS=$'\t' read -r code f <<<"$(call "$FTOKEN" POST "/api/business/flow/task/$TASK2/approve" '{"comment":"t_a6e8aca3 真机验收：财务经理同意"}')"
  cp "$f" "$EV/07-finmgr-approve.json"
  echo "  HTTP $code  $(head -c 300 "$f")"
  [ "$code" = "200" ] && pass "财务经理 approve 200" || fail "财务经理 approve $code"
fi
IST=$(qp "select status from biz_flow_instance where id='$INSTANCE_ID'")
BST=$(qp "select status from biz_budget where id='$BUDGET_ID'")
echo "  实例=$IST  预算=$BST"
[ "$IST" = "APPROVED" ] && pass "审批实例 APPROVED" || fail "实例状态 $IST"
[ "$BST" = "ACTIVE" ] && pass "预算随桥接回调 ACTIVE（审批链真的推到底了）" || fail "预算状态 $BST"

# ---------------------------------------------------------------- ⑧ 反向对照
echo "==================== ⑧ 反向对照：撤掉 V0239 授权后同一个人再点 ===================="
BUDGET2=$(new_budget 13000)
[ -n "$BUDGET2" ] || fail "第二条预算创建失败"
code=$(submit_budget "$BUDGET2")
INSTANCE2=$(qp "select approval_instance_id from biz_budget where id='$BUDGET2'")
TASK3=$(qp "select id from biz_flow_task where instance_id='$INSTANCE2' and status='PENDING' and assignee_id='$LEADER_ID' limit 1")
echo "  第二条 budget=$BUDGET2 instance=$INSTANCE2 task=${TASK3:-无}（提交 $code）"
psql -q -v ON_ERROR_STOP=1 -h 127.0.0.1 -U alun -d "$DB" -f $M/V0239__flow_approver_role_grants.down.sql >/dev/null 2>&1 \
  && pass "down.sql 执行（8 个节点的授权清零；库里 approve 授权 = $(qp "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000000803'") 行）" || fail "down.sql 执行失败"
restart_instance && pass "重启实例（进程内权限缓存清空，等价于真实「迁移 + 重启」上线路径）" || fail "重启失败"
LTOKEN2=$(login "$LEADER" "$NEWPASS")
if [ -n "${TASK3:-}" ]; then
  IFS=$'\t' read -r code f <<<"$(call "$LTOKEN2" POST "/api/business/flow/task/$TASK3/approve" '{"comment":"撤权后重试"}')"
  cp "$f" "$EV/08-approve-after-down.json"
  echo "  HTTP $code  $(head -c 300 "$f")"
  [ "$code" = "403" ] && pass "撤权后同一个部门主管再点 → 403（复现卡面现象）" || fail "撤权后不是 403（$code）"
fi

# ---------------------------------------------------------------- ⑨ 复原
echo "==================== ⑨ 复原：重跑 up.sql（幂等） ===================="
psql -q -v ON_ERROR_STOP=1 -h 127.0.0.1 -U alun -d "$DB" -f $M/V0239__flow_approver_role_grants.up.sql 2>&1 | tail -2
restart_instance && pass "再次重启实例" || fail "重启失败"
LTOKEN3=$(login "$LEADER" "$NEWPASS")
if [ -n "${TASK3:-}" ]; then
  IFS=$'\t' read -r code f <<<"$(call "$LTOKEN3" POST "/api/business/flow/task/$TASK3/approve" '{"comment":"恢复授权后同意"}')"
  cp "$f" "$EV/09-approve-after-up.json"
  echo "  HTTP $code  $(head -c 300 "$f")"
  [ "$code" = "200" ] && pass "重新授权后同一账号 → 200（幂等复原成功）" || fail "复原后仍 $code"
fi

echo
echo "==================== 汇总 ===================="
echo "预算1 $BST / 实例1 $IST  |  预算2 $(qp "select status from biz_budget where id='$BUDGET2'") / 实例2 $(qp "select status from biz_flow_instance where id='$INSTANCE2'")"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] || exit 1
