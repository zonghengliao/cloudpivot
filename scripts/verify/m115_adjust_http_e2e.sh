#!/usr/bin/env bash
# =============================================================================
# m115_adjust_http_e2e.sh —— M1-15 间接法调整项配置五接口**真 HTTP** 实测
#
# 卡：t_ce938a2a（M1-15【M1·财务】现金流底稿：间接法调整项配置）
#
# 为什么还要这一层：cargo 单测证明的是**函数**，引擎实测证明的是**迁移**。
# 但路由有没有真的被 `scan()` 注册、`#[nik::permission]` 有没有真的拦住、
# JSON 出参字段名对不对 —— 只有打真 HTTP 才知道（宏写错 method 就 405，
# 权限码写错就 403，出参改名前端就白屏，编译都不会报）。
#
# 做的事（全部对临时 e2e 库，跑完就删）：
#   1. 克隆模板库 → 应用 V0414 / V0415 / V0416
#   2. 真启动后端（隔离端口 18094），admin 登录拿 token
#   3. 未带 token → 401（鉴权闸真的在）
#   4. GET  list    → 200：12 行模板 / 2 行 MANUAL / 4 个口径 / 2 个固定行 /
#                    命中自检（1602 命中 1，1231+1471 命中 0 且行级告警点名）
#   5. GET  catalog → 200：4 口径 + 2 来源 + 4 条模式语法 + 结构式
#   6. POST create  → 200（编码归一化）；重复码 409；同口径 409；MANUAL 带口径 400；坏码 400
#   7. PUT  update  → 200（改名生效）；改编码 400（编码不可变）
#   8. PUT  toggle  → 200（停用）；active_only=true 里看不到，全量里还在
#   9. DB 对账       → 库里 13 行（12 模板 + 1 用例）、用例行字段与接口一致
#
# 用法：bash scripts/verify/m115_adjust_http_e2e.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB="cp_m115_adjust_e2e"
PORT="${M115_HTTP_PORT:-18094}"
BASE="http://127.0.0.1:$PORT"

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M115_HTTP_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m115-http}"
mkdir -p "$RUN_DIR"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

psql_admin() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q_db()       { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -tAc "$1"; }

[[ -x "$BIN" ]] || { echo "FATAL: 缺 $BIN（cd backend && cargo build --bin cloudpivot）"; exit 2; }

JWT_FILE="$RUN_DIR/jwt_secret"
MASTER_FILE="$RUN_DIR/storage_master_key"
[[ -s "$JWT_FILE" ]]    || openssl rand -hex 32 > "$JWT_FILE"
[[ -s "$MASTER_FILE" ]] || openssl rand -hex 32 > "$MASTER_FILE"

start_backend() {
    (
        cd "$BACKEND_DIR" || exit 1
        export NIK_PROFILE=test
        export NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT=5432
        export NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER="$PG_USER"
        export NIK_DATABASE_PASSWORD="$PGPASSWORD" NIK_DATABASE_SSL_MODE=prefer
        export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
        export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(cat "$JWT_FILE")"
        export CLOUDPIVOT_STORAGE_MASTER_KEY="$(cat "$MASTER_FILE")"
        export X_CONFIG_SECRET_KEY="m115-http-config-secret"
        export NIK_ADMIN_PASSWORD='M115@Drill#2026'
        exec "$BIN"
    ) >"$RUN_DIR/backend.log" 2>&1 &
    echo $! > "$RUN_DIR/backend.pid"
    for _ in $(seq 1 120); do
        curl -fsS -m 2 "$BASE/health" >/dev/null 2>&1 && return 0
        sleep 1
    done
    tail -40 "$RUN_DIR/backend.log"
    return 1
}

stop_backend() {
    [[ -f "$RUN_DIR/backend.pid" ]] || return 0
    local pid; pid="$(cat "$RUN_DIR/backend.pid")"
    kill "$pid" 2>/dev/null
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    kill -9 "$pid" 2>/dev/null
    rm -f "$RUN_DIR/backend.pid"
    for _ in $(seq 1 20); do curl -fsS -m 1 "$BASE/health" >/dev/null 2>&1 || break; sleep 1; done
}

