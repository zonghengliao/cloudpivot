#!/usr/bin/env bash
# ============================================================================
# P0-FIX 数据修复演练：在**隔离库**上造「内控死锁」夹具 → 跑 maintenance 脚本 → 断言结果
#
# 覆盖的东西（全部真库断言，不打桩）：
#   · 判据能同时抓到 采购订单审批 / 应付付款 两类业务里的「制单人 / 发起人」PENDING 待办
#   · 修复只动 PENDING：APPROVED 行原样保留、待办行**一行不删**
#   · SKIPPED 必须留痕：`completed_at` 非空 + `comment` 写明内控原因
#   · 节点 `approver_count` 按引擎口径重算（不含 SKIPPED）
#   · `--rollback --apply` 能按行级快照精确回滚
#
# 用法（**必须指隔离库**，库名含 `p0fix`）：
#   P0FIX_DB=cloudpivot_p0fix bash scripts/verify/p0fix_deadlock_repair.sh
#   （连接信息取 NIK_DATABASE_*，与启动迁移同口径）
# ============================================================================
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 2

DB_NAME="${P0FIX_DB:-${NIK_DATABASE_NAME:-}}"
case "$DB_NAME" in
  *p0fix*) ;;
  *)
    echo "拒绝执行：P0FIX_DB/ NIK_DATABASE_NAME 必须指向隔离演练库（当前：'${DB_NAME}'）" >&2
    exit 2
    ;;
esac

export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
PSQL=(psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}"
  -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -At -v ON_ERROR_STOP=1)
q() { "${PSQL[@]}" -c "$1"; }

PASS=0
FAIL=0
ok() {
  PASS=$((PASS + 1))
  echo "  [PASS] $1"
}
bad() {
  FAIL=$((FAIL + 1))
  echo "  [FAIL] $1"
}
expect_eq() { # 期望 实际 说明
  if [ "$1" = "$2" ]; then ok "$3（$1）"; else bad "$3（期望 [$1] 实际 [$2]）"; fi
}

# ---------------------------------------------------------------- 夹具（固定 uuid，可重跑）
UID_DEF=0f000000-0000-4000-8000-00000000f001
UID_VER=0f000000-0000-4000-8000-00000000f002
UID_DEF2=0f000000-0000-4000-8000-00000000f011
UID_VER2=0f000000-0000-4000-8000-00000000f012
UID_INST=0f000000-0000-4000-8000-00000000f101
UID_INST2=0f000000-0000-4000-8000-00000000f102
UID_NODE=0f000000-0000-4000-8000-00000000f201
UID_NODE2=0f000000-0000-4000-8000-00000000f202
UID_ORDER=0f000000-0000-4000-8000-00000000f301
UID_PAY=0f000000-0000-4000-8000-00000000f302
UID_CREATOR=0f000000-0000-4000-8000-00000000f401
UID_INITIATOR=0f000000-0000-4000-8000-00000000f402
UID_KEEPER=0f000000-0000-4000-8000-00000000f403
UID_DONE=0f000000-0000-4000-8000-00000000f404

cleanup() {
  q "DELETE FROM biz_flow_task WHERE instance_id IN ('$UID_INST','$UID_INST2')" >/dev/null
  q "DELETE FROM biz_flow_instance_node WHERE instance_id IN ('$UID_INST','$UID_INST2')" >/dev/null
  q "DELETE FROM biz_flow_instance WHERE id IN ('$UID_INST','$UID_INST2')" >/dev/null
  q "DELETE FROM biz_purchase_order WHERE id = '$UID_ORDER'" >/dev/null
  q "DELETE FROM biz_payment WHERE id = '$UID_PAY'" >/dev/null
  q "DELETE FROM biz_payable WHERE id = '0f000000-0000-4000-8000-00000000f303'" >/dev/null
  q "DELETE FROM biz_flow_def_version WHERE id IN ('$UID_VER','$UID_VER2')" >/dev/null
  q "DELETE FROM biz_flow_def WHERE id IN ('$UID_DEF','$UID_DEF2')" >/dev/null
  q "DELETE FROM sys_user_role WHERE user_id IN ('$UID_CREATOR','$UID_INITIATOR','$UID_KEEPER','$UID_DONE')" >/dev/null
  q "DELETE FROM sys_user WHERE id IN ('$UID_CREATOR','$UID_INITIATOR','$UID_KEEPER','$UID_DONE')" >/dev/null
  q "DELETE FROM p0fix_self_approval_backup WHERE snapshot->>'instance_id' IN ('$UID_INST','$UID_INST2')" >/dev/null 2>&1
}

