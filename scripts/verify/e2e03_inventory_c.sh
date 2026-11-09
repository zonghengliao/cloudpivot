#!/usr/bin/env bash
# E2E-03 库存域演练 C 段：⑤⑥⑦ 只读页面巡检 + 全量对账 + 历史回归（P0-03 / FIX-4 / FIX-7）
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e03_inventory_common.sh"

need() {
  WH_A=${WH_A:-$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E03-WH-A' and deleted_at is null;")}
  WH_B=${WH_B:-$(PSQLQ -c "select id from biz_warehouse where warehouse_code='E2E03-WH-B' and deleted_at is null;")}
  MAT1=${MAT1:-$(PSQLQ -c "select id from biz_material where material_code='E2E03-M1' and deleted_at is null;")}
  MATS=${MATS:-$(PSQLQ -c "select id from biz_material where material_code='E2E03-MS' and deleted_at is null;")}
  PERIOD=${PERIOD:-2026-08}
  T_ADMIN=${T_ADMIN:-$(login admin "$NIK_ADMIN_PASSWORD" c0)}
  TR=${TR:-$(PSQLQ -c "select id from biz_stock_transfer where remark='E2E-03 指定库位桶' and deleted_at is null order by created_at desc limit 1;")}
  STK=${STK:-$(PSQLQ -c "select id from biz_stock_take where remark='E2E-03 盘盈演练' and deleted_at is null order by created_at desc limit 1;")}
}
need

step9() {
  say "== step9 ⑤⑥⑦ 只读页面巡检（即时库存 / 库位库存 / 库存流水 / 序列号 / 调拨 / 盘点 / 期初 / 巡检接口）"
  # ⑤ 即时库存
  api c9_inv GET "/business/erp/inventory/page?page=1&page_size=20&material_id=$MAT1" "$T_ADMIN"
  check "即时库存分页 HTTP/code" "$HTTP/$CODE" "200/0"
  check "即时库存 M1 行数（A 两桶 + B 一桶）" "$(data '.data.total')" "3"
  ok "即时库存: $(data '[.data.list[]? | (.warehouse_id[0:8]) + ":" + .qty] | join(", ")')"
  INV_ID=$(data '.data.list[0].id')
  api c9_inv_detail GET "/business/erp/inventory/$INV_ID" "$T_ADMIN"
  check "即时库存详情 HTTP/code" "$HTTP/$CODE" "200/0"
  check "详情字段 qty/avg_cost" "$([ -n "$(data '.data.qty')" ] && [ -n "$(data '.data.avg_cost')" ] && echo yes)" "yes"
  # ⑥ 库位库存
  api c9_loc GET "/business/erp/inventory-location/page?page=1&page_size=20&warehouse_id=$WH_A&material_id=$MAT1" "$T_ADMIN"
  check "库位库存分页 HTTP/code" "$HTTP/$CODE" "200/0"
  ok "库位库存（M1@A）$(data '.data.total') 行: $(data '[.data.list[]? | .qty] | join(" + ")')"
  check "库位账合计 = 成本账合计（M1@A，两桶 92+5）" \
    "$(PSQLQ -c "select coalesce(sum(qty),0)::numeric(20,0)::text from biz_inventory_location where material_id='$MAT1' and warehouse_id='$WH_A';")" \
    "$(inv_qty "$MAT1" "$WH_A" | cut -d. -f1)"
  # ⑦ 库存流水
  api c9_txn GET "/business/erp/inventory-txn/page?page=1&page_size=50&material_id=$MAT1" "$T_ADMIN"
  check "库存流水分页 HTTP/code" "$HTTP/$CODE" "200/0"
  ok "M1 流水 $(data '.data.total') 条"
  api c9_txn_type GET "/business/erp/inventory-txn/page?page=1&page_size=50&material_id=$MAT1&txn_type=OUT" "$T_ADMIN"
  check "流水按方向过滤 txn_type=OUT（调拨出/盘亏/出库）" "$(data '.data.total')" "3"
  api c9_txn_doc GET "/business/erp/inventory-txn/page?page=1&page_size=50&material_id=$MAT1&doc_type=STOCK_TRANSFER" "$T_ADMIN"
  check "流水按单据类型过滤 doc_type=STOCK_TRANSFER" "$(data '.data.total')" "2"
  TXN_ID=$(data '.data.list[0].id')
  api c9_txn_detail GET "/business/erp/inventory-txn/$TXN_ID" "$T_ADMIN"
  check "流水详情 before/after 快照" "$([ -n "$(data '.data.before_qty')" ] && [ -n "$(data '.data.after_qty')" ] && echo yes)" "yes"
  # 序列号
  api c9_serial GET "/business/erp/serial/page?page=1&page_size=50&material_id=$MATS" "$T_ADMIN"
  check "序列号分页 total（3 批量 + 2 显式 + 1 自动）" "$(data '.data.total')" "6"
  api c9_serial_out GET "/business/erp/serial/page?page=1&page_size=50&material_id=$MATS&status=OUTBOUND" "$T_ADMIN"
  check "序列号按状态过滤 OUTBOUND" "$(data '.data.total')" "1"
  # 调拨
  api c9_tr GET "/business/erp/transfer/page?page=1&page_size=20&status=DONE" "$T_ADMIN"
  check "调拨分页 HTTP/code" "$HTTP/$CODE" "200/0"
  api c9_tr_detail GET "/business/erp/transfer/$TR" "$T_ADMIN"
  check "调拨详情 状态/明细数" "$(data '.data.status + "|" + ((.data.items|length)|tostring)')" "DONE|1"
  api c9_tr_serials GET "/business/erp/transfer/$TR/serials" "$T_ADMIN"
  check "调拨序列号接口 HTTP/code" "$HTTP/$CODE" "200/0"
  # 盘点
  api c9_st GET "/business/erp/stock-take/page?page=1&page_size=20" "$T_ADMIN"
  check "盘点分页 HTTP/code" "$HTTP/$CODE" "200/0"
  api c9_st_detail GET "/business/erp/stock-take/$STK" "$T_ADMIN"
  check "盘点详情 状态/差异" "$(data '.data.status + "|" + .data.items[0].diff_qty')" "POSTED|5.0000"
  # 期初
  api c9_open GET "/business/erp/opening-balance?period=$PERIOD" "$T_ADMIN"
  check "期初查询 HTTP/code" "$HTTP/$CODE" "200/0"
  ok "期初 $PERIOD: $(data '.data.inventory|length') 行，金额 $(data '.data.inventory_amount')"
  # 库存巡检接口（服务端自检）—— ISSUE-01：一物一码多桶时账面取「汇总桶」→ 假差异
  api c9_audit GET "/business/erp/inventory/audit?dry_run=true" "$T_ADMIN"
  check "库存巡检接口 HTTP/code" "$HTTP/$CODE" "200/0"
  ok "巡检结论: checked=$(data '.data.checked') mismatched=$(data '.data.mismatched') unfixable=$(data '.data.unfixable')"
  ok "巡检命中本卡物料（ISSUE-01 假差异）: $(data "[.data.items[]? | select(.material_code|startswith(\"E2E03-\")) | .material_code + \" book=\" + .book_qty + \" actual=\" + .actual_qty + \" diff=\" + .diff] | join(\"; \")")"
  summary
}

step10() {
  say "== step10 全量对账（库存=流水 / 逐桶同桶 / 金额 / 序列号不变式 / 凭证平衡 / 历史回归）"
  inv=$(inv_qty "$MAT1" "$WH_A"); ok "M1@A 期末 = $inv（A/L01 92 + A/L02 5）"
  check "M1@B 期末 = 50" "$(inv_qty "$MAT1" "$WH_B")" "50.0000"
  check "MS@A 期末 = 5（序列号在库数）" "$(inv_qty "$MATS" "$WH_A")" "5.0000"
  # ① 库存 vs 流水（本卡物料，0 行差异）
  D1=$(PSQLQ -c "WITH inv AS (SELECT material_id,warehouse_id,SUM(qty) q FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id,warehouse_id,SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) q FROM biz_inventory_txn GROUP BY 1,2) SELECT count(*) FROM inv FULL JOIN txn USING (material_id,warehouse_id) JOIN biz_material m ON m.id=COALESCE(inv.material_id,txn.material_id) WHERE m.material_code LIKE 'E2E03-%' AND COALESCE(inv.q,0)<>COALESCE(txn.q,0);")
  check "① 库存表 = 流水累计（本卡物料差异行）" "$D1" "0"
  # ①b 全库差异行数未增加（既有 11 行历史遗留）
  DBASE=${BASE_DIFF_ROWS:-11}
  DNOW=$(PSQLQ -c "WITH inv AS (SELECT material_id,warehouse_id,SUM(qty) q FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id,warehouse_id,SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) q FROM biz_inventory_txn GROUP BY 1,2) SELECT count(*) FROM inv FULL JOIN txn USING (material_id,warehouse_id) WHERE COALESCE(inv.q,0)<>COALESCE(txn.q,0);")
  check "①b 全库差异行数 == 演练前基线（$DBASE）" "$DNOW" "$DBASE"
  # ② 逐桶 成本账 vs 库位账（本卡物料 0 行差异）
  D2=$(PSQLQ -c "WITH cost AS (SELECT i.material_id,i.warehouse_id,COALESCE(il.location_id::text,'(未分配)') b,SUM(i.qty) q FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id GROUP BY 1,2,3), loc AS (SELECT material_id,warehouse_id,COALESCE(location_id::text,'(未分配)') b,SUM(qty) q FROM biz_inventory_location GROUP BY 1,2,3) SELECT count(*) FROM cost FULL JOIN loc ON cost.material_id=loc.material_id AND cost.warehouse_id=loc.warehouse_id AND cost.b=loc.b JOIN biz_material m ON m.id=COALESCE(cost.material_id,loc.material_id) WHERE m.material_code LIKE 'E2E03-%' AND COALESCE(cost.q,0)<>COALESCE(loc.q,0);")
  check "② 成本账 = 库位账（逐桶，本卡物料差异行）" "$D2" "0"
  BBASE=${BASE_BUCKET_ROWS:-10}
  BNOW=$(PSQLQ -c "WITH cost AS (SELECT i.material_id,i.warehouse_id,COALESCE(il.location_id::text,'(未分配)') b,SUM(i.qty) q FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id GROUP BY 1,2,3), loc AS (SELECT material_id,warehouse_id,COALESCE(location_id::text,'(未分配)') b,SUM(qty) q FROM biz_inventory_location GROUP BY 1,2,3) SELECT count(*) FROM cost FULL JOIN loc ON cost.material_id=loc.material_id AND cost.warehouse_id=loc.warehouse_id AND cost.b=loc.b WHERE COALESCE(cost.q,0)<>COALESCE(loc.q,0);")
  check "②b 全库逐桶差异行数 == 基线（$BBASE）" "$BNOW" "$BBASE"
  # ③ 金额
  A1=$(PSQLQ -c "SELECT COALESCE(SUM(amount),0)::numeric(20,2)::text FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code LIKE 'E2E03-%';")
  A2=$(PSQLQ -c "SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END),0)::numeric(20,2)::text FROM biz_inventory_txn t JOIN biz_material m ON m.id=t.material_id WHERE m.material_code LIKE 'E2E03-%';")
  check "③ 库存金额 = 流水金额累计（本卡物料）" "$A1" "$A2"
  # ④ 流水自洽
  check "④ 流水 after−before = ±qty（全库异常行）" "$(PSQLQ -c "select count(*) from biz_inventory_txn where (txn_type='IN' and after_qty-before_qty<>qty) or (txn_type='OUT' and before_qty-after_qty<>qty);")" "0"
  # ⑤ 序列号不变式（按 (物料, 仓库) 求和 —— 与对账口径一致）
  check "⑤a 一物一码 qty == count(IN_STOCK)（本卡物料违例行）" "$(PSQLQ -c "SELECT count(*) FROM (SELECT i.material_id, i.warehouse_id, SUM(i.qty) q FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id AND m.serial_required WHERE m.material_code LIKE 'E2E03-%' GROUP BY 1,2) i JOIN LATERAL (SELECT count(*)::numeric(20,4) c FROM biz_material_serial s WHERE s.material_id=i.material_id AND s.warehouse_id=i.warehouse_id AND s.status='IN_STOCK' AND s.deleted_at IS NULL) x ON true WHERE i.q<>x.c;")" "0"
  ok "⑤b 全库既有违例（别人的夹具，不计入本卡）: $(PSQLQ -c "SELECT coalesce(string_agg(m.material_code||' book='||i.q||' actual='||x.c,', '),'(无)') FROM (SELECT i.material_id, i.warehouse_id, SUM(i.qty) q FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id AND m.serial_required GROUP BY 1,2) i JOIN biz_material m ON m.id=i.material_id JOIN LATERAL (SELECT count(*)::numeric(20,4) c FROM biz_material_serial s WHERE s.material_id=i.material_id AND s.warehouse_id=i.warehouse_id AND s.status='IN_STOCK' AND s.deleted_at IS NULL) x ON true WHERE m.material_code NOT LIKE 'E2E03-%' AND i.q<>x.c;")"
  # ⑥ FIX-4 期初桶必须真实在成本账里
  check "⑥ FIX-4 期初桶在成本账中存在（违例行）" "$(PSQLQ -c "SELECT count(*) FROM biz_opening_inventory oi WHERE oi.location_row_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM biz_inventory i WHERE i.location_id=oi.location_row_id AND i.material_id=oi.material_id);")" "0"
  # ⑦ FIX-7 销售出库流水 biz_type
  check "⑦ FIX-7 销售出库 biz_type=SALE_OUT（违例行）" "$(PSQLQ -c "SELECT count(*) FROM biz_stock_out o JOIN biz_inventory_txn t ON t.doc_id=o.id WHERE o.source_type='SALES' AND o.out_type='OTHER' AND t.biz_type<>'SALE_OUT';")" "0"
  # ⑧ 已过账单据必须有流水
  check "⑧ 已过账单据缺流水（违例单据数）" "$(PSQLQ -c "SELECT (SELECT count(*) FROM biz_stock_in d WHERE d.status='POSTED' AND d.deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id)) + (SELECT count(*) FROM biz_stock_out d WHERE d.status='POSTED' AND d.deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id)) + (SELECT count(*) FROM biz_stock_take d WHERE d.status='POSTED' AND d.deleted_at IS NULL AND EXISTS (SELECT 1 FROM biz_stock_take_item i WHERE i.take_id=d.id AND i.diff_qty<>0) AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id)) + (SELECT count(*) FROM biz_stock_transfer d WHERE d.status='DONE' AND d.deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id));")" "0"
  # ⑨ 凭证借贷平衡（全库）
  check "⑨ 全库凭证借贷不平张数" "$(PSQLQ -c "SELECT count(*) FROM (SELECT v.id FROM biz_voucher v JOIN biz_voucher_entry e ON e.voucher_id=v.id WHERE v.status='POSTED' AND v.deleted_at IS NULL GROUP BY v.id HAVING SUM(e.debit_amount)<>SUM(e.credit_amount)) x;")" "0"
  # ⑩ 负库存
  check "⑩ 负库存行数" "$(PSQLQ -c "select count(*) from biz_inventory where qty<0;")" "0"
  # ⑪ 本卡凭证均平衡 + 张数
  NV=$(PSQLQ -c "select count(*) from biz_voucher v where v.source_id in (select id from biz_stock_in where remark like 'E2E-03%') or v.source_id in (select id from biz_stock_out where reason like 'E2E-03%') or v.source_id in (select id from biz_stock_take where remark like 'E2E-03%');")
  ok "⑪ 本卡单据产生的凭证 $NV 张（均已并入 ⑨ 全库平衡校验）"
  summary
}

