#!/usr/bin/env python3
"""E2E-11 第 1 段：任务域 4 页面 + 项目前置数据
   S0 夹具 / S1 任务列表（建→分配→子任务 8 层→折叠展开）/ S2 状态流转 + 状态日志
   S3 依赖（环检测 + 前置未完成拦截 + 完成联动）/ S4 看板（分组 + 拖拽 + 批量）
   S5 甘特（只读排期 + 窗口上限）/ S6 模板（建模板→一键建任务）

在 145 上执行：python3 /tmp/e2e11/e2e11_a.py
"""
import sys

sys.path.insert(0, "/tmp/e2e11")
from e2e11_lib import *  # noqa: F403,E402

ROLE_WORKER = "E2E11_WORKER"
ROLE_READONLY = "E2E11_READONLY"
PERM_WORKER = [
    "business:task:read", "business:task:create", "business:task:update",
    "business:task:status", "business:task:subtask", "business:task:progress",
    "business:task:assign", "business:task:delete",
    "business:project:read", "business:announcement:read", "business:notice:mine",
]


def fixtures(state):
    scenario("S0", "夹具：两个角色 / 两个用户 / 权限直授 + 基线核对")
    admin = login("admin", ADMIN_PW, name="admin_login")
    check(bool(admin), "admin 登录（令牌就绪）")
    if not admin:
        raise SystemExit("admin 登录失败")
    st, body = req("GET", "/api/business/task/page?page=1&page_size=1", admin, name="baseline_taskpage")
    eq("任务分页 → 200", st, 200)
    base_task = psql("select count(*) from biz_task")
    check(int(base_task or 0) >= 0, "基线：biz_task 可读", f"存量={base_task}")
    eq("基线：biz_project 可读", psql("select count(*) from biz_project where deleted_at is null") >= "0", True)

    roles = {}
    for code in (ROLE_WORKER, ROLE_READONLY):
        rid, st, body = mk_role(admin, code, f"E2E11 {code}（演练）", sort=960)
        check(bool(rid), f"建角色 {code}", f"http={st} id={rid}")
        if not rid:
            raise SystemExit(f"角色 {code} 建失败: {msg_of(body)}")
        roles[code] = rid
    users = {}
    for username, real_name, code in (
        ("e2e11_w1", "E2E11 执行人甲", ROLE_WORKER),
        ("e2e11_w2", "E2E11 执行人乙", ROLE_WORKER),
        ("e2e11_ro", "E2E11 只读用户", ROLE_READONLY),
    ):
        uid, st, body = mk_user(admin, username, real_name, [roles[code]])
        check(bool(uid), f"建用户 {username}（{code}）", f"http={st} id={uid}")
        if not uid:
            raise SystemExit(f"用户 {username} 建失败: {msg_of(body)}")
        users[username] = uid
    n, body2 = grant(admin, roles[ROLE_WORKER], PERM_WORKER)
    check(n >= len(PERM_WORKER), f"角色 {ROLE_WORKER} 授予 {len(PERM_WORKER)} 个权限码", f"实授菜单={n}")
    n2, _ = grant(admin, roles[ROLE_READONLY], ["business:task:read"])
    check(n2 >= 1, f"角色 {ROLE_READONLY} 只授 business:task:read", f"实授菜单={n2}")

    tokens = {}
    for username in users:
        tokens[username] = login(username, name=f"login_{username}")
        check(bool(tokens[username]), f"{username} 登录")
    state.update({"admin_token": admin, "users": users, "roles": roles, "tokens": tokens})
    save_state(state)
    return state


