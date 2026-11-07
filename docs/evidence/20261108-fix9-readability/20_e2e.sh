#!/usr/bin/env bash
# ============================================================================
# FIX-9（t_03122334）145 真机取证 · 三条验收逐条实测
#
#   ② 采购入库凭证摘要：物料没配存货科目（DEMO-P101）时也写**物料编码**，不是 uuid 前 8 位
#   ③ `biz_status_log.operator_name`：只有 operator_id 的路径也写姓名快照
#   ① 审批完成后 `biz_flow_instance.result_comment` = 最后一条审批意见
#
# 前置：11_start_instance.sh 已起 8049（连拷贝库）。跑法：
#   ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix9-readability/20_e2e.sh'
# ============================================================================
set -uo pipefail
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$EV/00_env.sh"

RAW="$EV/raw"
echo "== 环境 ==" | tee "$RAW/10_env.txt"
echo "BASE=$BASE DB=$DBNAME" | tee -a "$RAW/10_env.txt"
echo "schema 版本（拷贝库，启动时已补迁移）= $(sq "select max(version::bigint) from schema_migrations where version ~ '^[0-9]+\$'")" | tee -a "$RAW/10_env.txt"

ADMIN_TOK=$(login "$ADMIN_USER" "$ADMIN_PASS")
if [ -z "$ADMIN_TOK" ]; then echo "❌ admin 登录失败（$ADMIN_USER）"; exit 1; fi
echo "$ADMIN_TOK" > "$TKDIR/admin.tok"
pass "admin 登录（$ADMIN_USER）"

AH="Authorization: Bearer $ADMIN_TOK"
CT='Content-Type: application/json'

# ---------------------------------------------------------------- 夹具
WH=$(sq "select id from biz_warehouse where is_leaf = 1 and status = 1 and deleted_at is null order by warehouse_code limit 1")
SUP=$(sq "select id from biz_supplier where deleted_at is null order by supplier_code limit 1")
MAT=$(sq "select id from biz_material where material_code='DEMO-P101' and deleted_at is null limit 1")
MAT_ACCT=$(sq "select coalesce(inventory_account_id::text,'NULL') from biz_material where id='$MAT'::uuid")
{
  echo "WH=$WH"
  echo "SUP=$SUP"
  echo "MAT=$MAT MAT_CODE=DEMO-P101 MAT_INVENTORY_ACCOUNT=$MAT_ACCT"
} | tee "$RAW/11_fixtures.txt"
if [ -z "$WH" ] || [ -z "$SUP" ] || [ -z "$MAT" ]; then
  echo "❌ 夹具缺失（仓库 / 供应商 / DEMO-P101 物料）"; exit 1
fi
check_eq "NULL" "$MAT_ACCT" "前置：演示物料 DEMO-P101 没有配存货科目（②的退化条件）"

# ================================================================ ② 凭证摘要
echo
echo "== ② 采购入库凭证摘要：物料编码 vs uuid 前 8 位 ==" | tee "$RAW/20_summary.txt"

SI_JSON=$(curl -sS -X POST "$API/business/erp/stock-in" -H "$AH" -H "$CT" -d "{
  \"supplier_id\":\"$SUP\",
  \"is_sourceless\":true,
  \"warehouse_id\":\"$WH\",
  \"remark\":\"FIX-9 ② 凭证摘要可读性验证（无源入库）\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":3,\"price_excl\":100,\"tax_rate\":13}]
}")
echo "$SI_JSON" | jq -c '{code,msg,no:.data.header.in_no,status:.data.header.status}' | tee -a "$RAW/20_summary.txt"
SI=$(echo "$SI_JSON" | jq -r '.data.header.id // empty')
SI_NO=$(echo "$SI_JSON" | jq -r '.data.header.in_no // empty')
IN_TYPE=$(sq "select in_type from biz_stock_in where id='$SI'::uuid")
if [ -z "$SI" ]; then echo "❌ 建收货单失败"; fail "建无源收货单"; summary; exit 1; fi
pass "建无源收货单 $SI_NO（in_type=$IN_TYPE）"
check_eq "SOURCELESS" "$IN_TYPE" "无源入库 in_type 落 SOURCELESS（FIX-7 口径）"

POST_CODE=$(curl -sS -o "$RAW/21_post.json" -w '%{http_code}' -X PATCH "$API/business/erp/stock-in/$SI/status" \
  -H "$AH" -H "$CT" -d '{"status":"POSTED"}')
