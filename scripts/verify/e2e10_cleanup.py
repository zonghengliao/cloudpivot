#!/usr/bin/env python3
"""E2E-10 演练数据自清：只删本卡 E2E10 前缀造的行，按外键顺序；不 DROP / TRUNCATE。
审计表（operation_logs / audit_logs / sys_change_log）按设计保留。"""
import sys

sys.path.insert(0, "/tmp/e2e10")
from e2e10_lib import psql  # noqa: E402

BEFORE = {
    "flow_def": "select count(*) from biz_flow_def where flow_code like 'e2e10%'",
    "flow_instance": "select count(*) from biz_flow_instance where title like '%E2E10%'",
    "flow_task": "select count(*) from biz_flow_task t join biz_flow_instance i on i.id=t.instance_id where i.title like '%E2E10%'",
    "delegate": "select count(*) from biz_flow_delegate where reason like 'E2E10%'",
    "linkage": "select count(*) from biz_flow_linkage_rule where name like 'E2E10%'",
    "task": "select count(*) from biz_task where title like 'E2E10%'",
    "user": "select count(*) from sys_user where username like 'u\\_%' and id in (select user_id from sys_user_role ur join sys_role r on r.id=ur.role_id where r.role_code like 'E2E10%')",
    "role": "select count(*) from sys_role where role_code like 'E2E10%'",
    "state_edge": "select count(*) from biz_flow_state_edge where label = 'E2E10 演练文案'",
}
print("== 清理前 ==")
for key, sql in BEFORE.items():
    print(f"  {key} = {psql(sql)}")

STEPS = [
    # 流程实例侧
    "delete from biz_flow_task where instance_id in (select id from biz_flow_instance where title like '%E2E10%')",
    "delete from biz_flow_cc where instance_id in (select id from biz_flow_instance where title like '%E2E10%')",
    "delete from biz_flow_instance_node where instance_id in (select id from biz_flow_instance where title like '%E2E10%')",
    "delete from biz_flow_instance where title like '%E2E10%'",
    # 任务（联动载体）
    "delete from biz_task_comment where task_id in (select id from biz_task where title like 'E2E10%')",
    "delete from biz_task_member where task_id in (select id from biz_task where title like 'E2E10%')",
    "delete from biz_task where title like 'E2E10%'",
    # 通知（本卡联动通知，按 biz_id 命中本卡任务）
    "delete from sys_notice_receiver where notice_id in (select id from sys_notice where biz_id in "
    "(select id::text from biz_task where title like 'E2E10%'))",
    "delete from sys_notice where biz_id in (select id::text from biz_task where title like 'E2E10%')",
    # 委托 / 联动规则 / 状态边（只动本卡 label）
    "delete from biz_flow_delegate where reason like 'E2E10%'",
    "delete from biz_flow_linkage_rule where name like 'E2E10%'",
    "delete from biz_flow_state_edge where label = 'E2E10 演练文案'",
    # 流程定义侧
    "delete from biz_flow_edge where version_id in (select v.id from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id where d.flow_code like 'e2e10%')",
    "delete from biz_flow_node_assignee where node_id in (select n.id from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id where d.flow_code like 'e2e10%')",
    "delete from biz_flow_node where version_id in (select v.id from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id where d.flow_code like 'e2e10%')",
    "delete from biz_flow_def_version where def_id in (select id from biz_flow_def where flow_code like 'e2e10%')",
    "delete from biz_flow_def where flow_code like 'e2e10%'",
    # 用户 / 角色
    "delete from sys_user_role where role_id in (select id from sys_role where role_code like 'E2E10%') "
    "or user_id in (select id from sys_user where username like 'u\\_%' and id in "
    "(select user_id from sys_user_role ur join sys_role r on r.id=ur.role_id where r.role_code like 'E2E10%'))",
    "delete from sys_role_menu where role_id in (select id from sys_role where role_code like 'E2E10%')",
    "delete from sys_role where role_code like 'E2E10%'",
    "delete from sys_user where username like 'u\\_%' and username in "
    "('u_a1','u_t1','u_t2','u_t3','u_m1','u_b1','u_b2','u_l1','u_fb','u_solo','u_del','u_pa','u_pb','u_pbn','u_init')",
]
print("\n== 执行 ==")
for sql in STEPS:
    out = psql(sql)
    print(f"  {sql[:90]}... => {out}")

print("\n== 清理后（应全 0）==")
for key, sql in BEFORE.items():
    print(f"  {key} = {psql(sql)}")
print("\n== 审计表保留（按设计不可删）==")
print("  operation_logs(FLOW_SELF_APPROVAL*) =",
      psql("select count(*) from operation_logs where operation like 'FLOW_SELF_APPROVAL%'"))
