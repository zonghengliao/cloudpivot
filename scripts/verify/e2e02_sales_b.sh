#!/usr/bin/env bash
# E2E-02 销售域演练 B 段：⑤ 出库勾稽 · ⑥ 应收台账/账龄 · ⑦ 收款核销 · ⑧ 收款退款 · ⑨ 发票/红冲
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e02_sales_common.sh"

step5() {
  say "== step5 ⑤ 出库单（扣库存 → 过账 → 成本结转）勾稽"
  local st=$(PSQLQ -c "select status from biz_stock_out where id='$SOUT1';")
  check "出库单状态" "$st" "POSTED"
  check "出库明细 2 行" "$(PSQLQ -c "select count(*) from biz_stock_out_item where stock_out_id='$SOUT1';")" "2"
  say "  出库明细: $(PSQLQ -c "select string_agg(m.material_code||':'||i.qty||'@'||i.unit_cost,',') from biz_stock_out_item i join biz_material m on m.id=i.material_id where i.stock_out_id='$SOUT1';")"
  check "出库成本合计 = Σ(qty×unit_cost)" "$(PSQLQ -c "select (total_amount = (select sum(qty*unit_cost) from biz_stock_out_item where stock_out_id='$SOUT1'))::text from biz_stock_out where id='$SOUT1';")" "true"
  say "  154 成本凭证分录: $(PSQLQ -c "select string_agg(e.account_code||':'||e.debit_amount||'/'||e.credit_amount,',') from biz_voucher_entry e where e.voucher_id=(select voucher_id from biz_stock_out where id='$SOUT1');" 2>/dev/null)"
  api s5_detail GET "/business/erp/stock-out/$SOUT1" "$T_ADMIN"
  ok "出库详情 out_no=$(data '.data.out_no') source=$(data '.data.source_type') 金额=$(data '.data.total_amount')"
  api s5_page GET "/business/erp/stock-out/page?page=1&page_size=10&out_type=OTHER" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "出库列表(OTHER) total=$(data '.data.total')" || bad "出库列表 http=$HTTP $(show)"
  summary
}

step6() {
  say "== step6 ⑥ 应收台账（自动挂账 → 账龄）"
  AR1=$(PSQLQ -c "select id from biz_receivable where source_type='SALES_OUT' and source_id='$SOUT1' and deleted_at is null;")
  save AR1 "$AR1"
  say "  应收: $(PSQLQ -c "select ar_no||' amt='||amount||' recv='||received_amount||' unpaid='||unsettled_amount||' st='||status||' due='||due_date from biz_receivable where id='$AR1';")"
  check "应收金额 = 订单价税合计" "$(PSQLQ -c "select (r.amount = o.total_with_tax)::text from biz_receivable r join biz_sales_order o on o.id='$SO1' where r.id='$AR1';")" "true"
  check "应收初始状态 OPEN" "$(PSQLQ -c "select status from biz_receivable where id='$AR1';")" "OPEN"
  check "应收账期 = 客户 credit_days(30) / 到期日" "$(PSQLQ -c "select credit_days||'/'||due_date from biz_receivable where id='$AR1';")" "30/2026-12-09"
  api s6_page GET "/business/erp/receivable/page?page=1&page_size=10&ar_no=$(PSQLQ -c "select ar_no from biz_receivable where id='$AR1';")" "$T_ADMIN"
  check "应收分页命中" "$(data '.data.total')" "1"
  api s6_aging GET "/business/erp/receivable/aging?as_of=2026-11-30" "$T_ADMIN"
  [ "$CODE" = 0 ] && ok "账龄接口 200 客户数=$(data '.data.customers | length' 2>/dev/null)" || bad "账龄 http=$HTTP $(show)"
  say "  客户行: $(data ".data.customers[]? | select(.customer_name==\"E2E-02 演练客户甲（改名）\") | (.customer_name+\" docs=\"+(.doc_count|tostring)+\" unpaid=\"+.total_unsettled+\" maxoverdue=\"+(.max_overdue_days|tostring))" 2>/dev/null)"
  say "  档位: $(data ".data.customers[]? | select(.customer_name==\"E2E-02 演练客户甲（改名）\") | .buckets | map(.label+\":\"+.unpaid) | join(\" \")" 2>/dev/null)"
  check "账龄未收合计 = 应收未收" "$(PSQLQ -c "select (coalesce((select total_unsettled from (select sum(unsettled_amount) total_unsettled from biz_receivable where customer_id='$CUST1' and deleted_at is null and status in ('OPEN','PARTIAL')) x),0) = (select unsettled_amount from biz_receivable where id='$AR1'))::text;")" "true"
  summary
}

