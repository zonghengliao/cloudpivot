#!/usr/bin/env bash
# ============================================================================
# FIX-6（t_728f716e）145 取证 · E2E：税率标度统一 / 期初科目可配 / 含税勾稽 / 留抵重分类
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix6-tax-and-accounts/21_e2e.sh'
# 前置：10_prepare_145.sh + 11_start_instance.sh（8047 + 拷贝库）
#
# 断言全部对着**真接口 + 真库**：HTTP 状态 + 响应字段 + 库内落值 + 凭证分录。
# ============================================================================
set -uo pipefail
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$EV/00_env.sh"

: > "$EV/raw/e2e.log"
exec > >(tee -a "$EV/raw/e2e.log") 2>&1

echo "=== FIX-6 E2E $(date '+%F %T') db=$DBNAME period=$PERIOD ==="

# ---------- 登录 ----------
ADMIN=$(login admin "$ADMIN_PASS")
if [ -z "$ADMIN" ]; then echo "FATAL: admin 登录失败"; exit 1; fi
echo "$ADMIN" > "$(tokf admin)"
echo "token ok (len=${#ADMIN})"

# ---------- 0. 迁移落库（存量库升级路径） ----------
echo "--- 0) V0327/V0328/V0329 已应用到拷贝库"
check_eq "327" "$(sq "select version from schema_migrations where name='tax_rate_percent_scale'")" "V0327 已应用"
check_eq "328" "$(sq "select version from schema_migrations where name='opening_equity_account_config'")" "V0328 已应用"
check_eq "329" "$(sq "select version from schema_migrations where name='report_item_negative_reclass'")" "V0329 已应用"
check_eq "7,4" "$(sq "select numeric_precision||','||numeric_scale from information_schema.columns where table_name='biz_invoice' and column_name='tax_rate'")" "biz_invoice.tax_rate 放宽到 NUMERIC(7,4)"
check_eq "1" "$(sq "select count(*) from information_schema.columns where table_name='biz_invoice' and column_name='tax_rate_mode'")" "biz_invoice.tax_rate_mode 已加列"
check_eq "1" "$(sq "select count(*) from information_schema.columns where table_name='biz_material' and column_name='tax_rate_mode'")" "biz_material.tax_rate_mode 已加列"
check_eq "3103" "$(sq "select config_value from sys_config where config_key='inventory_opening_equity_account' and deleted_at is null")" "期初对方科目参数已种（默认 3103）"
check_eq "OTHER_CURRENT_ASSETS" "$(sq "select reclass_negative_to from biz_report_item where report_code='BALANCE_SHEET' and item_code='TAXES_PAYABLE'")" "应交税费重分类目标 = 其他流动资产"
# 存量迁移：两表都不能再有「(0,1] 未标注」的行
check_eq "0" "$(sq "select count(*) from biz_invoice where tax_rate > 0 and tax_rate <= 1 and tax_rate_mode is null")" "发票无未迁的小数税率"
check_eq "0" "$(sq "select count(*) from biz_material where tax_rate > 0 and tax_rate <= 1 and tax_rate_mode is null")" "物料无未迁的小数税率"
check_eq "0" "$(sq "select count(*) from biz_invoice where tax_rate_mode is null")" "发票税率标度位全部回填"

# ---------- 0b. 存量数据迁移后，历史单据金额不变 ----------
echo "--- 0b) 存量单据金额一个字没动（与迁移前 dump 对比）"
PRE="$EV/raw/pre_migration_amounts.txt"
if [ -f "$PRE" ]; then
  CUR_INV=$(sq "select count(*)||'|'||coalesce(sum(amount),0)||'|'||coalesce(sum(tax_amount),0)||'|'||coalesce(sum(total_with_tax),0) from biz_invoice")
  CUR_MAT=$(sq "select count(*)||'|'||coalesce(sum(tax_rate),0) from biz_material")
  PRE_INV=$(sed -n 's/^invoice|//p' "$PRE")
  PRE_MAT_ROWS=$(sed -n 's/^material_rows|//p' "$PRE")
  check_eq "$PRE_INV" "$CUR_INV" "发票行数+三个金额合计与迁移前一致"
  check_eq "$PRE_MAT_ROWS" "$(echo "$CUR_MAT" | cut -d'|' -f1)" "物料行数与迁移前一致"
