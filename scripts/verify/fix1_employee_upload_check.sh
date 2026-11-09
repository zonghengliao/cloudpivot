#!/usr/bin/env bash
# 卡面验收①的**最严口径**：只持 EMPLOYEE（不带 DRILL_CLERK / 任何管理岗）的账号，
# 能不能自己传 EXPENSE_REPORT 附件并提交报销。跑在 145（cloudpivot_dev / :8033）。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE=http://127.0.0.1:8033/api
SQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off -c "$1"; }
P=0; F=0
ok()  { P=$((P+1)); echo "  PASS $*"; }
bad() { F=$((F+1)); echo "  FAIL $*"; }
chk() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

ADMIN=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
[ -z "$ADMIN" ] && { echo "FATAL: admin 登录失败"; exit 1; }

DEPT=$(SQ "select id from sys_dept where dept_code='E2E06-BDG' and deleted_at is null")
TYPE=$(SQ "select id from biz_expense_type where type_code='E2E06_TRAVEL' and deleted_at is null")
[ -z "$DEPT" ] && { echo "FATAL: 缺部门 E2E06-BDG（先跑 e2e06_budget_a.sh）"; exit 1; }
[ -z "$TYPE" ] && { echo "FATAL: 缺费用类型 E2E06_TRAVEL（先跑 e2e06_budget_a.sh）"; exit 1; }

echo "== 1) 造一个**只持 EMPLOYEE** 的账号"
EMP_ROLE=$(SQ "select id from sys_role where role_code='EMPLOYEE' and deleted_at is null")
UID_=$(SQ "select id from sys_user where username='e2e06_emp_only'")
if [ -z "$UID_" ]; then
  curl -s -m 30 -o /tmp/emp_create.json -X POST "$BASE/system/user" --oauth2-bearer "$ADMIN" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"e2e06_emp_only\",\"password\":\"Drill@123456\",\"real_name\":\"E2E06 纯员工\",\"dept_id\":\"$DEPT\",\"status\":1,\"role_ids\":[\"$EMP_ROLE\"],\"remark\":\"E2E-06 验收：只持 EMPLOYEE\"}"
  chk "建用户 e2e06_emp_only code" "$(jq -r '.code' /tmp/emp_create.json)" "0"
  UID_=$(SQ "select id from sys_user where username='e2e06_emp_only'")
else
  echo "  · 已存在，复用"
fi
chk "该账号角色数（应为 1，且是 EMPLOYEE）" \
  "$(SQ "select count(*)||'|'||min(r.role_code) from sys_user_role ur join sys_role r on r.id=ur.role_id where ur.user_id='$UID_'::uuid")" "1|EMPLOYEE"

echo "== 2) 登录（口令校验 + 拿到 claims 里的权限）"
TOK=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"e2e06_emp_only","password":"Drill@123456"}' | jq -r '.data.access_token // empty')
[ -z "$TOK" ] && { echo "FATAL: EMPLOYEE 账号登录失败"; exit 1; }
ok "EMPLOYEE 专用账号登录成功"

echo "== 3) 建一张报销单（草稿）"
RUN_TAG=$(date +%H%M%S)
BODY="{\"title\":\"E2E06 EMPLOYEE-only 自助报销 $RUN_TAG\",\"expense_date\":\"2027-03-05\",\"dept_id\":\"$DEPT\",\"items\":[{\"expense_type_id\":\"$TYPE\",\"with_tax_amount\":\"100\",\"tax_rate\":\"0.06\",\"summary\":\"纯员工自助\",\"invoice_no\":\"INV-E2E06-EMP-$RUN_TAG\",\"invoice_type\":\"VAT_SPECIAL\"}]}"
HTTP=$(curl -s -m 60 -o /tmp/emp_exp.json -w '%{http_code}' -X POST "$BASE/business/erp/expense" \
  --oauth2-bearer "$TOK" -H 'Content-Type: application/json' -d "$BODY")
chk "建报销单 http" "$HTTP" "200"
chk "建报销单 code" "$(jq -r '.code' /tmp/emp_exp.json)" "0"
EXP=$(jq -r '.data.report.id // empty' /tmp/emp_exp.json)
[ -z "$EXP" ] && { echo "FATAL: 拿不到报销单 id"; exit 1; }