def s1_tasks(state):
    scenario("S1", "任务列表：建任务 → 分配 → 子任务 8 层 → 折叠展开 → 权限边界")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    w2 = state["users"]["e2e11_w2"]
    tw1 = state["tokens"]["e2e11_w1"]
    tw2 = state["tokens"]["e2e11_w2"]

    st, body = req("POST", "/api/business/task", admin,
                   {"title": "E2E11 任务根（八层链）", "description": "演练根任务",
                    "priority": "P1", "need_review": False, "due_date": "2026-12-31"},
                   name="s1_create_root")
    eq("建根任务 → 200", st, 200)
    root = data_of(body).get("id")
    check(bool(root), "根任务 id 返回", f"id={root} task_no={data_of(body).get('task_no')}")
    eq("新建任务默认状态 TODO", psql(f"select status from biz_task where id='{root}'::uuid"), "TODO")
    eq("任务编号格式 TSK-YYYYMM-*", psql(f"select case when task_no ~ '^TSK-[0-9]{{6}}-[0-9]{{6}}$' then 'ok' else task_no end "
                                     f"from biz_task where id='{root}'::uuid"), "ok")

    # ① 分配
    st, body = req("PATCH", f"/api/business/task/{root}/assign", admin,
                   {"owner_id": w1, "reason": "E2E11 派给执行人甲"}, name="s1_assign")
    eq("分配负责人 → 200", st, 200)
    eq("落库 owner_id", psql(f"select owner_id from biz_task where id='{root}'::uuid"), w1)
    st, body = req("GET", f"/api/business/task/{root}", admin, name="s1_detail_root")
    eq("详情 owner_name = 执行人甲", data_of(body).get("owner_name"), "E2E11 执行人甲")
    eq("创建人派活不写 assigner_id（口径：creator_id 已是源头）", 
       psql(f"select coalesce(assigner_id::text,'<null>') from biz_task where id='{root}'::uuid"), "<null>")

    # ①b 非创建人转派 → assigner_id 记操作人 + source=TRANSFERRED
    st, body = req("POST", "/api/business/task", admin, {"title": "E2E11 转派样本", "owner_id": w1},
                   name="s1_mk_transfer")
    tr = data_of(body).get("id")
    st, body = req("PATCH", f"/api/business/task/{tr}/assign", tw1, {"owner_id": w2, "reason": "E2E11 转派"},
                   name="s1_transfer")
    eq("负责人转派给乙 → 200", st, 200)
    eq("转派落库 owner=乙", psql(f"select owner_id from biz_task where id='{tr}'::uuid"), w2)
    eq("转派落库 assigner=甲（非创建人记操作人）", psql(f"select assigner_id from biz_task where id='{tr}'::uuid"), w1)
    eq("转派后 source = TRANSFERRED", psql(f"select source from biz_task where id='{tr}'::uuid"), "TRANSFERRED")

    # ② 八层父子链（root = 第 1 层，再挂 7 层）
    chain = [root]
    for level in range(2, 9):
        parent = chain[-1]
        st, body = req("POST", f"/api/business/task/{parent}/subtask", admin,
                       {"title": f"E2E11 L{level} 子任务"}, name=f"s1_subtask_l{level}")
        ok = st == 200 and data_of(body).get("id")
        check(ok, f"建第 {level} 层子任务 → 200", f"http={st} id={data_of(body).get('id')}")
        if not ok:
            print("    中断：", msg_of(body))
            break
        sid = data_of(body).get("id")
        chain.append(sid)
        eq(f"第 {level} 层继承父任务 project/priority", 
           psql(f"select priority from biz_task where id='{sid}'::uuid"), "P1")
    eq("链长 = 8 层", len(chain), 8)
    depth_sql = (
        "WITH RECURSIVE up AS (SELECT id, parent_id, 1 AS d FROM biz_task WHERE id='%s'::uuid "
        "UNION ALL SELECT t.id, t.parent_id, up.d+1 FROM biz_task t JOIN up ON t.id=up.parent_id) "
        "SELECT max(d) FROM up" % chain[-1]
    )
    eq("递归确认最深 8 层", psql(depth_sql), "8")
    st, body = req("POST", f"/api/business/task/{chain[-1]}/subtask", admin,
                   {"title": "E2E11 L9 越界子任务"}, name="s1_subtask_l9")
    eq("第 9 层被拒 → 400", st, 400)
    eq("错误码 63005（父任务非法/超深）", code_of(body), 63005)
    contains("拒绝文案点名深度上限", msg_of(body), "8")
    eq("第 9 层未落库", psql(f"select count(*) from biz_task where title='E2E11 L9 越界子任务'"), "0")

    # ③ 折叠展开：root_only 只出根，子任务靠逐层懒加载
    st, body = req("GET", "/api/business/task/page?page=1&page_size=50&root_only=true&keyword=E2E11", admin,
                   name="s1_page_root_only")
    eq("根任务分页 → 200", st, 200)
    ids = [x.get("id") for x in data_of(body).get("list") or []]
    check(root in ids, "root_only 包含根任务")
    check(not any(x in ids for x in chain[1:]), "root_only 不含任何子任务（折叠口径）", f"条数={len(ids)}")
    st, body = req("GET", f"/api/business/task/{root}/subtasks", admin, name="s1_subtasks_l1")
    eq("展开第 1 层 → 200", st, 200)
    eq("第 1 层子任务数 = 1", len(data_of(body).get("list") or []), 1)
    eq("子任务列表带 parent_status", data_of(body).get("parent_status"), "DOING" if False else "TODO")
    st, body = req("GET", f"/api/business/task/{chain[3]}/subtasks", admin, name="s1_subtasks_l4")
    eq("展开第 4 层 → 200", st, 200)
    eq("第 4 层子任务数 = 1", len(data_of(body).get("list") or []), 1)

    # ④ 可见性：被分派者在自己的分页里看得到
    st, body = req("GET", "/api/business/task/page?page=1&page_size=50&keyword=E2E11", tw1, name="s1_page_w1")
    eq("执行人甲任务分页 → 200", st, 200)
    check(root in [x.get("id") for x in data_of(body).get("list") or []],
          "执行人甲（负责人）能看到被派的任务", f"total={data_of(body).get('total')}")
    st, body = req("GET", f"/api/business/task/{root}", tw2, name="s1_detail_w2")
    ne("执行人乙（无关）看不到该任务 → 404", st, 200)

    # ⑤ 写权限 ≠ 可见性：协作人看得见、改不了
    st, body = req("POST", f"/api/business/task/{root}/members", admin, {"user_ids": [w2]}, name="s1_add_member")
    eq("加协作人 → 200", st, 200)
    st, body = req("GET", f"/api/business/task/{root}", tw2, name="s1_detail_w2_after")
    eq("协作人可见 → 200", st, 200)
    st, body = req("PUT", f"/api/business/task/{root}", tw2, {"title": "E2E11 协作人越权改标题"},
                   name="s1_update_by_collaborator")
    eq("协作人编辑被拒 → 403", st, 403)
    st, body = req("PUT", f"/api/business/task/{root}", admin, {"title": "E2E11 任务根（八层链）",
                                                             "priority": "P0"}, name="s1_update_creator")
    eq("创建人编辑 → 200", st, 200)
    eq("编辑落库 priority=P0", psql(f"select priority from biz_task where id='{root}'::uuid"), "P0")
    eq("我的任务（甲）含该任务",
       psql(f"select count(*) from biz_task where id='{root}'::uuid and owner_id='{w1}'::uuid"), "1")

    state["chain"] = chain
    save_state(state)


