#!/bin/bash
# CODE-05 验收（真实 token curl，隔离实例 127.0.0.1:8045，库 cloudpivot_code05 = 全新空库）
#
# 覆盖卡面验收 ①–⑥：
#   ① 新建时编码栏可留空（后端口径：字段缺席 / null / 空串 / 纯空白 都自动取号）
#   ② 预览接口给 SUP-000X / CUS-000X（形态 + 只读不占号）
#   ③ 手工编码（GYS-001 / KH-001）原样保留
#   ④ 不点生成直接保存 → 自动生成，响应里就带回真正用的编码（前端据此回填）
#   ⑤ 编辑时编码只读（PUT 不带编码 ⇒ 编码不变）
#   ⑥ 撞号兜底：预览号被别人用掉后重复提交 → 409 + 66001 且文案点名编码
#
# 用法：bash 01_curl.sh          （实例已 start）
# 落证：../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8045}
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
# 「下一个号」：SUP-0002 → SUP-0003（空值/畸形返回空，避免 arithmetic 报错掩盖真问题）
next_code() { # $1=前缀 $2=当前编码
  local prefix=$1 digits=${2#"$1"-}
  if ! [[ "$digits" =~ ^[0-9]+$ ]]; then echo ""; return; fi
  printf '%s-%04d' "$prefix" $((10#$digits + 1))
}

# ---------- ⓿ 登录拿真 token ----------
# 全新库的 admin 口令就是启动时注入的 NIK_ADMIN_PASSWORD（本脚本 Admin@123456）。
# 首次登录若被 `must_change_password` 闸挡住，login 仍会返回 token，但业务接口 403
# ⇒ 这时才走改密（改密后等 60s 会话缓存过期再登）。
login() {
  curl -s -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" > "$RAW/00_login.json"
  jq -r '.data.access_token // empty' "$RAW/00_login.json"
}

TOKEN=$(login "$ADMIN_PASS")
if [ -z "$TOKEN" ]; then
  TOKEN=$(login "$ADMIN_PASS2")
fi
if [ -z "$TOKEN" ]; then
  say "FATAL: 登录失败（初始口令 / 新口令都不对）"
  tee -a "$SUM" < "$RAW/00_login.json"
  exit 1
fi
say "login ok, access_token 长度 = ${#TOKEN}"

# 全新库首登有 `must_change_password` 闸（业务接口 403「首次登录必须先修改初始密码」），
# 只有改密 / 登出接口放行 ⇒ 改成 ADMIN_PASS2 再用新口令登录。
# 会话状态有 60s 缓存（session_cache::load_session_flags），改密后最多等一轮缓存过期。
PROBE=$(curl -s -o "$RAW/00_probe.json" -w '%{http_code}' \
  -H "Authorization: Bearer $TOKEN" "$BASE/api/business/erp/supplier/next-code")
if [ "$PROBE" = "403" ] && grep -q "首次登录必须先修改初始密码" "$RAW/00_probe.json"; then
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
say ""
say ""
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

post() { local n=$1 p=$2 b=$3; curl -s -o "$RAW/$n.json" -w '%{http_code}' -X POST "$BASE$p" "${AUTH[@]}" -d "$b"; }
put()  { local n=$1 p=$2 b=$3; curl -s -o "$RAW/$n.json" -w '%{http_code}' -X PUT  "$BASE$p" "${AUTH[@]}" -d "$b"; }
get()  { local n=$1 p=$2;    curl -s -o "$RAW/$n.json" -w '%{http_code}' "$BASE$p" "${AUTH[@]}"; }

# ================================================================
# 一、供应商：预览接口（GET /business/erp/supplier/next-code）
# ================================================================
say "=== ① 供应商预览接口 GET /api/business/erp/supplier/next-code ==="
HTTP=$(get 10_sup_next1 "/api/business/erp/supplier/next-code")
SUP_P1=$(jq -r '.data.code // empty' "$RAW/10_sup_next1.json")
say "预览 #1 → HTTP $HTTP, code=$SUP_P1"
check "① 预览 HTTP = 200" "200" "$HTTP"
check_like "① 预览形态 = SUP-NNNN（纯流水 4 位）" '^SUP-[0-9]{4}$' "$SUP_P1"

HTTP=$(get 11_sup_next2 "/api/business/erp/supplier/next-code")
SUP_P2=$(jq -r '.data.code // empty' "$RAW/11_sup_next2.json")
say "预览 #2 → HTTP $HTTP, code=$SUP_P2"
check "① 预览只读不占号：连点两次同号" "$SUP_P1" "$SUP_P2"

# ================================================================
# 二、供应商：不点生成直接保存（编码字段缺席）⇒ 后端取号 = 预览号
# ================================================================
say ""
say "=== ② 供应商：编码留空保存 ⇒ 自动取号（前端「不点生成」路径） ==="
HTTP=$(post 12_sup_auto_absent "/api/business/erp/supplier" \
  '{"supplier_name":"CODE05 自动编码供应商（字段缺席）","credit_days":0}')
SUP_A1=$(jq -r '.data.supplier_code // empty' "$RAW/12_sup_auto_absent.json")
SUP_A1_ID=$(jq -r '.data.id // empty' "$RAW/12_sup_auto_absent.json")
say "POST 不带 supplier_code → HTTP $HTTP, code=$SUP_A1, id=$SUP_A1_ID"
check "② HTTP = 200" "200" "$HTTP"
check "② 自动号 == 刚才预览的号（预览没占号，取号在保存时）" "$SUP_P1" "$SUP_A1"

HTTP=$(get 13_sup_next3 "/api/business/erp/supplier/next-code")
SUP_P3=$(jq -r '.data.code // empty' "$RAW/13_sup_next3.json")
say "保存后再预览 → code=$SUP_P3"
check_ne "② 号被用掉后预览往后走" "$SUP_A1" "$SUP_P3"

HTTP=$(post 14_sup_auto_empty "/api/business/erp/supplier" \
  '{"supplier_code":"","supplier_name":"CODE05 自动编码供应商（空串）"}')
SUP_A2=$(jq -r '.data.supplier_code // empty' "$RAW/14_sup_auto_empty.json")
SUP_A2_ID=$(jq -r '.data.id // empty' "$RAW/14_sup_auto_empty.json")
say "POST supplier_code=\"\" → HTTP $HTTP, code=$SUP_A2"
check "② 空串也自动取号（形态不变）" "$(next_code SUP "$SUP_A1")" "$SUP_A2"

HTTP=$(post 15_sup_auto_null "/api/business/erp/supplier" \
  '{"supplier_code":null,"supplier_name":"CODE05 自动编码供应商（null）"}')
SUP_A3=$(jq -r '.data.supplier_code // empty' "$RAW/15_sup_auto_null.json")
SUP_A3_ID=$(jq -r '.data.id // empty' "$RAW/15_sup_auto_null.json")
say "POST supplier_code=null → HTTP $HTTP, code=$SUP_A3"
check "② null 也自动取号（连续 +1）" "$(next_code SUP "$SUP_A2")" "$SUP_A3"

# ================================================================
# 三、供应商：手工编码（验收 ③）+ 重复（验收 ⑥）
# ================================================================
say ""
say "=== ③ 供应商：手工编码原样保留（企业自有体系对接） ==="
HTTP=$(post 16_sup_manual "/api/business/erp/supplier" \
  '{"supplier_code":"GYS-001","supplier_name":"CODE05 手工编码供应商"}')
SUP_M=$(jq -r '.data.supplier_code // empty' "$RAW/16_sup_manual.json")
SUP_M_ID=$(jq -r '.data.id // empty' "$RAW/16_sup_manual.json")
say "POST supplier_code=GYS-001 → HTTP $HTTP, code=$SUP_M"
check "③ 手工编码原样保留（不被改成 SUP-NNNN）" "GYS-001" "$SUP_M"

HTTP=$(post 17_sup_dup "/api/business/erp/supplier" \
  '{"supplier_code":"GYS-001","supplier_name":"CODE05 重复编码供应商"}')
SUP_DUP_CODE=$(jq -r '.code // empty' "$RAW/17_sup_dup.json")
SUP_DUP_MSG=$(jq -r '.msg // empty' "$RAW/17_sup_dup.json")
say "重复 GYS-001 → HTTP $HTTP, 业务码=$SUP_DUP_CODE, msg=$SUP_DUP_MSG"
check "⑥ 重复编码 HTTP = 409" "409" "$HTTP"
check "⑥ 重复编码业务码 = 66001" "66001" "$SUP_DUP_CODE"
case "$SUP_DUP_MSG" in
  *GYS-001*) say "  PASS  ⑥ 文案点名具体编码" ;;
  *) say "  FAIL  ⑥ 文案没点名编码：$SUP_DUP_MSG"; FAILED=1 ;;
esac

# ================================================================
# 四、供应商：交互方式 B 全链路（点生成 → 用该号保存 → 成功）
# ================================================================
say ""
say "=== ④ 供应商：交互方式 B 全链路（点「生成编码」→ 填的号保存 → 成功） ==="
HTTP=$(get 18_sup_next4 "/api/business/erp/supplier/next-code")
SUP_P4=$(jq -r '.data.code // empty' "$RAW/18_sup_next4.json")
say "点「生成编码」→ 预览 code=$SUP_P4"
check_like "④ 预览形态仍是 SUP-NNNN" '^SUP-[0-9]{4}$' "$SUP_P4"

HTTP=$(post 19_sup_use_previewed "/api/business/erp/supplier" \
  "{\"supplier_code\":\"$SUP_P4\",\"supplier_name\":\"CODE05 用预览号建单供应商\"}")
SUP_USED=$(jq -r '.data.supplier_code // empty' "$RAW/19_sup_use_previewed.json")
SUP_USED_ID=$(jq -r '.data.id // empty' "$RAW/19_sup_use_previewed.json")
say "用该号保存 → HTTP $HTTP, code=$SUP_USED"
check "④ HTTP = 200" "200" "$HTTP"
check "④ 保存结果就是预览看到的那个号" "$SUP_P4" "$SUP_USED"

# 并发抢号兜底：同一个号再提交一次（模拟别人先保存了）
HTTP=$(post 20_sup_stolen "/api/business/erp/supplier" \
  "{\"supplier_code\":\"$SUP_P4\",\"supplier_name\":\"CODE05 抢号失败供应商\"}")
SUP_STOLEN_CODE=$(jq -r '.code // empty' "$RAW/20_sup_stolen.json")
say "同号再提交（模拟被抢） → HTTP $HTTP, 业务码=$SUP_STOLEN_CODE"
check "⑥ 被抢号后提交 HTTP = 409" "409" "$HTTP"
check "⑥ 被抢号后业务码 = 66001" "66001" "$SUP_STOLEN_CODE"

HTTP=$(post 21_sup_bad "/api/business/erp/supplier" \
  '{"supplier_code":"BAD CODE","supplier_name":"CODE05 非法字符供应商"}')
SUP_BAD_CODE=$(jq -r '.code // empty' "$RAW/21_sup_bad.json")
say "非法编码 \"BAD CODE\" → HTTP $HTTP, 业务码=$SUP_BAD_CODE"
check "编码含空格 HTTP = 400" "400" "$HTTP"
check "编码含空格业务码 = 10001" "10001" "$SUP_BAD_CODE"

# ================================================================
# 五、供应商编辑：编码只读（验收 ⑤）
# ================================================================
say ""
say "=== ⑤ 供应商编辑：不带编码 ⇒ 编码不变（前端编辑态编码框 + 按钮都禁用） ==="
HTTP=$(put 22_sup_update "/api/business/erp/supplier/$SUP_A1_ID" \
  '{"supplier_name":"CODE05 自动编码供应商（改名）","credit_days":30}')
SUP_A1_AFTER=$(jq -r '.data.supplier_code // empty' "$RAW/22_sup_update.json")
SUP_A1_NAME=$(jq -r '.data.supplier_name // empty' "$RAW/22_sup_update.json")
say "PUT 只改名称 → HTTP $HTTP, code=$SUP_A1_AFTER, name=$SUP_A1_NAME"
check "⑤ 编辑 HTTP = 200" "200" "$HTTP"
check "⑤ 编辑不改编码" "$SUP_A1" "$SUP_A1_AFTER"

# ================================================================
# 六、客户：同四条（预览 / 自动 / 手工 / 重复 / 编辑）
# ================================================================
say ""
say "=== ⑥ 客户预览接口 GET /api/business/erp/customer/next-code ==="
HTTP=$(get 30_cus_next1 "/api/business/erp/customer/next-code")
CUS_P1=$(jq -r '.data.code // empty' "$RAW/30_cus_next1.json")
say "预览 #1 → HTTP $HTTP, code=$CUS_P1"
check "⑥ 预览 HTTP = 200" "200" "$HTTP"
check_like "⑥ 预览形态 = CUS-NNNN" '^CUS-[0-9]{4}$' "$CUS_P1"

HTTP=$(get 31_cus_next2 "/api/business/erp/customer/next-code")
CUS_P2=$(jq -r '.data.code // empty' "$RAW/31_cus_next2.json")
say "预览 #2 → code=$CUS_P2"
check "⑥ 预览只读不占号：连点两次同号" "$CUS_P1" "$CUS_P2"

say ""
say "=== ⑥ 客户：留空保存 ⇒ 自动取号 ==="
HTTP=$(post 32_cus_auto "/api/business/erp/customer" \
  '{"name":"CODE05 自动编码客户"}')
CUS_A1=$(jq -r '.data.code // empty' "$RAW/32_cus_auto.json")
CUS_A1_ID=$(jq -r '.data.id // empty' "$RAW/32_cus_auto.json")
say "POST 不带 code → HTTP $HTTP, code=$CUS_A1"
check "⑥ 自动号 == 预览号" "$CUS_P1" "$CUS_A1"

HTTP=$(post 33_cus_null "/api/business/erp/customer" \
  '{"code":null,"name":"CODE05 自动编码客户（null）"}')
CUS_A2=$(jq -r '.data.code // empty' "$RAW/33_cus_null.json")
CUS_A2_ID=$(jq -r '.data.id // empty' "$RAW/33_cus_null.json")
say "POST code=null → HTTP $HTTP, code=$CUS_A2"
check "⑥ null 自动取号（连续 +1）" "$(next_code CUS "$CUS_A1")" "$CUS_A2"

say ""
say "=== ⑥ 客户：手工编码 + 重复 + 编辑不改码 ==="
HTTP=$(post 34_cus_manual "/api/business/erp/customer" \
  '{"code":"KH-001","name":"CODE05 手工编码客户"}')
CUS_M=$(jq -r '.data.code // empty' "$RAW/34_cus_manual.json")
say "POST code=KH-001 → HTTP $HTTP, code=$CUS_M"
check "⑥ 手工编码原样保留" "KH-001" "$CUS_M"

HTTP=$(post 35_cus_dup "/api/business/erp/customer" \
  '{"code":"KH-001","name":"CODE05 重复编码客户"}')
CUS_DUP_CODE=$(jq -r '.code // empty' "$RAW/35_cus_dup.json")
CUS_DUP_MSG=$(jq -r '.msg // empty' "$RAW/35_cus_dup.json")
say "重复 KH-001 → HTTP $HTTP, 业务码=$CUS_DUP_CODE, msg=$CUS_DUP_MSG"
check "⑥ 客户重复编码 HTTP = 409" "409" "$HTTP"
check "⑥ 客户重复编码业务码 = 66001" "66001" "$CUS_DUP_CODE"
case "$CUS_DUP_MSG" in
  *KH-001*) say "  PASS  ⑥ 客户文案点名具体编码" ;;
  *) say "  FAIL  ⑥ 客户文案没点名编码：$CUS_DUP_MSG"; FAILED=1 ;;
