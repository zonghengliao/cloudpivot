#!/bin/bash
# CODE-04 验收：145 测试环境（cloudpivot-test，127.0.0.1:8033，库 cloudpivot_dev）
#             真实 token curl —— 资产分类(AC) / 费用类型(ET) 自动取号 + 手工覆盖 + 重复 + 预览
#
# 用法（**在 145 上跑**）：
#   ADMIN_PASS="$(sed -n 's/^NIK_ADMIN_PASSWORD=//p' /etc/cloudpivot/cloudpivot-dev.env)" bash 01_curl.sh
#   落证：同目录 ../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
#
# 前置口径（145 现状，跑之前已核对）：
#   * biz_asset_category 存量 7 条手工码（ELEC/FURNITURE/BUILDING/VEHICLE/MACHINE + 2 条 DRILL_* 夹具），
#     没有一条是 AC-NN 形态 ⇒ 首个自动号 = AC-01；
#   * biz_expense_type 存量 9 条手工码（TRAVEL/MEAL/…），同样没有 ET-NN ⇒ 首个自动号 = ET-01；
#   * biz_seq 的 AC / ET 都是 current_no=0、pad_width=2（V0337 种入）。
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8033}
ADMIN_USER=${ADMIN_USER:-admin}
# 145 的 admin 口令在 /etc/cloudpivot/cloudpivot-dev.env 里（别的卡改过初始口令后，
# `Admin@123456` 已经不是当前值）⇒ 不写死，按下面的用法注入。
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
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
check_ne() { # 两值必须不同
  if [ "$2" != "$3" ]; then say "  PASS  $1（$2 ≠ $3）"; else say "  FAIL  $1 两者相同[$2]"; FAILED=1; fi
}
check_like() { # $1=描述 $2=正则 $3=实际
  if [[ "$3" =~ $2 ]]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 不匹配[$2] 实际[$3]"; FAILED=1; fi
}

# ---------- ① 登录拿真 token ----------
# 全新库的首登有 `must_change_password` 闸（403「首次登录必须先修改初始密码」），
# 只放行改密 / 登出接口 ⇒ 先把初始口令改成 ADMIN_PASS2，再用新口令登录。
# 会话状态有 60s 缓存（`session_cache::load_session_flags`），改密后最多等一轮缓存过期。
ADMIN_PASS2=${ADMIN_PASS2:-Admin@123456x}

login() { # $1=密码 → 打印 token
  curl -s -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" > "$RAW/00_login.json"
  jq -r '.data.access_token // empty' "$RAW/00_login.json"
}

TOKEN=$(login "$ADMIN_PASS2")
if [ -z "$TOKEN" ]; then
  TOKEN=$(login "$ADMIN_PASS")
  if [ -z "$TOKEN" ]; then
    say "FATAL: 登录失败（新口令 / 初始口令都不对）"; tee -a "$SUM" < "$RAW/00_login.json"; exit 1
  fi
  HTTP=$(curl -s -o "$RAW/00_change_password.json" -w '%{http_code}' \
    -X PUT "$BASE/api/system/auth/password" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_PASS2\"}")
  say "首登改密（初始口令 → 新口令）→ HTTP $HTTP"
  if [ "$HTTP" != "200" ]; then tee -a "$SUM" < "$RAW/00_change_password.json"; exit 1; fi
  sleep 65   # 等会话状态缓存（uv:{user_id}，60s）过期
  TOKEN=$(login "$ADMIN_PASS2")
  if [ -z "$TOKEN" ]; then say "FATAL: 改密后仍登录不上"; exit 1; fi
fi
say "login ok, access_token 长度 = ${#TOKEN}"
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

CODE_ID="C04$(date +%H%M%S)"
CAT_MANUAL="CAT-BUILDING-$CODE_ID"
TYPE_MANUAL="ADMIN-FEE-$CODE_ID"

post() { # $1=文件名 $2=路径 $3=body → 打印 HTTP 码
  local name=$1 path=$2 body=$3
  curl -s -o "$RAW/$name.json" -w '%{http_code}' \
    -X POST "$BASE$path" "${AUTH[@]}" -d "$body"
}
put() { # $1=文件名 $2=路径 $3=body → 打印 HTTP 码
  local name=$1 path=$2 body=$3
  curl -s -o "$RAW/$name.json" -w '%{http_code}' \
    -X PUT "$BASE$path" "${AUTH[@]}" -d "$body"
}
get() { # $1=文件名 $2=路径 → 打印 HTTP 码
  local name=$1 path=$2
  curl -s -o "$RAW/$name.json" -w '%{http_code}' "$BASE$path" "${AUTH[@]}"
}