echo "== 4) 提交前的附件闸（clerk 视角 = /attachment/check，要 attachment:list）"
HTTP=$(curl -s -m 30 -o /tmp/emp_check.json -w '%{http_code}' \
  "$BASE/business/attachment/check?doc_type=EXPENSE_REPORT&doc_id=$EXP" --oauth2-bearer "$TOK")
chk "查附件闸 http" "$HTTP" "200"
chk "查附件闸 code" "$(jq -r '.code' /tmp/emp_check.json)" "0"
chk "闸判定（缺发票 → 不放行）" "$(jq -r '.data.can_proceed' /tmp/emp_check.json)" "false"

echo "== 5) 本人上传发票影像（此前 403）"
printf '%%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<</Root 1 0 R>>\n%%%%EOF\n' > /tmp/emp_invoice.pdf
HTTP=$(curl -s -m 60 -o /tmp/emp_upload.json -w '%{http_code}' -X POST "$BASE/business/attachment/upload" \
  --oauth2-bearer "$TOK" -F "doc_type=EXPENSE_REPORT" -F "doc_id=$EXP" -F "file_role=INVOICE_SCAN" \
  -F "remark=E2E-06 纯 EMPLOYEE 自传" -F "file=@/tmp/emp_invoice.pdf")
chk "本人上传附件 http（曾经 403）" "$HTTP" "200"
chk "本人上传附件 code" "$(jq -r '.code' /tmp/emp_upload.json)" "0"
chk "附件落库" \
  "$(SQ "select count(*) from biz_doc_attachment where doc_type='EXPENSE_REPORT' and doc_id='$EXP'::uuid and deleted_at is null")" "1"
chk "上传人姓名快照（不是 UUID）" \
  "$(SQ "select coalesce(uploader_name,'-') from biz_doc_attachment where doc_type='EXPENSE_REPORT' and doc_id='$EXP'::uuid and deleted_at is null")" "E2E06 纯员工"

echo "== 6) 附件列表 / 下载（attachment:list + 单据读码）"
HTTP=$(curl -s -m 30 -o /tmp/emp_list.json -w '%{http_code}' \
  "$BASE/business/attachment/list?doc_type=EXPENSE_REPORT&doc_id=$EXP" --oauth2-bearer "$TOK")
chk "附件列表 http" "$HTTP" "200"
chk "附件列表条数" "$(jq -r '.data.total' /tmp/emp_list.json)" "1"
chk "闸复查（有发票 → 放行）" \
  "$(curl -s -m 30 "$BASE/business/attachment/check?doc_type=EXPENSE_REPORT&doc_id=$EXP" --oauth2-bearer "$TOK" | jq -r '.data.can_proceed')" "true"

echo "== 7) 提交报销（此前被 400「必须上传发票影像」挡住）"
HTTP=$(curl -s -m 60 -o /tmp/emp_submit.json -w '%{http_code}' -X POST "$BASE/business/erp/expense/$EXP/submit" \
  --oauth2-bearer "$TOK" -H 'Content-Type: application/json' -d '{}')
chk "提交 http" "$HTTP" "200"
chk "提交 code" "$(jq -r '.code' /tmp/emp_submit.json)" "0"
chk "单据状态" "$(SQ "select status from biz_expense_report where id='$EXP'::uuid")" "SUBMITTED"
chk "审批实例已起（直属上级 = e2e06_mgr）" \
  "$(SQ "select coalesce(string_agg(u.username,','),'-') from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id left join sys_user u on u.id=t.assignee_id where i.biz_type='EXPENSE' and i.biz_id='$EXP'::uuid and t.status='PENDING'")" "e2e06_mgr"

echo "== 8) 越权面检查：EMPLOYEE 不该有 attachment:delete / expense:pay"
chk "EMPLOYEE 权限里没有 attachment:delete" \
  "$(SQ "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='EMPLOYEE' and m.auth_code='business:attachment:delete'")" "0"
chk "EMPLOYEE 权限里没有 expense:pay" \
  "$(SQ "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='EMPLOYEE' and m.auth_code='business:erp:expense:pay'")" "0"
HTTP=$(curl -s -m 30 -o /tmp/emp_pay.json -w '%{http_code}' -X POST "$BASE/business/erp/expense/$EXP/pay" \
  --oauth2-bearer "$TOK" -H 'Content-Type: application/json' -d '{"amount":"100"}')
chk "本人调付款接口被拒 http" "$HTTP" "403"

echo
echo "== 汇总：PASS=$P FAIL=$F"
[ "$F" = "0" ]
