#!/usr/bin/env python3
"""E2E-11 DB 层证据 + 勾稽（145 cloudpivot_dev，只读）。输出可直接贴进 README。"""
import sys

sys.path.insert(0, "/tmp/e2e11")
from e2e11_lib import psql  # noqa: E402

CHECKS = []


def q(title, sql):
    val = psql(sql)
    CHECKS.append((title, val, sql))
    print(f"{title}\n  => {val}\n")


print("======== E2E-11 DB 证据（cloudpivot_dev）========")
q("① 本卡任务 / 依赖 / 附件 / 评论 / 工时 / 模板",
  "select 'task='||(select count(*) from biz_task where title like 'E2E11%' and deleted_at is null)"
  "||' dep='||(select count(*) from biz_task_dependency d join biz_task t on t.id=d.task_id where t.title like 'E2E11%')"
  "||' att='||(select count(*) from biz_task_attachment a join biz_task t on t.id=a.task_id where t.title like 'E2E11%')"
  "||' tpl='||(select count(*) from biz_task_template where name like 'E2E11%')")
q("② 任务状态分布（本卡）",
  "select status||'='||count(*) from biz_task where title like 'E2E11%' and deleted_at is null group by status order by 1")
q("③ 八层父子链深度（从根向下递归）",
  "with recursive dn as ("
  " select t.id, 1 as lvl from biz_task t where t.title='E2E11 任务根（八层链）'"
  " union all select c.id, dn.lvl+1 from biz_task c join dn on c.parent_id=dn.id)"
  " select 'depth='||max(lvl)||' nodes='||count(*) from dn")
q("④ 父子链逐层标题（从根往下）",
  "with recursive dn as ("
  " select t.id, t.title, t.parent_id, 1 as lvl from biz_task t where t.title='E2E11 任务根（八层链）'"
  " union all select c.id, c.title, c.parent_id, dn.lvl+1 from biz_task c join dn on c.parent_id=dn.id)"
  " select lvl||'|'||title from dn order by lvl")
q("⑤ 勾稽：parent_id 指向的任务必须存在且未软删（悬空父 = 0）",
  "select count(*) from biz_task c where c.title like 'E2E11%' and c.parent_id is not null "
  "and not exists (select 1 from biz_task p where p.id=c.parent_id and p.deleted_at is null)")
q("⑥ 勾稽：依赖行两端都存在（本卡依赖悬空 = 0）",
  "select count(*) from biz_task_dependency d where "
  "(d.task_id in (select id from biz_task where title like 'E2E11%') "
  "or d.depends_on_id in (select id from biz_task where title like 'E2E11%')) "
  "and (not exists (select 1 from biz_task t where t.id=d.task_id) "
  "or not exists (select 1 from biz_task p where p.id=d.depends_on_id))")
q("⑦ 勾稽：DONE 任务 done_at 必须非空（本卡 DONE 缺 done_at = 0）",
  "select count(*) from biz_task where title like 'E2E11%' and status='DONE' and done_at is null")
q("⑧ 勾稽：DONE 任务进度必须 100（本卡不一致 = 0）",
  "select count(*) from biz_task where title like 'E2E11%' and status='DONE' and progress<>100")
q("⑨ 勾稽：子任务进度/状态与父任务汇总（父任务 progress 落后子任务 = 列出）",
  "select p.title||'|p='||p.progress||'|avg_child='||round(avg(c.progress))"
  " from biz_task p join biz_task c on c.parent_id=p.id and c.deleted_at is null"
  " where p.title like 'E2E11%' and p.deleted_at is null group by p.id, p.title, p.progress")
q("⑩ 状态日志（biz_task / STATUS）逐条",
  "select resource_id||'|'||coalesce(diff->'fields'->>'status','?')||'|'||coalesce(operator_name,'?')"
  "||'|'||to_char(created_at,'MM-DD HH24:MI:SS') from sys_change_log"
  " where resource='biz_task' and resource_id in (select id::text from biz_task where title like 'E2E11%')"
  " order by created_at limit 20")
q("⑪ 附件 uploader_name 快照覆盖率（本卡附件非空 = 全部）",
  "select 'rows='||count(*)||' with_name='||count(uploader_name)"
  " from biz_task_attachment where task_id in (select id from biz_task where title like 'E2E11%')")
q("⑫ 项目 / 成员 / 关联任务",
  "select p.project_no||'|members='||(select count(*) from biz_project_member m where m.project_id=p.id)"
  "||'|tasks='||(select count(*) from biz_task t where t.project_id=p.id and t.deleted_at is null)"
  " from biz_project p where p.project_no like 'E2E11%'")
q("⑬ 勾稽：项目任务里 DONE 的比例 vs 项目 task_done（逐项目列出）",
  "select p.project_no||'|total='||count(t.id)||'|done='||count(t.id) filter (where t.status='DONE')"
  " from biz_project p left join biz_task t on t.project_id=p.id and t.deleted_at is null"
  " where p.project_no like 'E2E11%' group by p.project_no")