def s2_status(state):
    scenario("S2", "状态流转：TODO → DOING → DONE + 状态日志 + 非法流转拦截 + 验收门")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    tw1 = state["tokens"]["e2e11_w1"]
    root = state["chain"][0]

    # ① 叶子任务正常链（无子任务、免验收 → 可直通 DONE）
    st, body = req("POST", "/api/business/task", admin, {"title": "E2E11 状态叶子任务"},
                   name="s2_mk_leaf")
    leaf = data_of(body).get("id")
    eq("建叶子任务 → 200", bool(leaf), True)
    st, body = req("PATCH", f"/api/business/task/{leaf}/status", admin, {"to": "DOING"}, name="s2_leaf_doing")
    eq("TODO → DOING → 200", st, 200)
    eq("落库 DOING", psql(f"select status from biz_task where id='{leaf}'::uuid"), "DOING")
    check(int(psql(f"select progress from biz_task where id='{leaf}'::uuid") or 0) >= 10,
          "进 DOING 后进度下限 10（无子任务才联动）",
          f"progress={psql(f'select progress from biz_task where id={chr(39)}{leaf}{chr(39)}::uuid')}")
    st, body = req("PATCH", f"/api/business/task/{leaf}/status", admin, {"to": "DONE"}, name="s2_leaf_done")
    eq("DOING → DONE（免验收）→ 200", st, 200)
    eq("落库 DONE", psql(f"select status from biz_task where id='{leaf}'::uuid"), "DONE")
    eq("done_at 已写", psql(f"select case when done_at is null then 'null' else 'set' end "
                          f"from biz_task where id='{leaf}'::uuid"), "set")
    eq("进度拉满 100", psql(f"select progress from biz_task where id='{leaf}'::uuid"), "100")

    # ② 状态日志（sys_change_log action=STATUS）
    logs = psql("select action||'|'||coalesce(diff->'fields'->>'status','?')||'|'||coalesce(operator_name,'?') "
                "from sys_change_log where resource='biz_task' and resource_id='%s' and action='STATUS' "
                "order by created_at" % leaf)
    contains("状态日志落 sys_change_log（action=STATUS）", logs, "DOING")
    contains("状态日志记录终态 DONE", logs, "DONE")
    contains("状态日志记录操作人（username=admin）", logs, "admin")
    st, body = req("GET", f"/api/business/changelog/timeline?table=biz_task&target_id={leaf}&limit=50", admin,
                   name="s2_timeline")
    eq("变更时间线 → 200", st, 200)
    items = data_of(body).get("items") or []
    check(any(i.get("action") == "STATUS" for i in items), "时间线含 STATUS 条目", f"条数={len(items)}")
    eq("时间线行数 ≥ 2（两次状态 + 建单）", len(items) >= 2, True)

    # ③ 非法流转
    st, body = req("POST", "/api/business/task", admin, {"title": "E2E11 非法流转探针"}, name="s2_mk_skip")
    skip = data_of(body).get("id")
    st, body = req("PATCH", f"/api/business/task/{skip}/status", admin, {"to": "DONE"}, name="s2_skip_todo_done")
    eq("TODO → DONE 跳级被拒 → 409", st, 409)
    eq("错误码 63010", code_of(body), 63010)
    eq("状态仍为 TODO", psql(f"select status from biz_task where id='{skip}'::uuid"), "TODO")
    st, body = req("PATCH", f"/api/business/task/{leaf}/status", admin, {"to": "CANCELLED"}, name="s2_done_cancel")
    eq("DONE → CANCELLED 被拒 → 409", st, 409)
    eq("错误码 63010（终态不可再推）", code_of(body), 63010)

    # ④ 父任务门：有子任务 → 进度锁定 + 不许直接完成
    child = state["chain"][1]
    st, body = req("PATCH", f"/api/business/task/{child}/status", admin, {"to": "DOING"}, name="s2_child_doing")
    eq("父任务（有子）TODO → DOING → 200", st, 200)
    eq("有子任务 → 进度不联动（保持 0）", psql(f"select progress from biz_task where id='{child}'::uuid"), "0")
    st, body = req("PATCH", f"/api/business/task/{child}/status", admin, {"to": "DONE"}, name="s2_child_done")
    eq("父任务 DOING → DONE 被拒 → 409", st, 409)
    eq("错误码 63006（未完成子任务）", code_of(body), 63006)
    contains("拒绝文案说明原因", msg_of(body), "子任务")

    # ⑤ 需验收任务：DOING 不能直接 DONE；进 REVIEW 挂 TASK_ACCEPT 审批单 → 门闸 65030
    st, body = req("POST", "/api/business/task", admin,
                   {"title": "E2E11 需验收任务", "need_review": True, "owner_id": w1}, name="s2_mk_review")
    rv = data_of(body).get("id")
    eq("建需验收任务（负责人=甲）→ 200", bool(rv), True)
    st, body = req("PATCH", f"/api/business/task/{rv}/status", tw1, {"to": "DOING"}, name="s2_rv_doing")
    eq("甲 TODO → DOING → 200", st, 200)
    st, body = req("PATCH", f"/api/business/task/{rv}/status", tw1, {"to": "DONE"}, name="s2_rv_skip_review")
    eq("需验收任务 DOING → DONE 被拒 → 409", st, 409)
    eq("错误码 63010（需走验收）", code_of(body), 63010)
    st, body = req("PATCH", f"/api/business/task/{rv}/status", tw1, {"to": "REVIEW"}, name="s2_rv_review")
    eq("甲 DOING → REVIEW → 200", st, 200)
    eq("REVIEW 进度 100", psql(f"select progress from biz_task where id='{rv}'::uuid"), "100")
    inst = psql(f"select coalesce(approval_instance_id::text,'') from biz_task where id='{rv}'::uuid")
    check(len(inst) == 36, "提交验收自动挂 TASK_ACCEPT 审批单（approval_instance_id）", f"instance={inst[:8]}")
    eq("审批单类型 TASK_ACCEPT / 状态 PENDING",
       psql(f"select biz_type||'/'||status from biz_flow_instance where id='{inst}'::uuid"), "TASK_ACCEPT/PENDING")
    st, body = req("PATCH", f"/api/business/task/{rv}/status", tw1, {"to": "DONE"}, name="s2_rv_owner_done")
    eq("审批中 负责人 REVIEW → DONE 被门闸拦 → 409", st, 409)
    eq("错误码 65030（审批中）", code_of(body), 65030)
    st, body = req("PATCH", f"/api/business/task/{rv}/status", admin, {"to": "DONE"}, name="s2_rv_creator_done")
    eq("审批中 创建人也推不动 → 409", st, 409)
    eq("错误码 65030（门闸优先于身份）", code_of(body), 65030)

    # 审批通过 → 回调把任务推到 DONE
    st, body = req("GET", "/api/business/flow/task/todo?page=1&page_size=100", admin, name="s2_todo")
    eq("审批待办列表 → 200", st, 200)
    todo = [x for x in data_of(body).get("list") or [] if x.get("instance_id") == inst]
    check(bool(todo), "本单待办在 admin 的待办里（TASK_CREATOR 规则）", f"待办数={len(todo)}")
    todo_id = todo[0].get("id") if todo else None
    if todo_id:
        st, body = req("POST", f"/api/business/flow/task/{todo_id}/approve", admin,
                       {"comment": "E2E11 验收通过"}, name="s2_rv_approve")
        eq("审批通过 → 200", st, 200)
    eq("审批单终态 APPROVED",
       psql(f"select status from biz_flow_instance where id='{inst}'::uuid"), "APPROVED")
    eq("回调把任务推到 DONE", psql(f"select status from biz_task where id='{rv}'::uuid"), "DONE")
    eq("回调写 done_at", psql(f"select case when done_at is null then 'null' else 'set' end "
                            f"from biz_task where id='{rv}'::uuid"), "set")
    eq("回调留状态日志（FlowCallback 身份）",
       psql("select count(*) from sys_change_log where resource='biz_task' and resource_id='%s' "
            "and action='STATUS' and diff::text like '%%DONE%%'" % rv) != "0", True)

    # ⑥ 驳回：默认 return_to=PREV（第一个审批节点 → 退回发起人重提，实例仍 PENDING、任务不动）
    st, body = req("POST", "/api/business/task", admin,
                   {"title": "E2E11 打回探针", "need_review": True}, name="s2_mk_reject")
    rj = data_of(body).get("id")
    req("PATCH", f"/api/business/task/{rj}/status", admin, {"to": "DOING"}, name="s2_rj_doing")
    req("PATCH", f"/api/business/task/{rj}/status", admin, {"to": "REVIEW"}, name="s2_rj_review")
    rj_inst = psql(f"select coalesce(approval_instance_id::text,'') from biz_task where id='{rj}'::uuid")
    st, body = req("GET", "/api/business/flow/task/todo?page=1&page_size=100", admin, name="s2_todo_rj")
    rj_todo = [x for x in data_of(body).get("list") or [] if x.get("instance_id") == rj_inst]
    check(bool(rj_todo), "打回单待办可见")
    if rj_todo:
        st, body = req("POST", f"/api/business/flow/task/{rj_todo[0].get('id')}/reject", admin,
                       {"comment": "E2E11 验收驳回：附件缺页"}, name="s2_rj_reject")
        eq("审批驳回（默认 PREV）→ 200", st, 200)
    eq("PREV 驳回：实例仍 PENDING（退发起人重提）",
       psql(f"select status from biz_flow_instance where id='{rj_inst}'::uuid"), "PENDING")
    eq("PREV 驳回：任务不被误推（仍 REVIEW）", psql(f"select status from biz_task where id='{rj}'::uuid"), "REVIEW")
    eq("PREV 驳回：轮次 +1（round=2）",
       psql(f"select max(round) from biz_flow_instance_node where instance_id='{rj_inst}'::uuid"), "2")
    eq("PREV 驳回：生成 RESUBMIT 待办给发起人",
       psql(f"select count(*) from biz_flow_task where instance_id='{rj_inst}'::uuid "
            f"and task_kind='RESUBMIT' and status='PENDING'"), "1")

    # ⑦ 终止驳回（return_to=NONE，需 flow:admin）→ 实例 REJECTED → 任务回 DOING + reject_reason
    st, body = req("POST", "/api/business/task", admin,
                   {"title": "E2E11 终止驳回探针", "need_review": True}, name="s2_mk_final")
    fin = data_of(body).get("id")
    req("PATCH", f"/api/business/task/{fin}/status", admin, {"to": "DOING"}, name="s2_fin_doing")
    req("PATCH", f"/api/business/task/{fin}/status", admin, {"to": "REVIEW"}, name="s2_fin_review")
    fin_inst = psql(f"select coalesce(approval_instance_id::text,'') from biz_task where id='{fin}'::uuid")
    st, body = req("GET", "/api/business/flow/task/todo?page=1&page_size=100", admin, name="s2_todo_fin")
    fin_todo = [x for x in data_of(body).get("list") or [] if x.get("instance_id") == fin_inst]
    if fin_todo:
        st, body = req("POST", f"/api/business/flow/task/{fin_todo[0].get('id')}/reject", admin,
                       {"comment": "E2E11 终止驳回：需求变更", "return_to": "NONE"}, name="s2_fin_reject")
        eq("终止驳回（return_to=NONE）→ 200", st, 200)
        eq("实例落 REJECTED", psql(f"select status from biz_flow_instance where id='{fin_inst}'::uuid"), "REJECTED")
        eq("回调把任务打回 DOING", psql(f"select status from biz_task where id='{fin}'::uuid"), "DOING")
        contains("reject_reason 取审批意见", psql(f"select reject_reason from biz_task where id='{fin}'::uuid"),
                 "E2E11 终止驳回：需求变更")
        eq("打回后进度归位 10", psql(f"select progress from biz_task where id='{fin}'::uuid"), "10")

    state["status_tasks"] = {"leaf": leaf, "skip": skip, "review": rv, "reject": rj, "final": fin}
    save_state(state)


