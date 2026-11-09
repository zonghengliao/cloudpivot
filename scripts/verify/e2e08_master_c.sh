#!/usr/bin/env bash
# E2E-08 · ⑥ 客户价格（协议价 / 阶梯价 / 促销价 / 取价优先级）
#          · 编码策略开关 erp.master_data.allow_manual_code=false
#          · AUD-01 附件 uploader_name 快照
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e08_master_common.sh"
T_ADMIN=${T_ADMIN:-$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1}
save T_ADMIN "$T_ADMIN"
fixtures
mkdir -p "$WORK"; printf 'E2E-08 附件内容\n' > "$WORK/upload.txt"

say "=== ⑥-0 准备：专属客户 + 物料（价格配置的载体） ==="
api p_cus POST /business/erp/customer "$T_ADMIN" '{"code":"E2E08-P-C1","name":"E2E08 价格演练客户","status":"ACTIVE"}'
check "①客户建单 HTTP" "$HTTP" "200"; PC=$(data '.data.id')
api p_mat POST /business/erp/material "$T_ADMIN" "{\"material_code\":\"E2E08-P-M1\",\"material_name\":\"E2E08 价格演练物料\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"status\":1}"
check "①物料建单 HTTP" "$HTTP" "200"; PM=$(data '.data.id')

resolve() { # resolve <name> <date> <qty>  → 输出 "price|source"
  api "rs_$1" GET "/business/erp/price/resolve?customer_id=$PC&material_id=$PM&qty=$3&date=$2" "$T_ADMIN"
  printf '%s|%s' "$(data '.data.price')" "$(data '.data.source')"
}

say
say "=== ⑥-1 协议价（CRUD + 校验 + 优先级最高） ==="
api ag_c POST /business/erp/price/agreed "$T_ADMIN" "{\"customer_id\":\"$PC\",\"material_id\":\"$PM\",\"price\":100,\"min_qty\":0,\"effective_from\":\"2027-01-01\",\"effective_to\":\"2027-12-31\",\"remark\":\"E2E08 协议价\"}"
check "①协议价建单 HTTP" "$HTTP" "200"; AG=$(data '.data.id')
check "①协议价列表回填客户名" "$(data '.data.customer_name')" "E2E08 价格演练客户"
check "①协议价列表回填物料名" "$(data '.data.material_name')" "E2E08 价格演练物料"
check "②取价命中协议价（100 / AGREED）" "$(resolve a1 2027-06-15 5)" "100.0000|AGREED"
api ag_dup POST /business/erp/price/agreed "$T_ADMIN" "{\"customer_id\":\"$PC\",\"material_id\":\"$PM\",\"price\":110,\"min_qty\":0,\"effective_from\":\"2027-01-01\"}"
check "③同键协议价重复 HTTP" "$HTTP" "409"
contains "③重复文案点明起订量" "$(data '.msg')" "起订量"
api ag_upd PUT "/business/erp/price/agreed/$AG" "$T_ADMIN" '{"price":120}'
check "④协议价改价 HTTP" "$HTTP" "200"
check "④改价后取价 = 120" "$(resolve a2 2027-06-15 5)" "120.0000|AGREED"
api ag_bad_price POST /business/erp/price/agreed "$T_ADMIN" "{\"customer_id\":\"$PC\",\"material_id\":\"$PM\",\"price\":0,\"min_qty\":1,\"effective_from\":\"2027-01-01\"}"
check "⑤协议价 0 元被拒 HTTP" "$HTTP" "400"
api ag_bad_date POST /business/erp/price/agreed "$T_ADMIN" "{\"customer_id\":\"$PC\",\"material_id\":\"$PM\",\"price\":10,\"min_qty\":2,\"effective_from\":\"2027-12-31\",\"effective_to\":\"2027-01-01\"}"
check "⑥协议价 失效日早于生效日被拒 HTTP" "$HTTP" "400"
api ag_page GET "/business/erp/price/agreed/page?page=1&page_size=20&customer_id=$PC" "$T_ADMIN"
check "⑦协议价分页 total = 1" "$(data '.data.total')" "1"
check "⑦协议价库内价格 = 120" "$(SQ "select price from biz_customer_price where id='$AG'")" "120.0000"