q("⑭ 公告状态 / 范围 / 附件 / 阅读回执",
  "select a.ann_no||'|'||a.status||'|scope='||a.scope_type||'|readers='||a.read_count"
  "||'|atts='||(select count(*) from sys_announcement_attachment aa where aa.announcement_id=a.id)"
  "||'|receipts='||(select count(*) from sys_announcement_read r where r.announcement_id=a.id)"
  " from sys_announcement a where a.title like 'E2E11%' order by a.created_at")
q("⑮ 勾稽：公告 read_count 冗余计数 vs 已读回执行数（差异必须 0）",
  "select count(*) from sys_announcement a where a.title like 'E2E11%' and a.read_count <> "
  "(select count(*) from sys_announcement_read r where r.announcement_id=a.id)")
q("⑯ 勾稽：公告扇出通知数 vs 实际接收行（公告类通知）",
  "select n.title||'|receiver_count='||n.receiver_count||'|rows='||"
  "(select count(*) from sys_notice_receiver r where r.notice_id=n.id)"
  " from sys_notice n where n.title like 'E2E11%' order by n.created_at")
q("⑰ 勾稽：通知已读冗余 vs 接收行 read_at（差异必须 0）",
  "select count(*) from sys_notice n where n.title like 'E2E11%' and n.read_count <> "
  "(select count(*) from sys_notice_receiver r where r.notice_id=n.id and r.read_at is not null)")
q("⑱ 消息送达通道（本卡通知；channel_mask=1 只走站内信，不进 delivery 表）",
  "select coalesce(string_agg(distinct n.title||'|mask='||n.channel_mask||'|status='||n.dispatch_status, ' ; '), '<none>')"
  " from sys_notice n where n.title like 'E2E11%'")
q("⑲ 通知模板状态（本卡）",
  "select template_code||'|'||status||'|'||template_name from sys_notice_template where template_code like 'E2E11%'")
q("⑳ 残留自检：本卡角色 / 用户 / 项目 / 公告 / 通知行数",
  "select 'role='||(select count(*) from sys_role where role_code like 'E2E11%')"
  "||' user='||(select count(*) from sys_user where username like 'e2e11\\_%')"
  "||' proj='||(select count(*) from biz_project where project_no like 'E2E11%')"
  "||' ann='||(select count(*) from sys_announcement where title like 'E2E11%')"
  "||' notice='||(select count(*) from sys_notice where title like 'E2E11%')")

print("\n======== 断言式勾稽（0 = 一致）========")
GUARDS = [
    ("悬空父任务", "select count(*) from biz_task c where c.title like 'E2E11%' and c.parent_id is not null "
                 "and not exists (select 1 from biz_task p where p.id=c.parent_id and p.deleted_at is null)"),
    ("DONE 缺 done_at", "select count(*) from biz_task where title like 'E2E11%' and status='DONE' and done_at is null"),
    ("DONE 进度非 100", "select count(*) from biz_task where title like 'E2E11%' and status='DONE' and progress<>100"),
    ("公告 read_count 不一致", "select count(*) from sys_announcement a where a.title like 'E2E11%' and a.read_count <> "
                          "(select count(*) from sys_announcement_read r where r.announcement_id=a.id)"),
    ("通知 read_count 不一致", "select count(*) from sys_notice n where n.title like 'E2E11%' and n.read_count <> "
                          "(select count(*) from sys_notice_receiver r where r.notice_id=n.id and r.read_at is not null)"),
    ("附件 uploader_name 为空", "select count(*) from biz_task_attachment where task_id in "
                           "(select id from biz_task where title like 'E2E11%') and uploader_name is null"),
    ("审批单已通过但任务未 DONE（回调漏推）",
     "select count(*) from biz_task t join biz_flow_instance i on i.id=t.approval_instance_id "
     "where t.title like 'E2E11%' and i.status='APPROVED' and t.status<>'DONE'"),
    ("公告 receiver_count 与实际通知接收行不一致",
     "select count(*) from sys_announcement a where a.title like 'E2E11%' and a.receiver_count <> "
     "(select count(*) from sys_notice_receiver r join sys_notice n on n.id=r.notice_id "
     " where n.biz_id = a.id::text)"),
    ("依赖自环 / 父子环（本卡任务）",
     "select count(*) from biz_task_dependency where task_id = depends_on_id and "
     "task_id in (select id from biz_task where title like 'E2E11%')"),
]
bad = 0
for name, sql in GUARDS:
    val = psql(sql)
    ok = str(val).strip() == "0"
    bad += 0 if ok else 1
    print(f"  {'OK ' if ok else 'BAD'} {name} = {val}")
print(f"\n勾稽失败项 = {bad}")
sys.exit(0 if bad == 0 else 1)
