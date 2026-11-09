#!/usr/bin/env bash
# E2E-03 库存域演练 A 段：前置 · ① 仓库管理 · ② 库位管理 · ③ 序列号管理 · ④ 期初库存
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e03_inventory_common.sh"

step0() {
  say "== step0 前置（部门 / 仓管审批人 / 单位 / 供应商 / 物料）"
  T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD" a0) || exit 1
  save T_ADMIN "$T_ADMIN"; ok "admin 登录"
  PSQLQ -c "update sys_user set must_change_password=false, is_locked=false, login_fail_count=0 where username='admin';" >/dev/null
  api a0_dept POST /system/dept "$T_ADMIN" '{"dept_code":"E2E03-DEPT","dept_name":"库存演练部","status":1,"sort_order":930}'
  if [ "$CODE" = 0 ]; then DEPT_ID=$(data '.data.id'); else DEPT_ID=$(PSQLQ -c "select id from sys_dept where dept_code='E2E03-DEPT' and deleted_at is null;"); fi
  save DEPT_ID "$DEPT_ID"; ok "部门 E2E03-DEPT $DEPT_ID"
  # 审批人：WAREHOUSE_MANAGER（stock_transfer v2 / stock_take_post 的 n_* 节点都指这个角色）
  api a0_wh_user POST /system/user "$T_ADMIN" "{\"username\":\"e2e03_wh\",\"password\":\"E2e03@Wh123456\",\"real_name\":\"E2E03 仓管审批人\",\"dept_id\":\"$DEPT_ID\",\"status\":1,\"role_ids\":[\"20000000-0000-4000-8000-0000000f0004\"],\"remark\":\"E2E-03 库存演练审批人\"}"
  if [ "$CODE" = 0 ]; then APP_USER_ID=$(data '.data.id');
  else APP_USER_ID=$(PSQLQ -c "select id from sys_user where username='e2e03_wh' and deleted_at is null;");
       api a0_wh_pwd PATCH "/system/user/$APP_USER_ID/password" "$T_ADMIN" '{"password":"E2e03@Wh123456"}'; fi
  save APP_USER_ID "$APP_USER_ID"
  PSQLQ -c "update sys_user set must_change_password=false, is_locked=false, login_fail_count=0 where username='e2e03_wh';" >/dev/null
  T_APP=$(login e2e03_wh 'E2e03@Wh123456' a0w) || exit 1; save T_APP "$T_APP"; ok "仓管审批人 e2e03_wh 登录（$APP_USER_ID）"
  api a0_unit POST /business/erp/unit "$T_ADMIN" '{"unit_code":"E2E03-PCS","unit_name":"个(E2E03)","is_base":true,"status":1,"precision":0}'
  if [ "$CODE" = 0 ]; then UNIT_ID=$(data '.data.id'); else UNIT_ID=$(PSQLQ -c "select id from biz_unit where unit_code='E2E03-PCS' and deleted_at is null;"); fi
  save UNIT_ID "$UNIT_ID"; ok "单位 $UNIT_ID"
  api a0_sup POST /business/erp/supplier "$T_ADMIN" '{"supplier_code":"E2E03-SUP","supplier_name":"E2E-03 演练供应商","contact_person":"供货联系人","contact_phone":"01000000003","credit_days":30,"status":1,"remark":"E2E-03 演示供应商"}'
  if [ "$CODE" = 0 ]; then SUP_ID=$(data '.data.id'); else SUP_ID=$(PSQLQ -c "select id from biz_supplier where supplier_code='E2E03-SUP' and deleted_at is null;"); fi
  save SUP_ID "$SUP_ID"; ok "供应商 $SUP_ID"
  mkm() { # mkm <code> <name> <serial_required>
    api "a0_mat_$1" POST /business/erp/material "$T_ADMIN" "{\"material_code\":\"$1\",\"material_name\":\"$2\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"tax_rate\":13,\"status\":1,\"serial_required\":$3,\"remark\":\"E2E-03 演练物料\"}"
    if [ "$CODE" = 0 ]; then data '.data.id'; else PSQLQ -c "select id from biz_material where material_code='$1' and deleted_at is null;"; fi
  }
  MAT1=$(mkm E2E03-M1 "E2E-03 演练物料一(数量)" false); save MAT1 "$MAT1"
  MAT2=$(mkm E2E03-M2 "E2E-03 演练物料二(期初)" false); save MAT2 "$MAT2"
  MATS=$(mkm E2E03-MS "E2E-03 演练物料三(一物一码)" true); save MATS "$MATS"
  ok "物料 M1=$MAT1 M2=$MAT2 MS=$MATS"
  check "MS serial_required" "$(PSQLQ -c "select serial_required::text from biz_material where id='$MATS';")" "true"
  # 对账基线（全库既有数据里有历史差异，本卡只保证「不增加」并把自有物料打成 0）
  BASE_DIFF_ROWS=$(PSQLQ -c "WITH inv AS (SELECT material_id,warehouse_id,SUM(qty) q FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id,warehouse_id,SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) q FROM biz_inventory_txn GROUP BY 1,2) SELECT count(*) FROM inv FULL JOIN txn USING (material_id,warehouse_id) WHERE COALESCE(inv.q,0)<>COALESCE(txn.q,0);")
  save BASE_DIFF_ROWS "$BASE_DIFF_ROWS"; ok "全库既有差异行基线 = $BASE_DIFF_ROWS 行"
  BASE_BUCKET_ROWS=$(PSQLQ -c "WITH cost AS (SELECT i.material_id,i.warehouse_id,COALESCE(il.location_id::text,'(未分配)') b,SUM(i.qty) q FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id GROUP BY 1,2,3), loc AS (SELECT material_id,warehouse_id,COALESCE(location_id::text,'(未分配)') b,SUM(qty) q FROM biz_inventory_location GROUP BY 1,2,3) SELECT count(*) FROM cost FULL JOIN loc ON cost.material_id=loc.material_id AND cost.warehouse_id=loc.warehouse_id AND cost.b=loc.b WHERE COALESCE(cost.q,0)<>COALESCE(loc.q,0);")
  save BASE_BUCKET_ROWS "$BASE_BUCKET_ROWS"; ok "全库既有逐桶差异行基线 = $BASE_BUCKET_ROWS 行"
  summary
}