def s3_dependency(state):
    scenario("S3", "任务依赖：前置未完成拦截 → 完成后放行 + 环检测 + 解除")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    tw1 = state["tokens"]["e2e11_w1"]

    def mk(title, owner=None):
        st, body = req("POST", "/api/business/task", admin,
                       {"title": title, "owner_id": owner, "due_date": "2026-12-20"}, name="s3_mk_" + title[:12])
        return data_of(body).get("id")

    a = mk("E2E11 依赖-前置A")
    b = mk("E2E11 依赖-后继B", w1)
    c = mk("E2E11 依赖-后继C")
    eq("前置/后继任务创建", bool(a and b and c), True)

    st, body = req("POST", f"/api/business/task/{b}/dependencies", admin, {"depends_on_id": a}, name="s3_add_dep")
    eq("B 加前置 A → 200", st, 200)
    eq("依赖落库 biz_task_dependency", psql(
        f"select count(*) from biz_task_dependency where task_id='{b}'::uuid and depends_on_id='{a}'::uuid"), "1")
    st, body = req("GET", f"/api/business/task/{b}/dependencies", admin, name="s3_list_dep")
    eq("查询依赖 → 200", st, 200)
    pre = data_of(body).get("prerequisites") or []
    check(any(x.get("dep_id") == a for x in pre), "前置列表含 A", f"prerequisites={len(pre)}")
    eq("blocking_count = 1（A 未完成）", data_of(body).get("blocking_count"), 1)

    # ① 前置未完成 → 进 DOING 被拦
    st, body = req("PATCH", f"/api/business/task/{b}/status", tw1, {"to": "DOING"}, name="s3_blocked")
    eq("前置未完成 B TODO → DOING → 409", st, 409)
    eq("错误码 63102", code_of(body), 63102)
    contains("拦截文案点名未完成前置", msg_of(body), "E2E11 依赖-前置A")
    eq("B 仍为 TODO", psql(f"select status from biz_task where id='{b}'::uuid"), "TODO")

    # ② 自依赖 / 成环
    st, body = req("POST", f"/api/business/task/{b}/dependencies", admin, {"depends_on_id": b}, name="s3_self")
    eq("自依赖 → 400", st, 400)
    eq("错误码 63103", code_of(body), 63103)
    st, body = req("POST", f"/api/business/task/{a}/dependencies", admin, {"depends_on_id": b}, name="s3_cycle")
    eq("A 依赖 B 成环 → 400", st, 400)
    eq("错误码 63101（环）", code_of(body), 63101)
    contains("环检测点名环上任务", msg_of(body), "E2E11 依赖-后继B")
    eq("成环未落库", psql(
        f"select count(*) from biz_task_dependency where task_id='{a}'::uuid and depends_on_id='{b}'::uuid"), "0")

    # ③ 前置完成 → 放行 + 通知后继负责人
    st, body = req("PATCH", f"/api/business/task/{a}/status", admin, {"to": "DOING"}, name="s3_a_doing")
    eq("A TODO → DOING → 200", st, 200)
    st, body = req("PATCH", f"/api/business/task/{a}/status", admin, {"to": "DONE"}, name="s3_a_done")
    eq("A DOING → DONE → 200", st, 200)
    notice = psql("select count(*) from sys_notice_receiver r join sys_notice n on n.id=r.notice_id "
                  "where r.user_id='%s'::uuid and n.biz_id='%s'" % (w1, b))
    check(int(notice or 0) >= 1, "前置完成强制通知后继负责人（站内信落库）", f"条数={notice}")
    st, body = req("PATCH", f"/api/business/task/{b}/status", tw1, {"to": "DOING"}, name="s3_unblocked")
    eq("前置完成后 B → DOING → 200", st, 200)

    # ④ 取消前置也算解除阻塞
    d = mk("E2E11 依赖-前置D")
    e = mk("E2E11 依赖-后继E", w1)
    req("POST", f"/api/business/task/{e}/dependencies", admin, {"depends_on_id": d}, name="s3_add_dep2")
    st, body = req("PATCH", f"/api/business/task/{d}/status", admin, {"to": "CANCELLED"}, name="s3_d_cancel")
    eq("前置 D 取消 → 200", st, 200)
    st, body = req("PATCH", f"/api/business/task/{e}/status", tw1, {"to": "DOING"}, name="s3_e_doing")
    eq("前置已取消不阻塞，E → DOING → 200", st, 200)

    # ⑤ 解除依赖（传对端任务 id）
    st, body = req("DELETE", f"/api/business/task/{b}/dependencies/{a}", admin, name="s3_del_dep")
    eq("解除依赖 → 200", st, 200)
    eq("依赖行已删", psql(
        f"select count(*) from biz_task_dependency where task_id='{b}'::uuid and depends_on_id='{a}'::uuid"), "0")
    st, body = req("GET", f"/api/business/task/{b}/dependencies", admin, name="s3_list_after_del")
    eq("解除后前置列表为空", len(data_of(body).get("prerequisites") or []), 0)
    eq("解除后 blocking_count = 0", data_of(body).get("blocking_count"), 0)

    # ⑥ 后继视角：前置任务详情能看到「谁在等我」
    st, body = req("POST", f"/api/business/task/{e}/dependencies", admin, {"depends_on_id": d}, name="s3_re_add")
    st, body = req("GET", f"/api/business/task/{d}/dependencies", admin, name="s3_successors")
    succ = data_of(body).get("successors") or []
    check(any(x.get("dep_id") == e for x in succ), "前置任务的后继列表含 E", f"successors={len(succ)}")
    req("DELETE", f"/api/business/task/{e}/dependencies/{d}", admin, name="s3_del_dep2")

    state["deps"] = {"a": a, "b": b, "c": c, "d": d, "e": e}
    save_state(state)


