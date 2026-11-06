#!/usr/bin/env bash
# =============================================================================
# SO-29 验收：销售退货 CRUD + 状态机 + 超退守卫（真后端 + 真 token curl）
# =============================================================================
#   bash scripts/verify/so29_db_setup.sh                       # 空库
#   bash scripts/verify/so29_instance.sh up                    # 实例 :18229（全量迁移）
#   bash scripts/verify/so29_sales_return_fixture.sql           # 夹具（psql -f）
#   bash scripts/verify/so29_sales_return_e2e.sh               # 本脚本
#
# 验什么（卡面验收）：
#   ① 迁移：V0303（两张表）+ V0304（3 个权限码）都落 schema_migrations，
#      kind=versioned / applied_via=executed；权限码在 sys_menu 有承载行
#   ② curl 实测建单：items 留空 ⇒ 从原发货单带出可退明细（数量 = 可退量）
#   ③ curl 实测提交：DRAFT → SUBMITTED → APPROVED → PENDING_POST 全链走通，
#      `biz_status_log` 有完整轨迹；POSTED 在本卡显式 409（SO-30 落地）
#   ④ 超退被拒：qty > 已发货量 − 已退量 ⇒ 400；作废前一张后同一数量又放行
#   ⑤ 非法边 409 / 未过账发货单 409 / 非法 return_type 400 / 缺 items 400
#
# 证据：docs/evidence/20261107-so29-sales-return/raw/*（响应体逐份落盘，JWT 已脱敏）
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
PORT=${SO29_PORT:-18229}
BASE="http://127.0.0.1:$PORT/api"
DB=${SO29_DB:-cloudpivot_so29}
EVID="$ROOT/docs/evidence/20261107-so29-sales-return"
RAW="$EVID/raw"
mkdir -p "$RAW"
export PGPASSWORD=alun
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }

# 夹具 UUID（与 so29_sales_return_fixture.sql 逐字一致）
CUST='d0290000-0000-4000-8000-000000000001'
SO='d0291000-0000-4000-8000-000000000001'
SO_ITEM1='d0291100-0000-4000-8000-000000000001'
SO_ITEM2='d0291100-0000-4000-8000-000000000002'
SO_ITEM3='d0291100-0000-4000-8000-000000000003'
SH1='d0292000-0000-4000-8000-000000000001'
SH2='d0292000-0000-4000-8000-000000000002'
SH3='d0292000-0000-4000-8000-000000000003'
WH='d1370001-0000-4000-8000-000000000002'
MAT='d1370003-0000-4000-8000-000000000001'

PASS=0; FAIL=0; FAILED=()
ok()  { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$*"); printf '  ❌ %s\n' "$*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
# 数值比较：jq 会**保留数字字面量格式**（`10.0000` 原样输出，tonumber/tostring 也不改），
# 所以不按字符串比，一律 ×10000 取整后比（Decimal 最多 4 位小数，够用且无浮点误差）。
check_n() { # check_n <标签> <jq 表达式> <期望值（十进制字面量）>
  local label=$1 expr=$2 want=$3 got expect
  got=$(printf '%s' "$BODY" | jq -r "($expr|tonumber)*10000|round" 2>/dev/null)
  expect=$(python3 -c "print(round(float('$want')*10000))" 2>/dev/null)
  check "$label" "$got" "$expect"
}

# api <名> <METHOD> <path> [token] [json]
api() {
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  # 用 --oauth2-bearer 而不是手写 Authorization 头：脱敏会把「Bearer + 变量」改写掉
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null || true)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  if printf '%s' "$BODY" | jq -e '.data.access_token? // .data.refresh_token?' >/dev/null 2>&1; then
    jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
        | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
       "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  fi
}
d() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