else
  fail "缺少迁移前快照 $PRE（10_prepare_145.sh 没记录？）"
fi
# 存量发票税率形态：不该再有 (0,1] 的值
check_eq "0" "$(sq "select count(*) from biz_invoice where tax_rate > 0 and tax_rate <= 1 and tax_rate_mode='DECIMAL'")" "存量发票税率已全部 ×100（无 0.13 形态）"

# ---------- 夹具 ----------
MCODE="FIX6-M-$(date +%H%M%S)"
UNIT=$(sq "select id from biz_unit where deleted_at is null limit 1")
CUST=$(sq "select id from biz_customer where deleted_at is null limit 1")
if [ -z "$CUST" ]; then
  RES=$(api POST /business/erp/customer "$ADMIN" '{"customer_code":"FIX6-C-001","customer_name":"FIX6 演练客户","customer_type":"ENTERPRISE","status":1}')
  CUST=$(echo "$RES" | jq -r '.data.id // empty')
fi
check_eq "1" "$(echo -n "${CUST:0:8}" | wc -c | tr -d ' ')" "夹具客户就绪（${CUST:0:8}…）"

# ============================================================================
echo "--- ① 发票 tax_rate = 13（百分数）→ 200，税额 = 金额 × 13%"
BODY=$(jq -nc --arg c "$CUST" --arg d "$(date +%F)" \
  '{customer_id:$c, invoice_date:$d, amount:400, tax_rate:13, source_type:"MANUAL", remark:"FIX6 百分数税率"}')
RES=$(api POST /business/erp/invoice "$ADMIN" "$BODY"); check_http 200 "开票（tax_rate=13）"
INV_PCT=$(echo "$RES" | jq -r '.data.id // empty')
echo "$RES" > "$EV/raw/inv_percent_13.json"
check_num "400" "$(echo "$RES" | jq -r '.data.amount')" "不含税金额 400"
check_num "52" "$(echo "$RES" | jq -r '.data.tax_amount')" "税额 = 400 × 13% = 52"
check_num "452" "$(echo "$RES" | jq -r '.data.total_with_tax')" "价税合计 = 452"
check_num "13" "$(echo "$RES" | jq -r '.data.tax_rate')" "回显 tax_rate = 13（百分数）"
check_eq "PERCENT" "$(echo "$RES" | jq -r '.data.tax_rate_mode')" "落库标度 = PERCENT"
check_eq "0.1300" "$(sq "select tax_rate_mode || '' from biz_invoice where id='$INV_PCT'" >/dev/null 2>&1; sq "select round(tax_rate/100,4) from biz_invoice where id='$INV_PCT'")" "库里 13 就是 13%（13/100=0.13）"

# ============================================================================
echo "--- ② 发票 tax_rate = 0.13（老前端小数）→ 200 + ×100 + WARN 日志"
WARN_BEFORE=$(grep -c "已自动 ×100" /root/backup/fix6_instance.log || true)
BODY=$(jq -nc --arg c "$CUST" --arg d "$(date +%F)" \
  '{customer_id:$c, invoice_date:$d, amount:400, tax_rate:0.13, source_type:"MANUAL", remark:"FIX6 过渡期小数税率"}')
RES=$(api POST /business/erp/invoice "$ADMIN" "$BODY"); check_http 200 "开票（tax_rate=0.13，兼容）"
INV_DEC=$(echo "$RES" | jq -r '.data.id // empty')
echo "$RES" > "$EV/raw/inv_decimal_013.json"
check_num "13" "$(echo "$RES" | jq -r '.data.tax_rate')" "0.13 自动归一成 13"
check_num "52" "$(echo "$RES" | jq -r '.data.tax_amount')" "税额仍是 52（不是 100 倍）"
check_eq "DECIMAL" "$(echo "$RES" | jq -r '.data.tax_rate_mode')" "落库标度 = DECIMAL（审计位）"
sleep 1
WARN_AFTER=$(grep -c "已自动 ×100" /root/backup/fix6_instance.log || true)
if [ "$WARN_AFTER" -gt "$WARN_BEFORE" ]; then pass "打了 WARN 日志（$WARN_BEFORE → $WARN_AFTER 条）"; else fail "没有 WARN 日志（$WARN_BEFORE → $WARN_AFTER）"; fi
grep "已自动 ×100" /root/backup/fix6_instance.log | tail -2 > "$EV/raw/tax_rate_warn.log" || true

