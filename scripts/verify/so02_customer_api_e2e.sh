#!/usr/bin/env bash
# SO-02 客户档案 API 的端到端验收（真 token curl 五个接口）
#
# 前置：库 cloudpivot_v0260 已迁移 + 实例已起（见 so02_start.sh），BASE 指向它。
# 用法：BASE=http://127.0.0.1:18097 bash so02_e2e.sh
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18097}
ADMIN_PASSWORD=${NIK_ADMIN_PASSWORD:-Admin@123456}
OUT=${OUT:-/opt/cloudpivot/docs/evidence/20261106-so02-customer}
mkdir -p "$OUT"
LOG="$OUT/e2e-$(date +%H%M%S).log"
: > "$LOG"

pass=0
fail=0
say() { printf '%s\n' "$*" | tee -a "$LOG"; }

# json 取值（不依赖 jq：用 python3 读 stdin）
jget() { python3 -c 'import sys,json;d=json.load(sys.stdin);
cur=d
for k in sys.argv[1].split("."):
    if cur is None: break
    if isinstance(cur,dict): cur=cur.get(k)
    elif isinstance(cur,list) and k.isdigit(): cur=cur[int(k)] if int(k)<len(cur) else None
    else: cur=None
if cur is None: print("")
elif isinstance(cur,bool): print("true" if cur else "false")
else: print(cur)' "$1"; }

check() { # check <描述> <实际> <期望>
    if [ "$2" = "$3" ]; then
        pass=$((pass+1)); say "[PASS] $1 = $2"
    else
        fail=$((fail+1)); say "[FAIL] $1 = $2（期望 $3）"
    fi
}

http() { # http <method> <path> [data]  → 输出 "STATUS|BODY"
    local method="$1" path="$2" data="${3:-}"
    if [ -n "$data" ]; then
        curl -sS -o /tmp/so02_body.json -w '%{http_code}' -X "$method" "$BASE$path" \
            -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$data"
    else
        curl -sS -o /tmp/so02_body.json -w '%{http_code}' -X "$method" "$BASE$path" \
            -H "Authorization: Bearer $TOKEN"
    fi
    printf '|%s' "$(cat /tmp/so02_body.json)"
}

say "== SO-02 客户档案 API e2e · $(date '+%F %T') · BASE=$BASE"

# ---------- 0. 登录取真实 token（首登必须先改密，改完用新口令重登） ----------
# 与 p2b1_stockin.sh / p2c2_payable.sh 同一做法：先试新口令（重跑幂等），再试初始口令。
NEW_PASS="So02_${ADMIN_PASSWORD}_new"
login_once() {
    curl -sS -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}
LOGIN_BODY=$(login_once "$NEW_PASS")
TOKEN=$(printf '%s' "$LOGIN_BODY" | jget data.access_token)
MUST_CHANGE=$(printf '%s' "$LOGIN_BODY" | jget data.must_change_password)
if [ -z "$TOKEN" ]; then
    LOGIN_BODY=$(login_once "$ADMIN_PASSWORD")
    TOKEN=$(printf '%s' "$LOGIN_BODY" | jget data.access_token)
    MUST_CHANGE=$(printf '%s' "$LOGIN_BODY" | jget data.must_change_password)
fi
if [ "$MUST_CHANGE" = "true" ]; then
    say "[..] must_change_password=true → 先走改密接口（真实令牌 + 真实密码流程）"
    code=$(curl -sS -o /tmp/so02_pwd.json -w '%{http_code}' -X PUT \
        -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
        --data-binary "{\"old_password\":\"$ADMIN_PASSWORD\",\"new_password\":\"$NEW_PASS\"}" \
        "$BASE/api/system/auth/password")
    say "PUT /api/system/auth/password → $code · $(cat /tmp/so02_pwd.json)"
    [ "$code" = "200" ] || { say "[FAIL] 首登改密失败"; exit 1; }
    TOKEN=$(login_once "$NEW_PASS" | jget data.access_token)
fi
if [ -z "$TOKEN" ]; then
    say "[FAIL] 登录失败：拿不到 access_token"
    exit 1
fi
say "[PASS] 登录成功（admin，token 长度 ${#TOKEN}）"

CODE="SO2E2E-$(date +%H%M%S)"
say "本轮测试编码 = $CODE"

# ---------- 1. POST 新建 ----------
resp=$(http POST /api/business/erp/customer \
    "{\"code\":\"  $CODE  \",\"name\":\" e2e 客户甲 \",\"tax_no\":\"91110108E2E0001XY\",
      \"contact_name\":\"张伟\",\"contact_phone\":\"010-88886666\",\"address\":\"北京市海淀区 1 号\",
      \"credit_limit\":\"123456.7890\",\"remark\":\"SO-02 e2e\"}")
status=${resp%%|*}; body=${resp#*|}
say "POST /api/business/erp/customer → $status"
say "$body"
check "POST 状态码" "$status" "200"
CID=$(printf '%s' "$body" | jget data.id)
check "POST 回填 code（trim 后）" "$(printf '%s' "$body" | jget data.code)" "$CODE"
check "POST 默认 status" "$(printf '%s' "$body" | jget data.status)" "ACTIVE"
check "POST 额度定点保真" "$(printf '%s' "$body" | jget data.credit_limit)" "123456.7890"

# ---------- 2. GET 详情 ----------
resp=$(http GET "/api/business/erp/customer/$CID")
status=${resp%%|*}; body=${resp#*|}
say "GET /api/business/erp/customer/$CID → $status"
say "$body"
check "GET 详情状态码" "$status" "200"
check "GET 详情 id 一致" "$(printf '%s' "$body" | jget data.id)" "$CID"

# ---------- 3. PUT 编辑（部分更新） ----------
resp=$(http PUT "/api/business/erp/customer/$CID" \
    "{\"name\":\"e2e 客户甲（改名）\",\"credit_limit\":\"200000.5\",\"status\":\"FROZEN\"}")
status=${resp%%|*}; body=${resp#*|}
say "PUT /api/business/erp/customer/$CID → $status"
say "$body"
check "PUT 状态码" "$status" "200"
check "PUT 改名生效" "$(printf '%s' "$body" | jget data.name)" "e2e 客户甲（改名）"
check "PUT 额度生效" "$(printf '%s' "$body" | jget data.credit_limit)" "200000.5000"
check "PUT 状态生效" "$(printf '%s' "$body" | jget data.status)" "FROZEN"
check "PUT 未传的 code 不变" "$(printf '%s' "$body" | jget data.code)" "$CODE"

# ---------- 3b. 负额度必须 400 ----------
resp=$(http POST /api/business/erp/customer \
    "{\"code\":\"$CODE-NEG\",\"name\":\"负额度客户\",\"credit_limit\":\"-1\"}")
status=${resp%%|*}; body=${resp#*|}
say "POST 负额度 → $status · $body"
check "负额度状态码" "$status" "400"
check "负额度业务码" "$(printf '%s' "$body" | jget code)" "10001"

# ---------- 3c. 重复编码必须 409 ----------
resp=$(http POST /api/business/erp/customer "{\"code\":\"$CODE\",\"name\":\"重复编码\"}")
status=${resp%%|*}; body=${resp#*|}
say "POST 重复编码 → $status · $body"
check "重复编码状态码" "$status" "409"
check "重复编码业务码" "$(printf '%s' "$body" | jget code)" "66001"

# ---------- 4. GET 分页（code / name / status 三个过滤） ----------
resp=$(http GET "/api/business/erp/customer/page?page=1&page_size=10&code=$CODE&status=FROZEN")
status=${resp%%|*}; body=${resp#*|}
say "GET /api/business/erp/customer/page?code=$CODE&status=FROZEN → $status"
say "$body"
check "分页状态码" "$status" "200"
check "分页 total（code+status 命中 1 条）" "$(printf '%s' "$body" | jget data.total)" "1"
check "分页行 id 一致" "$(printf '%s' "$body" | jget data.list.0.id)" "$CID"

resp=$(http GET "/api/business/erp/customer/page?page=1&page_size=5&name=e2e%20%E5%AE%A2%E6%88%B7")
status=${resp%%|*}; body=${resp#*|}
say "GET /api/business/erp/customer/page?name=e2e 客户 → $status"
say "$body"
check "按名称筛选状态码" "$status" "200"
check "按名称筛选命中 ≥1" "$(python3 -c 'import sys;print(1 if int(sys.argv[1])>=1 else 0)' "$(printf '%s' "$body" | jget data.total)")" "1"

# ---------- 5. DELETE 软删 ----------
resp=$(http DELETE "/api/business/erp/customer/$CID")
status=${resp%%|*}; body=${resp#*|}
say "DELETE /api/business/erp/customer/$CID → $status · $body"
check "DELETE 状态码" "$status" "200"

resp=$(http GET "/api/business/erp/customer/$CID")
status=${resp%%|*}; body=${resp#*|}
say "GET 软删后详情 → $status · $body"
check "软删后详情 404" "$status" "404"

resp=$(http GET "/api/business/erp/customer/page?page=1&page_size=10&code=$CODE")
status=${resp%%|*}; body=${resp#*|}
check "软删后分页 total=0" "$(printf '%s' "$body" | jget data.total)" "0"

# 软删后编码可复用（部分唯一索引语义）
resp=$(http POST /api/business/erp/customer "{\"code\":\"$CODE\",\"name\":\"复用编码重建\"}")
status=${resp%%|*}; body=${resp#*|}
say "POST 复用已软删编码 → $status · $body"
check "软删后编码可复用" "$status" "200"
REUSED=$(printf '%s' "$body" | jget data.id)
http DELETE "/api/business/erp/customer/$REUSED" > /dev/null

say "---------------------------------------------"
say "结果：PASS=$pass FAIL=$fail"
say "证据日志：$LOG"
[ "$fail" -eq 0 ]