echo "过账 HTTP=$POST_CODE $(jq -c '{code,msg}' "$RAW/21_post.json")" | tee -a "$RAW/20_summary.txt"
check_eq "200" "$POST_CODE" "收货单过账（POSTED）"

V=$(sq "select coalesce(voucher_id::text,'') from biz_stock_in where id='$SI'::uuid")
echo "凭证 id=$V" | tee -a "$RAW/20_summary.txt"
psqlc -tAF'|' -c "select line_no, summary, debit_amount, credit_amount from biz_voucher_entry
                    where voucher_id='$V'::uuid order by line_no" | tee -a "$RAW/20_summary.txt"
SUMMARIES=$(psqlc -tAc "select string_agg(summary, ' ⏎ ' order by line_no) from biz_voucher_entry where voucher_id='$V'::uuid")
contains "$SUMMARIES" "存货 物料 DEMO-P101（第 1 行）" "凭证存货行摘要写物料编码"
if printf '%s' "$SUMMARIES" | grep -qE "物料 [0-9a-f]{8}（"; then
  fail "摘要仍出现 uuid 前 8 位：$SUMMARIES"
else
  pass "摘要里没有 uuid 前 8 位"
fi

# ================================================================ ③ 姓名快照
echo
echo "== ③ biz_status_log.operator_name 姓名快照 ==" | tee "$RAW/30_operator_name.txt"
psqlc -tAF'|' -c "select from_status, to_status, coalesce(operator_id::text,'-'), coalesce(operator_name,'<NULL>'), coalesce(reason,'-')
                    from biz_status_log where entity_type='STOCK_IN' and entity_id='$SI'::uuid order by created_at"
psqlc -tAF'|' -c "select from_status, to_status, coalesce(operator_id::text,'-'), coalesce(operator_name,'<NULL>') 
                    from biz_status_log where entity_type='STOCK_IN' and entity_id='$SI'::uuid order by created_at" >> "$RAW/30_operator_name.txt"
BLANK=$(sq "select count(*) from biz_status_log where entity_type='STOCK_IN' and entity_id='$SI'::uuid and coalesce(operator_name,'')=''")
TOTAL=$(sq "select count(*) from biz_status_log where entity_type='STOCK_IN' and entity_id='$SI'::uuid")
echo "状态日志 $TOTAL 行 / operator_name 为空 $BLANK 行" | tee -a "$RAW/30_operator_name.txt"
check_eq "0" "$BLANK" "该单据的状态日志没有一行 operator_name 为空"
ADMIN_NAME=$(sq "select coalesce(nullif(btrim(real_name),''), username) from sys_user where username='$ADMIN_USER'")
NAMES=$(sq "select string_agg(distinct operator_name, ',') from biz_status_log where entity_type='STOCK_IN' and entity_id='$SI'::uuid")
contains "$NAMES" "$ADMIN_NAME" "operator_name = 超级管理员快照（real_name 优先）"

# ================================================================ ① 审批意见回写
echo
echo "== ① 审批意见回写 biz_flow_instance.result_comment ==" | tee "$RAW/40_result_comment.txt"
ST_JSON=$(curl -sS -X POST "$API/business/erp/stock-take" -H "$AH" -H "$CT" -d "{
  \"warehouse_id\":\"$WH\",
  \"remark\":\"FIX-9 ① 审批意见回写验证\"
}")
echo "$ST_JSON" | jq -c '{code,msg,no:.data.take_no,status:.data.status}' | tee -a "$RAW/40_result_comment.txt"
ST=$(echo "$ST_JSON" | jq -r '.data.id // empty')
if [ -z "$ST" ]; then echo "❌ 建盘点单失败"; fail "建盘点单"; summary; exit 1; fi
pass "建盘点单 $(echo "$ST_JSON" | jq -r '.data.take_no')"

SUB_CODE=$(curl -sS -o "$RAW/41_submit.json" -w '%{http_code}' -X PATCH "$API/business/erp/stock-take/$ST/status" \
  -H "$AH" -H "$CT" -d '{"status":"PENDING_POST"}')
echo "提交审批 HTTP=$SUB_CODE $(jq -c '{code,msg}' "$RAW/41_submit.json")" | tee -a "$RAW/40_result_comment.txt"
check_eq "200" "$SUB_CODE" "盘点单提交审批（DRAFT → PENDING_POST）"

