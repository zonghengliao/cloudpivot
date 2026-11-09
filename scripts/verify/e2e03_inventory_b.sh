#!/usr/bin/env bash
# E2E-03 库存域演练 B 段：入库补货 · ⑧ 库存调拨 · ⑨ 盘点单（盘盈/盘亏）· 出库
# 卡面 ⑤/⑥/⑦（即时库存/库位库存/库存流水）是只读页，放在 C 段做接口巡检。
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e03_inventory_common.sh"

need() { # 缺前置就补算（允许分段重跑）
  WH_A=${WH_A:-$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E03-WH-A' and deleted_at is null;")}
  WH_B=${WH_B:-$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E03-WH-B' and deleted_at is null;")}
  MAT1=${MAT1:-$(PSQLQ -c "select id from biz_material where material_code='E2E03-M1' and deleted_at is null;")}
  MATS=${MATS:-$(PSQLQ -c "select id from biz_material where material_code='E2E03-MS' and deleted_at is null;")}
  SUP_ID=${SUP_ID:-$(PSQLQ -c "select id from biz_supplier where supplier_code='E2E03-SUP' and deleted_at is null;")}
  LOC_A1=${LOC_A1:-$(PSQLQ -c "select id from biz_location where location_code='E2E03-L01A' and warehouse_id='$WH_A' and deleted_at is null;")}
  LOC_A2=${LOC_A2:-$(PSQLQ -c "select id from biz_location where location_code='E2E03-L02A' and warehouse_id='$WH_A' and deleted_at is null;")}
  LOC_B1=${LOC_B1:-$(PSQLQ -c "select id from biz_location where location_code='E2E03-L01B' and warehouse_id='$WH_B' and deleted_at is null;")}
  T_ADMIN=${T_ADMIN:-$(login admin "$NIK_ADMIN_PASSWORD" n0)}
  T_APP=${T_APP:-$(login e2e03_wh 'E2e03@Wh123456' n1)}
  BUCKET_A=$(bucket_row "$MAT1" "$WH_A")
}
need

step5() {
  say "== step5 入库补货（无源入库 50@12 → 提交过账；桶与期初一致 / in_type=SOURCELESS）"
  api b5_in POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_A\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-03 入库补货\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":50,\"price_excl\":12,\"tax_rate\":0}]}"
  [ "$CODE" = 0 ] || bad "建无源入库 http=$HTTP $(show)"
  IN2=$(data '.data.header.id // .data.id // empty')
  [ -z "$IN2" ] && IN2=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-03 入库补货' and deleted_at is null order by created_at desc limit 1;")
  save IN2 "$IN2"; ok "入库单 $IN2"
  ST=$(post_stockin "$IN2" b5_in)
  check "入库单状态（600<1000 免审自动过账）" "$ST" "POSTED"
  check "in_type = SOURCELESS（无源单不得记成 PURCHASE）" "$(PSQLQ -c "select in_type from biz_stock_in where id='$IN2';")" "SOURCELESS"
  check "库存 M1@A = 150" "$(inv_qty "$MAT1" "$WH_A")" "150.0000"
  check "流水 PURCHASE_IN = 50" "$(txn_sum "$MAT1" "$WH_A" PURCHASE_IN IN)" "50.0000"
  check "收货与期初同桶（FIX-4：整仓只有一个桶）" "$(PSQLQ -c "select count(*) from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_A';")" "1"
  check "该桶 = 默认库位桶行" "$(bucket_row "$MAT1" "$WH_A")" "$BUCKET_A"
  check "库位账默认桶 = 150" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")" "150.0000"
  V_IN=$(PSQLQ -c "select coalesce(voucher_id::text,'') from biz_stock_in where id='$IN2';")
  check "入库凭证借贷平衡（600）" "$(voucher_balance "$V_IN")" "600.00|600.00"
  # 负向：无源入库必须带供应商（ck_biz_si_supplier_scope）
  api b5_nosup POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"warehouse_id\":\"$WH_A\",\"in_date\":\"2026-11-09\",\"remark\":\"E2E-03 无源无供应商\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":1,\"price_excl\":12,\"tax_rate\":0}]}"
  if [ "$HTTP" = "400" ] || [ "$HTTP" = "409" ]; then ok "无源入库缺供应商被拦 http=$HTTP code=$CODE"; else bad "无源入库缺供应商 http=$HTTP（期望 400/409）"; fi
  summary
}

step6() {
  say "== step6 ⑧ 库存调拨（A→B 40；A 仓两桶并存，指定 L01 桶 → 只扣该桶，L02 桶不动）"
  # 1) B 仓建桶（无 location 参数 → 默认库位桶）
  api b6_b_in POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_B\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-03 B 仓建桶\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":10,\"price_excl\":12,\"tax_rate\":0}]}"
  IN_B=$(data '.data.header.id // .data.id // empty')
  [ -z "$IN_B" ] && IN_B=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-03 B 仓建桶' and deleted_at is null order by created_at desc limit 1;")
  STB=$(post_stockin "$IN_B" b6_b_in)
  check "B 仓建桶入库状态" "$STB" "POSTED"
  BUCKET_B=$(bucket_row "$MAT1" "$WH_B"); save BUCKET_B "$BUCKET_B"
  ok "B 仓桶行 = $BUCKET_B（库位账行 id）"
  # 2) A 仓第二桶（显式指定非默认库位 LOC_A2 → 制造「同仓两桶并存」= P0-03 的触发条件）
  api b6_a2_in POST /business/erp/stock-in "$T_ADMIN" "{\"is_sourceless\":true,\"supplier_id\":\"$SUP_ID\",\"warehouse_id\":\"$WH_A\",\"in_date\":\"2026-11-09\",\"invoice_received\":true,\"remark\":\"E2E-03 A 仓第二桶\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":5,\"price_excl\":12,\"tax_rate\":0,\"location_dict_id\":\"$LOC_A2\"}]}"
  IN_A2=$(data '.data.header.id // .data.id // empty')
  [ -z "$IN_A2" ] && IN_A2=$(PSQLQ -c "select id from biz_stock_in where remark='E2E-03 A 仓第二桶' and deleted_at is null order by created_at desc limit 1;")
  STA2=$(post_stockin "$IN_A2" b6_a2_in)
  check "A 仓第二桶入库状态" "$STA2" "POSTED"
  check "A 仓现在两桶并存" "$(PSQLQ -c "select count(*) from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_A';")" "2"
  BUCKET_A2=$(PSQLQ -c "select id from biz_inventory_location where material_id='$MAT1' and warehouse_id='$WH_A' and location_id='$LOC_A2';")
  save BUCKET_A2 "$BUCKET_A2"; ok "A 仓 L02 桶行 = $BUCKET_A2"
  L01_BEFORE=$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")
  TOTAL_BEFORE=$(( $(inv_qty "$MAT1" "$WH_A" | cut -d. -f1) + $(inv_qty "$MAT1" "$WH_B" | cut -d. -f1) ))
  save TOTAL_BEFORE "$TOTAL_BEFORE"
  ok "调拨前：A/L01=$L01_BEFORE A/L02=$(loc_qty "$MAT1" "$WH_A" "$LOC_A2") B/L01=$(loc_qty "$MAT1" "$WH_B" "$LOC_B1") 总量=$TOTAL_BEFORE"

  # 负向 1：调出=调入 → 400
  api b6_same POST /business/erp/transfer "$T_ADMIN" "{\"from_warehouse_id\":\"$WH_A\",\"to_warehouse_id\":\"$WH_A\",\"transfer_date\":\"2026-11-09\",\"reason\":\"E2E-03 同仓调拨\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":1}]}"
  check "同仓调拨 http" "$HTTP" "400"
  # 负向 2：数量 0 → 400
  api b6_zero POST /business/erp/transfer "$T_ADMIN" "{\"from_warehouse_id\":\"$WH_A\",\"to_warehouse_id\":\"$WH_B\",\"transfer_date\":\"2026-11-09\",\"reason\":\"E2E-03 零数量\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":0}]}"
  check "调拨数量 0 http" "$HTTP" "400"

  # 正单：从 A/L01 桶 → B/L01 桶，40 件
  api b6_create POST /business/erp/transfer "$T_ADMIN" "{\"from_warehouse_id\":\"$WH_A\",\"to_warehouse_id\":\"$WH_B\",\"transfer_date\":\"2026-11-09\",\"reason\":\"E2E-03 演练调拨 A→B\",\"remark\":\"E2E-03 指定库位桶\",\"items\":[{\"material_id\":\"$MAT1\",\"from_location_id\":\"$BUCKET_A\",\"to_location_id\":\"$BUCKET_B\",\"qty\":40}]}"
  [ "$CODE" = 0 ] || bad "建调拨单 http=$HTTP $(show)"
  TR=$(data '.data.id // .data.header.id // empty')
  [ -z "$TR" ] && TR=$(PSQLQ -c "select id from biz_stock_transfer where remark='E2E-03 指定库位桶' and deleted_at is null order by created_at desc limit 1;")
  save TR "$TR"; ok "调拨单 $TR 状态=$(PSQLQ -c "select status from biz_stock_transfer where id='$TR';")"
  check "明细 from 库位 = A/L01 桶" "$(PSQLQ -c "select from_location_id from biz_stock_transfer_item where transfer_id='$TR';")" "$BUCKET_A"
  check "明细 to 库位 = B/L01 桶" "$(PSQLQ -c "select to_location_id from biz_stock_transfer_item where transfer_id='$TR';")" "$BUCKET_B"

  api b6_pending PATCH "/business/erp/transfer/$TR/status" "$T_ADMIN" '{"status":"PENDING"}'
  [ "$CODE" = 0 ] && ok "提交审批 DRAFT→PENDING（http=$HTTP）" || bad "提交审批 http=$HTTP $(show)"
  IID=$(PSQLQ -c "select coalesce(approval_instance_id::text,'') from biz_stock_transfer where id='$TR';")
  save TR_IID "$IID"
  check "调拨审批实例已建" "$([ -n "$IID" ] && echo yes)" "yes"
  approve_instance "$T_APP" "$IID" b6_ap
  check "审批后状态" "$(PSQLQ -c "select status from biz_stock_transfer where id='$TR';")" "APPROVED"
  api b6_transit PATCH "/business/erp/transfer/$TR/status" "$T_ADMIN" '{"status":"IN_TRANSIT"}'
  check "发运 IN_TRANSIT http" "$HTTP" "200"
  api b6_done PATCH "/business/erp/transfer/$TR/status" "$T_ADMIN" '{"status":"DONE"}'
  check "完成 DONE http" "$HTTP" "200"
  check "调拨单终态" "$(PSQLQ -c "select status from biz_stock_transfer where id='$TR';")" "DONE"
  check "A/L01 桶扣到 110（精确扣指定桶）" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")" "110.0000"
  check "A/L01 成本账同桶 110" "$(PSQLQ -c "select qty::numeric(20,4)::text from biz_inventory where location_id='$BUCKET_A';")" "110.0000"
  check "A/L02 桶未被静默改扣（仍 5）" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A2")" "5.0000"
  check "A/L02 成本账同桶 5" "$(PSQLQ -c "select qty::numeric(20,4)::text from biz_inventory where location_id='$BUCKET_A2';")" "5.0000"
  check "B/L01 桶加到 50" "$(loc_qty "$MAT1" "$WH_B" "$LOC_B1")" "50.0000"
  check "B/L01 成本账同桶 50" "$(PSQLQ -c "select qty::numeric(20,4)::text from biz_inventory where location_id='$BUCKET_B';")" "50.0000"
  check "流水 TRANSFER_OUT = 40" "$(txn_sum "$MAT1" "$WH_A" TRANSFER_OUT OUT)" "40.0000"
  check "流水 TRANSFER_IN = 40" "$(txn_sum "$MAT1" "$WH_B" TRANSFER_IN IN)" "40.0000"
  TOTAL_AFTER=$(( $(inv_qty "$MAT1" "$WH_A" | cut -d. -f1) + $(inv_qty "$MAT1" "$WH_B" | cut -d. -f1) ))
  check "调拨前后总量不变" "$TOTAL_AFTER" "$TOTAL_BEFORE"
  # 调拨是仓间移位（同一 1405 科目）：按设计**不产生凭证**，只写两侧流水与两本账
  N_V=$(PSQLQ -c "select count(*) from biz_voucher where source_id='$TR';")
  check "调拨不产生凭证（设计如此：同科目仓间移位）" "$N_V" "0"
  check "调拨金额守恒（OUT 40×10.6667 == IN 40×10.6667）" "$(PSQLQ -c "select count(*) from biz_inventory_txn where material_id='$MAT1' and doc_id='$TR' and biz_type in ('TRANSFER_OUT','TRANSFER_IN') and amount between 426.66 and 426.68;")" "2"
  check "逐桶 A：成本=库位（0 行差异）" "$(PSQLQ -c "select count(*) from biz_inventory i join biz_inventory_location il on il.id=i.location_id where i.material_id='$MAT1' and i.warehouse_id='$WH_A' and i.qty<>il.qty;")" "0"
  check "逐桶 B：成本=库位（0 行差异）" "$(PSQLQ -c "select count(*) from biz_inventory i join biz_inventory_location il on il.id=i.location_id where i.material_id='$MAT1' and i.warehouse_id='$WH_B' and i.qty<>il.qty;")" "0"
  summary
}

