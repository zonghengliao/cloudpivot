#!/usr/bin/env bash
# E2E-02 销售域演练 C 段：⑩ 销售退货 · ⑬ 币种/汇率 · ⑪ 销售目标 · ⑫ 佣金 · 结论勾稽
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e02_sales_common.sh"

step10() {
  say "== step10 ⑩ 销售退货（可退明细 → 建单 → 附件 → 提交 → 审批 → 过账：库存回冲+红字应收）"
  api s10_retble GET "/business/erp/sales-return/returnable/$SHIP1" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "可退明细 $(data '.data | length' 2>/dev/null) 行" || bad "可退明细 http=$HTTP $(show)"
  say "  $(data '.data[]? | .so_item_id[0:8]+" 已发="+.shipped+" 已退="+.returned+" 可退="+.remaining' 2>/dev/null | tr '\n' ' ')"
  SOI1=$(PSQLQ -c "select id from biz_sales_order_item where so_id='$SO1' order by line_no limit 1;")
  save SOI1 "$SOI1"
  api s10_over_qty POST /business/erp/sales-return "$T_ADMIN" "{\"shipment_id\":\"$SHIP1\",\"warehouse_id\":\"$WH_ID\",\"return_type\":\"QUALITY\",\"remark\":\"E2E-02 防超退验证\",\"items\":[{\"so_item_id\":\"$SOI1\",\"qty\":20}]}"
  if [ "$CODE" != 0 ]; then ok "防超退拦截（退 20 > 已发 10）http=$HTTP $(show)"; else bad "防超退失效：退 20 建单成功"; fi
  RET_PRE=$(PSQLQ -c "select coalesce((select status from biz_sales_return where remark='E2E-02 退货单甲（退 2 件）' order by created_at desc limit 1),'-');")
  if [ "$RET_PRE" != "-" ] && [ "$RET_PRE" != "CANCELLED" ]; then
    RET1=$(PSQLQ -c "select id from biz_sales_return where remark='E2E-02 退货单甲（退 2 件）' order by created_at desc limit 1;")
    save RET1 "$RET1"; say "  复用已存在退货单 $RET1（状态 $RET_PRE）"
  else
  api s10_create POST /business/erp/sales-return "$T_ADMIN" "{\"shipment_id\":\"$SHIP1\",\"warehouse_id\":\"$WH_ID\",\"return_type\":\"QUALITY\",\"remark\":\"E2E-02 退货单甲（退 2 件）\",\"items\":[{\"so_item_id\":\"$SOI1\",\"qty\":2}]}"
  [ "$CODE" = 0 ] || { bad "建退货单 http=$HTTP $(show)"; summary; return; }
  RET1=$(data '.data.id // empty')
  [ -z "$RET1" ] && RET1=$(PSQLQ -c "select id from biz_sales_return where remark='E2E-02 退货单甲（退 2 件）' order by created_at desc limit 1;")
  save RET1 "$RET1"
  ok "建退货单 $(data '.data.return_no // "?"') id=$RET1 金额=$(data '.data.total_amount') 税=$(data '.data.total_tax') 价税=$(data '.data.total_with_tax')"
  check "退货金额 = 2×(100)+税" "$(PSQLQ -c "select total_amount||'/'||total_tax||'/'||total_with_tax from biz_sales_return where id='$RET1';")" "200.0000/26.0000/226.0000"
  fi
  RST=$(PSQLQ -c "select status from biz_sales_return where id='$RET1';")
  if [ "$RST" = "DRAFT" ]; then
    api s10_submit_noatt PATCH "/business/erp/sales-return/$RET1/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
    if [ "$CODE" != 0 ]; then ok "无附件提交被拦 http=$HTTP $(show)"; else bad "无附件竟然能提交"; fi
    local pdf=$(mkpdf)
    attach_upload s10_attach SALES_RETURN "$RET1" "$pdf" application/pdf
    [ "$CODE" = 0 ] && ok "上传退货单附件" || bad "上传附件 http=$HTTP $(show)"
    api s10_submit PATCH "/business/erp/sales-return/$RET1/status" "$T_ADMIN" '{"target_status":"SUBMITTED"}'
    [ "$CODE" = 0 ] && ok "→ SUBMITTED" || bad "→SUBMITTED http=$HTTP $(show)"
  fi
  if [ "$RST" = "DRAFT" ] || [ "$RST" = "SUBMITTED" ]; then
    api s10_skip PATCH "/business/erp/sales-return/$RET1/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
    if [ "$CODE" != 0 ]; then ok "未审批直接跳 PENDING_POST 被拦 http=$HTTP（状态机 409）"; else bad "跳过审批未被拦"; fi
    api s10_approve PATCH "/business/erp/sales-return/$RET1/status" "$T_ADMIN" '{"target_status":"APPROVED"}'
    [ "$CODE" = 0 ] && ok "→ APPROVED" || bad "→APPROVED http=$HTTP $(show)"
  fi
  RST=$(PSQLQ -c "select status from biz_sales_return where id='$RET1';")
  say "  · 说明：sales-return:status 权限码未授予任何业务角色（仅 SUPER_ADMIN），故状态推进用超管；见问题清单 P2"
  check "库存(过账前仍 190)" "$(PSQLQ -c "select sum(qty)::numeric(20,4)::text from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_ID';")" "190.0000"
  if [ "$RST" = "APPROVED" ]; then
    api s10_pending PATCH "/business/erp/sales-return/$RET1/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
    [ "$CODE" = 0 ] && ok "→ PENDING_POST" || bad "→PENDING_POST http=$HTTP $(show)"
  fi
  RST=$(PSQLQ -c "select status from biz_sales_return where id='$RET1';")
  if [ "$RST" = "PENDING_POST" ]; then
    api s10_posted PATCH "/business/erp/sales-return/$RET1/status" "$T_ADMIN" '{"target_status":"POSTED"}'
    [ "$CODE" = 0 ] && ok "→ POSTED（回库 + 红字应收 + 冲减凭证）" || bad "→POSTED http=$HTTP $(show)"
  fi
  check "退货单 POSTED" "$(PSQLQ -c "select status from biz_sales_return where id='$RET1';")" "POSTED"
  check "库存回冲 190→192" "$(PSQLQ -c "select sum(qty)::numeric(20,4)::text from biz_inventory where material_id='$MAT1' and warehouse_id='$WH_ID';")" "192.0000"
  check "应收金额 1582→1356（红字 226）" "$(PSQLQ -c "select amount from biz_receivable where id='$AR1';")" "1356.0000"
  check "应收未收 782→556" "$(PSQLQ -c "select unsettled_amount||'/'||status from biz_receivable where id='$AR1';")" "556.0000/PARTIAL"
  check "订单行已发回写 10→8" "$(PSQLQ -c "select delivered_qty::numeric(20,4)::text from biz_sales_order_item where id='$SOI1';")" "8.0000"
  say "  红字发票: $(PSQLQ -c "select invoice_no||' type='||invoice_type||' total='||total_with_tax from biz_invoice where receivable_id='$AR1' and invoice_type='OUT_RED' order by created_at desc limit 1;" | tr '\n' ' ')"
  say "  冲减凭证: $(PSQLQ -c "select string_agg(voucher_no||' '||summary,' | ') from biz_voucher where summary like '%'||(select return_no from biz_sales_return where id='$RET1')||'%';" | tr '\n' ' ')"
  say "  退货流水: $(PSQLQ -c "select txn_type||'/'||biz_type||'/'||qty from biz_inventory_txn where material_id='$MAT1' and warehouse_id='$WH_ID' order by created_at desc limit 1;")"
  summary
}