esac

HTTP=$(get 36_cus_next3 "/api/business/erp/customer/next-code")
CUS_P3=$(jq -r '.data.code // empty' "$RAW/36_cus_next3.json")
say "建单后再预览 → code=$CUS_P3"
check_ne "⑥ 预览跳过已占用的号（≠ 刚用的自动号）" "$CUS_A2" "$CUS_P3"

HTTP=$(put 37_cus_update "/api/business/erp/customer/$CUS_A1_ID" \
  '{"name":"CODE05 自动编码客户（改名）"}')
CUS_A1_AFTER=$(jq -r '.data.code // empty' "$RAW/37_cus_update.json")
say "PUT 只改名称 → HTTP $HTTP, code=$CUS_A1_AFTER"
check "⑥ 编辑 HTTP = 200" "200" "$HTTP"
check "⑥ 编辑不改编码" "$CUS_A1" "$CUS_A1_AFTER"

# ================================================================
# 汇总
# ================================================================
say ""
if [ "$FAILED" = "0" ]; then
  say "=== 全部断言 PASS ==="
else
  say "=== 有断言 FAIL（见上）==="
fi
echo "SUP_P1=$SUP_P1 SUP_A1=$SUP_A1 SUP_A2=$SUP_A2 SUP_A3=$SUP_A3 SUP_M=$SUP_M SUP_P4=$SUP_P4" >> "$SUM"
echo "CUS_P1=$CUS_P1 CUS_A1=$CUS_A1 CUS_A2=$CUS_A2 CUS_M=$CUS_M" >> "$SUM"
exit "$FAILED"
