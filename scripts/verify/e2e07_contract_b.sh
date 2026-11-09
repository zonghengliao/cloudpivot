#!/usr/bin/env bash
# E2E-07 阶段 B：审批（两道路径只联动一次）→ 履约挂单/额度守卫 → 到期提醒 → 归档/终止
# 在 145 上跑：bash e2e07_contract_b.sh
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e07_contract_common.sh"
TODAY=$(date +%F)

T_ADMIN=$(login admin "$NIK_ADMIN_PASSWORD" admin) || exit 1; save T_ADMIN "$T_ADMIN"
T_APPR=$(login e2e01_appr 'E2e01@Appr123' appr) || exit 1; save T_APPR "$T_APPR"
T_APPR07=$(login e2e07_appr 'E2e07@Appr123' appr07) || exit 1; save T_APPR07 "$T_APPR07"
T_P3A3=$(login p3a_0003 'E2e01@Appr123' p3a3) || exit 1; save T_P3A3 "$T_P3A3"

tok_of() { case "$1" in admin) printf '%s' "$T_ADMIN";; e2e01_appr) printf '%s' "$T_APPR";; e2e07_appr) printf '%s' "$T_APPR07";; p3a_0003) printf '%s' "$T_P3A3";; *) printf '';; esac; }
task_of() { SQ "select id from biz_flow_task where instance_id='$1' and node_key='$2' and status='PENDING' order by created_at desc limit 1"; }
approve() { api "approve_$1" POST "/business/flow/task/$2/approve" "$3" "{\"comment\":\"$4\"}"; }

# 审批一个节点上的**全部**待办（合同流程 PM 节点是 ALL 会签：3 个采购经理都要签）
approve_node() { # approve_node <tag> <instance_id> <node_key> [limit]
  local tag=$1 inst=$2 node=$3 limit=${4:-99} n=0 row tid uname tok
  while IFS='|' read -r tid uname; do
    [ -z "${tid:-}" ] && continue
    [ "$n" -ge "$limit" ] && break
    tok=$(tok_of "$uname")
    if [ -z "$tok" ]; then
      bad "节签失败：$node 派给 $uname，没有该账号令牌"; continue
    fi
    approve "${tag}_$n" "$tid" "$tok" "E2E07 $node 会签通过（$uname）"
    n=$((n+1))
    check "会签 $node / $uname" "$HTTP/c$CODE" "200/c0"
  done < <(PSQLQ -c "select t.id||'|'||coalesce(u.username,'-') from biz_flow_task t left join sys_user u on u.id=t.assignee_id where t.instance_id='$inst' and t.node_key='$node' and t.status='PENDING' order by u.username")
  check "会签节点 $node 待办已清空" "$(SQ "select count(*) from biz_flow_task where instance_id='$inst' and node_key='$node' and status='PENDING'")" "0"
}

say "### B0 临时联动规则（合同签订 → 通知制单人）——默认库里 CONTRACT 零规则，加一条才能观测 LNK-07"
PSQLQ -v ON_ERROR_STOP=1 -c "insert into biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code, action_params, enabled, seq)
  select gen_random_uuid(), 'E2E07 临时：合同签订→通知制单人', 'CONTRACT', 'SIGNED', 'SEND_NOTICE',
         '{\"mode\":\"TAKEOVER\",\"recipient\":\"CREATOR\"}'::jsonb, true, 90
   where not exists (select 1 from biz_flow_linkage_rule where name like 'E2E07 临时%')" > /dev/null
check "临时联动规则已就绪（收口时删除）" "$(SQ "select count(*) from biz_flow_linkage_rule where trigger_entity='CONTRACT' and trigger_status='SIGNED' and name like 'E2E07 临时%'")" "1"