step13() {
  say "== step13 ⑬ 币种/汇率（外币订单 → 本位币折算 → 汇率变动影响）"
  api s13_rate1 POST /business/erp/exchange-rate "$T_ADMIN" '{"from_currency":"USD","rate":"7.10000000","rate_type":"SPOT","effective_date":"2026-11-01","remark":"E2E-02 演练汇率 11-01"}'
  if [ "$CODE" = 0 ]; then ok "建 USD 汇率 7.10 @2026-11-01"; else
    api s13_rate1b PUT "/business/erp/exchange-rate/$(PSQLQ -c "select id from biz_exchange_rate where from_currency='USD' and effective_date='2026-11-01' and rate_type='SPOT' limit 1;")" "$T_ADMIN" '{"rate":"7.10000000"}'
    say "  （已存在，改为 7.10）http=$HTTP $(show)"; fi
  api s13_rate2 POST /business/erp/exchange-rate "$T_ADMIN" '{"from_currency":"USD","rate":"7.20000000","rate_type":"SPOT","effective_date":"2026-11-15","remark":"E2E-02 演练汇率 11-15（变动）"}'
  if [ "$CODE" = 0 ]; then ok "建 USD 汇率 7.20 @2026-11-15（变动）"; else
    api s13_rate2b PUT "/business/erp/exchange-rate/$(PSQLQ -c "select id from biz_exchange_rate where from_currency='USD' and effective_date='2026-11-15' and rate_type='SPOT' limit 1;")" "$T_ADMIN" '{"rate":"7.20000000"}'
    say "  （11-15 已存在，改为 7.20）http=$HTTP $(show)"; fi
  api s13_quote GET "/business/erp/exchange-rate/quote?from_currency=USD&date=2026-11-09" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "汇率报价接口 2026-11-09 → $(data '.data.rate // .data | tostring' | head -c 30)" || bad "汇率报价 http=$HTTP $(show)"
  # 外币订单 1（2026-11-09 → 锁 7.10）
  api s13_so2 POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"salesperson_id\":\"$SP_ID\",\"order_date\":\"2026-11-09\",\"currency_code\":\"USD\",\"remark\":\"E2E-02 美元订单甲\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":20,\"price\":10,\"tax_rate\":13}]}"
  [ "$CODE" = 0 ] || bad "建美元订单 http=$HTTP $(show)"
  SO2=$(data '.data.id // empty')
  [ -z "$SO2" ] && SO2=$(PSQLQ -c "select id from biz_sales_order where remark='E2E-02 美元订单甲' and deleted_at is null order by created_at desc limit 1;")
  save SO2 "$SO2"
  ok "美元订单 $(data '.data.so_no // "?"') 原币合计=$(data '.data.total_with_tax') 汇率=$(data '.data.exchange_rate') 本位币合计=$(data '.data.base_total_with_tax')"
  check "汇率锁定 7.10 + 本位币 1604.60" "$(PSQLQ -c "select exchange_rate||'/'||base_total_with_tax from biz_sales_order where id='$SO2';")" "7.10000000/1604.6000"
  check "本位币恒等式 base_total=base_amount+base_tax" "$(PSQLQ -c "select (base_total_with_tax=base_total_amount+base_total_tax)::text from biz_sales_order where id='$SO2';")" "true"
  # 外币订单 2（2026-11-20 → 锁 7.20；仅建单，验证汇率按单据日期取）
  api s13_so3 POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"salesperson_id\":\"$SP_ID\",\"order_date\":\"2026-11-20\",\"currency_code\":\"USD\",\"remark\":\"E2E-02 美元订单乙（汇率变动）\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":10,\"price\":10,\"tax_rate\":13}]}"
  [ "$CODE" = 0 ] || bad "建美元订单乙 http=$HTTP $(show)"
  SO3=$(data '.data.id // empty')
  [ -z "$SO3" ] && SO3=$(PSQLQ -c "select id from biz_sales_order where remark='E2E-02 美元订单乙（汇率变动）' and deleted_at is null order by created_at desc limit 1;")
  save SO3 "$SO3"
  check "订单乙锁 7.20" "$(PSQLQ -c "select exchange_rate from biz_sales_order where id='$SO3';")" "7.20000000"
  check "订单甲汇率不漂移（仍 7.10）" "$(PSQLQ -c "select exchange_rate from biz_sales_order where id='$SO2';")" "7.10000000"
  # 无汇率的币种 → 400（不默认 1）
  api s13_eur POST /business/erp/sales-order "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"order_date\":\"2026-11-09\",\"currency_code\":\"EUR\",\"items\":[{\"material_id\":\"$MAT1\",\"qty\":1,\"price\":10,\"tax_rate\":13}]}"
  if [ "$CODE" != 0 ]; then ok "EUR 无牌价 → 拒绝建单 http=$HTTP $(show)"; else bad "EUR 无牌价竟然建单成功"; fi
  # 美元订单走完发货 → 应收继承币种与汇率
  api s13_submit POST "/business/erp/sales-order/$SO2/submit-approval" "$T_ADMIN" '{"reason":"E2E-02 美元订单提交"}'
  IID=$(PSQLQ -c "select approval_instance_id from biz_sales_order where id='$SO2';")
  approve_instance "$T_APP" "$IID" s13_ap
  check "美元订单审批后状态" "$(PSQLQ -c "select status from biz_sales_order where id='$SO2';")" "APPROVED"
  api s13_ship POST /business/erp/shipment "$T_ADMIN" "{\"so_id\":\"$SO2\",\"warehouse_id\":\"$WH_ID\",\"ship_date\":\"2026-11-09\",\"remark\":\"E2E-02 美元发货单\",\"items\":[]}"
  SHIP2=$(data '.data.id // empty')
  [ -z "$SHIP2" ] && SHIP2=$(PSQLQ -c "select id from biz_shipment where remark='E2E-02 美元发货单' order by created_at desc limit 1;")
  save SHIP2 "$SHIP2"
  api s13_pending PATCH "/business/erp/shipment/$SHIP2/status" "$T_ADMIN" '{"target_status":"PENDING_POST"}'
  api s13_posted PATCH "/business/erp/shipment/$SHIP2/status" "$T_ADMIN" '{"target_status":"POSTED"}'
  [ "$CODE" = 0 ] && ok "美元发货单 POSTED" || bad "美元发货过账 http=$HTTP $(show)"
  AR2=$(PSQLQ -c "select id from biz_receivable where source_type='SALES_OUT' and source_id=(select id from biz_stock_out where source_type='SALES' and source_id='$SHIP2' and deleted_at is null) and deleted_at is null;")
  save AR2 "$AR2"
  SOUT2=$(PSQLQ -c "select id from biz_stock_out where source_type='SALES' and source_id='$SHIP2' and deleted_at is null;")
  save SOUT2 "$SOUT2"
  check "应收继承币种+汇率" "$(PSQLQ -c "select currency_code||'/'||exchange_rate||'/'||base_total_with_tax from biz_receivable where id='$AR2';")" "USD/7.10000000/1604.6000"
  check "美元应收原币 226.00" "$(PSQLQ -c "select amount from biz_receivable where id='$AR2';")" "226.0000"
  # 从美元应收开票：币种继承、汇率按开票日重锁（11-16 → 7.20）
  api s13_inv POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR2\",\"invoice_date\":\"2026-11-16\",\"is_electronic\":true,\"total_with_tax\":226,\"tax_rate\":13,\"remark\":\"E2E-02 美元电子票\"}"
  [ "$CODE" = 0 ] || bad "美元开票 http=$HTTP $(show)"
  INV4=$(data '.data.id // empty')
  [ -z "$INV4" ] && INV4=$(PSQLQ -c "select id from biz_invoice where remark='E2E-02 美元电子票' order by created_at desc limit 1;")
  save INV4 "$INV4"
  local pdf=$(mkpdf)
  attach_upload s13_inv_att INVOICE "$INV4" "$pdf" application/pdf
  api s13_inv_issue PATCH "/business/erp/invoice/$INV4/status" "$T_ADMIN" '{"status":"ISSUED"}'
  [ "$CODE" = 0 ] && ok "美元发票 ISSUED" || bad "美元发票 ISSUED http=$HTTP $(show)"
  say "  发票币种/汇率/本位币: $(PSQLQ -c "select coalesce(currency_code,'-')||'/'||coalesce(exchange_rate::text,'-')||'/'||base_total_with_tax from biz_invoice where id='$INV4';")"
  check "发票按开票日重锁 7.20（汇率变动 +22.60）" "$(PSQLQ -c "select exchange_rate||'/'||(base_total_with_tax - 1604.6) from biz_invoice where id='$INV4';")" "7.20000000/22.6000"
  summary
}