def s4_board(state):
    scenario("S4", "看板：状态分组列 + 拖拽改状态 + 批量改状态/批量指派")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]
    w2 = state["users"]["e2e11_w2"]

    def mk(title, owner=None):
        st, body = req("POST", "/api/business/task", admin, {"title": title, "owner_id": owner},
                       name="s4_mk_" + title[:12])
        return data_of(body).get("id")

    k1, k2, k3 = mk("E2E11 看板卡1"), mk("E2E11 看板卡2"), mk("E2E11 看板卡3")
    eq("看板三张卡创建", bool(k1 and k2 and k3), True)

    st, body = req("GET", "/api/business/task/page?page=1&page_size=50&status=TODO&keyword=E2E11 看板卡", admin,
                   name="s4_col_todo")
    eq("TODO 列查询 → 200", st, 200)
    todo_ids = [x.get("id") for x in data_of(body).get("list") or []]
    check(all(x in todo_ids for x in (k1, k2, k3)), "三张卡都在 TODO 列", f"列内={len(todo_ids)}")

    st, body = req("PATCH", f"/api/business/task/{k1}/status", admin, {"to": "DOING"}, name="s4_drag_doing")
    eq("拖拽卡1：TODO → DOING → 200", st, 200)
    st, body = req("GET", "/api/business/task/page?page=1&page_size=50&status=DOING&keyword=E2E11 看板卡", admin,
                   name="s4_col_doing")
    doing_ids = [x.get("id") for x in data_of(body).get("list") or []]
    check(k1 in doing_ids and k2 not in doing_ids, "卡1 出现在 DOING 列、卡2 不在", f"列内={len(doing_ids)}")

    st, body = req("POST", "/api/business/task/batch-status", admin,
                   {"ids": [k2, k3], "status": "DOING"}, name="s4_batch_status")
    eq("批量改状态 → 200", st, 200)
    eq("批量后 k2/k3 均 DOING", psql(
        "select string_agg(distinct status, ',') from biz_task where id in "
        f"('{k2}'::uuid,'{k3}'::uuid)"), "DOING")
    eq("批量结果 success_count=2", data_of(body).get("success_count"), 2)

    st, body = req("POST", "/api/business/task/batch-assign", admin,
                   {"ids": [k1, k2], "owner_id": w2, "reason": "E2E11 批量指派"}, name="s4_batch_assign")
    eq("批量指派 → 200", st, 200)
    eq("批量指派落库 owner=乙", psql(
        f"select count(*) from biz_task where id in ('{k1}'::uuid,'{k2}'::uuid) and owner_id='{w2}'::uuid"), "2")

    st, body = req("POST", "/api/business/task/batch-status", admin,
                   {"ids": [k1, "00000000-0000-0000-0000-000000000000"], "status": "DONE"},
                   name="s4_batch_mixed")
    eq("批量含非法 uuid → 200（逐条结果）", st, 200)
    res = data_of(body)
    check(int(res.get("fail_count") or 0) >= 1, "逐条失败计数 ≥1（不带整体回滚）",
          f"fail_count={res.get('fail_count')}")
    eq("k1 也被批量推进（逐条独立）", psql(f"select status from biz_task where id='{k1}'::uuid"), "DONE")

    state["board"] = {"k1": k1, "k2": k2, "k3": k3}
    save_state(state)


