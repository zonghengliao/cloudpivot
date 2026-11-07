#!/bin/bash
# LNK-05 取证 / 单测夹具硬清（复跑前先跑这个）
#
# 为什么需要：配置页 DELETE 走**软删**（deleted_at），而唯一索引
# uk_flow_linkage_rule(trigger_entity, trigger_status, action_code) 不含
# WHERE deleted_at IS NULL —— 软删的规则仍占坑位，下一次 PUT 同一条
# (EXPENSE_REPORT, APPROVED, SEND_NOTICE) 会撞唯一键。
#
# 清理口径（两类夹具都要认，否则会留尸）：
#   - 单测直插的单：单号前缀 EXP-LNK05-
#   - 真机取证走 API 建的单：单号是 EXP-YYYYMMDD-NNNNNN，只能靠 remark 认（remark 固定 LNK-05…）
#   - 规则名前缀 LNK05 / 通知幂等键 LNK:EXPENSE:%
# 业务库（cloudpivot_so44）里其它单据一律不动。
#
# 顺序：先把引用单据的子表清干净（通知 → 凭证 → 预算 → 状态日志 → 付款 → 明细），最后删单头。
set -u
DB="${1:-cloudpivot_so44}"
PSQL="psql -h 127.0.0.1 -U alun -d $DB -Atqc"
export PGPASSWORD=alun

REPORTS="(SELECT id FROM biz_expense_report WHERE report_no LIKE 'EXP-LNK05-%' OR remark LIKE 'LNK-05%')"
INSTANCES="(SELECT id FROM biz_flow_instance WHERE biz_type = 'EXPENSE' AND biz_id IN $REPORTS)"
INSTANCE_TEXT="(SELECT id::text FROM biz_flow_instance WHERE biz_type = 'EXPENSE' AND biz_id IN $REPORTS)"

# 流程侧（通知先删：它的 biz_id 是实例 id 的文本，实例删掉就查不到了）
$PSQL "DELETE FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN $INSTANCE_TEXT)"
$PSQL "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN $INSTANCE_TEXT"
$PSQL "DELETE FROM biz_flow_task WHERE instance_id IN $INSTANCES"
$PSQL "DELETE FROM biz_flow_cc WHERE instance_id IN $INSTANCES"
$PSQL "UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE prev_instance_id IN $INSTANCES"
$PSQL "DELETE FROM biz_flow_instance WHERE biz_type = 'EXPENSE' AND biz_id IN $REPORTS"

# 账务侧
$PSQL "DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id IN $REPORTS)"
$PSQL "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN (SELECT id FROM biz_voucher WHERE source_id IN $REPORTS)"
$PSQL "DELETE FROM biz_voucher WHERE source_id IN $REPORTS"
$PSQL "DELETE FROM biz_budget_usage WHERE source_id IN $REPORTS"
$PSQL "DELETE FROM biz_status_log WHERE entity_type = 'EXPENSE_REPORT' AND entity_id IN $REPORTS"
$PSQL "DELETE FROM biz_expense_payment WHERE report_id IN $REPORTS"
$PSQL "DELETE FROM biz_expense_item WHERE report_id IN $REPORTS"
$PSQL "DELETE FROM biz_expense_report WHERE report_no LIKE 'EXP-LNK05-%' OR remark LIKE 'LNK-05%'"

# 联动配置与通知
$PSQL "DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK05%'"
$PSQL "DELETE FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM sys_notice WHERE idempotency_key LIKE 'LNK:EXPENSE:%')"
$PSQL "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:EXPENSE:%'"

echo "LNK-05 取证夹具已硬清（$DB）"