step11() {
  say "== step11 ⑪ 销售目标（目标设定 → 达成率：订单口径 / 收款口径）"
  api s11_t1 POST /business/erp/sales-target "$T_ADMIN" "{\"scope_type\":\"SALESPERSON\",\"scope_id\":\"$SP_ID\",\"period_code\":\"2026-11\",\"target_amount\":5000,\"target_qty\":0,\"remark\":\"E2E-02 销售员目标\"}"
  if [ "$CODE" = 0 ]; then TGT1=$(data '.data.id // empty'); ok "销售员目标 $(data '.data.target_no') 5000（范围=$(data '.data.scope_name')）";
  else TGT1=$(PSQLQ -c "select id from biz_sales_target where period_code='2026-11' and scope_type='SALESPERSON' and scope_id='$SP_ID' and deleted_at is null limit 1;"); say "  · 销售员目标已存在，复用 $TGT1"; fi
  save TGT1 "$TGT1"
  api s11_t2 POST /business/erp/sales-target "$T_ADMIN" '{"scope_type":"ALL","scope_id":null,"period_code":"2026-11","target_amount":20000,"remark":"E2E-02 全公司目标"}'
  if [ "$CODE" = 0 ]; then TGT2=$(data '.data.id // empty'); ok "全公司目标 20000";
  else TGT2=$(PSQLQ -c "select id from biz_sales_target where period_code='2026-11' and scope_type='ALL' and deleted_at is null limit 1;"); say "  · 全公司目标已存在，复用 $TGT2"; fi
  save TGT2 "$TGT2"
  api s11_dup POST /business/erp/sales-target "$T_ADMIN" "{\"scope_type\":\"SALESPERSON\",\"scope_id\":\"$SP_ID\",\"period_code\":\"2026-11\",\"target_amount\":1}"
  if [ "$CODE" != 0 ]; then ok "同范围同期间重复目标被拒 http=$HTTP $(show)"; else bad "重复目标竟然成功"; fi
  for basis in ORDER RECEIPT; do
    api "s11_ach_$basis" GET "/business/erp/sales-target/achievement?period_code=2026-11&scope_type=SALESPERSON&basis=$basis" "$T_ADMIN"
    [ "$CODE" = 0 ] || { bad "达成率($basis) http=$HTTP $(show)"; continue; }
    say "  [$basis] $(data ".data[]? | select(.scope==\"E2E02 销售员甲\") | \"目标=\"+.target_amount+\" 实际=\"+.actual_amount+\" 达成率=\"+.achievement_rate+\"% 缺口=\"+.gap")"
  done
  local ord=$(PSQLQ -c "select coalesce(sum(total_with_tax),0)::numeric(20,2)::text from biz_sales_order where salesperson_id='$SP_ID' and deleted_at is null and status in ('APPROVED','PARTIAL_OUT','OUTBOUND','CLOSED');")
  api s11_ach_order2 GET "/business/erp/sales-target/achievement?period_code=2026-11&scope_type=SALESPERSON&basis=ORDER" "$T_ADMIN"
  check "订单口径实际 = Σ订单价税合计（原币，含美元单 $ord）" "$(data ".data[]? | select(.scope==\"E2E02 销售员甲\") | .actual_amount" 2>/dev/null)" "1808.00"
  api s11_ach_r GET "/business/erp/sales-target/achievement?period_code=2026-11&scope_type=SALESPERSON&basis=RECEIPT" "$T_ADMIN"
  check "收款口径实际 = Σ应收已收(800)" "$(data ".data[]? | select(.scope==\"E2E02 销售员甲\") | .actual_amount" 2>/dev/null)" "800.00"
  say "  （DB Σ应收已收 = $(PSQLQ -c "select coalesce(sum(received_amount),0) from biz_receivable where customer_id='$CUST1' and deleted_at is null;")）"
  api s11_page GET "/business/erp/sales-target/page?page=1&page_size=10&period_code=2026-11" "$T_ADMIN"
  ok "目标列表 total=$(data '.data.total')"
  summary
}