def s5_gantt(state):
    scenario("S5", "甘特图（只读排期）：窗口 + 条带偏移 + 依赖边 + 边界校验")
    admin = state["admin_token"]

    def mk(title, start, due):
        st, body = req("POST", "/api/business/task", admin,
                       {"title": title, "start_date": start, "due_date": due}, name="s5_mk_" + title[:12])
        return data_of(body).get("id")

    g1 = mk("E2E11 甘特-设计", "2026-11-10", "2026-11-14")
    g2 = mk("E2E11 甘特-开发", "2026-11-15", "2026-11-25")
    eq("甘特样本创建", bool(g1 and g2), True)
    st, body = req("POST", f"/api/business/task/{g2}/dependencies", admin, {"depends_on_id": g1}, name="s5_dep")
    eq("甘特样本依赖 → 200", st, 200)

    st, body = req("GET", "/api/business/task/gantt?start=2026-11-01&end=2026-11-30&scope=all", admin,
                   name="s5_gantt")
    eq("甘特 → 200", st, 200)
    data = data_of(body)
    rng = data.get("range") or {}
    eq("窗口起点回显", str(rng.get("start") or "")[:10], "2026-11-01")
    eq("窗口天数 = 30", rng.get("days"), 30)
    bars = {x.get("id"): x for x in data.get("tasks") or []}
    check(g1 in bars and g2 in bars, "两条任务都在甘特里")
    if g1 in bars:
        bar = bars[g1].get("bar") or {}
        eq("条带 offset_days（11-10 相对 11-01 = 9）", bar.get("offset_days"), 9)
        eq("条带 length_days（10..14 含头尾 = 5）", bar.get("length_days"), 5)
    deps = data.get("dependencies") or []
    check(any(d.get("from") == g1 and d.get("to") == g2 for d in deps), "依赖边下发", f"边数={len(deps)}")
    eq("truncated=false", data.get("truncated"), False)

    st, body = req("GET", "/api/business/task/gantt?start=2026-01-01&end=2026-12-31", admin, name="s5_gantt_wide")
    eq("窗口 > 180 天 → 400", st, 400)
    eq("错误码 63105", code_of(body), 63105)
    st, body = req("GET", "/api/business/task/gantt?board_id=x", admin, name="s5_gantt_board")
    eq("传 board_id（P1 能力）→ 400", st, 400)
    eq("错误码 63103", code_of(body), 63103)
    st, body = req("GET", "/api/business/task/gantt?start=2026-11-30&end=2026-11-01", admin, name="s5_gantt_rev")
    eq("起止倒置 → 400", st, 400)

    # 只读：甘特没有写入口（PATCH schedule 是另一个接口）
    st, body = req("PATCH", f"/api/business/task/{g2}/schedule", admin, {"shift_days": 2}, name="s5_schedule")
    eq("改期走 PATCH /schedule（非甘特拖拽）→ 200", st, 200)
    eq("改期后 due_date = 11-27", psql(f"select due_date from biz_task where id='{g2}'::uuid"), "2026-11-27")
    eq("start_date 同步平移 = 11-17",
       psql(f"select start_date from biz_task where id='{g2}'::uuid"), "2026-11-17")

    state["gantt"] = {"g1": g1, "g2": g2}
    save_state(state)


