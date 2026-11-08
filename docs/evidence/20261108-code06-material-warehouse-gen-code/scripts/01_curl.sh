#!/bin/bash
# CODE-06 验收（真实 token curl，隔离实例 127.0.0.1:8046，库 cloudpivot_code06a = 全新空库）
#
# 覆盖卡面验收（两个表单各 4 条用例，同 CODE-05 模式）：
#   ① 编码栏可留空（字段缺席 / null / 空串 都自动取号）
#   ② 预览接口给 MAT-0001 / WH-01（形态 + **只读不占号**：连点两次同号）
#   ③ 手工编码（YB-001-02 / CK-001）原样保留
#   ④ 保存后用掉预览号 → 编码回填同一号；撞号兜底 409 + 66001 且文案点名编码
#
# 用法：bash 01_curl.sh          （实例已 start）
# 落证：../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8046}
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
login() {
  curl -s -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" > "$RAW/00_login.json"
  jq -r '.data.access_token // empty' "$RAW/00_login.json"
}

TOKEN=$(login "$ADMIN_PASS")
if [ -z "$TOKEN" ]; then TOKEN=$(login "$ADMIN_PASS2"); fi
if [ -z "$TOKEN" ]; then
  say "FATAL: 登录失败"
  tee -a "$SUM" < "$RAW/00_login.json"
  exit 1
fi
say "login ok, access_token 长度 = ${#TOKEN}"

AUTH="Authorization: Bearer $TOKEN"

# 全新库首登有 `must_change_password` 闸（业务接口 403），只有改密 / 登出放行
PROBE=$(curl -s -o "$RAW/00_probe.json" -w '%{http_code}' -H "$AUTH" "$BASE/api/business/erp/material/next-code")
if [ "$PROBE" = "403" ] && grep -q "首次登录必须先修改初始密码" "$RAW/00_probe.json"; then
  HTTP=$(curl -s -o "$RAW/00_change_password.json" -w '%{http_code}' \
    -X PUT "$BASE/api/system/auth/password" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_PASS2\"}")
  say "首登改密（初始口令 → 新口令）→ HTTP $HTTP"
  if [ "$HTTP" != "200" ]; then tee -a "$SUM" < "$RAW/00_change_password.json"; exit 1; fi
  sleep 65   # 会话状态缓存 60s
  TOKEN=$(login "$ADMIN_PASS2")
  AUTH="Authorization: Bearer $TOKEN"
fi

# 物料必须有基本计量单位 —— 种子库已有 PCS（V0137），先查；查不到才建
UNIT=$(curl -s -H "$AUTH" "$BASE/api/business/erp/unit/page?page=1&page_size=50" \
  | tee "$RAW/00_unit_list.json" | jq -r '(.data.list // [])[] | select(.unit_code=="PCS") | .id' | head -1)
if [ -z "$UNIT" ]; then
  UNIT=$(curl -s -X POST "$BASE/api/business/erp/unit" -H "$AUTH" -H 'Content-Type: application/json' \
    -d '{"unit_code":"PCS","unit_name":"个","is_base":true,"status":1}' | tee "$RAW/00_unit.json" | jq -r '.data.id // empty')
fi
say "基础单位 PCS id = $UNIT"
if [ -z "$UNIT" ]; then tee -a "$SUM" < "$RAW/00_unit_list.json"; exit 1; fi

echo >> "$SUM"
say "=== ① 物料编码预览（只读不占号） ==="
curl -s -H "$AUTH" "$BASE/api/business/erp/material/next-code" > "$RAW/10_mat_next1.json"
curl -s -H "$AUTH" "$BASE/api/business/erp/material/next-code" > "$RAW/11_mat_next2.json"
MAT1=$(jq -r '.data.code // empty' "$RAW/10_mat_next1.json")
MAT2=$(jq -r '.data.code // empty' "$RAW/11_mat_next2.json")
check_like "物料预览形态（4 位纯流水）" '^MAT-[0-9]{4,}$' "$MAT1"
check "连点两次同一个号（预览不占号）" "$MAT1" "$MAT2"

