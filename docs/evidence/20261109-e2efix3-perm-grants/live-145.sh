#!/usr/bin/env bash
# =============================================================================
# E2E-FIX-3 实测：销售退货单权限码零授予 + 同类清单 + 常驻不变量闸
# （卡 t_c7038fa7）
#
# 打的是一个**全新隔离库 + 隔离端口**的实例（不碰 144 生产 / 不碰 :8023 / 不碰别人的库），
# 覆盖卡面 5 条验收：
#   ① e2e02_appr（销售主管）PATCH 退货单状态 → 200（不再 403）
#   ② SALES_CLERK 能建退货单 / 查列表
#   ③ 零授予清单完整（扫描结果，本脚本的 phase A 复现现场 + 闸输出）
#   ④ 不变量检查能发现缺失授权（phase A 闸 rc=1；phase B 恢复后 rc=0）
#   ⑤ push（脚本外完成，见 README）
#
# 「修复前」怎么复现：把 V0356 的 down 跑一遍（只撤本迁移授的 10 行授权），
# 就是 E2E-02 演练当天的库状态；再 up 回来即修复态。库是一次性的，随便折腾。
#
# 用法（145 上）：
#   bash docs/evidence/20261109-e2efix3-perm-grants/live-145.sh
#   可覆盖：DB / PORT / ENVF / REPO
# =============================================================================
set -uo pipefail

DB="${DB:-cp_e2efix3}"
PORT="${PORT:-18319}"
ENVF="${ENVF:-/etc/cloudpivot/cloudpivot.env}"
REPO="${REPO:-/opt/cloudpivot}"
BASE="http://127.0.0.1:$PORT/api"
RAW="/tmp/e2efix3/raw"; mkdir -p "$RAW"
LOG="/tmp/e2efix3.log"
MIG_UP="$REPO/backend/migrations/business/V0356__doc_core_perm_grants.up.sql"
MIG_DOWN="$REPO/backend/migrations/business/V0356__doc_core_perm_grants.down.sql"
GATE="$REPO/scripts/verify/core_perm_grants.py"

PASS=0; FAIL=0
say(){ printf '%s\n' "$*"; }
ok(){ PASS=$((PASS+1)); say "  ✅ $*"; }
bad(){ FAIL=$((FAIL+1)); say "  ❌ $*"; }

# ---- 环境：145 有 /etc/cloudpivot/cloudpivot.env；99 没有则兜底本机 alun ----
if [ -f "$ENVF" ]; then
    set -a; . "$ENVF"; set +a
    SAY_ENV="env 文件 $ENVF"
else
    export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432
    export NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="e2efix3_local_jwt_secret_0123456789abcdefghijklmn"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    export X_CONFIG_SECRET_KEY="e2efix3_local_config_secret"
    SAY_ENV="本机兜底（无 $ENVF，DB 账号 alun）"
fi
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
export PATH="/root/.cargo/bin:$PATH"

BIN="$REPO/backend/target/release/cloudpivot"
[ -x "$BIN" ] || BIN="$REPO/backend/target/debug/cloudpivot"

ADMIN_PASS='E2eFix3@Admin'
NEW_PASS='E2eFix3@Pass145'
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"

PSQL(){ psql -q -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
    -U "${NIK_DATABASE_USER:-alun}" -d "$DB" -At -F'|' -P pager=off -c "$1"; }
PSQLF(){ psql -q -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
    -U "${NIK_DATABASE_USER:-alun}" -d "$DB" -At -F'|' -P pager=off -v ON_ERROR_STOP=1 -f "$1"; }
SU(){ su - postgres -c "$1"; }
GATE_RUN(){ # GATE_RUN → 打印闸输出并返回 rc
    NIK_DATABASE_NAME="$DB" python3 "$GATE"
}

