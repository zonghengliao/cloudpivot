#!/usr/bin/env bash
# E2E-02 销售域演练 A 段：前置 · ① 客户 · ② 客户价格 · ③ 销售订单 · ④ 发货/出库
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e02_sales_common.sh"

step0() {
  say "== step0 前置（部门 / 审批人 / 业务员 / 仓库+库位 / 单位 / 物料 / 供应商 / 可用库存）"
  T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD" admin0) || exit 1
  save T_ADMIN "$T_ADMIN"; ok "admin 登录"
  PSQLQ -c "update sys_user set must_change_password=false, is_locked=false, login_fail_count=0 where username='admin';" >/dev/null
  api s0_dept POST /system/dept "$T_ADMIN" '{"dept_code":"E2E02-DEPT","dept_name":"销售演练部","status":1,"sort_order":920}'
  if [ "$CODE" = 0 ]; then DEPT_ID=$(data '.data.id'); else DEPT_ID=$(PSQLQ -c "select id from sys_dept where dept_code='E2E02-DEPT' and deleted_at is null;"); fi
  save DEPT_ID "$DEPT_ID"; ok "部门 E2E02-DEPT $DEPT_ID"
  # 审批人：销售主管 + 财务经理 + 总经理 + 财务总监 + 仓管（覆盖 so_approve / receipt_refund / 入库 节点）
  ROLES="\"20000000-0000-4000-8000-0000000f0007\",\"20000000-0000-4000-8000-0000000f0005\",\"20000000-0000-4000-8000-0000000f0003\",\"20000000-0000-4000-8000-0000000f0006\",\"20000000-0000-4000-8000-0000000f0004\""
  api s0_appr POST /system/user "$T_ADMIN" "{\"username\":\"e2e02_appr\",\"password\":\"E2e02@Appr123\",\"real_name\":\"E2E02 销售演练审批人\",\"dept_id\":\"$DEPT_ID\",\"status\":1,\"role_ids\":[$ROLES],\"remark\":\"E2E-02 演练审批人\"}"
  if [ "$CODE" = 0 ]; then APP_USER_ID=$(data '.data.id');
  else APP_USER_ID=$(PSQLQ -c "select id from sys_user where username='e2e02_appr' and deleted_at is null;");
       api s0_appr_pwd PATCH "/system/user/$APP_USER_ID/password" "$T_ADMIN" '{"password":"E2e02@Appr123"}'; fi
  save APP_USER_ID "$APP_USER_ID"; ok "审批人 e2e02_appr $APP_USER_ID"
  T_APP=$(login e2e02_appr 'E2e02@Appr123' appr0) || exit 1; save T_APP "$T_APP"; ok "审批人登录"
  # 业务员（佣金归属人；SALES_CLERK）
  api s0_sp POST /system/user "$T_ADMIN" "{\"username\":\"e2e02_sp\",\"password\":\"E2e02@Sp12345\",\"real_name\":\"E2E02 销售员甲\",\"dept_id\":\"$DEPT_ID\",\"status\":1,\"role_ids\":[\"20000000-0000-4000-8000-0000000f0008\"],\"remark\":\"E2E-02 演练业务员\"}"
  if [ "$CODE" = 0 ]; then SP_ID=$(data '.data.id');
  else SP_ID=$(PSQLQ -c "select id from sys_user where username='e2e02_sp' and deleted_at is null;");
       api s0_sp_pwd PATCH "/system/user/$SP_ID/password" "$T_ADMIN" '{"password":"E2e02@Sp12345"}'; fi
  save SP_ID "$SP_ID"; ok "销售员 e2e02_sp $SP_ID"
  # 仓库 + 默认库位
  api s0_wh POST /business/erp/warehouse "$T_ADMIN" '{"warehouse_code":"E2E02-WH","warehouse_name":"E2E-02 销售演练仓","warehouse_type":"NORMAL","level":1,"status":1,"remark":"E2E-02 演练仓"}'
  if [ "$CODE" = 0 ]; then WH_ID=$(data '.data.id'); else WH_ID=$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E02-WH' and deleted_at is null;"); fi
  save WH_ID "$WH_ID"; ok "仓库 E2E02-WH $WH_ID"
  api s0_loc POST /business/erp/location "$T_ADMIN" "{\"location_code\":\"E2E02-L01\",\"warehouse_id\":\"$WH_ID\",\"location_name\":\"默认库位\",\"location_type\":\"STORAGE\",\"is_default\":true,\"status\":1}"
  if [ "$CODE" = 0 ]; then LOC_ID=$(data '.data.id'); else LOC_ID=$(PSQLQ -c "select id from biz_location where location_code='E2E02-L01' and warehouse_id='$WH_ID' and deleted_at is null;"); fi
  save LOC_ID "$LOC_ID"; ok "默认库位 $LOC_ID"
  # 单位 / 物料（含一个无标准价物料，用来验取价链第 ⑤ 级）
  api s0_unit POST /business/erp/unit "$T_ADMIN" '{"unit_code":"E2E02-PCS","unit_name":"个(E2E02)","is_base":true,"status":1,"precision":0}'
  if [ "$CODE" = 0 ]; then UNIT_ID=$(data '.data.id'); else UNIT_ID=$(PSQLQ -c "select id from biz_unit where unit_code='E2E02-PCS' and deleted_at is null;"); fi
  save UNIT_ID "$UNIT_ID"; ok "单位 $UNIT_ID"
  mkm() { # mkm <code> <name>
    api "s0_mat_$1" POST /business/erp/material "$T_ADMIN" "{\"material_code\":\"$1\",\"material_name\":\"$2\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"FINISHED\",\"tax_rate\":13,\"status\":1,\"serial_required\":false,\"remark\":\"E2E-02 演练物料\"}"
    if [ "$CODE" = 0 ]; then data '.data.id'; else PSQLQ -c "select id from biz_material where material_code='$1' and deleted_at is null;"; fi
  }
  MAT1=$(mkm E2E02-M1 "E2E-02 演练物料一"); save MAT1 "$MAT1"
  MAT2=$(mkm E2E02-M2 "E2E-02 演练物料二"); save MAT2 "$MAT2"
  MAT3=$(mkm E2E02-M3 "E2E-02 演练物料三(无价)"); save MAT3 "$MAT3"
  ok "物料 M1=$MAT1 M2=$MAT2 M3=$MAT3"
  api s0_sup POST /business/erp/supplier "$T_ADMIN" '{"supplier_code":"E2E02-SUP","supplier_name":"E2E-02 演练供应商","contact_person":"供货联系人","contact_phone":"01000000001","credit_days":30,"status":1,"remark":"E2E-02 演练供应商"}'
  if [ "$CODE" = 0 ]; then SUP_ID=$(data '.data.id'); else SUP_ID=$(PSQLQ -c "select id from biz_supplier where supplier_code='E2E02-SUP' and deleted_at is null;"); fi
  save SUP_ID "$SUP_ID"; ok "供应商 $SUP_ID"
  # 可用库存：无源入库 200@4（价税合计 800 < 1000 → 免审自动过账）
  api s0_in POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_ID\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-02 演练期初库存\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":200,\"price_excl\":4,\"tax_rate\":0},{\"material_id\":\"$MAT2\",\"qty\":200,\"price_excl\":4,\"tax_rate\":0}]}"
  if [ "$CODE" != 0 ]; then bad "无源入库 http=$HTTP $(show)"; else
    IN_ID=$(data '.data.header.id // .data.id // empty')
    [ -z "$IN_ID" ] && IN_ID=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-02 演练期初库存' and deleted_at is null order by created_at desc limit 1;")
    save IN_ID "$IN_ID"
    ST=$(PSQLQ -c "select status from biz_stock_in where id='$IN_ID';")
    if [ "$ST" = "POSTED" ]; then ok "无源入库 $IN_ID 免审自动过账 POSTED"
    else
      api s0_in_post POST "/business/erp/stock-in/$IN_ID/submit-post" "$T_ADMIN" '{}'
      IID=$(PSQLQ -c "select approval_instance_id from biz_stock_in where id='$IN_ID';")
      [ -n "$IID" ] && [ "$IID" != "null" ] && approve_instance "$T_APP" "$IID" s0_in_ap
      check "入库状态" "$(PSQLQ -c "select status from biz_stock_in where id='$IN_ID';")" "POSTED"
    fi
  fi
  say "  可用库存: $(PSQLQ -c "select m.material_code||'='||coalesce(sum(i.qty),0) from biz_material m left join biz_inventory i on i.material_id=m.id and i.warehouse_id='$WH_ID' where m.id in ('$MAT1','$MAT2') group by m.material_code order by 1;" | tr '\n' ' ')"
  check "库存 M1=200" "$(PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_ID';")" "200.0000"
  summary
}