cleanup() {
    stop_backend
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
}
trap cleanup EXIT

# 打一个请求：METHOD PATH [BODY] → 输出「响应体 + 换行 + HTTP码」
call() {
    local method="$1" path="$2" body="${3:-}"
    if [ -n "$body" ]; then
        curl -sS -m 20 -X "$method" "$BASE$path" \
             -H "Content-Type: application/json" -H "Authorization: Bearer $AT" \
             -d "$body" -w '\n%{http_code}'
    else
        curl -sS -m 20 -X "$method" "$BASE$path" \
             -H "Authorization: Bearer $AT" -w '\n%{http_code}'
    fi
}

# 断言：响应体里的 jq 表达式等于期望值
assert_jq() { # 期望 响应体 表达式 说明
    local expect="$1" body="$2" expr="$3" label="$4" actual
    actual="$(printf '%s' "$body" | jq -r "$expr" 2>/dev/null)"
    if [ "$actual" = "$expect" ]; then ok "$label（$expr = $actual）"; else
        printf '%s' "$body" > "$RUN_DIR/last_body.json"
        bad "$label（$expr 期望 $expect，实际 [$actual]）"
    fi
}

echo "== M1-15 间接法调整项配置五接口 真 HTTP 实测 =="
echo "   PG=${PG_HOST}:5432 库=${DB} 端口=${PORT}"
echo

# ============================================================================
echo "== 0) 准备库 =="
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null 2>&1 || {
    echo "  模板库被占用，5 秒后重试"; sleep 5
    psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null || { echo "FATAL: 克隆失败"; exit 2; }
}
for f in V0414__cashflow_worksheet.up V0415__cashflow_worksheet_perm.up V0416__cashflow_adjust_item.up; do
    psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -q -v ON_ERROR_STOP=1 \
         -f "$BACKEND_DIR/app/migrations/business/$f.sql" >/dev/null 2>&1 \
        && ok "$f 已应用" || bad "$f 应用失败"
done

# 模板库里的 admin 口令是历史快照，这里把用户名腾出来，让启动钩子用
# NIK_ADMIN_PASSWORD 重新种一个可登录的超管（改名而非删除：sys_user 被十几张表外键引用）。
q_db "update sys_user set username='admin_pre_m115' where username='admin'" >/dev/null
ok "腾出 admin 用户名（改名 admin_pre_m115）"

# ============================================================================
echo
echo "== 1) 启动 + 登录 =="
start_backend || { echo "FATAL: 后端没起来"; exit 2; }
ok "后端真启动成功（$BASE/health 200）"

LOGIN="$(curl -sS -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"M115@Drill#2026"}')"
AT="$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')"
[[ -n "$AT" ]] && ok "admin 登录取到 access_token" || { bad "登录失败：$(printf '%s' "$LOGIN" | head -c 300)"; echo; echo "== 汇总：PASS=$PASS FAIL=$FAIL =="; exit 1; }

# 首登强制改密（CODE-06），改完重新登录
CHG="$(curl -sS -m 20 -X PUT "$BASE/api/system/auth/password" \
    -H 'Content-Type: application/json' -H "Authorization: Bearer $AT" \
    -d '{"old_password":"M115@Drill#2026","new_password":"M115@Ready#2026"}')"
[[ "$(printf '%s' "$CHG" | jq -r '.msg // ""')" == "密码已修改" ]] \
    && ok "首登强制改密完成" || bad "改密失败：$(printf '%s' "$CHG" | head -c 300)"
LOGIN="$(curl -sS -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"M115@Ready#2026"}')"
AT="$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')"
[[ -n "$AT" ]] && ok "用新口令重新登录成功" || { bad "重新登录失败"; echo; echo "== 汇总：PASS=$PASS FAIL=$FAIL =="; exit 1; }

NOAUTH_CODE="$(curl -sS -m 20 -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/report/cash-flow/adjust-item/list")"
[[ "$NOAUTH_CODE" == "401" ]] && ok "未带 token → 401（鉴权闸真的在）" || bad "未带 token 返回 $NOAUTH_CODE（应为 401）"

