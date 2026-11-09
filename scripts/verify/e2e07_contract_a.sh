#!/usr/bin/env bash
# E2E-07 阶段 A：四类合同建档 + 乙方类型闸 + 附件 + 变更留痕 + 状态机负例 + 提交审批
# 在 145 上跑：bash e2e07_contract_a.sh
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e07_contract_common.sh"

TODAY=$(date +%F)
D30=$(date -d '+30 days' +%F)
RUN=${RUN:-$(date +%m%d%H%M)}
save RUN "$RUN"

say "### A0 登录与主档"
T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1; save T_ADMIN "$T_ADMIN"
ids
# 合同审批人（PURCHASE_MANAGER + GENERAL_MANAGER）：自审负例要「本人提交、本人审批」
api user_hit GET "/system/user/page?page=1&page_size=10&username=e2e07_appr" "$T_ADMIN"
APPR07_ID=$(SQ "select id from sys_user where username='e2e07_appr' and deleted_at is null")
if [ -z "$APPR07_ID" ]; then
  api user_create POST /system/user "$T_ADMIN" \
    "{\"username\":\"e2e07_appr\",\"password\":\"E2e07@Appr123\",\"real_name\":\"E2E07 合同审批人\",\"status\":1,\"remark\":\"E2E-07 合同域演练夹具\",\"role_ids\":[\"$PM_ROLE\",\"$GM_ROLE\"]}"
  check "建审批人 e2e07_appr" "$HTTP/c$CODE" "200/c0"
  APPR07_ID=$(data '.data.id // .data')
  APPR07_ID=$(SQ "select id from sys_user where username='e2e07_appr' and deleted_at is null")
else
  ok "审批人 e2e07_appr 已存在（复用）"
fi
save APPR07_ID "$APPR07_ID"
T_APPR07=$(login e2e07_appr 'E2e07@Appr123' appr07) || exit 1; save T_APPR07 "$T_APPR07"
check "e2e07_appr 角色数" "$(SQ "select count(*) from sys_user_role where user_id='$APPR07_ID'")" "2"

# 夹具修复：合同审批 PM 节点是 ALL 会签，会把角色内每个成员都派一张待办；
# p3a_0003（历史夹具）口令未知 → 会签永远凑不齐。把它重置为 e2e01_appr 的哈希（可登录、无信息损失）。
HASH=$(SQ "select password from sys_user where username='e2e01_appr'")
printf "update sys_user set password = '%s', updated_at = now() where username = 'p3a_0003';\n" "$HASH" > "$WORK/reset_p3a3.sql"
PSQLQ -q -f "$WORK/reset_p3a3.sql" > /dev/null 2>&1
check "夹具修复：p3a_0003 口令哈希与 e2e01_appr 一致" "$(SQ "select case when (select password from sys_user where username='p3a_0003') = (select password from sys_user where username='e2e01_appr') then 'yes' else 'no' end")" "yes"
T_P3A3=$(login p3a_0003 'E2e01@Appr123' p3a3) || exit 1; save T_P3A3 "$T_P3A3"
check "p3a_0003 可登录（会签成员齐备）" "$([ -n "$T_P3A3" ] && echo yes || echo no)" "yes"

say "### A1 四类合同建档（PURCHASE / SALES / SERVICE / LEASE）"
mk() { # mk <tag> <type> <party_id> <amount> <sign> <start> <end> <token>
  api "create_$1" POST /business/erp/contract "$8" \
    "{\"name\":\"E2E07-$1-$RUN\",\"contract_type\":\"$2\",\"party_b_id\":\"$3\",\"amount\":$4,\"sign_date\":\"$5\",\"start_date\":\"$6\",\"end_date\":\"$7\",\"owner_id\":\"$ADMIN_ID\"}"
  say "  · create $1 http=$HTTP $(show)"
}
ADMIN_ID=$(SQ "select id from sys_user where username='admin'")
mk C1_PURCHASE PURCHASE "$SUP_ID" 120000 2026-11-01 2026-11-01 2027-10-31 "$T_ADMIN"
C1=$(data '.data.id'); C1NO=$(data '.data.contract_no'); save C1 "$C1"; save C1NO "$C1NO"
check "PURCHASE 建档 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "PURCHASE 状态" "$(data '.data.status')" "DRAFT"
check "PURCHASE 乙方类型" "$(data '.data.party_b_type')" "SUPPLIER"
if printf '%s' "$C1NO" | grep -Eq '^HT-[0-9]{8}-[0-9]{6}$'; then ok "PURCHASE 合同号格式 $C1NO"; else bad "PURCHASE 合同号格式异常：$C1NO"; fi
chknum "PURCHASE 金额" "$(data '.data.amount')" "120000"
check "PURCHASE 乙方名称快照非空" "$([ -n "$(data '.data.party_b_name')" ] && echo yes || echo no)" "yes"

mk C2_SALES SALES "$CUS_ID" 80000 2026-11-02 2026-11-02 2027-09-30 "$T_ADMIN"
C2=$(data '.data.id'); C2NO=$(data '.data.contract_no'); save C2 "$C2"; save C2NO "$C2NO"
check "SALES 建档 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "SALES 乙方类型" "$(data '.data.party_b_type')" "CUSTOMER"