step1() {
  say "== step1 ① 仓库管理（建总仓 → 建 2 个子仓 → 树/分页/详情/改 / 非叶子不可挂库位）"
  api a1_whp POST /business/erp/warehouse "$T_ADMIN" '{"warehouse_code":"E2E03-WH-P","warehouse_name":"E2E-03 演练总仓","warehouse_type":"NORMAL","level":1,"status":1,"sort_order":930,"remark":"E2E-03 演练总仓"}'
  P_NEW=0
  if [ "$CODE" = 0 ]; then P_NEW=1; elif [ "$CODE" = 66001 ]; then ok "总仓已存在（重跑幂等，跳过建仓断言）"; else bad "建总仓 http=$HTTP $(show)"; fi
  WH_P=$(data '.data.id // empty')
  [ -z "$WH_P" ] && WH_P=$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E03-WH-P' and deleted_at is null;")
  save WH_P "$WH_P"; ok "总仓 E2E03-WH-P -> $WH_P"
  if [ "$P_NEW" = 1 ]; then
    check "总仓 DB 层级/叶子/路径" "$(PSQLQ -c "select level||'|'||is_leaf||'|'||warehouse_path from biz_warehouse where id='$WH_P';")" "1|1|/$WH_P/"
  else
    ok "总仓已存在：层级/路径 = $(PSQLQ -c "select level||'|'||is_leaf||'|'||warehouse_path from biz_warehouse where id='$WH_P';")"
  fi

  api a1_wha POST /business/erp/warehouse "$T_ADMIN" "{\"warehouse_code\":\"E2E03-WH-A\",\"warehouse_name\":\"E2E-03 演练主仓\",\"warehouse_type\":\"RAW\",\"parent_id\":\"$WH_P\",\"level\":2,\"status\":1,\"sort_order\":931,\"allow_negative\":false,\"remark\":\"E2E-03 演练主仓\"}"
  if [ "$CODE" != 0 ] && [ "$CODE" != 66001 ]; then bad "建主仓 http=$HTTP $(show)"; fi
  WH_A=$(data '.data.id // empty')
  [ -z "$WH_A" ] && WH_A=$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E03-WH-A' and deleted_at is null;")
  save WH_A "$WH_A"; ok "主仓 E2E03-WH-A -> $WH_A"
  api a1_whb POST /business/erp/warehouse "$T_ADMIN" "{\"warehouse_code\":\"E2E03-WH-B\",\"warehouse_name\":\"E2E-03 演练调入仓\",\"warehouse_type\":\"RAW\",\"parent_id\":\"$WH_P\",\"level\":2,\"status\":1,\"sort_order\":932,\"remark\":\"E2E-03 演练调入仓\"}"
  if [ "$CODE" = 0 ]; then WH_B=$(data '.data.id'); else WH_B=$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E03-WH-B' and deleted_at is null;"); fi
  save WH_B "$WH_B"; ok "调入仓 E2E03-WH-B -> $WH_B"
  check "子仓 DB 层级/路径前缀" "$(PSQLQ -c "select level||'|'||warehouse_path from biz_warehouse where id='$WH_A';")" "2|/$WH_P/$WH_A/"
  check "总仓建子仓后 is_leaf" "$(PSQLQ -c "select is_leaf::text from biz_warehouse where id='$WH_P';")" "0"

  api a1_tree GET "/business/erp/warehouse/tree" "$T_ADMIN"
  check "仓库树 HTTP" "$HTTP" "200"
  check "仓库树 总仓子节点数" "$(data "[.data[]? | select(.id==\"$WH_P\") | .children|length] | first // 0" 2>/dev/null || echo 0)" "2"
  api a1_page GET "/business/erp/warehouse/page?page=1&page_size=20&keyword=E2E03-WH" "$T_ADMIN"
  check "仓库分页 HTTP/code" "$HTTP/$CODE" "200/0"
  ok "仓库分页命中 $(data '[.data.list[]? | select(.warehouse_code|startswith("E2E03-WH"))] | length') 条（≥3）"
  api a1_detail GET "/business/erp/warehouse/$WH_A" "$T_ADMIN"
  check "仓库详情与库一致（名称/上级）" "$(data '.data.warehouse_name + "|" + .data.parent_id')" "$(PSQLQ -c "select warehouse_name||'|'||parent_id from biz_warehouse where id='$WH_A';")"
  api a1_update PUT "/business/erp/warehouse/$WH_A" "$T_ADMIN" '{"warehouse_name":"E2E-03 演练主仓(改名)","manager":"仓管甲","address":"北京市朝阳区演练路 3 号","allow_negative":false}'
  [ "$CODE" = 0 ] && ok "改仓库（名称/负责人/地址）" || bad "改仓库 http=$HTTP $(show)"
  check "改后 DB 名称/负责人" "$(PSQLQ -c "select warehouse_name||'/'||manager from biz_warehouse where id='$WH_A';")" "E2E-03 演练主仓(改名)/仓管甲"
  api a1_nextcode GET /business/erp/warehouse/next-code "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "仓库编码预览 = $(data '.data.code // .data.warehouse_code // .data')" || bad "仓库编码预览 http=$HTTP $(show)"

  # 负向：非叶子仓库不能挂库位（400 + 66005）
  api a1_loc_on_parent POST /business/erp/location "$T_ADMIN" "{\"location_code\":\"E2E03-L-P1\",\"warehouse_id\":\"$WH_P\",\"location_name\":\"总仓库位\",\"location_type\":\"STORAGE\",\"is_default\":false,\"status\":1}"
  check "非叶子仓库挂库位 http/code" "$HTTP/$CODE" "400/66005"
  check "非叶子仓库挂库位 DB 未落库" "$(PSQLQ -c "select count(*) from biz_location where location_code='E2E03-L-P1';")" "0"

  mk_loc() { # mk_loc <code> <wh> <name> <default> <tag>
    api "a1_loc_$5" POST /business/erp/location "$T_ADMIN" "{\"location_code\":\"$1\",\"warehouse_id\":\"$2\",\"location_name\":\"$3\",\"location_type\":\"STORAGE\",\"is_default\":$4,\"status\":1}"
    if [ "$CODE" = 0 ]; then data '.data.id'; else PSQLQ -c "select id from biz_location where location_code='$1' and warehouse_id='$2' and deleted_at is null;"; fi
  }
  LOC_A1=$(mk_loc E2E03-L01A "$WH_A" "A 仓默认库位" true a); save LOC_A1 "$LOC_A1"
  LOC_A2=$(mk_loc E2E03-L02A "$WH_A" "A 仓二号库位" false b); save LOC_A2 "$LOC_A2"
  LOC_B1=$(mk_loc E2E03-L01B "$WH_B" "B 仓默认库位" true c); save LOC_B1 "$LOC_B1"
  ok "库位 L01A=$LOC_A1 L02A=$LOC_A2 L01B=$LOC_B1"
  check "A 仓默认库位唯一" "$(PSQLQ -c "select count(*) from biz_location where warehouse_id='$WH_A' and is_default=1 and deleted_at is null;")" "1"

  # 默认库位切换：用一次性库位（先软删旧的）→ 旧默认自动让位 → 再切回
  TMP_LOC=$(PSQLQ -c "select id from biz_location where location_code='E2E03-L09A' and warehouse_id='$WH_A' and deleted_at is null;")
  [ -n "$TMP_LOC" ] && api a1_loc_tmp_del DELETE "/business/erp/location/$TMP_LOC" "$T_ADMIN"
  api a1_loc_tmp POST /business/erp/location "$T_ADMIN" "{\"location_code\":\"E2E03-L09A\",\"warehouse_id\":\"$WH_A\",\"location_name\":\"A 仓临时默认库位\",\"location_type\":\"STORAGE\",\"is_default\":true,\"status\":1}"
  TMP_LOC=$(data '.data.id // empty')
  [ -z "$TMP_LOC" ] && TMP_LOC=$(PSQLQ -c "select id from biz_location where location_code='E2E03-L09A' and warehouse_id='$WH_A' and deleted_at is null;")
  ok "临时库位 L09A = $TMP_LOC（置默认）"
  check "默认库位切到 L09A 后 L01A 让位" "$(PSQLQ -c "select is_default::text from biz_location where id='$LOC_A1';")" "0"
  check "默认库位唯一（切换后）" "$(PSQLQ -c "select count(*) from biz_location where warehouse_id='$WH_A' and is_default=1 and deleted_at is null;")" "1"
  api a1_loc_tmp_del2 DELETE "/business/erp/location/$TMP_LOC" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "删除临时库位 L09A（未使用可删）" || bad "删临时库位 http=$HTTP $(show)"
  api a1_loc1_back PUT "/business/erp/location/$LOC_A1" "$T_ADMIN" '{"is_default":true}'
  [ "$CODE" = 0 ] && ok "PUT 把 L01A 改回默认" || bad "PUT 默认库位 http=$HTTP $(show)"
  check "默认库位回到 L01A" "$(PSQLQ -c "select is_default::text from biz_location where id='$LOC_A1';")" "1"
  summary
}

step2() {
  say "== step2 ② 库位管理（库位树 = 仓库树 × 库位；分页 / 下拉 / 详情 / 改 / 增 / 删）"
  # 自清理：只动本卡自造的库位（E2E03-*，且不是 L01A/L02A）
  for stale in $(PSQLQ -c "select id from biz_location where warehouse_id='$WH_A' and location_code like 'E2E03-%' and location_code not in ('E2E03-L01A','E2E03-L02A') and deleted_at is null;"); do
    api "a2_clean_$stale" DELETE "/business/erp/location/$stale" "$T_ADMIN"
  done
  api a2_page GET "/business/erp/location/page?page=1&page_size=50&warehouse_id=$WH_A" "$T_ADMIN"
  check "库位分页 HTTP/code" "$HTTP/$CODE" "200/0"
  check "A 仓库位数" "$(data '.data.total')" "2"
  ok "库位分页行: $(data '[.data.list[]?.location_code] | join(",")')"
  api a2_list GET "/business/erp/location/list?warehouse_id=$WH_A&include_disabled=true" "$T_ADMIN"
  check "库位下拉条数" "$(data '.data | length')" "2"
  api a2_detail GET "/business/erp/location/$LOC_A2" "$T_ADMIN"
  check "库位详情 编码/仓库/默认" "$(data '.data.location_code + "|" + .data.warehouse_id + "|" + (.data.is_default|tostring)')" "E2E03-L02A|$WH_A|false"
  api a2_update PUT "/business/erp/location/$LOC_A2" "$T_ADMIN" '{"location_name":"A 仓二号库位(改名)","capacity":500,"remark":"E2E-03 库位改名"}'
  [ "$CODE" = 0 ] && ok "改库位（名称/容量/备注）" || bad "改库位 http=$HTTP $(show)"
  check "改后 DB 名称/容量" "$(PSQLQ -c "select location_name||'|'||capacity::numeric(20,2) from biz_location where id='$LOC_A2';")" "A 仓二号库位(改名)|500.00"
  api a2_add POST /business/erp/location "$T_ADMIN" "{\"location_code\":\"E2E03-L08A\",\"warehouse_id\":\"$WH_A\",\"location_name\":\"A 仓待删库位\",\"location_type\":\"STORAGE\",\"is_default\":false,\"status\":1}"
  LOC_TMP2=$(data '.data.id // empty')
  [ -z "$LOC_TMP2" ] && LOC_TMP2=$(PSQLQ -c "select id from biz_location where location_code='E2E03-L08A' and warehouse_id='$WH_A' and deleted_at is null;")
  [ -n "$LOC_TMP2" ] && ok "新建待删库位 L08A = $LOC_TMP2"
  check "新增后 A 仓库位数" "$(PSQLQ -c "select count(*) from biz_location where warehouse_id='$WH_A' and deleted_at is null;")" "3"
  api a2_delete2 DELETE "/business/erp/location/$LOC_TMP2" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "删除 L08A" || bad "删库位 http=$HTTP $(show)"
  check "删除后 A 仓库位数" "$(PSQLQ -c "select count(*) from biz_location where warehouse_id='$WH_A' and deleted_at is null;")" "2"
  check "删除为软删" "$(PSQLQ -c "select (deleted_at is not null)::text from biz_location where id='$LOC_TMP2';")" "true"
  api a2_tree2 GET "/business/erp/warehouse/tree" "$T_ADMIN"
  ok "库位树（仓库→库位）: 总仓/$(data '[.data[]?|select(.id=="'$WH_P'")|.children[]?.warehouse_code]|join("+")')"
  summary
}

step3() {
  say "== step3 ③ 序列号管理（批量生成 → 列表/扫码/详情 → 入库带号 → 出库带号 → 状态机）"
  api a3_gen_bad POST /business/erp/serial/batch-generate "$T_ADMIN" "{\"material_id\":\"$MAT1\",\"warehouse_id\":\"$WH_A\",\"count\":2,\"unit_cost\":10}"
  check "非一物一码物料批量生成 http/code" "$HTTP/$CODE" "400/66007"
  api a3_gen POST /business/erp/serial/batch-generate "$T_ADMIN" "{\"material_id\":\"$MATS\",\"warehouse_id\":\"$WH_A\",\"batch_no\":\"E2E03-B1\",\"count\":3,\"unit_cost\":50,\"remark\":\"E2E-03 序列号批量生成\"}"
  [ "$CODE" = 0 ] && ok "批量生成 3 个序列号（@50）" || bad "批量生成 http=$HTTP $(show)"
  say "     出参: $(printf '%s' "$BODY" | jq -c '.data|{count,unit_cost,amount,inventory}')"
  check "DB 序列号条数" "$(PSQLQ -c "select count(*) from biz_material_serial where material_id='$MATS' and deleted_at is null;")" "3"
  check "DB 序列号状态全 IN_STOCK" "$(PSQLQ -c "select count(*) from biz_material_serial where material_id='$MATS' and status='IN_STOCK' and deleted_at is null;")" "3"
  check "库存 MS = 3（默认库位桶）" "$(inv_qty "$MATS" "$WH_A")" "3.0000"
  check "流水 SERIAL_IN = 3" "$(txn_sum "$MATS" "$WH_A" SERIAL_IN IN)" "3.0000"
  # ADR-20261031：批量生成不指定 location_id 时两本账都落「未分配桶」（与收货补桶兜底一致）
  check "库位账 MS = 3（未分配桶，与成本账同桶）" "$(PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,4)::text from biz_inventory_location where material_id='$MATS' and warehouse_id='$WH_A' and location_id is null;")" "3.0000"
  check "成本账 MS 桶 = 未分配（location_id 空）" "$(PSQLQ -c "select count(*) from biz_inventory where material_id='$MATS' and warehouse_id='$WH_A' and location_id is null and qty=3;")" "1"
  api a3_page GET "/business/erp/serial/page?page=1&page_size=20&material_id=$MATS" "$T_ADMIN"
  check "序列号分页 total" "$(data '.data.total')" "3"
  SN1=$(data '.data.list[0].serial_no')
  SNID1=$(data '.data.list[0].id')
  save SN1 "$SN1"; save SNID1 "$SNID1"; ok "首个序列号 $SN1 ($SNID1)"
  api a3_scan GET "/business/erp/serial/scan?serial_no=$SN1" "$T_ADMIN"
  check "扫码出参 物料/仓库/库位" "$(data '.data.material_code + "|" + .data.warehouse_name')" "E2E03-MS|E2E-03 演练主仓(改名)"
  api a3_detail GET "/business/erp/serial/$SNID1" "$T_ADMIN"
  check "序列号详情 状态" "$(data '.data.status')" "IN_STOCK"
  ok "序列号 location_id = $(data '.data.location_id // "null"')（批量生成未指定库位 → ADR-20261031 口径落未分配桶）"
  # 入库带显式序列号（qty=2 → 2 个号；号码只能在 submit-post 补，建单不收）
  api a3_sn_stockin POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_A\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-03 入库带序列号\",\"items\":[{\"material_id\":\"$MATS\",\"qty\":2,\"price_excl\":50,\"tax_rate\":0}]}"
  if [ "$CODE" != 0 ]; then bad "入库带序列号 http=$HTTP $(show)"; else
    IN_SN=$(data '.data.header.id // .data.id // empty')
    [ -z "$IN_SN" ] && IN_SN=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-03 入库带序列号' and deleted_at is null order by created_at desc limit 1;")
    save IN_SN "$IN_SN"
    ST_IN=$(post_stockin "$IN_SN" a3_sn_stockin '{"serials":[{"line_no":1,"serial_ids":["E2E03SN-9001","E2E03SN-9002"]}]}')
    check "入库带号单据状态（<1000 免审自动过账）" "$ST_IN" "POSTED"
    check "显式序列号原样落库（E2E03SN-9001/9002）" "$(PSQLQ -c "select count(*) from biz_material_serial where serial_no in ('E2E03SN-9001','E2E03SN-9002') and status='IN_STOCK';")" "2"
    check "显式号挂上本单来源" "$(PSQLQ -c "select count(*) from biz_material_serial where serial_no in ('E2E03SN-9001','E2E03SN-9002') and source_id='$IN_SN';")" "2"
    check "显式号落库位（非空）" "$(PSQLQ -c "select count(*) from biz_material_serial where serial_no in ('E2E03SN-9001','E2E03SN-9002') and location_id is not null;")" "2"
    check "库存 MS = 5" "$(inv_qty "$MATS" "$WH_A")" "5.0000"
    check "流水 PURCHASE_IN(序列号物料) = 2" "$(txn_sum "$MATS" "$WH_A" PURCHASE_IN IN)" "2.0000"
  fi
  # 负向 A：建单时序列号条数 ≠ 数量 → 400（DTO 层）
  api a3_sn_mismatch POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_A\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-03 号数不符\",\"items\":[{\"material_id\":\"$MATS\",\"qty\":2,\"price_excl\":50,\"tax_rate\":0,\"serial_ids\":[\"E2E03SN-9101\"]}]}"
  check "建单时序列号条数≠数量 http" "$HTTP" "400"
  check "号数不符单据未落库" "$(PSQLQ -c "select count(*) from biz_stock_in where remark='E2E-03 号数不符';")" "0"
  # 负向 B：过账时补号条数 ≠ 数量 → 400，单保持 DRAFT（然后删掉，不留死单）
  api a3_sn_mm2 POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_A\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-03 过账补号不符\",\"items\":[{\"material_id\":\"$MATS\",\"qty\":2,\"price_excl\":50,\"tax_rate\":0}]}"
  IN_MM2=$(data '.data.header.id // .data.id // empty')
  [ -z "$IN_MM2" ] && IN_MM2=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-03 过账补号不符' and deleted_at is null order by created_at desc limit 1;")
  api a3_sn_mm2_sub POST "/business/erp/stock-in/$IN_MM2/submit-post" "$T_ADMIN" '{"serials":[{"line_no":1,"serial_ids":["E2E03SN-9102"]}]}'
  check "过账补号条数≠数量 http" "$HTTP" "400"
  check "补号不符后单据仍 DRAFT" "$(PSQLQ -c "select status from biz_stock_in where id='$IN_MM2';")" "DRAFT"
  api a3_sn_mm2_del DELETE "/business/erp/stock-in/$IN_MM2" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "删掉该草稿单（不留死单）" || bad "删草稿单 http=$HTTP $(show)"
  # 入库不带号 → 服务端自动取号（一物一码不变式①：qty == count(IN_STOCK)）
  api a3_sn_auto POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_A\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-03 入库自动取号\",\"items\":[{\"material_id\":\"$MATS\",\"qty\":1,\"price_excl\":50,\"tax_rate\":0}]}"
  IN_AUTO=$(data '.data.header.id // .data.id // empty')
  [ -z "$IN_AUTO" ] && IN_AUTO=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-03 入库自动取号' and deleted_at is null order by created_at desc limit 1;")
  save IN_AUTO "$IN_AUTO"
  ST_AUTO=$(post_stockin "$IN_AUTO" a3_sn_auto)
  check "自动取号入库状态" "$ST_AUTO" "POSTED"
  check "库存 MS = 6" "$(inv_qty "$MATS" "$WH_A")" "6.0000"
  check "不变式① qty == count(IN_STOCK)" "$(PSQLQ -c "select (select count(*) from biz_material_serial where material_id='$MATS' and status='IN_STOCK' and deleted_at is null)::text;")" "6"
  # 出库带序列号（一物一码物料禁止走出库单，出库走序列号状态流转）
  api a3_sn_out PATCH "/business/erp/serial/$SNID1/status" "$T_ADMIN" '{"status":"OUTBOUND","reason":"E2E-03 演练出库"}'
  [ "$CODE" = 0 ] && ok "序列号出库（IN_STOCK→OUTBOUND）" || bad "序列号出库 http=$HTTP $(show)"
  check "序列号状态" "$(PSQLQ -c "select status from biz_material_serial where id='$SNID1';")" "OUTBOUND"
  check "库存 MS = 5（出库扣 1）" "$(inv_qty "$MATS" "$WH_A")" "5.0000"
  check "流水 SERIAL_OUT = 1" "$(txn_sum "$MATS" "$WH_A" SERIAL_OUT OUT)" "1.0000"
  check "出库流水 remark 带原因" "$(PSQLQ -c "select count(*) from biz_inventory_txn where material_id='$MATS' and biz_type='SERIAL_OUT' and remark like '%E2E-03 演练出库%';")" "1"
  api a3_sn_back PATCH "/business/erp/serial/$SNID1/status" "$T_ADMIN" '{"status":"IN_STOCK","reason":"非法回库尝试"}'
  check "OUTBOUND→IN_STOCK 被拦 http" "$HTTP" "400"
  check "状态未被改回" "$(PSQLQ -c "select status from biz_material_serial where id='$SNID1';")" "OUTBOUND"
  check "MS 库存 = 序列号在库数（5=5）" "$(inv_qty "$MATS" "$WH_A")" "$(PSQLQ -c "select count(*)::numeric(20,4)::text from biz_material_serial where material_id='$MATS' and status='IN_STOCK' and deleted_at is null;")"
  summary
}

