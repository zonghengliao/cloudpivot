#!/usr/bin/env bash
# ============================================================================
# M3-10 客户信用「实时占用 / 可用额度」· HTTP 端到端（真起服务，真打接口）
#
# 卡：t_8729b656
#
# 为什么要这一层（单测已经全绿，但单测调的是函数，不是路由）：
#   * `#[nik::permission]` / `#[nik::get]` 宏的路径**写错一个字符**、权限码拼错、
#     查询参数解析接错，单测全绿但接口 404 / 403；
#   * JSON 字段名与类型（`Decimal` 出字符串、`Option` 出 null）只有真报文能证；
#   * 「未清应收 / 在审 / 本单」三个分量在真报文里是否各自独立，也要眼见为实。
#
# 做的事：
#   1. 建**空库** cp_m310_http_e2e（让应用自己跑全量迁移 —— 顺带证明本卡没让迁移红）
#   2. 起隔离实例（127.0.0.1:18095），等 /health 200
#   3. admin 登录 → 建客户（额度 1000）→ `GET /credit/{id}?amount=250`（只有本单）
#   4. 直接插「已审订单 600 / 在审订单 200 / 部分收款应收 300（核销 100）」，
#      再拉快照：used / pending / open_receivable / occupied / available 逐个对账
#   5. 试算超限：`?amount=500` → total 1200 超 200 → verdict=BLOCK、allowed=false
#   6. 入参闸：未知参数 400、客户不存在 404
#   7. 停服务 + 删库（KEEP=1 留库排查）
#
# 用法：
#   bash scripts/verify/m310_credit_http_e2e.sh                # 99 → 145 的 PG
#   PG_HOST=127.0.0.1 bash scripts/verify/m310_credit_http_e2e.sh   # 在 145 上跑
#   KEEP=1 bash scripts/verify/m310_credit_http_e2e.sh         # 留库 / 留日志
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
DB="cp_m310_http_e2e_$$"
# 端口随机化 + 启动前先探测：**绝不能**连到上一轮残留实例（那会用别库的数据算，
# 断言是红是绿都毫无意义 —— 2026-11-17 本脚本第一版就栽在这，见卡内汇报）
PORT="${PORT:-$(( 18100 + RANDOM % 400 ))}"
KEEP="${KEEP:-0}"

TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m310_http_$$.log"
PIDFILE="$TMP/m310_http_$$.pid"

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }

stop_app() {
    if [ -n "${APP_PID:-}" ] && kill -0 "$APP_PID" 2>/dev/null; then
        kill "$APP_PID" 2>/dev/null
        for _ in $(seq 1 40); do
            kill -0 "$APP_PID" 2>/dev/null || break
            sleep 1
        done
        kill -9 "$APP_PID" 2>/dev/null
    fi
    rm -f "$PIDFILE"
}
cleanup() {
    stop_app
    if [ "$KEEP" = "1" ]; then
        printf '\n  （KEEP=1：库 %s 与日志 %s 保留）\n' "$DB" "$LOG"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

printf '== M3-10 信用快照 · HTTP 端到端（PG %s:%s / 库 %s / 端口 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB" "$PORT"

# ---------- 0) 空库（应用自己跑全量迁移） ----------
echo "== 0) 建空库 + 起实例"
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if psql_admin "CREATE DATABASE $DB" >/dev/null 2>&1; then
    ok "空库 $DB 已建（应用启动时全量迁移）"
else
    bad "建库失败"
    exit 1
fi

if [ ! -x "$ROOT/backend/target/debug/cloudpivot" ]; then
    bad "缺 target/debug/cloudpivot（先 cd backend && cargo build）"
    exit 1
fi

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST="$PG_HOST"
export NIK_DATABASE_PORT="$PG_PORT"
export NIK_DATABASE_USER="$PG_USER"
export NIK_DATABASE_PASSWORD="$PGPASSWORD"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD='Admin@123456'

# 端口必须是空的：上一轮残留实例占着端口时，`/health` 会由**别人**应答，
# 后续所有 API 都打在别的库上（本脚本第一版就是这样把断言跑成一片红的）。
if curl -s -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/health"; then
    bad "端口 $PORT 已有实例在应答（残留进程）—— 换端口或先清掉，否则断言会打在别的库上"
    exit 1
fi
ok "端口 $PORT 空闲（不会打到残留实例）"

rm -f "$LOG"
# 不用 setsid：`setsid` 会 fork，`$!` 拿到的是立即退出的那个进程，`kill` 杀不掉真身
# （第一版就是这么留下了一个跑满两轮演练的僵尸实例）。
cd "$ROOT/backend" || exit 1
nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
APP_PID=$!
echo "$APP_PID" > "$PIDFILE"
cd - >/dev/null || true

HEALTHY=0
for _ in $(seq 1 180); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null || true)
    if [ "$code" = "200" ]; then HEALTHY=1; break; fi
    if ! kill -0 "$APP_PID" 2>/dev/null; then break; fi
    sleep 1
