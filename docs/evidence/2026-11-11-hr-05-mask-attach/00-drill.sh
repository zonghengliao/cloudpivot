#!/usr/bin/env bash
# HR-05 真库演练：V0388（脱敏/配置）+ V0389（附件类型放宽）
#
# 覆盖：
#   ① 存量基线（17 类老 doc_type 附件各 1 条 + 1 条客户自建规则）→ 放宽后逐类仍在、0 行违规
#   ② up 幂等（重放插入 0 行、约束不重建）
#   ③ down 幂等 + 4 条护栏负向反证（新类型附件 / 客户自建新类型规则 / 有员工身份数据 / 拒绝后库未被改动）
#   ④ 往返（up → down → up）后终态与首轮一致
#
# 用法：bash hr05_drill.sh [库名]     （库由 hr0387_t1 模板重建，脚本内部会 drop/create）
set -uo pipefail

DB="${1:-hr0389_t1}"
PLAIN=/opt/cloudpivot/backend/migrations/business/V0388__hr_mask_config.up.sql
PLAIND=/opt/cloudpivot/backend/migrations/business/V0388__hr_mask_config.down.sql
ATT=/opt/cloudpivot/backend/migrations/business/V0389__hr_attachment_doc_types.up.sql
ATTD=/opt/cloudpivot/backend/migrations/business/V0389__hr_attachment_doc_types.down.sql

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); echo "[PASS] $*"; }
bad()  { FAIL=$((FAIL+1)); echo "[FAIL] $*"; }
chk()  { # chk <描述> <实际> <期望>
    if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：实际 [$2] 期望 [$3]"; fi
}

q() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atq -v ON_ERROR_STOP=1 -c "$1"; }
run_sql_file() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -f "$1"; }
# 期望失败的执行：输出 rc 与错误文本
expect_fail() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -f "$1" 2>&1; }

echo "===== HR-05 真库演练 · 库 $DB · $(date '+%F %T') ====="

# ---------- 0. 由 318 迁移的模板库重建 ----------
su - postgres -c "psql -q -c \"DROP DATABASE IF EXISTS $DB WITH (FORCE)\"" >/dev/null 2>&1
su - postgres -c "psql -q -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER alun TEMPLATE hr0387_t1\"" >/dev/null 2>&1
chk "0.1 基线库迁移数（V0387 之后 = 318）" "$(q 'select count(*) from schema_migrations')" "318"

# ---------- 1. 存量夹具：17 类老 doc_type 附件各 1 条 + 1 条客户自建规则 ----------
q "
WITH kinds(dt) AS (VALUES ('CONTRACT'),('PURCHASE_ORDER'),('STOCK_IN'),('PAYABLE'),('PAYMENT'),
    ('FIXED_ASSET'),('EXPENSE_REPORT'),('INVOICE'),('RECEIPT'),('STOCK_TAKE'),('PURCHASE_RETURN'),
    ('SALES_RETURN'),('STOCK_TRANSFER'),('SUPPLIER'),('CUSTOMER'),('OPENING_BALANCE'),('RECEIPT_REFUND')),
  f AS (
    INSERT INTO sys_file (file_name, original_name, file_key, file_size, file_hash, mime_type, extension)
    SELECT 'fixture-'||dt||'.pdf', 'fixture-'||dt||'.pdf', 'hr05fixture/'||dt||'.pdf', 1024,
           md5('hr05fixture-'||dt), 'application/pdf', 'pdf'
      FROM kinds
    RETURNING id, file_key)
INSERT INTO biz_doc_attachment (doc_type, doc_id, file_id, file_role, file_name, uploader_name, remark)
SELECT replace(split_part(file_key, '/', 2), '.pdf', ''), gen_random_uuid(), id, 'DEFAULT',
       split_part(file_key, '/', 2), 'HR-05 存量夹具', 'HR-05 演练：放宽前就存在的历史附件'
  FROM f;
" >/dev/null
q "INSERT INTO biz_attachment_rule (doc_type, file_role, required_at, min_count, max_count, remark)
   VALUES ('CONTRACT', 'CONTRACT_SCAN', 'SUBMIT', 1, NULL, '客户自建：合同扫描件（不属于 HR-05，回滚不许动它）');" >/dev/null