cleanup

echo "== 造夹具（库：$DB_NAME）=="
q "
INSERT INTO sys_user (id, username, password, real_name, status) VALUES
  ('$UID_CREATOR','p0fix_v_creator','x','演练制单人',1),
  ('$UID_INITIATOR','p0fix_v_initiator','x','演练发起人',1),
  ('$UID_KEEPER','p0fix_v_keeper','x','演练仓管',1),
  ('$UID_DONE','p0fix_v_done','x','演练已批人',1);

INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, create_by)
VALUES ('$UID_ORDER','P0FIX-VERIFY-0001',gen_random_uuid(),current_date,gen_random_uuid(),'SUBMITTED','$UID_CREATOR');

INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no, due_date, period_code, amount, unpaid_amount, create_by)
VALUES ('0f000000-0000-4000-8000-00000000f303','P0FIX-VERIFY-AP-0001',gen_random_uuid(),'STOCK_IN',gen_random_uuid(),'P0FIX-VERIFY-IN-0001',current_date,'2026-11',100,100,'$UID_CREATOR');

INSERT INTO biz_payment (id, payment_no, payable_id, amount, pay_date, approval_status, create_by)
VALUES ('$UID_PAY','P0FIX-VERIFY-PAY-0001','0f000000-0000-4000-8000-00000000f303',100,current_date,'NONE','$UID_CREATOR');

INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status, current_version_id, current_version_no)
VALUES ('$UID_DEF','p0fix_verify','P0-FIX 演练','PO_APPROVE','PUBLISHED','$UID_VER',1),
       ('$UID_DEF2','p0fix_verify_pay','P0-FIX 演练付款','PAYABLE_PAY','PUBLISHED','$UID_VER2',1);

INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot, node_count, edge_count, published_at)
VALUES ('$UID_VER','$UID_DEF',1,'PUBLISHED',true,repeat('0',64),'{}'::jsonb,3,2,now()),
       ('$UID_VER2','$UID_DEF2',1,'PUBLISHED',true,repeat('0',64),'{}'::jsonb,3,2,now());

INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, biz_type, biz_id, biz_no, title, initiator_id, status)
VALUES ('$UID_INST','FLW-P0FIX-V-0001','$UID_DEF','$UID_VER',1,'p0fix_verify','P0-FIX 演练','PO_APPROVE','$UID_ORDER','P0FIX-VERIFY-0001','演练采购单','$UID_INITIATOR','PENDING'),
       ('$UID_INST2','FLW-P0FIX-V-0002','$UID_DEF2','$UID_VER2',1,'p0fix_verify_pay','P0-FIX 演练付款','PAYABLE_PAY','$UID_PAY','P0FIX-VERIFY-PAY-0001','演练付款单','$UID_INITIATOR','PENDING');

INSERT INTO biz_flow_instance_node (id, instance_id, node_key, node_name, node_type, round, status, approver_count, approved_count, rejected_count)
VALUES ('$UID_NODE','$UID_INST','n_dept','部门主管审批','APPROVAL',1,'ACTIVE',4,1,0),
       ('$UID_NODE2','$UID_INST2','n_pay','付款审批','APPROVAL',1,'ACTIVE',2,0,0);