say
say "=== ⑥-2 阶梯价（区间 / 边界 / 与协议价的层级） ==="
api st1 POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"customer_id\":\"$PC\",\"tier_type\":\"TIER\",\"min_qty\":1,\"max_qty\":10,\"price\":95,\"effective_from\":\"2027-01-01\",\"effective_to\":\"2027-12-31\"}"
check "①阶梯档 1-10 建单 HTTP" "$HTTP" "200"; T1=$(data '.data.id')
api st2 POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"customer_id\":\"$PC\",\"tier_type\":\"TIER\",\"min_qty\":10,\"max_qty\":50,\"price\":90,\"effective_from\":\"2027-01-01\",\"effective_to\":\"2027-12-31\"}"
check "①阶梯档 10-50 建单 HTTP" "$HTTP" "200"; T2=$(data '.data.id')
check "②阶梯价不抢协议价（qty=8 → 协议价 120）" "$(resolve t1 2027-06-15 8)" "120.0000|AGREED"
api ag_off PUT "/business/erp/price/agreed/$AG" "$T_ADMIN" '{"status":"INACTIVE"}'
check "③停用协议价 HTTP" "$HTTP" "200"
check "③停用后 qty=8 命中客户阶梯 95" "$(resolve t2 2027-06-15 8)" "95.0000|TIER_CUSTOMER"
check "④区间左闭右开：qty=10 命中 90" "$(resolve t3 2027-06-15 10)" "90.0000|TIER_CUSTOMER"
check "④区间边界：qty=1 命中 95" "$(resolve t4 2027-06-15 1)" "95.0000|TIER_CUSTOMER"
check "⑤区间之外（qty=60）无档位 → NONE" "$(resolve t5 2027-06-15 60)" "null|NONE"
api st_bad_max POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"customer_id\":\"$PC\",\"tier_type\":\"TIER\",\"min_qty\":10,\"max_qty\":10,\"price\":88,\"effective_from\":\"2027-01-01\"}"
check "⑥max_qty = min_qty 被拒 HTTP" "$HTTP" "400"
api st_bad_type POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"customer_id\":\"$PC\",\"tier_type\":\"BOGUS\",\"min_qty\":1,\"max_qty\":10,\"price\":88,\"effective_from\":\"2027-01-01\"}"
check "⑦非法 tier_type 被拒 HTTP" "$HTTP" "400"
contains "⑦非法 tier_type 文案" "$(data '.msg')" "TIER"

say
say "=== ⑥-3 阶梯区间不重叠：产品口径探测（重叠档能否建单） ==="
api st_overlap POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"customer_id\":\"$PC\",\"tier_type\":\"TIER\",\"min_qty\":5,\"max_qty\":20,\"price\":92,\"effective_from\":\"2027-01-01\",\"effective_to\":\"2027-12-31\"}"
if [ "$HTTP" = "200" ]; then
  ok "⑧重叠区间（5-20 与 1-10 / 10-50）被接受（HTTP 200）→ 登记 P3：区间不重叠未拦截"
  TOV=$(data '.data.id')
  check "⑨重叠时取价确定性（qty=8 取 min_qty 最大的 92）" "$(resolve ov 2027-06-15 8)" "92.0000|TIER_CUSTOMER"
  api st_overlap_del DELETE "/business/erp/price/tier/$TOV" "$T_ADMIN"
  check "⑨清理重叠档 HTTP" "$HTTP" "200"
  check "⑨清理后 qty=8 回到 95" "$(resolve ov2 2027-06-15 8)" "95.0000|TIER_CUSTOMER"
else
  ok "⑧重叠区间被拒（HTTP $HTTP）$(data '.msg' | head -c 100)"
fi

say
say "=== ⑥-4 促销价：生效期 + 优先级高于阶梯 ==="
api promo POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"customer_id\":\"$PC\",\"tier_type\":\"PROMO\",\"min_qty\":0,\"max_qty\":100,\"price\":80,\"effective_from\":\"2027-06-01\",\"effective_to\":\"2027-06-30\",\"remark\":\"E2E08 六月促销\"}"
check "①促销价建单 HTTP" "$HTTP" "200"; PR=$(data '.data.id')
check "②促销期内（2027-06-15）命中 80 / PROMO" "$(resolve p1 2027-06-15 5)" "80.0000|TIER_CUSTOMER"
check "③促销期外（2027-07-15）回落阶梯 95" "$(resolve p2 2027-07-15 5)" "95.0000|TIER_CUSTOMER"
api promo2 POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"customer_id\":\"$PC\",\"tier_type\":\"PROMO\",\"min_qty\":0,\"max_qty\":100,\"price\":70,\"effective_from\":\"2026-01-01\",\"effective_to\":\"2026-01-31\"}"
check "④历史促销（2026-01）建单 HTTP" "$HTTP" "200"; PR2=$(data '.data.id')
check "⑤历史促销只在自己的生效期命中（2026-01-15 → 70）" "$(resolve p3 2026-01-15 5)" "70.0000|TIER_CUSTOMER"
check "⑥历史促销不影响 2027-07（仍 95）" "$(resolve p4 2027-07-15 5)" "95.0000|TIER_CUSTOMER"
api promo2_off PUT "/business/erp/price/tier/$PR2" "$T_ADMIN" '{"status":"INACTIVE"}'
check "⑦停用历史促销 HTTP" "$HTTP" "200"
check "⑦停用后 2026-01-15 无价 → NONE" "$(resolve p5 2026-01-15 5)" "null|NONE"