step1() {
  say "== step1 ① 客户档案（建 / 改 / 停用 / 联系人 / 列表详情）"
  api s1_create POST /business/erp/customer "$T_ADMIN" '{"code":"E2E02-C1","name":"E2E-02 演练客户甲","tax_no":"91110000E2E02C1X","contact_name":"张三","contact_phone":"13800000001","address":"北京市海淀区演练路 1 号","credit_limit":100000,"credit_days":30,"status":"ACTIVE","remark":"E2E-02 演练客户"}'
  [ "$CODE" = 0 ] || bad "建客户 http=$HTTP $(show)"
  CUST1=$(data '.data.id // empty')
  [ -z "$CUST1" ] && CUST1=$(PSQLQ -c "select id from biz_customer where code='E2E02-C1' and deleted_at is null;")
  save CUST1 "$CUST1"; ok "建客户 E2E02-C1 -> $CUST1"
  check "DB 客户名/联系人/账期" "$(PSQLQ -c "select name||'/'||contact_name||'/'||credit_days from biz_customer where id='$CUST1';")" "E2E-02 演练客户甲/张三/30"
  api s1_update PUT "/business/erp/customer/$CUST1" "$T_ADMIN" '{"name":"E2E-02 演练客户甲（改名）","contact_name":"李四","contact_phone":"13900000002","credit_limit":120000}'
  [ "$CODE" = 0 ] && ok "改客户（名称/联系人/额度）" || bad "改客户 http=$HTTP $(show)"
  check "DB 改后 名称/联系人/额度" "$(PSQLQ -c "select name||'/'||contact_name||'/'||credit_limit from biz_customer where id='$CUST1';")" "E2E-02 演练客户甲（改名）/李四/120000.0000"
  api s1_detail GET "/business/erp/customer/$CUST1" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "客户详情 contact=$(data '.data.contact_name') credit_days=$(data '.data.credit_days')" || bad "客户详情 http=$HTTP $(show)"
  api s1_page GET "/business/erp/customer/page?page=1&page_size=10&code=E2E02" "$T_ADMIN"
  check "客户列表(按编码筛) total" "$(data '.data.total')" "1"
  api s1_nextcode GET "/business/erp/customer/next-code" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "编码预览=$(data '.data.code // .data | tostring' | head -c 40)" || bad "next-code http=$HTTP $(show)"
  # 停用（FROZEN）→ 冻结后不允许开新单（销售订单建单应 400/409），再恢复
  api s1_freeze PUT "/business/erp/customer/$CUST1" "$T_ADMIN" '{"status":"FROZEN"}'
  [ "$CODE" = 0 ] && ok "停用客户（FROZEN）" || bad "停用客户 http=$HTTP $(show)"
  check "DB status(停用)" "$(PSQLQ -c "select status from biz_customer where id='$CUST1';")" "FROZEN"
  api s1_order_frozen POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":1,\"price\":100,\"tax_rate\":13}]}"
  if [ "$CODE" != 0 ]; then ok "冻结客户不允许开单（http=$HTTP $(show)）"; else bad "冻结客户竟然能建销售订单"; fi
  api s1_active PUT "/business/erp/customer/$CUST1" "$T_ADMIN" '{"status":"ACTIVE"}'
  check "DB status(恢复)" "$(PSQLQ -c "select status from biz_customer where id='$CUST1';")" "ACTIVE"
  summary
}

