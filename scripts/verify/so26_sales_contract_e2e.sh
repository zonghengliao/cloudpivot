#!/usr/bin/env bash
# =============================================================================
# SO-26 验收：合同模块放开 SALES + 接 biz_customer（真后端 + 真 token curl）
# =============================================================================
#   bash scripts/verify/so26_instance.sh up      # 先起实例（:18226）
#   bash scripts/verify/so26_sales_contract_e2e.sh
#
# 验什么（卡面验收三条）：
#   ① 建 SALES 类型合同成功（party_b_id → biz_customer.id，乙方名称由服务端快照）
#   ② 建 SERVICE / LEASE 仍被拒（合法枚举但未开放）
#   ③ party_b_type=CUSTOMER 时校验客户存在（不存在 / 已软删 / FROZEN → 400 66021）
#   另加回归：PURCHASE 仍可建（供应商分支没被改坏）、列表 / 详情带出客户现值。
#
# 证据：docs/evidence/20261106-so26-sales-contract/raw/*（响应体逐份落盘，JWT 已脱敏）
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
PORT=${SO26_PORT:-18226}
BASE="http://127.0.0.1:$PORT/api"
DB=${SO26_DB:-cloudpivot_so26}
EVID="$ROOT/docs/evidence/20261106-so26-sales-contract"
RAW="$EVID/raw"
mkdir -p "$RAW"
export PGPASSWORD=alun
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }

PASS=0; FAIL=0; FAILED=()
ok()  { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$*"); printf '  ❌ %s\n' "$*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

# api <名> <METHOD> <path> [token] [json]
api() {
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  # 用 --oauth2-bearer 而不是手写 Authorization 头：本仓的密钥脱敏会把
  # 「Bearer + 变量」这种字面串改写成 Bearer ***（so24_e2e.sh 就是这么被改坏的）
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null || true)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  # JWT 脱敏：先取出 token 再抹盘（顺序反了会把 <redacted> 当 token 用）
  if printf '%s' "$BODY" | jq -e '.data.access_token? // .data.refresh_token?' >/dev/null 2>&1; then
    jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
        | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
       "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  fi
}
d() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

try_login() { # try_login <user> <pass> [tag] → token（失败回空，不打 PASS/FAIL）
  local code tok out="$RAW/login_${3:-try}.json"
  code=$(curl -s -m 20 -o "$out" -w '%{http_code}' -X POST "$BASE/system/auth/login" \
           -H 'Content-Type: application/json' \
           -d "{\"username\":\"$1\",\"password\":\"$2\"}")
  [ "$code" = "200" ] || { printf ''; return; }
  tok=$(jq -r '.data.access_token // empty' "$out")
  jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
      | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
     "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  MUST_CHANGE=$(jq -r '.data.must_change_password // false' "$out" 2>/dev/null)
  printf '%s' "$tok"
}

echo "=================================================================="
echo "SO-26 验收：库=$DB 实例=$BASE"
echo "=================================================================="

echo; echo "== 0) 实例 / 迁移记账（本卡无表结构改动 ⇒ 不该有新迁移） =="
health=$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$PORT/health")
check "/health" "$health" "200"
migs=$(PSQLQ -c "select count(*) from schema_migrations")
echo "  · 当前迁移条数=$migs（启动日志若打印「应用 0 个迁移」即与卡面第 4 条一致）"
badvia=$(PSQLQ -c "select count(*) from schema_migrations where applied_via not in ('executed','imported')")
check "applied_via 非 executed/imported 的迁移数" "$badvia" "0"
salesmig=$(PSQLQ -c "select count(*) from schema_migrations where name like '%sales_contract%'")
check "本卡新增迁移数（卡面第 4 条：无表结构改动）" "$salesmig" "0"