# ============================================================================
echo
echo "== 2) GET list（12 行模板 + 取数自检）=="
RAW="$(call GET '/api/business/erp/report/cash-flow/adjust-item/list')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "GET list 200" || bad "GET list 返回 $CODE：$(printf '%s' "$BODY" | head -c 400)"
assert_jq "12" "$BODY" '.data.total' "总行数 12（标准模板）"
assert_jq "12" "$BODY" '.data.active_total' "启用 12 行"
assert_jq "12" "$BODY" '[.data.items[] | select(.item_code | startswith("ADJ_"))] | length' "12 行都是 ADJ_ 编码"
assert_jq "2" "$BODY" '[.data.items[] | select(.source_type == "MANUAL")] | length' "2 行手工填列"
assert_jq "4" "$BODY" '[.data.items[] | select(.calc_basis != null) | .calc_basis] | unique | length' "4 个取数口径"
assert_jq "2" "$BODY" '[.data.anchors[]] | length' "两个固定行（净利润 / 小计）"
assert_jq "START" "$BODY" '.data.anchors[0].role' "第一个固定行是起点"
assert_jq "SUBTOTAL" "$BODY" '.data.anchors[1].role' "第二个固定行是小计"
assert_jq "true" "$BODY" '.data.formula | contains("净利润")' "结构式含净利润"
assert_jq "true" "$BODY" '.data.formula | contains("经营活动产生的现金流量净额")' "结构式含小计"
assert_jq "true" "$BODY" '.data.account_total > 50' "科目目录已加载（命中数分母）"
assert_jq "科目取数" "$BODY" '[.data.items[] | select(.item_code=="ADJ_DEPRECIATION")][0].source_label' "来源中文名"
assert_jq "贷方净额（贷 − 借）" "$BODY" '[.data.items[] | select(.item_code=="ADJ_DEPRECIATION")][0].basis_label' "口径中文名"
assert_jq "1" "$BODY" '[.data.items[] | select(.item_code=="ADJ_DEPRECIATION")][0].matched_account_count' "1602 命中 1 个科目"
assert_jq "1602" "$BODY" '[.data.items[] | select(.item_code=="ADJ_DEPRECIATION")][0].matched_accounts[0].account_code' "命中科目就是 1602"
assert_jq "0" "$BODY" '[.data.items[] | select(.item_code=="ADJ_ASSET_IMPAIRMENT")][0].matched_account_count' \
    "1231/1471 当前 0 命中（科目表还没建这俩）"
assert_jq "true" "$BODY" '[.data.items[] | select(.item_code=="ADJ_ASSET_IMPAIRMENT")][0].warnings | length > 0' \
    "0 命中的行必须被点名（不许静默恒为 0）"
assert_jq "null" "$BODY" '[.data.items[] | select(.item_code=="ADJ_OTHER")][0].matched_account_count' \
    "MANUAL 行没有命中概念（null）"

# ============================================================================
echo
echo "== 3) GET catalog（字典）=="
RAW="$(call GET '/api/business/erp/report/cash-flow/adjust-item/catalog')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "GET catalog 200" || bad "GET catalog 返回 $CODE：$(printf '%s' "$BODY" | head -c 400)"
assert_jq "2" "$BODY" '[.data.source_types[]] | length' "2 个取数来源"
assert_jq "4" "$BODY" '[.data.calc_bases[]] | length' "4 个取数口径"
assert_jq "CREDIT_NET,DEBIT_NET,BALANCE_INCREASE,BALANCE_DECREASE" "$BODY" '[.data.calc_bases[].value] | join(",")' "口径顺序与常量一致"
assert_jq "4" "$BODY" '[.data.pattern_tips[]] | length' "4 条模式语法提示"
assert_jq "12" "$BODY" '.data.total' "字典里回显当前行数"
assert_jq "true" "$BODY" '.data.formula | contains("Σ")' "结构式带求和符号"

# ============================================================================
echo
echo "== 4) POST create（新增 + 五道校验）=="
RAW="$(call POST '/api/business/erp/report/cash-flow/adjust-item' '{
  "item_code":" m115_sales_expense ",
  "item_name":"销售费用（用例）",
  "calc_basis":"debit_net",
  "account_pattern":"5601",
  "sort_order":130,
  "remark":"用例行"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "POST 新增 200" || bad "POST 新增返回 $CODE：$(printf '%s' "$BODY" | head -c 400)"
