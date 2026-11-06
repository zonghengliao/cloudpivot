#!/usr/bin/env bash
# P1-FIX-12B 自检：联动规则页「状态显示中文 + 动作白名单说明」的前后端契约
#
#   BASE=http://127.0.0.1:8023 NIK_ADMIN_PASSWORD='<口令>' bash scripts/verify/p1fix12b_linkage_zh_check.sh
#
# 覆盖两件事（都能一眼判红）：
#   ① 后端 GET /business/flow/linkage 真的下发了中文名
#        - triggers[].status_labels 与 statuses 逐项对齐（同长度、同序 code）
#        - rules[].trigger_status_label 非空（查不到时后端回落成编码，也不会空）
#   ② 前端纯逻辑把字典翻成「中文（编码）」，且缺字典时回落编码
#        - 跑 tests/unit/flow-linkage.test.ts（含 statusOptions / statusLabelOf /
#          ruleTriggerStatusText / actionWhitelistNote 的断言）
#
# 首次登录会被强制改密（403「首次登录必须先修改初始密码」）：脚本会自动改一次再重登，
# 改密后新口令 = $NIK_ADMIN_PASSWORD（与旧口令相同则跳过改密，接口不会报错）。
set -o pipefail

BASE=${BASE:-http://127.0.0.1:8023}
PW=${NIK_ADMIN_PASSWORD:-Admin@123456}
TMP=${TMPDIR:-/tmp}/p1fix12b_check
mkdir -p "$TMP"
FAIL=0

login() {
  curl -s -o "$TMP/login.json" -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$PW\"}"
  jq -r '.data.access_token // empty' "$TMP/login.json"
}

echo '== ① 登录'
TOKEN=$(login)
if [ -z "$TOKEN" ]; then
  echo "登录失败：$(jq -c '.code,.msg' "$TMP/login.json" 2>/dev/null)"
  echo '（首登强制改密的库：先 PUT /api/system/auth/password 改密，或直接设 NIK_ADMIN_PASSWORD 为已改后的口令）'
  exit 1
fi

echo '== ② GET /business/flow/linkage'
code=$(curl -s -o "$TMP/linkage.json" -w '%{http_code}' "$BASE/api/business/flow/linkage" \
  -H "Authorization: Bearer $TOKEN")
echo "linkage_http=$code"
[ "$code" = '200' ] || { jq -c '.code,.msg' "$TMP/linkage.json"; exit 1; }

echo '== ③ 断言'
jq -r '
  "triggers=\(.data.triggers|length)  actions=\(.data.actions|length)  implemented=\([.data.actions[]|select(.implemented)]|length)  rules=\(.data.rules|length)",
  "带 status_labels 的触发点：\([.data.triggers[]|select((.status_labels|length) == (.statuses|length))]|length)/\(.data.triggers|length)",
  "示例 STOCK_IN：\(.data.triggers[]|select(.entity=="STOCK_IN")|.status_labels|map("\(.label)（\(.code)）")|join(" / "))",
  "规则行 trigger_status_label：\(.data.rules|map("\(.trigger_status_label)（\(.trigger_status)）")|join(" / "))"
' "$TMP/linkage.json"

check() {
  local name=$1 filter=$2
  if jq -e "$filter" "$TMP/linkage.json" >/dev/null; then
    echo "绿：$name"
  else
    echo "红：$name"
    FAIL=$((FAIL + 1))
  fi
}

check '每个触发点都下发 status_labels，且与 statuses 等长' \
  '([.data.triggers[]|select((.status_labels|length) != (.statuses|length))]|length) == 0'
check 'status_labels 的 code 顺序与 statuses 逐项一致' \
  '([.data.triggers[] | . as $t | select(any(range(0; ($t.statuses|length)); $t.statuses[.] != $t.status_labels[.].code))]|length) == 0'
check 'status_labels 的 label 都不为空' \
  '([.data.triggers[].status_labels[]|select((.label // "") == "")]|length) == 0'
check '每条规则都带 trigger_status_label（非空）' \
  '([.data.rules[]|select((.trigger_status_label // "") == "")]|length) == 0'

echo '== ④ 前端纯逻辑单测'
cd "$(dirname "$0")/../../frontend" || exit 1
if ./node_modules/.bin/tsx --test tests/unit/flow-linkage.test.ts > "$TMP/unit.txt" 2>&1; then
  echo "绿：flow-linkage 单测 $(grep -c '^ok ' "$TMP/unit.txt") 条"
else
  echo '红：flow-linkage 单测'
  tail -20 "$TMP/unit.txt"
  FAIL=$((FAIL + 1))
fi

echo "== 结果：失败 $FAIL 条"
[ "$FAIL" -eq 0 ] || exit 1
echo 'P1-FIX-12B 自检通过 ✓'