# try_login <user> <pass> [tag] → 「token<TAB>must_change」（失败回空）
# ⚠ 返回值必须带 must_change：函数体在 `$( )` 子 shell 里跑，函数内赋值传不回父 shell
#   （实测踩过：MUST_CHANGE 永远是父层旧值 ⇒ 首登改密分支不执行 ⇒ 全部 403）
try_login() {
  local code tok out="$RAW/login_${3:-try}.json" must
  code=$(curl -s -m 20 -o "$out" -w '%{http_code}' -X POST "$BASE/system/auth/login" \
           -H 'Content-Type: application/json' \
           -d "{\"username\":\"$1\",\"password\":\"$2\"}")
  [ "$code" = "200" ] || { printf ''; return; }
  tok=$(jq -r '.data.access_token // empty' "$out")
  must=$(jq -r '.data.must_change_password // false' "$out")
  jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
      | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
     "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  printf '%s\t%s' "$tok" "$must"
}

login_split() { # → 设 T_ADMIN / MUST_CHANGE（父 shell 里赋值）
  local line
  line=$(try_login "$1" "$2" "${3:-try}")
  T_ADMIN=${line%%$'\t'*}
  MUST_CHANGE=${line##*$'\t'}
  [ -z "$T_ADMIN" ] && MUST_CHANGE=false
}

echo "=================================================================="
echo "SO-29 验收：库=$DB 实例=$BASE"
echo "=================================================================="

echo; echo "== 0) 实例 / 迁移记账（V0303 建表 + V0304 权限码） =="
health=$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$PORT/health")
check "/health" "$health" "200"

for pair in "sales_return|303" "sales_return_permissions|304"; do
  name=${pair%%|*}; ver=${pair##*|}
  row=$(PSQLQ -c "select version||'|'||name||'|'||kind||'|'||applied_via from schema_migrations where name='$name'")
  echo "  · $name 记账：$row"
  case "$row" in
    *"|$name|versioned|executed") ok "V$ver kind=versioned applied_via=executed" ;;
    *) bad "$name 记账不对：$row" ;;
  esac
done
badvia=$(PSQLQ -c "select count(*) from schema_migrations where applied_via not in ('executed','imported')")
check "applied_via 非 executed/imported 的迁移数" "$badvia" "0"
migcnt=$(PSQLQ -c "select count(*) from schema_migrations")
echo "  · 迁移总条数=$migcnt（空库全量重放）"

t303=$(PSQLQ -c "select count(*) from information_schema.tables where table_name in ('biz_sales_return','biz_sales_return_item')")
check "两张退货表存在" "$t303" "2"
codes=$(PSQLQ -c "select count(*) from sys_menu where deleted_at is null and auth_code like 'business:erp:sales-return:%'")
check "sys_menu 承载 3 个退货权限码" "$codes" "3"
fk303=$(PSQLQ -c "select count(*) from pg_constraint where conname='fk_biz_sreturn_item_return' and contype='f'")
check "明细 → 单头 CASCADE 外键" "$fk303" "1"

echo; echo "== 1) admin 登录 =="
T_ADMIN=""; FIRST_PWD=""; MUST_CHANGE=false
for cand in 'Admin@So29!2026' 'Admin@123456' 'Admin@So28!2026' 'Admin@So27!2026'; do
  login_split admin "$cand" "admin_$(printf '%s' "$cand" | tr -cd 'A-Za-z0-9')"
  if [ -n "$T_ADMIN" ]; then FIRST_PWD="$cand"; break; fi
done
[ -z "$T_ADMIN" ] && { echo "FATAL: admin 口令未知，无法继续"; exit 1; }
ok "admin 登录成功（must_change_password=$MUST_CHANGE）"
if [ "$MUST_CHANGE" = "true" ]; then
  api pre_pwd_change PUT /system/auth/password "$T_ADMIN" \
      "{\"old_password\":\"$FIRST_PWD\",\"new_password\":\"Admin@So29!2026\"}"
  [ "$CODE" = "0" ] && ok "前置：首登改密 → Admin@So29!2026" || bad "前置改密失败 http=$HTTP $(d '.msg')"
  login_split admin 'Admin@So29!2026' admin_so29
  [ -n "$T_ADMIN" ] && ok "新口令重登成功" || { bad "新口令重登失败"; exit 1; }
fi

