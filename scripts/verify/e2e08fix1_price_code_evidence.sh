#!/usr/bin/env bash
# E2E08-FIX-1 线上实测取证（真 token curl · 145 演练机）
#
# 覆盖卡面验收 ①~⑤：
#   ① 建 qty 5~20 的阶梯 → 再建 1~10 → 409 + 点名重叠区间
#   ② 建 qty 21~30（不重叠）→ 200
#   ③ 不同物料的同区间 → 200（不误伤）
#   ④ PUT 客户带新 code → 400「客户编码不可修改」
#   ⑤ 七类主数据的编码字段都改不动（客户 / 资产分类 = 400；供应商 / 物料 / 仓库 / 单位 /
#      费用类型 = 字段被忽略、库里编码不变）—— 逐类贴 HTTP 码 + 库内编码前后对比
#
# 跑法（145）：
#   BASE=http://127.0.0.1:8034/api bash scripts/verify/e2e08fix1_price_code_evidence.sh
# （443 个断言量级不需要，本脚本 ~40 条；PASS/FAIL 汇总在末尾）
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
# 本卡的独立工作目录（**必须在 source 之前导出**：common 里是 `WORK=${WORK:-/tmp/e2e08}`，
# 先导出才不会把 raw/断言日志写进别卡共用的 /tmp/e2e08）
WORK=${WORK:-/tmp/e2e08fix1}
export WORK
. "$HERE/e2e08_master_common.sh"

RAW=$WORK/raw
LOG=$WORK/asserts.log
mkdir -p "$RAW"
: > "$LOG"
PASS=0
FAIL=0

say "BASE=$BASE"
say "WORK=$WORK"
T=$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1
fixtures
say "UNIT_ID=$UNIT_ID  ACC_ID=$ACC_ID"
say

CUS_CODE="E2E08F1-C1"
MAT_A_CODE="E2E08F1-MA"
MAT_B_CODE="E2E08F1-MB"

# ---------- 夹具：客户 + 两个物料 ----------
say "=== 夹具 ==="
api fx_cus POST /business/erp/customer "$T" "{\"code\":\"$CUS_CODE\",\"name\":\"E2E08FIX1 取证客户\",\"status\":\"ACTIVE\"}"
if [ "$HTTP" = "200" ]; then
  CUS=$(data '.data.id')
else
  # 上一轮残留（软/硬都在）：按编码查回来
  CUS=$(SQ "select id from biz_customer where code='$CUS_CODE' and deleted_at is null")
  say "  客户已存在，复用 id=$CUS（HTTP $HTTP）"
fi
check "客户就绪" "$([ -n "${CUS:-}" ] && echo ok)" "ok"

api fx_mat_a POST /business/erp/material "$T" "{\"material_code\":\"$MAT_A_CODE\",\"material_name\":\"E2E08FIX1 物料甲\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"status\":1}"
if [ "$HTTP" = "200" ]; then
  MAT_A=$(data '.data.id')
else
  MAT_A=$(SQ "select id from biz_material where material_code='$MAT_A_CODE' and deleted_at is null")
  say "  物料甲已存在，复用 id=$MAT_A（HTTP $HTTP）"
fi
api fx_mat_b POST /business/erp/material "$T" "{\"material_code\":\"$MAT_B_CODE\",\"material_name\":\"E2E08FIX1 物料乙\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"status\":1}"
if [ "$HTTP" = "200" ]; then
  MAT_B=$(data '.data.id')
else
  MAT_B=$(SQ "select id from biz_material where material_code='$MAT_B_CODE' and deleted_at is null")
  say "  物料乙已存在，复用 id=$MAT_B（HTTP $HTTP）"
fi
check "物料甲就绪" "$([ -n "${MAT_A:-}" ] && echo ok)" "ok"
check "物料乙就绪" "$([ -n "${MAT_B:-}" ] && echo ok)" "ok"

# 先把本卡可能需要清理的档位清掉（只动这三个物料下的行 → 可重跑）
SQ "delete from biz_price_tier where material_id in ('$MAT_A'::uuid, '$MAT_B'::uuid)" >/dev/null
say

TIER_BODY() { # TIER_BODY <material> <min> <max|-> [tier_type]
  printf '{"material_id":"%s","customer_id":"%s","tier_type":"%s","min_qty":%s,"max_qty":%s,"price":92,"effective_from":"2031-01-01","effective_to":"2031-12-31"}' \
    "$1" "$CUS" "${4:-TIER}" "$2" "$3"
}