step2() {
  say "== step2 ② 客户价格（协议价 / 阶梯价 / 促销价 / 标准价 优先级）"
  # 协议价 120（含税）
  api s2_agreed POST /business/erp/price/agreed "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"material_id\":\"$MAT1\",\"price\":120,\"min_qty\":0,\"effective_from\":\"2026-01-01\",\"effective_to\":\"2026-12-31\",\"status\":\"ACTIVE\",\"remark\":\"E2E-02 协议价 120\"}"
  if [ "$CODE" != 0 ]; then say "  · 协议价已存在（$(show)），复用"; fi
  AGREED_ID=$(data '.data.id // empty')
  [ -z "$AGREED_ID" ] && AGREED_ID=$(PSQLQ -c "select id from biz_customer_price where customer_id='$CUST1' and material_id='$MAT1' and deleted_at is null order by created_at desc limit 1;")
  save AGREED_ID "$AGREED_ID"; ok "协议价 M1 -> 120（id=$AGREED_ID）"
  # 客户专属阶梯 100 件起 110
  api s2_tierc POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$MAT1\",\"customer_id\":\"$CUST1\",\"tier_type\":\"TIER\",\"min_qty\":100,\"max_qty\":null,\"price\":110,\"effective_from\":\"2026-01-01\",\"status\":\"ACTIVE\",\"remark\":\"E2E-02 客户阶梯\"}"
  if [ "$CODE" != 0 ]; then say "  · 客户阶梯已存在（$(show)），复用"; fi; TIER_C_ID=$(data '.data.id // empty')
  [ -z "$TIER_C_ID" ] && TIER_C_ID=$(PSQLQ -c "select id from biz_price_tier where material_id='$MAT1' and customer_id='$CUST1' and tier_type='TIER' and deleted_at is null order by created_at desc limit 1;")
  save TIER_C_ID "$TIER_C_ID"; ok "客户专属阶梯 100+ -> 110"
  # 通用促销阶梯 1 件起 90
  api s2_promo POST /business/erp/price/tier "$T_ADMIN" "{\"material_id\":\"$MAT1\",\"customer_id\":null,\"tier_type\":\"PROMO\",\"min_qty\":1,\"max_qty\":null,\"price\":90,\"effective_from\":\"2026-01-01\",\"status\":\"ACTIVE\",\"remark\":\"E2E-02 通用促销价 90\"}"
  if [ "$CODE" != 0 ]; then say "  · 促销价已存在（$(show)），复用"; fi; PROMO_ID=$(data '.data.id // empty')
  [ -z "$PROMO_ID" ] && PROMO_ID=$(PSQLQ -c "select id from biz_price_tier where material_id='$MAT1' and customer_id is null and tier_type='PROMO' and deleted_at is null order by created_at desc limit 1;")
  save PROMO_ID "$PROMO_ID"; ok "通用促销 1+ -> 90"
  # 物料标准售价（当前没有写接口，用 SQL 夹具；这本身记为问题）
  PSQLQ -c "update biz_material set standard_price=100 where id='$MAT1';" >/dev/null
  PSQLQ -c "update biz_material set standard_price=80 where id='$MAT2';" >/dev/null
  say "  物料标准价夹具: M1=100 M2=80（API 无写入口，见问题清单）"
  rv() { # rv <tag> <material_id> <qty>
    api "s2_rv_$1" GET "/business/erp/price/resolve?material_id=$2&qty=$3&customer_id=$CUST1&date=2026-11-09" "$T_ADMIN"
    say "  取价[$1] qty=$3 -> price=$(data '.data.price') source=$(data '.data.source')" >&2
    data '(.data.price|tostring) + "/" + .data.source' 2>/dev/null
  }
  check "①协议价胜出(qty=10)" "$(rv a "$MAT1" 10)" "120.0000/AGREED"
  check "①协议价优先于阶梯(qty=150)" "$(rv b "$MAT1" 150)" "120.0000/AGREED"
  # 停用协议价 → 阶梯价
  api s2_agreed_off PUT "/business/erp/price/agreed/$AGREED_ID" "$T_ADMIN" '{"status":"INACTIVE"}'
  check "DB 协议价停用" "$(PSQLQ -c "select status from biz_customer_price where id='$AGREED_ID';")" "INACTIVE"
  check "②客户阶梯命中(qty=150)" "$(rv c "$MAT1" 150)" "110.0000/TIER_CUSTOMER"
  check "③促销阶梯命中(qty=10)" "$(rv d "$MAT1" 10)" "90.0000/TIER_COMMON"
  # 停用促销 → 标准价
  api s2_promo_off PUT "/business/erp/price/tier/$PROMO_ID" "$T_ADMIN" '{"status":"INACTIVE"}'
  check "④物料标准价命中(qty=10)" "$(rv e "$MAT1" 10)" "100.0000/STANDARD"
  check "④M2 标准价命中" "$(rv f "$MAT2" 1)" "80.0000/STANDARD"
  check "⑤无价物料返回 NONE" "$(rv g "$MAT3" 1)" "null/NONE"
  check "DB 标准价夹具 M1" "$(PSQLQ -c "select standard_price from biz_material where id='$MAT1';")" "100.0000"
  # 恢复协议价与促销（后续订单取价用协议价）
  api s2_agreed_on PUT "/business/erp/price/agreed/$AGREED_ID" "$T_ADMIN" '{"status":"ACTIVE"}'
  api s2_promo_on PUT "/business/erp/price/tier/$PROMO_ID" "$T_ADMIN" '{"status":"ACTIVE"}'
  check "恢复后取价回到 AGREED" "$(rv h "$MAT1" 10)" "120.0000/AGREED"
  summary
}

