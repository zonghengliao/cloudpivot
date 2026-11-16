#!/usr/bin/env bash
# =============================================================================
# m116_statement_http_e2e.sh —— M1-16 由底稿生成现金流量表（真 HTTP + 真引擎实测）
#
# 卡：t_74e707ca（M1-16【M1·财务】现金流底稿：由底稿生成现金流量表）
#
# 为什么还要这一层：cargo 单测证明的是**函数**（合并 / 小计 / 勾稽 / 出表闸），
# 但路由有没有真的被 scan() 注册、`#[nik::permission]` 有没有真的拦住、JSON 字段名
# 对不对、xlsx 有没有真的产出 —— 只有打真 HTTP 才知道（宏写错 method 就 405，
# 权限码写错就 403，出参改名前端就白屏，编译都不会报）。
#
# 做的事（全部对临时 e2e 库，跑完就删）：
#   1. 克隆模板库（模板库只到 V0404）→ 真启动后端，让**迁移引擎**把 V0405..V0417 全应用
#   2. 断言引擎真的应用了 V0414 / V0415 / V0416 / V0417（schema_migrations 一行不少）
#   3. 重启一次 → 0 条新迁移（引擎幂等，V0417 不会被重放成重复菜单）
#   4. 造两个期间（1913-04 / 1913-05）+ 用 M1-14 的 PUT 接口录两张底稿
#   5. GET statement → 200：三栏（项目 / 本期 / 上期）、小计、净额、期初期末、
#                            勾稽闭合、上期期间、line_count、hide_zero 变少
#   6. GET statement/export → 200：xlsx 真文件（≥5KB，文件名带期间）
#   7. 把期末改坏 → GET 仍 200（balanced=false + diff=100），export → 400 + 66087
#   8. 期间不存在 → 400 + 66010；period 格式错 → 400 + 10001
#   9. 未带 token → 401（鉴权闸真的在）
#  10. DB 对账：sys_menu 两码在位、授权齐、底稿行数与接口一致
#
# 用法：bash scripts/verify/m116_statement_http_e2e.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB="cp_m116_stmt_e2e"
PORT="${M116_HTTP_PORT:-18095}"
BASE="http://127.0.0.1:$PORT"

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M116_HTTP_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m116-http}"
mkdir -p "$RUN_DIR"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

psql_admin() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q_db()       { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -tAc "$1"; }
exec_db()    { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -q -v ON_ERROR_STOP=1 -c "$1" >/dev/null 2>&1; }

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
        export X_CONFIG_SECRET_KEY="m116-http-config-secret"
        export NIK_ADMIN_PASSWORD='M116@Drill#2026'
        exec "$BIN"
    ) >"$RUN_DIR/backend.log" 2>&1 &
    echo $! > "$RUN_DIR/backend.pid"
    for _ in $(seq 1 180); do
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
        curl -sS -m 30 -X "$method" "$BASE$path" \
             -H "Content-Type: application/json" -H "Authorization: Bearer $AT" \
             -d "$body" -w '\n%{http_code}'
    else
        curl -sS -m 30 -X "$method" "$BASE$path" \
             -H "Authorization: Bearer $AT" -w '\n%{http_code}'
    fi
}

assert_jq() { # 期望 响应体 表达式 说明
    local expect="$1" body="$2" expr="$3" label="$4" actual
    actual="$(printf '%s' "$body" | jq -r "$expr" 2>/dev/null)"
    if [ "$actual" = "$expect" ]; then ok "$label（$expr = $actual）"; else
        printf '%s' "$body" > "$RUN_DIR/last_body.json"
        bad "$label（$expr 期望 $expect，实际 [$actual]）"
    fi
}

echo "== M1-16 由底稿生成现金流量表 · 真 HTTP 实测 =="
echo "   PG=${PG_HOST}:5432 库=${DB} 端口=${PORT}"
echo