api(){ # api <name> <METHOD> <path> [token] [json]
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data(){ printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
login(){ # login <user> <pass> <tag> → 打印 token
  local user=$1 pass=$2 tag=$3 tok=""
  for _ in $(seq 1 15); do
    api "login_$tag" POST /system/auth/login "" "{\"username\":\"$user\",\"password\":\"$pass\"}"
    tok=$(data '.data.access_token')
    [ -n "$tok" ] && [ "$tok" != "null" ] && break
    sleep 2
  done
  printf '%s' "$tok"
}

say "==== 0. 隔离库 + 隔离实例（$SAY_ENV）"
SU "dropdb --force $DB" >/dev/null 2>&1
SU "createdb -O ${NIK_DATABASE_USER:-alun} $DB" || { echo "createdb 失败"; exit 1; }
cd "$REPO/backend" || exit 1
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT"
: > "$LOG"
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
PID=$!
HEALTH=000
for _ in $(seq 1 120); do
  HEALTH=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")
  [ "$HEALTH" = "200" ] && break
  sleep 2
done
say "  pid=$PID bin=$BIN health=$HEALTH"
[ "$HEALTH" = "200" ] || { bad "实例没起来（health=$HEALTH），日志：$(tail -5 "$LOG")"; kill "$PID" 2>/dev/null; exit 1; }
say "  库指纹：迁移 $(PSQL 'select count(*) from schema_migrations') 条 / V0356 $(PSQL "select count(*) from schema_migrations where name='doc_core_perm_grants'") 行"
[ "$(PSQL "select count(*) from schema_migrations where name='doc_core_perm_grants'")" = "1" ] \
  && ok "V0356 已随迁移落地（schema_migrations 有 doc_core_perm_grants）" \
  || bad "V0356 没被迁移器应用（145 的工作树是不是没 pull？）"
GRANTS=$(PSQL "select count(*) from sys_role_menu rm
                 join sys_role r on r.id=rm.role_id
                 join sys_menu m on m.id=rm.menu_id
                where r.role_code in ('SALES_CLERK','SALES_SUPERVISOR','FINANCE_MANAGER','PURCHASE_MANAGER')
                  and m.auth_code in ('business:erp:sales-return:list','business:erp:sales-return:create',
                                      'business:erp:sales-return:status','business:erp:receipt-refund:list',
                                      'business:erp:receipt-refund:detail','business:erp:receipt-refund:create',
                                      'business:erp:receipt-refund:allocate','business:erp:receipt-refund:status',
                                      'business:erp:unit:create')")
say "  本迁移授权行：$GRANTS（期望 10）"

say "==== 1. admin 登录（全新库首登带 must_change_password）"
TOK=$(login admin "$ADMIN_PASS" admin0)
[ -n "$TOK" ] && [ "$TOK" != "null" ] || { bad "admin 登录失败：$BODY"; kill "$PID" 2>/dev/null; exit 1; }
api chpass PUT /system/auth/password "$TOK" "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}"
say "  改密 http=$HTTP msg=$(data '.msg')"
TOK=$(login admin "$NEW_PASS" admin1)
[ -n "$TOK" ] && [ "$TOK" != "null" ] && ok "admin 已登录" || { bad "二次登录失败：$BODY"; kill "$PID" 2>/dev/null; exit 1; }

say "==== 2. 演练账号（销售员 / 销售主管+财务 / 采购）"
api dept POST /system/dept "$TOK" '{"dept_code":"E2EFIX3-DEPT","dept_name":"E2E-FIX-3 权限演练部","status":1,"sort_order":930}'
DEPT_ID=$(data '.data.id')
[ -z "$DEPT_ID" ] || [ "$DEPT_ID" = "null" ] && DEPT_ID=$(PSQL "select id from sys_dept where dept_code='E2EFIX3-DEPT' and deleted_at is null")
say "  部门 $DEPT_ID"
R_CLERK=$(PSQL "select id from sys_role where role_code='SALES_CLERK' and deleted_at is null")
R_SUP=$(PSQL "select id from sys_role where role_code='SALES_SUPERVISOR' and deleted_at is null")
R_FIN=$(PSQL "select id from sys_role where role_code='FINANCE_MANAGER' and deleted_at is null")
R_GM=$(PSQL "select id from sys_role where role_code='GENERAL_MANAGER' and deleted_at is null")
R_WM=$(PSQL "select id from sys_role where role_code='WAREHOUSE_MANAGER' and deleted_at is null")
R_FD=$(PSQL "select id from sys_role where role_code='FINANCE_DIRECTOR' and deleted_at is null")
R_BUY=$(PSQL "select id from sys_role where role_code='PURCHASE_MANAGER' and deleted_at is null")
say "  角色：CLERK=$R_CLERK SUP=$R_SUP FIN=$R_FIN BUY=$R_BUY"

mkuser(){ # mkuser <username> <password> <realname> <role_ids_json_array>
  api "u_$1" POST /system/user "$TOK" \
    "{\"username\":\"$1\",\"password\":\"$2\",\"real_name\":\"$3\",\"dept_id\":\"$DEPT_ID\",\"status\":1,\"role_ids\":$4}"
  local id
  id=$(data '.data.id')
  if [ -z "$id" ] || [ "$id" = "null" ]; then
    id=$(PSQL "select id from sys_user where username='$1' and deleted_at is null")
    api "u_pwd_$1" PATCH "/system/user/$id/password" "$TOK" "{\"password\":\"$2\"}"
  fi
  printf '%s' "$id"
}
U_SP=$(mkuser e2e02_sp 'E2e02@Sp12345' 'E2E-FIX-3 销售员' "[\"$R_CLERK\"]")
U_APPR=$(mkuser e2e02_appr 'E2e02@Appr123' 'E2E-FIX-3 销售主管' "[\"$R_SUP\",\"$R_FIN\",\"$R_GM\",\"$R_WM\",\"$R_FD\"]")
U_BUY=$(mkuser e2efix3_buyer 'E2eFix3@Buy123' 'E2E-FIX-3 采购' "[\"$R_BUY\"]")
say "  用户：sp=$U_SP appr=$U_APPR buyer=$U_BUY"
[ -n "$U_SP" ] && [ -n "$U_APPR" ] && [ -n "$U_BUY" ] && ok "三个演练账号已建" || bad "建号失败"

say "==== 3. 造 4 张草稿退货单（admin，超管短路；后面分别给主管/销售员做权限对照）"
WH=$(PSQL "select id from biz_warehouse where status=1 and deleted_at is null order by warehouse_code limit 1")
MAT=$(PSQL "select id from biz_material where deleted_at is null order by material_code limit 1")
CUS=$(PSQL "select id from biz_customer where deleted_at is null order by customer_code limit 1" 2>/dev/null)
[ -z "$CUS" ] && CUS=$(PSQL "select id from biz_customer where deleted_at is null limit 1")
say "  warehouse=$WH material=$MAT customer=$CUS"
mkret(){ # mkret <name>
  api "$1" POST /business/erp/sales-return "$TOK" \
    "{\"customer_id\":\"$CUS\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\",\
      \"remark\":\"E2E-FIX-3 权限演练 $1\",\
      \"items\":[{\"material_id\":\"$MAT\",\"qty\":1}]}"
  data '.data.id'
}
D1=$(mkret ret_d1); D2=$(mkret ret_d2); D3=$(mkret ret_d3); D4=$(mkret ret_d4)
say "  草稿：$D1 / $D2 / $D3 / $D4（http=$HTTP）"
[ -n "$D1" ] && [ "$D1" != "null" ] && ok "草稿退货单已建（超管）" || bad "建草稿失败：$BODY"

say "==== 4. ① 复现现场：跑 V0356 down（撤掉本迁移授的 10 行）→ 就是 E2E-02 演练当天的库"
PSQLF "$MIG_DOWN" | tail -2
T_SP=$(login e2e02_sp 'E2e02@Sp12345' spA)
T_APP=$(login e2e02_appr 'E2e02@Appr123' appA)
api a_clerk_list GET "/business/erp/sales-return/page?page=1&page_size=5" "$T_SP"
say "  销售员查列表 http=$HTTP"
[ "$HTTP" = "403" ] && ok "修复前：SALES_CLERK 查列表 403（缺陷复现）" || bad "期望 403，实际 $HTTP"
api a_sup_patch PATCH "/business/erp/sales-return/$D1/status" "$T_APP" '{"target_status":"CANCELLED"}'
say "  销售主管 PATCH 状态 http=$HTTP msg=$(data '.msg')"
[ "$HTTP" = "403" ] && ok "修复前：销售主管 PATCH 状态 403（卡面症状复现：审批只能超管自批）" || bad "期望 403，实际 $HTTP"
say "  ④ 不变量闸打「修复前」的库："
NIK_DATABASE_NAME="$DB" python3 "$GATE" 2>&1 | sed -n 1,12p
GATE_A=${PIPESTATUS[0]}
[ "$GATE_A" = "1" ] && ok "修复前：core_perm_grants.py rc=1（闸能发现缺失授权）" || bad "修复前闸 rc=$GATE_A（期望 1）"

say "==== 5. ② 修复：跑 V0356 up（补 10 行授权；token_version 推进后需重新登录）"
PSQLF "$MIG_UP" | tail -2
T_SP=$(login e2e02_sp 'E2e02@Sp12345' spB)
T_APP=$(login e2e02_appr 'E2e02@Appr123' appB)
T_BUY=$(login e2efix3_buyer 'E2eFix3@Buy123' buyB)
[ -n "$T_SP" ] && [ "$T_SP" != "null" ] && ok "销售员重新登录（token_version 已推进）" || bad "销售员登录失败"

say "  ---- 验收 ②：SALES_CLERK 建单 + 查列表 ----"
api b_clerk_create POST /business/erp/sales-return "$T_SP" \
  "{\"customer_id\":\"$CUS\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\",\
    \"remark\":\"E2E-FIX-3 销售员自建\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":2}]}"