step11() {
  say "== step11 历史问题回归（P0-03 分库位漂移 / FIX-4 期初桶 / FIX-7 biz_type）"
  # P0-03：调拨明细指定库位时，成本账与库位账必须动同一个桶
  BUCKET_A=$(bucket_row "$MAT1" "$WH_A")
  check "P0-03 调拨指定桶后 成本账桶数(qty>0) = 库位账桶数(qty>0)" \
    "$(PSQLQ -c "select count(*) from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_A' and qty>0;")" \
    "$(PSQLQ -c "select count(*) from biz_inventory_location where material_id='$MAT1' and warehouse_id='$WH_A' and qty>0;")"
  check "P0-03 调拨两侧流水金额一致（OUT=IN）" \
    "$(PSQLQ -c "select (select amount from biz_inventory_txn where doc_id='$TR' and biz_type='TRANSFER_OUT')::numeric(20,4) = (select amount from biz_inventory_txn where doc_id='$TR' and biz_type='TRANSFER_IN')::numeric(20,4);")" "t"
  # FIX-4：期初桶 == 收货桶（同一 location_row_id）
  check "FIX-4 期初桶 = 入库落桶（同一个 location_row_id）" \
    "$(PSQLQ -c "select (select location_row_id from biz_opening_inventory where period_code='$PERIOD' and material_id='$MAT1') = (select location_id from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_A' and location_id='$BUCKET_A');")" "t"
  # FIX-7：销售出库 biz_type（用库里既有 E2E-02 销售出库单做回归）
  NSALE=$(PSQLQ -c "select count(*) from biz_stock_out where source_type='SALES' and out_type='OTHER' and status='POSTED';")
  ok "库内销售出库单 $NSALE 张（FIX-7 判据：全部 SALE_OUT，见 step10 ⑦）"
  summary
}

case "${1:-all}" in
  all) step9; step10; step11 ;;
  step9|step10|step11) "$1" ;;
esac
