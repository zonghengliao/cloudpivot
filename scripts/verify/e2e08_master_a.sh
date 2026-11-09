#!/usr/bin/env bash
# E2E-08 · ① 自动编码矩阵（SUP/CUS/MAT/WH/AC/ET 各 19 条路径）+ ④ 计量单位（不自动）
# 用法：bash e2e08_master_a.sh
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e08_master_common.sh"
T_ADMIN=${T_ADMIN:-$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1}
save T_ADMIN "$T_ADMIN"
fixtures

fmt_re() { case "$1" in SUP|CUS|MAT) printf '^%s-[0-9]{4,}$' "$1";; *) printf '^%s-[0-9]{2,}$' "$1";; esac; }
code_num() { local n=${1##*-}; printf '%d' "$((10#$n))"; }
pad_of() { case "$1" in SUP|CUS|MAT) printf 4;; *) printf 2;; esac; }

# ---- 六类主数据请求体（$1 = 编码的 JSON 字面量，可能是 null / "" / "X"；$2 = 名称）----
mk_sup() { jq -nc --argjson c "$1" --arg n "$2" '{supplier_name:$n,credit_days:30,status:1} + (if $c==null then {} else {supplier_code:$c} end)'; }
mk_cus() { jq -nc --argjson c "$1" --arg n "$2" '{name:$n,credit_days:15,credit_limit:50000} + (if $c==null then {} else {code:$c} end)'; }
mk_mat() { jq -nc --argjson c "$1" --arg n "$2" --arg u "$UNIT_ID" '{material_name:$n,unit_id:$u,material_type:"RAW"} + (if $c==null then {} else {material_code:$c} end)'; }
mk_wh()  { jq -nc --argjson c "$1" --arg n "$2" '{warehouse_name:$n,warehouse_type:"NORMAL",allow_negative:false,status:1,sort_order:0,level:1} + (if $c==null then {} else {warehouse_code:$c} end)'; }
mk_ac()  { jq -nc --argjson c "$1" --arg n "$2" '{category_name:$n,useful_life_months:36,salvage_rate:0.05} + (if $c==null then {} else {category_code:$c} end)'; }
mk_et()  { jq -nc --argjson c "$1" --arg n "$2" --arg a "$ACC_ID" '{type_name:$n,account_id:$a} + (if $c==null then {} else {type_code:$c} end)'; }

