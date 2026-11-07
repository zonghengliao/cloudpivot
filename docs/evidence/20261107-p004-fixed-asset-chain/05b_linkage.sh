#!/usr/bin/env bash
# ⑦-附 FIXED_ASSET 联动：dev 库 biz_flow_linkage_rule 原本**没有** FIXED_ASSET 规则
#       ⇒ 配一条「停用 → 通知制单人」「启用 → 通知制单人」，再走一遍停用/启用，证明 hook 真的会发通知
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
A=$(cat "$EV/raw/asset.id")

echo "===== ⑦-7 配置前：FIXED_ASSET 规则行数 ====="
psqlq "select coalesce(count(*),0) as fixed_asset_rules from biz_flow_linkage_rule where trigger_entity = 'FIXED_ASSET'"

echo "===== ⑦-8 配规则：FIXED_ASSET/IDLE → SEND_NOTICE ====="
api PUT "/business/flow/linkage" "$ADMIN" '{"name":"演练：资产停用 → 通知制单人","trigger_entity":"FIXED_ASSET","trigger_status":"IDLE","action_code":"SEND_NOTICE","action_params":{"recipient":"CREATOR"},"enabled":true,"seq":10,"confirm":true}' > "$EV/raw/l1_rule_idle.json"
echo "http=$HTTP $(jq -c '{code,msg,data:{id:.data.id,enabled:.data.enabled}}' "$EV/raw/l1_rule_idle.json")"

echo "===== ⑦-9 配规则：FIXED_ASSET/IN_USE → SEND_NOTICE ====="
api PUT "/business/flow/linkage" "$ADMIN" '{"name":"演练：资产启用 → 通知制单人","trigger_entity":"FIXED_ASSET","trigger_status":"IN_USE","action_code":"SEND_NOTICE","action_params":{"recipient":"CREATOR"},"enabled":true,"seq":10,"confirm":true}' > "$EV/raw/l2_rule_inuse.json"
echo "http=$HTTP $(jq -c '{code,msg,data:{id:.data.id,enabled:.data.enabled}}' "$EV/raw/l2_rule_inuse.json")"

echo "===== ⑦-10 再走一遍：停用 → 启用（第二次事件必须再发一条）====="
api POST "/business/erp/asset/$A/status" "$ADMIN" '{"target_status":"IDLE","change_date":"2026-06-02","reason":"演练停用（配规则后复验）"}' > "$EV/raw/s5_idle2.json"
echo "停用 http=$HTTP $(jq -c '{code,status:.data.asset.status}' "$EV/raw/s5_idle2.json")"
api POST "/business/erp/asset/$A/status" "$ADMIN" '{"target_status":"IN_USE","change_date":"2026-06-03","reason":"演练启用（配规则后复验）"}' > "$EV/raw/s6_enable2.json"
echo "启用 http=$HTTP $(jq -c '{code,status:.data.asset.status}' "$EV/raw/s6_enable2.json")"

echo "===== ⑦-11 通知落库（应恰好 2 条，幂等键 = LNK:FIXED_ASSET:<状态>:<卡片>:<变动行>）====="
sleep 1
psqlq "select notice_no, biz_type, title, content, status, idempotency_key from sys_notice where biz_type = 'FIXED_ASSET' order by created_at"
psqlq "select notice_no, target_type, target_ids, receiver_count, dispatch_status from sys_notice where biz_type = 'FIXED_ASSET' order by created_at"

echo "===== ⑦-12 变动留痕全量（4 次事件）====="
psqlq "select change_type, change_date, from_status, to_status, reason, id from biz_fixed_asset_change order by created_at"