echo >> "$SUM"
say "=== ② 物料：不填编码直接保存 → 用掉刚才预览的号 ==="
curl -s -X POST "$BASE/api/business/erp/material" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"material_name\":\"CODE06 物料（字段缺席）\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"status\":1}" \
  > "$RAW/12_mat_auto_absent.json"
MAT_AUTO=$(jq -r '.data.material_code // empty' "$RAW/12_mat_auto_absent.json")
check "字段缺席保存 → 编码 == 预览号" "$MAT1" "$MAT_AUTO"

curl -s -X POST "$BASE/api/business/erp/material" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"material_code\":null,\"material_name\":\"CODE06 物料（null）\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"status\":1}" \
  > "$RAW/13_mat_auto_null.json"
MAT_NULL=$(jq -r '.data.material_code // empty' "$RAW/13_mat_auto_null.json")
check_like "null → 自动取号" '^MAT-[0-9]{4,}$' "$MAT_NULL"
check_ne "null 取号与上一次不同" "$MAT_AUTO" "$MAT_NULL"

curl -s -H "$AUTH" "$BASE/api/business/erp/material/next-code" > "$RAW/14_mat_next3.json"
MAT3=$(jq -r '.data.next // .data.code // empty' "$RAW/14_mat_next3.json")
check_ne "号被保存用掉后预览往后走" "$MAT1" "$MAT3"

echo >> "$SUM"
say "=== ③ 物料：手工编码原样保留（按分类自编号） ==="
curl -s -X POST "$BASE/api/business/erp/material" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"material_code\":\"YB-001-02\",\"material_name\":\"CODE06 物料（手工码）\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"status\":1}" \
  > "$RAW/15_mat_manual.json"
check "手工编码原样保留" "YB-001-02" "$(jq -r '.data.material_code // empty' "$RAW/15_mat_manual.json")"

echo >> "$SUM"
say "=== ④ 物料：预览号当手工编码提交（原样用） + 撞号 409+66001 ==="
MAT_NEXT=$(curl -s -H "$AUTH" "$BASE/api/business/erp/material/next-code" | jq -r '.data.code // empty')
echo "{\"previewed\":\"$MAT_NEXT\"}" > "$RAW/16_mat_preview_for_use.json"
HTTP=$(curl -s -o "$RAW/17_mat_use_previewed.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/material" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"material_code\":\"$MAT_NEXT\",\"material_name\":\"CODE06 物料（用预览号）\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"status\":1}")
check "预览号建单 HTTP" "200" "$HTTP"
check "预览号原样保留" "$MAT_NEXT" "$(jq -r '.data.material_code // empty' "$RAW/17_mat_use_previewed.json")"

HTTP=$(curl -s -o "$RAW/18_mat_dup.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/material" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"material_code\":\"$MAT_NEXT\",\"material_name\":\"CODE06 物料（撞号）\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"status\":1}")
check "撞号 HTTP" "409" "$HTTP"
check "撞号错误码 66001" "66001" "$(jq -r '.code // empty' "$RAW/18_mat_dup.json")"
if grep -q "$MAT_NEXT" "$RAW/18_mat_dup.json"; then say "  PASS  撞号文案点名编码 $MAT_NEXT"; else say "  FAIL  撞号文案没点名编码"; FAILED=1; fi

echo >> "$SUM"
say "=== ⑤ 仓库编码预览（只读不占号，2 位流水） ==="
curl -s -H "$AUTH" "$BASE/api/business/erp/warehouse/next-code" > "$RAW/30_wh_next1.json"
curl -s -H "$AUTH" "$BASE/api/business/erp/warehouse/next-code" > "$RAW/31_wh_next2.json"
WH1=$(jq -r '.data.code // empty' "$RAW/30_wh_next1.json")
WH2=$(jq -r '.data.code // empty' "$RAW/31_wh_next2.json")
check_like "仓库预览形态（2 位纯流水）" '^WH-[0-9]{2,}$' "$WH1"
check "连点两次同一个号（预览不占号）" "$WH1" "$WH2"