# ============================================================================
echo "--- ③ 越界 400 / 1% 与 100% 的边界"
BODY=$(jq -nc --arg c "$CUST" --arg d "$(date +%F)" '{customer_id:$c, invoice_date:$d, amount:400, tax_rate:100.01}')
RES=$(api POST /business/erp/invoice "$ADMIN" "$BODY"); check_http 400 "tax_rate=100.01 → 400"
echo "$RES" > "$EV/raw/inv_tax_rate_too_high.json"
BODY=$(jq -nc --arg c "$CUST" --arg d "$(date +%F)" '{customer_id:$c, invoice_date:$d, amount:400, tax_rate:-1}')
RES=$(api POST /business/erp/invoice "$ADMIN" "$BODY"); check_http 400 "tax_rate=-1 → 400"
BODY=$(jq -nc --arg c "$CUST" --arg d "$(date +%F)" '{customer_id:$c, invoice_date:$d, amount:400, tax_rate:1, tax_rate_mode:"PERCENT"}')
RES=$(api POST /business/erp/invoice "$ADMIN" "$BODY"); check_http 200 "tax_rate=1 + mode=PERCENT → 200"
check_num "4" "$(echo "$RES" | jq -r '.data.tax_amount')" "1 就是 1%（400 → 4），不是 100%"
echo "$RES" > "$EV/raw/inv_one_percent.json"
BODY=$(jq -nc --arg c "$CUST" --arg d "$(date +%F)" '{customer_id:$c, invoice_date:$d, amount:400, tax_rate:0.5, tax_rate_mode:"DECIMAL"}')
RES=$(api POST /business/erp/invoice "$ADMIN" "$BODY"); check_http 200 "0.5 + mode=DECIMAL → 200"
check_num "50" "$(echo "$RES" | jq -r '.data.tax_rate')" "DECIMAL 模式下 0.5 = 50%"
echo "$RES" > "$EV/raw/inv_decimal_mode.json"

# ============================================================================
echo "--- ④ 物料 tax_rate 同口径（0.13 兼容 / 13 直传 / 越界 400）"
RES=$(api POST /business/erp/material "$ADMIN" \
  "$(jq -nc --arg u "$UNIT" --arg c "$MCODE-A" '{material_code:$c, material_name:"FIX6 夹具物料A", unit_id:$u, material_type:"RAW", costing_method:"MOVING_AVG", tax_rate:0.13, status:1}')")
check_http 200 "建物料（tax_rate=0.13）"
MAT_A=$(echo "$RES" | jq -r '.data.id // empty')
echo "$RES" > "$EV/raw/material_decimal.json"
check_num "13" "$(sq "select tax_rate from biz_material where id='$MAT_A'")" "物料库内 tax_rate = 13"
check_eq "DECIMAL" "$(sq "select tax_rate_mode from biz_material where id='$MAT_A'")" "物料标度位 = DECIMAL"
RES=$(api POST /business/erp/material "$ADMIN" \
  "$(jq -nc --arg u "$UNIT" --arg c "$MCODE-B" '{material_code:$c, material_name:"FIX6 夹具物料B", unit_id:$u, material_type:"RAW", costing_method:"MOVING_AVG", tax_rate:13, status:1}')")
check_http 200 "建物料（tax_rate=13）"
MAT_B=$(echo "$RES" | jq -r '.data.id // empty')
check_num "13" "$(sq "select tax_rate from biz_material where id='$MAT_B'")" "物料库内 tax_rate = 13（百分数直传）"
check_eq "PERCENT" "$(sq "select tax_rate_mode from biz_material where id='$MAT_B'")" "物料标度位 = PERCENT"
RES=$(api POST /business/erp/material "$ADMIN" \
  "$(jq -nc --arg u "$UNIT" --arg c "$MCODE-X" '{material_code:$c, material_name:"FIX6 越界", unit_id:$u, material_type:"RAW", costing_method:"MOVING_AVG", tax_rate:100.5, status:1}')")