say "### B1 C1（PURCHASE 12 万·两级审批）"
check "C1 第一级待办 n_pm_hi（ALL 会签 3 人）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I1' and node_key='n_pm_hi' and status='PENDING'")" "3"
T1HI=$(task_of "$I1" n_pm_hi); save T1HI "$T1HI"
approve c1_admin "$T1HI" "$T_ADMIN" "超管代批试探"
check "非会签人（admin 不是待办人）审批被拒" "$HTTP" "403"
approve_node c1lv1 "$I1" n_pm_hi
check "C1 一级会签后仍审批中" "$(SQ "select status from biz_contract where id='$C1'")" "PENDING_APPROVAL"
check "C1 二级待办 n_gm 已生成（ANY 任一即可）" "$(SQ "select case when count(*) >= 1 then 'yes' else 'no' end from biz_flow_task where instance_id='$I1' and node_key='n_gm' and status='PENDING'")" "yes"
APPR01_ID=$(SQ "select id from sys_user where username='e2e01_appr'")
T1GM=$(SQ "select id from biz_flow_task where instance_id='$I1' and node_key='n_gm' and status='PENDING' and assignee_id='$APPR01_ID' limit 1")
check "C1 二级待办里能找到 e2e01_appr 那一票（ANY 由他签）" "$([ -n "$T1GM" ] && echo yes || echo no)" "yes"
approve c1_gm "$T1GM" "$T_APPR" "E2E07 总经理通过"
check "C1 二级审批 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "C1 审批通过 → SIGNED" "$(SQ "select status from biz_contract where id='$C1'")" "SIGNED"
check "C1 状态日志两条迁移" "$(SQ "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C1'")" "2"
check "C1 SIGNED 联动通知 1 条（幂等键）" "$(SQ "select count(*) from sys_notice where biz_type='CONTRACT' and biz_id='$C1' and idempotency_key='LNK:CONTRACT:SIGNED:$C1'")" "1"
approve c1_replay "$T1GM" "$T_APPR" "E2E07 重放审批"
check "重放审批是幂等返回（不报错但无副作用）" "$HTTP/c$CODE" "200/c0"
check "重放返回 idempotent=true" "$(data '.data.idempotent')" "true"
check "重放返回 approved_count=0（没有多记一票）" "$(data '.data.approved_count')" "0"
check "重放后 SIGNED 联动通知仍 1 条" "$(SQ "select count(*) from sys_notice where biz_type='CONTRACT' and biz_id='$C1' and idempotency_key='LNK:CONTRACT:SIGNED:$C1'")" "1"
api c1_resign PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"SIGNED"}'
check "已 SIGNED 再走路径②被状态机挡下" "$HTTP/c$CODE" "409/c66015"
check "路径②被挡后 SIGNED 联动通知仍 1 条" "$(SQ "select count(*) from sys_notice where biz_type='CONTRACT' and biz_id='$C1' and idempotency_key='LNK:CONTRACT:SIGNED:$C1'")" "1"
detail_of c1a "$C1" "$T_ADMIN"
check "SIGNED 的合法去向" "$CT_NEXT" "ACTIVE,TERMINATED"
api c1_active PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "C1 生效 → ACTIVE" "$HTTP/c$CODE" "200/c0"

say "### B2 C2（SALES·单级会签；「两条审批路径只联动一次」专项）"
check "C2 单级待办 n_pm（<10 万，ALL 会签 3 人）" "$(SQ "select count(*) from biz_flow_task where instance_id='$I2' and node_key='n_pm' and status='PENDING'")" "3"
api c2_sign PATCH "/business/erp/contract/$C2/status" "$T_ADMIN" '{"target_status":"SIGNED"}'
check "路径②：handler 直接登记签订" "$HTTP/c$CODE" "200/c0"
check "C2 状态 SIGNED" "$(SQ "select status from biz_contract where id='$C2'")" "SIGNED"
check "登记签订后仍保留审批实例 id（审计，V0209）" "$(SQ "select case when approval_instance_id is null then 'null' else 'kept' end from biz_contract where id='$C2'")" "kept"
api c2_detail GET "/business/erp/contract/$C2" "$T_ADMIN"
check "详情可见审批单号 FLW-…" "$(printf '%s' "$(data '.data.approval_instance_no')" | grep -c '^FLW')" "1"
check "C2 SIGNED 联动通知 1 条" "$(SQ "select count(*) from sys_notice where biz_type='CONTRACT' and biz_id='$C2' and idempotency_key='LNK:CONTRACT:SIGNED:$C2'")" "1"
check "C2 PENDING→SIGNED 状态日志 1 行" "$(SQ "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C2' and to_status='SIGNED'")" "1"
approve_node c2lv "$I2" n_pm
check "路径①回调后 SIGNED 联动通知仍 1 条（不重复联动）" "$(SQ "select count(*) from sys_notice where biz_type='CONTRACT' and biz_id='$C2' and idempotency_key='LNK:CONTRACT:SIGNED:$C2'")" "1"
check "路径①回调后 SIGNED 状态日志仍 1 行" "$(SQ "select count(*) from biz_status_log where entity_type='CONTRACT' and entity_id='$C2' and to_status='SIGNED'")" "1"
check "回调后合同仍是 SIGNED（没有被二次推进）" "$(SQ "select status from biz_contract where id='$C2'")" "SIGNED"
api c2_active PATCH "/business/erp/contract/$C2/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "C2 生效 → ACTIVE" "$HTTP/c$CODE" "200/c0"

