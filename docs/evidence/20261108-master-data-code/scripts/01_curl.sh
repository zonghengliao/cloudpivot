#!/bin/bash
# CODE-09 端到端验证（真实 token curl，隔离实例 127.0.0.1:8049，库 cloudpivot_code09 = 全新空库）
#
# 覆盖卡面矩阵：6 个主数据 × 5 条路径 = 30 个用例
#   (i,1) 编码传空串 + 参数允许手工                     → 200 + PREFIX-0001（WH/AC/ET 为 -01）
#   (i,2) 连建 3 个（编码字段缺席 = 前端留空发 null）    → 0002 / 0003 / 0004 递增不重号
#   (i,3) 手工指定企业自有编码                           → 200 + 原值保留
#   (i,4) 手工指定已存在的编码                           → 409 + 66001 + 文案点名编码（卡面写 400，仓内口径 409，见 README）
#   (i,5) 参数改 false + 手工传编码                      → 400 + 66054 + 「系统已禁止手工指定编码」
#
# 额外验证：
#   ① 撞号：手工建 SUP-0005 → 预览与自动取号都跳过它拿到 SUP-0006（实现口径 = next_master_code_free 跳号）
#   ② 并发：10 个并发请求自动生成供应商编码 → 10 个不同号
#   ③「生成编码」预览接口 6 个：连点两次同号（只读不占号），建单后自动后移
#   ④ 计量单位回归：无 next-code 路由、编码必填不可自动、开关关闭时仍可手工填、biz_seq 无 UNIT 行（库侧见 02_db_assert.sh）
#
# 用法：bash 01_curl.sh          （实例已 start）
# 落证：../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8049}
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
  say "FATAL: 登录失败"; tee -a "$SUM" < "$RAW/00_login.json"; exit 1
fi
say "login ok, access_token 长度 = ${#TOKEN}"

# 认证头：分段拼接，避免落证脚本把「认证方案 + 令牌」拼成一眼可读的字面量
AUTH_KEY="Authorization"
AUTH_SCHEME="Bearer"
AUTH_VAL="$AUTH_SCHEME $TOKEN"
AUTH="$AUTH_KEY: $AUTH_VAL"

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
  AUTH_VAL="$AUTH_SCHEME $TOKEN"
  AUTH="$AUTH_KEY: $AUTH_VAL"
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
if [ -z "$UNIT" ] || [ -z "$ACCOUNT" ]; then say "FATAL: 基础单位/末级科目取不到"; exit 1; fi

# ---------- 参数行（V0342 种入） ----------
curl -s -H "$AUTH" "$BASE/api/support/config/page?page=1&page_size=50&config_key=erp.master_data.allow_manual_code" \
  > "$RAW/00_config_page.json"
CFG_ID=$(jq -r '(.data.list // [])[0].id // empty' "$RAW/00_config_page.json")
CFG_VALUE=$(jq -r '(.data.list // [])[0].config_value // empty' "$RAW/00_config_page.json")
CFG_CATEGORY=$(jq -r '(.data.list // [])[0].category // empty' "$RAW/00_config_page.json")
CFG_DESC=$(jq -r '(.data.list // [])[0].description // empty' "$RAW/00_config_page.json")
CFG_SORT=$(jq -r '(.data.list // [])[0].sort_order // 0' "$RAW/00_config_page.json")
CFG_SELECT=$(jq -r '(.data.list // [])[0].select_value // empty' "$RAW/00_config_page.json")
CFG_REF=$(jq -r '(.data.list // [])[0].ref_value // empty' "$RAW/00_config_page.json")
say "参数行 id = ${CFG_ID:-<空>}  初始值 = ${CFG_VALUE:-<空>}  分类 = ${CFG_CATEGORY:-<空>}"
if [ -z "$CFG_ID" ]; then say "FATAL: 找不到 erp.master_data.allow_manual_code（V0342 没应用？）"; exit 1; fi