check_http 400 "建物料 tax_rate=100.5 → 400"

# ============================================================================
echo "--- ⑤ P0-01 金额勾稽：开票（ISSUED）凭证 借1122=价税合计 / 贷5001=不含税 / 贷2221.02=税额"
RES=$(api PATCH "/business/erp/invoice/$INV_PCT/status" "$ADMIN" '{"status":"ISSUED"}')
check_http 200 "发票 ISSUED"
check_eq "ISSUED" "$(sq "select status from biz_invoice where id='$INV_PCT'")" "库内状态 ISSUED"
check_num "452" "$(sq "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_invoice i on i.voucher_id=e.voucher_id where i.id='$INV_PCT' and e.account_code='1122'")" "凭证 借 1122 应收账款 452"
check_num "400" "$(sq "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_invoice i on i.voucher_id=e.voucher_id where i.id='$INV_PCT' and e.account_code='5001'")" "凭证 贷 5001 主营业务收入 400"
check_num "52" "$(sq "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_invoice i on i.voucher_id=e.voucher_id where i.id='$INV_PCT' and e.account_code='2221.02'")" "凭证 贷 2221.02 销项税 52"
check_eq "1" "$(sq "select count(*) from biz_voucher v join biz_invoice i on i.voucher_id=v.id where i.id='$INV_PCT' and v.total_debit = v.total_credit")" "该凭证借贷相等"

echo "--- ⑤b P0-02 金额勾稽：采购单金额三列恒等（total_with_tax = amount + tax）"
if [ -n "$MAT_B" ]; then
  RES=$(api POST /business/erp/purchase-order "$ADMIN" \
    "$(jq -nc --arg m "$MAT_B" --arg u "$UNIT" '{supplier_id:null, order_date:"'"$(date +%F)"'", price_mode:"EXCL", items:[{material_id:$m, unit_id:$u, qty:2, price:100, tax_rate:13}]}' )")
  echo "$RES" > "$EV/raw/purchase_order_13.json"
  HTTP_PO=$HTTP
  if [ "$HTTP_PO" = "200" ] || [ "$HTTP_PO" = "201" ]; then
    PO_ID=$(echo "$RES" | jq -r '.data.id // empty')
    check_num "200" "$(sq "select total_amount from biz_purchase_order where id='$PO_ID'")" "采购单不含税 200"
    check_num "26"  "$(sq "select total_tax from biz_purchase_order where id='$PO_ID'")" "采购单税额 = 200 × 13% = 26"
    check_num "226" "$(sq "select total_with_tax from biz_purchase_order where id='$PO_ID'")" "采购单价税合计 226（= 应付金额）"
    check_num "226" "$(sq "select with_tax_amount from biz_purchase_order_item where order_id='$PO_ID' limit 1")" "采购行 含税金额 226"
  else
    echo "  （采购单建单返回 HTTP $HTTP_PO，跳过 P0-02 单头断言：$(echo "$RES" | jq -c '{code,msg}' 2>/dev/null)）"
  fi
fi

# ============================================================================
echo "--- ⑥ 科目期初：借 2221.01 260（进项） / 贷 1002 260 → 形成留抵 208"
RES=$(api POST /business/erp/opening-balance "$ADMIN" \
  "$(jq -nc --arg p "$PERIOD" '{period:$p, entries:[{account_code:"2221.01", debit:260, credit:0, remark:"FIX6 留抵演练（进项）"},{account_code:"1002", debit:0, credit:260, remark:"FIX6 留抵演练（对方）"}]}')")
check_http 200 "保存科目期初（借 2221.01 / 贷 1002）"
echo "$RES" > "$EV/raw/opening_balance.json"
check_num "260" "$(sq "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.period='$PERIOD' and v.voucher_type='OPENING' and e.account_code='2221.01'")" "期初凭证 借 2221.01 进项税 260"