step7() {
  say "== step7 ⑦ 收款单（登记 → 核销 → 确认 → 过账）"
  api s7_create POST /business/erp/receipt "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"receipt_date\":\"2026-11-09\",\"amount\":1000,\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222020000000001\",\"remark\":\"E2E-02 收款甲\"}"
  [ "$CODE" = 0 ] || { bad "登记收款 http=$HTTP $(show)"; summary; return; }
  RC1=$(data '.data.id // empty')
  [ -z "$RC1" ] && RC1=$(PSQLQ -c "select id from biz_receipt where remark='E2E-02 收款甲' order by created_at desc limit 1;")
  save RC1 "$RC1"; ok "登记收款 $(data '.data.receipt_no // "?"') id=$RC1 金额=$(data '.data.amount') 状态=$(data '.data.status')"
  # 未核销就过账 → 409（口径：未核销余额必须为 0）
  api s7_post_early PATCH "/business/erp/receipt/$RC1/status" "$T_ADMIN" '{"status":"CONFIRMED"}'
  say "  CONFIRMED（未上传银行回单前）http=$HTTP $(show)"
  check "收款 DB 状态 DRAFT" "$(PSQLQ -c "select status from biz_receipt where id='$RC1';")" "DRAFT"
  # 银行回单附件（RECEIPT/SUBMIT 规则）
  local pdf=$(mkpdf)
  attach_upload s7_attach RECEIPT "$RC1" "$pdf" application/pdf
  [ "$CODE" = 0 ] && ok "上传银行回单附件" || bad "上传回单 http=$HTTP $(show)"
  api s7_alloc POST "/business/erp/receipt/$RC1/allocate" "$T_ADMIN" "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":1000,\"remark\":\"E2E-02 核销\"}]}"
  [ "$CODE" = 0 ] && ok "核销 1000 → 应收" || bad "核销 http=$HTTP $(show)"
  check "应收已收 1000 / 未收 582" "$(PSQLQ -c "select received_amount||'/'||unsettled_amount from biz_receivable where id='$AR1';")" "1000.0000/582.0000"
  check "应收状态 PARTIAL" "$(PSQLQ -c "select status from biz_receivable where id='$AR1';")" "PARTIAL"
  # 超核销 → 400/409
  api s7_over POST "/business/erp/receipt/$RC1/allocate" "$T_ADMIN" "{\"items\":[{\"receivable_id\":\"$AR1\",\"amount\":1200}]}"
  if [ "$CODE" != 0 ]; then ok "超核销被拦 http=$HTTP $(show)"; else bad "超核销竟然成功"; fi
  api s7_confirm PATCH "/business/erp/receipt/$RC1/status" "$T_ADMIN" '{"status":"CONFIRMED"}'
  [ "$CODE" = 0 ] && ok "→ CONFIRMED" || bad "→CONFIRMED http=$HTTP $(show)"
  api s7_posted PATCH "/business/erp/receipt/$RC1/status" "$T_ADMIN" '{"status":"POSTED"}'
  [ "$CODE" = 0 ] && ok "→ POSTED（生成收款凭证）" || bad "→POSTED http=$HTTP $(show)"
  check "收款状态 POSTED" "$(PSQLQ -c "select status from biz_receipt where id='$RC1';")" "POSTED"
  say "  收款凭证: $(PSQLQ -c "select voucher_no||' '||summary from biz_voucher where source_id='$RC1';")"
  say "  分录: $(PSQLQ -c "select string_agg(e.account_code||':'||e.debit_amount||'/'||e.credit_amount,',') from biz_voucher_entry e where e.voucher_id=(select voucher_id from biz_receipt where id='$RC1');")"
  check "收款凭证 借1002=贷1122=1000" "$(PSQLQ -c "select string_agg(account_code||':'||debit_amount::numeric(20,2)||'/'||credit_amount::numeric(20,2),',' order by account_code) from biz_voucher_entry where voucher_id=(select voucher_id from biz_receipt where id='$RC1');")" "1002:1000.00/0.00,1122:0.00/1000.00"
  summary
}