-- 与生产 PO-20261031-000002 完全同形：4 个审批人 = {发起人, 制单人, 老六测试号, 已批的人}
INSERT INTO biz_flow_task (id, instance_id, instance_node_id, node_key, round, task_kind, assignee_id, assignee_name, assignee_source, status, is_self_approval, is_auto, is_added_sign, created_at, completed_at)
VALUES
  (gen_random_uuid(),'$UID_INST','$UID_NODE','n_dept',1,'APPROVAL','$UID_INITIATOR','演练发起人','ROLE','PENDING',true ,false,false,now() - interval '3 hour',NULL),
  (gen_random_uuid(),'$UID_INST','$UID_NODE','n_dept',1,'APPROVAL','$UID_CREATOR'  ,'演练制单人','ROLE','PENDING',false,false,false,now() - interval '3 hour',NULL),
  (gen_random_uuid(),'$UID_INST','$UID_NODE','n_dept',1,'APPROVAL','$UID_KEEPER'   ,'演练仓管'  ,'ROLE','PENDING',false,false,false,now() - interval '3 hour',NULL),
  (gen_random_uuid(),'$UID_INST','$UID_NODE','n_dept',1,'APPROVAL','$UID_DONE'     ,'演练已批人','ROLE','APPROVED',false,false,false,now() - interval '3 hour',now() - interval '2 hour'),
  (gen_random_uuid(),'$UID_INST2','$UID_NODE2','n_pay',1,'APPROVAL','$UID_CREATOR','演练制单人','USER','PENDING',false,false,false,now() - interval '1 hour',NULL),
  (gen_random_uuid(),'$UID_INST2','$UID_NODE2','n_pay',1,'APPROVAL','$UID_KEEPER' ,'演练仓管'  ,'USER','PENDING',false,false,false,now() - interval '1 hour',NULL);
" >/dev/null || {
  echo "夹具写入失败" >&2
  exit 3
}
TASKS_BEFORE=$(q "SELECT count(*) FROM biz_flow_task WHERE instance_id IN ('$UID_INST','$UID_INST2')")
expect_eq 6 "$TASKS_BEFORE" "夹具待办行数"

echo
echo "== ① dry-run（不许改数据）=="
BEFORE_PENDING=$(q "SELECT count(*) FROM biz_flow_task WHERE instance_id IN ('$UID_INST','$UID_INST2') AND status='PENDING'")
bash scripts/maintenance/p0fix_self_approval.sh --dry-run >/tmp/p0fix_dryrun.log 2>&1
AFTER_PENDING=$(q "SELECT count(*) FROM biz_flow_task WHERE instance_id IN ('$UID_INST','$UID_INST2') AND status='PENDING'")
expect_eq "$BEFORE_PENDING" "$AFTER_PENDING" "dry-run 后 PENDING 数不变"
if grep -q "命中 PENDING 待办：3 条" /tmp/p0fix_dryrun.log; then
  ok "dry-run 认出 3 条内控待办（采购 2 + 付款 1）"
else
  bad "dry-run 命中数不对（日志：$(grep -c . /tmp/p0fix_dryrun.log) 行，见 /tmp/p0fix_dryrun.log）"
  sed -n '1,40p' /tmp/p0fix_dryrun.log
fi

echo
echo "== ② apply（备份 + 单事务修复）=="
P0FIX_BACKUP_DIR=/tmp/p0fix_verify_backup bash scripts/maintenance/p0fix_self_approval.sh --apply >/tmp/p0fix_apply.log 2>&1
APPLY_RC=$?
expect_eq 0 "$APPLY_RC" "apply 退出码"
if [ -s /tmp/p0fix_verify_backup/db.dump.sql ]; then
  ok "整库备份已生成（$(du -h /tmp/p0fix_verify_backup/db.dump.sql | cut -f1)）"