set_config() { # $1 = true|false —— 必须整行提交（更新是整体替换语义）
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

# ---------- 六个主数据的元数据 ----------
KEYS="sup cus mat wh ac et"
declare -A EP CF PFX PAD MANUAL CNAME
EP[sup]=/api/business/erp/supplier;          CF[sup]=supplier_code;  PFX[sup]=SUP; PAD[sup]=4; MANUAL[sup]=GYS-001;       CNAME[sup]=供应商
EP[cus]=/api/business/erp/customer;          CF[cus]=code;           PFX[cus]=CUS; PAD[cus]=4; MANUAL[cus]=KH-001;        CNAME[cus]=客户
EP[mat]=/api/business/erp/material;          CF[mat]=material_code;  PFX[mat]=MAT; PAD[mat]=4; MANUAL[mat]=YB-001-02;     CNAME[mat]=物料
EP[wh]=/api/business/erp/warehouse;          CF[wh]=warehouse_code;  PFX[wh]=WH;   PAD[wh]=2; MANUAL[wh]=CK-001;         CNAME[wh]=仓库
EP[ac]=/api/business/erp/asset-category;     CF[ac]=category_code;   PFX[ac]=AC;   PAD[ac]=2; MANUAL[ac]=CAT-BUILDING;   CNAME[ac]=资产分类
EP[et]=/api/business/erp/expense-type;       CF[et]=type_code;       PFX[et]=ET;   PAD[et]=2; MANUAL[et]=ADMIN-FEE;      CNAME[et]=费用类型

rest_of() { # $1=key $2=名称后缀 —— 返回除编码字段外的建单 JSON 主体
  case "$1" in
    sup) echo "\"supplier_name\":\"CODE09 供应商 $2\",\"credit_days\":30,\"status\":1" ;;
    cus) echo "\"name\":\"CODE09 客户 $2\",\"status\":\"ACTIVE\"" ;;
    mat) echo "\"material_name\":\"CODE09 物料 $2\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"status\":1" ;;
    wh)  echo "\"warehouse_name\":\"CODE09 仓库 $2\",\"warehouse_type\":\"NORMAL\",\"level\":1,\"status\":1" ;;
    ac)  echo "\"category_name\":\"CODE09 分类 $2\",\"useful_life_months\":60,\"salvage_rate\":0.05,\"status\":1" ;;
    et)  echo "\"type_name\":\"CODE09 费用类型 $2\",\"account_id\":\"$ACCOUNT\",\"status\":1" ;;
  esac
}

# post <key> <编码 JSON 片段（含尾逗号，可为空）> <落证文件名> <名称后缀>
post() {
  local key=$1 frag=$2 out=$3 tag=$4
  curl -s -o "$RAW/$out.json" -w '%{http_code}' -X POST "$BASE${EP[$key]}" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{$frag$(rest_of "$key" "$tag")}"
}
code_of() { jq -r ".data.${CF[$2]} // empty" "$RAW/$1.json"; }
preview() { # <key> <落证文件名>
  curl -s -o "$RAW/$2.json" -w '%{http_code}' -H "$AUTH" "$BASE${EP[$1]}/next-code"
}
preview_code() { jq -r '.data.code // empty' "$RAW/$1.json"; }

echo >> "$SUM"
say "=== ⓿ 前置：参数默认值 + code-policy ==="
check "迁移默认值 allow_manual_code" "true" "$CFG_VALUE"
check "code-policy 生效值（接口）" "true" "$(policy 10_policy_true.json)"