chk "1.1 存量附件行数（17 类各 1 条）"        "$(q 'select count(*) from biz_doc_attachment')" "17"
chk "1.2 存量附件覆盖的 doc_type 种类数"      "$(q 'select count(distinct doc_type) from biz_doc_attachment')" "17"
chk "1.3 存量约束取值数（V0368 之后 = 17）"   "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type'), '''([A-Z_]{2,40})''', 'g')")" "17"
chk "1.4 存量规则数（V0347 的 9 条 + 客户 1 条）" "$(q 'select count(*) from biz_attachment_rule')" "10"
BASELINE_BY_TYPE="$(q "select string_agg(doc_type||':'||c, ',' order by doc_type) from (select doc_type, count(*) c from biz_doc_attachment group by 1) t")"
echo "       存量基线：$BASELINE_BY_TYPE"

# ---------- 2. up（两个迁移）----------
run_sql_file "$PLAIN" >/dev/null || bad "2.1 V0388 up 执行失败"
run_sql_file "$ATT"   >/dev/null || bad "2.2 V0389 up 执行失败"

chk "2.3 脱敏规则 2 条（biz_employee 全局）" "$(q "select count(*) from sys_field_mask where table_name='biz_employee' and role_id is null and deleted_at is null")" "2"
chk "2.4 id_card 规则取值"  "$(q "select mask_type||'/'||mask_param||'/status='||status from sys_field_mask where table_name='biz_employee' and column_name='id_card' and role_id is null and deleted_at is null")" "2/6,4/status=1"
chk "2.5 bank_account 规则取值" "$(q "select mask_type||'/'||mask_param||'/status='||status from sys_field_mask where table_name='biz_employee' and column_name='bank_account' and role_id is null and deleted_at is null")" "5/4,4/status=1"
chk "2.6 hr 配置 3 条" "$(q "select count(*) from sys_config where category='hr' and deleted_at is null")" "3"
chk "2.7 三条配置键与值" "$(q "select string_agg(config_key||'='||config_value||'/'||config_type, ',' order by sort_order) from sys_config where category='hr' and deleted_at is null")" "hr.salary.iit_mode=MANUAL/STRING,hr.salary.si_mode=MANUAL/STRING,hr.attendance.require_leave_match=TRUE/BOOLEAN"
chk "2.8 附件类型约束取值数（17 → 21）" "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type'), '''([A-Z_]{2,40})''', 'g')")" "21"
chk "2.9 规则表约束取值数（17 → 21）"   "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_attachment_rule_doc_type'), '''([A-Z_]{2,40})''', 'g')")" "21"
chk "2.10 RECEIPT_REFUND 仍在约束里（设计稿 20 值列表会摘掉它）" "$(q "select position('''RECEIPT_REFUND''' in (select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type')) > 0")" "t"
chk "2.11 规则数（10 + 2 条 HR 规则）" "$(q 'select count(*) from biz_attachment_rule')" "12"
chk "2.12 合同必传规则（硬闸）" "$(q "select doc_type||'/'||file_role||'/'||required_at||'/min='||min_count||'/max='||max_count||'/enabled='||enabled from biz_attachment_rule where doc_type='EMPLOYEE_CONTRACT'")" "EMPLOYEE_CONTRACT/CONTRACT_SCAN/SUBMIT/min=1/max=20/enabled=true"
chk "2.13 工资表建议规则（不硬拦 = enabled false，file_role 不用 'OTHER'）" "$(q "select doc_type||'/'||coalesce(file_role,'<null>')||'/'||required_at||'/enabled='||enabled from biz_attachment_rule where doc_type='SALARY_SHEET'")" "SALARY_SHEET/<null>/ACTIVATE/enabled=false"
chk "2.14 存量附件逐类仍在（基线一致）" "$BASELINE_BY_TYPE" "$(q "select string_agg(doc_type||':'||c, ',' order by doc_type) from (select doc_type, count(*) c from biz_doc_attachment group by 1) t")"
chk "2.15 存量附件 0 行违反新约束" "$(q "select count(*) from biz_doc_attachment where doc_type not in ('CONTRACT','PURCHASE_ORDER','STOCK_IN','PAYABLE','PAYMENT','FIXED_ASSET','EXPENSE_REPORT','INVOICE','RECEIPT','STOCK_TAKE','PURCHASE_RETURN','SALES_RETURN','STOCK_TRANSFER','SUPPLIER','CUSTOMER','OPENING_BALANCE','RECEIPT_REFUND','EMPLOYEE','EMPLOYEE_CONTRACT','LEAVE_REQUEST','SALARY_SHEET')")" "0"
chk "2.16 客户自建规则未被改动" "$(q "select count(*) from biz_attachment_rule where remark = '客户自建：合同扫描件（不属于 HR-05，回滚不许动它）'")" "1"
chk "2.17 新类型附件可写入（EMPLOYEE 插入 → 回滚前删掉）" "$(q "WITH f AS (INSERT INTO sys_file (file_name, file_key, file_size, file_hash, extension) VALUES ('probe-emp.pdf','hr05probe/EMP.pdf',2,md5('hr05probe'),'pdf') RETURNING id) INSERT INTO biz_doc_attachment (doc_type, doc_id, file_id, file_role, file_name) SELECT 'EMPLOYEE', gen_random_uuid(), id, 'DEFAULT', 'probe-emp.pdf' FROM f RETURNING id" | grep -c .)" "1"

# ---------- 3. up 幂等（重放）----------
REPLAY1="$(run_sql_file "$PLAIN" 2>&1)"; REPLAY2="$(run_sql_file "$ATT" 2>&1)"
chk "3.1 V0388 重放：5 条 INSERT 全为 0 行" "$(echo "$REPLAY1" | grep -c 'INSERT 0 0')" "5"
chk "3.2 V0389 重放：约束跳过（不重建）" "$(echo "$REPLAY2" | grep -c '已含 4 个新值，跳过')" "2"
chk "3.3 重放后脱敏规则仍 2 条（没插重复）" "$(q "select count(*) from sys_field_mask where table_name='biz_employee'")" "2"
chk "3.4 重放后 hr 配置仍 3 条" "$(q "select count(*) from sys_config where category='hr'")" "3"
chk "3.5 重放后规则仍 12 条" "$(q 'select count(*) from biz_attachment_rule')" "12"

# ---------- 4. down 护栏 A：新类型附件拒绝回滚 ----------
OUT="$(expect_fail "$ATTD")"; RC=$?
chk "4.1 down 因 EMPLOYEE 附件被拒（rc≠0）" "$([ $RC -ne 0 ] && echo yes || echo no)" "yes"
chk "4.2 拒绝报文点名新类型附件" "$(echo "$OUT" | grep -c '回滚被拒绝：biz_doc_attachment 还有 1 条 HR 新类型')" "1"
chk "4.3 拒绝后库未被改动（约束仍 21 值）" "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type'), '''([A-Z_]{2,40})''', 'g')")" "21"
chk "4.4 拒绝后 HR 规则仍在（2 条）" "$(q "select count(*) from biz_attachment_rule where doc_type in ('EMPLOYEE_CONTRACT','SALARY_SHEET')")" "2"

# ---------- 5. down 护栏 B：客户自建的新类型规则拒绝回滚 ----------
q "delete from biz_doc_attachment where doc_type='EMPLOYEE'" >/dev/null
q "insert into biz_attachment_rule (doc_type, file_role, required_at, min_count, remark) values ('LEAVE_REQUEST', null, 'SUBMIT', 1, '客户自建：请假单附件')" >/dev/null
OUT="$(expect_fail "$ATTD")"; RC=$?
chk "5.1 down 因客户自建的 LEAVE_REQUEST 规则被拒" "$([ $RC -ne 0 ] && echo yes || echo no)" "yes"
chk "5.2 拒绝报文点名「不是本迁移种的两条」" "$(echo "$OUT" | grep -c '不是本迁移种的两条')" "1"
chk "5.3 拒绝后约束仍 21 值" "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type'), '''([A-Z_]{2,40})''', 'g')")" "21"