# ================= ② 预览（GET /next-code）：只读，不消耗号 =================
say ""
say "=== 预览接口：GET /api/business/erp/{asset-category,expense-type}/next-code ==="
HTTP=$(get 01_ac_next_code1 "/api/business/erp/asset-category/next-code")
AC_PEEK1=$(jq -r '.data.code // empty' "$RAW/01_ac_next_code1.json")
say "④ 资产分类预览 #1 → HTTP $HTTP, code=$AC_PEEK1"
check "④ 预览 HTTP = 200" "200" "$HTTP"
check_like "④ 预览形态 = AC-NN" '^AC-[0-9]{2,}$' "$AC_PEEK1"

HTTP=$(get 02_ac_next_code2 "/api/business/erp/asset-category/next-code")
AC_PEEK2=$(jq -r '.data.code // empty' "$RAW/02_ac_next_code2.json")
say "④ 资产分类预览 #2 → HTTP $HTTP, code=$AC_PEEK2"
check "④ 预览不消耗号：两次结果相同" "$AC_PEEK1" "$AC_PEEK2"

HTTP=$(get 03_et_next_code1 "/api/business/erp/expense-type/next-code")
ET_PEEK1=$(jq -r '.data.code // empty' "$RAW/03_et_next_code1.json")
say "④ 费用类型预览 #1 → HTTP $HTTP, code=$ET_PEEK1"
check "④ 预览 HTTP = 200" "200" "$HTTP"
check_like "④ 预览形态 = ET-NN" '^ET-[0-9]{2,}$' "$ET_PEEK1"

# ================= ③ 资产分类 biz_asset_category（AC / 2 位） =================
say ""
say "=== 资产分类 biz_asset_category（AC / 2 位） ==="

HTTP=$(post 04_ac_absent "/api/business/erp/asset-category" \
  "{\"category_name\":\"CODE04 分类（字段缺席）\",\"useful_life_months\":36}")
AC1=$(jq -r '.data.category_code // empty' "$RAW/04_ac_absent.json")
AC1_ID=$(jq -r '.data.id // empty' "$RAW/04_ac_absent.json")
say "① 不传 category_code       → HTTP $HTTP, code=$AC1, id=$AC1_ID"
check "① 字段缺席自动取号（2 位）" "$AC_PEEK1" "$AC1"
check "① HTTP = 200" "200" "$HTTP"

HTTP=$(post 05_ac_empty "/api/business/erp/asset-category" \
  '{"category_code":"","category_name":"CODE04 分类（空串）","useful_life_months":36}')
AC2=$(jq -r '.data.category_code // empty' "$RAW/05_ac_empty.json")
AC2_ID=$(jq -r '.data.id // empty' "$RAW/05_ac_empty.json")
say "② 传 category_code=\"\"     → HTTP $HTTP, code=$AC2"
check "② 空串自动取号且连续 +1" "$(printf 'AC-%02d' $((10#${AC1#AC-} + 1)))" "$AC2"

HTTP=$(post 06_ac_null "/api/business/erp/asset-category" \
  '{"category_code":null,"category_name":"CODE04 分类（null）","useful_life_months":36}')
AC3=$(jq -r '.data.category_code // empty' "$RAW/06_ac_null.json")
AC3_ID=$(jq -r '.data.id // empty' "$RAW/06_ac_null.json")
say "③ 传 category_code=null    → HTTP $HTTP, code=$AC3"
check "③ null 自动取号且连续 +1" "$(printf 'AC-%02d' $((10#${AC2#AC-} + 1)))" "$AC3"
check_ne "③ 三个自动号两两不同（首尾）" "$AC1" "$AC3"

HTTP=$(post 07_ac_manual "/api/business/erp/asset-category" \
  "{\"category_code\":\"$CAT_MANUAL\",\"category_name\":\"CODE04 分类（企业自有编码）\",\"useful_life_months\":120}")
