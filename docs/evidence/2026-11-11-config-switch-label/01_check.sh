#!/usr/bin/env bash
# t_f111fef0 验收：TDesign 语义 ↔ 源码文案 ↔ 库值/接口值 三方核对
#   前置：bash 00_up.sh up（隔离库 costfix_fef0 / :18052）
#   用法：bash 01_check.sh
set -uo pipefail
DB="${FEF0_DB:-costfix_fef0}"
PORT="${FEF0_PORT:-18052}"
BASE="http://127.0.0.1:$PORT/api"
HERE="$(cd "$(dirname "$0")" && pwd)"
SCRATCH="$HERE/.tmp"
mkdir -p "$SCRATCH"
FAIL=0
ok() { echo "PASS  $1"; }
bad() { echo "FAIL  $1"; FAIL=$((FAIL+1)); }

# ---- 0. TDesign 语义铁证：label[0] 是「开启（checked）态」文案 ----
SRC=/opt/cloudpivot/frontend/node_modules/tdesign-vue-next/es/switch/switch.mjs
echo "== 0. TDesign Switch label 语义（$SRC）=="
grep -n "props2.label\[0\]" "$SRC" | sed -n '1,3p'
if grep -q "innerValue.value === activeValue.value ? props2.label\[0\] : props2.label\[1\]" "$SRC"; then
  ok "label[0] = checked 态文案（源码行见上）"
else
  bad "TDesign 源码未匹配到 label[0]=checked 的判定"
fi

# ---- 1. 源码：布尔控件文案顺序必须是 ['开启', '关闭'] ----
echo; echo "== 1. 前端源码 =="
LINE=$(grep -n "label=\"\['开启', '关闭'\]\"" /opt/cloudpivot/frontend/src/views/support/config/index.vue || true)
echo "support/config/index.vue: $LINE"
[ -n "$LINE" ] && ok "布尔控件 label = ['开启','关闭']" || bad "源码文案不是 ['开启','关闭']"

# ---- 2. 登录（隔离库自建 admin；首登已改密） ----
echo; echo "== 2. 取令牌 =="
curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@1234567"}' -o "$SCRATCH/login.json"
TOK=$(jq -r .data.access_token "$SCRATCH/login.json")
if [ -z "$TOK" ] || [ "$TOK" = "null" ]; then bad "登录失败"; cat "$SCRATCH/login.json"; exit 1; fi
ok "登录成功（token ${#TOK} 字节）"

# ---- 3. 库值 ↔ 接口值 ↔ 期望文案（走真实 PUT 接口改值，不直改库） ----
echo; echo "== 3. erp.cost.enabled：接口改值 → 库值 → 期望文案 =="
curl -s -H "Authorization: Bearer $TOK" "$BASE/support/config/page?page=1&page_size=200&config_key=erp.cost.enabled" \
  -o "$SCRATCH/page.json"
ID=$(jq -r '.data.list[0].id' "$SCRATCH/page.json")
[ "$ID" != "null" ] && [ -n "$ID" ] && ok "取到参数 id=$ID" || { bad "未取到 erp.cost.enabled"; exit 1; }

for want in true false; do
  jq -c --arg v "$want" '.data.list[0] | .config_value=$v | {config_key,config_value,config_type,description,category,sort_order,config_level,is_modifiable,is_encrypted,is_restart,status,ref_value,select_value}' \
    "$SCRATCH/page.json" > "$SCRATCH/put.json"
  resp=$(curl -s -X PUT -H "Authorization: Bearer $TOK" -H 'Content-Type: application/json' \
         --data @"$SCRATCH/put.json" "$BASE/support/config/$ID")
  [ "$(echo "$resp" | jq -r .code)" = "0" ] || { bad "PUT 失败：$resp"; continue; }
  dbv=$(su postgres -c "psql -d $DB -Atc \"SELECT config_value FROM sys_config WHERE config_key='erp.cost.enabled'\"")
  apiv=$(curl -s -H "Authorization: Bearer $TOK" "$BASE/support/config/page?page=1&page_size=200&config_key=erp.cost.enabled" | jq -r '.data.list[0].config_value')
  if [ "$dbv" = "$want" ] && [ "$apiv" = "$want" ]; then
    if [ "$want" = "true" ]; then exp="开启（label[0] / t-is-checked）"; else exp="关闭（label[1]）"; fi
    ok "库=$dbv 接口=$apiv ⇒ 开关选中=$want ⇒ 控件文案「$exp」"
  else
    bad "库=$dbv 接口=$apiv 与期望 $want 不一致"
  fi
done

echo; echo "== 结果：$([ $FAIL -eq 0 ] && echo 'ALL PASS' || echo "$FAIL FAILED") =="
exit $([ $FAIL -eq 0 ] && echo 0 || echo 1)
