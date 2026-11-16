#!/usr/bin/env bash
# =============================================================================
# m114_worksheet_http_e2e.sh —— M1-14 现金流底稿三接口**真 HTTP** 实测
#
# 卡：t_9e14a327（M1-14 现金流底稿：录入 + 勾稽校验 · 直接法）
#
# 为什么还要这一层：cargo 单测证明的是**函数**，引擎实测证明的是**迁移**。
# 但路由有没有真的被 `scan()` 注册、`#[nik::permission]` 有没有真的拦住、
# JSON 出参字段名对不对 —— 只有打真 HTTP 才知道（宏写错 method 就 405，
# 权限码写错就 403，出参改名前端就白屏，编译都不会报）。
#
# 做的事（全部对临时 e2e 库，跑完就删）：
#   1. 克隆模板库 → 应用 V0414 / V0415 → 造两个期间（2026-11 OPEN / 2026-12 CLOSED）
#   2. 真启动后端（隔离端口 18092），admin 登录拿 token
#   3. 未带 token 请求 → 401（闸真的在）
#   4. GET  底稿（2026-11）→ 200：12 个配置项都在、entered 全 false、不闭合且点名缺期初/期末
#   5. PUT  录入（期初 500 + 流入 1200 + 流出 700 + 期末 1000）→ 200：勾稽闭合 diff=0
#   6. GET  回读 → 落库了（entered=true、金额对得上）
#   7. POST 校验（草稿期末 999 + require_balanced）→ 400 + 66087（不写库）
#   8. PUT  到已结账期间 2026-12 → 400 + 66085（期间闸）
#   9. PUT  不存在的项目 → 400 + 66086（项目闸）
#  10. PUT  负数金额 → 400 + 10001（金额闸）
#
# 用法：bash scripts/verify/m114_worksheet_http_e2e.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB="cp_m114_worksheet_e2e"
PORT="${M114_HTTP_PORT:-18092}"
BASE="http://127.0.0.1:$PORT"

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M114_HTTP_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m114-http}"
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
        export X_CONFIG_SECRET_KEY="m114-http-config-secret"
        export NIK_ADMIN_PASSWORD='M114@Drill#2026'
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

# 打一个请求：METHOD PATH [BODY] → 输出 "HTTP码<TAB>响应体"
call() {
    local method="$1" path="$2" body="${3:-}"
    if [ -n "$body" ]; then
        curl -sS -m 20 -X "$method" "$BASE$path" \
             -H "Content-Type: application/json" -H "Authorization: Bearer $TOKEN" \
             -d "$body" -w '\n%{http_code}'
    else
        curl -sS -m 20 -X "$method" "$BASE$path" \
             -H "Authorization: Bearer $TOKEN" -w '\n%{http_code}'
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

echo "== M1-14 现金流底稿三接口 真 HTTP 实测 =="
echo "   PG=${PG_HOST}:5432 库=${DB} 端口=${PORT}"
echo

# ============================================================================
echo "== 0) 准备库 =="
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null 2>&1 || {
    echo "  模板库被占用，5 秒后重试"; sleep 5
    psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null || { echo "FATAL: 克隆失败"; exit 2; }
}
psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -q -v ON_ERROR_STOP=1 \
     -f "$BACKEND_DIR/app/migrations/business/V0414__cashflow_worksheet.up.sql" >/dev/null 2>&1 \
     && ok "V0414 已应用（底稿表）" || bad "V0414 应用失败"
psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -q -v ON_ERROR_STOP=1 \
     -f "$BACKEND_DIR/app/migrations/business/V0415__cashflow_worksheet_perm.up.sql" >/dev/null 2>&1 \
     && ok "V0415 已应用（两个权限码）" || bad "V0415 应用失败"
q_db "delete from biz_accounting_period where period_code in ('2026-11','2026-12')" >/dev/null
q_db "insert into biz_accounting_period (id, period_year, period_month, period_code, start_date, end_date, status)
      values (gen_random_uuid(), 2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'OPEN'),
             (gen_random_uuid(), 2026, 12, '2026-12', '2026-12-01', '2026-12-31', 'CLOSED')" >/dev/null \
     && ok "期间夹具就位（2026-11 OPEN / 2026-12 CLOSED）" || bad "期间夹具失败"

# 模板库里的 admin 口令是历史快照（不是卡面口径的 Admin@123456），这里把它改名腾出
# `admin` 这个用户名，让启动钩子用 NIK_ADMIN_PASSWORD 重新种一个可登录的超管
# （改名而不是删除：sys_user 被 biz_task / biz_flow_* 等 19 张表外键引用，删不掉也不该删）。
q_db "update sys_user set username='admin_pre_m114' where username='admin'" >/dev/null
ok "腾出 admin 用户名（改名 admin_pre_m114，外键引用保持完好）"

# ============================================================================
echo
echo "== 1) 启动 + 登录 =="
start_backend || { echo "FATAL: 后端没起来"; exit 2; }
ok "后端真启动成功（$BASE/health 200）"

LOGIN="$(curl -sS -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"M114@Drill#2026"}')"
TOKEN="$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')"
[[ -n "$TOKEN" ]] && ok "admin 登录取到 access_token" || { bad "登录失败：$(printf '%s' "$LOGIN" | head -c 300)"; echo; echo "== 汇总：PASS=$PASS FAIL=$FAIL =="; exit 1; }

# 启动钩子新建的超管带 must_change_password=true（CODE-06：除改密/登出外全部 403），
# 这里先改密再重新登录 —— 顺带证明「首登强制改密」闸在真 HTTP 上也真的生效。
CHG="$(curl -sS -m 20 -X PUT "$BASE/api/system/auth/password" \
    -H 'Content-Type: application/json' -H "Authorization: Bearer $TOKEN" \
    -d '{"old_password":"M114@Drill#2026","new_password":"M114@Ready#2026"}')"
[[ "$(printf '%s' "$CHG" | jq -r '.msg // ""')" == "密码已修改" ]] \
    && ok "首登强制改密完成（改密后旧令牌作废）" || bad "改密失败：$(printf '%s' "$CHG" | head -c 300)"
LOGIN="$(curl -sS -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"M114@Ready#2026"}')"
TOKEN="$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')"
[[ -n "$TOKEN" ]] && ok "用新口令重新登录成功" || { bad "重新登录失败：$(printf '%s' "$LOGIN" | head -c 300)"; echo; echo "== 汇总：PASS=$PASS FAIL=$FAIL =="; exit 1; }

NOAUTH_CODE="$(curl -sS -m 20 -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/report/cash-flow/worksheet?period=2026-11")"
[[ "$NOAUTH_CODE" == "401" ]] && ok "未带 token → 401（鉴权闸真的在）" || bad "未带 token 返回 $NOAUTH_CODE（应为 401）"