step8() {
  say "== step8 ⑧ 收款退款单（登记 → 冲减 → 确认 → 审批 → 过账）"
  api s8_create POST /business/erp/receipt-refund "$T_ADMIN" "{\"customer_id\":\"$CUST1\",\"source_receipt_id\":\"$RC1\",\"refund_date\":\"2026-11-09\",\"amount\":200,\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222020000000001\",\"reason\":\"E2E-02 演练部分退款\",\"remark\":\"E2E-02 退款单甲\"}"
  [ "$CODE" = 0 ] || { bad "建退款单 http=$HTTP $(show)"; summary; return; }
  RF1=$(data '.data.id // empty')
  [ -z "$RF1" ] && RF1=$(PSQLQ -c "select id from biz_receipt_refund where remark='E2E-02 退款单甲' order by created_at desc limit 1;")
  save RF1 "$RF1"; ok "建退款单 $(data '.data.refund_no // "?"') id=$RF1 状态=$(data '.data.status')"
  api s8_alloc POST "/business/erp/receipt-refund/$RF1/allocate" "$T_ADMIN" "{\"allocations\":[{\"receivable_id\":\"$AR1\",\"amount\":200,\"remark\":\"E2E-02 冲减\"}]}"
  [ "$CODE" = 0 ] && ok "冲减应收 200" || bad "冲减 http=$HTTP $(show)"
  check "应收已收 1000→800 / 未收 782" "$(PSQLQ -c "select received_amount||'/'||unsettled_amount from biz_receivable where id='$AR1';")" "800.0000/782.0000"
  api s8_confirm PATCH "/business/erp/receipt-refund/$RF1/status" "$T_ADMIN" '{"status":"CONFIRMED"}'
  [ "$CODE" = 0 ] && ok "→ CONFIRMED" || bad "→CONFIRMED http=$HTTP $(show)"
  api s8_submit POST "/business/erp/receipt-refund/$RF1/submit-approval" "$T_ADMIN" '{"reason":"E2E-02 退款审批"}'
  [ "$CODE" = 0 ] && ok "提交审批（实例 $(data '.data.instance_status // "?"')）" || bad "提交审批 http=$HTTP $(show)"
  IID=$(PSQLQ -c "select id from biz_flow_instance where biz_id='$RF1' and biz_type='RECEIPT_REFUND' order by created_at desc limit 1;")
  say "  审批实例 = $IID 状态=$(flow_state "$IID")"
  approve_instance "$T_APP" "$IID" s8_ap
  check "退款单审批后自动过账 POSTED" "$(PSQLQ -c "select status from biz_receipt_refund where id='$RF1';")" "POSTED"
  say "  退款凭证: $(PSQLQ -c "select voucher_no||' '||summary from biz_voucher where source_id='$RF1';")"
  check "退款凭证 借1122=贷1002=200" "$(PSQLQ -c "select string_agg(account_code||':'||debit_amount::numeric(20,2)||'/'||credit_amount::numeric(20,2),',' order by account_code) from biz_voucher_entry where voucher_id=(select voucher_id from biz_receipt_refund where id='$RF1');")" "1002:0.00/200.00,1122:200.00/0.00"
  summary
}

