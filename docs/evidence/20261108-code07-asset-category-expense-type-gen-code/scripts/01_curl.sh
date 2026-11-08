#!/bin/bash
# CODE-07 验收（真实 token curl，隔离实例 127.0.0.1:8047，库 cloudpivot_code07e2e = 全新空库）
#
# 卡面：资产分类 asset-category/index.vue → AC-01，费用类型 expense-type/index.vue → ET-01，
# 每个表单 4 条用例（交互方式 B：编码栏 + 「生成编码」按钮，点按钮先取号预览，保存时用该号；
# 不点则保存时自动取号；必须允许手工覆盖）。
#
# 后端部分（CODE-04 已交付，本卡只验证前端所依赖的契约）：
#   GET /api/business/erp/asset-category/next-code
#   GET /api/business/erp/expense-type/next-code
#
# 断言与前端一一对应：
#   ① 预览：形态 AC-NN / ET-NN（2 位纯流水、不计日期）+ 连点两次同号（只读不占号）
#   ② 不点生成直接保存（category_code 字段缺席 / type_code=null）⇒ 后端取号，且号 == 预览号
#      （前端「不点则保存时自动取号」，前端发 null，见 master-code.ts 的 toMasterCodeInput）
#   ③ 手工编码原样保留（CAT-BUILDING-… / ADMIN-FEE-…，企业自有编码体系对接）
#   ④ 预览号当手工码提交 ⇒ 原样落库；再提交一次 ⇒ 409 + 66001 且文案点名编码；
#      号被用掉后预览往后走（前端「撞号就再点一次生成编码」的兜底）
#   ⑤ 无 token 访问两个预览接口 ⇒ 401
#   ⑥ 编辑（PUT）编码留空 = 不改编码（前端编辑态编码栏 + 按钮双禁用时的提交形态）
#
# 用法：bash 01_curl.sh          （实例已 start）
# 落证：../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8047}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
ADMIN_PASS2=${ADMIN_PASS2:-Admin@123456x}
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
mkdir -p "$RAW"
SUM="$RAW/00_summary.txt"
: > "$SUM"

say() { echo "$@" | tee -a "$SUM"; }

FAILED=0
check() { # $1=描述 $2=期望 $3=实际
  if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi
}
check_ne() {
  if [ "$2" != "$3" ]; then say "  PASS  $1（$2 ≠ $3）"; else say "  FAIL  $1 两者相同[$2]"; FAILED=1; fi
}
check_like() { # $1=描述 $2=正则 $3=实际
  if [[ "$3" =~ $2 ]]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 不匹配[$2] 实际[$3]"; FAILED=1; fi
}

# ---------- ⓿ 登录拿真 token ----------
# 全新库首登有 `must_change_password` 闸（业务接口 403），只放行改密 / 登出
# ⇒ 改成 ADMIN_PASS2 再登。会话状态有 60s 缓存，改密后等一轮过期。
login() {
  curl -s -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" > "$RAW/00_login.json"
  jq -r '.data.access_token // empty' "$RAW/00_login.json"
}

TOKEN=$(login "$ADMIN_PASS")
if [ -z "$TOKEN" ]; then TOKEN=$(login "$ADMIN_PASS2"); fi
if [ -z "$TOKEN" ]; then say "FATAL: 登录失败（初始口令 / 新口令都不对）"; tee -a "$SUM" < "$RAW/00_login.json"; exit 1; fi
say "login ok, access_token 长度 = ${#TOKEN}"

PROBE=$(curl -s -o "$RAW/00_probe.json" -w '%{http_code}' \
  -H "Authorization: Bearer ${TOKEN}" "$BASE/api/business/erp/asset-category/next-code")
if [ "$PROBE" = "403" ] && grep -q "首次登录必须先修改初始密码" "$RAW/00_probe.json"; then
  HTTP=$(curl -s -o "$RAW/00_change_password.json" -w '%{http_code}' \
    -X PUT "$BASE/api/system/auth/password" \
    -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_PASS2\"}")
  say "首登改密（初始口令 → 新口令）→ HTTP $HTTP"
  if [ "$HTTP" != "200" ]; then tee -a "$SUM" < "$RAW/00_change_password.json"; exit 1; fi
  sleep 65   # 等会话状态缓存（uv:{user_id}，60s）过期
  TOKEN=$(login "$ADMIN_PASS2")
  if [ -z "$TOKEN" ]; then say "FATAL: 改密后仍登录不上"; exit 1; fi