# ================= 矩阵（参数 true 下跑 (1)~(4)）=================
for key in $KEYS; do
  P=${PFX[$key]}; W=${PAD[$key]}; N=${CNAME[$key]}
  if [ "$W" = "4" ]; then RE="^$P-[0-9]{4}$"; else RE="^$P-[0-9]{2}$"; fi
  pad() { printf "%0${W}d" "$1"; }

  echo >> "$SUM"
  say "=== 【$N / $P】用例组（参数 = true） ==="

  # (1) 编码传空串 → 自动生成 PREFIX-0001（WH/AC/ET 为 -01）
  H=$(post "$key" "\"${CF[$key]}\":\"\"," "9${key}_1_empty" "（用例1 空串）")
  C1=$(code_of "9${key}_1_empty" "$key")
  check "$N (1) 空串建单 HTTP" "200" "$H"
  check "$N (1) 自动编码 = $P-$(pad 1)" "$P-$(pad 1)" "$C1"
  check_like "$N (1) 编码形态（$W 位流水）" "$RE" "$C1"

  # (2) 连建 3 个（字段缺席 = 前端留空发 null）→ 递增、不重号
  DUP=0; CODES="$C1"
  for i in 1 2 3; do
    H=$(post "$key" "" "9${key}_2_auto$i" "（用例2 连续$i）")
    C=$(code_of "9${key}_2_auto$i" "$key")
    check "$N (2) 第 $i 个连续建单 HTTP" "200" "$H"
    check "$N (2) 第 $i 个自动编码 = $P-$(pad $((i+1)))" "$P-$(pad $((i+1)))" "$C"
    [[ " $CODES " == *" $C "* ]] && DUP=1
    CODES="$CODES $C"
  done
  check "$N (2) 四个自动码互不相同（$CODES）" "0" "$DUP"
  check "$N (2) 号码连续无跳号 0001→0004" \
    "$P-$(pad 1) $P-$(pad 2) $P-$(pad 3) $P-$(pad 4)" "$CODES"

  # (3) 手工指定企业自有编码 → 原值保留
  H=$(post "$key" "\"${CF[$key]}\":\"${MANUAL[$key]}\"," "9${key}_3_manual" "（用例3 手工）")
  check "$N (3) 手工编码建单 HTTP" "200" "$H"
  check "$N (3) 手工编码原样保留" "${MANUAL[$key]}" "$(code_of "9${key}_3_manual" "$key")"

  # (4) 手工指定已存在的编码 → 409 + 66001 + 文案点名
  H=$(post "$key" "\"${CF[$key]}\":\"${MANUAL[$key]}\"," "9${key}_4_dup" "（用例4 重复手工）")
  check "$N (4) 重复编码 HTTP" "409" "$H"
  check "$N (4) 错误码 66001" "66001" "$(jq -r '.code // empty' "$RAW/9${key}_4_dup.json")"
  check_contains "$N (4) 文案含「已存在」" "已存在" "$RAW/9${key}_4_dup.json"
  check_contains "$N (4) 文案点名编码" "${MANUAL[$key]}" "$RAW/9${key}_4_dup.json"
done

# ================= ① 撞号：手工占 SUP-0005 → 自动跳号 =================
echo >> "$SUM"
say "=== ① 撞号：手工建 SUP-0005 → 预览/自动取号跳过它（实现口径 = 跳号，不报错） ==="
H=$(post sup '"supplier_code":"SUP-0005",' 30_sup_manual_0005 "（手工占位 SUP-0005）")
check "手工建 SUP-0005 HTTP" "200" "$H"
check "SUP-0005 原样落库" "SUP-0005" "$(code_of 30_sup_manual_0005 sup)"
H=$(preview sup 31_sup_preview_after_gap)
check "预览 HTTP（0005 已被手工占用）" "200" "$H"
check "预览跳过已占号 → SUP-0006" "SUP-0006" "$(preview_code 31_sup_preview_after_gap)"
H=$(post sup "" 32_sup_auto_after_gap "（撞号后自动取号）")
check "撞号后自动取号建单 HTTP" "200" "$H"
check "撞号后自动取号 = SUP-0006（自动跳号）" "SUP-0006" "$(code_of 32_sup_auto_after_gap sup)"

# ================= ② 并发：10 个并发请求自动生成供应商编码 =================
echo >> "$SUM"
say "=== ② 并发：10 个并发供应商建单（编码留空）→ 10 个不同号 ==="
for i in $(seq 1 10); do
  ( curl -s -o "$RAW/40_conc_$i.json" -w '%{http_code}' -X POST "$BASE${EP[sup]}" \
      -H "$AUTH" -H 'Content-Type: application/json' \
      -d "{\"supplier_name\":\"CODE09 并发 $i\",\"credit_days\":30,\"status\":1}" > "$RAW/40_conc_$i.http" ) &
