#!/bin/bash
# LNK-03 取证遗留清理：清掉上一轮跑挂的夹具（应付单 / 付款 / 凭证 / 通知 / 规则），恢复流程
set -u
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -d cloudpivot_so44 -Atq"
$PSQL -c "UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'payable_pay'"
$PSQL -c "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:PAYABLE:%'"
$PSQL -c "DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK03%'"
$PSQL -c "DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_type = 'PAYMENT' AND source_id IN (SELECT id FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-LNK03%')))"
$PSQL -c "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN (SELECT id FROM biz_voucher WHERE source_type = 'PAYMENT' AND source_id IN (SELECT id FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-LNK03%')))"
$PSQL -c "DELETE FROM biz_voucher WHERE source_type = 'PAYMENT' AND source_id IN (SELECT id FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-LNK03%'))"
$PSQL -c "DELETE FROM biz_status_log WHERE entity_type = 'PAYABLE' AND entity_id IN (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-LNK03%')"
$PSQL -c "DELETE FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-LNK03%')"
$PSQL -c "DELETE FROM biz_payable WHERE payable_no LIKE 'AP-LNK03%'"
echo "cleanup done"
$PSQL -c "SELECT 'rules=' || count(*) FROM biz_flow_linkage_rule WHERE name LIKE 'LNK03%'"
$PSQL -c "SELECT 'payables=' || count(*) FROM biz_payable WHERE payable_no LIKE 'AP-LNK03%'"
$PSQL -c "SELECT 'flow=' || status FROM biz_flow_def WHERE flow_code = 'payable_pay'"