step9() {
  say "== step9 ⑨ 销售发票（开票→电子/纸质分支→归档→红冲）"
  # ① 电子票：从应收开，金额 800
  api s9_inv1 POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR1\",\"invoice_date\":\"2026-11-09\",\"is_electronic\":true,\"total_with_tax\":800,\"tax_rate\":13,\"remark\":\"E2E-02 电子票\"}"
  [ "$CODE" = 0 ] || { bad "开电子票 http=$HTTP $(show)"; summary; return; }
  INV1=$(data '.data.id // empty')
  [ -z "$INV1" ] && INV1=$(PSQLQ -c "select id from biz_invoice where remark='E2E-02 电子票' order by created_at desc limit 1;")
  save INV1 "$INV1"
  ok "电子票 $(data '.data.invoice_no // "?"') 不含税=$(data '.data.amount') 税=$(data '.data.tax_amount') 合计=$(data '.data.total_with_tax')"
  check "电子票 税额口径(13%)" "$(PSQLQ -c "select (amount+tax_amount=total_with_tax)::text||'/'||round(tax_amount*100/amount)::int from biz_invoice where id='$INV1';")" "true/13"
  api s9_issue_noimg PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"ISSUED"}'
  if [ "$CODE" != 0 ]; then ok "无影像开票被拦 http=$HTTP $(show)"; else bad "无影像竟然开票成功"; fi
  local png=$(mkpng) pdf=$(mkpdf)
  attach_upload s9_png INVOICE "$INV1" "$png" image/png
  api s9_issue_png PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"ISSUED"}'
  if [ "$CODE" != 0 ]; then ok "电子票传 png 被拦 http=$HTTP $(show)"; else bad "电子票 png 竟然通过"; fi
  attach_upload s9_pdf INVOICE "$INV1" "$pdf" application/pdf
  api s9_issue PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"ISSUED"}'
  [ "$CODE" = 0 ] && ok "电子票 ISSUED（凭 $(data '.data.voucher_no // "?"')）" || bad "电子票 ISSUED http=$HTTP $(show)"
  say "  收入凭证分录: $(PSQLQ -c "select string_agg(account_code||':'||debit_amount::numeric(20,2)||'/'||credit_amount::numeric(20,2),',' order by account_code) from biz_voucher_entry where voucher_id=(select voucher_id from biz_invoice where id='$INV1');")"
  check "应收回写发票号" "$(PSQLQ -c "select coalesce(invoice_no,'NULL') from biz_receivable where id='$AR1';")" "$(PSQLQ -c "select invoice_no from biz_invoice where id='$INV1';")"
  # ② 纸质票：余额 782
  api s9_inv2 POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR1\",\"invoice_date\":\"2026-11-09\",\"is_electronic\":false,\"total_with_tax\":782,\"tax_rate\":13,\"remark\":\"E2E-02 纸质票\"}"
  [ "$CODE" = 0 ] || bad "开纸质票 http=$HTTP $(show)"
  INV2=$(data '.data.id // empty')
  [ -z "$INV2" ] && INV2=$(PSQLQ -c "select id from biz_invoice where remark='E2E-02 纸质票' order by created_at desc limit 1;")
  save INV2 "$INV2"; ok "纸质票 $(data '.data.invoice_no // "?"') 合计=$(data '.data.total_with_tax')"
  attach_upload s9_png2 INVOICE "$INV2" "$png" image/png
  api s9_issue2 PATCH "/business/erp/invoice/$INV2/status" "$T_ADMIN" '{"status":"ISSUED"}'
  [ "$CODE" = 0 ] && ok "纸质票传 png → ISSUED" || bad "纸质票 ISSUED http=$HTTP $(show)"
  api s9_posted2 PATCH "/business/erp/invoice/$INV2/status" "$T_ADMIN" '{"status":"POSTED"}'
  [ "$CODE" = 0 ] && ok "纸质票 → POSTED（归档）" || bad "纸质票 POSTED http=$HTTP $(show)"
  api s9_void_posted PATCH "/business/erp/invoice/$INV2/status" "$T_ADMIN" '{"status":"VOID","reason":"E2E-02 已归档作废测试"}'
  if [ "$CODE" != 0 ]; then ok "已归档(POSTED)发票不允许作废 http=$HTTP $(show)"; else bad "POSTED 发票竟然可作废"; fi
  # ③ 红冲：作废电子票 → 红字凭证 + 应收指针改指纸质票
  api s9_void PATCH "/business/erp/invoice/$INV1/status" "$T_ADMIN" '{"status":"VOID","reason":"E2E-02 演练作废重开"}'
  [ "$CODE" = 0 ] && ok "电子票 → VOID（红冲）" || bad "作废 http=$HTTP $(show)"
  check "红字凭证张数(is_red=1)" "$(PSQLQ -c "select count(*) from biz_voucher where source_id='$INV1' and is_red=1;")" "1"
  check "应收发票号改指纸质票" "$(PSQLQ -c "select invoice_no from biz_receivable where id='$AR1';")" "$(PSQLQ -c "select invoice_no from biz_invoice where id='$INV2';")"
  # ④ 税率口径：小数 0.13 → 自动 ×100 并记 DECIMAL 模式
  api s9_inv3 POST /business/erp/invoice "$T_ADMIN" "{\"receivable_id\":\"$AR1\",\"invoice_date\":\"2026-11-09\",\"is_electronic\":true,\"total_with_tax\":800,\"tax_rate\":0.13,\"remark\":\"E2E-02 税率小数口径\"}"
  if [ "$CODE" = 0 ]; then
    INV3=$(data '.data.id // empty')
    [ -z "$INV3" ] && INV3=$(PSQLQ -c "select id from biz_invoice where remark='E2E-02 税率小数口径' order by created_at desc limit 1;")
    save INV3 "$INV3"
    check "0.13 落库为百分数 13" "$(PSQLQ -c "select tax_rate::numeric(20,4)::text from biz_invoice where id='$INV3';")" "13.0000"
    say "  tax_rate_mode=$(PSQLQ -c "select coalesce(tax_rate_mode,'NULL') from biz_invoice where id='$INV3';")"
    api s9_void3 PATCH "/business/erp/invoice/$INV3/status" "$T_ADMIN" '{"status":"VOID","reason":"E2E-02 演练清理税率口径票"}'
    ok "税率口径票已作废（保持应收余额 782 已开票）"
  else bad "税率小数口径建票 http=$HTTP $(show)"; fi
  say "  应收现状: $(PSQLQ -c "select amount||' 已收'||received_amount||' 未收'||unsettled_amount||' 发票'||coalesce(invoice_no,'-') from biz_receivable where id='$AR1';")"
  summary
}

case "${1:-}" in
  step5) step5 ;; step6) step6 ;; step7) step7 ;; step8) step8 ;; step9) step9 ;;
  *) echo "用法: $0 {step5|step6|step7|step8|step9}"; exit 2 ;;
esac