step4() {
  say "== step4 ④ 期初库存（FIX-6 对方科目可配 → 录入即建账 → 桶与收货一致）"
  PERIOD=2026-08   # 该期间库内无既有库存期初（2026-09/10/11 已被别的演练占用，整期覆盖会误伤）
  save PERIOD "$PERIOD"
  api a4_cfg_get GET "/support/config/page?page=1&page_size=10&keyword=inventory_opening_equity_account" "$T_ADMIN"
  CFG_ID=$(PSQLQ -c "select id from sys_config where config_key='inventory_opening_equity_account';")
  save CFG_ID "$CFG_ID"
  check "配置项 id 存在" "$([ -n "$CFG_ID" ] && echo yes)" "yes"
  CFG_OLD=$(PSQLQ -c "select config_value from sys_config where config_key='inventory_opening_equity_account';")
  api a4_cfg_detail GET "/support/config/$CFG_ID" "$T_ADMIN"
  check "配置详情键名" "$(data '.data.config_key')" "inventory_opening_equity_account"
  save CFG_OLD "$CFG_OLD"; ok "期初对方科目当前值 = $CFG_OLD（DB 与接口同源）"
  CFG_JSON=$(printf '%s' "$BODY" | jq -c '.data | {config_key, config_type, description, category, sort_order, config_level, is_modifiable, is_encrypted, is_restart, status, ref_value, select_value}')
  CFG_BODY=$(printf '%s' "$CFG_JSON" | jq -c '. + {config_value:"3101"}')
  CFG_BODY2=$(printf '%s' "$CFG_JSON" | jq -c --arg v "$CFG_OLD" '. + {config_value:$v}')
  api a4_cfg_put PUT "/support/config/$CFG_ID" "$T_ADMIN" "$CFG_BODY"
  [ "$CODE" = 0 ] && ok "改配置 inventory_opening_equity_account = 3101（FIX-6 可配）" || bad "改配置 http=$HTTP $(show)"
  api a4_open POST /business/erp/opening-balance/inventory "$T_ADMIN" "{\"period\":\"$PERIOD\",\"items\":[{\"material_id\":\"$MAT1\",\"warehouse_id\":\"$WH_A\",\"qty\":100,\"unit_cost\":10,\"remark\":\"E2E-03 期初 M1\"}]}"
  [ "$CODE" = 0 ] && ok "库存期初录入 2026-08 M1 100@10" || bad "库存期初 http=$HTTP $(show)"
  OV=$(data '.data.voucher.id // empty'); save OV "$OV"
  say "     出参: $(printf '%s' "$BODY" | jq -c '.data|{state,inventory_amount,voucher_no:.voucher.voucher_no,inventory_debit_account,inventory_credit_account}')"
  check "期初台账落库" "$(PSQLQ -c "select qty::numeric(20,4)||'|'||unit_cost::numeric(20,2) from biz_opening_inventory where period_code='$PERIOD' and material_id='$MAT1';")" "100.0000|10.00"
  check "期初台账记了库位桶" "$(PSQLQ -c "select (location_row_id is not null)::text from biz_opening_inventory where period_code='$PERIOD' and material_id='$MAT1';")" "true"
  check "期初凭证 id 已返回" "$([ -n "$OV" ] && echo yes)" "yes"
  say "     期初凭证 1405/对方科目: $(PSQLQ -c "select account_code||' '||debit_amount::numeric(20,2)||'/'||credit_amount::numeric(20,2) from biz_voucher_entry where voucher_id='$OV' order by line_no;" | tr '\n' ' ')"
  check "FIX-6 贷方科目 = 3101（配置生效）" "$(PSQLQ -c "select count(*) from biz_voucher_entry where voucher_id='$OV' and account_code='3101' and credit_amount=1000;")" "1"
  check "期初凭证借贷平衡" "$(voucher_balance "$OV")" "1000.00|1000.00"
  check "库存 M1@A = 100" "$(inv_qty "$MAT1" "$WH_A")" "100.0000"
  check "期初流水 IN/OPENING = 100" "$(txn_sum "$MAT1" "$WH_A" OPENING IN)" "100.0000"
  check "期初落默认库位桶（库位账 100）" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")" "100.0000"
  BUCKET_A=$(bucket_row "$MAT1" "$WH_A"); save BUCKET_A "$BUCKET_A"
  ok "M1@A 成本账桶行 = $BUCKET_A（= 库位账行 id）"
  # 恢复配置 3103，并把 M2 也录进同期（证明可来回切换，且整期覆盖按差额、不误伤他人期间）
  api a4_cfg_restore PUT "/support/config/$CFG_ID" "$T_ADMIN" "$CFG_BODY2"
  [ "$CODE" = 0 ] && ok "配置恢复 $CFG_OLD" || bad "恢复配置 http=$HTTP $(show)"
  api a4_open2 POST /business/erp/opening-balance/inventory "$T_ADMIN" "{\"period\":\"$PERIOD\",\"items\":[{\"material_id\":\"$MAT1\",\"warehouse_id\":\"$WH_A\",\"qty\":100,\"unit_cost\":10,\"remark\":\"E2E-03 期初 M1\"},{\"material_id\":\"$MAT2\",\"warehouse_id\":\"$WH_A\",\"qty\":20,\"unit_cost\":5,\"remark\":\"E2E-03 期初 M2\"}]}"
  [ "$CODE" = 0 ] && ok "期初二次录入（M1 不动 + M2 新增，差额法）" || bad "期初二次录入 http=$HTTP $(show)"
  OV=$(data '.data.voucher.id // empty'); [ -n "$OV" ] && save OV "$OV"
  check "M1 期初未重复记（仍 100）" "$(inv_qty "$MAT1" "$WH_A")" "100.0000"
  check "M2 期初 20" "$(inv_qty "$MAT2" "$WH_A")" "20.0000"
  check "M2 期初落同一默认库位桶" "$(loc_qty "$MAT2" "$WH_A" "$LOC_A1")" "20.0000"
  say "     期初凭证(恢复后): $(PSQLQ -c "select account_code||' '||debit_amount::numeric(20,2)||'/'||credit_amount::numeric(20,2) from biz_voucher_entry where voucher_id='$OV' order by line_no;" | tr '\n' ' ')"
  check "恢复后贷方 = 3103 且金额 1100" "$(PSQLQ -c "select count(*) from biz_voucher_entry where voucher_id='$OV' and account_code='3103' and credit_amount=1100;")" "1"
  check "恢复后借贷平衡" "$(voucher_balance "$OV")" "1100.00|1100.00"
  check "期初台账无重复行（M1 一行）" "$(PSQLQ -c "select count(*) from biz_opening_inventory where period_code='$PERIOD' and material_id='$MAT1';")" "1"
  api a4_open_bad POST /business/erp/opening-balance/inventory "$T_ADMIN" "{\"period\":\"$PERIOD\",\"items\":[{\"material_id\":\"$MAT1\",\"warehouse_id\":\"$WH_A\",\"qty\":0,\"unit_cost\":10}]}"
  check "期初数量 0 http" "$HTTP" "400"
  api a4_query GET "/business/erp/opening-balance?period=$PERIOD" "$T_ADMIN"
  check "期初查询 HTTP/code" "$HTTP/$CODE" "200/0"
  ok "期初查询回显 $(data '.data.inventory | length') 行库存期初，借方科目 $(data '.data.inventory_debit_account')，贷方科目 $(data '.data.inventory_credit_account')"
  summary
}

case "${1:-all}" in
  all) step0; step1; step2; step3; step4 ;;
  step0|step1|step2|step3|step4) "$1" ;;
esac