# ============================================================================
echo "== 0) 准备库（模板只到 V0404，剩下交给真引擎）=="
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null 2>&1 || {
    echo "  模板库被占用，5 秒后重试"; sleep 5
    psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null || { echo "FATAL: 克隆失败"; exit 2; }
}
MIG_BEFORE="$(q_db "select count(*) from schema_migrations where kind='versioned' and success;")"
[[ -n "$MIG_BEFORE" ]] && ok "克隆完成（应用前 versioned 迁移 = $MIG_BEFORE）" || { echo "FATAL: 克隆库不可用"; exit 2; }
[[ "$(q_db "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" == "0" ]] \
    && ok "模板库里还没有底稿表（本卡前置迁移确实未应用）" || bad "模板库已有底稿表，前置假设不成立"

# 模板库里的 admin 口令是历史快照，把用户名腾出来让启动钩子重种一个可登录的超管
q_db "update sys_user set username='admin_pre_m116' where username='admin'" >/dev/null
ok "腾出 admin 用户名（改名 admin_pre_m116）"

# ============================================================================
echo
echo "== 1) 真启动（引擎应用 V0405..V0417）+ 迁移断言 =="
start_backend || { echo "FATAL: 后端没起来"; exit 2; }
ok "后端真启动成功（$BASE/health 200）"

MIG_AFTER="$(q_db "select count(*) from schema_migrations where kind='versioned' and success;")"
[[ "$MIG_AFTER" -gt "$MIG_BEFORE" ]] && ok "引擎应用了新迁移（$MIG_BEFORE → $MIG_AFTER）" \
    || bad "引擎没应用任何新迁移（仍是 $MIG_AFTER）"
assert_jq_plain() { if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 $1，实际 $2）"; fi; }
assert_jq_plain "414,415,416,417" \
    "$(q_db "select string_agg(version, ',' order by version::int) from schema_migrations where kind='versioned' and success and version in ('414','415','416','417');")" \
    "V0414 / V0415 / V0416 / V0417 四条都登记为已应用"
assert_jq_plain "1" "$(q_db "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" \
    "底稿表已建（V0414）"
assert_jq_plain "2" "$(q_db "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001810','20000000-0000-0000-0000-000000001820') and deleted_at is null;")" \
    "出表两码已在菜单表（V0417）"

echo
echo "== 1b) 重启一次 → 0 条新迁移（引擎幂等）=="
stop_backend
start_backend || { echo "FATAL: 重启失败"; exit 2; }
MIG_RESTART="$(q_db "select count(*) from schema_migrations where kind='versioned' and success;")"
assert_jq_plain "$MIG_AFTER" "$MIG_RESTART" "重启后迁移数不变（不重放）"
assert_jq_plain "2" "$(q_db "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001810','20000000-0000-0000-0000-000000001820') and deleted_at is null;")" \
    "重启后菜单不重复（仍 2 行）"

# ============================================================================
echo
echo "== 2) 登录 + 鉴权闸 =="
LOGIN="$(curl -sS -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"M116@Drill#2026"}')"
AT="$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')"
[[ -n "$AT" ]] && ok "admin 登录取到 access_token" || { bad "登录失败：$(printf '%s' "$LOGIN" | head -c 300)"; echo; echo "== 汇总：PASS=$PASS FAIL=$FAIL =="; exit 1; }

CHG="$(curl -sS -m 20 -X PUT "$BASE/api/system/auth/password" \
    -H 'Content-Type: application/json' -H "Authorization: Bearer $AT" \
    -d '{"old_password":"M116@Drill#2026","new_password":"M116@Ready#2026"}')"
[[ "$(printf '%s' "$CHG" | jq -r '.msg // ""')" == "密码已修改" ]] \
    && ok "首登强制改密完成" || bad "改密失败：$(printf '%s' "$CHG" | head -c 300)"
LOGIN="$(curl -sS -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"M116@Ready#2026"}')"
AT="$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')"
[[ -n "$AT" ]] && ok "用新口令重新登录成功" || { bad "重新登录失败"; exit 1; }

NOAUTH_CODE="$(curl -sS -m 20 -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/report/cash-flow/statement?period=1913-05")"
[[ "$NOAUTH_CODE" == "401" ]] && ok "未带 token → 401（鉴权闸真的在）" || bad "未带 token 返回 $NOAUTH_CODE（应为 401）"

# ============================================================================
echo
echo "== 3) 造期间 + 用 M1-14 的 PUT 录两张底稿（上期 900/400 → 500；本期 1200/700 → 1000）=="
for spec in "1913 4" "1913 5"; do
    set -- $spec
    Y="$1"; M="$2"
    START="$(printf '%04d-%02d-01' "$Y" "$M")"
    END="$(date -d "$START +1 month -1 day" +%Y-%m-%d)"
    exec_db "insert into biz_accounting_period (id, period_year, period_month, period_code, start_date, end_date, status) \
             values (gen_random_uuid(), $Y, $M, '$(printf '%04d-%02d' "$Y" "$M")', '${START}', '${END}', 'OPEN') on conflict (period_code) do nothing"
done
assert_jq_plain "2" "$(q_db "select count(*) from biz_accounting_period where period_code in ('1913-04','1913-05');")" \
    "两个期间就位（1913-04 / 1913-05）"

RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{
  "period":"1913-04",
  "opening":{"amount":"0.0000"},
  "closing":{"amount":"500.0000"},
  "items":[{"item_code":"OPER_SALES_RECEIPTS","amount":"900.0000"},
           {"item_code":"OPER_PURCHASE_PAYMENTS","amount":"400.0000"}]}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "上期底稿 PUT 200（勾稽闭合）" || bad "上期底稿 PUT 返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "true" "$BODY" '.data.check.balanced' "上期底稿闭合"

RAW="$(call PUT '/api/business/erp/report/cash-flow/worksheet' '{
  "period":"1913-05",
  "opening":{"amount":"500.0000","remark":"上月结转"},
  "closing":{"amount":"1000.0000"},
  "items":[{"item_code":"OPER_SALES_RECEIPTS","amount":"1200.0000","remark":"销售回款"},
           {"item_code":"OPER_PURCHASE_PAYMENTS","amount":"700.0000"}]}')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "本期底稿 PUT 200（勾稽闭合）" || bad "本期底稿 PUT 返回 $CODE：$(printf '%s' "$BODY" | head -c 300)"
assert_jq "true" "$BODY" '.data.check.balanced' "本期底稿闭合"

# ============================================================================
echo
echo "== 4) GET statement（三栏报表）=="
RAW="$(call GET '/api/business/erp/report/cash-flow/statement?period=1913-05')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "GET statement 200" || bad "GET statement 返回 $CODE：$(printf '%s' "$BODY" | head -c 400)"
assert_jq "CASH_FLOW_WORKSHEET" "$BODY" '.data.report_code' "报表编码"
assert_jq "现金流量表" "$BODY" '.data.report_name' "报表名称"
assert_jq "1913-05" "$BODY" '.data.period' "本期期间回显"
assert_jq "1913-04" "$BODY" '.data.previous_period' "上期期间（上一会计期间）"
assert_jq "本期金额" "$BODY" '.data.current_label' "本期列名"
assert_jq "上期金额" "$BODY" '.data.previous_label' "上期列名"
assert_jq "WORKSHEET" "$BODY" '.data.source' "数据来源 = 底稿"
assert_jq "3" "$BODY" '[.data.sections[]] | length' "三大活动分区"
assert_jq "OPERATING,INVESTING,FINANCING" "$BODY" '[.data.sections[].activity] | join(",")' "分区顺序固定"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].inflow[] | select(.item_code=="OPER_SALES_RECEIPTS") | .current | tonumber) == 1200)' \
    "本期销售收款 1200"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].inflow[] | select(.item_code=="OPER_SALES_RECEIPTS") | .previous | tonumber) == 900)' \
    "上期销售收款 900"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].outflow[] | select(.item_code=="OPER_PURCHASE_PAYMENTS") | .current | tonumber) == 700)' \
    "本期采购付款 700"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].outflow[] | select(.item_code=="OPER_PURCHASE_PAYMENTS") | .previous | tonumber) == 400)' \
    "上期采购付款 400"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].inflow_total.current | tonumber) == 1200)' "流入小计 1200"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].inflow_total.previous | tonumber) == 900)' "上期流入小计 900"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].net.current | tonumber) == 500)' "经营活动净额 500"