mk C3_SERVICE SERVICE "$SUP_ID" 50000 2026-11-03 2026-11-03 2027-06-30 "$T_ADMIN"
C3=$(data '.data.id'); C3NO=$(data '.data.contract_no'); save C3 "$C3"; save C3NO "$C3NO"
check "SERVICE 建档 HTTP/CODE（CTR-02-F1 四类全开）" "$HTTP/c$CODE" "200/c0"
check "SERVICE 乙方类型=SUPPLIER（服务商登记为供应商）" "$(data '.data.party_b_type')" "SUPPLIER"

mk C4_LEASE LEASE "$SUP_ID" 30000 "$TODAY" "$TODAY" "$D30" "$T_APPR07"
C4=$(data '.data.id'); C4NO=$(data '.data.contract_no'); save C4 "$C4"; save C4NO "$C4NO"
check "LEASE 建档（审批人本人建单，供自审负例）" "$HTTP/c$CODE" "200/c0"
check "LEASE 到期日 = 今天+30（到期提醒 D-30 档）" "$(data '.data.end_date')" "$D30"

say "### A2 类型 / 乙方闸负例"
api bad_type POST /business/erp/contract "$T_ADMIN" \
  "{\"name\":\"E2E07-BAD-$RUN\",\"contract_type\":\"FRAMEWORK\",\"party_b_id\":\"$SUP_ID\",\"amount\":1000}"
check "未来枚举 FRAMEWORK 被类型闸拦下" "$HTTP/c$CODE" "400/c10001"
api bad_party POST /business/erp/contract "$T_ADMIN" \
  "{\"name\":\"E2E07-BADP-$RUN\",\"contract_type\":\"SERVICE\",\"party_b_type\":\"CUSTOMER\",\"party_b_id\":\"$CUS_ID\",\"amount\":1000}"
check "SERVICE 传 CUSTOMER 乙方被拒" "$HTTP/c$CODE" "400/c10001"
contains "SERVICE 乙方错误文案" "$(printf '%s' "$BODY" | jq -r '.msg // ""')" "SUPPLIER"
api bad_sales_party POST /business/erp/contract "$T_ADMIN" \
  "{\"name\":\"E2E07-BADS-$RUN\",\"contract_type\":\"SALES\",\"party_b_id\":\"$SUP_ID\",\"amount\":1000}"
check "SALES 用供应商当乙方被拒（主档类型不匹配）" "$HTTP/c$CODE" "400/c66021"

say "### A3 状态机负例（DRAFT 只能提交审批）"
api draft_active PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "DRAFT→ACTIVE 被拒" "$HTTP/c$CODE" "409/c66015"
api draft_closed PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"CLOSED"}'
check "CLOSED 不是合法状态（实现名 COMPLETED）" "$HTTP/c$CODE" "400/c10001"
api draft_signed PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"SIGNED"}'
check "DRAFT→SIGNED 被拒（必须先走审批）" "$HTTP/c$CODE" "409/c66015"
contains "DRAFT→SIGNED 文案点明先提交审批" "$(printf '%s' "$BODY" | jq -r '.msg // ""')" "PENDING_APPROVAL"

say "### A4 合同附件（CONTRACT_SCAN）"
printf '%%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<</Root 1 0 R>>\n%%%%EOF\n' > "$WORK/e2e07-scan.pdf"
UP=$(curl -s -m 60 -w '\n%{http_code}' -X POST "$BASE/business/attachment/upload" --oauth2-bearer "$T_ADMIN" \
  -F doc_type=CONTRACT -F doc_id="$C1" -F file_role=CONTRACT_SCAN -F remark='E2E07 合同扫描件' \
  -F "file=@$WORK/e2e07-scan.pdf;type=application/pdf;filename=e2e07-contract-scan.pdf")
UP_HTTP=$(printf '%s' "$UP" | tail -1); UP_BODY=$(printf '%s' "$UP" | sed '$d')
printf '%s' "$UP_BODY" > "$RAW/attachment_upload.json"
check "合同附件上传" "$UP_HTTP" "200"
check "合同附件上传 code" "$(printf '%s' "$UP_BODY" | jq -r '.code // empty')" "0"
check "biz_doc_attachment 落库 1 行" "$(SQ "select count(*) from biz_doc_attachment where doc_type='CONTRACT' and doc_id='$C1'")" "1"
check "file_role=CONTRACT_SCAN" "$(SQ "select file_role from biz_doc_attachment where doc_type='CONTRACT' and doc_id='$C1' limit 1")" "CONTRACT_SCAN"
api att_list GET "/business/attachment/list?doc_type=CONTRACT&doc_id=$C1" "$T_ADMIN"
check "附件列表接口条目数" "$(data '.data.list | length')" "1"
UP_NAME=$(SQ "select coalesce(uploader_name,'') from biz_doc_attachment where doc_type='CONTRACT' and doc_id='$C1' limit 1")
check "上传人姓名快照非空（AUD-01，不落 UUID）" "$([ -n "$UP_NAME" ] && echo yes || echo no)" "yes"
check "上传人姓名不是裸 UUID" "$(printf '%s' "$UP_NAME" | grep -Eq '^[0-9a-f]{8}-' && echo uuid || echo name)" "name"