RET_SP=$(data '.data.id'); RET_NO=$(data '.data.return_no')
say "  http=$HTTP return_no=$RET_NO id=$RET_SP msg=$(data '.msg')"
if [ "$HTTP" = "200" ] || [ "$HTTP" = "201" ]; then ok "销售员建退货单成功（http=$HTTP）$RET_NO"; else bad "销售员建单失败 http=$HTTP $(data '.msg')"; fi
api b_clerk_list GET "/business/erp/sales-return/page?page=1&page_size=5" "$T_SP"
TOTAL=$(data '.data.total')
say "  http=$HTTP total=$TOTAL"
[ "$HTTP" = "200" ] && ok "销售员查列表 200（total=$TOTAL）" || bad "销售员查列表 http=$HTTP"
api b_clerk_detail GET "/business/erp/sales-return/$RET_SP" "$T_SP"
say "  详情 http=$HTTP status=$(data '.data.status')"
[ "$HTTP" = "200" ] && ok "销售员查详情 200" || bad "销售员查详情 http=$HTTP"
api b_clerk_retble GET "/business/erp/sales-return/returnable/$D1" "$T_SP"
say "  可退明细接口 http=$HTTP（无发货单 → 预期 4xx 业务错，不是 403 权限错）"

say "  ---- 验收 ①：销售主管 PATCH 退货单状态 → 200（不再 403）----"
api b_sup_patch PATCH "/business/erp/sales-return/$D2/status" "$T_APP" '{"target_status":"CANCELLED"}'
say "  http=$HTTP status=$(data '.data.status') msg=$(data '.msg')"
if [ "$HTTP" = "200" ] || [ "$HTTP" = "201" ]; then ok "销售主管 PATCH 状态 → 200（status=$(data '.data.status')）"; else bad "销售主管 PATCH 状态 http=$HTTP $(data '.msg')"; fi
api b_sup_submit PATCH "/business/erp/sales-return/$D3/status" "$T_APP" '{"target_status":"SUBMITTED"}'
say "  D3 提交 http=$HTTP msg=$(data '.msg')（ATT-06 附件闸可能 400 —— 关键是**不是 403**）"
case "$HTTP" in 403) bad "提交仍 403（权限没生效）";; *) ok "提交不再是 403（http=$HTTP：权限已通过，剩下的都是业务闸）";; esac