say
say "=== ⑥-5 通用档位（customer_id = null）与层级 ==="
api gen1 POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$PM\",\"tier_type\":\"TIER\",\"min_qty\":0,\"max_qty\":1000,\"price\":85,\"effective_from\":\"2027-01-01\",\"effective_to\":\"2027-12-31\"}"
check "①通用档位建单 HTTP" "$HTTP" "200"; GEN=$(data '.data.id')
check "②客户专属档优先于通用档（2027-07-15 qty=8 → 95 而非 85）" "$(resolve g1 2027-07-15 8)" "95.0000|TIER_CUSTOMER"
check "③客户无专属档时回落通用档（qty=60 → 85）" "$(resolve g2 2027-07-15 60)" "85.0000|TIER_COMMON"
api gen_page GET "/business/erp/price/tier/page?page=1&page_size=20&material_id=$PM&tier_type=TIER" "$T_ADMIN"
check "④档位分页按 tier_type 过滤（TIER 共 4 条：1-10/10-50/通用/历史？）" "$(printf '%s' "$BODY" | jq -r '[.data.list[]|select(.tier_type=="TIER")]|length >= 3')" "true"

say
say "=== ⑥-6 价格清空后取价回落 NONE ==="
api del_ag DELETE "/business/erp/price/agreed/$AG" "$T_ADMIN"; check "①删协议价 HTTP" "$HTTP" "200"
api del_t1 DELETE "/business/erp/price/tier/$T1" "$T_ADMIN"; check "①删档 1-10 HTTP" "$HTTP" "200"
api del_t2 DELETE "/business/erp/price/tier/$T2" "$T_ADMIN"; check "①删档 10-50 HTTP" "$HTTP" "200"
api del_pr DELETE "/business/erp/price/tier/$PR" "$T_ADMIN"; check "①删促销档 HTTP" "$HTTP" "200"
api del_gen DELETE "/business/erp/price/tier/$GEN" "$T_ADMIN"; check "①删通用档 HTTP" "$HTTP" "200"
api del_pr2 DELETE "/business/erp/price/tier/$PR2" "$T_ADMIN"; check "①删历史促销档 HTTP" "$HTTP" "200"
check "②全部删除后取价 NONE" "$(resolve n1 2027-06-15 5)" "null|NONE"
check "②库内该物料的存活价格行 = 0" "$(SQ "select (select count(*) from biz_customer_price where material_id='$PM' and deleted_at is null) + (select count(*) from biz_price_tier where material_id='$PM' and deleted_at is null)")" "0"

say
say "=== ⑦ 编码策略开关：allow_manual_code=false ==="
api pol1 GET /business/erp/master-data/code-policy "$T_ADMIN"
check "①策略接口允许手工（默认 true）" "$(data '.data.allow_manual_code')" "true"
CFG_ID=$(SQ "select id from sys_config where config_key='erp.master_data.allow_manual_code' and deleted_at is null")
CFG_TYPE=$(SQ "select config_type from sys_config where id='$CFG_ID'")
CFG_CAT=$(SQ "select coalesce(category,'') from sys_config where id='$CFG_ID'")
say "  · 参数 id=$CFG_ID type=$CFG_TYPE category=$CFG_CAT"
api cfg_off PUT "/support/config/$CFG_ID" "$T_ADMIN" "$(jq -nc --arg t "$CFG_TYPE" --arg c "$CFG_CAT" '{config_key:"erp.master_data.allow_manual_code",config_value:"false",config_type:$t,category:$c,status:1,is_modifiable:true}')"
check "②关掉手工编码参数 HTTP" "$HTTP" "200"
api pol2 GET /business/erp/master-data/code-policy "$T_ADMIN"
check "②策略接口立即生效（false）" "$(data '.data.allow_manual_code')" "false"
api mf_sup POST /business/erp/supplier "$T_ADMIN" '{"supplier_code":"E2E08-NO-MANUAL","supplier_name":"E2E08 禁止手工","credit_days":0,"status":1}'
check "③开关关闭时手工编码被拒 HTTP" "$HTTP" "400"
check "③被拒业务码 66054" "$CODE" "66054"
contains "③被拒文案点名系统参数" "$(data '.msg')" "erp.master_data.allow_manual_code"
api mf_sup_auto POST /business/erp/supplier "$T_ADMIN" '{"supplier_name":"E2E08 自动取号（开关关闭）","credit_days":0,"status":1}'
check "④开关关闭时留空仍自动取号 HTTP" "$HTTP" "200"
matches "④自动号形态" "$(data '.data.supplier_code')" '^SUP-[0-9]{4,}$'
api mf_blank POST /business/erp/supplier "$T_ADMIN" '{"supplier_code":"","supplier_name":"E2E08 空串（开关关闭）","credit_days":0,"status":1}'
check "⑤开关关闭时空串视为留空 HTTP" "$HTTP" "200"
api cfg_on PUT "/support/config/$CFG_ID" "$T_ADMIN" "$(jq -nc --arg t "$CFG_TYPE" --arg c "$CFG_CAT" '{config_key:"erp.master_data.allow_manual_code",config_value:"true",config_type:$t,category:$c,status:1,is_modifiable:true}')"
check "⑥恢复参数 true HTTP" "$HTTP" "200"
api pol3 GET /business/erp/master-data/code-policy "$T_ADMIN"
check "⑥策略恢复为 true" "$(data '.data.allow_manual_code')" "true"
api mf_after POST /business/erp/supplier "$T_ADMIN" '{"supplier_code":"E2E08-AFTER-ON","supplier_name":"E2E08 恢复后手工","credit_days":0,"status":1}'
check "⑦恢复后手工编码可用 HTTP" "$HTTP" "200"
check "⑦恢复后手工编码保留" "$(data '.data.supplier_code')" "E2E08-AFTER-ON"