AC4=$(jq -r '.data.category_code // empty' "$RAW/07_ac_manual.json")
AC4_ID=$(jq -r '.data.id // empty' "$RAW/07_ac_manual.json")
say "⑤ 传 category_code=\"$CAT_MANUAL\" → HTTP $HTTP, code=$AC4"
check "⑤ 手工编码原样保留（不强制 AC-NN 形态）" "$CAT_MANUAL" "$AC4"

HTTP=$(post 08_ac_dup "/api/business/erp/asset-category" \
  "{\"category_code\":\"$CAT_MANUAL\",\"category_name\":\"CODE04 分类（重复编码）\",\"useful_life_months\":36}")
DUP_CODE=$(jq -r '.code // empty' "$RAW/08_ac_dup.json")
DUP_MSG=$(jq -r '.msg // empty' "$RAW/08_ac_dup.json")
say "⑥ 重复 category_code       → HTTP $HTTP, code=$DUP_CODE, msg=$DUP_MSG"
check "⑥ 重复编码 HTTP = 409" "409" "$HTTP"
check "⑥ 重复编码业务码 = 66001" "66001" "$DUP_CODE"
case "$DUP_MSG" in
  *"$CAT_MANUAL"*) say "  PASS  ⑥ 文案点名具体编码" ;;
  *) say "  FAIL  ⑥ 文案没点名编码：$DUP_MSG"; FAILED=1 ;;
esac

HTTP=$(post 09_ac_bad "/api/business/erp/asset-category" \
  '{"category_code":"BAD CODE","category_name":"CODE04 分类（非法字符）","useful_life_months":36}')
BAD_CODE=$(jq -r '.code // empty' "$RAW/09_ac_bad.json")
BAD_MSG=$(jq -r '.msg // empty' "$RAW/09_ac_bad.json")
say "⑦ 非法 category_code      → HTTP $HTTP, code=$BAD_CODE, msg=$BAD_MSG"
check "⑦ 非法字符 HTTP = 400" "400" "$HTTP"
check "⑦ 非法字符业务码 = 10001" "10001" "$BAD_CODE"

# 编辑：编码留空 = 不改编码（表单编码栏只读时的提交形态）
HTTP=$(put 10_ac_update_blank "/api/business/erp/asset-category/$AC1_ID" \
  '{"category_code":"","category_name":"CODE04 分类（改名，编码留空）","useful_life_months":48}')
AC1_AFTER=$(jq -r '.data.category_code // empty' "$RAW/10_ac_update_blank.json")
say "⑧ 编辑时 category_code=\"\" → HTTP $HTTP, code=$AC1_AFTER"
check "⑧ 编辑留空不改编码" "$AC1" "$AC1_AFTER"
check "⑧ 编辑 HTTP = 200" "200" "$HTTP"

# ================= ④ 费用类型 biz_expense_type（ET / 2 位） =================
say ""
say "=== 费用类型 biz_expense_type（ET / 2 位） ==="
# 借一个末级借方科目（5602.02 差旅费，迁移种好）
ACC_ID=$(curl -s "$BASE/api/business/erp/account/page?page=1&page_size=500" "${AUTH[@]}" \
  | jq -r '.data.list[] | select(.account_code=="5602.02") | .id' | head -1)
say "借方科目 5602.02 id = $ACC_ID"
if [ -z "$ACC_ID" ]; then say "FATAL: 找不到科目 5602.02"; exit 1; fi

HTTP=$(post 11_et_absent "/api/business/erp/expense-type" \
  "{\"type_name\":\"CODE04 类型（字段缺席）\",\"account_id\":\"$ACC_ID\"}")
ET1=$(jq -r '.data.type_code // empty' "$RAW/11_et_absent.json")
ET1_ID=$(jq -r '.data.id // empty' "$RAW/11_et_absent.json")
say "① 不传 type_code          → HTTP $HTTP, code=$ET1, id=$ET1_ID"
check "① 字段缺席自动取号（2 位）" "$ET_PEEK1" "$ET1"
check "① HTTP = 200" "200" "$HTTP"

HTTP=$(post 12_et_empty "/api/business/erp/expense-type" \
  "{\"type_code\":\"\",\"type_name\":\"CODE04 类型（空串）\",\"account_id\":\"$ACC_ID\"}")
ET2=$(jq -r '.data.type_code // empty' "$RAW/12_et_empty.json")
ET2_ID=$(jq -r '.data.id // empty' "$RAW/12_et_empty.json")
say "② 传 type_code=\"\"        → HTTP $HTTP, code=$ET2"
check "② 空串自动取号且连续 +1" "$(printf 'ET-%02d' $((10#${ET1#ET-} + 1)))" "$ET2"