step7() {
  say "== step7 ⑨ 盘点单（抽盘盘盈 +5 → 全盘盘亏 −3；账面快照 / 差异流水 / 差异凭证）"
  # 自清理：本卡自造、还停在草稿的盘点单（重跑时不留死单）
  for stale in $(PSQLQ -c "select id from biz_stock_take where remark like 'E2E-03%' and status='DRAFT' and deleted_at is null;"); do
    api "b7_clean_$stale" DELETE "/business/erp/stock-take/$stale" "$T_ADMIN"
  done
  BOOK=$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")
  A_TOTAL=$(inv_qty "$MAT1" "$WH_A")
  ok "盘点前 A/L01 桶账面 = $BOOK（A 仓合计 $A_TOTAL）"
  api b7_create POST /business/erp/stock-take "$T_ADMIN" "{\"warehouse_id\":\"$WH_A\",\"take_date\":\"2026-11-09\",\"take_type\":\"PARTIAL\",\"remark\":\"E2E-03 盘盈演练\",\"items\":[{\"material_id\":\"$MAT1\",\"location_id\":\"$BUCKET_A\"}]}"
  [ "$CODE" = 0 ] || bad "建盘点单 http=$HTTP $(show)"
  STK=$(data '.data.id // empty')
  [ -z "$STK" ] && STK=$(PSQLQ -c "select id from biz_stock_take where remark='E2E-03 盘盈演练' and deleted_at is null order by created_at desc limit 1;")
  save STK "$STK"; ok "盘点单 STK=$STK"
  check "账面数服务端快照 = 110" "$(data '.data.items[0].book_qty')" "110.0000"
  ITEM_ID=$(data '.data.items[0].id'); save STK_ITEM "$ITEM_ID"
  api b7_neg PUT "/business/erp/stock-take/$STK" "$T_ADMIN" "{\"lines\":[{\"item_id\":\"$ITEM_ID\",\"actual_qty\":-1}]}"
  check "实盘数为负 http" "$HTTP" "400"
  api b7_fill PUT "/business/erp/stock-take/$STK" "$T_ADMIN" "{\"lines\":[{\"item_id\":\"$ITEM_ID\",\"actual_qty\":115}]}"
  [ "$CODE" = 0 ] && ok "录入实盘 115" || bad "录入实盘 http=$HTTP $(show)"
  check "差异 = 实盘 − 账面 = +5" "$(PSQLQ -c "select diff_qty::numeric(20,4)::text from biz_stock_take_item where id='$ITEM_ID';")" "5.0000"
  # 提交审批前必须先传「盘点表 / 盈亏说明」附件（ATT 规则的 CONFIRM 闸）
  api b7_noatt PATCH "/business/erp/stock-take/$STK/status" "$T_ADMIN" '{"status":"PENDING_POST"}'
  check "无附件提交审批被拦 http" "$HTTP" "400"
  ATT=$(mkpng)
  attach_upload b7_att1 STOCK_TAKE "$STK" "$ATT" DEFAULT
  [ "$CODE" = 0 ] && ok "上传盘点表附件（png）" || bad "上传附件 http=$HTTP $(show)"
  api b7_pending PATCH "/business/erp/stock-take/$STK/status" "$T_ADMIN" '{"status":"PENDING_POST"}'
  [ "$CODE" = 0 ] && ok "提交审批 DRAFT→PENDING_POST" || bad "提交审批 http=$HTTP $(show)"
  IID=$(PSQLQ -c "select coalesce(approval_instance_id::text,'') from biz_stock_take where id='$STK';")
  approve_instance "$T_APP" "$IID" b7_ap
  api b7_post PATCH "/business/erp/stock-take/$STK/status" "$T_ADMIN" '{"status":"POSTED"}'
  [ "$CODE" = 0 ] && ok "过账 POSTED http=$HTTP" || bad "过账 http=$HTTP $(show)"
  check "盘点单终态" "$(PSQLQ -c "select status from biz_stock_take where id='$STK';")" "POSTED"
  check "盘盈后 A/L01 = 115" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")" "115.0000"
  check "盘盈流水 STOCK_TAKE_GAIN = 5" "$(txn_sum "$MAT1" "$WH_A" STOCK_TAKE_GAIN IN)" "5.0000"
  check "A/L02 桶未受影响（5）" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A2")" "5.0000"
  V_ST=$(PSQLQ -c "select coalesce(voucher_id::text,'') from biz_stock_take where id='$STK';")
  BAL=$(voucher_balance "$V_ST"); D=${BAL%%|*}; C=${BAL##*|}
  check "盘盈差异凭证借贷平衡（$D/$C）" "$D" "$C"
  say "     盘盈凭证: $(PSQLQ -c "select account_code||' '||debit_amount::numeric(20,2)||'/'||credit_amount::numeric(20,2) from biz_voucher_entry where voucher_id='$V_ST' order by line_no;" | tr '\n' ' ')"

  # 全盘盘亏：按仓库现有库存桶自动快照（含一物一码物料，其差异为 0）
  api b7_full POST /business/erp/stock-take "$T_ADMIN" "{\"warehouse_id\":\"$WH_A\",\"take_date\":\"2026-11-09\",\"take_type\":\"FULL\",\"remark\":\"E2E-03 盘亏演练\"}"
  [ "$CODE" = 0 ] || bad "建全盘单 http=$HTTP $(show)"
  STK2=$(data '.data.id // empty')
  [ -z "$STK2" ] && STK2=$(PSQLQ -c "select id from biz_stock_take where remark='E2E-03 盘亏演练' and deleted_at is null order by created_at desc limit 1;")
  save STK2 "$STK2"
  N_LINES=$(data '.data.items | length')
  ok "全盘快照 $N_LINES 行: $(data '[.data.items[]? | .material_code + ":" + .book_qty] | join(", ")')"
  ITEM2=$(data ".data.items[] | select(.material_id==\"$MAT1\") | select(.location_id==\"$BUCKET_A\") | .id")
  [ -z "$ITEM2" ] && ITEM2=$(data ".data.items[] | select(.material_id==\"$MAT1\") | .id" | head -1)
  api b7_fill2 PUT "/business/erp/stock-take/$STK2" "$T_ADMIN" "{\"lines\":[{\"item_id\":\"$ITEM2\",\"actual_qty\":112}]}"
  [ "$CODE" = 0 ] && ok "全盘录入 M1 实盘 112（账面 115）" || bad "全盘录入 http=$HTTP $(show)"
  attach_upload b7_att2 STOCK_TAKE "$STK2" "$ATT" DEFAULT
  [ "$CODE" = 0 ] && ok "上传全盘盘点表附件" || bad "上传附件 http=$HTTP $(show)"
  api b7_pending2 PATCH "/business/erp/stock-take/$STK2/status" "$T_ADMIN" '{"status":"PENDING_POST"}'
  IID2=$(PSQLQ -c "select coalesce(approval_instance_id::text,'') from biz_stock_take where id='$STK2';")
  approve_instance "$T_APP" "$IID2" b7_ap2
  api b7_post2 PATCH "/business/erp/stock-take/$STK2/status" "$T_ADMIN" '{"status":"POSTED"}'
  [ "$CODE" = 0 ] && ok "全盘过账 POSTED" || bad "全盘过账 http=$HTTP $(show)"
  check "盘亏后 A/L01 = 112" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")" "112.0000"
  check "盘亏流水 STOCK_TAKE_LOSS = 3" "$(txn_sum "$MAT1" "$WH_A" STOCK_TAKE_LOSS OUT)" "3.0000"
  V_ST2=$(PSQLQ -c "select coalesce(voucher_id::text,'') from biz_stock_take where id='$STK2';")
  BAL2=$(voucher_balance "$V_ST2"); D2=${BAL2%%|*}; C2=${BAL2##*|}
  check "盘亏差异凭证借贷平衡（$D2/$C2）" "$D2" "$C2"
  check "全盘未动 L02 桶（5）" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A2")" "5.0000"
  summary
}

step8() {
  say "== step8 出库（领用 20 → POSTED；负库存闸；库存与流水同步扣）"
  api b8_create POST /business/erp/stock-out "$T_ADMIN" "{\"out_type\":\"ISSUE\",\"warehouse_id\":\"$WH_A\",\"out_date\":\"2026-11-09\",\"reason\":\"E2E-03 演练领用\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":20,\"location_id\":\"$BUCKET_A\"}]}"
  [ "$CODE" = 0 ] || bad "建出库单 http=$HTTP $(show)"
  OUT=$(data '.data.id // .data.header.id // empty')
  [ -z "$OUT" ] && OUT=$(PSQLQ -c "select id from biz_stock_out where reason='E2E-03 演练领用' and deleted_at is null order by created_at desc limit 1;")
  save OUT "$OUT"; ok "出库单 $OUT"
  api b8_post PATCH "/business/erp/stock-out/$OUT/status" "$T_ADMIN" '{"status":"POSTED"}'
  [ "$CODE" = 0 ] && ok "出库过账 POSTED" || bad "出库过账 http=$HTTP $(show)"
  check "出库后 A/L01 = 92" "$(loc_qty "$MAT1" "$WH_A" "$LOC_A1")" "92.0000"
  check "出库流水 ISSUE_OUT = 20" "$(txn_sum "$MAT1" "$WH_A" ISSUE_OUT OUT)" "20.0000"
  V_OUT=$(PSQLQ -c "select coalesce(voucher_id::text,'') from biz_stock_out where id='$OUT';")
  BAL=$(voucher_balance "$V_OUT"); D=${BAL%%|*}; C=${BAL##*|}
  check "出库结转成本凭证借贷平衡（$D/$C）" "$D" "$C"
  # 负向：超量出库 → 负库存闸拦下（单子留在 DRAFT，库存不动）
  api b8_over POST /business/erp/stock-out "$T_ADMIN" "{\"out_type\":\"ISSUE\",\"warehouse_id\":\"$WH_A\",\"out_date\":\"2026-11-09\",\"reason\":\"E2E-03 负库存尝试\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":99999}]}"
  OV=$(data '.data.id // .data.header.id // empty')
  [ -z "$OV" ] && OV=$(PSQLQ -c "select id from biz_stock_out where reason='E2E-03 负库存尝试' and deleted_at is null order by created_at desc limit 1;")
  api b8_over_post PATCH "/business/erp/stock-out/$OV/status" "$T_ADMIN" '{"status":"POSTED"}'
  if [ "$HTTP" = "200" ] && [ "$CODE" = "0" ]; then bad "超量出库竟然过账成功"; else ok "超量出库过账被拦 http=$HTTP code=$CODE"; fi
  check "超量单未过账" "$(PSQLQ -c "select status from biz_stock_out where id='$OV';")" "DRAFT"
  check "库存未被扣成负数" "$(inv_qty "$MAT1" "$WH_A")" "97.0000"
  summary
}

case "${1:-all}" in
  all) step5; step6; step7; step8 ;;
  step5|step6|step7|step8) "$1" ;;
esac