say "  ---- 职责分离：销售员**不得**能审批（V0356 不给 CLERK sales-return:status）----"
api b_clerk_patch PATCH "/business/erp/sales-return/$D4/status" "$T_SP" '{"target_status":"CANCELLED"}'
say "  http=$HTTP msg=$(data '.msg')"
[ "$HTTP" = "403" ] && ok "销售员 PATCH 状态 403（自建自批被堵死，职责分离成立）" || bad "营销员也能批！http=$HTTP（SoD 失效）"

say "  ---- 残余观察（本卡不改，如实记录）：销售员能不能选物料/仓库 ----"
api b_clerk_material GET "/business/erp/material/page?page=1&page_size=1" "$T_SP"
say "    /material/page  http=$HTTP（SALES_CLERK 的角色矩阵里没有 material:list）"
api b_clerk_warehouse GET "/business/erp/warehouse/page?page=1&page_size=1" "$T_SP"
say "    /warehouse/page http=$HTTP（同样没有 warehouse:list）"
say "    → 退货页前端 `Promise.allSettled` 会吞掉这两个 403：页面能开，但物料/仓库下拉为空、明细只显示 id。"

say "  ---- 同类补齐：收款退款单（财务）+ 计量单位新增（采购）----"
api b_fin_refund GET "/business/erp/receipt-refund/page?page=1&page_size=5" "$T_APP"
say "  收款退款单列表 http=$HTTP（财务经理）"
[ "$HTTP" = "200" ] && ok "FINANCE_MANAGER 能用收款退款单（原 403）" || bad "收款退款单 http=$HTTP"
UNIT_CODE="E2EFIX3-U$(date +%H%M%S)"
api b_buy_unit POST /business/erp/unit "$T_BUY" "{\"unit_code\":\"$UNIT_CODE\",\"unit_name\":\"E2E-FIX-3 演练单位\",\"is_base\":true,\"status\":1}"
say "  计量单位新增 http=$HTTP msg=$(data '.msg')"
case "$HTTP" in 200|201) ok "PURCHASE_MANAGER 能建计量单位（原 403）";; *) bad "建单位 http=$HTTP $(data '.msg')";; esac

say "  ---- ④ 不变量闸打「修复后」的库 ----"
NIK_DATABASE_NAME="$DB" python3 "$GATE" 2>&1 | sed -n 1,4p
GATE_B=${PIPESTATUS[0]}
[ "$GATE_B" = "0" ] && ok "修复后：core_perm_grants.py rc=0（ERP 核心码 98/98 有非超管授权）" || bad "修复后闸 rc=$GATE_B（期望 0）"

say "==== 6. 权限快照（库里实际授予关系）"
PSQL "select m.auth_code || ' -> ' || string_agg(r.role_code, ',' order by r.role_code)
        from sys_menu m
        join sys_role_menu rm on rm.menu_id = m.id
        join sys_role r on r.id = rm.role_id and r.deleted_at is null
       where m.auth_code like 'business:erp:sales-return:%'
          or m.auth_code like 'business:erp:receipt-refund:%'
          or m.auth_code = 'business:erp:unit:create'
       group by m.auth_code order by 1"

say "==== 7. 收尾：停实例（库保留供人工复核）"
kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null
say ""
say "================ 汇总：PASS=$PASS FAIL=$FAIL ================"
[ "$FAIL" = "0" ] || exit 1