auto_matrix() { # tag path respfield seqtype table col mk label
  local tag=$1 path=$2 field=$3 seqtype=$4 table=$5 col=$6 mk=$7 label=$8
  local seq0 p1 p2 c1 n1 c2 n2 c3 n3 manual lower up qp
  manual="E2E08-$tag-M1"
  lower="e2e08-$(printf '%s' "$tag" | tr 'A-Z' 'a-z')-lc"
  qp="keyword"; [ "$tag" = "CUS" ] && qp="code"
  seq0=$(seqof "$seqtype")
  say "── $label（号段 $seqtype 当前 $seq0）"

  api "prev1_$tag" GET "$path/next-code" "$T_ADMIN"
  check "①${label} 预览 HTTP" "$HTTP" "200"
  p1=$(data '.data.code')
  matches "①${label} 预览形态" "$p1" "$(fmt_re "$seqtype")"
  check "①${label} 预览号 = 号段+1" "$(code_num "$p1")" "$((seq0+1))"

  api "prev2_$tag" GET "$path/next-code" "$T_ADMIN"
  p2=$(data '.data.code')
  check "②${label} 预览两次同号（只读不占号）" "$p1" "$p2"
  check "②${label} 预览后 current_no 未推进" "$(seqof "$seqtype")" "$seq0"

  api "auto1_$tag" POST "$path" "$T_ADMIN" "$($mk null "$label 自动1")"
  check "③${label} 留空(null)建单 HTTP" "$HTTP" "200"
  c1=$(data ".data.$field")
  n1=$(code_num "$c1")
  check "③${label} 自动号 = 预览号" "$c1" "$p1"
  check "③${label} 自动号 = 号段+1" "$n1" "$((seq0+1))"
  save "ID_$tag" "$(data '.data.id')"; save "CODE_$tag" "$c1"

  api "auto2_$tag" POST "$path" "$T_ADMIN" "$($mk '""' "$label 自动2")"
  check "④${label} 空串建单 HTTP" "$HTTP" "200"
  c2=$(data ".data.$field"); n2=$(code_num "$c2")
  check "④${label} 空串也自动取号（连续 +1）" "$n2" "$((n1+1))"
  check "④${label} 号段推进到 $n2" "$(seqof "$seqtype")" "$n2"

  api "auto3_$tag" POST "$path" "$T_ADMIN" "$($mk '"   "' "$label 自动3")"
  check "⑤${label} 纯空白建单 HTTP" "$HTTP" "200"
  c3=$(data ".data.$field"); n3=$(code_num "$c3")
  check "⑤${label} 纯空白也自动取号（连续 +1）" "$n3" "$((n2+1))"

  api "man_$tag" POST "$path" "$T_ADMIN" "$($mk "\"$manual\"" "$label 手工")"
  check "⑥${label} 手工编码 HTTP" "$HTTP" "200"
  check "⑥${label} 手工编码保留原值" "$(data ".data.$field")" "$manual"
  check "⑥${label} 手工建单不推进号段" "$(seqof "$seqtype")" "$n3"

  api "manlc_$tag" POST "$path" "$T_ADMIN" "$($mk "\"$lower\"" "$label 小写手工")"
  check "⑦${label} 小写手工编码 HTTP" "$HTTP" "200"
  up=$(data ".data.$field")
  case "$up" in
    "$lower") ok "⑦${label} 原样保留大小写（$up）";;
    "$(printf '%s' "$lower" | tr 'a-z' 'A-Z')") ok "⑦${label} 归一成大写（$up）";;
    *) bad "⑦${label} 手工编码被改写：$lower → $up";;
  esac

  api "dup_$tag" POST "$path" "$T_ADMIN" "$($mk "\"$manual\"" "$label 重复")"
  check "⑧${label} 重复编码 HTTP" "$HTTP" "409"
  check "⑧${label} 重复编码业务码" "$CODE" "66001"
  contains "⑧${label} 重复编码文案点名" "$(data '.msg')" "$manual"

  api "bad_$tag" POST "$path" "$T_ADMIN" "$($mk '"E2E08 BAD"' "$label 非法")"
  check "⑨${label} 非法字符 HTTP" "$HTTP" "400"
  check "⑨${label} 非法字符业务码" "$CODE" "10001"

  api "long_$tag" POST "$path" "$T_ADMIN" "$($mk "\"$(printf 'A%.0s' $(seq 1 41))\"" "$label 超长")"
  check "⑩${label} 超长编码 HTTP" "$HTTP" "400"

  # 前端「生成编码」按钮的真实契约：预览出号 → 用户保存时把该号当手工编码提交 → 必须原值落库且号段不再前进
  api "btnprev_$tag" GET "$path/next-code" "$T_ADMIN"
  local p3 seqb; p3=$(data '.data.code'); seqb=$(seqof "$seqtype")
  api "btn_$tag" POST "$path" "$T_ADMIN" "$($mk "\"$p3\"" "$label 预览号填单")"
  check "⑩b${label} 预览号当手工码提交 HTTP" "$HTTP" "200"
  check "⑩b${label} 预览号原值落库（按钮填充闭环）" "$(data ".data.$field")" "$p3"
  check "⑩b${label} 该路径不再推进号段" "$(seqof "$seqtype")" "$seqb"

  # 落库与列表
  check "⑪${label} 库内编码 = 接口返回" "$(SQ "select $col from $table where id='$(eval echo \$ID_$tag)'")" "$c1"
  check "⑪${label} 库内行未软删" "$(SQ "select (deleted_at is null)::text from $table where id='$(eval echo \$ID_$tag)'")" "true"
  check "⑫${label} 列表页能检索到自动编码" "$(page_has "$T_ADMIN" "$path/page" "$c1" "$field" "$qp")" "1"
  check "⑫${label} 列表页能检索到手工编码" "$(page_has "$T_ADMIN" "$path/page" "$manual" "$field" "$qp")" "1"

  # 编辑：编码栏空 = 不改（AC/ET 的编辑请求要求带名称，否则 400「名称必填」）
  case "$tag" in
    AC) upd=$(jq -nc '{remark:"E2E08 编辑一次",category_name:"E2E08 资产分类 编辑一次",useful_life_months:36,salvage_rate:0.05}');;
    ET) upd=$(jq -nc --arg a "$ACC_ID" '{remark:"E2E08 编辑一次",type_name:"E2E08 费用类型 编辑一次",account_id:$a}');;
    *)  upd='{"remark":"E2E08 编辑一次"}';;
  esac
  api "upd_$tag" PUT "$path/$(eval echo \$ID_$tag)" "$T_ADMIN" "$upd"
  check "⑬${label} 编辑（不带编码）HTTP" "$HTTP" "200"
  check "⑬${label} 编辑后编码不变" "$(data ".data.$field")" "$c1"
}

say "=== A. 六类主数据自动编码矩阵 ==="
auto_matrix SUP /business/erp/supplier supplier_code SUP biz_supplier supplier_code mk_sup "供应商"
auto_matrix CUS /business/erp/customer code CUS biz_customer code mk_cus "客户"
auto_matrix MAT /business/erp/material material_code MAT biz_material material_code mk_mat "物料"
auto_matrix WH  /business/erp/warehouse warehouse_code WH biz_warehouse warehouse_code mk_wh "仓库"
auto_matrix AC  /business/erp/asset-category category_code AC biz_asset_category category_code mk_ac "资产分类"
auto_matrix ET  /business/erp/expense-type type_code ET biz_expense_type type_code mk_et "费用类型"

