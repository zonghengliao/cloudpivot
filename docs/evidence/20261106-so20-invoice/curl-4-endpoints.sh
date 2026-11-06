#!/usr/bin/env bash
# ============================================================================
# SO-20 销售发票后端 API 的 4 接口 curl 实测（真实 token / 真实库 / 真凭证）
#
# 用法（先起一个指向**隔离库**的实例）：
#   BASE=http://127.0.0.1:18123/api DB=cloudpivot_so20 \
#     bash docs/evidence/20261106-so20-invoice/curl-4-endpoints.sh
#
# 断言分组：
#   ① 登录（首登强制改密自动处理）
#   ② 从应收开票 POST /invoice（DRAFT、IV- 取号、金额自应收带出、客户带出）
#   ③ 超额开票被拒（> 可开票额度 → 409；金额不自洽 → 400；负金额 → 400）
#   ④ 详情 GET /invoice/{id} / 分页 GET /invoice/page
#   ⑤ 开票 PATCH status=ISSUED（收入凭证 借 1122 / 贷 5001 + 2221.02，回写应收发票号）
#   ⑥ 作废 PATCH status=VOID（红字凭证 + 撤回应收发票号 + 红字票再开被拒）
#   ⑦ 归档 ISSUED→POSTED 与终态约束（POSTED→VOID 无此边 409）
#   ⑧ 手工开票（无应收）与库层留痕（biz_invoice / biz_voucher / biz_status_log）
#
# ⚠ 会清空演示库的 biz_invoice / biz_receivable / 演示客户与 INVOICE 凭证，只对一次性隔离库运行。
# ============================================================================
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:18123/api}
DB=${DB:-cloudpivot_so20}
PGHOST=${PGHOST:-127.0.0.1}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}

CUSTOMER='5a200000-0000-4000-8000-000000000001'
OTHER_CUSTOMER='5a200000-0000-4000-8000-000000000002'
AR_MAIN='5a200000-0000-4000-8000-0000000000a1'   # 1130，整张开票 → 作废
AR_POST='5a200000-0000-4000-8000-0000000000a2'   # 565，开票 → 归档
AR_SPARE='5a200000-0000-4000-8000-0000000000a3'  # 226，超额 / 分次开票用例

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1：期望 $3，实际 $2"; fi; }
checkne(){ if [ "$2" != "$3" ]; then ok "$1（$3）"; else bad "$1：不该等于 $3"; fi; }

