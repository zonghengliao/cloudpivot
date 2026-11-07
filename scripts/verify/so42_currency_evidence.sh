#!/usr/bin/env bash
# ============================================================================
# SO-42 证据脚本：币种 / 汇率维护页（ARCH-7 §15.6.3）
#
# 做什么：
#   1) 起一个**干净库**上的实例（自动跑全部迁移，含本卡的 V0316）
#   2) 真实 token curl 打 10 个新接口，逐条断言（币种 CRUD / 本位币唯一 / 汇率 CRUD /
#      按月批量 upsert / 汇率预览）
#   3) 反例：重复币种码 409、本位币停用 409、汇率 0 400、自反汇率 400、
#      同键汇率重复 409、外币未录汇率保存被拒 400
#   4) 单据侧：用 USD 建销售订单 → 校验锁定的 exchange_rate 落库且不随汇率表变动
#
# 用法：bash scripts/verify/so42_currency_evidence.sh
# 前置：本机 PG 有 alun 账号；su postgres 可建库；backend 已 cargo build
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

DB=${ERP_DB:-cloudpivot_so42}
PORT=${PORT:-18098}
BIN=${BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/so42-evidence.log}
OUT=${OUT:-/opt/cloudpivot/docs/evidence/20261107-so42-currency}
BASE="http://127.0.0.1:$PORT"
PSQL() { PGPASSWORD=alun psql -h 127.0.0.1 -U $DB_USER -d "$DB" -Atc "$1"; }
DB_USER=alun

EV="$OUT/api_evidence.txt"
mkdir -p "$OUT"
: > "$EV"

say() { echo "$@" | tee -a "$EV"; }
show() { # show <label> <json>
  say "---- $1"
  echo "$2" | python3 -m json.tool 2>/dev/null | tee -a "$EV" || echo "$2" | tee -a "$EV"
}

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="so42-evidence-jwt-secret-0123456789abcdef0123456789abcdef"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD=Admin@123456

say "=============================================================="
say "SO-42 币种 / 汇率维护接口证据  $(date '+%F %T %Z')"
say "库=$DB 端口=$PORT 二进制=$BIN"
say "=============================================================="

# 0) 首次登录必须改密的旗标会 403 掉所有业务接口，先清掉（与改密无关，本卡不验它）。
#    ⚠️ 顺序：新库上 `sys_user` 表要等首次启动跑完迁移才存在，而且实例内有 60s 缓存，
#    所以流程是「启动 → 等健康 → 停 → 改旗标 → 再启动 → 登录」。
kill "$(cat /root/so42-evidence.pid 2>/dev/null)" 2>/dev/null
sleep 1

boot() {
  cd /opt/cloudpivot/backend || exit 1
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > /root/so42-evidence.pid
  for _ in $(seq 1 240); do
    curl -sf "$BASE/health" > /dev/null && break
    sleep 1
  done
  sleep 2
  echo "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")"
}

say "== 第一次启动（跑迁移）pid=$({ boot; } > /dev/null; cat /root/so42-evidence.pid) health=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")"
kill "$(cat /root/so42-evidence.pid)" 2>/dev/null
sleep 2
PSQL "UPDATE sys_user SET must_change_password = false WHERE username = 'admin'"
say "== 清掉 must_change_password（影响行数见上）"
say "== 第二次启动 health=$(boot)"
say "== 迁移记账（末 4 条 versioned）:"
PSQL "SELECT version||'|'||kind||'|'||applied_via||'|'||name FROM schema_migrations WHERE kind='versioned' ORDER BY version::int DESC LIMIT 4" | tee -a "$EV"
say "== V0316 记账行: $(PSQL "SELECT version||'|'||kind||'|'||applied_via||'|success='||success FROM schema_migrations WHERE name='currency_menu'")"
say "== 菜单：$(PSQL "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000003161','20000000-0000-0000-0000-000000003168') AND menu_type=2 AND path IS NOT NULL") 个页面节点 / $(PSQL "SELECT count(*) FROM sys_menu WHERE parent_id IN ('20000000-0000-0000-0000-000000003161','20000000-0000-0000-0000-000000003168') AND menu_type=3") 个按钮 / 10 个权限码承载 $(PSQL "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:currency:%' OR auth_code LIKE 'business:erp:fx-rate:%'") 行"
say "== 授权（FINANCE_MANAGER / SALES_SUPERVISOR / SALES_CLERK 的菜单数）: $(PSQL "SELECT r.role_code||'='||count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE rm.menu_id::text LIKE '20000000-0000-0000-0000-00000000316%' OR rm.menu_id::text LIKE '20000000-0000-0000-0000-00000000317%' GROUP BY r.role_code ORDER BY r.role_code" | tr '\n' ' ')"