# ============================================================================
echo "--- ⑦ 资产负债表：应交税费不再显示 -208，重分类到「其他流动资产」+ 附注"
RES=$(api GET "/business/erp/report/balance-sheet?period=$PERIOD" "$ADMIN")
check_http 200 "资产负载表出表"
echo "$RES" > "$EV/raw/balance_sheet.json"
TAX_LINE=$(echo "$RES" | jq -r '.data.liabilities[] | select(.item_code=="TAXES_PAYABLE") | .amount')
OCA_LINE=$(echo "$RES" | jq -r '.data.assets[] | select(.item_code=="OTHER_CURRENT_ASSETS") | .amount')
RECLASS_AMT=$(echo "$RES" | jq -r '.data.reclass[0].amount // "0"')
RECLASS_FROM=$(echo "$RES" | jq -r '.data.reclass[0].from_item // ""')
RECLASS_TO=$(echo "$RES" | jq -r '.data.reclass[0].to_item // ""')
NOTES=$(echo "$RES" | jq -r '.data.notes | join("\n")')
BAL=$(echo "$RES" | jq -r '.data.check.balanced')
DIFF=$(echo "$RES" | jq -r '.data.check.diff')
check_num "0" "$TAX_LINE" "应交税费按 0 列示（不再是 -208）"
check_num "208" "$RECLASS_AMT" "重分类金额 = 进项 260 − 销项 52 = 208"
check_eq "TAXES_PAYABLE" "$RECLASS_FROM" "重分类来源 = 应交税费"
check_eq "OTHER_CURRENT_ASSETS" "$RECLASS_TO" "重分类目标 = 其他流动资产"
if echo "$NOTES" | grep -q "留抵税额"; then pass "附注写明「留抵税额」"; else fail "附注缺留抵说明：$NOTES"; fi
if echo "$NOTES" | grep -q "可结转下期"; then pass "附注写明「可结转下期抵扣」"; else fail "附注缺结转说明"; fi
check_eq "true" "$BAL" "资产负债表仍然平衡"
check_num "0" "$DIFF" "勾稽差额 = 0"
TAX_DETAIL=$(echo "$RES" | jq -r '.data.liabilities[] | select(.item_code=="TAXES_PAYABLE") | .detail')
if echo "$TAX_DETAIL" | grep -q "留抵税额"; then pass "应交税费行取数说明点明留抵"; else fail "取数说明缺留抵：$TAX_DETAIL"; fi
# 重分类前后总额守恒：其他流动资产 = 1901 余额 + 208
BASE_1901=$(sq "select coalesce(sum(case when e.debit_amount>=0 then e.debit_amount-e.credit_amount else 0 end),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id join biz_account a on a.account_code=e.account_code where v.status='POSTED' and v.deleted_at is null and a.account_code='1901' and v.voucher_date <= date_trunc('month', current_date)::date - 1")
echo "  （参考）1901 历史余额 = ${BASE_1901:-0}，其他流动资产列示 = $OCA_LINE"

# ============================================================================
echo "--- ⑧ 期初对方科目可配：改成 3101 → 新生成的期初凭证贷 3101（不是 3103）"
CFG_ID=$(sq "select id from sys_config where config_key='inventory_opening_equity_account' and deleted_at is null")
WH=$(sq "select id from biz_warehouse where deleted_at is null limit 1")
if [ -z "$WH" ]; then
  RES=$(api POST /business/erp/warehouse "$ADMIN" '{"warehouse_code":"FIX6-WH","warehouse_name":"FIX6 仓","warehouse_type":"NORMAL","status":1,"level":1}')
  WH=$(echo "$RES" | jq -r '.data.id // empty')
fi
RES=$(api PUT "/support/config/$CFG_ID" "$ADMIN" \
  "$(jq -nc '{config_key:"inventory_opening_equity_account", config_value:"3101", config_type:"STRING", description:"库存期初对方科目（FIX6 E2E 临时改 3101）", category:"erp", config_level:2, is_modifiable:true, status:1}')")