step12() {
  say "== step12 ⑫ 佣金规则 + 佣金结算（按已收款计提 → 审批 → 发放）"
  api s12_r1 POST /business/erp/commission-rule "$T_ADMIN" '{"rule_name":"E2E-02 全局 5%","scope_type":"ALL","scope_id":null,"calc_type":"PERCENT","rate":"5","min_amount":"0","max_amount":null,"effective_from":"2026-01-01","status":"ACTIVE","remark":"E2E-02 兜底规则"}'
  [ "$CODE" = 0 ] || bad "建全局规则 http=$HTTP $(show)"
  R_ALL=$(data '.data.id // empty'); save R_ALL "$R_ALL"; ok "全局 5% 规则"
  api s12_r2 POST /business/erp/commission-rule "$T_ADMIN" "{\"rule_name\":\"E2E-02 销售员 12%\",\"scope_type\":\"SALESPERSON\",\"scope_id\":\"$SP_ID\",\"calc_type\":\"PERCENT\",\"rate\":\"12\",\"min_amount\":\"0\",\"max_amount\":null,\"effective_from\":\"2026-01-01\",\"status\":\"ACTIVE\",\"remark\":\"E2E-02 销售员规则（应压过全局）\"}"
  [ "$CODE" = 0 ] || bad "建销售员规则 http=$HTTP $(show)"
  R_SP=$(data '.data.id // empty'); save R_SP "$R_SP"
  ok "销售员 12% 规则（scope_name=$(data '.data.scope_name')）"
  api s12_page GET "/business/erp/commission-rule/page?page=1&page_size=10" "$T_ADMIN"
  ok "佣金规则列表 total=$(data '.data.total')"
  api s12_settle POST /business/erp/commission/settle "$T_ADMIN" "{\"period_code\":\"2026-11\",\"salesperson_id\":\"$SP_ID\",\"remark\":\"E2E-02 佣金计提\"}"
  [ "$CODE" = 0 ] || { bad "计提 http=$HTTP $(show)"; summary; return; }
  SET1=$(data '.data.settlements[0].id // empty')
  [ -z "$SET1" ] && SET1=$(PSQLQ -c "select id from biz_commission_settlement where period_code='2026-11' and salesperson_id='$SP_ID' order by created_at desc limit 1;")
  save SET1 "$SET1"
  ok "结算单 $(data '.data.settlements[0].settlement_no // "?"') 佣金合计=$(data '.data.settlements[0].total_commission')"
  check "佣金 = 已收 800 × 12% = 96" "$(PSQLQ -c "select total_commission from biz_commission_settlement where id='$SET1';")" "96.0000"
  check "计提基数 = 应收已收合计" "$(PSQLQ -c "select total_sales from biz_commission_settlement where id='$SET1';")" "800.0000"
  say "  明细: $(PSQLQ -c "select string_agg(i.base_amount||'×'||i.rate||'='||i.commission||' 规则:'||coalesce(r.rule_name,'-'),' | ') from biz_commission_item i left join biz_commission_rule r on r.id=i.rule_id where i.settlement_id='$SET1';")"
  say "  明细关联订单: $(PSQLQ -c "select coalesce(string_agg(distinct o.so_no,','),'-') from biz_commission_item i join biz_sales_order o on o.id=i.so_id where i.settlement_id='$SET1';")"
  api s12_dup POST /business/erp/commission/settle "$T_ADMIN" "{\"period_code\":\"2026-11\",\"salesperson_id\":\"$SP_ID\",\"remark\":\"E2E-02 重算幂等\"}"
  say "  重算 http=$HTTP $(show) $(data '.data.skipped // .data | tostring' | head -c 160)"
  # 重算会软删旧草稿并新建一张 → 重新取当前存活结算单
  SET1=$(PSQLQ -c "select id from biz_commission_settlement where period_code='2026-11' and salesperson_id='$SP_ID' and deleted_at is null order by created_at desc limit 1;")
  save SET1 "$SET1"
  check "重算后佣金仍 96（不重复发钱）" "$(PSQLQ -c "select total_commission from biz_commission_settlement where id='$SET1';")" "96.0000"
  api s12_approve PATCH "/business/erp/commission/$SET1/status" "$T_ADMIN" '{"status":"APPROVED","reason":"E2E-02 佣金审批"}'
  [ "$CODE" = 0 ] && ok "→ APPROVED（计提凭证 $(data '.data.voucher_no // "?"')）" || bad "→APPROVED http=$HTTP $(show)"
  say "  计提凭证分录: $(PSQLQ -c "select string_agg(account_code||':'||debit_amount::numeric(20,2)||'/'||credit_amount::numeric(20,2),',' order by account_code) from biz_voucher_entry where voucher_id=(select voucher_id from biz_commission_settlement where id='$SET1');" 2>/dev/null)"
  api s12_pay PATCH "/business/erp/commission/$SET1/status" "$T_ADMIN" '{"status":"PAID","reason":"E2E-02 银行转账发放"}'
  [ "$CODE" = 0 ] && ok "→ PAID（发放凭证 $(data '.data.voucher_no // "?"')）" || bad "→PAID http=$HTTP $(show)"
  api s12_bad PATCH "/business/erp/commission/$SET1/status" "$T_ADMIN" '{"status":"DRAFT"}'
  if [ "$CODE" != 0 ]; then ok "PAID 后回退 DRAFT 被拦 http=$HTTP"; else bad "PAID 竟然能回退 DRAFT"; fi
  summary
}