echo; echo "== 2) 从原发货单带出可退明细（GET /returnable/{shipment_id}） =="
api returnable_1 GET "/business/erp/sales-return/returnable/$SH1" "$T_ADMIN"
check "http" "$HTTP" "200"
check "行数" "$(d '.data | length')" "1"
check_n "已发量" '.data[0].shipped' 10
check_n "已退量" '.data[0].returned' 0
check_n "可退量" '.data[0].remaining' 10
check_n "单价从订单行带出" '.data[0].price' 100
check_n "税率从订单行带出" '.data[0].tax_rate' 13

echo; echo "== 3) 建单：items 留空 ⇒ 自动从发货单带出（DRAFT / SR- 取号） =="
api create_auto POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH1\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\",\"remark\":\"SO29 e2e：自动带出\"}"
check "http" "$HTTP" "200"
check "业务码" "$CODE" "0"
R1=$(d '.data.id')
R1_NO=$(d '.data.return_no')
check "状态" "$(d '.data.status')" "DRAFT"
check "明细行数" "$(d '.data.items | length')" "1"
check_n "明细数量 = 可退量" '.data.items[0].qty' 10
check_n "不含税合计" '.data.total_amount' 1000
check_n "税额合计" '.data.total_tax' 130
check_n "价税合计" '.data.total_with_tax' 1130
case "$R1_NO" in SR-*) ok "单号形如 SR-yyyyMMdd-NNNNNN（$R1_NO）" ;; *) bad "单号形状不对：$R1_NO" ;; esac
check "详情带出原订单号" "$(d '.data.so_no')" "SO29-SO-0001"
check "详情带出原发货单号" "$(d '.data.shipment_no')" "SO29-SH-0001"
check "详情带出客户名" "$(d '.data.customer_name')" "SO29 退货测试客户"
check "退货类型标签" "$(d '.data.return_type_label')" "质量"

echo; echo "== 4) 详情 / 分页 =="
api detail GET "/business/erp/sales-return/$R1" "$T_ADMIN"
check "http" "$HTTP" "200"
check "详情单号一致" "$(d '.data.return_no')" "$R1_NO"
check "明细 so_item_id 带出" "$(d '.data.items[0].so_item_id')" "$SO_ITEM1"

api page GET "/business/erp/sales-return/page?return_no=$R1_NO" "$T_ADMIN"
check "http" "$HTTP" "200"
check "分页命中 1 条" "$(d '.data.total')" "1"
check "分页行号一致" "$(d '.data.list[0].return_no')" "$R1_NO"
check "分页行明细为空" "$(d '.data.list[0].items | length')" "0"

echo; echo "== 5) 状态机：DRAFT → SUBMITTED → APPROVED → PENDING_POST → POSTED =="
api st_submitted PATCH "/business/erp/sales-return/$R1/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
check "提交 http" "$HTTP" "200"
check "提交后状态" "$(d '.data.status')" "SUBMITTED"

api st_approved PATCH "/business/erp/sales-return/$R1/status" "$T_ADMIN" '{"target_status":"APPROVED"}'
check "批准 http" "$HTTP" "200"
check "批准后状态" "$(d '.data.status')" "APPROVED"

api st_pending PATCH "/business/erp/sales-return/$R1/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
check "待过账 http" "$HTTP" "200"
check "待过账后状态" "$(d '.data.status')" "PENDING_POST"

# ⚠ SO-30 已解锁 PENDING_POST → POSTED（过账事务体在 sales_return_posting.rs）：
#   本脚本原来断言的「409 + 点名 SO-30」是 SO-29 当时的占位行为，现已改为「过账成功」。
#   过账的六件事（回库 / 红字入库凭证 / 冲应收 / 红字发票 / delivered_qty / 额度释放）
#   由 scripts/verify/so30_return_posting_e2e.sh 逐条验证；本脚本只确认状态到了 POSTED。
api st_posted PATCH "/business/erp/sales-return/$R1/status" "$T_ADMIN" '{"target_status":"POSTED"}'
check "过账 http（SO-30 解锁后应成功）" "$HTTP" "200"
check "过账后状态" "$(d '.data.status')" "POSTED"

trail=$(PSQLQ -c "select count(*) from biz_status_log where entity_type='SALES_RETURN' and entity_id='$R1'")
check "biz_status_log 轨迹条数（建单 + 4 次流转）" "$trail" "5"
trail_no=$(PSQLQ -c "select count(distinct entity_no) from biz_status_log where entity_type='SALES_RETURN' and entity_id='$R1'")
check "轨迹里 entity_no 唯一（= 单号）" "$trail_no" "1"