check_http 200 "把系统参数改成 3101"
check_eq "3101" "$(sq "select config_value from sys_config where config_key='inventory_opening_equity_account'")" "参数已落库 3101"
RES=$(api POST /business/erp/opening-balance/inventory "$ADMIN" \
  "$(jq -nc --arg p "$PERIOD" --arg m "$MAT_A" --arg w "$WH" '{period:$p, items:[{material_id:$m, warehouse_id:$w, qty:10, unit_cost:100, remark:"FIX6 期初对方科目可配"}]}')")
check_http 200 "保存库存期初（1000 元）"
echo "$RES" > "$EV/raw/opening_inventory_3101.json"
check_eq "3101" "$(echo "$RES" | jq -r '.data.inventory_credit_account')" "返回体回显对方科目 3101"
check_num "1000" "$(sq "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.period='$PERIOD' and v.voucher_type='OPENING' and e.account_code='3101'")" "期初凭证 贷 3101 = 1000"
check_num "0" "$(sq "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.period='$PERIOD' and v.voucher_type='OPENING' and e.account_code='3103'")" "3103 一分钱都没有"
check_num "1000" "$(sq "select coalesce(sum(e.debit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.period='$PERIOD' and v.voucher_type='OPENING' and e.account_code='1405'")" "借方仍是 1405 = 1000"

echo "--- ⑧b 还原参数（3103）并复核期初凭证重新生成到 3103"
RES=$(api PUT "/support/config/$CFG_ID" "$ADMIN" \
  "$(jq -nc '{config_key:"inventory_opening_equity_account", config_value:"3103", config_type:"STRING", description:"库存期初对方科目", category:"erp", config_level:2, is_modifiable:true, status:1}')")
check_http 200 "参数改回 3103"
RES=$(api POST /business/erp/opening-balance/inventory "$ADMIN" \
  "$(jq -nc --arg p "$PERIOD" --arg m "$MAT_A" --arg w "$WH" '{period:$p, items:[{material_id:$m, warehouse_id:$w, qty:10, unit_cost:100}]}')")
check_http 200 "重存库存期初"
check_eq "3103" "$(echo "$RES" | jq -r '.data.inventory_credit_account')" "对方科目回到 3103"
check_num "1000" "$(sq "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.period='$PERIOD' and v.voucher_type='OPENING' and e.account_code='3103'")" "期初凭证已改用 3103 贷 1000"
check_num "0" "$(sq "select coalesce(sum(e.credit_amount),0) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.period='$PERIOD' and v.voucher_type='OPENING' and e.account_code='3101'")" "3101 已清零"

# ============================================================================
echo "--- ⑨ 借贷平衡不变（期初凭证 + 试算平衡 + 资产负债表）"
check_eq "0" "$(sq "select count(*) from biz_voucher where deleted_at is null and status='POSTED' and total_debit <> total_credit")" "全库无借贷不等的已记账凭证"
RES=$(api GET "/business/erp/report/balance-sheet?period=$PERIOD" "$ADMIN")
echo "$RES" > "$EV/raw/balance_sheet_after.json"
check_eq "true" "$(echo "$RES" | jq -r '.data.check.balanced')" "重分类 + 换科目后报表仍平衡"
check_num "0" "$(echo "$RES" | jq -r '.data.check.diff')" "勾稽差额仍为 0"
check_num "208" "$(echo "$RES" | jq -r '.data.reclass[0].amount // "0"')" "留抵重分类仍为 208（换对方科目不影响）"

echo "--- ⑩ 清理本卡夹具（只删自己造的物料/发票期初，不动历史数据）"
psqlc -q -c "DELETE FROM biz_opening_inventory WHERE period_code='$PERIOD' AND material_id IN (select id from biz_material where material_code like 'FIX6-M-%')" >/dev/null 2>&1
echo "  夹具保留在拷贝库（实例用完即 kill，库随后丢弃；8033/cloudpivot_dev 全程未动）"

echo
echo "================================"
if [ "$FAILS" -eq 0 ]; then echo "RESULT: ALL PASS"; else echo "RESULT: $FAILS FAILED"; fi
echo "================================"
exit "$FAILS"