psql_q() { psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1"; }

echo "==> 0. 夹具（2 客户 + 3 张应收）"
psql_q "DELETE FROM biz_invoice" >/dev/null
psql_q "DELETE FROM biz_voucher WHERE source_type = 'INVOICE'" >/dev/null 2>&1
psql_q "DELETE FROM biz_receivable" >/dev/null
psql_q "DELETE FROM biz_customer WHERE id IN ('$CUSTOMER'::uuid, '$OTHER_CUSTOMER'::uuid)" >/dev/null
# 状态日志是审计流水，不随单据表清空：本库是一次性隔离库，按实体类型清干净才能断言条数
psql_q "DELETE FROM biz_status_log WHERE entity_type IN ('INVOICE','RECEIVABLE')" >/dev/null
psql_q "INSERT INTO biz_customer (id, code, name, status) VALUES
          ('$CUSTOMER'::uuid, 'INV20-01', '开票演示客户', 'ACTIVE'),
          ('$OTHER_CUSTOMER'::uuid, 'INV20-02', '别家客户', 'ACTIVE')" >/dev/null
mk_ar() { # id ar_no customer amount
  psql_q "INSERT INTO biz_receivable
            (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
             amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status)
          VALUES ('$1'::uuid, '$2', '$3'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY',
                  $4, 0, $4, 30, DATE '2026-12-06', '2026-11', 'OPEN')" >/dev/null
}
mk_ar "$AR_MAIN"  'AR20-001' "$CUSTOMER" 1130
mk_ar "$AR_POST"  'AR20-002' "$CUSTOMER" 565
mk_ar "$AR_SPARE" 'AR20-003' "$CUSTOMER" 226
echo "      应收 3 张（1130 / 565 / 226），收入科目：$(psql_q "select string_agg(account_code, ',' order by account_code) from biz_account where account_code in ('1122','5001','2221.02') and deleted_at is null")"

echo
echo "==> 1. 登录"
login() {
  curl -s -X POST -H 'Content-Type: application/json' \
    --data-binary "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" \
    "$BASE/system/auth/login"
}
RESP=""; TOKEN=""
for candidate in "$ADMIN_PASS" "${ADMIN_PASS}_so20"; do
  RESP=$(login "$candidate")
  TOKEN=$(echo "$RESP" | jq -r '.data.access_token // empty')
  if [ -n "$TOKEN" ]; then ADMIN_PASS="$candidate"; break; fi
done
if [ -z "$TOKEN" ]; then echo "  !! 登录失败：$RESP"; exit 1; fi
MUST=$(echo "$RESP" | jq -r '.data.must_change_password // false')
if [ "$MUST" = "true" ]; then
  NEW_PASS="${ADMIN_PASS}_so20"
  curl -s -o /dev/null -X PUT -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    --data-binary "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}" \
    "$BASE/system/auth/password"
  ADMIN_PASS="$NEW_PASS"
  TOKEN=$(login "$ADMIN_PASS" | jq -r '.data.access_token // empty')
  [ -n "$TOKEN" ] && ok "首登改密后重新登录" || { bad "改密后登录失败"; exit 1; }
fi
[ -n "$TOKEN" ] && ok "拿到 access_token（admin）"
AUTH="Authorization: Bearer $TOKEN"

echo
echo "==> 2. 从应收开票 POST /invoice（金额 / 客户 / 来源自应收带出）"
J=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"receivable_id\": \"$AR_MAIN\",
  \"invoice_date\": \"2026-11-06\",
  \"remark\": \"SO-20 curl 实测 · 从应收开票\"
}" "$BASE/business/erp/invoice")
echo "      → $(echo "$J" | jq -c '{code, msg, invoice_no: .data.invoice_no, status: .data.status, amount: .data.amount, tax: .data.tax_amount, total: .data.total_with_tax, customer: .data.customer_name, receivable_no: .data.receivable_no, headroom: .data.receivable_headroom}')"
check "开票 code" "$(echo "$J" | jq -r '.code')" "0"
INV1=$(echo "$J" | jq -r '.data.id')
INV1_NO=$(echo "$J" | jq -r '.data.invoice_no')
check "状态 = DRAFT" "$(echo "$J" | jq -r '.data.status')" "DRAFT"
check "状态中文" "$(echo "$J" | jq -r '.data.status_label')" "草稿"
check "票种中文" "$(echo "$J" | jq -r '.data.invoice_type_label')" "销项正票"
check "不含税 = 1000（1130/1.13）" "$(echo "$J" | jq -r '.data.amount')" "1000.0000"
check "税额 = 130" "$(echo "$J" | jq -r '.data.tax_amount')" "130.0000"
check "价税合计 = 1130（自应收带出）" "$(echo "$J" | jq -r '.data.total_with_tax')" "1130.0000"
check "税率 = 0.13（缺省）" "$(echo "$J" | jq -r '.data.tax_rate')" "0.1300"
check "客户自应收带出" "$(echo "$J" | jq -r '.data.customer_name')" "开票演示客户"
check "关联应收单号" "$(echo "$J" | jq -r '.data.receivable_no')" "AR20-001"
check "开票额度已用尽（headroom 0）" "$(echo "$J" | jq -r '.data.receivable_headroom')" "0.0000"
check "来源自应收带出（MANUAL）" "$(echo "$J" | jq -r '.data.source_type')" "MANUAL"
if echo "$INV1_NO" | grep -Eq '^IV-[0-9]{8}-[0-9]{6}$'; then ok "发票号形状 IV-yyyyMMdd-000001（$INV1_NO）"; else bad "发票号形状不对：$INV1_NO"; fi
echo "      发票 id=$INV1 单号=$INV1_NO"

echo
echo "==> 3. 超额 / 非法开票被拒"
JOVER=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"receivable_id\": \"$AR_MAIN\", \"total_with_tax\": 1
}" "$BASE/business/erp/invoice")
echo "      超额 → $(echo "$JOVER" | jq -c '{code, msg}')"
check "已开满再开 1 元 → 409(10003)" "$(echo "$JOVER" | jq -r '.code')" "10003"

JOVER2=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"receivable_id\": \"$AR_SPARE\", \"total_with_tax\": 226.01
}" "$BASE/business/erp/invoice")
echo "      超应收 1 分 → $(echo "$JOVER2" | jq -c '{code, msg}')"
check "226.01 > 应收 226 → 409(10003)" "$(echo "$JOVER2" | jq -r '.code')" "10003"
echo "      文案：$(echo "$JOVER2" | jq -r '.msg')"