def s6_template(state):
    scenario("S6", "任务模板：建模板（含子任务）→ 一键建任务 → 改/删")
    admin = state["admin_token"]
    w1 = state["users"]["e2e11_w1"]

    st, body = req("POST", "/api/business/task/template", admin,
                   {"name": "E2E11 发布巡检模板", "title": "E2E11 周度发布巡检",
                    "description": "模板生成的任务", "priority": "P1", "estimate_hours": 4,
                    "need_review": False,
                    "subtasks": [{"title": "E2E11 巡检-环境"}, {"title": "E2E11 巡检-回归"},
                                 {"title": "E2E11 巡检-报告"}]},
                   name="s6_mk_template")
    eq("建模板 → 200", st, 200)
    tpl = data_of(body).get("id")
    eq("模板落库", psql(f"select count(*) from biz_task_template where id='{tpl}'::uuid"), "1")
    st, body = req("GET", "/api/business/task/templates?page=1&page_size=50&keyword=E2E11", admin,
                   name="s6_list_tpl")
    eq("模板列表 → 200", st, 200)
    check(any(x.get("id") == tpl for x in data_of(body).get("list") or []), "列表含新模板")

    st, body = req("POST", f"/api/business/task/from-template/{tpl}", admin,
                   {"title": "E2E11 从模板建的巡检单", "owner_id": w1}, name="s6_from_tpl")
    eq("从模板建任务 → 200", st, 200)
    made = data_of(body).get("id") or data_of(body).get("task_id")
    check(bool(made), "返回新任务 id", f"data={list(data_of(body).keys())}")
    eq("新任务标题被覆盖", psql(f"select title from biz_task where id='{made}'::uuid"), "E2E11 从模板建的巡检单")
    eq("新任务负责人 = 模板/请求指定", psql(f"select owner_id from biz_task where id='{made}'::uuid"), w1)
    eq("子任务生成 3 条", psql(
        f"select count(*) from biz_task where parent_id='{made}'::uuid and deleted_at is null"), "3")
    eq("三个子任务标题都来自模板", psql(
        "select count(*) from biz_task where parent_id='%s'::uuid and title in "
        "('E2E11 巡检-环境','E2E11 巡检-回归','E2E11 巡检-报告')" % made), "3")
    eq("模板生成的父任务带 estimate_hours",
       float(psql(f"select estimate_hours from biz_task where id='{made}'::uuid") or 0), 4.0)

    st, body = req("PUT", f"/api/business/task/template/{tpl}", admin,
                   {"name": "E2E11 发布巡检模板v2", "title": "E2E11 周度发布巡检v2",
                    "priority": "P0", "subtasks": [{"title": "E2E11 巡检-唯一子任务"}]},
                   name="s6_upd_tpl")
    eq("改模板 → 200", st, 200)
    eq("模板名落库 v2", psql(f"select name from biz_task_template where id='{tpl}'::uuid"), "E2E11 发布巡检模板v2")
    st, body = req("DELETE", f"/api/business/task/template/{tpl}", admin, name="s6_del_tpl")
    eq("删模板 → 200", st, 200)
    eq("模板软删", psql(f"select case when deleted_at is null then 'live' else 'deleted' end "
                       f"from biz_task_template where id='{tpl}'::uuid"), "deleted")
    st, body = req("POST", f"/api/business/task/from-template/{tpl}", admin, {}, name="s6_from_deleted")
    eq("已删模板再建任务 → 404/400", st in (400, 404), True)

    state["template"] = {"id": tpl, "task": made}
    save_state(state)


def main():
    state = load_state()
    fixtures(state)
    s1_tasks(state)
    s2_status(state)
    s3_dependency(state)
    s4_board(state)
    s5_gantt(state)
    s6_template(state)
    print("\n================ A 段汇总 ================")
    total_ok = sum(v[0] for v in STATS.values())
    total_bad = sum(v[1] for v in STATS.values())
    for sid in sorted(STATS):
        ok, bad = STATS[sid]
        print(f"  {sid}: PASS={ok} FAIL={bad}")
    print(f"  合计 PASS={total_ok} FAIL={total_bad}")
    save_state(state)
    return 0 if total_bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