say "=== ① 建 5~20 后再建 1~10 → 409 且点名重叠区间 ==="
api t1 POST /business/erp/price/tier "$T" "$(TIER_BODY "$MAT_A" 5 20)"
check "①a 建 5~20 HTTP" "$HTTP" "200"
T1=$(data '.data.id')
api t2 POST /business/erp/price/tier "$T" "$(TIER_BODY "$MAT_A" 1 10)"
check "①b 建 1~10 HTTP（必须 409）" "$HTTP" "409"
check "①c 业务码 10003" "$CODE" "10003"
contains "①d 点名已有区间 5~20" "$(data '.msg')" "5~20"
contains "①e 点名本次区间 1~10" "$(data '.msg')" "1~10"
contains "①f 点名生效期 2031-01-01~2031-12-31" "$(data '.msg')" "2031-01-01~2031-12-31"
check "①g 被拒的档没落库（存活档数 = 1）" \
  "$(SQ "select count(*) from biz_price_tier where material_id='$MAT_A'::uuid and deleted_at is null")" "1"
say "  · 409 原文：$(data '.msg')"

say
say "=== ② 建 21~30（不重叠）→ 200 ==="
api t3 POST /business/erp/price/tier "$T" "$(TIER_BODY "$MAT_A" 21 30)"
check "②a 建 21~30 HTTP" "$HTTP" "200"
T3=$(data '.data.id')

say
say "=== ③ 不同物料同区间 → 200（不误伤） ==="
api t4 POST /business/erp/price/tier "$T" "$(TIER_BODY "$MAT_B" 5 20)"
check "③a 另一物料建 5~20 HTTP" "$HTTP" "200"
T4=$(data '.data.id')

say
say "=== ③b 同一物料但生效期错开 / 类型不同 → 200（另两个维度） ==="
api t5 POST /business/erp/price/tier "$T" \
  "{\"material_id\":\"$MAT_A\",\"customer_id\":\"$CUS\",\"tier_type\":\"TIER\",\"min_qty\":5,\"max_qty\":20,\"price\":93,\"effective_from\":\"2032-01-01\",\"effective_to\":\"2032-12-31\"}"
check "③b-1 生效期不交 → 200" "$HTTP" "200"
T5=$(data '.data.id')
api t6 POST /business/erp/price/tier "$T" "$(TIER_BODY "$MAT_A" 5 20 PROMO)"
check "③b-2 PROMO 与 TIER 并存 → 200" "$HTTP" "200"
T6=$(data '.data.id')

say
say "=== ③c 改档位（PUT）也拦：把 21~30 下界改到 18 → 409；只改价 → 200 ==="
api t3_price PUT "/business/erp/price/tier/$T3" "$T" '{"price":88}'
check "③c-1 只改价格 HTTP" "$HTTP" "200"
check "③c-2 价格已改" "$(data '.data.price')" "88.0000"
api t3_bad PUT "/business/erp/price/tier/$T3" "$T" '{"min_qty":18}'
check "③c-3 改区间造成重叠 HTTP（必须 409）" "$HTTP" "409"
contains "③c-4 点名叫 5~20" "$(data '.msg')" "5~20"
check "③c-5 被拒的编辑没落库（min_qty 仍 21）" \
  "$(SQ "select min_qty::numeric(18,2)::text from biz_price_tier where id='$T3'::uuid")" "21.00"

say
say "=== ④ PUT 客户带新 code → 400「客户编码不可修改」 ==="
api cus_chg PUT "/business/erp/customer/$CUS" "$T" "{\"code\":\"$CUS_CODE-CHG\",\"contact_phone\":\"010-99998888\"}"
check "④a HTTP（必须 400）" "$HTTP" "400"
check "④b 业务码 10001" "$CODE" "10001"
contains "④c 文案点名不可修改" "$(data '.msg')" "客户编码不可修改"
check "④d 库里编码未变" "$(SQ "select code from biz_customer where id='$CUS'::uuid")" "$CUS_CODE"
check "④e 同一请求里的其他字段也没落库（整体 400）" \
  "$(SQ "select coalesce(contact_phone,'') from biz_customer where id='$CUS'::uuid")" ""
say "  · 400 原文：$(data '.msg')"