fi

AUTH=(-H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json')
post() { local n=$1 p=$2 b=$3; curl -s -o "$RAW/$n.json" -w '%{http_code}' -X POST "$BASE$p" "${AUTH[@]}" -d "$b"; }
put()  { local n=$1 p=$2 b=$3; curl -s -o "$RAW/$n.json" -w '%{http_code}' -X PUT  "$BASE$p" "${AUTH[@]}" -d "$b"; }
get()  { local n=$1 p=$2;    curl -s -o "$RAW/$n.json" -w '%{http_code}' "$BASE$p" "${AUTH[@]}"; }

STAMP=$(date +%H%M%S)
AC_MANUAL="CAT-BUILDING-$STAMP"
ET_MANUAL="ADMIN-FEE-$STAMP"

# ================================================================
# 一、资产分类 AC（2 位纯流水，不计日期）
# ================================================================
say ""
say "=== 资产分类：① 预览接口 GET /api/business/erp/asset-category/next-code（只读不占号） ==="
HTTP=$(get 10_ac_next1 "/api/business/erp/asset-category/next-code")
AC_P1=$(jq -r '.data.code // empty' "$RAW/10_ac_next1.json")
say "  预览 #1 → HTTP $HTTP, code=$AC_P1"
check "① 预览 HTTP = 200" "200" "$HTTP"
check_like "① 预览形态 = AC-NN（2 位纯流水、无日期）" '^AC-[0-9]{2}$' "$AC_P1"

HTTP=$(get 11_ac_next2 "/api/business/erp/asset-category/next-code")
AC_P2=$(jq -r '.data.code // empty' "$RAW/11_ac_next2.json")
say "  预览 #2 → HTTP $HTTP, code=$AC_P2"
check "① 连点两次同号（点了又取消不浪费号）" "$AC_P1" "$AC_P2"

say ""
say "=== 资产分类：② 不点「生成编码」直接保存（编码字段缺席 / null）⇒ 自动取号 = 预览号 ==="
HTTP=$(post 20_ac_absent "/api/business/erp/asset-category" \
  "{\"category_name\":\"CODE07 分类（字段缺席，未点生成）\",\"useful_life_months\":36}")
AC_A1=$(jq -r '.data.category_code // empty' "$RAW/20_ac_absent.json")
AC_A1_ID=$(jq -r '.data.id // empty' "$RAW/20_ac_absent.json")
say "  POST 不带 category_code → HTTP $HTTP, code=$AC_A1, id=$AC_A1_ID"
check "② HTTP = 200" "200" "$HTTP"
check "② 自动号 == 刚才预览的号" "$AC_P1" "$AC_A1"

HTTP=$(post 21_ac_null "/api/business/erp/asset-category" \
  "{\"category_code\":null,\"category_name\":\"CODE07 分类（前端发 null）\",\"useful_life_months\":36}")
AC_A2=$(jq -r '.data.category_code // empty' "$RAW/21_ac_null.json")
say "  POST category_code=null（前端 toMasterCodeInput('') 的产物）→ HTTP $HTTP, code=$AC_A2"
check "② null 也自动取号，且连续 +1" "$(printf 'AC-%02d' $((10#${AC_A1#AC-} + 1)))" "$AC_A2"
check_like "② 号仍是 2 位纯流水" '^AC-[0-9]{2}$' "$AC_A2"

say ""
say "=== 资产分类：③ 手工编码原样保留（企业已有编码体系对接） ==="
HTTP=$(post 22_ac_manual "/api/business/erp/asset-category" \
  "{\"category_code\":\"$(echo "$AC_MANUAL" | tr 'A-Z' 'a-z')\",\"category_name\":\"CODE07 分类（企业自有编码，小写提交）\",\"useful_life_months\":120}")
AC_M=$(jq -r '.data.category_code // empty' "$RAW/22_ac_manual.json")
say "  POST category_code=$AC_MANUAL（小写提交）→ HTTP $HTTP, code=$AC_M"
check "③ 手工编码原样保留（只转大写，不掰成 AC-NN）" "$AC_MANUAL" "$AC_M"

say ""
say "=== 资产分类：④ 预览号当手工码提交（原样落库）+ 撞号 409+66001 + 号被用掉后预览往后走 ==="
HTTP=$(get 23_ac_next3 "/api/business/erp/asset-category/next-code")
AC_P3=$(jq -r '.data.code // empty' "$RAW/23_ac_next3.json")
say "  预览 #3 → HTTP $HTTP, code=$AC_P3（应为 $AC_A2 的下一个）"
check "④ 预览跟上已用掉的号" "$(printf 'AC-%02d' $((10#${AC_A2#AC-} + 1)))" "$AC_P3"

HTTP=$(post 24_ac_use_previewed "/api/business/erp/asset-category" \
  "{\"category_code\":\"$AC_P3\",\"category_name\":\"CODE07 分类（用预览号建单）\",\"useful_life_months\":24}")
AC_U=$(jq -r '.data.category_code // empty' "$RAW/24_ac_use_previewed.json")
say "  POST category_code=$AC_P3 → HTTP $HTTP, code=$AC_U"
check "④ 预览号建单 HTTP = 200" "200" "$HTTP"
check "④ 预览号原样保留" "$AC_P3" "$AC_U"

HTTP=$(post 25_ac_dup "/api/business/erp/asset-category" \
  "{\"category_code\":\"$AC_P3\",\"category_name\":\"CODE07 分类（撞号）\",\"useful_life_months\":24}")
DUP_CODE=$(jq -r '.code // empty' "$RAW/25_ac_dup.json")
DUP_MSG=$(jq -r '.msg // empty' "$RAW/25_ac_dup.json")
say "  再提交同一个号 → HTTP $HTTP, code=$DUP_CODE, msg=$DUP_MSG"
check "④ 撞号 HTTP = 409" "409" "$HTTP"
check "④ 撞号业务码 = 66001" "66001" "$DUP_CODE"
case "$DUP_MSG" in
  *"$AC_P3"*) say "  PASS  ④ 撞号文案点名编码 $AC_P3" ;;
  *) say "  FAIL  ④ 撞号文案没点名编码：$DUP_MSG"; FAILED=1 ;;
