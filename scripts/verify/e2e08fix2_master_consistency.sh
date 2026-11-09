#!/usr/bin/env bash
# E2E08-FIX-2 线上实测取证（真 token curl · 145 演练机）
#
# 覆盖卡面验收 ①~④：
#   ① 六类（SUP/CUS/MAT/WH/AC/ET）手工编码**大小写原样保留**：
#      小写码落库仍是小写、响应也回小写；大写码仍是大写（两边都不改写）
#   ② POST /business/erp/unit 只带 unit_name → **400 + 10001 + 人话**（不是框架 422）
#   ③ 重复单位编码 → **409 + 66001 + 点名编码**（不是裸「唯一约束 uk_biz_unit_code」）
#   ④ GET /business/erp/unit/next-code → **400 + 10001 +「计量单位不提供自动取号」**（不是 66006 路由错）
#
# 跑法（145）：
#   BASE=http://127.0.0.1:8036/api bash scripts/verify/e2e08fix2_master_consistency.sh
# 夹具一律 `e2e08f2-` 前缀，跑完硬删自己的行（按前缀，不 DROP / 不 TRUNCATE / 不动他人数据）。
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
# 本卡独立工作目录（**必须在 source 之前 export**：common 里是 `WORK=${WORK:-/tmp/e2e08}`，
# 先导出才不会把 raw/断言日志写进别卡共用的 /tmp/e2e08）
WORK=${WORK:-/tmp/e2e08fix2}
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

# 分类 / 费用类型 / 物料 需要的科目三件套
ACC_ASSET=$(SQ "select id from biz_account where account_code='1601' and deleted_at is null")
ACC_ACCUM=$(SQ "select id from biz_account where account_code='1602' and deleted_at is null")
ACC_EXP=$(SQ "select id from biz_account where account_code like '5602.%' and deleted_at is null order by account_code limit 1")
[ -z "$ACC_ASSET" ] && ACC_ASSET=$ACC_ID
[ -z "$ACC_ACCUM" ] && ACC_ACCUM=$ACC_ID
[ -z "$ACC_EXP" ] && ACC_EXP=$ACC_ID

say "=== ⓪ 前置：手工编码开关必须是「允许」（否则 ① 的夹具建不出来）==="
api pol GET /business/erp/master-data/code-policy "$T"
check "手工编码开关" "$(data '.data.allow_manual_code')" "true"
say

########################################################################
# ① 六类手工编码：大小写原样保留
########################################################################
say "=== ① 六类手工编码大小写原样保留（小写码）==="

mk_sup() { # mk_sup <code> <name>
  api "sup_$1" POST /business/erp/supplier "$T" \
    "$(jq -nc --arg c "$1" --arg n "$2" '{supplier_code:$c,supplier_name:$n,credit_days:0,status:1}')"
}
mk_cus() {
  api "cus_$1" POST /business/erp/customer "$T" \
    "$(jq -nc --arg c "$1" --arg n "$2" '{code:$c,name:$n,status:"ACTIVE"}')"
}
mk_mat() {
  api "mat_$1" POST /business/erp/material "$T" \
    "$(jq -nc --arg c "$1" --arg n "$2" --arg u "$UNIT_ID" \
      '{material_code:$c,material_name:$n,unit_id:$u,material_type:"RAW",status:1}')"
}
mk_wh() {
  api "wh_$1" POST /business/erp/warehouse "$T" \
    "$(jq -nc --arg c "$1" --arg n "$2" \
      '{warehouse_code:$c,warehouse_name:$n,warehouse_type:"NORMAL",level:1,status:1,allow_negative:false,sort_order:0}')"
}
mk_ac() {
  api "ac_$1" POST /business/erp/asset-category "$T" \
    "$(jq -nc --arg c "$1" --arg n "$2" --arg a1 "$ACC_ASSET" --arg a2 "$ACC_ACCUM" --arg a3 "$ACC_EXP" \
      '{category_code:$c,category_name:$n,useful_life_months:36,salvage_rate:0.05,asset_account_id:$a1,accum_account_id:$a2,expense_account_id:$a3,status:1}')"
}
mk_et() {
  api "et_$1" POST /business/erp/expense-type "$T" \
    "$(jq -nc --arg c "$1" --arg n "$2" --arg a "$ACC_EXP" \
      '{type_code:$c,type_name:$n,account_id:$a,tax_deductible:true,amount_limit:"0",sort_order:90,status:1}')"
}

# 逐类：建 → HTTP 200 → 响应里的编码 == 提交的编码（逐字，含大小写）→ 库内编码 == 提交的编码
ONE() { # ONE <类标签> <提交编码> <响应编码> <表名> <列名>
  local label=$1 want=$2 got=$3 table=$4 col=$5
  check "① $label HTTP" "$HTTP" "200"
  check "① $label 响应编码原样" "$got" "$want"
  check "① $label 库内编码原样" \
    "$(SQ "select $col from $table where $col='$want' and deleted_at is null")" "$want"
}