step3() {
  say "== step3 ③ 销售订单（建单→提交审批→换人批→查询）"
  # 10×100 + 5×80 = 1400 不含税；税 13% = 182；价税合计 1582（< 1 万 → 只走销售主管一级）
  api s3_create POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"salesperson_id\":\"$SP_ID\",\"order_date\":\"2026-11-09\",\"delivery_date\":\"2026-11-20\",\"remark\":\"E2E-02 销售订单甲\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":10,\"price\":100,\"tax_rate\":13},{\"material_id\":\"$MAT2\",\"qty\":5,\"price\":80,\"tax_rate\":13}]}"
  [ "$CODE" = 0 ] || { bad "建销售订单 http=$HTTP $(show)"; summary; return; }
  SO1=$(data '.data.id // empty')
  [ -z "$SO1" ] && SO1=$(PSQLQ -c "select id from biz_sales_order where remark='E2E-02 销售订单甲' and deleted_at is null order by created_at desc limit 1;")
  save SO1 "$SO1"
  ok "建订单 $(data '.data.so_no // "?"') id=$SO1 金额=$(data '.data.total_amount') 税=$(data '.data.total_tax') 价税合计=$(data '.data.total_with_tax')"
  check "订单金额合计" "$(PSQLQ -c "select total_amount||'/'||total_tax||'/'||total_with_tax from biz_sales_order where id='$SO1';")" "1400.0000/182.0000/1582.0000"
  check "订单行金额(逐行)" "$(PSQLQ -c "select string_agg(amount||'@'||tax_rate,',') from (select amount,tax_rate from biz_sales_order_item where so_id='$SO1' order by line_no) t;")" "1000.0000@13.0000,400.0000@13.0000"
  api s3_submit POST "/business/erp/sales-order/$SO1/submit-approval" "$T_ADMIN" '{"reason":"E2E-02 提交审批"}'
  [ "$CODE" = 0 ] && ok "提交审批 → $(PSQLQ -c "select status from biz_sales_order where id='$SO1';")" || bad "提交审批 http=$HTTP $(show)"
  IID=$(PSQLQ -c "select approval_instance_id from biz_sales_order where id='$SO1';")
  say "  审批实例 = $IID"
  approve_instance "$T_APP" "$IID" s3_ap
  check "订单审批后状态" "$(PSQLQ -c "select status from biz_sales_order where id='$SO1';")" "APPROVED"
  check "审批实例状态" "$(flow_state "$IID")" "APPROVED"
  api s3_detail GET "/business/erp/sales-order/$SO1" "$T_ADMIN"
  ok "订单详情 客户=$(data '.data.customer_name') 销售员=$(data '.data.salesperson_name') 状态=$(data '.data.status')"
  api s3_page GET "/business/erp/sales-order/page?page=1&page_size=10&customer_id=$CUST1" "$T_ADMIN"
  check "订单列表(按客户筛) total" "$(data '.data.total')" "1"
  summary
}

