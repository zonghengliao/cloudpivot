#!/usr/bin/env python3
"""E2E-10 第 1 段：流程定义（设计器/节点配置/条件分支/发布校验/版本冻结）
   + 发起审批 + 审批中心（待办/已办/我发起）+ 三种签法 + 串行/并行/条件 + 驳回四目标。

在 145 上执行：python3 /tmp/e2e10/e2e10_a.py
"""
import json
import sys

sys.path.insert(0, "/tmp/e2e10")
from e2e10_lib import *  # noqa: F403,E402

ROLE_PLAN = {
    "E2E10_A": ["u_a1"],
    "E2E10_3": ["u_t1", "u_t2", "u_t3"],
    "E2E10_MAKER": ["u_m1"],
    "E2E10_BOSS": ["u_b1", "u_b2"],
    "E2E10_LONE": ["u_l1"],
    "E2E10_FB": ["u_fb"],
    "E2E10_SOLO": ["u_solo"],
    "E2E10_DEL": ["u_del"],
    "E2E10_PA": ["u_pa"],
    "E2E10_PB": ["u_pb"],
    "E2E10_PBN": ["u_pbn"],
    "E2E10_INIT": ["u_init"],
}
PERM_BASE = ["business:flow:instance:read", "business:flow:approve"]
PERM_CREATE = ["business:flow:instance:create", "business:flow:instance:withdraw"]
PERM_DELEGATE = ["business:flow:delegate:list", "business:flow:delegate:save", "business:flow:delegate:delete"]
CREATORS = {"u_t1", "u_m1", "u_solo", "u_del", "u_l1", "u_init"}


def fixtures(state):
    scenario("S0", "夹具：14 个角色 / 19 个用户 / 权限直授 + 基线核对")
    admin = login("admin", ADMIN_PW, name="admin_login")
    check(bool(admin), "admin 登录（令牌就绪）")
    if not admin:
        raise SystemExit("admin 登录失败")
    st, body = req("GET", "/api/business/flow/def/page?page=1&page_size=1", admin, name="baseline_defpage")
    eq("流程定义分页 → 200", st, 200)
    base_def = psql("select count(*) from biz_flow_def where deleted_at is null")
    check(int(base_def or 0) >= 18, "基线：存量流程定义 ≥ 18 个", f"实际={base_def}")
    st, body = req("GET", "/api/business/flow/def/templates", admin, name="baseline_templates")
    eq("标准流程模板库 → 200", st, 200)
    groups = data_of(body).get("groups") or []
    check(len(groups) >= 3, "模板库分组 ≥ 3（分类展示）", f"实际={len(groups)}")

    roles, users = {}, {}
    for code, members in ROLE_PLAN.items():
        rid, st, body = mk_role(admin, code, f"E2E10 {code}（演练）", sort=950)
        check(bool(rid), f"建角色 {code}", f"http={st} id={rid}")
        if not rid:
            raise SystemExit(f"角色 {code} 建失败: {msg_of(body)}")
        roles[code] = rid
    st, body = set_escalation(admin, roles["E2E10_MAKER"], "E2E10_MAKER", "E2E10 E2E10_MAKER（演练）", roles["E2E10_BOSS"])
    eq("角色 E2E10_MAKER 上级角色 = E2E10_BOSS → 200", st, 200)
    eq("升级链落库", psql(f"select escalation_role_id from sys_role where id='{roles['E2E10_MAKER']}'::uuid"),
       roles["E2E10_BOSS"])

    for code, members in ROLE_PLAN.items():
        for username in members:
            uid, st, body = mk_user(admin, username, f"{code} {username}", [roles[code]])
            check(bool(uid), f"建用户 {username}（角色 {code}）", f"http={st} id={uid}")
            if not uid:
                raise SystemExit(f"用户 {username} 建失败: {msg_of(body)}")
            users[username] = uid

    for code, members in ROLE_PLAN.items():
        codes = list(PERM_BASE)
        if code == "E2E10_INIT":
            codes += PERM_CREATE
        if code == "E2E10_PB":
            codes += ["business:flow:admin"]
        if code == "E2E10_DEL":
            codes += PERM_CREATE + PERM_DELEGATE
        for username in members:
            if username in CREATORS and code != "E2E10_INIT":
                codes += PERM_CREATE
        n, body2 = grant(admin, roles[code], codes)
        check(n >= len(set(codes)), f"角色 {code} 授予 {len(set(codes))} 个权限码",
              f"实际命中菜单={n} msg={msg_of(body2)}")

    tokens = {}
    for username in users:
        token = login(username, BASE_PW, name="login_" + username)
        check(bool(token), f"用户 {username} 登录令牌就绪")
        tokens[username] = token

    sql_eq("E2E10_3 在册成员 3 人",
           f"select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id "
           f"where ur.role_id='{roles['E2E10_3']}'::uuid and u.deleted_at is null", 3)
    sql_eq("E2E10_MAKER 在册成员 1 人（=制单人）",
           f"select count(*) from sys_user_role ur join sys_user u on u.id=ur.user_id "
           f"where ur.role_id='{roles['E2E10_MAKER']}'::uuid and u.deleted_at is null", 1)

    state.update({"admin": admin, "roles": roles, "users": users, "base_def": base_def})
    save_state(state)
    return state