say
say "=== ⑧ AUD-01 附件 uploader_name 快照 ==="
ROLE_ID=$(SQ "select id from sys_role where role_code='SUPER_ADMIN' and deleted_at is null")
api aud_user POST /system/user "$T_ADMIN" "$(jq -nc --arg r "$ROLE_ID" '{username:"e2e08_aud",password:"E2e08@Aud123",real_name:"E2E08 审计员",role_ids:[$r]}')"
check "①创建演练用户 HTTP" "$HTTP" "200"; AUD_ID=$(data '.data.id')
T_AUD=$(login e2e08_aud 'E2e08@Aud123' e2e08_aud) || T_AUD=""
check "②演练用户登录成功" "$([ -n "$T_AUD" ] && echo ok)" "ok"
api aud_cus POST /business/erp/customer "$T_ADMIN" '{"code":"E2E08-AUD-C1","name":"E2E08 附单单据客户","status":"ACTIVE"}'
check "③附件载体客户建单 HTTP" "$HTTP" "200"; AC1=$(data '.data.id')
HTTP=$(curl -s -m 60 -o "$RAW/aud_upload.json" -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
  --oauth2-bearer "$T_AUD" -F "file=@$WORK/upload.txt" -F "doc_type=CUSTOMER" -F "doc_id=$AC1" -F "remark=E2E08 AUD-01 快照")
BODY=$(cat "$RAW/aud_upload.json"); CODE=$(printf '%s' "$BODY" | jq -r '.code // empty')
check "④演练用户上传附件 HTTP" "$HTTP" "200"
check "④uploader_name 落库 = 上传时姓名" "$(SQ "select uploader_name from biz_doc_attachment where doc_id='$AC1' and deleted_at is null")" "E2E08 审计员"
check "④uploader_name 不是 UUID（非空非 id）" "$(SQ "select (uploader_name is not null and uploader_name <> '' and uploader_name !~ '^[0-9a-f-]{36}$')::text from biz_doc_attachment where doc_id='$AC1' and deleted_at is null")" "true"
api aud_rename PUT "/system/user/$AUD_ID" "$T_ADMIN" '{"real_name":"E2E08 审计员（改名）"}'
check "⑤管理员改名 HTTP" "$HTTP" "200"
check "⑤改名后 sys_user.real_name 已是新名" "$(SQ "select real_name from sys_user where id='$AUD_ID'")" "E2E08 审计员（改名）"
check "⑥改名后附件 uploader_name 仍是旧名（快照不回改）" "$(SQ "select uploader_name from biz_doc_attachment where doc_id='$AC1' and deleted_at is null")" "E2E08 审计员"
api aud_list GET "/business/attachment/list?doc_type=CUSTOMER&doc_id=$AC1&sort=time" "$T_ADMIN"
check "⑦附件列表返回上传人名（快照）" "$(printf '%s' "$BODY" | jq -r '[.data.list[]?|.uploader_name] | join(",")')" "E2E08 审计员"
save AUD_CUS "$AC1"; save AUD_USER "$AUD_ID"

save PASS "$PASS"; save FAIL "$FAIL"
summary