JPART=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"receivable_id\": \"$AR_SPARE\", \"total_with_tax\": 113, \"remark\": \"分次开票（第一次 113）\"
}" "$BASE/business/erp/invoice")
check "分次开票 113/226 可以" "$(echo "$JPART" | jq -r '.code')" "0"
INV2=$(echo "$JPART" | jq -r '.data.id')
JPART2=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"receivable_id\": \"$AR_SPARE\", \"total_with_tax\": 113.01
}" "$BASE/business/erp/invoice")
check "剩余额度 113，再开 113.01 → 409" "$(echo "$JPART2" | jq -r '.code')" "10003"
JPART3=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"receivable_id\": \"$AR_SPARE\", \"total_with_tax\": 113, \"remark\": \"分次开票（第二次 113）\"
}" "$BASE/business/erp/invoice")
check "剩余额度 113 刚好开满 → 0" "$(echo "$JPART3" | jq -r '.code')" "0"

JBAD=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"customer_id\": \"$CUSTOMER\", \"amount\": 1000, \"tax_amount\": 130, \"total_with_tax\": 1200
}" "$BASE/business/erp/invoice")
echo "      金额不自洽 → $(echo "$JBAD" | jq -c '{code, msg}')"
check "三列不自洽 → 400(10001)" "$(echo "$JBAD" | jq -r '.code')" "10001"

JNEG=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"customer_id\": \"$CUSTOMER\", \"total_with_tax\": -100
}" "$BASE/business/erp/invoice")
check "负金额 → 400(10001)" "$(echo "$JNEG" | jq -r '.code')" "10001"

JRED=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"customer_id\": \"$CUSTOMER\", \"total_with_tax\": 100, \"invoice_type\": \"OUT_RED\"
}" "$BASE/business/erp/invoice")
echo "      手工建红字票 → $(echo "$JRED" | jq -c '{code, msg}')"
check "红字票不开放手工创建 → 400" "$(echo "$JRED" | jq -r '.code')" "10001"

JNOCUST=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"customer_id\": \"5a200000-0000-4000-8000-0000000000ff\", \"total_with_tax\": 10
}" "$BASE/business/erp/invoice")
check "客户不存在 → 400" "$(echo "$JNOCUST" | jq -r '.code')" "10001"

echo
echo "==> 4. 详情 / 分页"
JD=$(curl -s -H "$AUTH" "$BASE/business/erp/invoice/$INV1")
check "详情 code" "$(echo "$JD" | jq -r '.code')" "0"
check "详情客户名" "$(echo "$JD" | jq -r '.data.customer_name')" "开票演示客户"
check "详情应收单号" "$(echo "$JD" | jq -r '.data.receivable_no')" "AR20-001"
check "未开票前 voucher_no 为空" "$(echo "$JD" | jq -r '.data.voucher_no // "null"')" "null"
JP=$(curl -s -H "$AUTH" "$BASE/business/erp/invoice/page?page=1&page_size=10")
echo "      分页 → $(echo "$JP" | jq -c '{code, total: .data.total, first: .data.list[0].invoice_no}')"
check "分页 code" "$(echo "$JP" | jq -r '.code')" "0"
check "分页命中 3 张（整张 1 + 分次 2；两条超额被拒没落库）" "$(echo "$JP" | jq -r '.data.total')" "3"
JPS=$(curl -s -H "$AUTH" "$BASE/business/erp/invoice/page?status=ISSUED")
check "分页 status=ISSUED 命中 0" "$(echo "$JPS" | jq -r '.data.total')" "0"
JPBAD=$(curl -s -H "$AUTH" "$BASE/business/erp/invoice/page?status=CANCELLED")
echo "      status=CANCELLED（收款域值）→ $(echo "$JPBAD" | jq -c '{code, msg}')"
check "status 只认 4 值 → 400(66006? 10001)" "$(echo "$JPBAD" | jq -r '.code')" "10001"