esac

HTTP=$(get 26_ac_next4 "/api/business/erp/asset-category/next-code")
AC_P4=$(jq -r '.data.code // empty' "$RAW/26_ac_next4.json")
say "  预览 #4 → code=$AC_P4"
check "④ 号被用掉后预览继续往后走" "$(printf 'AC-%02d' $((10#${AC_P3#AC-} + 1)))" "$AC_P4"

say ""
say "=== 资产分类：⑤ 无 token 访问预览接口 ⇒ 401 ==="
HTTP=$(curl -s -o "$RAW/27_ac_next_noauth.json" -w '%{http_code}' \
  "$BASE/api/business/erp/asset-category/next-code")
say "  无 token → HTTP $HTTP"
check "⑤ 无 token = 401" "401" "$HTTP"

say ""
say "=== 资产分类：⑥ 编辑（PUT）编码留空 = 不改编码（编辑态编码栏只读的提交形态） ==="
HTTP=$(put 28_ac_update_blank "/api/business/erp/asset-category/$AC_A1_ID" \
  "{\"category_code\":\"\",\"category_name\":\"CODE07 分类（改名，编码留空）\",\"useful_life_months\":48}")
AC_AFTER=$(jq -r '.data.category_code // empty' "$RAW/28_ac_update_blank.json")
say "  PUT category_code=\"\" → HTTP $HTTP, code=$AC_AFTER"
check "⑥ 编辑 HTTP = 200" "200" "$HTTP"
check "⑥ 编辑留空不改编码" "$AC_A1" "$AC_AFTER"