assert_jq "M115_SALES_EXPENSE" "$BODY" '.data.item.item_code' "编码归一化成大写"
assert_jq "DEBIT_NET" "$BODY" '.data.item.calc_basis' "口径归一化成大写"
assert_jq "1" "$BODY" '.data.item.matched_account_count' "5601 命中 1 个科目"
assert_jq "销售费用（用例）" "$BODY" '.data.item.item_name' "名称落库"
NEW_ID="$(printf '%s' "$BODY" | jq -r '.data.item.id')"

RAW="$(call POST '/api/business/erp/report/cash-flow/adjust-item' '{
  "item_code":"M115_SALES_EXPENSE","item_name":"重复码","calc_basis":"DEBIT_NET","account_pattern":"5602"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "409" ]] && ok "重复编码 → 409" || bad "重复编码返回 $CODE（应为 409）"
assert_jq "true" "$BODY" '.msg | contains("已存在")' "报文说编码已存在"

RAW="$(call POST '/api/business/erp/report/cash-flow/adjust-item' '{
  "item_code":"M115_DUP_BASIS","item_name":"同口径","calc_basis":"DEBIT_NET","account_pattern":"5601"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "409" ]] && ok "同模式同口径 → 409（同一批科目会被算两遍）" || bad "同口径返回 $CODE（应为 409）"
assert_jq "true" "$BODY" '.msg | contains("取数口径重复")' "报文说口径重复"

RAW="$(call POST '/api/business/erp/report/cash-flow/adjust-item' '{
  "item_code":"M115_MANUAL_BAD","item_name":"手工坏行","source_type":"MANUAL","calc_basis":"CREDIT_NET"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "MANUAL 带口径 → 400（不许留摆设字段）" || bad "MANUAL 带口径返回 $CODE（应为 400）"
assert_jq "10001" "$BODY" '.code' "错误码 10001（参数非法）"

RAW="$(call POST '/api/business/erp/report/cash-flow/adjust-item' '{
  "item_code":"1BAD","item_name":"坏码","calc_basis":"CREDIT_NET","account_pattern":"1602"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "非法编码 → 400" || bad "非法编码返回 $CODE（应为 400）"
assert_jq "true" "$BODY" '.msg | contains("大写字母开头")' "报文点名编码规则"

RAW="$(call POST '/api/business/erp/report/cash-flow/adjust-item' '{
  "item_code":"M115_BAD_PATTERN","item_name":"坏模式","calc_basis":"CREDIT_NET","account_pattern":"1602,,5601"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "坏模式（空项）→ 400" || bad "坏模式返回 $CODE（应为 400）"

RAW="$(call POST '/api/business/erp/report/cash-flow/adjust-item' '{
  "item_code":"M115_MANUAL_OK","item_name":"手工行（用例）","source_type":"MANUAL"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "MANUAL 行（不带口径）→ 200" || bad "MANUAL 合法行返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "null" "$BODY" '.data.item.calc_basis' "MANUAL 行口径为 null"
assert_jq "null" "$BODY" '.data.item.matched_account_count' "MANUAL 行无命中数"
MANUAL_ID="$(printf '%s' "$BODY" | jq -r '.data.item.id')"

# ============================================================================
echo
echo "== 5) PUT update（改名 / 编码不可变 / 改类型）=="
RAW="$(call PUT "/api/business/erp/report/cash-flow/adjust-item/$MANUAL_ID" '{
  "item_name":"手工行（改名后）","remark":"改成手工说明"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "PUT 改名 200" || bad "PUT 改名返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "手工行（改名后）" "$BODY" '.data.item.item_name' "名称已更新"
assert_jq "改成手工说明" "$BODY" '.data.item.remark' "备注已更新"

RAW="$(call PUT "/api/business/erp/report/cash-flow/adjust-item/$MANUAL_ID" '{"item_code":"M115_RENAMED"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "改编码 → 400（编码不可变）" || bad "改编码返回 $CODE（应为 400）"
assert_jq "true" "$BODY" '.msg | contains("编码不可修改")' "报文说编码不可修改"