done
if [ "$HEALTHY" = "1" ]; then
    ok "实例起来了（pid=$APP_PID，/health 200，全量迁移完成）"
else
    bad "实例没起来（见 $LOG）：$(tail -5 "$LOG" 2>/dev/null | tr '\n' ' ')"
    exit 1
fi

API="http://127.0.0.1:$PORT/api"
jget() { python3 -c 'import json,sys;cur=json.load(sys.stdin);
keys=sys.argv[1].split(".")
for k in keys:
    cur = cur[k] if not isinstance(cur,list) else cur[int(k)]
print(cur)' "$1"; }

# ---------- 1) 登录 ----------
echo "== 1) admin 登录"
LOGIN="$(curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"Admin@123456"}')"
TOKEN="$(printf '%s' "$LOGIN" | python3 -c 'import json,sys;print(json.load(sys.stdin)["data"]["access_token"])' 2>/dev/null || true)"
if [ -n "${TOKEN:-}" ]; then
    ok "拿到 access_token（${#TOKEN} 字节）"
else
    bad "登录失败：$LOGIN"
    exit 1
fi
AUTH=(-H "Authorization: Bearer $TOKEN")

# 隔离库首启：admin 的 `must_change_password = true`，任何业务接口都会 403
# 「首次登录必须先修改初始密码」—— 先改密再重登（与 `d4_po_to_stockin.sh` 同一做法）
MUST_CHANGE="$(printf '%s' "$LOGIN" | jget data.must_change_password 2>/dev/null || echo False)"
if [ "$MUST_CHANGE" = "True" ]; then
    NEW_PASS='M310_Admin@123456'
    CODE_PWD=$(curl -s -o /dev/null -w '%{http_code}' -X PUT "$API/system/auth/password" "${AUTH[@]}" \
        -H 'Content-Type: application/json' \
        -d "{\"old_password\":\"Admin@123456\",\"new_password\":\"$NEW_PASS\"}")
    if [ "$CODE_PWD" = "200" ]; then
        ok "首登强制改密（PUT /system/auth/password 200）"
    else
        bad "改密失败（HTTP $CODE_PWD）"
        exit 1
    fi
    LOGIN="$(curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$NEW_PASS\"}")"
    TOKEN="$(printf '%s' "$LOGIN" | python3 -c 'import json,sys;print(json.load(sys.stdin)["data"]["access_token"])' 2>/dev/null || true)"
    [ -n "${TOKEN:-}" ] && ok "改密后重新登录成功" || { bad "改密后重新登录失败：$LOGIN"; exit 1; }
    AUTH=(-H "Authorization: Bearer $TOKEN")
fi

# ---------- 2) 建客户（额度 1000） ----------
echo "== 2) 建客户（额度 1000，策略默认 BLOCK）"
CUST="$(curl -s -X POST "$API/business/erp/customer" "${AUTH[@]}" -H 'Content-Type: application/json' \
    -d '{"code":"CR10-E2E","name":"M3-10 端到端客户","credit_limit":1000,"credit_days":30,"status":"ACTIVE"}')"
CID="$(printf '%s' "$CUST" | jget data.id 2>/dev/null || true)"
if [ -n "${CID:-}" ]; then
    ok "客户已建（id=$CID）"
else
    bad "建客户失败：$CUST"
    exit 1
fi

# ---------- 3) 快照：只有本单（250） ----------
echo "== 3) GET /credit/{id}?amount=250（还没有任何订单 / 应收）"
S1="$(curl -s "$API/business/erp/credit/$CID?amount=250" "${AUTH[@]}")"
chk() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}
# 金额比较按**数值**（后端的 Decimal 标度可能带 4 位也可能收敛到 2 位，
# 那不是业务差异 —— 用 0.01 的分位相等来判，避免把标度写死进断言）
chk_num() { # 期望 实际 说明
    if python3 -c 'import sys
from decimal import Decimal as D
a, b = D(sys.argv[1]), D(sys.argv[2])
sys.exit(0 if abs(a - b) <= D("0.01") else 1)' "$1" "$2" 2>/dev/null; then
        ok "$3（=$2）"
    else
        bad "$3（期望 [$1]，实际 [$2]）"
    fi
}
chk_num "1000" "$(printf '%s' "$S1" | jget data.credit_limit)" "credit_limit = 1000（NUMERIC 出 JSON 是字符串）"
chk_num "0" "$(printf '%s' "$S1" | jget data.used)" "used = 0（没有有效订单）"
chk_num "0" "$(printf '%s' "$S1" | jget data.pending)" "pending = 0（没有在审单）"
chk_num "0" "$(printf '%s' "$S1" | jget data.open_receivable)" "open_receivable = 0（没有未清应收）"
chk_num "250" "$(printf '%s' "$S1" | jget data.total)" "total = 本单 250"
chk_num "1000" "$(printf '%s' "$S1" | jget data.available)" "available = 1000（不含本单）"
chk_num "750" "$(printf '%s' "$S1" | jget data.available_after)" "available_after = 750"
chk "WITHIN" "$(printf '%s' "$S1" | jget data.verdict)" "verdict = WITHIN"
chk "True" "$(printf '%s' "$S1" | jget data.allowed)" "allowed 出布尔（True）"