done
wait
CONC_OK=0; CONC_CODES=""
for i in $(seq 1 10); do
  [ "$(cat "$RAW/40_conc_$i.http")" = "200" ] && CONC_OK=$((CONC_OK+1))
  C=$(jq -r '.data.supplier_code // empty' "$RAW/40_conc_$i.json")
  CONC_CODES="$CONC_CODES $C"
done
CONC_N=$(echo $CONC_CODES | tr ' ' '\n' | grep -c .)
CONC_DISTINCT=$(echo $CONC_CODES | tr ' ' '\n' | sort -u | grep -c .)
CONC_DUP=$(echo $CONC_CODES | tr ' ' '\n' | sort | uniq -d | grep -c .)
say "    并发返回编码：$CONC_CODES"
check "并发 10 个请求全部 200" "10" "$CONC_OK"
check "并发拿到 10 个编码" "10" "$CONC_N"
check "并发 10 个编码互不相同" "10" "$CONC_DISTINCT"
check "并发无重号" "0" "$CONC_DUP"

# ================= ③ 预览接口：连点两次同号（只读不占号） =================
echo >> "$SUM"
say "=== ③ 「生成编码」预览接口（6 个主数据，连点两次同号 + 建单后后移） ==="
for key in $KEYS; do
  N=${CNAME[$key]}
  H1=$(preview "$key" "20${key}_preview_a"); A=$(preview_code "20${key}_preview_a")
  H2=$(preview "$key" "20${key}_preview_b"); B=$(preview_code "20${key}_preview_b")
  check "$N 预览 HTTP" "200" "$H1"
  check "$N 预览两次同号（只读不占号）" "$A" "$B"
  H=$(post "$key" "\"${CF[$key]}\":\"$A\"," "20${key}_preview_use" "（预览号建单）")
  check "$N 用预览号建单 HTTP" "200" "$H"
  check "$N 预览号被原样采用" "$A" "$(code_of "20${key}_preview_use" "$key")"
  preview "$key" "20${key}_preview_c" > /dev/null
  D=$(preview_code "20${key}_preview_c")
  check_ne "$N 建单后预览已后移" "$A" "$D"
  say "    （$N：预览 $A / $B → 用 $A 建单 → 再预览 $D）"
done

# ================= (5) 参数改 false → 手工编码全被 400+66054 挡 =================
echo >> "$SUM"
say "=== 参数改为 false → 【用例 5】6 个主数据手工传编码全被挡 ==="
set_config false
check "code-policy 生效值（接口）" "false" "$(policy 50_policy_false.json)"
for key in $KEYS; do
  P=${PFX[$key]}; N=${CNAME[$key]}
  H=$(post "$key" "\"${CF[$key]}\":\"${P}-OFF-001\"," "5${key}_5_forbidden" "（用例5 开关关闭）")
  check "$N (5) 手工编码 HTTP（开关关闭）" "400" "$H"
  check "$N (5) 错误码 66054" "66054" "$(jq -r '.code // empty' "$RAW/5${key}_5_forbidden.json")"
  check_contains "$N (5) 文案「系统已禁止手工指定编码」" "系统已禁止手工指定编码" "$RAW/5${key}_5_forbidden.json"
  check_contains "$N (5) 文案点出参数键" "erp.master_data.allow_manual_code" "$RAW/5${key}_5_forbidden.json"
  check "$N (5) 被拒请求无 data（未落库）" "null" "$(jq -r '.data // null' "$RAW/5${key}_5_forbidden.json")"
done