LOW_SUP="e2e08f2-sup-lc";   mk_sup "$LOW_SUP" "E2E08F2 供应商小写";   ONE "供应商(小写)" "$LOW_SUP" "$(data '.data.supplier_code')" biz_supplier supplier_code
LOW_CUS="e2e08f2-cus-lc";   mk_cus "$LOW_CUS" "E2E08F2 客户小写";     ONE "客户(小写)"   "$LOW_CUS" "$(data '.data.code')"          biz_customer code
LOW_MAT="e2e08f2-mat-lc";   mk_mat "$LOW_MAT" "E2E08F2 物料小写";     ONE "物料(小写)"   "$LOW_MAT" "$(data '.data.material_code')"  biz_material material_code
LOW_WH="e2e08f2-wh-lc";     mk_wh  "$LOW_WH"  "E2E08F2 仓库小写";     ONE "仓库(小写)"   "$LOW_WH"  "$(data '.data.warehouse_code')" biz_warehouse warehouse_code
LOW_AC="e2e08f2-ac-lc";     mk_ac  "$LOW_AC"  "E2E08F2 分类小写";     ONE "分类(小写)"   "$LOW_AC"  "$(data '.data.category_code')"  biz_asset_category category_code
LOW_ET="e2e08f2-et-lc";     mk_et  "$LOW_ET"  "E2E08F2 类型小写";     ONE "费用类型(小写)" "$LOW_ET" "$(data '.data.type_code')"     biz_expense_type type_code

say
say "=== ①-b 大写码同样原样保留（不是「一律转大写」，是「不改」）==="
UP_SUP="E2E08F2-SUP-UP";  mk_sup "$UP_SUP" "E2E08F2 供应商大写";  ONE "供应商(大写)" "$UP_SUP" "$(data '.data.supplier_code')" biz_supplier supplier_code
UP_CUS="E2E08F2-CUS-UP";  mk_cus "$UP_CUS" "E2E08F2 客户大写";    ONE "客户(大写)"   "$UP_CUS" "$(data '.data.code')"          biz_customer code
UP_MAT="E2E08F2-MAT-UP";  mk_mat "$UP_MAT" "E2E08F2 物料大写";    ONE "物料(大写)"   "$UP_MAT" "$(data '.data.material_code')"  biz_material material_code
UP_WH="E2E08F2-WH-UP";    mk_wh  "$UP_WH"  "E2E08F2 仓库大写";    ONE "仓库(大写)"   "$UP_WH"  "$(data '.data.warehouse_code')" biz_warehouse warehouse_code
UP_AC="E2E08F2-AC-UP";    mk_ac  "$UP_AC"  "E2E08F2 分类大写";    ONE "分类(大写)"   "$UP_AC"  "$(data '.data.category_code')"  biz_asset_category category_code
UP_ET="E2E08F2-ET-UP";    mk_et  "$UP_ET"  "E2E08F2 类型大写";    ONE "费用类型(大写)" "$UP_ET" "$(data '.data.type_code')"     biz_expense_type type_code

say
say "=== ①-c 六类行为一致性的静态对照（同一条规则表里只有一种行为）==="
say "  提交的是小写 → 六类库内都是小写；提交的是大写 → 六类库内都是大写（大小写不被平台改写）"

########################################################################
# ② 缺 unit_code → 400 + 10001 + 人话（不是 422）
########################################################################
say
say "=== ② POST /business/erp/unit 只带 unit_name → 400 + 10001 + 人话 ==="
api u_nocode POST /business/erp/unit "$T" '{"unit_name":"E2E08F2 无编码单位"}'
check "② 缺 unit_code HTTP" "$HTTP" "400"
checkne "② 不是框架 422" "$HTTP" "422"
check "② 业务码 10001" "$CODE" "10001"
check "② 文案人话" "$(data '.msg')" "计量单位编码必填（单位不自动取号）"

api u_blankcode POST /business/erp/unit "$T" '{"unit_code":"","unit_name":"E2E08F2 空编码单位"}'
check "② 空串 unit_code HTTP" "$HTTP" "400"
check "② 空串业务码 10001" "$CODE" "10001"
check "② 空串文案人话" "$(data '.msg')" "计量单位编码必填（单位不自动取号）"

api u_spacecode POST /business/erp/unit "$T" '{"unit_code":"   ","unit_name":"E2E08F2 空白编码单位"}'
check "② 纯空白 unit_code HTTP" "$HTTP" "400"
check "② 纯空白文案人话" "$(data '.msg')" "计量单位编码必填（单位不自动取号）"

########################################################################
# ③ 重复单位编码 → 409 + 点名编码
########################################################################
say
say "=== ③ 重复单位编码 → 409 + 66001 + 点名编码 ==="
U_CODE="e2e08f2-unit-dup"
api u_new POST /business/erp/unit "$T" \
  "$(jq -nc --arg c "$U_CODE" '{unit_code:$c,unit_name:"E2E08F2 重复用单位",is_base:true,status:1,precision:0}')"
