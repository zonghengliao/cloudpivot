#!/usr/bin/env bash
# =============================================================================
# SO-25 菜单树/角色授权验收（真后端 + 真 token curl，全程留证）
# =============================================================================
#   bash scripts/verify/so25_menu_apply.sh up          # 先建库并应用迁移（:18225）
#   bash scripts/verify/so25_menu_verify.sh            # 再跑本脚本
#
# 验什么（对应卡面验收三条）：
#   ① 迁移 APPLY_OK（由 so25_menu_apply.sh 断言 schema_migrations 记账）
#   ② laoliu 账号能看到销售菜单（GET /api/system/auth/menus → 「销售管理」+ 6 个子页）
#   ③ 各角色权限正确（5 角色菜单子集 + 越权 403 / 有权 200 的实测对照）
#
# 证据：docs/evidence/20261106-so25-sales-menu/raw/*（响应体逐份落盘，JWT 已脱敏）
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
PORT=${SO25_PORT:-18225}
BASE="http://127.0.0.1:$PORT/api"
EVID="$ROOT/docs/evidence/20261106-so25-sales-menu"
RAW="$EVID/raw"
mkdir -p "$RAW"
export PGPASSWORD=alun
DB=${SO25_DB:-cloudpivot_so25}
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }

PASS=0; FAIL=0; FAILED=()
ok()  { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$*"); printf '  ❌ %s\n' "$*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

# api <名> <METHOD> <path> [token] [json] → 设置 HTTP / BODY / CODE
api() {
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  # 用 curl 的 --oauth2-bearer 而不是手写 "Authorization: Bearer <token>" 头：
  # 本仓的密钥扫描/脱敏会把「Bearer + 变量」这种字面串改写成 "Bearer ***"
  # （so24_e2e.sh 里就是这么被改坏的 —— 那份脚本现在发出去的 token 是三个星号）。
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out")
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  # JWT 脱敏：落盘前把 access_token / refresh_token 抹成 <redacted>
  if printf '%s' "$BODY" | jq -e '.data.access_token? // .data.refresh_token?' >/dev/null 2>&1; then
    jq '(.data.access_token? // "") |= (if . == "" then . else "<redacted>" end)
        | (.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end)' \
       "$out" > "$out.tmp" && mv "$out.tmp" "$out"
  fi
}
d() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

login() { # login <user> <pass> <tag> → 设置全局 TOKEN（**不 echo**：echo 会被 $( ) 捕获，
  # 把「✅ 登录成功」这行一起塞进 token 变量 ⇒ Authorization 头里带换行 ⇒ hyper 直接 400 空响应）
  api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
  TOKEN=""
  if [ "$HTTP" != "200" ] || [ "$CODE" != "0" ]; then
    bad "登录 $1 http=$HTTP body=$(printf '%s' "$BODY" | head -c 200)"; return
  fi
  ok "登录 $1 成功（token 已脱敏落盘）"
  TOKEN=$(printf '%s' "$BODY" | jq -r '.data.access_token // empty')
}

mkuser() { # mkuser <admin_token> <username> <pass> <role_ids_json> <realname>
  # 用 jq 组 JSON（手写转义 + 折行在实测里会送出不合法请求体 → 400 空响应）
  local body
  body=$(jq -nc --arg u "$2" --arg p "$3" --arg n "$5" --argjson r "$4" \
          '{username:$u, password:$p, real_name:$n, role_ids:$r, status:1, remark:"SO-25 验收夹具"}')
  api "user_$2" POST /system/user "$1" "$body"
  if [ "$CODE" = "0" ]; then ok "建号 $2"; else bad "建号 $2 http=$HTTP body=$(printf '%s' "$BODY" | head -c 300)"; fi
}

# 侧边栏菜单 → 「销售管理」子树（标题用 · 拼）
sales_tree() { # sales_tree <menus.json>
  jq -r '[.. | objects | select(.title? == "销售管理")][0]
         | if . == null then "<无销售管理>" else (.children // [] | map(.title) | join(" · ")) end' "$1" 2>/dev/null
}
sales_count() { # sales_count <menus.json> → 销售管理下子节点数
  jq -r '[.. | objects | select(.title? == "销售管理")][0] | (.children // []) | length' "$1" 2>/dev/null
}

echo "=================================================================="
echo "SO-25 验收：库=$DB 实例=$BASE"
echo "=================================================================="

echo; echo "== 0) 实例与迁移记账 =="
health=$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$PORT/health")
check "/health" "$health" "200"
mig=$(PSQLQ -c "select applied_via||'/'||kind||'/'||success from schema_migrations where name='sales_menu'")
check "schema_migrations 的 V0267 记账（applied_via/kind/success）" "$mig" "executed/versioned/true"
badvia=$(PSQLQ -c "select count(*) from schema_migrations where applied_via not in ('executed','imported')")
check "applied_via 非 executed/imported 的迁移数" "$badvia" "0"

echo; echo "== 1) 菜单树（库侧，直查 sys_menu） =="
tree=$(PSQLQ -c "select string_agg(m.sort_order||'='||m.title, ' · ' order by m.sort_order)
                   from sys_menu m join sys_menu d on d.id = m.parent_id
                  where d.id='20000000-0000-0000-0000-000000002701'::uuid and m.deleted_at is null")
check "「销售管理」下的子页（sort=标题，按序）" \
      "$tree" "10=客户档案 · 20=销售订单 · 30=发货单 · 40=应收台账 · 50=收款单 · 60=销售发票"
dircode=$(PSQLQ -c "select coalesce(auth_code,'<null>')||'/'||menu_type||'/'||path from sys_menu where id='20000000-0000-0000-0000-000000002701'::uuid")
check "目录节点形状（auth_code/menu_type/path）" "$dircode" "/1//business/erp/sales-mgmt"

# 授权矩阵（库侧全量口径：码级计数）
echo; echo "== 2) 授权矩阵（库侧，码级 + 目录闭包） =="
while IFS='|' read -r role want; do
  got=$(PSQLQ -c "select count(distinct m.auth_code) from sys_role_menu rm
                    join sys_role r on r.id=rm.role_id and r.role_code='$role'
                    join sys_menu m on m.id=rm.menu_id
                   where m.deleted_at is null and m.auth_code like 'business:erp:%'
                     and split_part(m.auth_code,':',3) in ('customer','sale-order','shipment','receivable','receipt','invoice')")
  dir=$(PSQLQ -c "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.role_code='$role'
                   where rm.menu_id='20000000-0000-0000-0000-000000002701'::uuid")
  check "$role 销售码数" "$got" "$want"
  check "$role 持有「销售管理」目录（侧边栏祖先闭包）" "$dir" "1"
done <<'ROLES'
SALES_SUPERVISOR|12
SALES_CLERK|4
WAREHOUSE_MANAGER|4
FINANCE_MANAGER|17
GENERAL_MANAGER|10
ROLES

echo; echo "== 3) 真 token 登录 + 菜单子集（GET /api/system/auth/menus） =="
# 模板库出自 SO-24 的 e2e 库 ⇒ admin 口令可能已被上一张卡改过（Admin@So24!2026）。
# 这里按「候选口令」尝试，改为本卡口令后再统一用 Admin@So25!2026。
try_login() { # try_login <user> <pass> [tag] → token（失败回空，不打 PASS/FAIL）
  local out="$RAW/login_${3:-try}.json"
  local code tok
  code=$(curl -s -m 20 -o "$out" -w '%{http_code}' -X POST "$BASE/system/auth/login" \
           -H 'Content-Type: application/json' \
           -d "{\"username\":\"$1\",\"password\":\"$2\"}")
  [ "$code" = "200" ] || { printf ''; return; }
  tok=$(jq -r '.data.access_token // empty' "$out")
  # 先把 token 取出来，再脱敏落盘（顺序反了会把 "<redacted>" 当 token 用）
  jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
      | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
     "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  printf '%s' "$tok"
}

T_ADMIN=$(try_login admin 'Admin@123456' admin_first)
FIRST_PWD='Admin@123456'
if [ -z "${T_ADMIN:-}" ]; then
  T_ADMIN=$(try_login admin 'Admin@So24!2026' admin_so24)
  FIRST_PWD='Admin@So24!2026'
fi
if [ -z "${T_ADMIN:-}" ]; then echo "FATAL: admin 口令既不是 Admin@123456 也不是 Admin@So24!2026，无法继续"; exit 1; fi
ok "admin 登录成功（模板库口令 = $FIRST_PWD）"
echo "  · 统一下发本卡口令：走真实改密接口"
api pre_pwd_change PUT /system/auth/password "$T_ADMIN" "{\"old_password\":\"$FIRST_PWD\",\"new_password\":\"Admin@So25!2026\"}"
if [ "$CODE" = "0" ]; then ok "前置：admin 改密 → Admin@So25!2026"; else bad "前置改密 http=$HTTP body=$(printf '%s' "$BODY" | head -c 200)"; fi
login admin 'Admin@So25!2026' admin; T_ADMIN=$TOKEN
[ -z "${T_ADMIN:-}" ] && { echo "FATAL: admin 改密后登录失败"; exit 1; }

R_SUP=$(PSQLQ -c "select id from sys_role where role_code='SALES_SUPERVISOR'")
R_CLK=$(PSQLQ -c "select id from sys_role where role_code='SALES_CLERK'")
R_WH=$(PSQLQ -c "select id from sys_role where role_code='WAREHOUSE_MANAGER'")
R_FIN=$(PSQLQ -c "select id from sys_role where role_code='FINANCE_MANAGER'")
R_GM=$(PSQLQ -c "select id from sys_role where role_code='GENERAL_MANAGER'")

mkuser "$T_ADMIN" laoliu 'Laoliu@So25!' "[\"$R_SUP\"]" '刘销售（销售经理）'
mkuser "$T_ADMIN" so25_clerk 'So25!Clerk' "[\"$R_CLK\"]" 'SO25 销售员'
mkuser "$T_ADMIN" so25_wh 'So25!Wh01' "[\"$R_WH\"]" 'SO25 仓库主管'
mkuser "$T_ADMIN" so25_fin 'So25!Fin01' "[\"$R_FIN\"]" 'SO25 财务经理'
mkuser "$T_ADMIN" so25_gm 'So25!Gm001' "[\"$R_GM\"]" 'SO25 总经理'

login laoliu 'Laoliu@So25!' laoliu;      T_SUP=$TOKEN
login so25_clerk 'So25!Clerk' clerk;     T_CLK=$TOKEN
login so25_wh 'So25!Wh01' wh;           T_WH=$TOKEN
login so25_fin 'So25!Fin01' fin;        T_FIN=$TOKEN
login so25_gm 'So25!Gm001' gm;          T_GM=$TOKEN

for pair in "laoliu:$T_SUP:sup" "clerk:$T_CLK:clk" "wh:$T_WH:wh" "fin:$T_FIN:fin" "gm:$T_GM:gm"; do
  IFS=':' read -r who tok tag <<< "$pair"
  [ -z "$tok" ] && { bad "$who 无 token，跳过菜单断言"; continue; }
  api "menus_$tag" GET /system/auth/menus "$tok"
  check "$who GET /auth/menus http" "$HTTP" "200"
done

echo; echo "  --- laoliu（SALES_SUPERVISOR）看到销售菜单 ---"
n=$(sales_count "$RAW/menus_sup.json"); t=$(sales_tree "$RAW/menus_sup.json")
# §10.2 销售经理：客户读写 / 订单读写+提交 / 发货只读 / 应收只读，收款与发票为「—」
# ⇒ 侧边栏里应当**只有 4 个**子页（这正是「各角色权限正确」的证据，不是缺陷）
check "laoliu「销售管理」子页数" "$n" "4"
if [ "$t" = "客户档案 · 销售订单 · 发货单 · 应收台账" ]; then
  ok "laoliu 销售菜单 = $t（收款/发票按 §10.2 不可见）"
else
  bad "laoliu 销售菜单 = $t"
fi
echo; echo "  --- 其余角色（菜单子集必须与矩阵一致） ---"
check "销售员 子页数（客户+订单）"                  "$(sales_count "$RAW/menus_clk.json")" "2"
check "销售员 销售菜单"                              "$(sales_tree "$RAW/menus_clk.json")" "客户档案 · 销售订单"
check "仓库主管 子页数（订单+发货）"                "$(sales_count "$RAW/menus_wh.json")" "2"
check "仓库主管 销售菜单"                            "$(sales_tree "$RAW/menus_wh.json")" "销售订单 · 发货单"
check "财务经理 子页数（全线）"                      "$(sales_count "$RAW/menus_fin.json")" "6"
check "总经理 子页数（全线只读）"                    "$(sales_count "$RAW/menus_gm.json")" "6"

echo; echo "== 4) 权限落地（越权 403 / 有权 200，真接口实测） =="
while IFS='|' read -r who tok method path want desc; do
  [ -z "$tok" ] && { bad "$desc（无 token）"; continue; }
  api "perm_${desc//[^a-zA-Z0-9]/_}" "$method" "$path" "$tok"
  check "$desc（$who $method $path）HTTP" "$HTTP" "$want"
done <<EOF
laoliu|$T_SUP|GET|/business/erp/customer/page|200|销售经理看客户列表
laoliu|$T_SUP|GET|/business/erp/receivable/aging|200|销售经理看应收账龄
laoliu|$T_SUP|GET|/business/erp/receipt/page|403|销售经理无收款权限
clerk|$T_CLK|GET|/business/erp/sales-order/page|200|销售员看订单列表
clerk|$T_CLK|GET|/business/erp/customer/page|200|销售员客户只读
clerk|$T_CLK|GET|/business/erp/receivable/page|403|销售员无应收权限
wh|$T_WH|GET|/business/erp/shipment/page|200|仓库主管看发货单
wh|$T_WH|POST|/business/erp/customer|403|仓库主管无客户写权限
fin|$T_FIN|GET|/business/erp/invoice/page|200|财务经理看发票列表
fin|$T_FIN|GET|/business/erp/shipment/page|200|财务经理发货只读
gm|$T_GM|GET|/business/erp/receivable/page|200|总经理应收只读
gm|$T_GM|POST|/business/erp/customer|403|总经理无客户写权限
EOF

echo; echo "== 5) 前端路由（sys_menu.path → router/index.ts 手工比对） =="
for p in /business/erp/customer /business/erp/sales-order /business/erp/shipment \
         /business/erp/receivable /business/erp/receipt /business/erp/invoice; do
  if grep -q "path: '${p#/}'" "$ROOT/frontend/src/router/index.ts"; then ok "路由已注册 $p"; else bad "路由缺失 $p"; fi
done
npath=$(PSQLQ -c "select count(*) from sys_menu where parent_id='20000000-0000-0000-0000-000000002701'::uuid and menu_type=2 and path is not null")
check "销售管理下有 path 的页面数" "$npath" "6"

echo
echo "=================================================================="
echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then printf '失败项：\n'; printf '  - %s\n' "${FAILED[@]}"; echo "结果：FAIL"; exit 1; fi
echo "结果：PASS"