# ---------- 4) 插入真实占用，再拉快照对账 ----------
echo "== 4) 插「已审订单 600 / 在审订单 200 / 部分收款应收 300（核销 100）」后对账"
# 注入用直连 SQL（绕过 API：本卡只做「算」，不造单据）；**每条都验返回**，
# 否则 SQL 报错会被重定向吞掉，快照算出一片 0 还以为是代码错。
ins() { # SQL 说明
    local out
    out=$(psql_q "$1")
    if printf '%s' "$out" | grep -q "INSERT 0 1"; then
        ok "$2"
    else
        bad "$2：$out"
    fi
}
ins "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax)
     VALUES (gen_random_uuid(), 'SO10-E2E-A', '$CID', CURRENT_DATE, 'APPROVED', 600, 0, 600)" "已审订单 600 已插（进「已占」）"
ins "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax)
     VALUES (gen_random_uuid(), 'SO10-E2E-B', '$CID', CURRENT_DATE, 'SUBMITTED', 200, 0, 200)" "在审订单 200 已插（进「在审」）"
ins "INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, amount,
            received_amount, unsettled_amount, credit_days, period_code, due_date, status)
     VALUES (gen_random_uuid(), 'AR-SO10-E2E', '$CID', 'MANUAL', gen_random_uuid(), 'MANUAL',
            300, 100, 200, 30, '2026-11', CURRENT_DATE + 30, 'PARTIAL')" "部分收款应收 300/100 已插（未结 200）"

S2="$(curl -s "$API/business/erp/credit/$CID" "${AUTH[@]}")"
# 已占 = 有效订单 600 − 已核销应收 100 = 500；在审 200；占用 700；可用 300
chk_num "500" "$(printf '%s' "$S2" | jget data.used)" "used = 有效订单 600 − 已核销 100 = 500"
chk_num "200" "$(printf '%s' "$S2" | jget data.pending)" "pending = 在审 200（不含本单）"
chk_num "200" "$(printf '%s' "$S2" | jget data.open_receivable)" "open_receivable = 未结 200（对照口径）"
chk_num "700" "$(printf '%s' "$S2" | jget data.occupied)" "occupied = 500 + 200 = 700"
chk_num "300" "$(printf '%s' "$S2" | jget data.available)" "available = 1000 − 700 = 300"
chk "False" "$(printf '%s' "$S2" | jget data.is_over)" "is_over 出布尔（False）"
chk "WITHIN" "$(printf '%s' "$S2" | jget data.verdict)" "verdict = WITHIN（还没试算本单）"

# ---------- 5) 试算超限 ----------
echo "== 5) 试算超限：?amount=500 → total 1200 超 200"
S3="$(curl -s "$API/business/erp/credit/$CID?amount=500" "${AUTH[@]}")"
chk_num "1200" "$(printf '%s' "$S3" | jget data.total)" "total = 700 + 500 = 1200"
chk_num "200" "$(printf '%s' "$S3" | jget data.over)" "over = 1200 − 1000 = 200"
chk "BLOCK" "$(printf '%s' "$S3" | jget data.verdict)" "verdict = BLOCK（默认策略）"
chk "False" "$(printf '%s' "$S3" | jget data.allowed)" "allowed = false（提交会被拒）"
chk_num "0" "$(printf '%s' "$S3" | jget data.available_after)" "available_after 不出负数"

# ---------- 6) 入参闸 / 404 ----------
echo "== 6) 入参闸与 404"
CODE_UNKNOWN=$(curl -s -o /dev/null -w '%{http_code}' "$API/business/erp/credit/$CID?amout=100" "${AUTH[@]}")
chk "400" "$CODE_UNKNOWN" "拼错参数 amout → 400（不静默当没传）"
CODE_NEG=$(curl -s -o /dev/null -w '%{http_code}' "$API/business/erp/credit/$CID?amount=-1" "${AUTH[@]}")
chk "400" "$CODE_NEG" "负金额 → 400"
CODE_404=$(curl -s -o /dev/null -w '%{http_code}' \
    "$API/business/erp/credit/01a10000-0000-4000-8000-0000000000ff" "${AUTH[@]}")
chk "404" "$CODE_404" "客户不存在 → 404"

# ---------- 7) 停服务 ----------
echo "== 7) 收尾"
stop_app
if curl -s -o /dev/null --max-time 2 "http://127.0.0.1:$PORT/health"; then
    bad "实例还在应答（端口 $PORT 未释放）"
else
    ok "实例已停、端口 $PORT 已释放（不留僵尸进程）"
fi

printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项（日志：$LOG）"
exit 1