check "③ 首建单位 HTTP" "$HTTP" "200"
check "③ 首建单位编码原样（小写）" "$(data '.data.unit_code')" "$U_CODE"
check "③ 首建单位库内编码" "$(SQ "select unit_code from biz_unit where unit_code='$U_CODE' and deleted_at is null")" "$U_CODE"

api u_dup POST /business/erp/unit "$T" \
  "$(jq -nc --arg c "$U_CODE" '{unit_code:$c,unit_name:"E2E08F2 重复用单位 2",is_base:true,status:1,precision:0}')"
check "③ 重复编码 HTTP" "$HTTP" "409"
check "③ 重复编码业务码 66001" "$CODE" "66001"
contains "③ 文案点名编码" "$(data '.msg')" "计量单位编码 \`$U_CODE\` 已存在"
contains "③ 文案与其余五类同口径" "$(data '.msg')" "已软删的编码可以复用"
case "$(data '.msg')" in
  *uk_biz_unit_code*) bad "③ 文案仍是裸 DB 口径（出现 uk_biz_unit_code）" ;;
  *) ok "③ 文案不含裸约束名 uk_biz_unit_code" ;;
esac
check "③ 重复未被落库（存活行数=1）" \
  "$(SQ "select count(*) from biz_unit where unit_code='$U_CODE' and deleted_at is null")" "1"

# 大小写不同 = 不同编码（与其余五类的唯一索引口径一致：未删除内唯一、区分大小写）
api u_dup_upper POST /business/erp/unit "$T" \
  "$(jq -nc --arg c "${U_CODE^^}" '{unit_code:$c,unit_name:"E2E08F2 大写同码不同单位",is_base:true,status:1,precision:0}')"
check "③ 大小写不同视为不同编码 HTTP" "$HTTP" "200"

########################################################################
# ④ GET /unit/next-code → 明确 400（不是 66006 路由错）
########################################################################
say
say "=== ④ GET /business/erp/unit/next-code → 400 + 10001 +「不提供自动取号」==="
api u_next GET /business/erp/unit/next-code "$T"
check "④ HTTP" "$HTTP" "400"
check "④ 业务码 10001（不是 66006）" "$CODE" "10001"
checkne "④ 不是 66006 路由错" "$CODE" "66006"
contains "④ 文案点明结论" "$(data '.msg')" "计量单位不提供自动取号"
contains "④ 文案给出下一步" "$(data '.msg')" "unit_code"

# 对照：其余五类的 next-code 仍然正常给号（单位是唯一的例外，不是整组路由坏了）
api sup_next GET /business/erp/supplier/next-code "$T"
check "④ 对照·供应商 next-code HTTP" "$HTTP" "200"
matches "④ 对照·供应商取号形态" "$(data '.data.code')" '^SUP-[0-9]{4,}$'
api cus_next GET /business/erp/customer/next-code "$T"
check "④ 对照·客户 next-code HTTP" "$HTTP" "200"
api mat_next GET /business/erp/material/next-code "$T"
check "④ 对照·物料 next-code HTTP" "$HTTP" "200"
api wh_next GET /business/erp/warehouse/next-code "$T"
check "④ 对照·仓库 next-code HTTP" "$HTTP" "200"
api ac_next GET /business/erp/asset-category/next-code "$T"
check "④ 对照·分类 next-code HTTP" "$HTTP" "200"
api et_next GET /business/erp/expense-type/next-code "$T"
check "④ 对照·费用类型 next-code HTTP" "$HTTP" "200"

########################################################################
# 清场：只删本卡夹具（前缀 e2e08f2-，大小写都算）
########################################################################
say
say "=== 清场（只删 e2e08f2- 前缀的夹具行）==="
SQ "delete from biz_material where material_code ilike 'e2e08f2-%'" >/dev/null
SQ "delete from biz_warehouse where warehouse_code ilike 'e2e08f2-%'" >/dev/null
SQ "delete from biz_customer where code ilike 'e2e08f2-%'" >/dev/null
SQ "delete from biz_supplier where supplier_code ilike 'e2e08f2-%'" >/dev/null
SQ "delete from biz_asset_category where category_code ilike 'e2e08f2-%'" >/dev/null
SQ "delete from biz_expense_type where type_code ilike 'e2e08f2-%'" >/dev/null
SQ "delete from biz_unit where unit_code ilike 'e2e08f2-%'" >/dev/null

LEFTOVER=$(SQ "select (select count(*) from biz_material where material_code ilike 'e2e08f2-%')
                    + (select count(*) from biz_warehouse where warehouse_code ilike 'e2e08f2-%')
                    + (select count(*) from biz_customer where code ilike 'e2e08f2-%')
                    + (select count(*) from biz_supplier where supplier_code ilike 'e2e08f2-%')
                    + (select count(*) from biz_asset_category where category_code ilike 'e2e08f2-%')
                    + (select count(*) from biz_expense_type where type_code ilike 'e2e08f2-%')
                    + (select count(*) from biz_unit where unit_code ilike 'e2e08f2-%')")
check "清场后本卡夹具残留 = 0" "$LEFTOVER" "0"

summary
[ "$FAIL" = 0 ]