# ============================================================================
echo
echo "== 2) GET 底稿（未录入状态）=="
RAW="$(call GET '/api/business/erp/report/cash-flow/worksheet?period=2026-11')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "GET 200" || bad "GET 返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "2026-11" "$BODY" '.data.period' "期间回显"
assert_jq "OPEN" "$BODY" '.data.period_status' "期间状态"
assert_jq "true" "$BODY" '.data.editable' "打开期间可编辑"
assert_jq "12" "$BODY" '[.data.rows[]] | length' "12 个配置项全部列出"
assert_jq "0" "$BODY" '[.data.rows[] | select(.entered)] | length' "一行都没录（entered 全 false）"
assert_jq "false" "$BODY" '.data.check.balanced' "未录入 ⇒ 不闭合"
assert_jq "true" "$BODY" '[.data.check.problems[].code] | index("OPENING_MISSING") != null' "点名缺期初"
assert_jq "true" "$BODY" '[.data.check.problems[].code] | index("CLOSING_MISSING") != null' "点名缺期末"
assert_jq "3" "$BODY" '[.data.activities[]] | length' "三大活动小计齐"

# ============================================================================
echo
echo "== 3) PUT 录入（期初 500 + 流入 1200 + 流出 700 + 期末 1000）=="
RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{
  "period":"2026-11",
  "opening":{"amount":"500.0000","remark":"上月结转"},
  "closing":{"amount":"1000.0000"},
  "items":[
    {"item_code":"OPER_SALES_RECEIPTS","amount":"1200.0000","remark":"11 月销售回款"},
    {"item_code":"OPER_PURCHASE_PAYMENTS","amount":"700.0000"}
  ]}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "PUT 200" || bad "PUT 返回 $CODE：$(printf '%s' "$BODY" | head -c 400)"
assert_jq "true" "$BODY" '.data.check.balanced' "勾稽闭合"
assert_jq "true" "$BODY" "((.data.check.diff|tonumber) == 0)" "差额为 0"
assert_jq "true" "$BODY" "((.data.totals.inflow|tonumber) == 1200)" "流入合计 1200"
assert_jq "true" "$BODY" "((.data.totals.outflow|tonumber) == 700)" "流出合计 700"
assert_jq "true" "$BODY" "((.data.totals.net|tonumber) == 500)" "净增加额 500"
assert_jq "true" "$BODY" "((.data.check.expected_closing|tonumber) == 1000)" "按勾稽式算出的期末 = 1000"
assert_jq "2" "$BODY" '[.data.rows[] | select(.entered)] | length' "两行已录入"
assert_jq "销售商品、提供劳务收到的现金" "$BODY" '[.data.rows[] | select(.item_code=="OPER_SALES_RECEIPTS")][0].item_name' "快照名取自配置"
assert_jq "INFLOW" "$BODY" '[.data.rows[] | select(.item_code=="OPER_SALES_RECEIPTS")][0].direction' "方向取自配置（不信客户端）"