say "### B3 C3（SERVICE：履约「不适用」口径 + 归档）"
approve_node c3lv "$I3" n_pm
check "C3 审批通过 → SIGNED" "$(SQ "select status from biz_contract where id='$C3'")" "SIGNED"
detail_of c3s "$C3" "$T_ADMIN"
check "C3 状态 SIGNED" "$CT_STATUS" "SIGNED"
check "C3 履约口径 NOT_APPLICABLE（CTR-02-F1）" "$CT_SCOPE" "NOT_APPLICABLE"
chknum "C3 已执行金额恒 0（不适用，不是未执行）" "$CT_COMMITTED" "0"
chknum "C3 剩余额度 = 合同额" "$CT_REMAINING" "50000"
check "C3 未关闭单据 0" "$CT_OPEN" "0"
api po_on_service POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C3\",\"warehouse_id\":\"$WH_ID\",\"order_date\":\"$TODAY\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":1,\"price_excl\":1,\"tax_rate\":0}]}"
check "SERVICE 合同不可挂采购单" "$HTTP/c$CODE" "400/c66014"
api c3_active PATCH "/business/erp/contract/$C3/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "C3 生效" "$HTTP/c$CODE" "200/c0"
api c3_archive PATCH "/business/erp/contract/$C3/status" "$T_ADMIN" '{"target_status":"COMPLETED"}'
check "C3 归档 → COMPLETED" "$HTTP/c$CODE" "200/c0"
detail_of c3d "$C3" "$T_ADMIN"
check "归档后状态" "$CT_STATUS" "COMPLETED"
check "归档后无后续状态" "$CT_NEXT" ""

say "### B4 C4（LEASE：自审内控 → 到期提醒 → 终止）"
check "C4 制单人本人的待办被内控置 SKIPPED（不派给自己）" "$(SQ "select status from biz_flow_task where instance_id='$I4' and node_key='n_pm' and assignee_id='$APPR07_ID'")" "SKIPPED"
T4PM=$(task_of "$I4" n_pm); save T4PM "$T4PM"
approve c4_self "$T4PM" "$T_APPR07" "本人审批自己提交的合同"
check "自审被拒（403）" "$HTTP" "403"
check "自审错误码" "$CODE" "65019"
contains "自审拒绝文案点明制单与审批分离" "$(printf '%s' "$BODY" | jq -r '.msg // ""')" "制单与审批分离"
approve_node c4lv "$I4" n_pm
check "C4 会签通过 → SIGNED" "$(SQ "select status from biz_contract where id='$C4'")" "SIGNED"
api c4_active PATCH "/business/erp/contract/$C4/status" "$T_ADMIN" '{"target_status":"ACTIVE"}'
check "C4 生效" "$HTTP/c$CODE" "200/c0"
detail_of c4a "$C4" "$T_ADMIN"
check "LEASE 履约口径 NOT_APPLICABLE" "$CT_SCOPE" "NOT_APPLICABLE"
chknum "LEASE 已执行金额 0（不适用）" "$CT_COMMITTED" "0"