# ---------- 6. down 成功：收回 17 值 ----------
q "delete from biz_attachment_rule where doc_type='LEAVE_REQUEST'" >/dev/null
run_sql_file "$ATTD" >/dev/null || bad "6.1 V0389 down 执行失败"
chk "6.1 约束收回 17 值" "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type'), '''([A-Z_]{2,40})''', 'g')")" "17"
chk "6.2 RECEIPT_REFUND 还在（收回时不许摘老值）" "$(q "select position('''RECEIPT_REFUND''' in (select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type')) > 0")" "t"
chk "6.3 规则表约束收回 17 值" "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_attachment_rule_doc_type'), '''([A-Z_]{2,40})''', 'g')")" "17"
chk "6.4 2 条 HR 规则已删、客户自建规则留下" "$(q "select count(*) from biz_attachment_rule")" "10"
chk "6.5 存量附件逐类仍在（回滚不动存量）" "$(q 'select count(*) from biz_doc_attachment')" "17"
OUT2="$(run_sql_file "$ATTD" 2>&1)"
chk "6.6 down 再执行一遍幂等（删 0 条规则）" "$(echo "$OUT2" | grep -c '删除 HR 附件规则 0 条')" "1"

# ---------- 7. V0388 down 护栏：有员工身份数据拒绝回滚 ----------
run_sql_file "$ATT" >/dev/null   # 附件侧回到 up 态，方便后面 up/down 统一
q "insert into biz_employee (org_id, emp_no, name, dept_id, hire_date, id_card, bank_account)
   select gen_random_uuid(), 'HR05PROBE', '演练人员',
          (select id from sys_dept where deleted_at is null order by sort_order limit 1),
          current_date, '110101199001011234', '6222021234567890'
    where not exists (select 1 from biz_employee where emp_no='HR05PROBE')" >/dev/null
OUT="$(expect_fail "$PLAIND")"; RC=$?
chk "7.1 down 因有员工身份数据被拒" "$([ $RC -ne 0 ] && echo yes || echo no)" "yes"
chk "7.2 拒绝报文点名「明文直接出现」" "$(echo "$OUT" | grep -c '脱敏规则会让这些明文直接出现在列表/详情/导出里')" "1"
chk "7.3 拒绝后脱敏规则仍在（2 条）" "$(q "select count(*) from sys_field_mask where table_name='biz_employee'")" "2"

# ---------- 8. 清干净后 down 成功 + 再 up 往返 ----------
q "update biz_employee set id_card=null, bank_account=null where emp_no='HR05PROBE'" >/dev/null
run_sql_file "$PLAIND" >/dev/null || bad "8.1 V0388 down 执行失败"
chk "8.1 脱敏规则清零" "$(q "select count(*) from sys_field_mask where table_name='biz_employee'")" "0"
chk "8.2 hr 配置清零（含软删行）" "$(q "select count(*) from sys_config where config_key like 'hr.%'")" "0"
OUT2="$(run_sql_file "$PLAIND" 2>&1)"
chk "8.3 down 再执行一遍幂等（DELETE 0）" "$(echo "$OUT2" | grep -c 'DELETE 0')" "2"

run_sql_file "$PLAIN" >/dev/null || bad "8.4 V0388 再 up 失败"
chk "8.4 往返后脱敏规则回 2 条" "$(q "select count(*) from sys_field_mask where table_name='biz_employee'")" "2"
chk "8.5 往返后 hr 配置回 3 条" "$(q "select count(*) from sys_config where category='hr'")" "3"
chk "8.6 往返后附件约束仍 21 值" "$(q "select count(*) from regexp_matches((select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_doc_attachment_type'), '''([A-Z_]{2,40})''', 'g')")" "21"

# 清理演练夹具（保留 HR 种子，便于人工复核）
q "delete from biz_doc_attachment where remark = 'HR-05 演练：放宽前就存在的历史附件'" >/dev/null
q "delete from sys_file where file_key like 'hr05fixture/%'" >/dev/null
q "delete from biz_employee where emp_no='HR05PROBE'" >/dev/null
chk "9.1 夹具清理后附件 0 行" "$(q 'select count(*) from biz_doc_attachment')" "0"

echo
echo "===== 结果：PASS=$PASS FAIL=$FAIL ====="
[ "$FAIL" -eq 0 ] || exit 1
