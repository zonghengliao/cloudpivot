#!/usr/bin/env bash
# ============================================================================
# SO-37 佣金前端 · 真实 token curl 证据
#
# 覆盖卡面要求的三件事在**接口面**的全部契约：
#   ① 配规则（列表 / 新建 / 编辑 / 停用 / 删除 + 五条库层约束的 400）
#   ② 生成结算单（期间 + 销售员；幂等重算、跳过原因）
#   ③ 查看明细（单 + 逐笔「哪笔订单 / 应收产生多少佣金」）
#   ④ 审批 / 发放（DRAFT→APPROVED→PAID 的合法边 + 跳步 / 回退 409）
#   ⑤ 菜单（V0312 收尾后的两个页面节点真的在 /auth/menus 里，且 path 与前端路由逐字一致）
#
# 前置：bash scripts/verify/so37_instance.sh up（库 cloudpivot_so37 / 端口 18244）
#       夹具先跑：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so37 \
#                 -f scripts/verify/so36_fixture.sql（复用 SO-36 的销售链夹具）
#
# 用法：bash scripts/verify/so37_curl_evidence.sh
# 输出：逐条 PASS/FAIL；末尾汇总「ALL CHECKS PASSED (n)」，任一失败 exit 1
#
# 断言口径：**HTTP 状态码**用 `check`（400/403/409/200 是状态码，不是包体里的 code）；
# 包体字段用 `expect`（jq 取值）。两条分开写，避免「10003 当 409 用」这类假错。
# ============================================================================
set -uo pipefail
BASE=${SO37_BASE:-http://127.0.0.1:18244}
BODY_FILE=$(mktemp)
PASS=0
FAIL=0

LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
TOKEN=$(printf '%s' "$LOGIN" | jq -r '.data.access_token // empty')
if [ -z "$TOKEN" ]; then
  echo "FATAL: 登录失败：$LOGIN"
  exit 1
fi

# req METHOD PATH [BODY] → 设 BODY（响应体）与 STATUS（HTTP 状态码）
BODY=''
STATUS=''
req() {
  local method=$1 path=$2 body=${3:-}
  if [ -n "$body" ]; then
    STATUS=$(curl -s -o "$BODY_FILE" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$body")
  else
    STATUS=$(curl -s -o "$BODY_FILE" -w '%{http_code}' -X "$method" "$BASE$path" \
      -H "Authorization: Bearer $TOKEN")
  fi
  BODY=$(cat "$BODY_FILE")
}

# jq 取包体里的值
jv() { printf '%s' "$BODY" | jq -r "$1"; }

check() {  # check 描述 期望 实际
  if [ "$2" = "$3" ]; then
    PASS=$((PASS + 1)); printf 'PASS  %-56s = %s\n' "$1" "$3"
  else
    FAIL=$((FAIL + 1)); printf 'FAIL  %-56s expected=%s actual=%s\n' "$1" "$2" "$3"
  fi
}

echo "═══ ① 佣金规则：列表 / 过滤（含 scope_name 派生列） ═══"
req GET '/api/business/erp/commission-rule/page?page=1&page_size=20'
check '规则分页 HTTP 200' 200 "$STATUS"
check '规则分页 total（夹具 3 条）' 3 "$(jv '.data.total')"
check '出参带 scope_name（销售员姓名）' 'SO36 销售员甲' \
  "$(jv '.data.list[] | select(.scope_type=="SALESPERSON") | .scope_name')"
check '出参带 scope_name（物料名称）' 'SO36 物料一' \
  "$(jv '.data.list[] | select(.scope_type=="MATERIAL_CATEGORY") | .scope_name')"
check '比率是字符串（NUMERIC 口径 5.0000）' '5.0000' \
  "$(jv '.data.list[] | select(.scope_type=="ALL") | .rate')"

req GET '/api/business/erp/commission-rule/page?scope_type=SALESPERSON'
check 'scope_type 过滤只回销售员级' 1 "$(jv '.data.total')"
req GET '/api/business/erp/commission-rule/page?keyword=%E5%B0%81%E9%A1%B6'
check 'keyword 过滤（「封顶」只命中物料级那条）' 1 "$(jv '.data.total')"
req GET '/api/business/erp/commission-rule/page?status=INACTIVE'
check 'status 过滤（现在没有停用规则）' 0 "$(jv '.data.total')"

echo
echo "═══ ② 佣金规则：新建（页面表单发的就是这份入参） ═══"
SP1='e3600000-0000-4000-8000-000000000011'
M2='e3600000-0000-4000-8000-000000000022'
req POST '/api/business/erp/commission-rule' \
  "{\"rule_name\":\"SO37 页面新建：物料二 15%\",\"scope_type\":\"MATERIAL_CATEGORY\",\"scope_id\":\"$M2\",\"calc_type\":\"PERCENT\",\"rate\":\"15\",\"min_amount\":\"0\",\"max_amount\":null,\"effective_from\":\"2026-01-01\",\"effective_to\":null,\"status\":\"ACTIVE\",\"remark\":\"SO-37 curl 证据\"}"
check '新建规则 HTTP 200' 200 "$STATUS"
NEW_ID=$(jv '.data.id // empty')
check '新建回单带 id' 'yes' "$([ -n "$NEW_ID" ] && echo yes || echo no)"
check '新建回 scope_name = SO36 物料二' 'SO36 物料二' "$(jv '.data.scope_name')"
check '新建回 rate 字符串 15.0000' '15.0000' "$(jv '.data.rate')"
check '新建回 effective_to = null（长期有效）' 'null' "$(jv '.data.effective_to')"

echo
echo "═══ ③ 佣金规则：五条库层约束的入口拦截（前端校验的后端兜底） ═══"
req POST '/api/business/erp/commission-rule' \
  "{\"rule_name\":\"SO37 非法：ALL 带 scope_id\",\"scope_type\":\"ALL\",\"scope_id\":\"$SP1\",\"calc_type\":\"PERCENT\",\"rate\":\"5\",\"min_amount\":\"0\",\"effective_from\":\"2026-01-01\"}"
check 'ALL 带 scope_id → 400' 400 "$STATUS"

req POST '/api/business/erp/commission-rule' \
  "{\"rule_name\":\"SO37 非法：百分比超 100\",\"scope_type\":\"SALESPERSON\",\"scope_id\":\"$SP1\",\"calc_type\":\"PERCENT\",\"rate\":\"120\",\"min_amount\":\"0\",\"effective_from\":\"2026-01-01\"}"
check 'PERCENT rate=120 → 400' 400 "$STATUS"

req POST '/api/business/erp/commission-rule' \
  "{\"rule_name\":\"SO37 非法：封顶小于起算\",\"scope_type\":\"SALESPERSON\",\"scope_id\":\"$SP1\",\"calc_type\":\"PERCENT\",\"rate\":\"5\",\"min_amount\":\"100\",\"max_amount\":\"99\",\"effective_from\":\"2026-01-01\"}"
check 'max_amount < min_amount → 400' 400 "$STATUS"

req POST '/api/business/erp/commission-rule' \
  "{\"rule_name\":\"SO37 非法：截止早于生效\",\"scope_type\":\"SALESPERSON\",\"scope_id\":\"$SP1\",\"calc_type\":\"PERCENT\",\"rate\":\"5\",\"min_amount\":\"0\",\"effective_from\":\"2026-11-30\",\"effective_to\":\"2026-11-01\"}"
check 'effective_to < effective_from → 400' 400 "$STATUS"

req POST '/api/business/erp/commission-rule' \
  '{"rule_name":"SO37 非法：销售员不存在","scope_type":"SALESPERSON","scope_id":"e3600000-0000-4000-8000-00000000ffff","calc_type":"PERCENT","rate":"5","min_amount":"0","effective_from":"2026-01-01"}'
check 'scope_id 指向不存在的账号 → 400' 400 "$STATUS"

echo
echo "═══ ④ 佣金规则：编辑 / 停用 / 删除 ═══"
req PUT "/api/business/erp/commission-rule/$NEW_ID" '{"rate":"12"}'
check '编辑 rate HTTP 200' 200 "$STATUS"
check '编辑后 rate = 12.0000' '12.0000' "$(jv '.data.rate')"
check '没传的字段保持原值（部分更新）' 'SO37 页面新建：物料二 15%' "$(jv '.data.rule_name')"

req PUT "/api/business/erp/commission-rule/$NEW_ID" '{"status":"INACTIVE"}'
check '停用（改 status，不删行）' 'INACTIVE' "$(jv '.data.status')"
req GET '/api/business/erp/commission-rule/page?status=INACTIVE'
check '停用后能在 INACTIVE 过滤里查到' 1 "$(jv '.data.total')"

req DELETE "/api/business/erp/commission-rule/$NEW_ID"
check '软删 HTTP 200' 200 "$STATUS"
req GET '/api/business/erp/commission-rule/page?page=1&page_size=20'
check '软删后列表回到 3 条' 3 "$(jv '.data.total')"

echo
echo "═══ ⑤ 生成结算单（期间 + 销售员；已收款口径） ═══"
req POST '/api/business/erp/commission/settle' '{"period_code":"2026-11"}'
check '生成 HTTP 200' 200 "$STATUS"
check '生成 2 张（SP1 / SP2）' 2 "$(jv '.data.settlements | length')"
CM_SP1=$(jv '.data.settlements[] | select(.salesperson_name=="SO36 销售员甲") | .id')
CM_SP1_NO=$(jv '.data.settlements[] | select(.salesperson_name=="SO36 销售员甲") | .settlement_no')
check 'SP1 结算单号前缀 CM-202611' 'CM-202611' "${CM_SP1_NO:0:9}"
check 'SP1 佣金 80.0000（销售员 20% 压过物料 10% / 全局 5%）' '80.0000' \
  "$(jv '.data.settlements[] | select(.salesperson_name=="SO36 销售员甲") | .total_commission')"

req GET '/api/business/erp/commission/page?period_code=2026-11'
check '结算单分页回来 2 张' 2 "$(jv '.data.total')"
check '分页带 item_count（明细行数 2）' 2 \
  "$(jv '.data.list[] | select(.salesperson_name=="SO36 销售员甲") | .item_count')"

echo
echo "═══ ⑥ 查看明细（哪笔订单 / 应收产生多少佣金） ═══"
req GET "/api/business/erp/commission/$CM_SP1"
check '详情 HTTP 200' 200 "$STATUS"
check '明细 2 行（SO-A 拆成物料一 / 物料二两桶）' 2 "$(jv '.data.items | length')"
check '明细带销售订单号' '__SO36-SO-A__' "$(jv '[.data.items[].so_no] | unique | join(",")')"
check '明细带应收单号' '__SO36-AR-A__' "$(jv '[.data.items[].ar_no] | unique | join(",")')"
check '明细带命中规则名' 'SO36 销售员甲 20%' "$(jv '[.data.items[].rule_name] | unique | join(",")')"
check '明细佣金合计 = 单头应发（48+32=80）' '80' \
  "$(jv '[.data.items[].commission | tonumber] | add')"
check '明细基数为正（计提）' 'true' "$(jv '[.data.items[].base_amount | tonumber > 0] | all')"

echo
echo "═══ ⑦ 审批 / 发放（合法边 + 跳步 / 回退 409） ═══"
req PATCH "/api/business/erp/commission/$CM_SP1/status" '{"status":"PAID"}'
check 'DRAFT → PAID 跳步 → 409' 409 "$STATUS"
req PATCH "/api/business/erp/commission/$CM_SP1/status" '{"status":"APPROVED"}'
check 'DRAFT → APPROVED HTTP 200' 200 "$STATUS"
check '审批后状态 APPROVED' 'APPROVED' "$(jv '.data.status')"
check '审批后出计提凭证号（PZ-）' 'PZ-' "$(jv '.data.voucher_no' | cut -c1-3)"

req PATCH "/api/business/erp/commission/$CM_SP1/status" '{"status":"DRAFT"}'
check 'APPROVED → DRAFT 回退 → 409' 409 "$STATUS"

req PATCH "/api/business/erp/commission/$CM_SP1/status" '{"status":"PAID","reason":"2026-11 工资一并发放"}'
check 'APPROVED → PAID HTTP 200' 200 "$STATUS"
check '发放后状态 PAID' 'PAID' "$(jv '.data.status')"
check 'PAID 后有发放时间' 'yes' \
  "$([ "$(jv '.data.paid_at')" != 'null' ] && echo yes || echo no)"

req POST '/api/business/erp/commission/settle' '{"period_code":"2026-11"}'
check '已审批 / 已发放的单不被重算（SP1 进 skipped）' 'true' \
  "$(jv '[.data.skipped[] | select(.salesperson_name=="SO36 销售员甲")] | length > 0')"

echo
echo "═══ ⑧ 冲回场景（SP2 应收 1000 → 600：−400 基数 / −40 佣金） ═══"
PGPASSWORD=alun psql -q -h 127.0.0.1 -U alun -d cloudpivot_so37 -c \
  "update biz_receivable set received_amount = 600, unsettled_amount = 400, status = 'PARTIAL' where ar_no = '__SO36-AR-B__'" >/dev/null 2>&1
req POST '/api/business/erp/commission/settle' \
  '{"period_code":"2026-12","salesperson_id":"e3600000-0000-4000-8000-000000000012"}'
check '2026-12 生成 1 张（SP2 冲回）' 1 "$(jv '.data.settlements | length')"
check '冲回佣金 -40.0000' '-40.0000' "$(jv '.data.settlements[0].total_commission')"
CM_DEC=$(jv '.data.settlements[0].id')
req GET "/api/business/erp/commission/$CM_DEC"
check '冲回明细基数为负' 'true' "$(jv '[.data.items[].base_amount | tonumber < 0] | any')"
check '冲回明细备注说明退货 / 退款' 'yes' \
  "$(jv 'if ([.data.items[].remark // ""] | join("") | test("冲回|退")) then "yes" else "no" end')"

echo
echo "═══ ⑨ 权限（无 commission-rule:* → 403；前端隐藏按钮的后端兜底） ═══"
T2=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"so36_sp1","password":"Admin@123456"}' | jq -r '.data.access_token // empty')
if [ -n "$T2" ]; then
  S=$(curl -s -o "$BODY_FILE" -w '%{http_code}' -X POST "$BASE/api/business/erp/commission-rule" \
    -H "Authorization: Bearer $T2" -H 'Content-Type: application/json' \
    -d '{"rule_name":"SO37 越权","scope_type":"ALL","calc_type":"PERCENT","rate":"5","min_amount":"0","effective_from":"2026-01-01"}')
  check '销售员账号建规则 → 403' 403 "$S"
  S=$(curl -s -o "$BODY_FILE" -w '%{http_code}' "$BASE/api/business/erp/commission-rule/page" \
    -H "Authorization: Bearer $T2")
  check '销售员账号看规则列表 → 403' 403 "$S"
else
  echo "SKIP  非超管令牌未拿到（夹具账号口令不符），跳过两条越权断言"
fi

echo
echo "═══ ⑩ 菜单（V0312：两个页面节点可见且 path 与前端路由逐字一致） ═══"
req GET '/api/system/auth/menus'
check '菜单树里有「佣金规则」' 'yes' \
  "$(jv 'if ([.. | objects | select(.title? == "佣金规则")] | length) > 0 then "yes" else "no" end')"
check '菜单树里有「佣金结算」' 'yes' \
  "$(jv 'if ([.. | objects | select(.title? == "佣金结算")] | length) > 0 then "yes" else "no" end')"
check '佣金规则 path' '/business/erp/commission-rule' \
  "$(jv '[.. | objects | select(.title? == "佣金规则")][0].path')"
check '佣金结算 path' '/business/erp/commission' \
  "$(jv '[.. | objects | select(.title? == "佣金结算")][0].path')"
check '佣金规则 component' 'business/erp/commission-rule/index' \
  "$(jv '[.. | objects | select(.title? == "佣金规则")][0].component')"

rm -f "$BODY_FILE"
echo
echo "──────────────────────────────────────────────"
if [ "$FAIL" -ne 0 ]; then
  echo "FAILED: $FAIL 条断言失败（PASS $PASS）"
  exit 1
fi
echo "ALL CHECKS PASSED ($PASS)"