echo; echo "== 1) admin 登录（模板库口令自 SO-25 继承，逐候选试） =="
T_ADMIN=$(try_login admin 'Admin@So25!2026' admin_so25); FIRST_PWD='Admin@So25!2026'
[ -z "$T_ADMIN" ] && { T_ADMIN=$(try_login admin 'Admin@123456' admin_def); FIRST_PWD='Admin@123456'; }
[ -z "$T_ADMIN" ] && { T_ADMIN=$(try_login admin 'Admin@So24!2026' admin_so24); FIRST_PWD='Admin@So24!2026'; }
[ -z "$T_ADMIN" ] && { echo "FATAL: admin 口令未知，无法继续"; exit 1; }
ok "admin 登录成功（口令 = $FIRST_PWD）"
if [ "${MUST_CHANGE:-false}" = "true" ]; then
  api pre_pwd_change PUT /system/auth/password "$T_ADMIN" \
      "{\"old_password\":\"$FIRST_PWD\",\"new_password\":\"Admin@So26!2026\"}"
  [ "$CODE" = "0" ] && ok "前置：首登改密 → Admin@So26!2026" || bad "前置改密失败 http=$HTTP"
  T_ADMIN=$(try_login admin 'Admin@So26!2026' admin_so26); FIRST_PWD='Admin@So26!2026'
  [ -z "$T_ADMIN" ] && { echo "FATAL: 改密后登录失败"; exit 1; }
fi

echo; echo "== 2) 夹具：建一个 ACTIVE 客户 + 一个 FROZEN 客户（真接口） =="
STAMP=$(date +%y%m%d%H%M%S)
api cus_active POST /business/erp/customer "$T_ADMIN" \
    "{\"code\":\"SO26-CUS-$STAMP\",\"name\":\"SO-26 销售合同客户\",\"credit_days\":30,\"remark\":\"SO-26 验收夹具\"}"
check "建客户 HTTP" "$HTTP" "200"
CUS_ID=$(d '.data.id'); CUS_NAME=$(d '.data.name')
check "客户名" "$CUS_NAME" "SO-26 销售合同客户"
[ -z "$CUS_ID" ] && { echo "FATAL: 客户 id 拿不到"; exit 1; }

api cus_frozen POST /business/erp/customer "$T_ADMIN" \
    "{\"code\":\"SO26-FRZ-$STAMP\",\"name\":\"SO-26 冻结客户\",\"status\":\"FROZEN\"}"
FROZEN_ID=$(d '.data.id'); FROZEN_NAME=$(d '.data.name')
check "建 FROZEN 客户 HTTP" "$HTTP" "200"

echo; echo "== 3) ① SALES 合同：建成功 + 乙方快照来自 biz_customer =="
api sales_create POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"SO-26 销售合同（验收）\",\"contract_type\":\"SALES\",\"party_b_id\":\"$CUS_ID\",
      \"amount\":\"120000.00\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",
      \"end_date\":\"2026-12-31\",\"remark\":\"SO-26 验收\"}"
check "POST /business/erp/contract（SALES）HTTP" "$HTTP" "200"
check "合同类型" "$(d '.data.contract_type')" "SALES"
check "乙方类型（SALES 推导 CUSTOMER）" "$(d '.data.party_b_type')" "CUSTOMER"
check "乙方 id = 客户 id" "$(d '.data.party_b_id')" "$CUS_ID"
check "乙方名称快照 = 客户主档名（服务端读）" "$(d '.data.party_b_name')" "$CUS_NAME"
check "乙方现值（LEFT JOIN biz_customer）" "$(d '.data.party_b_current_name')" "$CUS_NAME"
SALES_ID=$(d '.data.id'); SALES_NO=$(d '.data.contract_no')
check "初始状态" "$(d '.data.status')" "DRAFT"
check "合同号形状（HT-yyyyMMdd-000001）" "$(printf '%s' "$SALES_NO" | cut -c1-3)" "HT-"

echo; echo "  --- 详情 / 列表也能带出客户现值 ---"
api sales_detail GET "/business/erp/contract/$SALES_ID" "$T_ADMIN"
check "GET 详情 HTTP" "$HTTP" "200"
check "详情乙方现值" "$(d '.data.party_b_current_name')" "$CUS_NAME"
api sales_page GET "/business/erp/contract/page?contract_type=SALES&page_size=100" "$T_ADMIN"
check "GET 分页（contract_type=SALES）HTTP" "$HTTP" "200"
found=$(printf '%s' "$BODY" | jq -r --arg id "$SALES_ID" '[.data.list[]? | select(.id == $id)] | length')
check "列表里能查到刚建的 SALES 合同" "$found" "1"
check "列表行的乙方现值" \
      "$(printf '%s' "$BODY" | jq -r --arg id "$SALES_ID" '[.data.list[]? | select(.id == $id)][0].party_b_current_name')" "$CUS_NAME"
db_row=$(PSQLQ -c "select contract_type||'|'||party_b_type||'|'||party_b_name from biz_contract where id='$SALES_ID'")
check "库内落库三元组（contract_type|party_b_type|party_b_name）" "$db_row" "SALES|CUSTOMER|$CUS_NAME"

