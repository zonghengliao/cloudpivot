#!/usr/bin/env python3
"""E2E-10 DB 层证据 + 勾稽（145 cloudpivot_dev，只读）。输出可直接贴进 README。"""
import sys

sys.path.insert(0, "/tmp/e2e10")
from e2e10_lib import psql  # noqa: E402

CHECKS = []


def q(title, sql):
    val = psql(sql)
    CHECKS.append((title, val, sql))
    print(f"{title}\n  => {val}\n")


print("======== E2E-10 DB 证据（cloudpivot_dev）========")
q("① 本卡流程定义 / 版本 / 节点 / 边",
  "select 'def='||(select count(*) from biz_flow_def where flow_code like 'e2e10%' and deleted_at is null)"
  "||' ver='||(select count(*) from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id where d.flow_code like 'e2e10%')"
  "||' node='||(select count(*) from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code like 'e2e10%')"
  "||' edge='||(select count(*) from biz_flow_edge e join biz_flow_def_version v on v.id=e.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code like 'e2e10%')")
q("② 实例状态分布（本卡标题前缀 E2E10）",
  "select status||'='||count(*) from biz_flow_instance where title like 'E2E10%' group by status order by 1")
q("③ 待办状态分布",
  "select t.status||'='||count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id "
  "where i.title like '%E2E10%' group by t.status order by 1")
q("④ 勾稽：PENDING 待办 vs 实例状态（不该有终态实例还挂 PENDING 待办）",
  "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id "
  "where i.title like 'E2E10%' and t.status='PENDING' and i.status <> 'PENDING'")
q("⑤ 勾稽：实例节点 approved_count vs 待办 APPROVED 行数（差异应为 0）",
  "select count(*) from biz_flow_instance_node n join biz_flow_instance i on i.id=n.instance_id "
  "where i.title like 'E2E10%' and n.approved_count <> "
  "(select count(*) from biz_flow_task t where t.instance_node_id=n.id and t.status='APPROVED')")
q("⑥ 会签 / 或签 / 定额（节点签法落库 + 批准数）",
  "select n.node_key||'|'||coalesce(n.approval_mode,'<NULL>')||'|'||coalesce(to_char(n.approve_ratio,'FM0.990'),'-')"
  "||'|appr='||n.approved_count||'|'||n.status from biz_flow_instance_node n join biz_flow_instance i on i.id=n.instance_id "
  "where i.title like 'E2E10 S3%' order by i.title, n.node_key")
q("⑦ 版本冻结：实例绑定版本 vs 节点名快照",
  "select i.title||'|v='||(select version_no from biz_flow_def_version v where v.id=i.version_id)"
  "||'|'||n.node_key||'='||n.node_name from biz_flow_instance i join biz_flow_instance_node n on n.instance_id=i.id "
  "where i.title like 'E2E10 S2%' and n.node_key in ('n_low','n_extra') order by i.title, n.node_key")
q("⑧ 驳回轮次（PREV / 指定节点）",
  "select i.title||'|'||n.node_key||'|round='||n.round||'|'||n.status from biz_flow_instance i "
  "join biz_flow_instance_node n on n.instance_id=i.id where i.title like 'E2E10 S4%' "
  "and n.round>1 order by i.title, n.node_key")
q("⑨ RESUBMIT 待办（退回发起人）",
  "select i.title||'|'||t.task_kind||'|'||t.status||'|'||coalesce(t.comment,'') from biz_flow_task t "
  "join biz_flow_instance i on i.id=t.instance_id where t.task_kind='RESUBMIT' and i.title like 'E2E10%'")
q("⑩ 委托留痕（DELEGATED + 代 X 审批）",
  "select t.assignee_name||'|'||t.assigned_by_rule||'|'||coalesce(t.comment,'') from biz_flow_task t "
  "join biz_flow_instance i on i.id=t.instance_id where i.title like 'E2E10 S5%' order by i.title")
q("⑪ 委托配置行（软删留痕：deleted_at 非空）",
  "select coalesce(t.reason,'<NULL>')||'|enabled='||t.enabled||'|deleted='||(t.deleted_at is not null) "
  "from biz_flow_delegate t where t.delegator_id = (select id from sys_user where username='u_del') "
  "order by t.created_at")
q("⑫ 自审 SKIP / ALLOW 留痕",
  "select i.title||'|'||t.assignee_name||'|'||t.status||'|'||t.is_self_approval||'|'||coalesce(t.comment,'') "
  "from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id "
  "where i.title like 'E2E10 S6%' and (t.status='SKIPPED' or t.is_self_approval) order by i.title")
q("⑬ 自审审计（FLOW_SELF_APPROVAL_ALLOWED / VOTED）",
  "select operation||'='||count(*) from operation_logs where operation like 'FLOW_SELF_APPROVAL%' "
  "and target_id in (select id::text from biz_flow_instance where title like 'E2E10 S6%') group by operation order by 1")
q("⑭ 降级留痕（ESCALATED / FALLBACK / 原因）",
  "select coalesce(t.assigned_by_rule,'-')||'|'||coalesce(t.escalated_from_role,'-')||'|'"
  "||coalesce(t.escalate_reason,'-')||'='||count(*) "
  "from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.title like 'E2E10 S7%' "
  "group by t.assigned_by_rule, t.escalated_from_role, t.escalate_reason order by 1")
q("⑮ 联动通知（TASK 触发，幂等：每个任务 1 条）",
  "select n.biz_id||'|notices='||count(*) from sys_notice n where n.biz_id in "
  "(select id::text from biz_task where title like 'E2E10 联动%') group by n.biz_id order by 1")
q("⑯ 联动规则软删留痕",
  "select name||'|enabled='||enabled||'|deleted='||(deleted_at is not null) from biz_flow_linkage_rule "
  "where name like 'E2E10%' order by created_at")
q("⑰ 状态边配置行（审批通过后生效）",
  "select entity||'|'||from_status||'→'||to_status||'|requires_reason='||requires_reason||'|label='||coalesce(label,'-') "
  "from biz_flow_state_edge where entity='STOCK_IN' order by from_status, to_status")
q("⑱ 状态边配置变更单（走审批）",
  "select status||'='||count(*) from biz_flow_instance where biz_type='FLOW_EDGE_CFG' "
  "and form_data->>'label' like 'E2E10%' group by status")
q("⑲ 本卡残留实例数（清理前）",
  "select count(*) from biz_flow_instance where title like 'E2E10%'")
q("⑳ 孤儿检查：ACTIVE 节点却 0 条 PENDING 待办（审批类）",
  "select count(*) from biz_flow_instance_node n join biz_flow_instance i on i.id=n.instance_id "
  "where i.title like 'E2E10%' and n.status='ACTIVE' and n.node_type in ('APPROVAL','COUNTERSIGN') "
  "and not exists (select 1 from biz_flow_task t where t.instance_node_id=n.id and t.status='PENDING')")

with open("/tmp/e2e10/run6/db_evidence.log", "w", encoding="utf-8") as fh:
    for title, val, sql in CHECKS:
        fh.write(f"{title}\n  SQL: {sql}\n  => {val}\n\n")
print("written /tmp/e2e10/run6/db_evidence.log")