HTTP=$(post 13_et_null "/api/business/erp/expense-type" \
  "{\"type_code\":null,\"type_name\":\"CODE04 类型（null）\",\"account_id\":\"$ACC_ID\"}")
ET3=$(jq -r '.data.type_code // empty' "$RAW/13_et_null.json")
ET3_ID=$(jq -r '.data.id // empty' "$RAW/13_et_null.json")
say "③ 传 type_code=null       → HTTP $HTTP, code=$ET3"
check "③ null 自动取号且连续 +1" "$(printf 'ET-%02d' $((10#${ET2#ET-} + 1)))" "$ET3"
check_ne "③ 三个自动号两两不同（首尾）" "$ET1" "$ET3"

HTTP=$(post 14_et_manual "/api/business/erp/expense-type" \
  "{\"type_code\":\"$TYPE_MANUAL\",\"type_name\":\"CODE04 类型（企业自有编码）\",\"account_id\":\"$ACC_ID\"}")
ET4=$(jq -r '.data.type_code // empty' "$RAW/14_et_manual.json")
ET4_ID=$(jq -r '.data.id // empty' "$RAW/14_et_manual.json")
say "⑤ 传 type_code=\"$TYPE_MANUAL\" → HTTP $HTTP, code=$ET4"
check "⑤ 手工编码原样保留（不强制 ET-NN 形态）" "$TYPE_MANUAL" "$ET4"

HTTP=$(post 15_et_dup "/api/business/erp/expense-type" \
  "{\"type_code\":\"$TYPE_MANUAL\",\"type_name\":\"CODE04 类型（重复编码）\",\"account_id\":\"$ACC_ID\"}")
DUP_CODE=$(jq -r '.code // empty' "$RAW/15_et_dup.json")
DUP_MSG=$(jq -r '.msg // empty' "$RAW/15_et_dup.json")
say "⑥ 重复 type_code          → HTTP $HTTP, code=$DUP_CODE, msg=$DUP_MSG"
check "⑥ 重复编码 HTTP = 409" "409" "$HTTP"
check "⑥ 重复编码业务码 = 66001" "66001" "$DUP_CODE"
case "$DUP_MSG" in
  *"$TYPE_MANUAL"*) say "  PASS  ⑥ 文案点名具体编码" ;;
  *) say "  FAIL  ⑥ 文案没点名编码：$DUP_MSG"; FAILED=1 ;;
esac

HTTP=$(post 16_et_bad "/api/business/erp/expense-type" \
  "{\"type_code\":\"差旅费\",\"type_name\":\"CODE04 类型（非法字符）\",\"account_id\":\"$ACC_ID\"}")
BAD_CODE=$(jq -r '.code // empty' "$RAW/16_et_bad.json")
BAD_MSG=$(jq -r '.msg // empty' "$RAW/16_et_bad.json")
say "⑦ 非法 type_code          → HTTP $HTTP, code=$BAD_CODE, msg=$BAD_MSG"
check "⑦ 非法字符 HTTP = 400" "400" "$HTTP"
check "⑦ 非法字符业务码 = 10001" "10001" "$BAD_CODE"

# ---------- 汇总 ----------
say ""
say "创建的夹具（清理脚本用）："
say "  AC: $AC1_ID $AC2_ID $AC3_ID $AC4_ID"
say "  ET: $ET1_ID $ET2_ID $ET3_ID $ET4_ID"
{
  echo "AC1_ID=$AC1_ID"; echo "AC2_ID=$AC2_ID"; echo "AC3_ID=$AC3_ID"; echo "AC4_ID=$AC4_ID"
  echo "ET1_ID=$ET1_ID"; echo "ET2_ID=$ET2_ID"; echo "ET3_ID=$ET3_ID"; echo "ET4_ID=$ET4_ID"
  echo "CAT_MANUAL=$CAT_MANUAL"; echo "TYPE_MANUAL=$TYPE_MANUAL"
} > "$RAW/09_ids.env"

say ""
if [ "$FAILED" = "0" ]; then
  say "RESULT: ALL PASS"
else
  say "RESULT: 有 FAIL，见上"
fi
exit "$FAILED"
