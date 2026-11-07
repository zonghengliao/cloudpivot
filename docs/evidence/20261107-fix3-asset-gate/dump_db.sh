#!/usr/bin/env bash
# 在 145 上导出 FIX-3 验收库的关键断言（只读）
# 用法：DB=cloudpivot_fix3n bash dump_db.sh > db-assert.txt
set -uo pipefail
DB="${DB:-cloudpivot_fix3n}"
export PGPASSWORD="$(grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2)"
P=(psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -P pager=off)

echo "### schema_migrations（迁移条数 / 最大版本）"
"${P[@]}" -c "select count(*) as migrations, max(version::int) as max_version from schema_migrations"

echo "### FIXED_ASSET 联动规则（V0323 预置 3 条）"
"${P[@]}" -c "select trigger_status, enabled, action_params, name from biz_flow_linkage_rule where trigger_entity='FIXED_ASSET' order by trigger_status"

echo "### 资产卡片（A 已处置 / B C 在役）"
"${P[@]}" -c "select asset_no, status, original_value, accumulated_depreciation, net_value, start_depreciation_date, disposal_type, disposal_date from biz_fixed_asset order by asset_no"

echo "### 2026-11 折旧批次"
"${P[@]}" -c "select batch_no, period_code, status, asset_count, total_amount from biz_depreciation_batch"

echo "### 2026-11 折旧明细（逐台：A=1900 B=950 C 无行）"
"${P[@]}" -c "select asset_no, period_code, amount, accumulated_after, status from biz_depreciation order by asset_no"

echo "### 资产通知（幂等键 = LNK:FIXED_ASSET:<状态>:<卡片>:<事件>）"
"${P[@]}" -c "select title, biz_type, idempotency_key, dispatch_status from sys_notice where biz_type='FIXED_ASSET' order by created_at"

echo "### 处置凭证（1602 借 1900 / 1606 借 118100 贷 18100 / 1601 贷 120000）"
"${P[@]}" -c "select v.voucher_no, a.account_code, e.debit_amount, e.credit_amount from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id join biz_account a on a.id = e.account_id where v.source_type='FIXED_ASSET_DISPOSAL' order by v.voucher_no, e.line_no"