# ---------- 登录 ----------
TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["data"]["access_token"])')
[ -n "${TOKEN:-}" ] || { say "!! 登录失败，见 $LOG"; kill "$(cat /root/so42-evidence.pid)" 2>/dev/null; exit 1; }
say "== token 长度 ${#TOKEN}"
AUTH="Authorization: Bearer $TOKEN"
JSON="Content-Type: application/json"

code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

# ---------- 1) 币种列表（种子 4 个，CNY 本位币） ----------
say ""
say "=== 1) GET /api/business/erp/currency/list（V0272 种子 + 本位币标记）"
List=$(curl -s "$BASE/api/business/erp/currency/list" -H "$AUTH")
show "currency/list 响应" "$List"
say "== 断言："
echo "$List" | python3 -c '
import sys,json
d=json.load(sys.stdin)["data"]
codes={r["code"]:r for r in d}
assert {"CNY","USD","EUR","HKD"} <= set(codes), codes.keys()
assert codes["CNY"]["is_base"] is True and sum(1 for r in d if r["is_base"])==1, "本位币必须唯一且是 CNY"
print("   PASS 种子 4 币种齐全；本位币唯一（CNY）")' | tee -a "$EV"

# ---------- 2) 新建币种 + 重复 409 ----------
say ""
say "=== 2) POST /api/business/erp/currency（新建 JPY，小写码自动转大写）"
New=$(curl -s -X POST "$BASE/api/business/erp/currency" -H "$AUTH" -H "$JSON" \
  -d '{"code":"jpy","name":"日元","symbol":"¥","decimal_places":0,"remark":"SO-42 证据"}')
show "新建响应" "$New"
say "== 重复码 HTTP=$(code -X POST "$BASE/api/business/erp/currency" -H "$AUTH" -H "$JSON" -d '{"code":"JPY","name":"日元2","symbol":"Y"}')（期望 409）"
say "== 非法码 HTTP=$(code -X POST "$BASE/api/business/erp/currency" -H "$AUTH" -H "$JSON" -d '{"code":"JP","name":"x","symbol":"y"}')（期望 400）"

# ---------- 3) 编辑币种 ----------
say ""
say "=== 3) PUT /api/business/erp/currency/JPY（改名称 + 小数位）"
Upd=$(curl -s -X PUT "$BASE/api/business/erp/currency/JPY" -H "$AUTH" -H "$JSON" \
  -d '{"name":"日元（JP）","decimal_places":0}')
show "编辑响应" "$Upd"

# ---------- 4) 立本位币（唯一）+ 本位币不能停用 ----------
say ""
say "=== 4) POST /api/business/erp/currency/USD/base（把 USD 立为本位币）"
Base1=$(curl -s -X POST "$BASE/api/business/erp/currency/USD/base" -H "$AUTH")
show "立本位币响应" "$Base1"
say "== 库内本位币行数（期望 1，且为 USD）: $(PSQL "SELECT code FROM biz_currency WHERE is_base")"
say "== 本位币停用 HTTP=$(code -X PUT "$BASE/api/business/erp/currency/USD" -H "$AUTH" -H "$JSON" -d '{"status":"INACTIVE"}')（期望 409）"
say "== 把 CNY 立回来: $(curl -s -X POST "$BASE/api/business/erp/currency/CNY/base" -H "$AUTH" | python3 -c 'import sys,json;d=json.load(sys.stdin)["data"];print(d["code"],"is_base=",d["is_base"])')"

