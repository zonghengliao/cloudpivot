#!/usr/bin/env bash
# E2E-08 · ②③⑤⑥ 五个档案页的生命周期（建→查→改→停用/删除→引用闸）
# 覆盖：客户 / 供应商 / 仓库（含父子树）/ 物料（含一物一码开关 + 序列号批量生成）/ 计量单位（含折算）
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/e2e08_master_common.sh"
T_ADMIN=${T_ADMIN:-$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1}
save T_ADMIN "$T_ADMIN"
fixtures

cr() { # cr <name> <path> <json> → 设置全局 CR_ID / $HTTP（不用命令替换：子 shell 里 $HTTP 出不来）
  api "$1" POST "$2" "$T_ADMIN" "$3"
  CR_ID=$(data '.data.id')
}

say "=== ⑤ 计量单位：基础单位 + 折算单位 + 被引用闸 ==="
cr unit_b_u1 /business/erp/unit '{"unit_code":"E2E08-B-U1","unit_name":"E2E08 基础单位","is_base":true,"rate":1,"precision":2,"status":1,"sort_order":901}'; U1=$CR_ID
check "①基础单位建单 HTTP" "$HTTP" "200"
api unit_b_u1_d GET "/business/erp/unit/$U1" "$T_ADMIN"
check "①基础单位详情 rate=1" "$(data '.data.rate')" "1.000000"
cr unit_b_u2 /business/erp/unit "{\"unit_code\":\"E2E08-B-U2\",\"unit_name\":\"E2E08 折算单位\",\"is_base\":false,\"base_unit_id\":\"$U1\",\"rate\":12,\"precision\":3,\"status\":1,\"sort_order\":902}"; U2=$CR_ID
check "②折算单位建单 HTTP" "$HTTP" "200"
api unit_b_u2_d GET "/business/erp/unit/$U2" "$T_ADMIN"
check "②折算单位挂到基础单位" "$(data '.data.base_unit_id')" "$U1"
check "②折算率落库 = 12.000000" "$(data '.data.rate')" "12.000000"
api unit_b_u2_bad POST /business/erp/unit "$T_ADMIN" '{"unit_code":"E2E08-B-U3","unit_name":"E2E08 非法基础单位","is_base":true,"rate":5,"status":1,"sort_order":903}'
check "③基础单位 rate≠1 被拒 HTTP" "$HTTP" "400"
contains "③基础单位 rate≠1 文案" "$(data '.msg')" "rate"
check "③列表页能检索单位" "$(page_has "$T_ADMIN" /business/erp/unit/page E2E08-B-U1 unit_code)" "1"

say
say "=== ④ 仓库管理：父仓 + 子仓 + 树 + 删除闸 ==="
cr wh_b_p /business/erp/warehouse '{"warehouse_code":"E2E08-B-WH1","warehouse_name":"E2E08 一号仓","warehouse_type":"NORMAL","allow_negative":false,"status":1,"sort_order":910,"level":1}'; WH1=$CR_ID
check "①父仓建单 HTTP" "$HTTP" "200"
cr wh_b_c /business/erp/warehouse "{\"warehouse_code\":\"E2E08-B-WH2\",\"warehouse_name\":\"E2E08 二号仓（子仓）\",\"warehouse_type\":\"NORMAL\",\"allow_negative\":true,\"status\":1,\"sort_order\":911,\"parent_id\":\"$WH1\",\"level\":2}"; WH2=$CR_ID
check "②子仓建单 HTTP" "$HTTP" "200"
api wh_b_tree GET /business/erp/warehouse/tree "$T_ADMIN"
NODE=$(printf '%s' "$BODY" | jq -c --arg id "$WH2" '[.. | objects | select(.id?==$id)][0]')
check "③树里能查到子仓" "$(printf '%s' "$NODE" | jq -r '.warehouse_code')" "E2E08-B-WH2"
check "③子仓 level=2" "$(printf '%s' "$NODE" | jq -r '.level')" "2"
check "③子仓 parent_id = 父仓" "$(printf '%s' "$NODE" | jq -r '.parent_id')" "$WH1"
check "③子仓 warehouse_path 含父 id" "$(printf '%s' "$NODE" | jq -r '.warehouse_path' | grep -c "$WH1")" "1"
api wh_b_del_p1 DELETE "/business/erp/warehouse/$WH1" "$T_ADMIN"
check "④有下级时删父仓被拒 HTTP" "$HTTP" "409"
contains "④删父仓文案点名下级" "$(data '.msg')" "下级"
api wh_b_upd_c PUT "/business/erp/warehouse/$WH2" "$T_ADMIN" '{"warehouse_name":"E2E08 二号仓（改）","allow_negative":false}'
check "⑤子仓改名 HTTP" "$HTTP" "200"
check "⑤子仓改名落库" "$(SQ "select warehouse_name from biz_warehouse where id='$WH2'")" "E2E08 二号仓（改）"