INST=$(sq "select coalesce(approval_instance_id::text,'') from biz_stock_take where id='$ST'::uuid")
echo "审批实例=$INST" | tee -a "$RAW/40_result_comment.txt"
if [ -z "$INST" ]; then echo "❌ 没挂上审批实例"; fail "盘点单挂审批实例"; summary; exit 1; fi
pass "盘点单挂上审批实例"

COMMENT="FIX-9 ① 审批意见回写验证：实盘差异已核对，同意调平"
echo "审批意见=$COMMENT" >> "$RAW/40_result_comment.txt"

# 审批人可能是 demo_lead / fin_drill / gm_drill；巡检这三个演示账号能否登录，
# 不能就用**拷贝库**里 admin 的密码哈希覆盖（只在拷贝库，cloudpivot_dev 一个字节不动）。
APPROVERS=$(psqlc -tAc "select distinct u.username from biz_flow_task t join sys_user u on u.id=t.assignee_id
                         where t.instance_id='$INST'::uuid")
echo "节点审批人候选：$APPROVERS" | tee -a "$RAW/40_result_comment.txt"
declare -A TOK
for U in $APPROVERS; do
  T=$(login "$U" "${DEMO_PASS:-Demo@123456}")
  if [ -z "$T" ]; then
    psqlc -q -c "update sys_user set password=(select password from sys_user where username='$ADMIN_USER'),
                    must_change_password=false where username='$U'" >/dev/null
    echo "（演示账号 $U 密码不同 → 已在**拷贝库**用 admin 哈希覆盖）" | tee -a "$RAW/40_result_comment.txt"
    T=$(login "$U" "$ADMIN_PASS")
  fi
  TOK["$U"]="$T"
done

ROUNDS=0
while [ "$ROUNDS" -lt 12 ]; do
  ROW=$(psqlc -tAF'|' -c "select t.id, u.username from biz_flow_task t join sys_user u on u.id=t.assignee_id
                          where t.instance_id='$INST'::uuid and t.status='PENDING'
                          order by t.created_at limit 1")
  [ -z "$ROW" ] && break
  TID=${ROW%%|*}; U=${ROW##*|}
  TOKEN=${TOK[$U]:-}
  if [ -z "$TOKEN" ]; then
    echo "❌ 待办审批人 $U 没有可用令牌"; fail "审批人 $U 登录"; break
  fi
  C=$(curl -sS -o "$RAW/42_approve_${ROUNDS}.json" -w '%{http_code}' -X POST "$API/business/flow/task/$TID/approve" \
    -H "Authorization: Bearer $TOKEN" -H "$CT" -d "{\"comment\":\"$COMMENT\"}")
  echo "第 $((ROUNDS + 1)) 票：$U 审批 HTTP=$C $(jq -c '{code,msg,status:.data.instance_status}' "$RAW/42_approve_${ROUNDS}.json")" | tee -a "$RAW/40_result_comment.txt"
  if [ "$C" != "200" ]; then fail "审批人 $U 的同意请求（HTTP $C）"; break; fi
  ROUNDS=$((ROUNDS + 1))
  STATUS=$(sq "select status from biz_flow_instance where id='$INST'::uuid")
  [ "$STATUS" != "PENDING" ] && break
done

INST_STATUS=$(sq "select status from biz_flow_instance where id='$INST'::uuid")
echo "实例状态=$INST_STATUS（共投 $ROUNDS 票）" | tee -a "$RAW/40_result_comment.txt"
check_eq "APPROVED" "$INST_STATUS" "审批实例到终态 APPROVED"

echo "--- 待办行（意见本来就在这里）---" | tee -a "$RAW/40_result_comment.txt"
psqlc -tAF'|' -c "select node_key, status, assignee_id, coalesce(comment,'<NULL>') from biz_flow_task
                    where instance_id='$INST'::uuid order by created_at" | tee -a "$RAW/40_result_comment.txt"
TASK_COMMENT=$(psqlc -tAc "select coalesce(comment,'') from biz_flow_task where instance_id='$INST'::uuid and status='APPROVED' order by completed_at desc limit 1")
contains "$TASK_COMMENT" "$COMMENT" "待办行 comment 有审批意见（引擎原本的落点）"

RESULT_COMMENT=$(psqlc -tAc "select coalesce(result_comment,'<NULL>') from biz_flow_instance where id='$INST'::uuid")
echo "--- 实例 result_comment = 「$RESULT_COMMENT」---" | tee -a "$RAW/40_result_comment.txt"
check_eq "$COMMENT" "$RESULT_COMMENT" "实例 result_comment 回写了最后一条意见"

summary