echo >> "$SUM"
say "=== ⑥ 仓库：字段缺席 / 空串 保存 → 自动取号 ==="
curl -s -X POST "$BASE/api/business/erp/warehouse" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"warehouse_name":"CODE06 仓库（字段缺席）","warehouse_type":"NORMAL","level":1,"status":1}' \
  > "$RAW/32_wh_auto_absent.json"
WH_AUTO=$(jq -r '.data.warehouse_code // empty' "$RAW/32_wh_auto_absent.json")
check "字段缺席保存 → 编码 == 预览号" "$WH1" "$WH_AUTO"

curl -s -X POST "$BASE/api/business/erp/warehouse" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"warehouse_code":"","warehouse_name":"CODE06 仓库（空串）","warehouse_type":"NORMAL","level":1,"status":1}' \
  > "$RAW/33_wh_auto_empty.json"
WH_EMPTY=$(jq -r '.data.warehouse_code // empty' "$RAW/33_wh_auto_empty.json")
check_like "空串 → 自动取号" '^WH-[0-9]{2,}$' "$WH_EMPTY"
check_ne "空串取号与上一次不同" "$WH_AUTO" "$WH_EMPTY"

echo >> "$SUM"
say "=== ⑦ 仓库：手工编码原样保留 + 预览号原样用 + 撞号 409+66001 ==="
curl -s -X POST "$BASE/api/business/erp/warehouse" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"warehouse_code":"CK-001","warehouse_name":"CODE06 仓库（手工码）","warehouse_type":"NORMAL","level":1,"status":1}' \
  > "$RAW/34_wh_manual.json"
check "手工编码原样保留" "CK-001" "$(jq -r '.data.warehouse_code // empty' "$RAW/34_wh_manual.json")"

WH_NEXT=$(curl -s -H "$AUTH" "$BASE/api/business/erp/warehouse/next-code" | jq -r '.data.code // empty')
check_ne "已占号后预览往后走" "$WH1" "$WH_NEXT"
HTTP=$(curl -s -o "$RAW/35_wh_use_previewed.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/warehouse" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"warehouse_code\":\"$WH_NEXT\",\"warehouse_name\":\"CODE06 仓库（用预览号）\",\"warehouse_type\":\"NORMAL\",\"level\":1,\"status\":1}")
check "预览号建仓 HTTP" "200" "$HTTP"
check "预览号原样保留" "$WH_NEXT" "$(jq -r '.data.warehouse_code // empty' "$RAW/35_wh_use_previewed.json")"

HTTP=$(curl -s -o "$RAW/36_wh_dup.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/warehouse" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"warehouse_code\":\"$WH_NEXT\",\"warehouse_name\":\"CODE06 仓库（撞号）\",\"warehouse_type\":\"NORMAL\",\"level\":1,\"status\":1}")
check "撞号 HTTP" "409" "$HTTP"
check "撞号错误码 66001" "66001" "$(jq -r '.code // empty' "$RAW/36_wh_dup.json")"
if grep -q "$WH_NEXT" "$RAW/36_wh_dup.json"; then say "  PASS  撞号文案点名编码 $WH_NEXT"; else say "  FAIL  撞号文案没点名编码"; FAILED=1; fi

echo >> "$SUM"
say "=== ⑧ 权限：不带 token 访问两个预览接口 → 401 ==="
check "物料预览无 token" "401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/material/next-code")"
check "仓库预览无 token" "401" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/business/erp/warehouse/next-code")"

echo >> "$SUM"
if [ "$FAILED" = "0" ]; then say "RESULT: ALL PASS"; else say "RESULT: FAILED"; fi
exit "$FAILED"