say "### B5 到期提醒（D-30 档）"
api rem1 POST /business/erp/reminder/contract_expire/run "$T_ADMIN" ""
check "到期提醒手动触发 HTTP/CODE" "$HTTP/c$CODE" "200/c0"
check "C4 提醒台账 1 行" "$(SQ "select count(*) from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C4'")" "1"
check "提醒档位 D-30" "$(SQ "select stage from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C4' limit 1")" "D-30"
check "C4 到期提醒通知已发" "$(SQ "select count(*) from sys_notice where biz_type='contract_expire' and biz_id='$C4'")" "1"
check "提醒收件人非空" "$(SQ "select case when receiver_count>0 then 'yes' else 'no' end from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C4' limit 1")" "yes"
api rem2 POST /business/erp/reminder/contract_expire/run "$T_ADMIN" ""
check "二次触发 HTTP" "$HTTP" "200"
check "二次触发 skipped_reminded ≥ 1" "$(data '.data.runs[0].skipped_reminded >= 1')" "true"
check "C4 提醒台账仍 1 行（不重复）" "$(SQ "select count(*) from biz_reminder_log where doc_type='CONTRACT' and doc_id='$C4'")" "1"
check "C4 提醒通知仍 1 条" "$(SQ "select count(*) from sys_notice where biz_type='contract_expire' and biz_id='$C4'")" "1"

say "### B6 终止（原因必填 + 两处留痕）"
api c4_term_noreason PATCH "/business/erp/contract/$C4/status" "$T_ADMIN" '{"target_status":"TERMINATED"}'
check "终止缺原因被拒" "$HTTP/c$CODE" "400/c10001"
REASON="出租方提前收回场地（E2E07 演练 $RUN）"
api c4_term PATCH "/business/erp/contract/$C4/status" "$T_ADMIN" "{\"target_status\":\"TERMINATED\",\"reason\":\"$REASON\"}"
check "终止成功" "$HTTP/c$CODE" "200/c0"
check "C4 状态 TERMINATED" "$(SQ "select status from biz_contract where id='$C4'")" "TERMINATED"
contains "终止原因落 biz_status_log.reason" "$(SQ "select coalesce(reason,'') from biz_status_log where entity_type='CONTRACT' and entity_id='$C4' and to_status='TERMINATED'")" "$REASON"
contains "终止原因落 biz_contract.remark（V-18）" "$(SQ "select coalesce(remark,'') from biz_contract where id='$C4'")" "$REASON"
detail_of c4t "$C4" "$T_ADMIN"
check "终止后无后续状态" "$CT_NEXT" ""
api po_on_lease POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C4\",\"warehouse_id\":\"$WH_ID\",\"order_date\":\"$TODAY\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":1,\"price_excl\":1,\"tax_rate\":0}]}"
check "已终止合同不可挂采购单" "$HTTP/c$CODE" "400/c66014"

say "### B7 履约跟踪：挂采购单 → 已执行金额 = 关联订单合计"
po() { # po <tag> <qty> <price>
  api "po_$1" POST /business/erp/purchase-order "$T_ADMIN" \
    "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C1\",\"warehouse_id\":\"$WH_ID\",\"order_date\":\"$TODAY\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":$2,\"price_excl\":$3,\"tax_rate\":0}]}"
}
po p1 30 1000
check "采购单1（3 万）建档" "$HTTP/c$CODE" "200/c0"
PO1=$(data '.data.order.id'); PO1NO=$(data '.data.order.order_no'); save PO1 "$PO1"; save PO1NO "$PO1NO"
check "采购单1 供应商从合同乙方派生" "$(data '.data.order.supplier_id')" "$SUP_ID"
check "采购单1 合同号快照" "$(data '.data.order.contract_no_snapshot')" "$C1NO"
chknum "采购单1 含税金额" "$(data '.data.order.total_with_tax')" "30000"
po p2 20 1000
check "采购单2（2 万）建档" "$HTTP/c$CODE" "200/c0"
PO2=$(data '.data.order.id'); save PO2 "$PO2"
po over 70000.01 1
check "超合同额 0.01 被拦" "$HTTP/c$CODE" "409/c66019"
po eq 70000 1
check "恰好等于剩余额度放行" "$HTTP/c$CODE" "200/c0"
PO3=$(data '.data.order.id'); save PO3 "$PO3"
detail_of c1f "$C1" "$T_ADMIN"
chknum "已执行金额 = 关联采购单合计" "$CT_COMMITTED" "120000"
chknum "剩余额度 = 0" "$CT_REMAINING" "0"
check "未关闭单据数 = 3" "$CT_OPEN" "3"
check "DB 侧合计一致（SUM total_with_tax）" "$(SQ "select coalesce(sum(total_with_tax),0) from biz_purchase_order where contract_id='$C1' and status<>'CANCELLED' and deleted_at is null")" "120000.0000"
check "未关闭单据单号列表 3 个" "$(data '.data.open_order_nos | length')" "3"