# ================================================================
# 二、费用类型 ET（2 位纯流水，不计日期）
# ================================================================
say ""
say "=== 费用类型：① 预览接口 GET /api/business/erp/expense-type/next-code（只读不占号） ==="
HTTP=$(get 30_et_next1 "/api/business/erp/expense-type/next-code")
ET_P1=$(jq -r '.data.code // empty' "$RAW/30_et_next1.json")
say "  预览 #1 → HTTP $HTTP, code=$ET_P1"
check "① 预览 HTTP = 200" "200" "$HTTP"
check_like "① 预览形态 = ET-NN（2 位纯流水、无日期）" '^ET-[0-9]{2}$' "$ET_P1"

HTTP=$(get 31_et_next2 "/api/business/erp/expense-type/next-code")
ET_P2=$(jq -r '.data.code // empty' "$RAW/31_et_next2.json")
say "  预览 #2 → HTTP $HTTP, code=$ET_P2"
check "① 连点两次同号（点了又取消不浪费号）" "$ET_P1" "$ET_P2"

# 借一个末级借方科目（5602.02 差旅费，迁移种好）
ACC_ID=$(curl -s "$BASE/api/business/erp/account/page?page=1&page_size=500" "${AUTH[@]}" \
  | jq -r '.data.list[] | select(.account_code=="5602.02") | .id' | head -1)
say "  借方科目 5602.02 id = $ACC_ID"
if [ -z "$ACC_ID" ]; then say "FATAL: 找不到科目 5602.02"; exit 1; fi

say ""
say "=== 费用类型：② 不点「生成编码」直接保存（字段缺席 / null）⇒ 自动取号 = 预览号 ==="
HTTP=$(post 32_et_absent "/api/business/erp/expense-type" \
  "{\"type_name\":\"CODE07 类型（字段缺席，未点生成）\",\"account_id\":\"$ACC_ID\"}")
ET_A1=$(jq -r '.data.type_code // empty' "$RAW/32_et_absent.json")
ET_A1_ID=$(jq -r '.data.id // empty' "$RAW/32_et_absent.json")
say "  POST 不带 type_code → HTTP $HTTP, code=$ET_A1, id=$ET_A1_ID"
check "② HTTP = 200" "200" "$HTTP"
check "② 自动号 == 刚才预览的号" "$ET_P1" "$ET_A1"

HTTP=$(post 33_et_null "/api/business/erp/expense-type" \
  "{\"type_code\":null,\"type_name\":\"CODE07 类型（前端发 null）\",\"account_id\":\"$ACC_ID\"}")
ET_A2=$(jq -r '.data.type_code // empty' "$RAW/33_et_null.json")
say "  POST type_code=null → HTTP $HTTP, code=$ET_A2"
check "② null 也自动取号，且连续 +1" "$(printf 'ET-%02d' $((10#${ET_A1#ET-} + 1)))" "$ET_A2"
check_like "② 号仍是 2 位纯流水" '^ET-[0-9]{2}$' "$ET_A2"

say ""
say "=== 费用类型：③ 手工编码原样保留（企业已有编码体系对接） ==="
HTTP=$(post 34_et_manual "/api/business/erp/expense-type" \
  "{\"type_code\":\"$(echo "$ET_MANUAL" | tr 'A-Z' 'a-z')\",\"type_name\":\"CODE07 类型（企业自有编码，小写提交）\",\"account_id\":\"$ACC_ID\"}")
ET_M=$(jq -r '.data.type_code // empty' "$RAW/34_et_manual.json")
say "  POST type_code=$ET_MANUAL（小写提交）→ HTTP $HTTP, code=$ET_M"
check "③ 手工编码原样保留（只转大写，不掰成 ET-NN）" "$ET_MANUAL" "$ET_M"

say ""
say "=== 费用类型：④ 预览号当手工码提交 + 撞号 409+66001 + 号被用掉后预览往后走 ==="
HTTP=$(get 35_et_next3 "/api/business/erp/expense-type/next-code")
ET_P3=$(jq -r '.data.code // empty' "$RAW/35_et_next3.json")
say "  预览 #3 → HTTP $HTTP, code=$ET_P3"
check "④ 预览跟上已用掉的号" "$(printf 'ET-%02d' $((10#${ET_A2#ET-} + 1)))" "$ET_P3"