# ============================================================================
echo
echo "== 4) GET 回读（落库了）=="
RAW="$(call GET '/api/business/erp/report/cash-flow/worksheet?period=2026-11')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "GET 200" || bad "GET 返回 $CODE"
assert_jq "true" "$BODY" '.data.check.balanced' "回读仍闭合"
assert_jq "true" "$BODY" "((.data.opening.amount|tonumber) == 500)" "期初余额落库 = 500"
assert_jq "true" "$BODY" "((.data.closing.amount|tonumber) == 1000)" "期末余额落库 = 1000"
assert_jq "上月结转" "$BODY" '.data.opening.remark' "备注落库"
[[ "$(q_db "select count(*) from biz_cashflow_worksheet where period_code='2026-11'")" == "4" ]] \
    && ok "库里 4 行（2 明细 + 期初 + 期末）" \
    || bad "库里行数 = $(q_db "select count(*) from biz_cashflow_worksheet where period_code='2026-11'")（应为 4）"

# 重复保存同一批：行数不涨（upsert 幂等）
RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{
  "period":"2026-11",
  "items":[{"item_code":"OPER_SALES_RECEIPTS","amount":"1200.0000"}]}')"
CODE="$(printf '%s' "$RAW" | tail -1)"
[[ "$(q_db "select count(*) from biz_cashflow_worksheet where period_code='2026-11'")" == "4" ]] \
    && ok "重复提交同一行后库里仍是 4 行（upsert 不攒重复，HTTP $CODE）" \
    || bad "重复提交后行数 = $(q_db "select count(*) from biz_cashflow_worksheet where period_code='2026-11'")"

# ============================================================================
echo
echo "== 5) POST 校验（不落库 + 不平报 66087）=="
RAW="$(call POST '/api/business/erp/report/cash-flow/worksheet/check' '{
  "period":"2026-11",
  "closing":{"amount":"999.0000"},
  "require_balanced":true}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "不平 + require_balanced → 400" || bad "返回 $CODE（应为 400）"
assert_jq "66087" "$BODY" '.code' "错误码 66087（勾稽不闭合）"
assert_jq "true" "$BODY" '.msg | contains("勾稽不闭合")' "报文说明勾稽不闭合"

RAW="$(call POST '/api/business/erp/report/cash-flow/worksheet/check' '{
  "period":"2026-11","closing":{"amount":"1000.0000"},"require_balanced":true}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "草稿闭合 → 200" || bad "返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "true" "$BODY" '.data.balanced' "校验结果闭合"
[[ "$(q_db "select count(*) from biz_cashflow_worksheet where period_code='2026-11'")" == "4" ]] \
    && ok "校验接口没写库（仍是 4 行）" || bad "校验接口写库了"

# ============================================================================
echo
echo "== 6) 三道闸的 HTTP 表现 =="
RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{"period":"2026-12","items":[{"item_code":"OPER_SALES_RECEIPTS","amount":"1"}]}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "已结账期间 → 400" || bad "返回 $CODE（应为 400）"
assert_jq "66085" "$BODY" '.code' "错误码 66085（期间不可编辑）"

RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{"period":"2026-11","items":[{"item_code":"NO_SUCH_ITEM","amount":"1"}]}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "不存在的项目 → 400" || bad "返回 $CODE（应为 400）"
assert_jq "66086" "$BODY" '.code' "错误码 66086（项目不可用）"

RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{"period":"2026-11","items":[{"item_code":"OPER_SALES_RECEIPTS","amount":"-1"}]}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "负数明细行 → 400" || bad "返回 $CODE（应为 400）"
assert_jq "10001" "$BODY" '.code' "错误码 10001（金额形状非法）"

RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{"period":"2026-11"}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "空请求 → 400" || bad "返回 $CODE（应为 400）"
assert_jq "10001" "$BODY" '.code' "错误码 10001（空请求）"

# 坏请求之后原数据没被动
[[ "$(q_db "select count(*) from biz_cashflow_worksheet where period_code='2026-11'")" == "4" ]] \
    && ok "被拒的请求一行都没写（仍是 4 行）" || bad "被拒的请求动了数据"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