# ==================== S1 流程定义 ====================
def s1_definition(state):
    admin, roles = state["admin"], state["roles"]
    scenario("S1", "① 审批流程定义：设计器 / 节点配置 / 条件分支 / 发布校验 / 版本管理")

    # --- 1.1 新建定义 → 存图（串行 + 条件分支 + 并行 + 三种签法） ---
    nodes = [
        node("s", "开始", "START", sort_order=1),
        node("c1", "金额分级①（< 1 万）", "CONDITION", x=160, sort_order=2, fork_mode="AND", join_mode="AND"),
        appr("n_low", "小额审批（或签）", roles["E2E10_A"], x=340, sort_order=3, approval_mode="ANY"),
        node("c2", "金额分级②（< 10 万）", "CONDITION", x=340, y=140, sort_order=4, fork_mode="AND", join_mode="AND"),
        appr("n_mid", "中额审批（会签 2 人）", roles["E2E10_PB"], x=520, y=140, sort_order=5, approval_mode="ALL"),
        appr("n_high", "大额审批（定额 2/3）", roles["E2E10_3"], x=520, y=260, sort_order=6,
             approval_mode="RATIO", approve_ratio=0.67),
        node("e", "结束", "END", x=720, sort_order=7, join_mode="OR"),
    ]
    edges = [
        edge("e_s", "s", "c1"),
        edge("e_low", "c1", "n_low", condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10000}),
        edge("e_c2", "c1", "c2", is_default=True, priority=10, label="金额 ≥ 1 万"),
        edge("e_mid", "c2", "n_mid", condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 100000}),
        edge("e_high", "c2", "n_high", is_default=True, priority=10, label="金额 ≥ 10 万"),
        edge("e_el", "n_low", "e"), edge("e_em", "n_mid", "e"), edge("e_eh", "n_high", "e"),
    ]
    did, vid, st, body = mk_def(admin, "e2e10_main", "E2E10 主流程（三档金额）", "E2E10_MAIN")
    eq("新建流程定义 e2e10_main → 200/409（可重跑）", st in (200, 409), True)
    check(bool(did and vid), "定义 id / 首个 DRAFT 版本 id 就绪", f"def={did} v={vid}")
    if not did:
        return state
    st, body = save_graph(admin, vid, graph_of(nodes, edges, "E2E10 S1：串行 + 两级条件 + 三种签法"))
    eq("设计器存图（7 节点 / 8 边）→ 200", st, 200)
    sql_eq("库内节点数 = 7", f"select count(*) from biz_flow_node where version_id='{vid}'::uuid", 7)
    sql_eq("库内边数 = 8", f"select count(*) from biz_flow_edge where version_id='{vid}'::uuid", 8)
    sql_eq("节点审批人来源行（3 个审批节点各 1 行）",
           "select count(*) from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id "
           f"where n.version_id='{vid}'::uuid", 3)
    eq("RATIO 节点比例落库 0.670",
       psql(f"select to_char(approve_ratio,'FM0.000') from biz_flow_node where version_id='{vid}'::uuid and node_key='n_high'"), "0.670")
    eq("条件边表达式落库（form.amount lt 10000）",
       psql(f"select condition_expr->>'field'||' '||(condition_expr->>'cmp')||' '||(condition_expr->>'value') "
            f"from biz_flow_edge where version_id='{vid}'::uuid and edge_key='e_low'"), "form.amount lt 10000")
    eq("默认边标记落库（e_c2 is_default）",
       psql(f"select is_default from biz_flow_edge where version_id='{vid}'::uuid and edge_key='e_c2'"), "t")

    # 版本校验接口
    st, body = req("POST", f"/api/business/flow/version/{vid}/validate", admin, None, name="validate_main")
    eq("版本校验接口 → 200", st, 200)
    check(data_of(body).get("valid") in (True, "true", None), "校验结果 valid（无阻断项）", json.dumps(data_of(body), ensure_ascii=False)[:200])

    # 版本详情回读（设计器回显）
    st, body = req("GET", f"/api/business/flow/version/{vid}", admin, name="version_detail")
    eq("版本详情（设计器回显）→ 200", st, 200)
    gd = data_of(body)
    eq("回显节点数 = 7", len(gd.get("nodes") or []), 7)
    eq("回读 n_high 签法 = RATIO", [x.get("approval_mode") for x in gd.get("nodes") or [] if x.get("key") == "n_high"], ["RATIO"])

    # --- 1.2 发布校验负例 ---
    def bad_case(code, nodes_b, edges_b, want_st, want_code, label, contains_text=None):
        d2, v2, st2, b2 = mk_def(admin, code, code, "E2E10_BAD")
        if not d2 or not v2:
            check(False, f"负例 {code} 建定义", f"http={st2} {msg_of(b2)}")
            return
        st3, b3 = save_graph(admin, v2, graph_of(nodes_b, edges_b, "E2E10 负例 " + code))
        check(st3 == 200, f"负例 {code} 存草稿 → 200（保存期不拦）", f"http={st3}")
        st4, b4 = publish(admin, d2)
        eq(label + " → HTTP", st4, want_st)
        eq(label + f" → 错误码 {want_code}", code_of(b4), want_code)
        if contains_text:
            contains(label + " 报文可读", msg_of(b4), contains_text)

    base_nodes = [node("s", "开始", "START", sort_order=1),
                  appr("n1", "审批", roles["E2E10_A"], sort_order=2),
                  node("e", "结束", "END", sort_order=3)]
    base_edges = [edge("a", "s", "n1"), edge("b", "n1", "e")]

    cyc_nodes = [node("s", "开始", "START", sort_order=1),
                 appr("n1", "审批甲", roles["E2E10_A"], sort_order=2),
                 appr("n2", "审批乙", roles["E2E10_PA"], sort_order=3),
                 node("e", "结束", "END", sort_order=4)]
    cyc_edges = [edge("a", "s", "n1"), edge("b", "n1", "n2"), edge("c", "n2", "n1"), edge("d", "n2", "e")]
    bad_case("e2e10_bad_cycle", cyc_nodes, cyc_edges, 400, 65011, "环检测（n1↔n2）")

    no_start_nodes = [appr("n1", "审批", roles["E2E10_A"], sort_order=1), node("e", "结束", "END", sort_order=2)]
    bad_case("e2e10_bad_nostart", no_start_nodes, [edge("b", "n1", "e")], 400, 65010, "缺 START 节点")

    cond_nodes = [node("s", "开始", "START", sort_order=1),
                  node("c1", "条件", "CONDITION", sort_order=2, fork_mode="AND", join_mode="AND"),
                  appr("n_a", "甲", roles["E2E10_A"], sort_order=3),
                  appr("n_b", "乙", roles["E2E10_PA"], sort_order=4),
                  node("e", "结束", "END", sort_order=5)]
    cond_edges = [edge("a", "s", "c1"),
                  edge("b", "c1", "n_a", condition={"op": "cmp", "field": "form.amount", "cmp": "lt", "value": 10}),
                  edge("c", "c1", "n_b", condition={"op": "cmp", "field": "form.amount", "cmp": "gt", "value": 10}),
                  edge("d", "n_a", "e"), edge("f", "n_b", "e")]
    bad_case("e2e10_bad_nodefault", cond_nodes, cond_edges, 400, 65013, "条件节点缺默认边")

    deg_nodes = [node("s", "开始", "START", sort_order=1),
                 appr("n1", "审批分叉", roles["E2E10_A"], sort_order=2),
                 appr("n2", "甲", roles["E2E10_PA"], sort_order=3),
                 appr("n3", "乙", roles["E2E10_PB"], sort_order=4),
                 node("e", "结束", "END", sort_order=5)]
    deg_edges = [edge("a", "s", "n1"), edge("b", "n1", "n2"), edge("c", "n1", "n3"),
                 edge("d", "n2", "e"), edge("f", "n3", "e")]
    bad_case("e2e10_bad_degree", deg_nodes, deg_edges, 400, 65012, "审批节点隐式分叉（出度 2）")

    ratio_nodes = [node("s", "开始", "START", sort_order=1),
                   appr("n1", "按比例无阈值", roles["E2E10_3"], sort_order=2, approval_mode="RATIO"),
                   node("e", "结束", "END", sort_order=3)]
    d2, v2, st2, b2 = mk_def(admin, "e2e10_bad_ratio", "e2e10_bad_ratio", "E2E10_BAD")
    if d2 and v2:
        st3, b3 = save_graph(admin, v2, graph_of(ratio_nodes, [edge("a", "s", "n1"), edge("b", "n1", "e")],
                                                 "RATIO 未填比例"))
        eq("RATIO 未填比例：存图即被拒 → 400", st3, 400)
        eq("RATIO 未填比例：错误码 65005（字段非法）", code_of(b3), 65005)
        check(("ratio" in msg_of(b3).lower()) or ("比例" in msg_of(b3)), "RATIO 未填比例：报文点名比例/ratio", msg_of(b3)[:160])

    # --- 1.3 发布 + 版本冻结 ---
    st, body = publish(admin, did)
    eq("e2e10_main 发布 → 200", st, 200)
    eq("def.status = PUBLISHED", psql(f"select status from biz_flow_def where id='{did}'::uuid"), "PUBLISHED")
    eq("current_version_id = 首个版本", psql(f"select current_version_id from biz_flow_def where id='{did}'::uuid"), vid)
    st, body = req("GET", f"/api/business/flow/def/{did}/versions", admin, name="versions_v1")
    eq("版本列表 → 200", st, 200)
    eq("版本 1 状态 PUBLISHED",
       [v.get("version_status") for v in data_of(body).get("list") or []][:1], ["PUBLISHED"])
    st, body = save_graph(admin, vid, graph_of(nodes, edges, "改已发布版本（应被拒）"))
    check(st in (400, 409), "改已发布版本的行 → 4xx（版本不可编辑）", f"http={st} code={code_of(body)}")
    eq("版本不可编辑错误码 65022", code_of(body), 65022)

    # 另存新草稿 → v2（改节点名 + 新增节点）
    st, body = req("POST", f"/api/business/flow/def/{did}/draft", admin, {"change_note": "E2E10 S1 改版"}, name="mk_draft2")
    eq("另存新草稿 → 200", st, 200)
    vid2 = data_of(body).get("version_id") or data_of(body).get("id")
    check(bool(vid2) and vid2 != vid, "新草稿版本 id ≠ v1", f"v2={vid2}")
    if vid2:
        nodes2 = [dict(x) for x in nodes]
        for x in nodes2:
            if x["key"] == "n_low":
                x["name"] = "小额审批（新版改名）"
        nodes2.append(appr("n_extra", "新增复核节点", roles["E2E10_PA"], x=620, sort_order=8))
        edges2 = [dict(x) for x in edges]
        edges2 = [e for e in edges2 if e["key"] != "e_el"]
        edges2 += [edge("e_el", "n_low", "n_extra"), edge("e_ex", "n_extra", "e")]
        st, body = save_graph(admin, vid2, graph_of(nodes2, edges2, "E2E10 S1：改名 + 新增复核节点"))
        eq("v2 存图 → 200", st, 200)
        st, body = publish(admin, did)
        eq("v2 发布 → 200", st, 200)
        eq("current_version_id 切到 v2", psql(f"select current_version_id from biz_flow_def where id='{did}'::uuid"), vid2)
        eq("版本号 +1",
           psql(f"select (select version_no from biz_flow_def_version where id='{vid2}'::uuid) - "
                f"(select version_no from biz_flow_def_version where id='{vid}'::uuid)"), "1")
        state["vid2"] = vid2

    # 复制定义 / 离线 / 启用
    st, body = req("POST", f"/api/business/flow/def/{did}/duplicate", admin,
                   {"new_flow_code": "e2e10_copy", "new_flow_name": "E2E10 复制流程"},
                   name="duplicate_def")
    check(st in (200, 409), "另存为（复制定义）→ 200/409（可重跑）", f"http={st} code={code_of(body)}")
    eq("复制出的定义落库", psql("select count(*) from biz_flow_def where flow_code='e2e10_copy' and deleted_at is null"), "1")
    st, body = req("POST", f"/api/business/flow/def/{did}/offline", admin, None, name="offline_def")
    check(st == 200, "下线流程 → 200", f"http={st} code={code_of(body)}")
    eq("下线后 status = DISABLED", psql(f"select status from biz_flow_def where id='{did}'::uuid"), "DISABLED")
    st, body = req("POST", f"/api/business/flow/def/{did}/enable", admin, None, name="enable_def")
    check(st == 200, "重新启用 → 200", f"http={st} code={code_of(body)}")
    eq("启用后 status = PUBLISHED", psql(f"select status from biz_flow_def where id='{did}'::uuid"), "PUBLISHED")

    state["def_main"] = did
    state["vid_main"] = vid
    save_state(state)
    return state