echo
echo "==> 5. 开票（ISSUED）：收入凭证 + 回写应收"
JS=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"ISSUED"}' "$BASE/business/erp/invoice/$INV1/status")
echo "      → $(echo "$JS" | jq -c '{code, msg, status: .data.status, voucher_no: .data.voucher_no}')"
check "开票 code" "$(echo "$JS" | jq -r '.code')" "0"
check "状态 = ISSUED" "$(echo "$JS" | jq -r '.data.status')" "ISSUED"
check "状态中文 = 已开票" "$(echo "$JS" | jq -r '.data.status_label')" "已开票"
VNO1=$(echo "$JS" | jq -r '.data.voucher_no')
if echo "$VNO1" | grep -Eq '^PZ-[0-9]{8}-[0-9]{6}$'; then ok "收入凭证号形状 PZ-yyyyMMdd-000001（$VNO1）"; else bad "凭证号不对：$VNO1"; fi
check "库层：应收 invoice_no 已回写" "$(psql_q "select invoice_no from biz_receivable where id = '$AR_MAIN'::uuid")" "$INV1_NO"
check "库层：应收 invoice_date 已回写" "$(psql_q "select invoice_date from biz_receivable where id = '$AR_MAIN'::uuid")" "2026-11-06"
check "库层：发票 voucher_id 已回写" "$(psql_q "select case when voucher_id is null then 'null' else 'set' end from biz_invoice where id = '$INV1'::uuid")" "set"
echo "      凭证分录（借/贷/科目/金额）："
psql_q "select e.line_no || ' ' || e.account_code || ' 借' || e.debit_amount || ' 贷' || e.credit_amount from biz_voucher_entry e join biz_voucher v on v.id = e.voucher_id where v.source_type = 'INVOICE' and v.source_id = '$INV1'::uuid order by e.line_no" | sed 's/^/        /'
check "凭证借方合计 = 1130" "$(psql_q "select total_debit from biz_voucher where source_type='INVOICE' and source_id='$INV1'::uuid and is_red = 0")" "1130.0000"
check "凭证贷方合计 = 1130" "$(psql_q "select total_credit from biz_voucher where source_type='INVOICE' and source_id='$INV1'::uuid and is_red = 0")" "1130.0000"
check "凭证来源 = INVOICE" "$(psql_q "select source_type from biz_voucher where source_type='INVOICE' and source_id='$INV1'::uuid and is_red = 0")" "INVOICE"
check "状态日志：本单 2 条（建单 DRAFT + 开票 ISSUED）" "$(psql_q "select count(*) from biz_status_log where entity_type='INVOICE' and entity_id='$INV1'::uuid")" "2"

echo
echo "==> 6. 作废（VOID）：红字凭证 + 撤回应收发票号"
JV=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"VOID","reason":"客户退单，发票开错重开"}' "$BASE/business/erp/invoice/$INV1/status")
echo "      → $(echo "$JV" | jq -c '{code, status: .data.status, status_label: .data.status_label, voucher_no: .data.voucher_no}')"
check "作废 code" "$(echo "$JV" | jq -r '.code')" "0"
check "状态 = VOID" "$(echo "$JV" | jq -r '.data.status')" "VOID"
check "状态中文 = 已作废" "$(echo "$JV" | jq -r '.data.status_label')" "已作废"
check "红字凭证 = 1 张" "$(psql_q "select count(*) from biz_voucher where source_type='INVOICE' and source_id='$INV1'::uuid and is_red = 1")" "1"
REDNO=$(psql_q "select voucher_no from biz_voucher where source_type='INVOICE' and source_id='$INV1'::uuid and is_red = 1")
if echo "$REDNO" | grep -Eq '^PZR-[0-9]{8}-[0-9]{6}$'; then ok "红字凭证号形状 PZR-…（$REDNO）"; else bad "红字凭证号不对：$REDNO"; fi
check "原凭证 reversed_by 已回写" "$(psql_q "select case when reversed_by is null then 'null' else 'set' end from biz_voucher where source_type='INVOICE' and source_id='$INV1'::uuid and is_red = 0")" "set"
check "红字凭证借贷方向相反（贷 1122 / 借 5001+2221.02）" "$(psql_q "select string_agg(e.account_code || ':' || case when e.debit_amount > 0 then 'D' else 'C' end, ',' order by e.line_no) from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='INVOICE' and v.source_id='$INV1'::uuid and v.is_red=1")" "1122:C,5001:D,2221.02:D"
check "库层：应收 invoice_no 已撤回" "$(psql_q "select coalesce(invoice_no,'null') from biz_receivable where id = '$AR_MAIN'::uuid")" "null"
check "库层：应收 invoice_date 已撤回" "$(psql_q "select coalesce(invoice_date::text,'null') from biz_receivable where id = '$AR_MAIN'::uuid")" "null"
check "作废后额度恢复（可再开 1130）" "$(curl -s -H "$AUTH" "$BASE/business/erp/invoice/$INV1" | jq -r '.data.receivable_headroom')" "1130.0000"
check "VOID 是终态：再改 ISSUED → 409" "$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' --data-binary '{"status":"ISSUED"}' "$BASE/business/erp/invoice/$INV1/status" | jq -r '.code')" "10003"
JVNR=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' --data-binary '{"status":"VOID"}' "$BASE/business/erp/invoice/$INV2/status")
echo "      作废不给原因 → $(echo "$JVNR" | jq -c '{code, msg}')"
check "作废必须给 reason → 400" "$(echo "$JVNR" | jq -r '.code')" "10001"