# ---------- 5) 下拉（只 ACTIVE） ----------
say ""
say "=== 5) GET /api/business/erp/currency/options（只 ACTIVE）"
Opts=$(curl -s "$BASE/api/business/erp/currency/options" -H "$AUTH")
show "options 响应" "$Opts"

# ---------- 6) 汇率：新建 / 重复 409 / 0 400 / 自反 400 ----------
say ""
say "=== 6) POST /api/business/erp/exchange-rate"
Rate1=$(curl -s -X POST "$BASE/api/business/erp/exchange-rate" -H "$AUTH" -H "$JSON" \
  -d '{"from_currency":"USD","rate":"7.10000000","rate_type":"SPOT","effective_date":"2026-10-01","remark":"央行中间价"}')
show "新建 SPOT 响应" "$Rate1"
RID=$(echo "$Rate1" | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')
say "== 同键重复 HTTP=$(code -X POST "$BASE/api/business/erp/exchange-rate" -H "$AUTH" -H "$JSON" -d '{"from_currency":"USD","rate":"7.2","rate_type":"SPOT","effective_date":"2026-10-01"}')（期望 409）"
say "== 汇率=0 HTTP=$(code -X POST "$BASE/api/business/erp/exchange-rate" -H "$AUTH" -H "$JSON" -d '{"from_currency":"EUR","rate":"0","effective_date":"2026-10-01"}')（期望 400）"
say "== 自反汇率（CNY→CNY）HTTP=$(code -X POST "$BASE/api/business/erp/exchange-rate" -H "$AUTH" -H "$JSON" -d '{"from_currency":"CNY","rate":"1","effective_date":"2026-10-01"}')（期望 400）"
say "== 不存在的币种 HTTP=$(code -X POST "$BASE/api/business/erp/exchange-rate" -H "$AUTH" -H "$JSON" -d '{"from_currency":"GBP","rate":"9.1","effective_date":"2026-10-01"}')（期望 400）"

# ---------- 7) 按月批量录入（upsert） ----------
say ""
say "=== 7) POST /api/business/erp/exchange-rate/batch-month（2026-11 月末）"
Batch1=$(curl -s -X POST "$BASE/api/business/erp/exchange-rate/batch-month" -H "$AUTH" -H "$JSON" \
  -d '{"month":"2026-11","rate_type":"MONTH_END","remark":"月末中间价","rates":[{"from_currency":"USD","rate":"7.15000000"},{"from_currency":"EUR","rate":"7.80000000"},{"from_currency":"JPY","rate":"0.04800000"}]}')
show "批量录入响应" "$Batch1"
say "== 再跑一次（USD 改 7.2，EUR/JPY 同值）→ 期望 updated=3 / created=0"
Batch2=$(curl -s -X POST "$BASE/api/business/erp/exchange-rate/batch-month" -H "$AUTH" -H "$JSON" \
  -d '{"month":"2026-11","rate_type":"MONTH_END","rates":[{"from_currency":"USD","rate":"7.20000000"},{"from_currency":"EUR","rate":"7.80000000"},{"from_currency":"JPY","rate":"0.04800000"}]}')
show "重复批量（upsert）响应" "$Batch2"
say "== 库里 2026-11-30 的 MONTH_END 行:"
PSQL "SELECT from_currency||' '||rate||' @'||effective_date||' '||rate_type FROM biz_exchange_rate WHERE rate_type='MONTH_END' ORDER BY from_currency" | tee -a "$EV"
say "== 非法月份 HTTP=$(code -X POST "$BASE/api/business/erp/exchange-rate/batch-month" -H "$AUTH" -H "$JSON" -d '{"month":"2026-13","rates":[{"from_currency":"USD","rate":"7"}]}')（期望 400）"
say "== 空批次 HTTP=$(code -X POST "$BASE/api/business/erp/exchange-rate/batch-month" -H "$AUTH" -H "$JSON" -d '{"month":"2026-11","rates":[]}')（期望 400）"
say "== 批内本位币 HTTP=$(code -X POST "$BASE/api/business/erp/exchange-rate/batch-month" -H "$AUTH" -H "$JSON" -d '{"month":"2026-11","rates":[{"from_currency":"CNY","rate":"1"}]}')（期望 400）"

# ---------- 8) 分页 / 过滤 / 编辑 / 删除 ----------
say ""
say "=== 8) GET /api/business/erp/exchange-rate/page?from_currency=USD"
Page=$(curl -s "$BASE/api/business/erp/exchange-rate/page?from_currency=USD&page=1&page_size=20" -H "$AUTH")
show "分页响应" "$Page"
say "=== 8b) PUT /api/business/erp/exchange-rate/{id}（把 SPOT 改成 7.05）"
Edit=$(curl -s -X PUT "$BASE/api/business/erp/exchange-rate/$RID" -H "$AUTH" -H "$JSON" -d '{"rate":"7.05000000"}')
show "编辑响应" "$Edit"
say "=== 8c) DELETE /api/business/erp/exchange-rate/{id}（物理删）"
say "== HTTP=$(code -X DELETE "$BASE/api/business/erp/exchange-rate/$RID" -H "$AUTH")（期望 200）"
say "== 再删一次 HTTP=$(code -X DELETE "$BASE/api/business/erp/exchange-rate/$RID" -H "$AUTH")（期望 404）"

# ---------- 9) 汇率预览（开单页选币种） ----------
say ""
say "=== 9) GET /api/business/erp/exchange-rate/quote（预览 = 建单锁定同一把尺）"
Q1=$(curl -s "$BASE/api/business/erp/exchange-rate/quote?currency_code=USD&date=2026-11-15" -H "$AUTH")
show "USD @2026-11-15（只剩 MONTH_END，SPOT 已删 → MISSING）" "$Q1"
say "== 重新种一条 SPOT 2026-10-01 7.05 后再预览:"
curl -s -X POST "$BASE/api/business/erp/exchange-rate" -H "$AUTH" -H "$JSON" \
  -d '{"from_currency":"USD","rate":"7.05000000","rate_type":"SPOT","effective_date":"2026-10-01"}' > /dev/null
Q2=$(curl -s "$BASE/api/business/erp/exchange-rate/quote?currency_code=USD&date=2026-11-15" -H "$AUTH")
show "USD @2026-11-15（SPOT 7.05）" "$Q2"
Q3=$(curl -s "$BASE/api/business/erp/exchange-rate/quote?currency_code=CNY&date=2026-11-15" -H "$AUTH")
show "CNY @2026-11-15（本位币恒 1）" "$Q3"
Q4=$(curl -s "$BASE/api/business/erp/exchange-rate/quote?currency_code=EUR&date=2026-11-15" -H "$AUTH")
show "EUR @2026-11-15（只有 MONTH_END → MISSING）" "$Q4"
echo "$Q2$Q3$Q4" > /dev/null
say "== 断言："
python3 - "$Q2" "$Q3" "$Q4" <<'PY' | tee -a "$EV"
import sys, json
usd, cny, eur = (json.loads(x)["data"] for x in sys.argv[1:4])
assert usd["source"] == "SPOT" and float(usd["exchange_rate"]) == 7.05, usd
assert usd["effective_date"] == "2026-10-01", usd
assert cny["source"] == "BASE" and float(cny["exchange_rate"]) == 1, cny
assert eur["source"] == "MISSING", eur
print("   PASS USD→SPOT 7.05（牌价生效日 2026-10-01）；CNY→BASE 1；EUR→MISSING（不臆造汇率）")
PY

# ---------- 10) 单据侧：USD 单锁定汇率，且汇率变动不回写 ----------
say ""
say "=== 10) 单据侧：POST /api/business/erp/sales-order（currency_code=USD）"
CUST=$(PSQL "SELECT id FROM biz_customer WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1")
if [ -z "$CUST" ]; then
  CUST=$(curl -s -X POST "$BASE/api/business/erp/customer" -H "$AUTH" -H "$JSON" \
    -d '{"code":"SO42-CUST","name":"SO-42 币种证据客户","credit_limit":100000000}' \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')
fi
MAT=$(PSQL "SELECT id FROM biz_material WHERE deleted_at IS NULL ORDER BY material_code LIMIT 1")
say "== 夹具 客户=$CUST 物料=$MAT"
Order=$(curl -s -X POST "$BASE/api/business/erp/sales-order" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"order_date\":\"2026-11-07\",\"currency_code\":\"USD\",
  \"remark\":\"SO-42 证据：USD 单（应锁 7.05）\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":\"10\",\"price\":\"100\",\"tax_rate\":\"13\"}]}")
show "USD 建单响应（节选 currency_code / exchange_rate / base_*）" "$(echo "$Order" | python3 -c '
import sys,json
d=json.load(sys.stdin)["data"]
print(json.dumps({k:d[k] for k in ("so_no","currency_code","exchange_rate","total_with_tax","base_total_amount","base_total_tax","base_total_with_tax")}, ensure_ascii=False, indent=2))')"
OID=$(echo "$Order" | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["id"])')
say "== 改汇率表（USD SPOT 2026-11-07 改成 9.99）后，老单详情:"
curl -s -X POST "$BASE/api/business/erp/exchange-rate" -H "$AUTH" -H "$JSON" \
  -d '{"from_currency":"USD","rate":"9.99000000","rate_type":"SPOT","effective_date":"2026-11-07"}' > /dev/null
Detail=$(curl -s "$BASE/api/business/erp/sales-order/$OID" -H "$AUTH")
show "老单详情（exchange_rate 应仍为 7.05）" "$(echo "$Detail" | python3 -c '
import sys,json
d=json.load(sys.stdin)["data"]
print(json.dumps({k:d[k] for k in ("so_no","currency_code","exchange_rate","base_total_with_tax")}, ensure_ascii=False, indent=2))')"
say "== 断言："
python3 - "$Order" "$Detail" <<'PY' | tee -a "$EV"
import sys, json
new, old = json.loads(sys.argv[1])["data"], json.loads(sys.argv[2])["data"]
assert new["currency_code"] == "USD", new["currency_code"]
assert float(new["exchange_rate"]) == 7.05, new["exchange_rate"]
assert float(old["exchange_rate"]) == 7.05, f"汇率被回写了：{old['exchange_rate']}"
print("   PASS 建单锁定 USD@7.05；汇率表改成 9.99 后老单仍 7.05（不回写）")
PY
say "== 无汇率的 EUR 单（只有 MONTH_END，无 SPOT）HTTP=$(code -X POST "$BASE/api/business/erp/sales-order" -H "$AUTH" -H "$JSON" -d "{
  \"customer_id\":\"$CUST\",\"order_date\":\"2026-11-07\",\"currency_code\":\"EUR\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":\"1\",\"price\":\"10\",\"tax_rate\":\"13\"}]}")（期望 400）"

# ---------- 收尾 ----------
say ""
say "== 关实例"
kill "$(cat /root/so42-evidence.pid)" 2>/dev/null
sleep 1
say "== done $(date '+%F %T')"