say "### B8 归档（ACTIVE→COMPLETED，不硬拦在途单据）+ 引用保护"
api c1_archive PATCH "/business/erp/contract/$C1/status" "$T_ADMIN" '{"target_status":"COMPLETED"}'
check "C1 归档成功（在途采购单不阻塞）" "$HTTP/c$CODE" "200/c0"
detail_of c1c "$C1" "$T_ADMIN"
check "归档后状态 COMPLETED" "$CT_STATUS" "COMPLETED"
check "归档后仍能看到在途单据数（履约信息不丢）" "$CT_OPEN" "3"
api po_after_archive POST /business/erp/purchase-order "$T_ADMIN" "{\"purchase_type\":\"CONTRACT\",\"contract_id\":\"$C1\",\"warehouse_id\":\"$WH_ID\",\"order_date\":\"$TODAY\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":1,\"price_excl\":1,\"tax_rate\":0}]}"
check "归档后不可再挂新单" "$HTTP/c$CODE" "400/c66014"
api c1_del DELETE "/business/erp/contract/$C1" "$T_ADMIN"
check "被订单引用不可删" "$HTTP/c$CODE" "409/c66018"
contains "删除拒绝文案点名订单号" "$(printf '%s' "$BODY" | jq -r '.msg // ""')" "$PO1NO"

say "### B9 销售合同履约（C2）"
api so1 POST /business/erp/sales-order "$T_ADMIN" \
  "{\"customer_id\":\"$CUS_ID\",\"contract_id\":\"$C2\",\"order_date\":\"$TODAY\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":30,\"price\":1000,\"tax_rate\":0}]}"
check "销售单挂销售合同" "$HTTP/c$CODE" "200/c0"
SO1=$(data '.data.id'); SO1NO=$(data '.data.order_no'); save SO1 "$SO1"; save SO1NO "$SO1NO"
chknum "销售单金额" "$(data '.data.total_with_tax')" "30000"
detail_of c2f "$C2" "$T_ADMIN"
chknum "C2 已执行金额 = 销售单合计" "$CT_COMMITTED" "30000"
chknum "C2 剩余额度" "$CT_REMAINING" "55000"
check "C2 未关闭单据数" "$CT_OPEN" "1"
api so_over POST /business/erp/sales-order "$T_ADMIN" \
  "{\"customer_id\":\"$CUS_ID\",\"contract_id\":\"$C2\",\"order_date\":\"$TODAY\",\"items\":[{\"material_id\":\"$MAT_ID\",\"qty\":55001,\"price\":1,\"tax_rate\":0}]}"
check "销售单超合同额被拦" "$HTTP/c$CODE" "409/c66019"
check "DB 侧销售单合计一致" "$(SQ "select coalesce(sum(total_with_tax),0) from biz_sales_order where contract_id='$C2' and status<>'CANCELLED' and deleted_at is null")" "30000.0000"

say "### B10 联动通知去重 / 合同不记账 收口"
check "本卡四张合同 SIGNED 联动通知各 1 条、无重复" "$(SQ "select count(*) from (select idempotency_key from sys_notice where biz_id in ('$C1','$C2','$C3','$C4') and idempotency_key like 'LNK:CONTRACT:%' group by idempotency_key having count(*)>1) t")" "0"
check "合同不产生凭证（本卡单据来源）" "$(SQ "select count(*) from biz_voucher where source_no in ('$C1NO','$C2NO','$C3NO','$C4NO')")" "0"

say "### B11 收口：删除本卡临时联动规则"
PSQLQ -c "delete from biz_flow_linkage_rule where name like 'E2E07 临时%'" > /dev/null
check "临时联动规则已删除（恢复环境）" "$(SQ "select count(*) from biz_flow_linkage_rule where name like 'E2E07 临时%'")" "0"

say
summary