say
say "=== ③ 物料管理：规格/单位/默认仓/一物一码 ==="
cr mat_b_m1 /business/erp/material "{\"material_code\":\"E2E08-B-M1\",\"material_name\":\"E2E08 演练物料\",\"spec\":\"M8*20\",\"unit_id\":\"$U1\",\"material_type\":\"RAW\",\"default_warehouse_id\":\"$WH2\",\"safety_stock\":5,\"tax_rate\":13,\"serial_required\":true,\"status\":1}"; M1=$CR_ID
check "①物料建单（带规格/单位/默认仓/一物一码）HTTP" "$HTTP" "200"
api mat_b_m1_d GET "/business/erp/material/$M1" "$T_ADMIN"
check "①物料 spec 落库" "$(data '.data.spec')" "M8*20"
check "①物料 单位 = 基础单位" "$(data '.data.unit_id')" "$U1"
check "①物料 默认仓 = 子仓" "$(data '.data.default_warehouse_id')" "$WH2"
check "①物料 serial_required = true" "$(data '.data.serial_required')" "true"
check "①物料 单位/仓库 id 回填（名称由前端 unit 字典映射）" "$(printf '%s' "$BODY" | jq -r '.data.unit_id + "|" + .data.default_warehouse_id')" "$U1|$WH2"

api unit_b_u1_del_used DELETE "/business/erp/unit/$U1" "$T_ADMIN"
check "②被物料引用的单位不能删 HTTP" "$HTTP" "409"
contains "②单位引用闸文案" "$(data '.msg')" "物料在用"

api mat_b_serial_off PUT "/business/erp/material/$M1" "$T_ADMIN" '{"serial_required":false}'
check "③关掉一物一码 HTTP" "$HTTP" "200"
check "③serial_required 落库 false" "$(SQ "select serial_required::text from biz_material where id='$M1'")" "false"
api serial_b_gen_off POST /business/erp/serial/batch-generate "$T_ADMIN" "{\"material_id\":\"$M1\",\"warehouse_id\":\"$WH2\",\"count\":3,\"unit_cost\":10}"
check "④未开启一物一码时批量生成被拒 HTTP" "$HTTP" "400"
contains "④被拒文案点名 serial_required" "$(data '.msg')" "serial_required"

api mat_b_serial_on PUT "/business/erp/material/$M1" "$T_ADMIN" '{"serial_required":true}'
check "⑤重新开启一物一码 HTTP" "$HTTP" "200"
api serial_b_gen POST /business/erp/serial/batch-generate "$T_ADMIN" "{\"material_id\":\"$M1\",\"warehouse_id\":\"$WH2\",\"count\":3,\"unit_cost\":10}"
check "⑥一物一码物料批量生成 3 个序列号 HTTP" "$HTTP" "200"
check "⑥返回生成数量 = 3" "$(data '.data.count')" "3"
check "⑥biz_material_serial 落库 3 行" "$(SQ "select count(*) from biz_material_serial where material_id='$M1' and deleted_at is null")" "3"
check "⑥序列号互不相同（distinct=3）" "$(SQ "select count(distinct serial_no) from biz_material_serial where material_id='$M1'")" "3"
api serial_b_page GET "/business/erp/serial/page?page=1&page_size=20&material_id=$M1" "$T_ADMIN"
check "⑦序列号分页 total = 3" "$(data '.data.total')" "3"
check "⑦序列号状态为 IN_STOCK" "$(printf '%s' "$BODY" | jq -r '[.data.list[].status] | unique | join(",")')" "IN_STOCK"

api mat_b_upd_safety PUT "/business/erp/material/$M1" "$T_ADMIN" '{"safety_stock":8,"tax_rate":9}'
check "⑧物料改安全库存/税率 HTTP" "$HTTP" "200"
check "⑧安全库存落库 = 8" "$(SQ "select safety_stock from biz_material where id='$M1'")" "8.0000"

say
say "=== ③b 仓库删除闸：物料默认仓引用 ==="
api wh_b_del_c_used DELETE "/business/erp/warehouse/$WH2" "$T_ADMIN"
check "②被物料设为默认仓的仓库不能删 HTTP" "$HTTP" "409"
contains "②删除闸文案点名物料/默认仓" "$(data '.msg')" "默认仓"
api mat_b_m1_after_whdel GET "/business/erp/material/$M1" "$T_ADMIN"
check "②物料默认仓仍指向原仓库（未悬空）" "$(data '.data.default_warehouse_id')" "$WH2"
check "②仓库仍是存活状态" "$(SQ "select (deleted_at is null)::text from biz_warehouse where id='$WH2'")" "true"