step4() {
  say "== step4 ④ 发货单（从订单生成 → PENDING_POST → POSTED）+ ⑤ 出库单（扣库存→过账）"
  SHIP_PRE=$(PSQLQ -c "select coalesce((select status from biz_shipment where remark='E2E-02 发货单甲' order by created_at desc limit 1),'-');")
  if [ "$SHIP_PRE" = "DRAFT" ] || [ "$SHIP_PRE" = "PENDING_POST" ] || [ "$SHIP_PRE" = "POSTED" ]; then
    SHIP1=$(PSQLQ -c "select id from biz_shipment where remark='E2E-02 发货单甲' order by created_at desc limit 1;")
    save SHIP1 "$SHIP1"; say "  复用已存在发货单 $SHIP1（状态 $SHIP_PRE）"
  else
    api s4_ship POST /business/erp/shipment "$T_ADMIN" "{\"so_id\":\"$SO1\",\"warehouse_id\":\"$WH_ID\",\"ship_date\":\"2026-11-09\",\"remark\":\"E2E-02 发货单甲\",\"items\":[]}"
    [ "$CODE" = 0 ] || { bad "建发货单 http=$HTTP $(show)"; summary; return; }
    SHIP1=$(data '.data.id // empty')
    [ -z "$SHIP1" ] && SHIP1=$(PSQLQ -c "select id from biz_shipment where remark='E2E-02 发货单甲' order by created_at desc limit 1;")
    save SHIP1 "$SHIP1"
    ok "建发货单 $(data '.data.ship_no // "?"') id=$SHIP1 状态=$(data '.data.status') 行=$(data '.data.items | length' 2>/dev/null)"
  fi
  [ "$SHIP_PRE" = "DRAFT" ] && check "发货单状态(DRAFT)" "$(PSQLQ -c "select status from biz_shipment where id='$SHIP1';")" "DRAFT"
  check "发货明细 2 行" "$(PSQLQ -c "select count(*) from biz_shipment_item where shipment_id='$SHIP1';")" "2"
  [ "$SHIP_PRE" = "POSTED" ] || [ "$SHIP_PRE" = "PENDING_POST" ] || { api s4_pending PATCH "/business/erp/shipment/$SHIP1/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
  [ "$CODE" = 0 ] && ok "→ PENDING_POST" || bad "→PENDING_POST http=$HTTP $(show)"; }
  [ "$SHIP_PRE" = "POSTED" ] || check "未过账时库存不变" "$(PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_ID';")" "200.0000"
  [ "$SHIP_PRE" = "POSTED" ] || { api s4_posted PATCH "/business/erp/shipment/$SHIP1/status" "$T_ADMIN" '{"target_status":"POSTED"}'
  [ "$CODE" = 0 ] && ok "→ POSTED（扣库存 + 成本凭证 + 自动挂应收）" || bad "→POSTED http=$HTTP $(show)"; }
  check "发货单 POSTED" "$(PSQLQ -c "select status from biz_shipment where id='$SHIP1';")" "POSTED"
  check "订单状态 OUTBOUND（两行全发完）" "$(PSQLQ -c "select status from biz_sales_order where id='$SO1';")" "OUTBOUND"
  check "订单行已发数量" "$(PSQLQ -c "select string_agg(delivered_qty::numeric(20,4)::text,',') from (select delivered_qty from biz_sales_order_item where so_id='$SO1' order by line_no) t;")" "10.0000,5.0000"
  check "库存 M1 200→190" "$(PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_ID';")" "190.0000"
  check "库存 M2 200→195" "$(PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory where material_id='$MAT2' and warehouse_id='$WH_ID';")" "195.0000"
  api s4_page GET "/business/erp/stock-out/page?page=1&page_size=10" "$T_ADMIN"
  SOUT1=$(PSQLQ -c "select id from biz_stock_out where source_type='SALES' and source_id='$SHIP1' and deleted_at is null;")
  save SOUT1 "$SOUT1"
  ok "出库单 $SOUT1 out_no=$(PSQLQ -c "select out_no from biz_stock_out where id='$SOUT1';") 状态=$(PSQLQ -c "select status from biz_stock_out where id='$SOUT1';")"
  api s4_sout GET "/business/erp/stock-out/$SOUT1" "$T_ADMIN"
  check "出库单读取 source_type" "$(data '.data.source_type')" "SALES"
  check "出库单成本额(4×(10+5))" "$(PSQLQ -c "select total_amount from biz_stock_out where id='$SOUT1';")" "60.0000"
  say "  出库流水: $(PSQLQ -c "select txn_type||'/'||biz_type||'/'||qty from biz_inventory_txn where doc_id='$SOUT1' order by material_id;" | tr '\n' ' ')"
  say "  成本凭证: $(PSQLQ -c "select voucher_no||' '||summary from biz_voucher where source_id='$SOUT1';")"
  summary
}

case "${1:-}" in
  step0) step0 ;; step1) step1 ;; step2) step2 ;; step3) step3 ;; step4) step4 ;;
  *) echo "用法: $0 {step0|step1|step2|step3|step4}"; exit 2 ;;
esac