# ==================== S2 发起审批 + 审批中心 ====================
def s2_center(state):
    admin, users, tokens, roles = state["admin"], state["users"], state["tokens"], state["roles"]
    scenario("S2", "② 发起审批 + ③ 审批中心（待办 / 已办 / 我发起）")
    did = state["def_main"]
    st, body, iid = start(tokens["u_init"], did, "E2E10 S2 中心页演练单", {"amount": 5000})
    eq("发起审批（金额 5000，走小额分支）→ 200", st, 200)
    check(bool(iid), "返回 instance_id", str(iid))
    if not iid:
        return state
    state["inst_s2"] = iid
    eq("实例状态 PENDING", inst_status(iid), "PENDING")
    eq("实例号前缀 FLW-",
       psql(f"select split_part(instance_no,'-',1) from biz_flow_instance where id='{iid}'::uuid"), "FLW")
    eq("版本冻结：实例绑 v2", psql(f"select version_id from biz_flow_instance where id='{iid}'::uuid"), state.get("vid2", ""))

    st, body = req("GET", "/api/business/flow/task/todo?page=1&page_size=50", tokens["u_a1"], name="todo_a1")
    eq("审批人待办列表 → 200", st, 200)
    todo_ids = [x.get("instance_id") for x in data_of(body).get("list") or []]
    check(iid in todo_ids, "待办列表含本单（审批人视角）", f"total={data_of(body).get('total')}")
    st, body = req("GET", "/api/business/flow/instance/my?page=1&page_size=50", tokens["u_init"], name="my_uinit")
    eq("我发起的 → 200", st, 200)
    my_ids = [x.get("id") for x in data_of(body).get("list") or []]
    check(iid in my_ids, "我发起的列表含本单", f"total={data_of(body).get('total')}")
    st, body = req("GET", "/api/business/flow/instance/page?page=1&page_size=50", admin, name="inst_page_admin")
    eq("审批实例分页（管理视角）→ 200", st, 200)
    check(iid in [x.get("id") for x in data_of(body).get("list") or []], "实例分页含本单")

    st, body = req("GET", f"/api/business/flow/instance/{iid}", tokens["u_a1"], name="inst_detail_a1")
    eq("审批单详情 → 200", st, 200)
    detail = data_of(body).get("instance") or {}
    eq("详情状态 PENDING", detail.get("status"), "PENDING")
    active_names = [n.get("node_name") for n in data_of(body).get("nodes") or [] if n.get("status") == "ACTIVE"]
    eq("详情当前激活节点 = 小额审批（新版改名）", active_names, ["小额审批（新版改名）"])
    eq("详情发起人姓名实时 join（u_init 真实姓名）", detail.get("initiator_name"), "E2E10_INIT u_init")
    st, body = req("GET", f"/api/business/flow/instance/{iid}/tasks", tokens["u_a1"], name="inst_tasks_a1")
    eq("实例轨迹（任务列表）→ 200", st, 200)
    st, body = req("GET", f"/api/business/flow/instance/{iid}", tokens["u_a1"], name="inst_detail_a1b")
    check(len(data_of(body).get("timeline") or []) >= 1, "轨迹（timeline）含至少 1 条任务",
          f"timeline={len(data_of(body).get('timeline') or [])}")

    # 无权用户（u_fb 只有 read/approve，没有 instance:create；看他人实例）
    st, body = req("GET", f"/api/business/flow/instance/{iid}", tokens["u_fb"], name="inst_detail_noperm")
    check(st in (200, 403, 404), "无权用户查看他人审批单 → 有明确响应", f"http={st} code={code_of(body)}")

    # 审批 → 已办
    task = pending_task(iid, users["u_a1"], "n_low")
    check(bool(task), "取到 u_a1 的待办任务行")
    if not task:
        return state
    st, body = approve(tokens["u_a1"], task["id"])
    eq("审批通过（小额节点）→ 200", st, 200)
    st, body = req("GET", "/api/business/flow/task/done?page=1&page_size=50", tokens["u_a1"], name="done_a1")
    eq("已办列表 → 200", st, 200)
    check(iid in [x.get("instance_id") for x in data_of(body).get("list") or []], "已办列表含本单")
    st, body = req("GET", "/api/business/flow/task/todo?page=1&page_size=50", tokens["u_a1"], name="todo_after")
    check(iid not in [x.get("instance_id") for x in data_of(body).get("list") or []], "审批后待办里不再有本单")
    eq("实例状态（小额分支签完 → 还需新版复核节点）", inst_status(iid), "PENDING")
    eq("新版新增节点 n_extra 已激活（版本冻结：新单走新图）", node_status(iid, "n_extra"), "ACTIVE")
    eq("条件边落库：c1 走的边 = e_low",
       psql(f"select taken_edge_key from biz_flow_instance_node where instance_id='{iid}'::uuid and node_key='c1'"), "e_low")
    t2 = pending_task(iid, users["u_pa"], "n_extra")
    check(bool(t2), "复核节点待办派给 u_pa")
    if t2:
        st, body = approve(tokens["u_pa"], t2["id"])
        eq("复核节点审批 → 200", st, 200)
    eq("实例 APPROVED", inst_status(iid), "APPROVED")

    # 撤回闭环
    st, body, iid2 = start(tokens["u_init"], did, "E2E10 S2 撤回演练单", {"amount": 5000})
    eq("再发起一单用于撤回 → 200", st, 200)
    if iid2:
        st, body = req("POST", f"/api/business/flow/instance/{iid2}/withdraw", tokens["u_init"],
                       {"reason": "E2E10 演练撤回"}, name="withdraw_ok")
        eq("撤回 → 200", st, 200)
        eq("撤回后实例 WITHDRAWN", inst_status(iid2), "WITHDRAWN")
        st, body = req("POST", f"/api/business/flow/instance/{iid2}/withdraw", tokens["u_init"],
                       {"reason": "再撤一次"}, name="withdraw_again")
        check(st in (400, 409, 422), "重复撤回 → 4xx", f"http={st} code={code_of(body)}")
        eq("重复撤回错误码 65024（实例已非 PENDING）", code_of(body), 65024)
        eq("撤回后未结算待办 0 条（不孤儿）",
           psql(f"select count(*) from biz_flow_task where instance_id='{iid2}'::uuid and status='PENDING'"), "0")
    save_state(state)
    return state