echo
echo "==> 7. 归档（ISSUED → POSTED）与终态约束"
JPOST1=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"ISSUED"}' "$BASE/business/erp/invoice/$INV2/status")
check "第二张发票开票 → 0" "$(echo "$JPOST1" | jq -r '.code')" "0"
JPOST2=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"POSTED"}' "$BASE/business/erp/invoice/$INV2/status")
echo "      归档 → $(echo "$JPOST2" | jq -c '{code, status: .data.status, status_label: .data.status_label}')"
check "状态 = POSTED" "$(echo "$JPOST2" | jq -r '.data.status')" "POSTED"
check "状态中文 = 已归档" "$(echo "$JPOST2" | jq -r '.data.status_label')" "已归档"
check "归档不产生第二张凭证（仍 1 张蓝字）" "$(psql_q "select count(*) from biz_voucher where source_type='INVOICE' and source_id='$INV2'::uuid")" "1"
JPV=$(curl -s -X PATCH -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"status":"VOID","reason":"归档后想作废"}' "$BASE/business/erp/invoice/$INV2/status")
echo "      POSTED→VOID → $(echo "$JPV" | jq -c '{code, msg}')"
check "归档后作废无此边 → 409" "$(echo "$JPV" | jq -r '.code')" "10003"

echo
echo "==> 8. 手工开票（无应收）+ 库层留痕汇总"
JM=$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{
  \"customer_id\": \"$CUSTOMER\", \"amount\": 2000, \"invoice_no\": \"25337000000099887766\",
  \"invoice_type\": \"OUT\", \"remark\": \"手工开票（手填税控号）\"
}" "$BASE/business/erp/invoice")
echo "      手工 → $(echo "$JM" | jq -c '{code, invoice_no: .data.invoice_no, amount: .data.amount, tax: .data.tax_amount, total: .data.total_with_tax, status: .data.status}')"
check "手工开票 code" "$(echo "$JM" | jq -r '.code')" "0"
check "手填税控号原样保留" "$(echo "$JM" | jq -r '.data.invoice_no')" "25337000000099887766"
check "手工票税额 = 260（2000×13%）" "$(echo "$JM" | jq -r '.data.tax_amount')" "260.0000"
check "手工票价税合计 = 2260" "$(echo "$JM" | jq -r '.data.total_with_tax')" "2260.0000"
check "手工票无关联应收" "$(echo "$JM" | jq -r '.data.receivable_id // "null"')" "null"
INV3=$(echo "$JM" | jq -r '.data.id')
check "手填号重复 → 409" "$(curl -s -X POST -H "$AUTH" -H 'Content-Type: application/json' --data-binary "{\"customer_id\":\"$CUSTOMER\",\"amount\":1,\"invoice_no\":\"25337000000099887766\"}" "$BASE/business/erp/invoice" | jq -r '.code')" "10003"

echo
echo "      === 库层汇总 ==="
psql_q "select status || ' × ' || count(*) from biz_invoice group by status order by status" | sed 's/^/        biz_invoice /'
psql_q "select 'biz_voucher: 蓝字 ' || count(*) filter (where is_red = 0) || ' / 红字 ' || count(*) filter (where is_red = 1) from biz_voucher where source_type='INVOICE'" | sed 's/^/        /'
psql_q "select 'biz_status_log(INVOICE): ' || count(*) from biz_status_log where entity_type='INVOICE'" | sed 's/^/        /'
check "发票 4 张（作废的仍在表里：VOID 1 + POSTED 1 + DRAFT 2）" "$(psql_q "select count(*) from biz_invoice")" "4"
check "蓝字凭证 2 张（INV1 已红冲 + INV2）" "$(psql_q "select count(*) from biz_voucher where source_type='INVOICE' and is_red=0")" "2"
check "红字凭证 1 张" "$(psql_q "select count(*) from biz_voucher where source_type='INVOICE' and is_red=1")" "1"
check "发票状态日志 8 条（4 建单 + 3 开票 + 1 作废）" "$(psql_q "select count(*) from biz_status_log where entity_type='INVOICE'")" "8"
check "每张凭证借贷平衡" "$(psql_q "select count(*) from biz_voucher where source_type='INVOICE' and total_debit <> total_credit")" "0"
echo "      手工发票 id=$INV3（仅建单，未开票）"

echo
echo "==================== 结果 ===================="
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