say "### A5 合同变更（DRAFT 才可改）+ 变更留痕 sys_change_log"
api upd1 PUT "/business/erp/contract/$C2" "$T_ADMIN" \
  "{\"amount\":85000,\"end_date\":\"2027-12-31\",\"remark\":\"E2E07 变更：金额 80000→85000，到期 2027-09-30→2027-12-31\"}"
check "草稿改单 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
chknum "改单响应金额" "$(data '.data.amount')" "85000"
check "改单响应到期日" "$(data '.data.end_date')" "2027-12-31"
chknum "DB biz_contract.amount" "$(SQ "select amount from biz_contract where id='$C2'")" "85000"
check "DB end_date" "$(SQ "select end_date::text from biz_contract where id='$C2'")" "2027-12-31"
CL_CNT=$(SQ "select count(*) from sys_change_log where resource='biz_contract' and resource_id='$C2'")
check "sys_change_log 留痕行数" "$CL_CNT" "1"
CL_FIELDS=$(SQ "select changed_fields from sys_change_log where resource='biz_contract' and resource_id='$C2' limit 1")
contains "变更字段含 amount" "$CL_FIELDS" "amount"
contains "变更字段含 end_date" "$CL_FIELDS" "end_date"
contains "变更字段含 remark" "$CL_FIELDS" "remark"
CL_OP=$(SQ "select coalesce(operator_name,'') from sys_change_log where resource='biz_contract' and resource_id='$C2' limit 1")
check "留痕记录了「谁改的」" "$([ -n "$CL_OP" ] && echo yes || echo no)" "yes"
CL_DIFF=$(SQ "select diff::text from sys_change_log where resource='biz_contract' and resource_id='$C2' limit 1")
contains "diff 记录了改前值 80000" "$CL_DIFF" "80000"
contains "diff 记录了改后值 85000" "$CL_DIFF" "85000"
check "留痕有时间戳" "$(SQ "select count(*) from sys_change_log where resource='biz_contract' and resource_id='$C2' and created_at is not null")" "1"
api upd_noop PUT "/business/erp/contract/$C2" "$T_ADMIN" '{"amount":85000}'
check "原值回传不写空日志（HTTP）" "$HTTP/c$CODE" "200/c0"
check "原值回传后 sys_change_log 仍 1 行" "$(SQ "select count(*) from sys_change_log where resource='biz_contract' and resource_id='$C2'")" "1"

say "### A6 提交审批（DRAFT→PENDING_APPROVAL）"
submit() { # submit <tag> <id> <token>
  api "submit_$1" PATCH "/business/erp/contract/$2/status" "$3" '{"target_status":"PENDING_APPROVAL"}'
}
submit C1 "$C1" "$T_ADMIN"
check "C1 提交审批 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "C1 状态" "$(data '.data.status')" "PENDING_APPROVAL"
I1=$(data '.data.approval_instance_id'); save I1 "$I1"
check "C1 回写审批实例 id" "$([ -n "$I1" ] && echo yes || echo no)" "yes"
check "C1 状态日志 DRAFT→PENDING 1 行" "$(SQ "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C1' and from_status='DRAFT' and to_status='PENDING_APPROVAL'")" "1"
FN=$(SQ "select count(*) from sys_notice where biz_type='flow' and (title like '%$C1NO%' or content like '%$C1NO%')")
check "C1 审批待办通知 ≥1 条（按角色成员各一条）" "$([ "${FN:-0}" -ge 1 ] && echo yes || echo no)" "yes"
check "C1 流程实例 biz_type" "$(SQ "select biz_type from biz_flow_instance where id='$I1'")" "CONTRACT_APPROVE"
api resubmit PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"PENDING_APPROVAL"}'
check "重复提交被拒（幂等）" "$HTTP/c$CODE" "409/c66015"

submit C2 "$C2" "$T_ADMIN"
check "C2 提交审批" "$HTTP/c$CODE" "200/c0"
I2=$(data '.data.approval_instance_id'); save I2 "$I2"
api upd_after_submit PUT "/business/erp/contract/$C2" "$T_ADMIN" '{"amount":90000}'
check "C2 提交后草稿改单被拒（审批中冻结）" "$HTTP/c$CODE" "409/c66016"
check "审批中改单不写变更日志" "$(SQ "select count(*) from sys_change_log where resource='biz_contract' and resource_id='$C2'")" "1"

submit C3 "$C3" "$T_ADMIN"
check "C3 提交审批" "$HTTP/c$CODE" "200/c0"
I3=$(data '.data.approval_instance_id'); save I3 "$I3"

submit C4 "$C4" "$T_APPR07"
check "C4 由审批人本人提交" "$HTTP/c$CODE" "200/c0"
I4=$(data '.data.approval_instance_id'); save I4 "$I4"

say
summary