# MANUAL → ACCOUNT：必须同时给出口径与模式（否则行形状不成立）
RAW="$(call PUT "/api/business/erp/report/cash-flow/adjust-item/$MANUAL_ID" '{"source_type":"ACCOUNT"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"
[[ "$CODE" == "400" ]] && ok "MANUAL 改 ACCOUNT 但不给口径 → 400" || bad "改类型返回 $CODE（应为 400）"

RAW="$(call PUT "/api/business/erp/report/cash-flow/adjust-item/$MANUAL_ID" '{
  "source_type":"ACCOUNT","calc_basis":"CREDIT_NET","account_pattern":"5602"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "MANUAL 改 ACCOUNT（带口径模式）→ 200" || bad "改类型返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "ACCOUNT" "$BODY" '.data.item.source_type' "来源已改为 ACCOUNT"
assert_jq "12" "$BODY" '.data.item.matched_account_count' "5602 命中 12 个（含 5602.01~.12 下级：口径是「科目 + 全部下级」）"

# ============================================================================
echo
echo "== 6) PUT toggle（启停）=="
RAW="$(call PUT "/api/business/erp/report/cash-flow/adjust-item/$NEW_ID/toggle" '{"is_active":false}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "PUT toggle 停用 200" || bad "toggle 返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "false" "$BODY" '.data.is_active' "已停用"

RAW="$(call GET '/api/business/erp/report/cash-flow/adjust-item/list?active_only=true')"
BODY="$(printf '%s' "$RAW" | sed '$d')"
assert_jq "0" "$BODY" '[.data.items[] | select(.item_code=="M115_SALES_EXPENSE")] | length' "停用行不出现在 active_only 列表"
assert_jq "14" "$BODY" '.data.total' "总数不随筛选变（12 模板 + 2 用例行）"

RAW="$(call GET '/api/business/erp/report/cash-flow/adjust-item/list')"
BODY="$(printf '%s' "$RAW" | sed '$d')"
assert_jq "1" "$BODY" '[.data.items[] | select(.item_code=="M115_SALES_EXPENSE")] | length' "全量列表里停用行还在（可以改回来）"
assert_jq "13" "$BODY" '.data.active_total' "启用行数 13（12 模板 + 1 手工改的 ACCOUNT）"

RAW="$(call PUT "/api/business/erp/report/cash-flow/adjust-item/$NEW_ID/toggle" '{"is_active":true}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "PUT toggle 重新启用 200" || bad "toggle 返回 $CODE"
assert_jq "true" "$BODY" '.data.is_active' "已重新启用"

# 不存在的 id → 404
RAW="$(call PUT "/api/business/erp/report/cash-flow/adjust-item/00000000-0000-0000-0000-000000000000/toggle" '{"is_active":true}')"
CODE="$(printf '%s' "$RAW" | tail -1)"
[[ "$CODE" == "404" ]] && ok "不存在的 id → 404" || bad "不存在 id 返回 $CODE（应为 404）"

# ============================================================================
echo
echo "== 7) DB 对账 =="
[[ "$(q_db "select count(*) from biz_cashflow_adjust_item")" == "14" ]] \
    && ok "库里 14 行（12 模板 + 2 用例）" || bad "库里行数 = $(q_db "select count(*) from biz_cashflow_adjust_item")（应为 14）"
[[ "$(q_db "select count(*) from biz_cashflow_adjust_item where source_type='MANUAL' and (calc_basis is not null or account_pattern is not null)")" == "0" ]] \
    && ok "库里没有「MANUAL 带口径」的脏行" || bad "库里有 MANUAL 脏行"
[[ "$(q_db "select item_name from biz_cashflow_adjust_item where item_code='M115_SALES_EXPENSE'")" == "销售费用（用例）" ]] \
    && ok "接口写入的名称与库里一致" || bad "库里名称不一致"
[[ "$(q_db "select count(*) from sys_menu where auth_code like 'business:erp:cashflow-adjust:%' and deleted_at is null")" == "2" ]] \
    && ok "两个权限码在菜单表里" || bad "权限码不在菜单表"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