say
say "=== ④b 带原编码改其他字段 → 200（前端整体提交表单的形态） ==="
api cus_ok PUT "/business/erp/customer/$CUS" "$T" "{\"code\":\"$CUS_CODE\",\"contact_phone\":\"010-99998888\"}"
check "④b-1 HTTP" "$HTTP" "200"
check "④b-2 电话已改" "$(data '.data.contact_phone')" "010-99998888"
check "④b-3 编码仍是原值" "$(data '.data.code')" "$CUS_CODE"
# 复位，别把夹具留脏
api cus_reset PUT "/business/erp/customer/$CUS" "$T" "{\"contact_phone\":\"\"}"
check "④b-4 复位电话" "$HTTP" "200"

say
say "=== ⑤ 七类主数据「编码字段」逐个实测（客户/资产分类 = 400；其余 = 字段被忽略） ==="

# ⑤-1 供应商（既有行；只发 supplier_code，其余字段 merge 保留）
SUP_ID=$(SQ "select id from biz_supplier where deleted_at is null order by supplier_code limit 1")
SUP_CODE=$(SQ "select supplier_code from biz_supplier where id='$SUP_ID'::uuid")
api c5_sup PUT "/business/erp/supplier/$SUP_ID" "$T" '{"supplier_code":"E2E08F1-SUP-CHG"}'
check "⑤-1a 供应商 HTTP" "$HTTP" "200"
check "⑤-1b 供应商库内编码未变" "$(SQ "select supplier_code from biz_supplier where id='$SUP_ID'::uuid")" "$SUP_CODE"

# ⑤-2 物料（既有行）
MAT_X=$(SQ "select id from biz_material where deleted_at is null and id <> '$MAT_A'::uuid and id <> '$MAT_B'::uuid order by material_code limit 1")
MAT_X_CODE=$(SQ "select material_code from biz_material where id='$MAT_X'::uuid")
api c5_mat PUT "/business/erp/material/$MAT_X" "$T" '{"material_code":"E2E08F1-MAT-CHG"}'
check "⑤-2a 物料 HTTP" "$HTTP" "200"
check "⑤-2b 物料库内编码未变" "$(SQ "select material_code from biz_material where id='$MAT_X'::uuid")" "$MAT_X_CODE"

# ⑤-3 仓库（既有行）
WH_ID=$(SQ "select id from biz_warehouse where deleted_at is null order by warehouse_code limit 1")
WH_CODE=$(SQ "select warehouse_code from biz_warehouse where id='$WH_ID'::uuid")
api c5_wh PUT "/business/erp/warehouse/$WH_ID" "$T" '{"warehouse_code":"E2E08F1-WH-CHG"}'
check "⑤-3a 仓库 HTTP" "$HTTP" "200"
check "⑤-3b 仓库库内编码未变" "$(SQ "select warehouse_code from biz_warehouse where id='$WH_ID'::uuid")" "$WH_CODE"

# ⑤-4 单位（既有行）
UNIT_X=$(SQ "select id from biz_unit where deleted_at is null order by unit_code limit 1")
UNIT_X_CODE=$(SQ "select unit_code from biz_unit where id='$UNIT_X'::uuid")
api c5_unit PUT "/business/erp/unit/$UNIT_X" "$T" '{"unit_code":"E2E08F1-U-CHG"}'
check "⑤-4a 单位 HTTP" "$HTTP" "200"
check "⑤-4b 单位库内编码未变" "$(SQ "select unit_code from biz_unit where id='$UNIT_X'::uuid")" "$UNIT_X_CODE"