echo; echo "== 6) 非法边 / 必填原因 =="
api create_draft2 POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH2\",\"warehouse_id\":\"$WH\",\"return_type\":\"WRONG_ITEM\"}"
check "第二张草稿 http" "$HTTP" "200"
R2=$(d '.data.id')
check_n "第二张自动带出 5" '.data.total_with_tax' 1130
api illegal_edge PATCH "/business/erp/sales-return/$R2/status" "$T_ADMIN" '{"target_status":"APPROVED"}'
check "DRAFT → APPROVED（跳步）http" "$HTTP" "409"
case "$(d '.msg')" in
  *合法边表*) ok "409 文案给出当前可流转去向" ;;
  *) bad "409 文案没给去向：$(d '.msg')" ;;
esac

api st_sub2 PATCH "/business/erp/sales-return/$R2/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
check "第二张提交 http" "$HTTP" "200"
api cancel_no_reason PATCH "/business/erp/sales-return/$R2/status" "$T_ADMIN" '{"target_status":"CANCELLED"}'
check "提交后作废缺 reason http" "$HTTP" "400"
case "$(d '.msg')" in
  *原因*) ok "400 文案要求给出原因" ;;
  *) bad "400 文案没要求原因：$(d '.msg')" ;;
esac
api cancel_with_reason PATCH "/business/erp/sales-return/$R2/status" "$T_ADMIN" \
  '{"target_status":"CANCELLED","reason":"SO29 e2e：客户改主意，不再退货"}'
check "带原因作废 http" "$HTTP" "200"
check "作废后状态" "$(d '.data.status')" "CANCELLED"
api terminal_edge PATCH "/business/erp/sales-return/$R2/status" "$T_ADMIN" '{"target_status":"DRAFT"}'
check "终态再流转 http" "$HTTP" "409"
trail2=$(PSQLQ -c "select count(*) from biz_status_log where entity_type='SALES_RETURN' and entity_id='$R2'")
check "第二张轨迹条数（建单 + 提交 + 作废）" "$trail2" "3"

echo; echo "== 7) 超退守卫：qty > 已发货量 − 已退量 =="
# SH2 第 2 行已发 5、已退 0（第二张已作废）⇒ 退 3 成功，再退 3 必须被拒（3+3 > 5）
api over_first POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH2\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\",\"remark\":\"SO29 e2e：先退 3\",
    \"items\":[{\"so_item_id\":\"$SO_ITEM2\",\"qty\":3}]}"
check "退 3 http" "$HTTP" "200"
R3=$(d '.data.id')
check_n "退 3 后金额" '.data.total_with_tax' 678

api over_second POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH2\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\",\"remark\":\"SO29 e2e：再退 3（超退）\",
    \"items\":[{\"so_item_id\":\"$SO_ITEM2\",\"qty\":3}]}"
check "再退 3 http（超退被拒）" "$HTTP" "400"
case "$(d '.msg')" in
  *超退*) ok "400 文案点名「超退」" ;;
  *) bad "400 文案没点名超退：$(d '.msg')" ;;
esac
over_left=$(PSQLQ -c "select count(*) from biz_sales_return where remark='SO29 e2e：再退 3（超退）'")
check "被拒的超退单没有落库" "$over_left" "0"

# 作废前一张（DRAFT → CANCELLED 不要原因）⇒ 已退量释放 ⇒ 同样退 5 又放行
api cancel_r3 PATCH "/business/erp/sales-return/$R3/status" "$T_ADMIN" '{"target_status":"CANCELLED"}'
check "作废退 3 那张 http" "$HTTP" "200"
api over_retry POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH2\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\",\"remark\":\"SO29 e2e：作废后重退 5\",
    \"items\":[{\"so_item_id\":\"$SO_ITEM2\",\"qty\":5}]}"
check "作废后重退 5 http" "$HTTP" "200"
check_n "重退 5 金额" '.data.total_with_tax' 1130

