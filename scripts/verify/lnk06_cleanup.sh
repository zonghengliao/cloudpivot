#!/bin/bash
# LNK-06 夹具清场：按摘要前缀删本卡造的凭证（+ 规则 / 通知）
set -u
DB="${1:-cloudpivot_so44}"
export PGPASSWORD=alun
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -Atq <<'SQL'
DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER'
  AND entity_id IN (SELECT id FROM biz_voucher WHERE summary LIKE 'LNK06%');
DELETE FROM biz_voucher_entry WHERE voucher_id IN
  (SELECT id FROM biz_voucher WHERE summary LIKE 'LNK06%');
DELETE FROM biz_voucher WHERE summary LIKE 'LNK06%';
DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK06%';
DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:VOUCHER:%';
SQL
echo "cleaned: $DB"