else
  bad "整库备份缺失"
fi

TASKS_AFTER=$(q "SELECT count(*) FROM biz_flow_task WHERE instance_id IN ('$UID_INST','$UID_INST2')")
expect_eq "$TASKS_BEFORE" "$TASKS_AFTER" "待办行数不变（不删行，只改状态）"

expect_eq "SKIPPED" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE' AND assignee_id='$UID_CREATOR'")" "制单人（采购）→ SKIPPED"
expect_eq "SKIPPED" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE' AND assignee_id='$UID_INITIATOR'")" "发起人（采购）→ SKIPPED"
expect_eq "SKIPPED" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE2' AND assignee_id='$UID_CREATOR'")" "制单人（付款）→ SKIPPED"
expect_eq "PENDING" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE' AND assignee_id='$UID_KEEPER'")" "无关审批人仍是 PENDING"
expect_eq "PENDING" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE2' AND assignee_id='$UID_KEEPER'")" "无关审批人（付款）仍是 PENDING"
expect_eq "APPROVED" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE' AND assignee_id='$UID_DONE'")" "已批行不动"

expect_eq "true" "$(q "SELECT (count(*) = count(completed_at))::text FROM biz_flow_task WHERE instance_node_id IN ('$UID_NODE','$UID_NODE2') AND status='SKIPPED'")" "SKIPPED 行都有 completed_at（ck 约束）"
expect_eq "true" "$(q "SELECT ((count(*) FILTER (WHERE comment LIKE '%内控%制单人%')) = count(*) FILTER (WHERE status='SKIPPED'))::text FROM biz_flow_task WHERE instance_node_id IN ('$UID_NODE','$UID_NODE2')")" "SKIPPED 行都有内控留痕 comment"

# 引擎口径：approver_count 只数 PENDING/APPROVED/REJECTED
expect_eq "2" "$(q "SELECT approver_count FROM biz_flow_instance_node WHERE id='$UID_NODE'")" "采购节点 approver_count 重算为 2（4-2 SKIPPED）"
expect_eq "1" "$(q "SELECT approver_count FROM biz_flow_instance_node WHERE id='$UID_NODE2'")" "付款节点 approver_count 重算为 1"

echo
echo "== ③ verify 子命令（核对每个在跑节点还有没有能批的人）=="
bash scripts/maintenance/p0fix_self_approval.sh --verify >/tmp/p0fix_verify.log 2>&1
if grep -q "没有 ⚠ 行" /tmp/p0fix_verify.log; then
  ok "verify 未发现「ACTIVE 但 0 张 PENDING」的节点"
else
  bad "verify 报出遗留风险节点（见 /tmp/p0fix_verify.log）"
  grep "⚠" /tmp/p0fix_verify.log
fi

echo
echo "== ④ rollback（按行级快照精确回滚）=="
bash scripts/maintenance/p0fix_self_approval.sh --rollback --apply >/tmp/p0fix_rollback.log 2>&1
expect_eq "PENDING" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE' AND assignee_id='$UID_CREATOR'")" "回滚后制单人恢复 PENDING"
expect_eq "PENDING" "$(q "SELECT status FROM biz_flow_task WHERE instance_node_id='$UID_NODE' AND assignee_id='$UID_INITIATOR'")" "回滚后发起人恢复 PENDING"
expect_eq "true" "$(q "SELECT (count(*) = count(*) FILTER (WHERE completed_at IS NULL))::text FROM biz_flow_task WHERE instance_node_id='$UID_NODE' AND assignee_id IN ('$UID_CREATOR','$UID_INITIATOR')")" "回滚后 completed_at 恢复 NULL（ck 约束仍成立）"

echo
echo "== 清理夹具 =="
cleanup
echo "  已清理"

echo
echo "==> 汇总：PASS=${PASS} FAIL=${FAIL}"
[ "$FAIL" -eq 0 ] || exit 1