step99() {
  say "== step99 结论勾稽（订单/应收/发票/库存/凭证/目标/佣金）"
  local ARS="'$AR1','$AR2'"
  say "-- ① 订单主表 = Σ明细（不含税 / 税 / 价税）"
  PSQLQ -c "select o.so_no||' 主表='||o.total_amount||'/'||o.total_tax||'/'||o.total_with_tax||' 明细='||sum(i.amount)||'/'||sum(i.tax_amount)||'/'||sum(i.amount+i.tax_amount)||' 一致='||(o.total_amount=sum(i.amount) and o.total_tax=sum(i.tax_amount) and o.total_with_tax=sum(i.amount+i.tax_amount))::text
              from biz_sales_order o join biz_sales_order_item i on i.so_id=o.id
             where o.id in ('$SO1','$SO2','$SO3') group by o.id,o.so_no,o.total_amount,o.total_tax,o.total_with_tax order by o.so_no;" | sed 's/^/  /'
  local bad1=$(PSQLQ -c "select count(*) from (select o.id from biz_sales_order o join biz_sales_order_item i on i.so_id=o.id where o.id in ('$SO1','$SO2','$SO3') group by o.id,o.total_amount,o.total_tax,o.total_with_tax having not (o.total_amount=sum(i.amount) and o.total_tax=sum(i.tax_amount) and o.total_with_tax=sum(i.amount+i.tax_amount))) t;")
  check "订单主表↔明细不一致行" "$bad1" "0"
  say "-- ② 应收恒等式 amount = received + unsettled（本卡两张应收）"
  PSQLQ -c "select ar_no||' 原币 '||amount||' = '||received_amount||' + '||unsettled_amount||' → '||(amount=received_amount+unsettled_amount)::text||' 本位币 '||base_total_with_tax
               from biz_receivable where id in ($ARS) order by ar_no;" | sed 's/^/  /'
  check "应收恒等式违反行" "$(PSQLQ -c "select count(*) from biz_receivable where id in ($ARS) and amount <> received_amount+unsettled_amount;")" "0"
  say "-- ③ 应收金额 ↔ 订单价税合计（含退货冲减）"
  PSQLQ -c "select r.ar_no||' 应收='||r.amount||' 订单='||o.total_with_tax||' 退货红字='||coalesce(ret.red,0)||' 一致='||(r.amount = o.total_with_tax + coalesce(ret.red,0))::text||' 币种='||r.currency_code
               from biz_receivable r join biz_stock_out so on so.id=r.source_id join biz_shipment sh on sh.id=so.source_id join biz_sales_order o on o.id=sh.so_id
               left join (select receivable_id, sum(total_with_tax) red from biz_invoice where invoice_type='OUT_RED' group by 1) ret on ret.receivable_id=r.id
              where r.id in ($ARS) order by r.ar_no;" | sed 's/^/  /'
  check "应收↔订单不一致行" "$(PSQLQ -c "select count(*) from biz_receivable r join biz_stock_out so on so.id=r.source_id join biz_shipment sh on sh.id=so.source_id join biz_sales_order o on o.id=sh.so_id left join (select receivable_id, sum(total_with_tax) red from biz_invoice where invoice_type='OUT_RED' group by 1) ret on ret.receivable_id=r.id where r.id in ($ARS) and r.amount <> o.total_with_tax + coalesce(ret.red,0);")" "0"
  say "-- ④ 发票：amount + tax = total，且开票额 ≤ 应收额"
  PSQLQ -c "select invoice_no||' '||coalesce(invoice_type,'-')||' '||status||' 不含税='||amount||' 税='||tax_amount||' 价税='||total_with_tax||' 恒等='||(amount+tax_amount=total_with_tax)::text
               from biz_invoice where id in ('$INV1','$INV2','$INV3','$INV4') order by invoice_no;" | sed 's/^/  /'
  check "发票价税恒等式违反行" "$(PSQLQ -c "select count(*) from biz_invoice where id in ('$INV1','$INV2','$INV3','$INV4') and amount+tax_amount <> total_with_tax;")" "0"
  check "发票有效开票额 ≤ 应收额(AR1)" "$(PSQLQ -c "select (coalesce(sum(total_with_tax),0) <= (select amount from biz_receivable where id='$AR1'))::text from biz_invoice where receivable_id='$AR1' and status in ('ISSUED','POSTED');")" "true"
  say "-- ⑤ 库存 = 流水累计（本仓）"
  local diff=$(PSQLQ -c "with inv as (select material_id, warehouse_id, sum(qty) qty from biz_inventory where warehouse_id='$WH_ID' group by 1,2),
                 txn as (select material_id, warehouse_id, sum(case when txn_type='IN' then qty else -qty end) qty from biz_inventory_txn where warehouse_id='$WH_ID' group by 1,2)
            select count(*) from inv full join txn using (material_id, warehouse_id) where coalesce(inv.qty,0)<>coalesce(txn.qty,0);")
  check "库存↔流水差异行" "$diff" "0"
  PSQLQ -c "select m.material_code||' 库存='||coalesce(sum(i.qty),0) from biz_material m left join biz_inventory i on i.material_id=m.id and i.warehouse_id='$WH_ID' where m.id in ('$MAT1','$MAT2') group by m.material_code order by 1;" | sed 's/^/  /'
  say "-- ⑥ 凭证借贷平衡（本卡全部单据）"
  local vbad=$(PSQLQ -c "select count(*) from (
        select v.id, sum(e.debit_amount) d, sum(e.credit_amount) c from biz_voucher v join biz_voucher_entry e on e.voucher_id=v.id
         where v.source_id in ('$IN_ID','$SOUT1','$SOUT2','$SHIP1','$SHIP2','$RC1','$RF1','$INV1','$INV2','$INV4','$RET1','$SET1') group by v.id
        having sum(e.debit_amount) <> sum(e.credit_amount)) t;")
  check "借贷不平凭证数" "$vbad" "0"
  say "  凭证张数: $(PSQLQ -c "select count(*) from biz_voucher where source_id in ('$IN_ID','$SOUT1','$SOUT2','$SHIP1','$SHIP2','$RC1','$RF1','$INV1','$INV2','$INV4','$RET1','$SET1');")"
  say "-- ⑦ 科目净额（本位币）"
  PSQLQ -c "select e.account_code||' 借='||sum(e.debit_amount)::numeric(20,2)||' 贷='||sum(e.credit_amount)::numeric(20,2)||' 净='||(sum(e.debit_amount)-sum(e.credit_amount))::numeric(20,2)
               from biz_voucher_entry e where e.voucher_id in (select id from biz_voucher where source_id in ('$IN_ID','$SOUT1','$SOUT2','$SHIP1','$SHIP2','$RC1','$RF1','$INV1','$INV2','$INV4','$RET1','$SET1') or source_id in (select id from biz_invoice where receivable_id in ($ARS)))
              group by e.account_code order by 1;" | sed 's/^/  /'
  say "-- ⑧ 1122 净额 = 有效开票(本位币) − 已收 + 退款 − 退货红字"
  local net1122=$(PSQLQ -c "select (sum(debit_amount)-sum(credit_amount))::numeric(20,2) from biz_voucher_entry where account_code='1122' and voucher_id in (select id from biz_voucher where source_id in ('$RC1','$RF1','$INV1','$INV2','$INV3','$INV4') or source_id in (select id from biz_invoice where receivable_id in ($ARS)));")
  say "  1122净额=$net1122（口径：782 纸质票 + 1627.20 美元票 − 1000 收款 + 200 退款 − 226 退货红字 = 1383.20）"
  check "1122 净额与「已开票−已收+退款−退货红字」一致" "$net1122" "1383.20"
  say "  说明：1122 ≠ Σ应收未收(2160.60)，差 777.40 = 未开票部分 556.00 + 美元票汇兑 22.60 + 已收款对应未开票部分（设计口径：开票才确认收入）"
  say "-- ⑨ 销售目标达成率 vs 库层口径"
  PSQLQ -c "select '订单口径(原币合计)='||coalesce(sum(total_with_tax),0)||' 收款口径='||(select coalesce(sum(received_amount),0) from biz_receivable where customer_id='$CUST1' and deleted_at is null) from biz_sales_order where salesperson_id='$SP_ID' and deleted_at is null and status in ('APPROVED','PARTIAL_OUT','OUTBOUND','CLOSED');" | sed 's/^/  /'
  say "-- ⑩ 佣金基数 = 应收已收（不含未收款订单）"
  PSQLQ -c "select '基数='||coalesce(sum(base_amount),0)||' 佣金='||coalesce(sum(commission),0) from biz_commission_item where settlement_id='$SET1';" | sed 's/^/  /'
  summary
}

case "${1:-}" in
  step10) step10 ;; step11) step11 ;; step12) step12 ;; step13) step13 ;; step99) step99 ;;
  *) echo "用法: $0 {step10|step11|step12|step13|step99}"; exit 2 ;;
esac