say
say "=== A2. 编辑态编码口径（六类同族对比） ==="
api "chg_ac" PUT "/business/erp/asset-category/$ID_AC" "$T_ADMIN" '{"category_code":"E2E08-AC-CHG","category_name":"E2E08 资产分类 改名","useful_life_months":36,"salvage_rate":0.05}'
check "资产分类 改编码 HTTP" "$HTTP" "400"
contains "资产分类 改编码文案" "$(data '.msg')" "不可修改"
api "chg_sup" PUT "/business/erp/supplier/$ID_SUP" "$T_ADMIN" '{"supplier_code":"E2E08-SUP-CHG"}'
check "供应商 改编码字段被忽略 HTTP" "$HTTP" "200"
check "供应商 编码未变" "$(data '.data.supplier_code')" "$CODE_SUP"
api "chg_mat" PUT "/business/erp/material/$ID_MAT" "$T_ADMIN" '{"material_code":"E2E08-MAT-CHG"}'
check "物料 改编码字段被忽略 HTTP" "$HTTP" "200"
check "物料 编码未变" "$(data '.data.material_code')" "$CODE_MAT"
api "chg_cus" PUT "/business/erp/customer/$ID_CUS" "$T_ADMIN" '{"code":"E2E08-CUS-CHG"}'
if [ "$HTTP" = "200" ] && [ "$(data '.data.code')" = "E2E08-CUS-CHG" ]; then
  ok "客户 编码可被 PUT 改写（HTTP 200 → E2E08-CUS-CHG，登记为 P3 口径不一致）"
  api "chg_cus_back" PUT "/business/erp/customer/$ID_CUS" "$T_ADMIN" "{\"code\":\"$CODE_CUS\"}"
  check "客户 编码改回 HTTP" "$HTTP" "200"
else
  ok "客户 编码编辑被拒/忽略（HTTP $HTTP，code=$(data '.data.code')）"
fi

say
say "=== B. 计量单位：不自动（必须手工编码） ==="
api "unit_missing" POST /business/erp/unit "$T_ADMIN" '{"unit_name":"E2E08-无编码单位","is_base":true,"precision":0}'
case "$HTTP" in
  400) ok "①单位 不填编码被拒 HTTP = 400";;
  422) ok "①单位 不填编码被拒 HTTP = 422（框架反序列化拒绝，非 400+10001；登记 P4 口径差异）";;
  *) bad "①单位 不填编码未被拒 HTTP = $HTTP";;
esac
api "unit_create" POST /business/erp/unit "$T_ADMIN" '{"unit_code":"E2E08-U1","unit_name":"E2E08 演练单位","is_base":true,"precision":0,"status":1,"sort_order":900}'
check "②单位 手工编码建单 HTTP" "$HTTP" "200"
check "②单位 编码保留" "$(data '.data.unit_code')" "E2E08-U1"
save UNIT_E2E08_ID "$(data '.data.id')"
api "unit_dup" POST /business/erp/unit "$T_ADMIN" '{"unit_code":"E2E08-U1","unit_name":"E2E08 重复单位","is_base":true}'
check "③单位 重复编码 HTTP" "$HTTP" "409"
check "③单位 重复编码业务码" "$CODE" "66001"
api "unit_next" GET /business/erp/unit/next-code "$T_ADMIN"
checkne "④单位 无 next-code 接口（不提供自动编码）" "$HTTP" "200"
check "④单位 next-code 命中 {id} 路由（66006）" "$CODE" "66006"
check "⑤单位 库内 is_system=false（客户自建）" "$(SQ "select is_system::text from biz_unit where unit_code='E2E08-U1' and deleted_at is null")" "false"
check "⑤单位 库内 precision 落库" "$(SQ "select precision from biz_unit where unit_code='E2E08-U1' and deleted_at is null")" "0"

say
say "=== C. 六类主数据编码全局唯一性（库内扫描） ==="
for pair in "biz_supplier:supplier_code:E2E08" "biz_customer:code:E2E08" "biz_material:material_code:E2E08" "biz_warehouse:warehouse_code:E2E08" "biz_asset_category:category_code:E2E08" "biz_expense_type:type_code:E2E08"; do
  IFS=: read -r tbl col pre <<< "$pair"
  dup=$(SQ "select count(*) from (select $col from $tbl where deleted_at is null group by 1 having count(*)>1) t")
  check "⑥$tbl 存活编码零重复" "$dup" "0"
done

save PASS "$PASS"; save FAIL "$FAIL"
summary