say
say "=== ② 供应商档案生命周期 ==="
cr sup_b_s1 /business/erp/supplier '{"supplier_code":"E2E08-B-S1","supplier_name":"E2E08 演练供应商","contact_person":"李四","credit_days":45,"credit_limit":80000,"status":1}'; S1=$CR_ID
check "①供应商建单 HTTP" "$HTTP" "200"
api sup_b_s1_d GET "/business/erp/supplier/$S1" "$T_ADMIN"
check "①供应商详情 账期 = 45" "$(data '.data.credit_days')" "45"
api sup_b_s1_upd PUT "/business/erp/supplier/$S1" "$T_ADMIN" '{"credit_days":60,"credit_limit":100000,"contact_person":"王五"}'
check "②供应商改账期/额度 HTTP" "$HTTP" "200"
check "②账期落库 = 60" "$(SQ "select credit_days from biz_supplier where id='$S1'")" "60"
api sup_b_s1_off PUT "/business/erp/supplier/$S1" "$T_ADMIN" '{"status":0}'
check "③供应商停用 HTTP" "$HTTP" "200"
check "③列表 status=0 能检索到" "$(api sup_b_pg_off GET '/business/erp/supplier/page?page=1&page_size=20&status=0&keyword=E2E08-B-S1' "$T_ADMIN"; printf '%s' "$BODY" | jq -r '.data.total')" "1"
api sup_b_s1_del DELETE "/business/erp/supplier/$S1" "$T_ADMIN"
check "④供应商删除 HTTP" "$HTTP" "200"
api sup_b_s1_gone GET "/business/erp/supplier/$S1" "$T_ADMIN"
check "④删除后详情 404" "$HTTP" "404"
check "④库内 deleted_at 已置" "$(SQ "select (deleted_at is not null)::text from biz_supplier where id='$S1'")" "true"

say
say "=== ① 客户档案生命周期（含冻结 + 软删后编码复用） ==="
cr cus_b_c1 /business/erp/customer '{"code":"E2E08-B-C1","name":"E2E08 演练客户","contact_name":"张三","credit_days":30,"credit_limit":200000,"status":"ACTIVE"}'; C1=$CR_ID
check "①客户建单 HTTP" "$HTTP" "200"
api cus_b_c1_d GET "/business/erp/customer/$C1" "$T_ADMIN"
check "①客户详情 信用额度" "$(data '.data.credit_limit')" "200000.0000"
check "①客户详情 可用额度 = 额度 - 已用（200000）" "$(printf '%s' "$BODY" | jq -r '((.data.credit_available|tonumber)==200000) and ((.data.credit_used|tonumber)==0)')" "true"
api cus_b_c1_upd PUT "/business/erp/customer/$C1" "$T_ADMIN" '{"name":"E2E08 演练客户（改名）","credit_limit":260000}'
check "②客户改名/调额 HTTP" "$HTTP" "200"
check "②改名落库" "$(SQ "select name from biz_customer where id='$C1'")" "E2E08 演练客户（改名）"
api cus_b_c1_freeze PUT "/business/erp/customer/$C1" "$T_ADMIN" '{"status":"FROZEN"}'
check "③冻结客户 HTTP" "$HTTP" "200"
check "③status=FROZEN 落在库" "$(SQ "select status from biz_customer where id='$C1'")" "FROZEN"
api cus_b_frozen_list GET '/business/erp/customer/page?page=1&page_size=20&code=E2E08-B-C1&status=FROZEN' "$T_ADMIN"
check "③客户列表按 code+status 过滤命中 1 行" "$(data '.data.total')" "1"
api cus_b_c1_freeze_bad PUT "/business/erp/customer/$C1" "$T_ADMIN" '{"status":"BOGUS"}'
check "④非法客户状态被拒 HTTP" "$HTTP" "400"
api cus_b_c1_del DELETE "/business/erp/customer/$C1" "$T_ADMIN"
check "⑤客户删除 HTTP" "$HTTP" "200"
api cus_b_c1_gone GET "/business/erp/customer/$C1" "$T_ADMIN"
check "⑤删除后详情 404" "$HTTP" "404"
api cus_b_reuse POST /business/erp/customer "$T_ADMIN" '{"code":"E2E08-B-C1","name":"E2E08 编码复用客户","status":"ACTIVE"}'
check "⑥软删后同一编码可复用 HTTP" "$HTTP" "200"
check "⑥复用行的编码" "$(data '.data.code')" "E2E08-B-C1"

say
say "=== ⑦ 仓库/物料/单位 收尾：删物料后引用闸放开 ==="
api mat_b_m1_del DELETE "/business/erp/material/$M1" "$T_ADMIN"
check "①物料删除 HTTP" "$HTTP" "200"
api mat_b_m1_gone GET "/business/erp/material/$M1" "$T_ADMIN"
check "①删除后物料详情 404" "$HTTP" "404"
api unit_b_u1_del2 DELETE "/business/erp/unit/$U1" "$T_ADMIN"
check "②物料删除后单位可删 HTTP" "$HTTP" "200"
api wh_b_del_c2 DELETE "/business/erp/warehouse/$WH2" "$T_ADMIN"
check "③子仓最终不在存活集合（删成功或前面已被删）" "$(SQ "select count(*) from biz_warehouse where id='$WH2' and deleted_at is null")" "0"
api wh_b_del_p2 DELETE "/business/erp/warehouse/$WH1" "$T_ADMIN"
check "④子仓删除后父仓可删 HTTP" "$HTTP" "200"
check "④序列号随物料删除仍在库（软删物料不级联）" "$(SQ "select count(*) from biz_material_serial where material_id='$M1'")" "3"

save PASS "$PASS"; save FAIL "$FAIL"
summary