assert_jq "true" "$BODY" '(([.data.sections[] | select(.activity=="OPERATING")][0].net.previous | tonumber) == 500)' "上期经营活动净额 500"
assert_jq "true" "$BODY" '((.data.net_increase.current | tonumber) == 500)' "现金净增加额 500"
assert_jq "true" "$BODY" '((.data.opening.current | tonumber) == 500)' "期初余额 500"
assert_jq "true" "$BODY" '((.data.closing.current | tonumber) == 1000)' "期末余额 1000"
assert_jq "true" "$BODY" '((.data.opening.previous | tonumber) == 0)' "上期期初 0"
assert_jq "true" "$BODY" '((.data.closing.previous | tonumber) == 500)' "上期期末 500"
assert_jq "true" "$BODY" '.data.check.balanced' "勾稽闭合（出表闸打开）"
assert_jq "true" "$BODY" '.data.check.arithmetic_ok' "报表加减关系成立"
assert_jq "true" "$BODY" '.data.check.previous_balanced' "上期也闭合"
assert_jq "true" "$BODY" '.data.check.message | contains("勾稽闭合")' "结论是整句人话"
assert_jq "true" "$BODY" '.data.line_count > 10' "列示行数（配置项全列 + 已录行）"
assert_jq "0" "$BODY" '.data.hidden_zero_lines' "默认不隐藏零行"
assert_jq "true" "$BODY" '.data.source_note | contains("底稿")' "口径说明跟着报表走"

RAW="$(call GET '/api/business/erp/report/cash-flow/statement?period=1913-05&hide_zero=true')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY2="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "GET statement&hide_zero=true 200" || bad "hide_zero 返回 $CODE"
assert_jq "true" "$BODY2" '.data.hidden_zero_lines > 0' "隐藏了未录入的零行"
assert_jq "true" "$BODY2" '((.data.net_increase.current | tonumber) == 500)' "隐藏零行不改变净增加额"