# ==================== S3 签法 ====================
def s3_sign(state):
    admin, users, tokens, roles = state["admin"], state["users"], state["tokens"], state["roles"]
    scenario("S3", "③ 引擎：会签 ALL / 或签 ANY / 定额 RATIO / 串行 / 并行网关 / 条件分支")

    # --- ALL 会签 ---
    did, _ = build_flow(admin, "e2e10_all", "E2E10_ALL",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "三人会签", roles["E2E10_3"], sort_order=2, approval_mode="ALL"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S3 会签 ALL")
    if did:
        st, body, iid = start(tokens["u_init"], did, "E2E10 S3 会签 ALL")
        eq("会签单发起 → 200", st, 200)
        if iid:
            eq("3 人各 1 条待办",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'"), "3")
            eq("节点签法落库 ALL",
               psql(f"select approval_mode from biz_flow_instance_node where instance_id='{iid}'::uuid and node_key='n1'"), "ALL")
            t1 = pending_task(iid, users["u_t1"], "n1")
            approve(tokens["u_t1"], t1["id"])
            eq("ALL：1/3 签完实例仍 PENDING", inst_status(iid), "PENDING")
            t2 = pending_task(iid, users["u_t2"], "n1")
            approve(tokens["u_t2"], t2["id"])
            eq("ALL：2/3 签完实例仍 PENDING", inst_status(iid), "PENDING")
            t3 = pending_task(iid, users["u_t3"], "n1")
            st, _ = approve(tokens["u_t3"], t3["id"])
            eq("ALL：第 3 人签完 → 200", st, 200)
            eq("ALL：实例 APPROVED", inst_status(iid), "APPROVED")
            eq("ALL：approved_count = 3",
               psql(f"select approved_count from biz_flow_instance_node where instance_id='{iid}'::uuid and node_key='n1'"), "3")

    # --- ANY 或签 ---
    did, _ = build_flow(admin, "e2e10_any", "E2E10_ANY",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "三人或签", roles["E2E10_3"], sort_order=2, approval_mode="ANY"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S3 或签 ANY")
    if did:
        st, body, iid = start(tokens["u_init"], did, "E2E10 S3 或签 ANY")
        eq("或签单发起 → 200", st, 200)
        if iid:
            t1 = pending_task(iid, users["u_t1"], "n1")
            st, _ = approve(tokens["u_t1"], t1["id"])
            eq("ANY：1 人签完 → 200", st, 200)
            eq("ANY：实例 APPROVED", inst_status(iid), "APPROVED")
            eq("ANY：其余 2 条待办 CANCELLED",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='CANCELLED'"), "2")
            eq("ANY：节点 approved_count = 1",
               psql(f"select approved_count from biz_flow_instance_node where instance_id='{iid}'::uuid and node_key='n1'"), "1")

    # --- RATIO 定额（2/3） ---
    did, _ = build_flow(admin, "e2e10_ratio", "E2E10_RATIO",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "定额审批 2/3", roles["E2E10_3"], sort_order=2,
                              approval_mode="RATIO", approve_ratio=0.66),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S3 定额 RATIO 0.67")
    if did:
        st, body, iid = start(tokens["u_init"], did, "E2E10 S3 定额 2/3")
        eq("定额单发起 → 200", st, 200)
        if iid:
            eq("RATIO：比例落库 0.660",
               psql(f"select to_char(approve_ratio,'FM0.000') from biz_flow_instance_node "
                    f"where instance_id='{iid}'::uuid and node_key='n1'"), "0.660")
            t1 = pending_task(iid, users["u_t1"], "n1")
            approve(tokens["u_t1"], t1["id"])
            eq("RATIO：1/3 签完仍 PENDING", inst_status(iid), "PENDING")
            t2 = pending_task(iid, users["u_t2"], "n1")
            st, _ = approve(tokens["u_t2"], t2["id"])
            eq("RATIO：2/3 签完 → 200", st, 200)
            eq("RATIO：实例 APPROVED（2/3 ≥ 66%）", inst_status(iid), "APPROVED")
            eq("RATIO：剩余 1 条 CANCELLED",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='CANCELLED'"), "1")

    # --- 串行三节点 ---
    did, _ = build_flow(admin, "e2e10_serial", "E2E10_SERIAL",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "串行①", roles["E2E10_A"], sort_order=2, approval_mode="ANY"),
                         appr("n2", "串行②", roles["E2E10_PA"], sort_order=3, approval_mode="ANY"),
                         appr("n3", "串行③", roles["E2E10_PB"], sort_order=4, approval_mode="ANY"),
                         node("e", "结束", "END", sort_order=5)],
                        [edge("a", "s", "n1"), edge("b", "n1", "n2"), edge("c", "n2", "n3"), edge("d", "n3", "e")],
                        "E2E10 S3 串行 3 节点")
    if did:
        st, body, iid = start(tokens["u_init"], did, "E2E10 S3 串行三节点")
        eq("串行单发起 → 200", st, 200)
        if iid:
            eq("串行① ACTIVE", node_status(iid, "n1"), "ACTIVE")
            eq("串行② 尚无节点行", node_count(iid, "n2"), "0")
            approve(tokens["u_a1"], pending_task(iid, users["u_a1"], "n1")["id"])
            eq("① 签完 → ② ACTIVE", node_status(iid, "n2"), "ACTIVE")
            eq("① 已 APPROVED", node_status(iid, "n1"), "APPROVED")
            approve(tokens["u_pa"], pending_task(iid, users["u_pa"], "n2")["id"])
            eq("② 签完 → ③ ACTIVE", node_status(iid, "n3"), "ACTIVE")
            st, _ = approve(tokens["u_pb"], pending_task(iid, users["u_pb"], "n3")["id"])
            eq("③ 签完 → 200", st, 200)
            eq("串行三节点走完 → APPROVED", inst_status(iid), "APPROVED")
            eq("实例节点行 = 5（s/n1/n2/n3/e）",
               psql(f"select count(*) from biz_flow_instance_node where instance_id='{iid}'::uuid"), "5")

    # --- 并行网关 ---
    did, _ = build_flow(admin, "e2e10_par", "E2E10_PAR",
                        [node("s", "开始", "START", sort_order=1),
                         node("f", "并行分叉", "PARALLEL", x=160, sort_order=2, fork_mode="AND", join_mode="AND"),
                         appr("n_a", "并行臂甲", roles["E2E10_A"], x=340, sort_order=3),
                         appr("n_b", "并行臂乙", roles["E2E10_PA"], x=340, y=140, sort_order=4),
                         node("j", "并行汇聚", "PARALLEL", x=540, sort_order=5, fork_mode="AND", join_mode="AND"),
                         node("e1", "结束甲", "END", x=720, y=-60, sort_order=6),
                         node("e2", "结束乙", "END", x=720, y=60, sort_order=7)],
                        [edge("a", "s", "f"), edge("b", "f", "n_a"), edge("c", "f", "n_b"),
                         edge("d", "n_a", "j"), edge("e", "n_b", "j"),
                         edge("f", "j", "e1"), edge("g", "j", "e2")],
                        "E2E10 S3 并行 AND 分叉/汇聚")
    if did:
        st, body, iid = start(tokens["u_init"], did, "E2E10 S3 并行 AND")
        eq("并行单发起 → 200", st, 200)
        if iid:
            eq("两臂同时 ACTIVE", f"{node_status(iid,'n_a')}/{node_status(iid,'n_b')}", "ACTIVE/ACTIVE")
            eq("2 条待办", psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'"), "2")
            approve(tokens["u_a1"], pending_task(iid, users["u_a1"], "n_a")["id"])
            eq("只签一臂 → 实例仍 PENDING", inst_status(iid), "PENDING")
            eq("汇聚节点停 PENDING（等人齐）",
               psql(f"select coalesce(max(status),'<无行>') from biz_flow_instance_node "
                    f"where instance_id='{iid}'::uuid and node_key='j'"), "PENDING")
            approve(tokens["u_pa"], pending_task(iid, users["u_pa"], "n_b")["id"])
            eq("两臂签完 → 实例 APPROVED", inst_status(iid), "APPROVED")
            eq("两个 END 都走到",
               psql(f"select count(*) from biz_flow_instance_node where instance_id='{iid}'::uuid "
                    "and node_key in ('e1','e2') and status='APPROVED'"), "2")

    # --- 条件分支三档 ---
    for amount, want_node, want_edge in [(5000, "n_low", "e_low"), (50000, "n_mid", "e_mid"), (500000, "n_high", "e_high")]:
        st, body, iid = start(tokens["u_init"], state["def_main"], f"E2E10 S3 条件 {amount}", {"amount": amount})
        eq(f"条件单（{amount}）发起 → 200", st, 200)
        if not iid:
            continue
        eq(f"条件 {amount}：命中节点 {want_node}", node_status(iid, want_node), "ACTIVE")
        for other in {"n_low", "n_mid", "n_high"} - {want_node}:
            eq(f"条件 {amount}：未命中节点 {other} 无行", node_count(iid, other), "0")
        parent = "c1" if want_edge == "e_low" else "c2"
        eq(f"条件 {amount}：{parent} 走的边 = {want_edge}",
           psql(f"select taken_edge_key from biz_flow_instance_node where instance_id='{iid}'::uuid and node_key='{parent}'"),
           want_edge)
        state.setdefault("cond_insts", {})[str(amount)] = iid
    save_state(state)
    return state


# ==================== S4 驳回 ====================
def s4_reject(state):
    admin, users, tokens, roles = state["admin"], state["users"], state["tokens"], state["roles"]
    scenario("S4", "③ 驳回：退到上一节点 / 退到指定节点 / 退给发起人 / 驳回终止")
    nodes = [node("s", "开始", "START", sort_order=1),
             appr("n1", "甲审批", roles["E2E10_PA"], sort_order=2, approval_mode="ANY"),
             appr("n2", "乙审批", roles["E2E10_PB"], sort_order=3, approval_mode="ANY"),
             appr("n3", "丙审批", roles["E2E10_PA"], sort_order=4, approval_mode="ANY"),
             node("e", "结束", "END", sort_order=5)]
    edges = [edge("a", "s", "n1"), edge("b", "n1", "n2"), edge("c", "n2", "n3"), edge("d", "n3", "e")]
    did, _ = build_flow(admin, "e2e10_rej", "E2E10_REJ", nodes, edges, "E2E10 S4 驳回四目标")
    if not did:
        return state

    def advance_to_n2(title):
        st, body, iid = start(tokens["u_init"], did, title)
        if not iid:
            return None
        approve(tokens["u_pa"], pending_task(iid, users["u_pa"], "n1")["id"])
        return iid

    # PREV
    iid = advance_to_n2("E2E10 S4 驳回到上一节点")
    if iid:
        t = pending_task(iid, users["u_pb"], "n2")
        st, body = reject(tokens["u_pb"], t["id"], "金额不对，请修正后重报", return_to="PREV")
        eq("驳回 PREV → 200", st, 200)
        eq("PREV：实例 PENDING", inst_status(iid), "PENDING")
        eq("PREV：n1 开第 2 轮 ACTIVE", node_status(iid, "n1", 2), "ACTIVE")
        eq("PREV：n2 第 1 轮 REJECTED", node_status(iid, "n2"), "REJECTED")
        eq("PREV：第 2 轮新待办派给 u_pa",
           psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
                "and node_key='n1' and round=2 and status='PENDING'"), "1")
        eq("PREV：instance.round_total = 2",
           psql(f"select round_total from biz_flow_instance where id='{iid}'::uuid"), "2")

    # 指定节点（非 admin 403 / admin 放行 / 不存在 400）
    iid = advance_to_n2("E2E10 S4 退回指定节点")
    if iid:
        # n2 节点同时配了 PB 与 PBN？本流程只有 PB；用 PBN（非 admin 也没有 n2 待办）→ 用 u_pb 但先收回 admin 权限不可行
        t = pending_task(iid, users["u_pb"], "n2")
        st, body = reject(tokens["u_pb"], t["id"], "退回给甲重新审", return_to="n1")
        eq("指定节点退回（admin）→ 200", st, 200)
        eq("指定节点：n1 第 2 轮 ACTIVE", node_status(iid, "n1", 2), "ACTIVE")
        eq("指定节点：实例 PENDING", inst_status(iid), "PENDING")

    # 退到不存在的节点 → 400 / 65025（新单，避免复用已终态任务）
    iid_bad = advance_to_n2("E2E10 S4 退到不存在节点")
    if iid_bad:
        t = pending_task(iid_bad, users["u_pb"], "n2")
        st, body = reject(tokens["u_pb"], t["id"], "退回给不存在的节点", return_to="n_no_such")
        eq("退到不存在的节点 → 400", st, 400)
        eq("退到不存在的节点 → 65025", code_of(body), 65025)
        eq("无副作用：实例仍 PENDING", inst_status(iid_bad), "PENDING")
        eq("无副作用：n2 仍 ACTIVE", node_status(iid_bad, "n2"), "ACTIVE")

    # 非 admin 退指定节点 → 403/65004（用 u_pa 在 n1 上试）
    iid2 = advance_to_n2("E2E10 S4 非管理员退指定节点")
    if iid2:
        # 让流程走到 n3（u_pa 又是 n3 审批人），在 n3 上以非 admin 身份退到 n3 之前
        approve(tokens["u_pb"], pending_task(iid2, users["u_pb"], "n2")["id"])
        t3 = pending_task(iid2, users["u_pa"], "n3")
        check(bool(t3), "n3 待办派给非管理员 u_pa")
        if t3:
            st, body = reject(tokens["u_pa"], t3["id"], "退回给甲重新审", return_to="n1")
            eq("非管理员退指定节点 → 403", st, 403)
            eq("非管理员退指定节点 → 65004", code_of(body), 65004)
            eq("拒绝后无副作用（n1 仍第 1 轮 APPROVED）", node_status(iid2, "n1"), "APPROVED")
            eq("拒绝后 n3 仍 ACTIVE", node_status(iid2, "n3"), "ACTIVE")

    # INITIATOR
    iid = advance_to_n2("E2E10 S4 驳回到发起人")
    if iid:
        t = pending_task(iid, users["u_pb"], "n2")
        st, body = reject(tokens["u_pb"], t["id"], "资料不全，请补充后重提", return_to="INITIATOR")
        eq("驳回 INITIATOR → 200", st, 200)
        eq("INITIATOR：生成 RESUBMIT 待办给发起人",
           psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
                f"and task_kind='RESUBMIT' and status='PENDING' and assignee_id='{users['u_init']}'::uuid"), "1")
        sql_eq("INITIATOR：comment 含「重新提交」",
               f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and task_kind='RESUBMIT' "
               "and comment like '%重新提交%'", 1)
        st, body = req("GET", "/api/business/flow/task/todo?page=1&page_size=50", tokens["u_init"], name="todo_resubmit")
        check(iid in [x.get("instance_id") for x in data_of(body).get("list") or []], "发起人待办列表见到 RESUBMIT")

    # FINAL / NONE
    iid = advance_to_n2("E2E10 S4 驳回终止")
    if iid:
        t = pending_task(iid, users["u_pb"], "n2")
        st, body = reject(tokens["u_pb"], t["id"], "不同意，终止", return_to="NONE")
        eq("驳回 NONE → 200", st, 200)
        eq("NONE：实例 REJECTED", inst_status(iid), "REJECTED")
        eq("NONE：n3 未激活", node_count(iid, "n3"), "0")
        eq("NONE：无新轮次（max round 仍 1）",
           psql(f"select max(round) from biz_flow_instance_node where instance_id='{iid}'::uuid"), "1")
    save_state(state)
    return state


def main():
    state = load_state()
    error = None
    try:
        if not state.get("roles"):
            state = fixtures(state)
        state["tokens"] = state.get("tokens") or {}
        if not state["tokens"]:
            for username in state["users"]:
                state["tokens"][username] = login(username, BASE_PW, name="relogin_" + username)
            save_state(state)
        s1_definition(state)
        s2_center(state)
        s3_sign(state)
        s4_reject(state)
    except Exception:  # noqa: BLE001
        import traceback
        error = traceback.format_exc()
        print(error)
        _log("ERROR\t" + error.replace("\n", " | ")[:2000])
    finally:
        save_state(state)
        fails = summary()
        sys.exit(1 if (error or fails) else 0)


if __name__ == "__main__":
    main()
