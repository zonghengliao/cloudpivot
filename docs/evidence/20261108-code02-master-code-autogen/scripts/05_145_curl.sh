#!/bin/bash
# CODE-02 验收：145 真实实例 + 真实 token curl（供应商 / 客户 自动取号 + 手工覆盖 + 重复）
#
# 用法：bash 05_145_curl.sh
#   依赖：curl / jq；从 99 直连 145 的 nginx（:80 → 后端 127.0.0.1:8033）
#   落证：同目录 ../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
set -uo pipefail

BASE=${BASE:-http://172.18.10.145}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
mkdir -p "$RAW"
SUM="$RAW/00_summary.txt"
: > "$SUM"

say() { echo "$@" | tee -a "$SUM"; }

# ---------- ① 登录拿真 token ----------
curl -s -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" > "$RAW/00_login.json"
TOKEN=$(jq -r '.data.access_token // empty' "$RAW/00_login.json")
if [ -z "$TOKEN" ]; then
  say "FATAL: 登录失败"; cat "$RAW/00_login.json" | tee -a "$SUM"; exit 1
fi
say "login ok, access_token 长度 = ${#TOKEN}"
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

# 不发响应体到 stdout 的 POST：$1=文件名 $2=路径 $3=body
post() {
  local name=$1 path=$2 body=$3
  curl -s -o "$RAW/$name.json" -w '%{http_code}' \
    -X POST "$BASE$path" "${AUTH[@]}" -d "$body"
}

check() { # $1=描述 $2=期望 $3=实际
  if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi
}
FAILED=0

# ================= ② 供应商：4 条验收 =================
say ""
say "=== 供应商 biz_supplier ==="

HTTP=$(post 01_sup_blank "/api/business/erp/supplier" '{"supplier_name":"CODE02 测试供应商A","supplier_code":""}')
SUP1=$(jq -r '.data.supplier_code // empty' "$RAW/01_sup_blank.json")
say "① 传 supplier_code=\"\"  → HTTP $HTTP, supplier_code=$SUP1"
check "① 空串自动取号" "SUP-0002" "$SUP1"

HTTP=$(post 02_sup_null "/api/business/erp/supplier" '{"supplier_name":"CODE02 测试供应商B","supplier_code":null}')
SUP2=$(jq -r '.data.supplier_code // empty' "$RAW/02_sup_null.json")
say "② 传 supplier_code=null → HTTP $HTTP, supplier_code=$SUP2"
check "② null 自动取号且连续" "SUP-0003" "$SUP2"

HTTP=$(post 03_sup_manual "/api/business/erp/supplier" '{"supplier_name":"CODE02 测试供应商C","supplier_code":"GYS-001"}')
SUP3=$(jq -r '.data.supplier_code // empty' "$RAW/03_sup_manual.json")
say "③ 传 supplier_code=\"GYS-001\" → HTTP $HTTP, supplier_code=$SUP3"
check "③ 手工编码保留（非 SUP-xxx 也允许）" "GYS-001" "$SUP3"

HTTP=$(post 04_sup_dup "/api/business/erp/supplier" "{\"supplier_name\":\"CODE02 测试供应商D\",\"supplier_code\":\"$SUP1\"}")
DUP_CODE=$(jq -r '.code // empty' "$RAW/04_sup_dup.json")
DUP_MSG=$(jq -r '.msg // empty' "$RAW/04_sup_dup.json")
say "④ 传重复 supplier_code=\"$SUP1\" → HTTP $HTTP, code=$DUP_CODE, msg=$DUP_MSG"
check "④ 重复编码 HTTP 409" "409" "$HTTP"
check "④ 重复编码业务码 66001" "66001" "$DUP_CODE"
case "$DUP_MSG" in
  *"$SUP1"*) say "  PASS  ④ 文案点名具体编码";;
  *) say "  FAIL  ④ 文案没点名编码：$DUP_MSG"; FAILED=1;;
esac

HTTP=$(post 05_sup_badchars "/api/business/erp/supplier" '{"supplier_name":"CODE02 测试供应商E","supplier_code":"GYS 001"}')
BAD_CODE=$(jq -r '.code // empty' "$RAW/05_sup_badchars.json")
BAD_MSG=$(jq -r '.msg // empty' "$RAW/05_sup_badchars.json")
say "⑤ 传非法字符 supplier_code=\"GYS 001\" → HTTP $HTTP, code=$BAD_CODE, msg=$BAD_MSG"
check "⑤ 非法字符 HTTP 400" "400" "$HTTP"
check "⑤ 非法字符业务码 10001" "10001" "$BAD_CODE"

# ================= ③ 客户：同样 4 条 =================
say ""
say "=== 客户 biz_customer ==="

HTTP=$(post 11_cus_blank "/api/business/erp/customer" '{"name":"CODE02 测试客户A","code":""}')
CUS1=$(jq -r '.data.code // empty' "$RAW/11_cus_blank.json")
say "① 传 code=\"\" → HTTP $HTTP, code=$CUS1"
check "① 空串自动取号" "CUS-0004" "$CUS1"

HTTP=$(post 12_cus_null "/api/business/erp/customer" '{"name":"CODE02 测试客户B"}')
CUS2=$(jq -r '.data.code // empty' "$RAW/12_cus_null.json")
say "② 字段缺席（= null）→ HTTP $HTTP, code=$CUS2"
check "② null 自动取号且连续" "CUS-0005" "$CUS2"

HTTP=$(post 13_cus_manual "/api/business/erp/customer" '{"name":"CODE02 测试客户C","code":"KH-001"}')
CUS3=$(jq -r '.data.code // empty' "$RAW/13_cus_manual.json")
say "③ 传 code=\"KH-001\" → HTTP $HTTP, code=$CUS3"
check "③ 手工编码保留" "KH-001" "$CUS3"

HTTP=$(post 14_cus_dup "/api/business/erp/customer" "{\"name\":\"CODE02 测试客户D\",\"code\":\"$CUS1\"}")
CDUP_CODE=$(jq -r '.code // empty' "$RAW/14_cus_dup.json")
CDUP_MSG=$(jq -r '.msg // empty' "$RAW/14_cus_dup.json")
say "④ 传重复 code=\"$CUS1\" → HTTP $HTTP, code=$CDUP_CODE, msg=$CDUP_MSG"
check "④ 重复编码 HTTP 409" "409" "$HTTP"
check "④ 重复编码业务码 66001" "66001" "$CDUP_CODE"
case "$CDUP_MSG" in
  *"$CUS1"*) say "  PASS  ④ 文案点名具体编码";;
  *) say "  FAIL  ④ 文案没点名编码：$CDUP_MSG"; FAILED=1;;
esac

say ""
if [ "$FAILED" -eq 0 ]; then say "RESULT: ALL PASS"; else say "RESULT: HAS FAILURES"; fi
# 把本轮造出来的编码留给 06_db_assert.sh / 07_cleanup.sh 用
printf '%s\n%s\n%s\n%s\n%s\n' "${SUP1:-}" "${SUP2:-}" "${SUP3:-}" "${CUS1:-}" "${CUS2:-}" > "$RAW/90_created_codes.txt"
printf '%s\n' "${CUS3:-}" >> "$RAW/90_created_codes.txt"
exit "$FAILED"