echo; echo "== 8) 原单闸 / 参数校验 =="
api not_posted POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH3\",\"warehouse_id\":\"$WH\",\"return_type\":\"QUALITY\"}"
check "未过账发货单退货 http" "$HTTP" "409"
case "$(d '.msg')" in
  *POSTED*) ok "409 文案说明只有已过账发货单能退" ;;
  *) bad "409 文案没说明过账要求：$(d '.msg')" ;;
esac

api bad_type POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH1\",\"warehouse_id\":\"$WH\",\"return_type\":\"REFUND\"}"
check "非法 return_type http" "$HTTP" "400"

api no_items_no_shipment POST /business/erp/sales-return "$T_ADMIN" \
  "{\"customer_id\":\"$CUST\",\"warehouse_id\":\"$WH\",\"return_type\":\"OTHER\"}"
check "既无 items 又无 shipment_id http" "$HTTP" "400"

api bad_warehouse POST /business/erp/sales-return "$T_ADMIN" \
  "{\"shipment_id\":\"$SH1\",\"warehouse_id\":\"00000000-0000-0000-0000-000000000000\",\"return_type\":\"QUALITY\"}"
check "仓库不存在 http" "$HTTP" "400"

echo; echo "== 9) 按订单退货（不指定发货单，用 delivered_qty 口径） =="
api by_order POST /business/erp/sales-return "$T_ADMIN" \
  "{\"so_id\":\"$SO\",\"warehouse_id\":\"$WH\",\"return_type\":\"OTHER\",\"remark\":\"SO29 e2e：按订单退第 3 行\",
    \"items\":[{\"so_item_id\":\"$SO_ITEM3\",\"qty\":3}]}"
check "按订单退 3 http" "$HTTP" "200"
check_n "按订单退 3 金额" '.data.total_with_tax' 169.5
R5=$(d '.data.id')
api by_order_over POST /business/erp/sales-return "$T_ADMIN" \
  "{\"so_id\":\"$SO\",\"warehouse_id\":\"$WH\",\"return_type\":\"OTHER\",
    \"items\":[{\"so_item_id\":\"$SO_ITEM3\",\"qty\":0.0001}]}"
check "按订单再退 0.0001（超退）http" "$HTTP" "400"

echo; echo "== 10) 无原单的 OTHER 行（material_id 必填） =="
api manual_no_material POST /business/erp/sales-return "$T_ADMIN" \
  "{\"customer_id\":\"$CUST\",\"warehouse_id\":\"$WH\",\"return_type\":\"OTHER\",
    \"items\":[{\"qty\":1,\"price\":10}]}"
check "无原单且缺 material_id http" "$HTTP" "400"
api manual_ok POST /business/erp/sales-return "$T_ADMIN" \
  "{\"customer_id\":\"$CUST\",\"warehouse_id\":\"$WH\",\"return_type\":\"OTHER\",\"remark\":\"SO29 e2e：无原单手工退货\",
    \"items\":[{\"material_id\":\"$MAT\",\"qty\":2,\"price\":9.5,\"tax_rate\":13}]}"
check "无原单手工退货 http" "$HTTP" "200"
check_n "手工行金额" '.data.total_amount' 19
check_n "手工行税额" '.data.total_tax' 2.47

echo; echo "== 11) 汇总 =="
api page_all GET "/business/erp/sales-return/page?page=1&page_size=50" "$T_ADMIN"
check "分页 http" "$HTTP" "200"
check "夹具客户名下退货单数（6 张成功，7 次被拒不落库）" "$(d '.data.total')" "6"
by_status=$(PSQLQ -c "select string_agg(status||'='||n, ',' order by status) from (select status, count(*) n from biz_sales_return where customer_id='$CUST' group by status) t")
echo "  · 夹具客户名下按状态：$by_status"
check "状态分布" "$by_status" "CANCELLED=2,DRAFT=3,PENDING_POST=1"

echo
echo "=================================================================="
printf '结果：%d 通过 / %d 失败\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
  printf '失败项：\n'; for item in "${FAILED[@]}"; do printf '  - %s\n' "$item"; done
  exit 1
fi
echo "全部通过 ✅"