# ============================================================================
echo
echo "== 5) GET export（xlsx 真文件）=="
HDR="$RUN_DIR/export_headers.txt"
curl -sS -m 60 -D "$HDR" -o "$RUN_DIR/statement_1913-05.xlsx" \
     "$BASE/api/business/erp/report/cash-flow/statement/export?period=1913-05" \
     -H "Authorization: Bearer $AT"
EXP_CODE="$(head -1 "$HDR" | awk '{print $2}')"
SIZE="$(stat -c %s "$RUN_DIR/statement_1913-05.xlsx" 2>/dev/null || echo 0)"
[[ "$EXP_CODE" == "200" ]] && ok "export 200" || bad "export 返回 $EXP_CODE"
[[ "$SIZE" -gt 5000 ]] && ok "xlsx 是真文件（$SIZE 字节）" || bad "xlsx 太小（$SIZE 字节）"
grep -qi 'spreadsheetml' "$HDR" && ok "Content-Type 是 xlsx" || bad "Content-Type 不是 xlsx：$(grep -i content-type "$HDR" | head -1)"
grep -q 'filename\*=UTF-8' "$HDR" && ok "文件名带期间（RFC5987 中文名）" || bad "缺 filename 头"
[[ "$(head -c 2 "$RUN_DIR/statement_1913-05.xlsx")" == "PK" ]] && ok "文件头是 ZIP（PK）" || bad "文件头不是 PK"

# ============================================================================
echo
echo "== 6) 把期末改坏 → GET 仍 200 但不闭合；export → 400 + 66087 =="
exec_db "update biz_cashflow_worksheet set amount = 1100.0000 where period_code='1913-05' and line_type='CLOSING'"
RAW="$(call GET '/api/business/erp/report/cash-flow/statement?period=1913-05')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "200" ]] && ok "未闭合时 GET 仍 200（页面要能打开去补）" || bad "未闭合时 GET 返回 $CODE"
assert_jq "false" "$BODY" '.data.check.balanced' "勾稽不闭合"
assert_jq "true" "$BODY" '((.data.check.diff | tonumber) == -100)' "差额精确 -100（期末多了 100：期初+净增加额−期末）"
assert_jq "true" "$BODY" '.data.check.current.problems | length > 0' "问题逐条点名"

RAW="$(call GET '/api/business/erp/report/cash-flow/statement/export?period=1913-05')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "未闭合时 export → 400（出表闸）" || bad "未闭合时 export 返回 $CODE（应为 400）"
assert_jq "66087" "$BODY" '.code' "错误码 66087（底稿勾稽不平）"
assert_jq "true" "$BODY" '.msg | contains("未闭合")' "报文说未闭合"
assert_jq "true" "$BODY" '.msg | contains("1913-05")' "报文点名期间"

# 修回去
exec_db "update biz_cashflow_worksheet set amount = 1000.0000 where period_code='1913-05' and line_type='CLOSING'"
RAW="$(call GET '/api/business/erp/report/cash-flow/statement/export?period=1913-05')"
CODE="$(printf '%s' "$RAW" | tail -1)"
[[ "$CODE" == "200" ]] && ok "修平后 export 恢复 200" || bad "修平后 export 返回 $CODE"

# ============================================================================
echo
echo "== 7) 期间 / 参数闸 =="
RAW="$(call GET '/api/business/erp/report/cash-flow/statement?period=1913-09')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "不存在的期间 → 400" || bad "不存在的期间返回 $CODE"
assert_jq "66010" "$BODY" '.code' "错误码 66010（期间不存在）"
assert_jq "true" "$BODY" '.msg | contains("会计期间")' "报文点名期间"

RAW="$(call GET '/api/business/erp/report/cash-flow/statement?period=2026-13')"
CODE="$(printf '%s' "$RAW" | tail -1)"; BODY="$(printf '%s' "$RAW" | sed '$d')"
[[ "$CODE" == "400" ]] && ok "非法期间格式 → 400" || bad "非法期间格式返回 $CODE"
assert_jq "10001" "$BODY" '.code' "错误码 10001（参数非法）"
assert_jq "true" "$BODY" '.msg | contains("yyyy-MM")' "报文说清格式"

# ============================================================================
echo
echo "== 8) DB 对账 =="
assert_jq_plain "2" "$(q_db "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001810','20000000-0000-0000-0000-000000001820') and menu_type=3 and parent_id='20000000-0000-0000-0000-000000001018' and deleted_at is null;")" \
    "菜单两码挂「现金流量表」页面"
assert_jq_plain "4" "$(q_db "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('20000000-0000-0000-0000-000000001810','20000000-0000-0000-0000-000000001820') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")" \
    "授权齐（超管 + 财务经理 × 2 码）"
assert_jq_plain "8" "$(q_db "select count(*) from biz_cashflow_worksheet where period_code in ('1913-04','1913-05');")" \
    "底稿行数 = 8（两期 × (2 明细 + 期初 + 期末)）"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