HTTP=$(post 36_et_use_previewed "/api/business/erp/expense-type" \
  "{\"type_code\":\"$ET_P3\",\"type_name\":\"CODE07 类型（用预览号建单）\",\"account_id\":\"$ACC_ID\"}")
ET_U=$(jq -r '.data.type_code // empty' "$RAW/36_et_use_previewed.json")
say "  POST type_code=$ET_P3 → HTTP $HTTP, code=$ET_U"
check "④ 预览号建单 HTTP = 200" "200" "$HTTP"
check "④ 预览号原样保留" "$ET_P3" "$ET_U"

HTTP=$(post 37_et_dup "/api/business/erp/expense-type" \
  "{\"type_code\":\"$ET_P3\",\"type_name\":\"CODE07 类型（撞号）\",\"account_id\":\"$ACC_ID\"}")
DUP_CODE=$(jq -r '.code // empty' "$RAW/37_et_dup.json")
DUP_MSG=$(jq -r '.msg // empty' "$RAW/37_et_dup.json")
say "  再提交同一个号 → HTTP $HTTP, code=$DUP_CODE, msg=$DUP_MSG"
check "④ 撞号 HTTP = 409" "409" "$HTTP"
check "④ 撞号业务码 = 66001" "66001" "$DUP_CODE"
case "$DUP_MSG" in
  *"$ET_P3"*) say "  PASS  ④ 撞号文案点名编码 $ET_P3" ;;
  *) say "  FAIL  ④ 撞号文案没点名编码：$DUP_MSG"; FAILED=1 ;;
esac

HTTP=$(get 38_et_next4 "/api/business/erp/expense-type/next-code")
ET_P4=$(jq -r '.data.code // empty' "$RAW/38_et_next4.json")
say "  预览 #4 → code=$ET_P4"
check "④ 号被用掉后预览继续往后走" "$(printf 'ET-%02d' $((10#${ET_P3#ET-} + 1)))" "$ET_P4"

say ""
say "=== 费用类型：⑤ 无 token 访问预览接口 ⇒ 401 ==="
HTTP=$(curl -s -o "$RAW/39_et_next_noauth.json" -w '%{http_code}' \
  "$BASE/api/business/erp/expense-type/next-code")
say "  无 token → HTTP $HTTP"
check "⑤ 无 token = 401" "401" "$HTTP"

say ""
say "=== 费用类型：⑥ 编辑（PUT）编码留空 = 不改编码 ==="
HTTP=$(put 40_et_update_blank "/api/business/erp/expense-type/$ET_A1_ID" \
  "{\"type_code\":\"\",\"type_name\":\"CODE07 类型（改名，编码留空）\",\"account_id\":\"$ACC_ID\"}")
ET_AFTER=$(jq -r '.data.type_code // empty' "$RAW/40_et_update_blank.json")
say "  PUT type_code=\"\" → HTTP $HTTP, code=$ET_AFTER"
check "⑥ 编辑 HTTP = 200" "200" "$HTTP"
check "⑥ 编辑留空不改编码" "$ET_A1" "$ET_AFTER"

# ---------- 汇总 ----------
{
  echo "AC_A1_ID=$AC_A1_ID"; echo "AC_A2=$AC_A2"; echo "AC_M=$AC_M"; echo "AC_U=$AC_U"; echo "AC_P4=$AC_P4"
  echo "ET_A1_ID=$ET_A1_ID"; echo "ET_A2=$ET_A2"; echo "ET_M=$ET_M"; echo "ET_U=$ET_U"; echo "ET_P4=$ET_P4"
  echo "AC_MANUAL=$AC_MANUAL"; echo "ET_MANUAL=$ET_MANUAL"
  echo "ACC_ID=$ACC_ID"; echo "DB=cloudpivot_code07e2e"
} > "$RAW/09_fixtures.env"

say ""
if [ "$FAILED" = "0" ]; then
  say "RESULT: ALL PASS"
else
  say "RESULT: 有 FAIL，见上"
fi
exit "$FAILED"
