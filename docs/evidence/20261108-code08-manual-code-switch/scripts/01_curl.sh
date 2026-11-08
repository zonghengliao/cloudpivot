#!/bin/bash
# CODE-08 验收（真实 token curl，隔离实例 127.0.0.1:8048，库 cloudpivot_code08d = 全新空库）
#
# 覆盖卡面验收三条，全部走真实 HTTP：
#   ① 参数为 true（默认）→ 6 个主数据「手工填编码」全部 200 且原样保留
#   ② 参数改 false      → 6 个主数据「手工填编码」全部 400 + 66054 + 「系统已禁止手工指定编码」；
#                          同时「留空」仍然 200 自动取号（开关只管手填，不改取号）
#   ③ 参数改回 true     → 手工编码恢复 200（不改代码、只改参数）
#   附：GET /api/business/erp/master-data/code-policy 三次读数（true / false / true）
#       未登录访问 → 401
#
# 参数通过 `POST /api/support/config` 的更新接口改（管理端同一条路），
# 走的是 `after_write()` → 运行时快照重载，所以**不重启**即可生效。
#
# 用法：bash 01_curl.sh          （实例已 start）
# 落证：../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8048}
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
check_contains() { # $1=描述 $2=子串 $3=文件
  if grep -qF "$2" "$3"; then say "  PASS  $1"; else say "  FAIL  $1（$3 里没有「$2」）"; FAILED=1; fi
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
PROBE=$(curl -s -o "$RAW/00_probe.json" -w '%{http_code}' -H "$AUTH" "$BASE/api/business/erp/master-data/code-policy")
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

# 物料要基本单位；费用类型要末级科目（种子库都有）
UNIT=$(curl -s -H "$AUTH" "$BASE/api/business/erp/unit/page?page=1&page_size=50" \
  | tee "$RAW/00_unit_list.json" | jq -r '(.data.list // [])[] | select(.unit_code=="PCS") | .id' | head -1)
say "基础单位 PCS id = ${UNIT:-<空>}"
ACCOUNT=$(curl -s -H "$AUTH" "$BASE/api/business/erp/account/page?page=1&page_size=200&is_leaf=true" \
  | tee "$RAW/00_account_list.json" | jq -r '(.data.list // [])[] | select(.account_code=="5602.01") | .id' | head -1)
if [ -z "$ACCOUNT" ]; then
  ACCOUNT=$(jq -r '(.data.list // [])[0].id // empty' "$RAW/00_account_list.json")
fi
say "末级科目 id = ${ACCOUNT:-<空>}"

# 参数行的 id（更新用；迁移 V0342 种入）
CFG_ID=$(curl -s -H "$AUTH" "$BASE/api/support/config/page?page=1&page_size=50&config_key=erp.master_data.allow_manual_code" \
  | tee "$RAW/00_config_page.json" | jq -r '(.data.list // [])[0].id // empty')
CFG_VALUE=$(jq -r '(.data.list // [])[0].config_value // empty' "$RAW/00_config_page.json")
CFG_CATEGORY=$(jq -r '(.data.list // [])[0].category // empty' "$RAW/00_config_page.json")
CFG_DESC=$(jq -r '(.data.list // [])[0].description // empty' "$RAW/00_config_page.json")
CFG_SORT=$(jq -r '(.data.list // [])[0].sort_order // 0' "$RAW/00_config_page.json")
CFG_SELECT=$(jq -r '(.data.list // [])[0].select_value // empty' "$RAW/00_config_page.json")
CFG_REF=$(jq -r '(.data.list // [])[0].ref_value // empty' "$RAW/00_config_page.json")
say "参数行 id = ${CFG_ID:-<空>}  初始值 = ${CFG_VALUE:-<空>}  分类 = ${CFG_CATEGORY:-<空>}"
if [ -z "$CFG_ID" ]; then say "FATAL: 找不到 erp.master_data.allow_manual_code（V0342 没应用？）"; exit 1; fi

# 改参数（管理端同一条接口，写后自动重载运行时快照，不需要重启）
#
# ⚠️ 两个坑（实测踩到）：
#   1. `ConfigSaveReq` 的 `config_key` / `config_type` 是**必填**（更新也走同一个 DTO）；
#   2. 更新是**整体替换**语义（`build_model(&req, …)` 后 `update`），少传 `category`
#      / `select_value` 就会把它们写成 NULL —— 配置页会因此把它挪到「未分类」、
#      开关控件退化成文本框。所以这里按前端的做法**整行提交**（全都是从库里读回来再发）。
set_config() { # $1 = true|false
  local http
  http=$(curl -s -o "$RAW/00_config_put_$1.json" -w '%{http_code}' \
    -X PUT "$BASE/api/support/config/$CFG_ID" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{\"config_key\":\"erp.master_data.allow_manual_code\",\"config_value\":\"$1\",\"config_type\":\"BOOLEAN\",\"description\":$(jq -Rn --arg v "$CFG_DESC" '$v'),\"category\":\"$CFG_CATEGORY\",\"sort_order\":$CFG_SORT,\"config_level\":2,\"is_modifiable\":true,\"is_encrypted\":false,\"is_restart\":false,\"status\":1,\"select_value\":$(jq -Rn --arg v "$CFG_SELECT" '$v'),\"ref_value\":$(jq -Rn --arg v "$CFG_REF" '$v')}")
  say "改参数 config_value=$1 → HTTP $http"
  if [ "$http" != "200" ]; then tee -a "$SUM" < "$RAW/00_config_put_$1.json"; exit 1; fi
  return 0
}

policy() { # $1 = 落证文件名
  curl -s -H "$AUTH" "$BASE/api/business/erp/master-data/code-policy" > "$RAW/$1"
  jq -r '.data.allow_manual_code' "$RAW/$1"
}

# 六个主数据的建单体（$1 = 编码参数片段，$2 = 落证前缀，$3 = 名称后缀）
post_supplier() {
  curl -s -o "$RAW/$2.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/supplier" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{$1\"supplier_name\":\"CODE08 供应商$3\",\"credit_days\":30,\"status\":1}"
}
post_customer() {
  curl -s -o "$RAW/$2.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/customer" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{$1\"name\":\"CODE08 客户$3\",\"status\":\"ACTIVE\"}"
}
post_material() {
  curl -s -o "$RAW/$2.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/material" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{$1\"material_name\":\"CODE08 物料$3\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"status\":1}"
}
post_warehouse() {
  curl -s -o "$RAW/$2.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/warehouse" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{$1\"warehouse_name\":\"CODE08 仓库$3\",\"warehouse_type\":\"NORMAL\",\"level\":1,\"status\":1}"
}
post_category() {
  curl -s -o "$RAW/$2.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/asset-category" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{$1\"category_name\":\"CODE08 分类$3\",\"useful_life_months\":60,\"salvage_rate\":0.05,\"status\":1}"
}
post_expense_type() {
  curl -s -o "$RAW/$2.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/expense-type" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{$1\"type_name\":\"CODE08 费用类型$3\",\"account_id\":\"$ACCOUNT\",\"status\":1}"
}

# 断言「这次建单被 400 + 66054 挡住」
assert_forbidden() { # $1=实体名 $2=落证前缀 $3=HTTP 状态码
  local f="$RAW/$2.json"
  check "$1 手工编码 HTTP（开关关闭）" "400" "$3"
  check "$1 错误码 66054" "66054" "$(jq -r '.code // empty' "$f")"
  check_contains "$1 文案「系统已禁止手工指定编码」" "系统已禁止手工指定编码" "$f"
  check_contains "$1 文案点出参数键" "erp.master_data.allow_manual_code" "$f"
}

echo >> "$SUM"
say "=== ① 参数为 true（迁移默认值）→ 六个主数据可手工填编码 ==="
check "迁移默认值" "true" "$CFG_VALUE"
check "code-policy 生效值（接口）" "true" "$(policy 10_policy_true.json)"

V=$(post_supplier '"supplier_code":"GYS-001",' 11_sup_manual_true "（手工）");        check "供应商手工码 HTTP" "200" "$V"
check "供应商手工码原样保留" "GYS-001" "$(jq -r '.data.supplier_code // empty' "$RAW/11_sup_manual_true.json")"
V=$(post_customer '"code":"KH-001",' 12_cus_manual_true "（手工）");                   check "客户手工码 HTTP" "200" "$V"
check "客户手工码原样保留" "KH-001" "$(jq -r '.data.code // empty' "$RAW/12_cus_manual_true.json")"
V=$(post_material '"material_code":"YB-001-02",' 13_mat_manual_true "（手工）");       check "物料手工码 HTTP" "200" "$V"
check "物料手工码原样保留" "YB-001-02" "$(jq -r '.data.material_code // empty' "$RAW/13_mat_manual_true.json")"
V=$(post_warehouse '"warehouse_code":"CK-001",' 14_wh_manual_true "（手工）");         check "仓库手工码 HTTP" "200" "$V"
check "仓库手工码原样保留" "CK-001" "$(jq -r '.data.warehouse_code // empty' "$RAW/14_wh_manual_true.json")"
V=$(post_category '"category_code":"CAT-BUILDING",' 15_ac_manual_true "（手工）");     check "资产分类手工码 HTTP" "200" "$V"
check "资产分类手工码原样保留" "CAT-BUILDING" "$(jq -r '.data.category_code // empty' "$RAW/15_ac_manual_true.json")"
V=$(post_expense_type '"type_code":"ADMIN-FEE",' 16_et_manual_true "（手工）");         check "费用类型手工码 HTTP" "200" "$V"
check "费用类型手工码原样保留" "ADMIN-FEE" "$(jq -r '.data.type_code // empty' "$RAW/16_et_manual_true.json")"

echo >> "$SUM"
say "=== ② 参数改为 false → 六个主数据手工编码全被 400 + 66054 挡住 ==="
set_config false
check "code-policy 生效值（接口）" "false" "$(policy 20_policy_false.json)"

H=$(post_supplier '"supplier_code":"GYS-OFF-001",' 21_sup_forbidden "（开关关闭）")
assert_forbidden "供应商" 21_sup_forbidden "$H"
H=$(post_customer '"code":"KH-OFF-001",' 22_cus_forbidden "（开关关闭）")
assert_forbidden "客户" 22_cus_forbidden "$H"
H=$(post_material '"material_code":"MAT-OFF-001",' 23_mat_forbidden "（开关关闭）")
assert_forbidden "物料" 23_mat_forbidden "$H"
H=$(post_warehouse '"warehouse_code":"WH-OFF-001",' 24_wh_forbidden "（开关关闭）")
assert_forbidden "仓库" 24_wh_forbidden "$H"
H=$(post_category '"category_code":"CAT-OFF-001",' 25_ac_forbidden "（开关关闭）")
assert_forbidden "资产分类" 25_ac_forbidden "$H"
H=$(post_expense_type '"type_code":"ET-OFF-001",' 26_et_forbidden "（开关关闭）")
assert_forbidden "费用类型" 26_et_forbidden "$H"

echo >> "$SUM"
say "=== ②b 同一开关下「留空」仍然自动取号（开关只管手填，不改取号口径） ==="
V=$(post_supplier '' 31_sup_auto_off "（自动）");        check "供应商留空 HTTP（开关关闭）" "200" "$V"
SUP_AUTO=$(jq -r '.data.supplier_code // empty' "$RAW/31_sup_auto_off.json")
check_like "供应商自动编码形态（4 位流水）" '^SUP-[0-9]{4,}$' "$SUP_AUTO"
V=$(post_customer '' 32_cus_auto_off "（自动）");        check "客户留空 HTTP（开关关闭）" "200" "$V"
check_like "客户自动编码形态" '^CUS-[0-9]{4,}$' "$(jq -r '.data.code // empty' "$RAW/32_cus_auto_off.json")"
V=$(post_material '' 33_mat_auto_off "（自动）");        check "物料留空 HTTP（开关关闭）" "200" "$V"
check_like "物料自动编码形态" '^MAT-[0-9]{4,}$' "$(jq -r '.data.material_code // empty' "$RAW/33_mat_auto_off.json")"
V=$(post_warehouse '' 34_wh_auto_off "（自动）");        check "仓库留空 HTTP（开关关闭）" "200" "$V"
check_like "仓库自动编码形态（2 位流水）" '^WH-[0-9]{2,}$' "$(jq -r '.data.warehouse_code // empty' "$RAW/34_wh_auto_off.json")"
V=$(post_category '' 35_ac_auto_off "（自动）");         check "资产分类留空 HTTP（开关关闭）" "200" "$V"
check_like "资产分类自动编码形态" '^AC-[0-9]{2,}$' "$(jq -r '.data.category_code // empty' "$RAW/35_ac_auto_off.json")"
V=$(post_expense_type '' 36_et_auto_off "（自动）");     check "费用类型留空 HTTP（开关关闭）" "200" "$V"
check_like "费用类型自动编码形态" '^ET-[0-9]{2,}$' "$(jq -r '.data.type_code // empty' "$RAW/36_et_auto_off.json")"

echo >> "$SUM"
say "=== ③ 参数改回 true → 手工编码恢复（只改参数，不改代码） ==="
set_config true
check "code-policy 生效值（接口）" "true" "$(policy 40_policy_true_again.json)"
V=$(post_supplier '"supplier_code":"GYS-BACK-001",' 41_sup_manual_back "（恢复）");   check "供应商手工码 HTTP（恢复后）" "200" "$V"
check "供应商手工码原样保留（恢复后）" "GYS-BACK-001" "$(jq -r '.data.supplier_code // empty' "$RAW/41_sup_manual_back.json")"
V=$(post_category '"category_code":"CAT-BACK-001",' 42_ac_manual_back "（恢复）");     check "资产分类手工码 HTTP（恢复后）" "200" "$V"

echo >> "$SUM"
say "=== ④ 权限：code-policy 需要登录（无 token → 401） ==="
HTTP=$(curl -s -o "$RAW/50_policy_anon.json" -w '%{http_code}' "$BASE/api/business/erp/master-data/code-policy")
check "未登录读 code-policy" "401" "$HTTP"

echo >> "$SUM"
say "=== ⑤ 参数行自检（改回 true 后库里的值） ==="
curl -s -H "$AUTH" "$BASE/api/support/config/page?page=1&page_size=50&config_key=erp.master_data.allow_manual_code" \
  > "$RAW/60_config_after.json"
check "参数最终值" "true" "$(jq -r '(.data.list // [])[0].config_value // empty' "$RAW/60_config_after.json")"
check "参数类型" "BOOLEAN" "$(jq -r '(.data.list // [])[0].config_type // empty' "$RAW/60_config_after.json")"
check "参数分类" "erp_master_data" "$(jq -r '(.data.list // [])[0].category // empty' "$RAW/60_config_after.json")"

echo >> "$SUM"
if [ "$FAILED" = "0" ]; then say "RESULT: ALL PASS"; else say "RESULT: FAILED（见上面 FAIL 行）"; fi
exit "$FAILED"