# ⑤-5 费用类型（既有行；type_update 会整行覆盖 → 必须回填全部字段，否则「测试编码不可改」
#      会顺手把名称/科目/开关/额度改掉，那是不可接受的副作用）
ET_ID=$(SQ "select id from biz_expense_type where deleted_at is null order by type_code limit 1")
ET_ROW=$(PSQLQ -c "select concat_ws('|', type_code, type_name, account_id::text, tax_deductible, budget_controlled, need_invoice_no, amount_limit::text, sort_order, status, coalesce(remark,'')) from biz_expense_type where id='$ET_ID'::uuid")
ET_CODE=$(printf '%s' "$ET_ROW" | cut -d'|' -f1)
ET_NAME=$(printf '%s' "$ET_ROW" | cut -d'|' -f2)
ET_ACC=$(printf '%s' "$ET_ROW" | cut -d'|' -f3)
ET_TAX=$(printf '%s' "$ET_ROW" | cut -d'|' -f4)
ET_BUD=$(printf '%s' "$ET_ROW" | cut -d'|' -f5)
ET_INV=$(printf '%s' "$ET_ROW" | cut -d'|' -f6)
ET_AMT=$(printf '%s' "$ET_ROW" | cut -d'|' -f7)
ET_SORT=$(printf '%s' "$ET_ROW" | cut -d'|' -f8)
ET_STATUS=$(printf '%s' "$ET_ROW" | cut -d'|' -f9)
ET_REMARK=$(printf '%s' "$ET_ROW" | cut -d'|' -f10)
api c5_et PUT "/business/erp/expense-type/$ET_ID" "$T" \
  "$(jq -nc --arg c "E2E08F1-ET-CHG" --arg n "$ET_NAME" --arg a "$ET_ACC" \
      --argjson tax "$([ "$ET_TAX" = "1" ] && echo true || echo false)" \
      --argjson bud "$([ "$ET_BUD" = "1" ] && echo true || echo false)" \
      --argjson inv "$([ "$ET_INV" = "1" ] && echo true || echo false)" \
      --argjson amt "$ET_AMT" --argjson sort "$ET_SORT" --argjson st "$ET_STATUS" --arg r "$ET_REMARK" \
      '{type_code:$c,type_name:$n,account_id:$a,tax_deductible:$tax,budget_controlled:$bud,need_invoice_no:$inv,amount_limit:$amt,sort_order:$sort,status:$st,remark:$r}')"
check "⑤-5a 费用类型 HTTP" "$HTTP" "200"
check "⑤-5b 费用类型库内编码未变" "$(SQ "select type_code from biz_expense_type where id='$ET_ID'::uuid")" "$ET_CODE"
check "⑤-5c 费用类型名称没被带跑（回填生效）" "$(SQ "select type_name from biz_expense_type where id='$ET_ID'::uuid")" "$ET_NAME"

# ⑤-6 资产分类（既有行；validate 要求 name + 使用月数，故回填后带新码 → 应 400）
AC_ID=$(SQ "select id from biz_asset_category where deleted_at is null order by category_code limit 1")
AC_CODE=$(SQ "select category_code from biz_asset_category where id='$AC_ID'::uuid")
AC_NAME=$(SQ "select category_name from biz_asset_category where id='$AC_ID'::uuid")
AC_MONTHS=$(SQ "select useful_life_months from biz_asset_category where id='$AC_ID'::uuid")
api c5_ac PUT "/business/erp/asset-category/$AC_ID" "$T" \
  "$(jq -nc --arg c "E2E08F1-AC-CHG" --arg n "$AC_NAME" --argjson m "$AC_MONTHS" '{category_code:$c,category_name:$n,useful_life_months:$m}')"
check "⑤-6a 资产分类 HTTP（必须 400）" "$HTTP" "400"
contains "⑤-6b 文案点名不可修改" "$(data '.msg')" "不可修改"
check "⑤-6c 资产分类库内编码未变" "$(SQ "select category_code from biz_asset_category where id='$AC_ID'::uuid")" "$AC_CODE"

say
say "=== 收尾：清掉本卡夹具（档位 → 物料 → 客户） ==="
SQ "delete from biz_price_tier where material_id in ('$MAT_A'::uuid, '$MAT_B'::uuid)" >/dev/null
for id in "$T1" "$T3" "$T4" "$T5" "$T6"; do
  [ -n "${id:-}" ] && : # 档位已被上一条 SQL 清掉；这里只留痕迹
done
api fx_mat_a_del DELETE "/business/erp/material/$MAT_A" "$T"
check "清理物料甲 HTTP" "$HTTP" "200"
api fx_mat_b_del DELETE "/business/erp/material/$MAT_B" "$T"
check "清理物料乙 HTTP" "$HTTP" "200"
api fx_cus_del DELETE "/business/erp/customer/$CUS" "$T"
check "清理客户 HTTP" "$HTTP" "200"
check "残留档位 = 0" "$(SQ "select count(*) from biz_price_tier where material_id in ('$MAT_A'::uuid, '$MAT_B'::uuid)")" "0"

summary

# 原始报文体留证（每条断言对应的 JSON）
say
say "== 原始响应（$RAW） =="
for f in t1 t2 t3 t4 t5 t6 t3_bad cus_chg cus_ok c5_sup c5_mat c5_wh c5_unit c5_et c5_ac; do
  [ -f "$RAW/$f.json" ] && printf '%-10s %s\n' "$f" "$(jq -c '{code,msg,value:(.data.price // .data.code // .data.status // .data.contact_phone // "")}' "$RAW/$f.json" 2>/dev/null | head -c 300)"
done
