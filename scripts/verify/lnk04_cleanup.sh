#!/bin/bash
# LNK-04 取证夹具硬清（复跑前先跑这个）
#
# 为什么需要：配置页的 DELETE 走**软删**（`deleted_at`），而唯一索引
# `uk_flow_linkage_rule(trigger_entity, trigger_status, action_code)` 不含
# `WHERE deleted_at IS NULL` —— 软删的规则仍占坑位，下一次 PUT 同一条
# (STOCK_TAKE, POSTED, SEND_NOTICE) 会撞唯一键。
set -u
DB="${1:-cloudpivot_so44}"
PSQL="psql -h 127.0.0.1 -U alun -d $DB -Atqc"
export PGPASSWORD=alun

$PSQL "DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK04%'"
$PSQL "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:STOCK_TAKE:%'"
$PSQL "DELETE FROM biz_inventory_txn WHERE material_id = 'd1a40400-0000-4000-8000-000000000011'::uuid"
$PSQL "DELETE FROM biz_inventory_location WHERE material_id = 'd1a40400-0000-4000-8000-000000000011'::uuid"
$PSQL "DELETE FROM biz_inventory WHERE material_id = 'd1a40400-0000-4000-8000-000000000011'::uuid"
$PSQL "DELETE FROM biz_voucher WHERE source_type = 'STOCK_TAKE' AND source_id IN \
        (SELECT id FROM biz_stock_take WHERE warehouse_id = 'd1a40400-0000-4000-8000-000000000001'::uuid)"
$PSQL "DELETE FROM biz_status_log WHERE entity_id IN \
        (SELECT id FROM biz_stock_take WHERE warehouse_id = 'd1a40400-0000-4000-8000-000000000001'::uuid)"
$PSQL "DELETE FROM biz_stock_take WHERE warehouse_id = 'd1a40400-0000-4000-8000-000000000001'::uuid"
$PSQL "DELETE FROM biz_material WHERE id = 'd1a40400-0000-4000-8000-000000000011'::uuid"
$PSQL "DELETE FROM biz_warehouse WHERE id = 'd1a40400-0000-4000-8000-000000000001'::uuid"
$PSQL "UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'stock_take_post' AND status = 'DRAFT'"

echo "LNK-04 取证夹具已硬清（$DB）"