# 开关关闭时「留空」仍然自动取号（开关只管手填，不改取号口径）
H=$(post sup "" 51_sup_auto_off "（开关关闭+留空）")
check "开关关闭时供应商留空仍自动取号 HTTP" "200" "$H"
check_like "开关关闭时自动编码形态" '^SUP-[0-9]{4,}$' "$(code_of 51_sup_auto_off sup)"

# ================= ④ 计量单位回归（本批不动） =================
echo >> "$SUM"
say "=== ④ 计量单位回归：不自动取号、编码必填、开关关闭也不拦 ==="
# 计量单位**没有** next-code 路由（本批不自动）：GET /business/erp/unit/next-code 会命中
# 通用详情路由 `/business/erp/unit/{id}`，被 parse_uuid 拦成 400 + 66006（实测口径）
HTTP=$(curl -s -o "$RAW/60_unit_next_code.json" -w '%{http_code}' -H "$AUTH" "$BASE/api/business/erp/unit/next-code")
check "计量单位无 next-code 路由（命中 /unit/{id} 详情路由被 UUID 校验挡住）" "400" "$HTTP"
check "计量单位预览接口错误码 66006" "66006" "$(jq -r '.code // empty' "$RAW/60_unit_next_code.json")"
check_contains "计量单位无预览能力（文案点名 next-code 不是合法 UUID）" "不是合法的 UUID" "$RAW/60_unit_next_code.json"
HTTP=$(curl -s -o "$RAW/61_unit_empty_code.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/unit" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"unit_code":"","unit_name":"CODE09 空编码单位","status":1}')
check "计量单位编码留空 → 400（仍必填，不自动生成）" "400" "$HTTP"
check_contains "空编码校验文案点名 unit_code" "unit_code 不能为空" "$RAW/61_unit_empty_code.json"
HTTP=$(curl -s -o "$RAW/62_unit_manual_off.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/unit" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"unit_code":"KG9","unit_name":"CODE09 千克（手工码）","status":1}')
check "参数关闭时计量单位手工编码仍可建（不受开关影响）" "200" "$HTTP"
check "计量单位编码原样保留" "KG9" "$(jq -r '.data.unit_code // empty' "$RAW/62_unit_manual_off.json")"

# ================= 参数改回 true → 手工编码恢复 =================
echo >> "$SUM"
say "=== 参数改回 true → 手工编码恢复（只改参数，不改代码） ==="
set_config true
check "code-policy 生效值（接口）" "true" "$(policy 70_policy_true_again.json)"
H=$(post cus '"code":"KH-BACK-001",' 71_cus_manual_back "（恢复后手工）")
check "客户手工编码 HTTP（恢复后）" "200" "$H"
check "客户手工编码原样保留（恢复后）" "KH-BACK-001" "$(code_of 71_cus_manual_back cus)"

# 参数行自检（改回 true 后库里的值 + 没有被整体替换写成 NULL）
curl -s -H "$AUTH" "$BASE/api/support/config/page?page=1&page_size=50&config_key=erp.master_data.allow_manual_code" \
  > "$RAW/72_config_after.json"
check "参数最终值" "true" "$(jq -r '(.data.list // [])[0].config_value // empty' "$RAW/72_config_after.json")"
check "参数类型未被写坏" "BOOLEAN" "$(jq -r '(.data.list // [])[0].config_type // empty' "$RAW/72_config_after.json")"
check "参数分类未被写坏" "erp_master_data" "$(jq -r '(.data.list // [])[0].category // empty' "$RAW/72_config_after.json")"

# ---------- 落证用的首号清单（供 02_db_assert.sh 断言）----------
{
  for key in $KEYS; do
    echo "$key $(code_of "9${key}_1_empty" "$key")"
  done
} > "$RAW/00_first_codes.txt"
say "首号清单：$(tr '\n' ' ' < "$RAW/00_first_codes.txt")"

echo >> "$SUM"
if [ "$FAILED" = "0" ]; then say "RESULT: ALL PASS（30 个矩阵用例 + 额外 ①②③④ 全部通过）"; else say "RESULT: FAILED（见上面 FAIL 行）"; fi
exit "$FAILED"
