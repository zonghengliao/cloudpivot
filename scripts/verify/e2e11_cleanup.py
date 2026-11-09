#!/usr/bin/env python3
"""E2E-11 演练数据自清：只删本卡 E2E11 前缀造的行，按外键顺序；不 DROP / TRUNCATE。
审计表（sys_change_log / operation_logs / audit_logs）按设计保留。"""
import sys

sys.path.insert(0, "/tmp/e2e11")
from e2e11_lib import psql  # noqa: E402

BEFORE = {
    "task": "select count(*) from biz_task where title like 'E2E11%'",
    "dep": "select count(*) from biz_task_dependency d join biz_task t on t.id=d.task_id where t.title like 'E2E11%'",
    "attachment": "select count(*) from biz_task_attachment a join biz_task t on t.id=a.task_id where t.title like 'E2E11%'",
    "template": "select count(*) from biz_task_template where name like 'E2E11%'",
    "project": "select count(*) from biz_project where project_no like 'E2E11%'",
    "announcement": "select count(*) from sys_announcement where title like 'E2E11%'",
    "notice": "select count(*) from sys_notice where title like 'E2E11%'",
    "notice_tpl": "select count(*) from sys_notice_template where template_code like 'E2E11%'",
    "role": "select count(*) from sys_role where role_code like 'E2E11%'",
    "user": "select count(*) from sys_user where username like 'e2e11\\_%'",
}
print("== 清理前 ==")
for key, sql in BEFORE.items():
    print(f"  {key} = {psql(sql)}")

TASK_SET = "select id from biz_task where title like 'E2E11%'"
STEPS = [
    # 通知侧（本卡标题 + 本卡任务 biz_id）
    "delete from sys_notice_delivery where notice_id in (select id from sys_notice where title like 'E2E11%' "
    "or biz_id in (select id::text from biz_task where title like 'E2E11%'))",
    "delete from sys_notice_receiver where notice_id in (select id from sys_notice where title like 'E2E11%' "
    "or biz_id in (select id::text from biz_task where title like 'E2E11%'))",
    "delete from sys_notice where title like 'E2E11%' or biz_id in (select id::text from biz_task where title like 'E2E11%')",
    "delete from sys_notice_template where template_code like 'E2E11%'",
    # 公告侧
    "delete from sys_announcement_read where announcement_id in (select id from sys_announcement where title like 'E2E11%')",
    "delete from sys_announcement_attachment where announcement_id in (select id from sys_announcement where title like 'E2E11%')",
    "delete from sys_announcement_scope where announcement_id in (select id from sys_announcement where title like 'E2E11%')",
    "delete from sys_announcement where title like 'E2E11%'",
    # 存储文件（本卡上传的 e2e11-* 公告附件）
    "delete from sys_file where original_name like 'e2e11-%' and category='announcement'",
    # 任务侧（叶子先删，循环 12 轮清空 8 层链）
    "delete from biz_task_worklog where task_id in (" + TASK_SET + ")",
    "delete from biz_task_comment where task_id in (" + TASK_SET + ")",
    "delete from biz_task_member where task_id in (" + TASK_SET + ")",
    "delete from biz_task_dependency where task_id in (" + TASK_SET + ") or depends_on_id in (" + TASK_SET + ")",
    "delete from biz_task_attachment where task_id in (" + TASK_SET + ")",
    "delete from biz_task_template where name like 'E2E11%'",
]
# 自底向上删任务：每轮删「没有未删子任务」的行
for _ in range(12):
    STEPS.append(
        "delete from biz_task where title like 'E2E11%' and not exists "
        "(select 1 from biz_task c where c.parent_id=biz_task.id and c.deleted_at is null)"
    )
STEPS += [
    "delete from biz_project_member where project_id in (select id from biz_project where project_no like 'E2E11%')",
    "delete from biz_project where project_no like 'E2E11%'",
    # 用户 / 角色
    "delete from sys_user_role where role_id in (select id from sys_role where role_code like 'E2E11%') "
    "or user_id in (select id from sys_user where username like 'e2e11\\_%')",
    "delete from sys_role_menu where role_id in (select id from sys_role where role_code like 'E2E11%')",
    "delete from sys_role where role_code like 'E2E11%'",
    "delete from sys_user where username like 'e2e11\\_%'",
]
print("\n== 执行 ==")
for sql in STEPS:
    out = psql(sql)
    print(f"  {sql[:88]}... => {out}")

AFTER = {
    "task": "select count(*) from biz_task where title like 'E2E11%'",
    "dep": "select count(*) from biz_task_dependency d join biz_task t on t.id=d.task_id where t.title like 'E2E11%'",
    "template": "select count(*) from biz_task_template where name like 'E2E11%'",
    "project": "select count(*) from biz_project where project_no like 'E2E11%'",
    "announcement": "select count(*) from sys_announcement where title like 'E2E11%'",
    "notice": "select count(*) from sys_notice where title like 'E2E11%'",
    "role": "select count(*) from sys_role where role_code like 'E2E11%'",
    "user": "select count(*) from sys_user where username like 'e2e11\\_%'",
}
print("\n== 清理后 ==")
residue = 0
for key, sql in AFTER.items():
    val = psql(sql)
    residue += 0 if str(val).strip() == "0" else 1
    print(f"  {key} = {val}")
print(f"\n残留非零项 = {residue}")
sys.exit(0 if residue == 0 else 1)