echo; echo "== 4) ② SERVICE / LEASE 仍被拒（合法枚举，未开放） =="
for t in SERVICE LEASE; do
  api "reject_$t" POST /business/erp/contract "$T_ADMIN" \
      "{\"name\":\"SO-26 $t 合同\",\"contract_type\":\"$t\",\"party_b_id\":\"$CUS_ID\",\"amount\":\"1000.00\"}"
  check "POST $t HTTP" "$HTTP" "400"
  check "$t 错误码（10001=入参非法）" "$CODE" "10001"
  msg=$(d '.msg')
  case "$msg" in
    *"PURCHASE"*"SALES"*) ok "$t 文案点名放行集：$msg" ;;
    *) bad "$t 文案未点名放行集：$msg" ;;
  esac
done

echo; echo "== 5) ③ party_b_type=CUSTOMER 时校验客户存在 =="
GHOST=$(python3 -c 'import uuid;print(uuid.uuid4())')
api sales_ghost POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"SO-26 幽灵客户合同\",\"contract_type\":\"SALES\",\"party_b_id\":\"$GHOST\",
      \"amount\":\"1000.00\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",\"end_date\":\"2026-03-31\"}"
check "不存在的客户 → HTTP" "$HTTP" "400"
check "不存在的客户 → 业务码（66021 PARTY_INVALID）" "$CODE" "66021"
msg=$(d '.msg')
case "$msg" in *"乙方客户"*) ok "文案按类型称谓：$msg" ;; *) bad "文案未点名「乙方客户」：$msg" ;; esac

api sales_frozen POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"SO-26 冻结客户合同\",\"contract_type\":\"SALES\",\"party_b_id\":\"$FROZEN_ID\",
      \"amount\":\"1000.00\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",\"end_date\":\"2026-03-31\"}"
check "FROZEN 客户 → HTTP" "$HTTP" "400"
check "FROZEN 客户 → 业务码" "$CODE" "66021"
msg=$(d '.msg')
case "$msg" in *"冻结"*) ok "文案说清冻结：$msg" ;; *) bad "FROZEN 文案未说冻结：$msg" ;; esac

api sales_supplier_conflict POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"SO-26 冲突合同\",\"contract_type\":\"SALES\",\"party_b_type\":\"SUPPLIER\",
      \"party_b_id\":\"$CUS_ID\",\"amount\":\"1000.00\"}"
check "SALES + party_b_type=SUPPLIER 冲突 → HTTP" "$HTTP" "400"
msg=$(d '.msg')
case "$msg" in *"CUSTOMER"*) ok "冲突文案点名 CUSTOMER：$msg" ;; *) bad "冲突文案：$msg" ;; esac

echo; echo "== 6) 回归：PURCHASE（供应商分支）仍可建 =="
SUP_ID=$(PSQLQ -c "select id from biz_supplier where deleted_at is null and status=1 order by created_at limit 1")
SUP_NAME=$(PSQLQ -c "select supplier_name from biz_supplier where id='$SUP_ID'")
if [ -z "$SUP_ID" ]; then
  bad "库内无启用供应商，PURCHASE 回归无法执行"
else
  api purchase_create POST /business/erp/contract "$T_ADMIN" \
      "{\"name\":\"SO-26 采购回归合同\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP_ID\",
        \"amount\":\"5000.00\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",\"end_date\":\"2026-06-30\"}"
  check "POST PURCHASE HTTP" "$HTTP" "200"
  check "PURCHASE 乙方类型" "$(d '.data.party_b_type')" "SUPPLIER"
  check "PURCHASE 乙方名称快照" "$(d '.data.party_b_name')" "$SUP_NAME"
  check "PURCHASE 乙方现值（仍走 biz_supplier）" "$(d '.data.party_b_current_name')" "$SUP_NAME"
fi

echo; echo "== 7) 夹具回读（库内证据） =="
PSQLQ -c "select contract_no||' | '||contract_type||' | '||party_b_type||' | '||party_b_name||' | '||party_b_id from biz_contract where id='$SALES_ID'" \
  | tee "$RAW/db_sales_contract.txt"

echo
echo "=================================================================="
echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then printf '失败项：\n'; printf '  - %s\n' "${FAILED[@]}"; echo "结果：FAIL"; exit 1; fi
echo "结果：PASS"
