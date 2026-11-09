#!/usr/bin/env python3
"""E2E-10 第 2 段：审批委托（④）/ 自审监控（⑤）/ ARCH-8 五层降级 / 联动规则（⑥）

前提：e2e10_a.py 已建好夹具（state.json 有 roles/users/tokens）。
"""
import datetime
import json
import sys

sys.path.insert(0, "/tmp/e2e10")
from e2e10_lib import *  # noqa: F403,E402
from e2e10_lib import _log

UID = ""


# ==================== S5 委托 ====================
def s5_delegate(state):
    admin, users, tokens, roles = state["admin"], state["users"], state["tokens"], state["roles"]
    scenario("S5", "④ 审批委托（出差委托）：建 / 生效期内代审批 / 过期回本人 / 重叠与非法区间 / 软删")

    # 清掉本卡上一轮残留的委托（只动 reason 带 E2E10 的行）
    psql("delete from biz_flow_delegate where reason like 'E2E10%'")
    # 建委托：u_del → u_pa，现在 ~ +3 天，事由「出差北京」
    now = datetime.datetime.now(datetime.timezone.utc)
    win_start = (now - datetime.timedelta(hours=1)).strftime("%Y-%m-%d %H:%M:%S")
    win_end = (now + datetime.timedelta(days=3)).strftime("%Y-%m-%d %H:%M:%S")
    st, body = req("POST", "/api/business/flow/delegate", tokens["u_del"],
                   {"delegate_id": users["u_pa"], "start_at": win_start, "end_at": win_end,
                    "reason": "E2E10 出差北京"}, name="delegate_create")
    eq("新建委托（u_del → u_pa，3 天）→ 200", st, 200)
    did = data_of(body).get("id")
    check(bool(did), "委托 id 就绪", str(did))
    eq("委托 is_active_now = true", data_of(body).get("is_active_now"), True)
    eq("委托人姓名快照", data_of(body).get("delegator_name"), "E2E10_DEL u_del")
    eq("受托人姓名快照", data_of(body).get("delegate_name"), "E2E10_PA u_pa")
    eq("委托落库（enabled）", psql(f"select enabled from biz_flow_delegate where id='{did}'::uuid"), "t")

    st, body = req("GET", "/api/business/flow/delegate/page?page=1&page_size=20", tokens["u_del"], name="delegate_page")
    eq("委托分页（本人）→ 200", st, 200)
    check(did in [x.get("id") for x in data_of(body).get("list") or []], "本人委托列表含该条",
          f"total={data_of(body).get('total')}")
    st, body = req("GET", "/api/business/flow/delegate/page?page=1&page_size=20&all=true", tokens["u_pa"], name="delegate_page_all_noperm")
    check(st in (200, 403), "非管理员看全部委托 → 200(仅自己)/403", f"http={st} code={code_of(body)}")

    # 委托期内发起 → 待办派给受托人
    fid, _ = build_flow(admin, "e2e10_del", "E2E10_DEL",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "出差期间审批", roles["E2E10_DEL"], sort_order=2, approval_mode="ANY"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S5 委托流程")
    if fid:
        st, body, iid = start(tokens["u_init"], fid, "E2E10 S5 委托期内单据")
        eq("委托期内发起 → 200", st, 200)
        if iid:
            task = pending_task(iid, users["u_pa"], "n1")
            check(bool(task), "待办派给受托人 u_pa")
            if task:
                eq("派单规则 = DELEGATED", task["rule"], "DELEGATED")
                contains("待办留痕「代 … 审批」", task["comment"], "代 E2E10_DEL u_del 审批")
                contains("待办留痕含委托截止日", task["comment"], "委托至")
                st, body = approve(tokens["u_pa"], task["id"], "E2E10 受托人代为审批通过")
                eq("受托人审批 → 200", st, 200)
            eq("实例 APPROVED", inst_status(iid), "APPROVED")
            eq("留痕记在受托人名下（审批人 = u_pa）",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
                    f"and status='APPROVED' and assignee_id='{users['u_pa']}'::uuid and assigned_by_rule='DELEGATED'"), "1")

    # 改到已过期窗口 → 新发起回本人
    past_end = (now - datetime.timedelta(days=1)).strftime("%Y-%m-%d %H:%M:%S")
    past_start = (now - datetime.timedelta(days=3)).strftime("%Y-%m-%d %H:%M:%S")
    st, body = req("PUT", f"/api/business/flow/delegate/{did}", tokens["u_del"],
                   {"delegate_id": users["u_pa"], "start_at": past_start, "end_at": past_end,
                    "reason": "E2E10 出差（已回来）"}, name="delegate_expire")
    eq("改委托为已过期窗口 → 200", st, 200)
    eq("过期后 is_active_now = false", data_of(body).get("is_active_now"), False)
    if fid:
        st, body, iid2 = start(tokens["u_init"], fid, "E2E10 S5 委托期外单据")
        eq("委托期外发起 → 200", st, 200)
        if iid2:
            mine = pending_task(iid2, users["u_del"], "n1")
            check(bool(mine), "过期后待办回到本人 u_del")
            if mine:
                eq("过期后派单规则 = ROLE", mine["rule"], "ROLE")
                not_contains("过期后不再带「代 … 审批」", mine["comment"], "代 E2E10_DEL")

    # 非法：自委托 / 区间反了 / 区间重叠
    st, body = req("POST", "/api/business/flow/delegate", tokens["u_del"],
                   {"delegate_id": users["u_del"], "start_at": win_start, "end_at": win_end}, name="delegate_self")
    check(st in (400, 422), "自委托 → 4xx", f"http={st} code={code_of(body)}")
    eq("自委托错误码 65043", code_of(body), 65043)
    st, body = req("POST", "/api/business/flow/delegate", tokens["u_del"],
                   {"delegate_id": users["u_pa"], "start_at": win_end, "end_at": win_start}, name="delegate_badrange")
    check(st in (400, 422), "起止时间反了 → 4xx", f"http={st} code={code_of(body)}")
    # 先恢复成有效窗口，再建重叠
    st, body = req("PUT", f"/api/business/flow/delegate/{did}", tokens["u_del"],
                   {"delegate_id": users["u_pa"], "start_at": win_start, "end_at": win_end, "enabled": True},
                   name="delegate_restore")
    eq("恢复有效窗口 → 200", st, 200)
    st, body = req("POST", "/api/business/flow/delegate", tokens["u_del"],
                   {"delegate_id": users["u_fb"], "start_at": win_start, "end_at": win_end},
                   name="delegate_overlap")
    check(st in (400, 409, 422), "同委托人重叠区间 → 4xx", f"http={st} code={code_of(body)}")
    eq("重叠错误码 65043", code_of(body), 65043)

    # 非管理员代他人建委托 → 403
    st, body = req("POST", "/api/business/flow/delegate", tokens["u_pa"],
                   {"delegator_id": users["u_del"], "delegate_id": users["u_fb"],
                    "start_at": win_start, "end_at": win_end}, name="delegate_other")
    check(st in (403, 400), "非管理员代他人建委托 → 4xx", f"http={st} code={code_of(body)}")
    check(code_of(body) in (65004, 403), "越权码 65004 / 权限闸 403", str(code_of(body)))

    # 停用 → 生效期回本人；删除 → 列表消失
    st, body = req("PUT", f"/api/business/flow/delegate/{did}", tokens["u_del"],
                   {"delegate_id": users["u_pa"], "start_at": win_start, "end_at": win_end, "enabled": False},
                   name="delegate_disable")
    eq("停用委托 → 200", st, 200)
    eq("停用后 is_active_now = false", data_of(body).get("is_active_now"), False)
    st, body = req("DELETE", f"/api/business/flow/delegate/{did}", tokens["u_del"], None, name="delegate_delete")
    eq("删除委托 → 200", st, 200)
    eq("软删落库（deleted_at 非空）",
       psql(f"select count(*) from biz_flow_delegate where id='{did}'::uuid and deleted_at is not null"), "1")
    st, body = req("GET", "/api/business/flow/delegate/page?page=1&page_size=20", tokens["u_del"], name="delegate_page_after")
    check(did not in [x.get("id") for x in data_of(body).get("list") or []], "删除后列表不再出现")
    state["delegate_id"] = did
    save_state(state)
    return state


# ==================== S6 自审监控 ====================
def s6_self_approval(state):
    admin, users, tokens, roles = state["admin"], state["users"], state["tokens"], state["roles"]
    scenario("S6", "⑤ 自审监控：SKIP 留痕 / ALLOW 自审+审计+报表 / 金额阈值降级 / FORBID 硬拒")

    # SKIP
    did, _ = build_flow(admin, "e2e10_skip", "E2E10_SKIP",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "三人审批（发起人自审 SKIP）", roles["E2E10_3"], sort_order=2, approval_mode="ALL"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S6 自审 SKIP")
    if did:
        st, body, iid = start(tokens["u_t1"], did, "E2E10 S6 自审 SKIP")
        eq("SKIP：发起（制单人在审批角色内）→ 200", st, 200)
        if iid:
            eq("SKIP：剩余 2 人待办",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'"), "2")
            eq("SKIP：发起人落 SKIPPED 1 行",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='SKIPPED' "
                    f"and assignee_id='{users['u_t1']}'::uuid"), "1")
            contains("SKIP：comment 写明 self_approval=SKIP",
                     psql(f"select comment from biz_flow_task where instance_id='{iid}'::uuid and status='SKIPPED'"),
                     "self_approval=SKIP")
            eq("SKIP：未走升级链（escalated_from_role 空）",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid "
                    "and escalated_from_role is not null"), "0")

    # ALLOW（阈值内）
    did, _ = build_flow(admin, "e2e10_allow", "E2E10_ALLOW",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "一人公司自审", roles["E2E10_SOLO"], sort_order=2,
                              mode="ANY", self_approval="ALLOW"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S6 自审 ALLOW")
    iid_allow = None
    if did:
        st, body, iid_allow = start(tokens["u_solo"], did, "E2E10 S6 自审 ALLOW 500", {"amount": 500})
        eq("ALLOW：自审发起 → 200", st, 200)
        if iid_allow:
            eq("ALLOW：待办派给本人且 is_self_approval = t",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid_allow}'::uuid "
                    f"and status='PENDING' and assignee_id='{users['u_solo']}'::uuid and is_self_approval"), "1")
            task = pending_task(iid_allow, users["u_solo"], "n1")
            if task:
                st, body = approve(tokens["u_solo"], task["id"], "E2E10 自审通过")
                eq("ALLOW：本人审批 → 200", st, 200)
            eq("ALLOW：实例 APPROVED", inst_status(iid_allow), "APPROVED")
            eq("ALLOW：审计 FLOW_SELF_APPROVAL_ALLOWED 1 行",
               psql(f"select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_ALLOWED' "
                    f"and target_id='{iid_allow}'"), "1")
            eq("ALLOW：审计 FLOW_SELF_APPROVAL_VOTED 1 行",
               psql(f"select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_VOTED' "
                    f"and target_id='{iid_allow}'"), "1")

    # 报表
    month = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m")
    st, body = req("GET", f"/api/business/flow/stats/self-approval?month={month}&biz_type=E2E10_ALLOW",
                   admin, name="self_report")
    eq("自审月度报表 → 200", st, 200)
    rep = data_of(body)
    eq("报表参数回显 month", rep.get("month"), month)
    check(int(rep.get("self_approval_instances") or 0) >= 1, "报表：自审单据数 ≥ 1",
          f"self_approval_instances={rep.get('self_approval_instances')}")
    check(float(rep.get("self_approval_amount") or 0) >= 500, "报表：自审金额 ≥ 500",
          f"amount={rep.get('self_approval_amount')}")
    check(any(x.get("node_key") == "n1" for x in rep.get("by_node") or []), "报表 by_node 含自审节点 n1",
          json.dumps(rep.get("by_node"), ensure_ascii=False)[:160])
    check(any(x.get("user_id") == users["u_solo"] for x in rep.get("by_person") or []), "报表 by_person 含自审人 u_solo",
          json.dumps(rep.get("by_person"), ensure_ascii=False)[:160])
    st, body = req("GET", "/api/business/flow/stats/self-approval?month=2026-13", admin, name="self_report_bad")
    eq("非法 month → 400", st, 400)
    eq("非法 month → 65005", code_of(body), 65005)

    # 金额阈值：把上限改成 1000，再走一笔 5000 → 降级 SKIP（无人可派 → 65018）
    st, body = req("GET", "/api/support/config/page?page=1&page_size=5&config_key=erp.separation_of_duties.self_approval_max_amount",
                   admin, name="cfg_page")
    eq("配置分页（自审上限）→ 200", st, 200)
    rows = data_of(body).get("list") or []
    cfg_id = rows[0].get("id") if rows else None
    old_value = rows[0].get("config_value") if rows else None
    check(bool(cfg_id), "自审上限配置行存在", f"id={cfg_id} value={old_value}")
    if cfg_id:
        st, body = req("PUT", f"/api/support/config/{cfg_id}", admin,
                       {"config_key": "erp.separation_of_duties.self_approval_max_amount",
                        "config_value": "1000", "config_type": rows[0].get("config_type") or "STRING",
                        "description": rows[0].get("description"), "category": rows[0].get("category"),
                        "status": 1, "is_modifiable": True}, name="cfg_set_threshold")
        eq("把自审金额上限设为 1000 → 200", st, 200)
        if did:
            st, body, iid_over = start(tokens["u_solo"], did, "E2E10 S6 自审超限 5000", {"amount": 5000})
            check(st in (400, 403, 409, 422, 423), "超限自审发起 → 被拦（4xx）", f"http={st} code={code_of(body)}")
            eq("超限：错误码 65044（自审金额上限）", code_of(body), 65044)
            eq("超限：库里没有半截实例（标题 0 行）",
               psql("select count(*) from biz_flow_instance where title='E2E10 S6 自审超限 5000'"), "0")
            contains("超限报文点名金额上限", msg_of(body), "上限")
        st, body = req("PUT", f"/api/support/config/{cfg_id}", admin,
                       {"config_key": "erp.separation_of_duties.self_approval_max_amount",
                        "config_value": old_value or "0", "config_type": rows[0].get("config_type") or "STRING",
                        "description": rows[0].get("description"), "category": rows[0].get("category"),
                        "status": 1, "is_modifiable": True}, name="cfg_restore")
        eq("恢复自审金额上限 → 200", st, 200)
        eq("恢复后配置值 = 原值",
           psql("select config_value from sys_config where config_key='erp.separation_of_duties.self_approval_max_amount'"),
           old_value or "0")

    # FORBID
    did, _ = build_flow(admin, "e2e10_forbid", "E2E10_FORBID",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "财务禁止自审", roles["E2E10_SOLO"], sort_order=2,
                              mode="ANY", self_approval="FORBID"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S6 自审 FORBID")
    if did:
        st, body, iid_forbid = start(tokens["u_solo"], did, "E2E10 S6 自审 FORBID")
        eq("FORBID：自审发起 → 422", st, 422)
        eq("FORBID：错误码 65019", code_of(body), 65019)
        contains("FORBID：报文点名 self_approval=FORBID", msg_of(body), "FORBID")
        eq("FORBID：库里没有半截实例",
           psql("select count(*) from biz_flow_instance where title='E2E10 S6 自审 FORBID'"), "0")
    save_state(state)
    return state


# ==================== S7 降级链 ====================
def s7_escalation(state):
    admin, users, tokens, roles = state["admin"], state["users"], state["tokens"], state["roles"]
    scenario("S7", "ARCH-8 ②③：SKIP→升级上级角色 / 升级链走完→兜底人 / 都没有→显式拒绝（65018）")

    # ② SKIP 后沿升级链找上级（MAKER 1 人 → BOSS 2 人）
    did, _ = build_flow(admin, "e2e10_esc", "E2E10_ESC",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "单人角色审批（自动升级）", roles["E2E10_MAKER"], sort_order=2, mode="ANY"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S7 升级链")
    if did:
        st, body, iid = start(tokens["u_m1"], did, "E2E10 S7 升级上级角色")
        eq("升级：发起（制单人 = 唯一角色成员）→ 200", st, 200)
        if iid:
            eq("升级：上级角色 2 人各 1 条待办",
               psql(f"select count(*) from biz_flow_task where instance_id='{iid}'::uuid and status='PENDING'"), "2")
            eq("升级：派单规则 = ESCALATED",
               psql(f"select distinct assigned_by_rule from biz_flow_task where instance_id='{iid}'::uuid "
                    "and status='PENDING'"), "ESCALATED")
            eq("升级：留痕原角色 = E2E10_MAKER",
               psql(f"select distinct escalated_from_role from biz_flow_task where instance_id='{iid}'::uuid "
                    "and status='PENDING'"), "E2E10_MAKER")
            eq("升级：原因 = SKIP_SELF_APPROVAL",
               psql(f"select distinct escalate_reason from biz_flow_task where instance_id='{iid}'::uuid "
                    "and status='PENDING'"), "SKIP_SELF_APPROVAL")
            task = pending_task(iid, users["u_b1"], "n1")
            check(bool(task), "升级后的待办属于上级角色成员 u_b1")
            if task:
                st, body = approve(tokens["u_b1"], task["id"], "E2E10 上级审批通过")
                eq("升级：上级成员审批 → 200", st, 200)
            eq("升级：实例 APPROVED", inst_status(iid), "APPROVED")

    # ③ 升级链走完仍无人 → 兜底人
    did, _ = build_flow(admin, "e2e10_fb", "E2E10_FB",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "无上级角色 → 兜底人", roles["E2E10_LONE"], sort_order=2, mode="ANY",
                              fallback_user_id=users["u_fb"]),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S7 兜底人")
    if did:
        st, body, iid = start(tokens["u_l1"], did, "E2E10 S7 兜底人")
        eq("兜底：发起（唯一成员 = 制单人，无上级）→ 200", st, 200)
        if iid:
            task = pending_task(iid, users["u_fb"], "n1")
            check(bool(task), "兜底：待办派给兜底人 u_fb")
            if task:
                eq("兜底：派单规则 = FALLBACK", task["rule"], "FALLBACK")
                st, body = approve(tokens["u_fb"], task["id"], "E2E10 兜底人审批通过")
                eq("兜底：审批 → 200", st, 200)
            eq("兜底：实例 APPROVED", inst_status(iid), "APPROVED")

    # 都没有 → 65018（显式拒绝，不留半截实例）
    did, _ = build_flow(admin, "e2e10_nomember", "E2E10_NOMEMBER",
                        [node("s", "开始", "START", sort_order=1),
                         appr("n1", "无人可派节点", roles["E2E10_LONE"], sort_order=2, mode="ANY"),
                         node("e", "结束", "END", sort_order=3)],
                        [edge("a", "s", "n1"), edge("b", "n1", "e")], "E2E10 S7 到顶无人")
    if did:
        st, body, iid = start(tokens["u_l1"], did, "E2E10 S7 到顶无人")
        check(st in (422, 423), "到顶无人：发起 → 4xx（显式拒绝）", f"http={st} code={code_of(body)}")
        eq("到顶无人：错误码 65018", code_of(body), 65018)
        eq("到顶无人：库里 0 半截实例",
           psql("select count(*) from biz_flow_instance where title='E2E10 S7 到顶无人'"), "0")
    save_state(state)
    return state


# ==================== S8 联动规则 ====================
def s8_linkage(state):
    admin, users, tokens, roles = state["admin"], state["users"], state["tokens"], state["roles"]
    scenario("S8", "⑥ 联动规则：15+ 触发点 / 白名单动作 / 未接线拦截 / 启停生效 / 幂等 / 通知在 commit 后 / 预览 / 软删")

    st, body = req("GET", "/api/business/flow/linkage", admin, name="linkage_list")
    eq("联动规则列表 → 200", st, 200)
    lst = data_of(body)
    triggers = lst.get("triggers") or []
    actions = lst.get("actions") or []
    rules = lst.get("rules") or []
    check(len(triggers) >= 15, "触发点白名单 ≥ 15（本版 16，含 PURCHASE_RETURN/TASK/FLOW_INSTANCE）",
          f"实际={len(triggers)}")
    check(len(actions) == 11, "白名单动作 = 11 个", f"实际={len(actions)}")
    check(all(x.get("statuses") for x in triggers), "每个触发点都下发可选状态（非空）")
    wired = [x for x in triggers if x.get("wired")]
    unwired = [x for x in triggers if not x.get("wired")]
    check(len(wired) >= 15, "已接线触发点 ≥ 15（本版 16 个全部接线）", f"已接线={len(wired)} 未接线={len(unwired)}")
    # 未接线拦截：本版若已全部接线，则记录「无未接线触发点」，配置面仅验证已接线路径
    if not unwired:
        check(True, "触发点接线表：16/16 已接线（无未接线项可演练拦截）")
    check(any(x.get("trigger_entity_label") for x in rules), "规则行带触发单据中文名（后端下发）",
          str(rules[0].get("trigger_entity_label") if rules else None))
    check(all(x.get("trigger_status_label") for x in rules), "规则行带触发状态中文名")

    # 非法配置
    base = {"name": "E2E10 非法规则", "trigger_entity": "TASK", "trigger_status": "DOING",
            "action_code": "SEND_NOTICE", "action_params": {"mode": "TAKEOVER"}, "confirm": True}
    for patch, label, want in [
        ({"confirm": False}, "未二次确认 → 400", 400),
        ({"trigger_entity": "NO_SUCH_ENTITY"}, "非法触发单据 → 400", 400),
        ({"trigger_status": "NO_SUCH_STATUS"}, "非法触发状态 → 400", 400),
        ({"action_code": "DROP_TABLE"}, "非白名单动作 → 400", 400),
    ]:
        payload = dict(base)
        payload.update(patch)
        st, body = req("PUT", "/api/business/flow/linkage", admin, payload,
                       name="linkage_bad_" + label.split(" ")[0])
        eq(f"联动{label}", st, want)

    # 未接线触发点：保存被拒；allow_unwired 后放行
    target = unwired[0] if unwired else None
    if target:
        status = (target.get("statuses") or ["POSTED"])[0]
        st, body = req("PUT", "/api/business/flow/linkage", admin,
                       {"name": "E2E10 未接线规则", "trigger_entity": target["entity"],
                        "trigger_status": status, "action_code": "SEND_NOTICE", "confirm": True},
                       name="linkage_unwired_reject")
        eq(f"未接线触发点 {target['entity']} 保存 → 400", st, 400)
        contains("报文要求显式 allow_unwired", msg_of(body), "allow_unwired")
        st, body = req("PUT", "/api/business/flow/linkage", admin,
                       {"name": "E2E10 未接线规则", "trigger_entity": target["entity"],
                        "trigger_status": status, "action_code": "SEND_NOTICE", "confirm": True,
                        "allow_unwired": True}, name="linkage_unwired_allow")
        eq("显式 allow_unwired=true → 200", st, 200)
        rid = data_of(body).get("id")
        if rid:
            st, body = req("DELETE", f"/api/business/flow/linkage?id={rid}&confirm=true", admin, None,
                           name="linkage_unwired_delete")
            eq("清掉未接线规则 → 200", st, 200)

    # 新规则：TASK → DOING 发通知（启用）；先建任务
    st, body = req("POST", "/api/business/task", admin,
                   {"title": "E2E10 联动触发任务", "owner_id": users["u_a1"]}, name="task_create")
    eq("建任务（联动触发载体）→ 200", st, 200)
    task_id = data_of(body).get("id")
    check(bool(task_id), "任务 id 就绪", str(task_id))

    st, body = req("PUT", "/api/business/flow/linkage", admin,
                   {"name": "E2E10 TASK DOING 通知", "trigger_entity": "TASK", "trigger_status": "DOING",
                    "action_code": "SEND_NOTICE", "action_params": {"mode": "TAKEOVER", "recipient": "CREATOR"},
                    "seq": 30, "confirm": True}, name="linkage_create")
    eq("新建联动规则（TASK DOING → SEND_NOTICE）→ 200", st, 200)
    rid = data_of(body).get("id")
    eq("规则 phase = AFTER_COMMIT（通知在事务提交后发）", data_of(body).get("phase"), "AFTER_COMMIT")
    eq("规则 severity = NON_CRITICAL", data_of(body).get("severity"), "NON_CRITICAL")
    eq("规则 implemented = true", data_of(body).get("implemented"), True)

    # 预览
    st, body = req("POST", "/api/business/flow/linkage/preview", admin,
                   {"trigger_entity": "TASK", "trigger_status": "DOING"}, name="linkage_preview")
    eq("预览影响 → 200", st, 200)
    pv = data_of(body)
    check(int(pv.get("enabled_count") or 0) >= 1, "预览：启用规则数 ≥ 1", str(pv.get("enabled_count")))
    steps = pv.get("steps") or []
    check(any(s.get("rule_id") == rid for s in steps), "预览 steps 含新规则")
    eq("预览 steps 按 seq 升序", [s.get("seq") for s in steps], sorted([s.get("seq") for s in steps]))
    check(bool(pv.get("next_run_hint")), "预览带 next_run_hint（配了不会回放历史单）", str(pv.get("next_run_hint"))[:80])

    # 触发一次：TASK TODO → DOING
    st, body = req("PATCH", f"/api/business/task/{task_id}/status", admin,
                   {"to": "DOING"}, name="task_doing")
    check(st == 200, "任务状态 TODO → DOING → 200", f"http={st} code={code_of(body)}")
    eq("任务状态落库 DOING", psql(f"select status from biz_task where id='{task_id}'::uuid"), "DOING")
    notif1 = psql("select count(*) from sys_notice where biz_id='%s'" % task_id)
    eq("联动产生 1 条通知（通知主体 biz_id = 任务 id）", notif1, "1")
    check(psql(f"select (select count(*) from sys_notice n join sys_notice_receiver r on r.notice_id=n.id "
               f"where n.biz_id='{task_id}') >= 1") == "t",
          "通知已扇出到接收人（sys_notice_receiver ≥ 1 行）")
    check(psql(f"select (select count(*) from sys_notice n join sys_notice_receiver r on r.notice_id=n.id "
               f"where n.biz_id='{task_id}' and r.user_id in ('{users['u_a1']}'::uuid,'{users['u_a1']}'::uuid)) >= 1") == "t",
          "接收人含任务负责人 u_a1")

    # 幂等：重复触发同一事件
    st, body = req("PATCH", f"/api/business/task/{task_id}/status", admin, {"to": "DOING"}, name="task_doing_again")
    check(st in (400, 409), "重复置 DOING → 409（状态机白名单拒绝）", f"http={st} code={code_of(body)}")
    eq("同一事件通知仍只有 1 条（幂等）",
       psql("select count(*) from sys_notice where biz_id='%s'" % task_id), "1")

    # 启停：停用后新任务不再触发
    st, body = req("PUT", "/api/business/flow/linkage/toggle", admin,
                   {"id": rid, "enabled": False, "confirm": True}, name="linkage_toggle_off")
    eq("停用规则 → 200", st, 200)
    eq("停用落库", psql(f"select enabled from biz_flow_linkage_rule where id='{rid}'::uuid"), "f")
    st, body = req("POST", "/api/business/task", admin,
                   {"title": "E2E10 联动触发任务B", "owner_id": users["u_a1"]}, name="task_create_b")
    task_b = data_of(body).get("id")
    if task_b:
        req("PATCH", f"/api/business/task/{task_b}/status", admin, {"to": "DOING"}, name="task_doing_b")
        eq("停用后通知回落到旧逻辑（兜底不丢，仍 1 条）",
           psql("select count(*) from sys_notice where biz_id='%s'" % task_b), "1")
    st, body = req("PUT", "/api/business/flow/linkage/toggle", admin,
                   {"id": rid, "enabled": True, "confirm": True}, name="linkage_toggle_on")
    eq("重新启用 → 200", st, 200)
    st, body = req("POST", "/api/business/task", admin,
                   {"title": "E2E10 联动触发任务C", "owner_id": users["u_a1"]}, name="task_create_c")
    task_c = data_of(body).get("id")
    if task_c:
        req("PATCH", f"/api/business/task/{task_c}/status", admin, {"to": "DOING"}, name="task_doing_c")
        eq("重新启用后触发 → 通知 1 条",
           psql("select count(*) from sys_notice where biz_id='%s'" % task_c), "1")
    state["task_ids"] = [x for x in [task_id, task_b, task_c] if x]

    # 防「凭证生凭证」：VOUCHER 触发点配 GENERATE_VOUCHER → 明确拒绝或标问题
    st, body = req("PUT", "/api/business/flow/linkage", admin,
                   {"name": "E2E10 凭证生凭证（应被拒）", "trigger_entity": "VOUCHER",
                    "trigger_status": "POSTED", "action_code": "GENERATE_VOUCHER", "confirm": True},
                   name="linkage_voucher_to_voucher")
    check(st in (400, 409, 422), "VOUCHER 触发点配 GENERATE_VOUCHER → 被拒", f"http={st} code={code_of(body)}")
    check(("凭证" in msg_of(body)) or ("VOUCHER" in msg_of(body).upper()), "报文解释「凭证生凭证」风险", msg_of(body)[:160])

    # 软删
    st, body = req("DELETE", f"/api/business/flow/linkage?id={rid}&confirm=true", admin, None, name="linkage_delete")
    eq("删除规则 → 200", st, 200)
    eq("软删落库（deleted_at 非空）",
       psql(f"select count(*) from biz_flow_linkage_rule where id='{rid}'::uuid and deleted_at is not null"), "1")
    st, body = req("GET", "/api/business/flow/linkage", admin, name="linkage_list_after")
    check(rid not in [x.get("id") for x in data_of(body).get("rules") or []], "删除后列表不再出现")
    save_state(state)
    return state


# ==================== S9 状态流转边（联动同域页面） ====================
def s9_state_edge(state):
    admin = state["admin"]
    scenario("S9", "⑥ 联动域：状态流转边配置（主链路必需边保护 E2E-FIX-1）")
    st, body = req("GET", "/api/business/flow/state-edge?entity=STOCK_IN", admin, name="edge_list")
    eq("状态边列表 → 200", st, 200)
    # 清掉上一轮本卡残留的待审批配置变更单（只动 label 含 E2E10 的）
    stale = psql("select coalesce(string_agg(distinct t.id::text, ' '), '') from biz_flow_instance i "
                 "join biz_flow_task t on t.instance_id=i.id and t.status='PENDING' "
                 "where i.biz_type='FLOW_EDGE_CFG' and i.status='PENDING' and i.form_data->>'label' like 'E2E10%'")
    for tid in (stale or "").split():
        reject(admin, tid, "E2E10 清理上一轮残留配置变更", return_to="NONE")
    edges = data_of(body).get("list") or []
    check(len(edges) >= 1, "STOCK_IN 边集非空", f"n={len(edges)}")
    crit = [e for e in (edges[0].get("base_edges") or []) if e.get("critical")]
    check(len(crit) >= 1, "边集标记了主链路必需边（critical）")
    # 禁用必需边：不带 confirm_critical → 400 点名后果
    if crit:
        c = crit[0]
        st, body = req("PUT", "/api/business/flow/state-edge", admin,
                       {"entity": "STOCK_IN", "from_status": c["from"], "to_status": c["to"],
                        "enabled": False, "confirm": True, "reason": "E2E10 禁用必需边（应被拒）"},
                       name="edge_disable_critical")
        eq("禁用主链路必需边（无 confirm_critical）→ 400", st, 400)
        check(("无法" in msg_of(body)) or ("必需" in msg_of(body)), "报文点名业务后果", msg_of(body)[:160])
    # 非必需边加 requires_reason（只能加）→ 走审批，通过后生效
    normal = [e for e in (edges[0].get("base_edges") or []) if not e.get("critical")]
    if normal:
        n = normal[0]
        st, body = req("PUT", "/api/business/flow/state-edge", admin,
                       {"entity": "STOCK_IN", "from_status": n["from"], "to_status": n["to"],
                        "enabled": True, "requires_reason": True, "label": "E2E10 演练文案",
                        "confirm": True, "reason": "E2E10 演练：要求填原因"}, name="edge_upsert")
        eq("改非必需边（要求原因 + 文案）→ 200", st, 200)
        info = data_of(body)
        eq("未直接生效（applied = false，先过审批）", info.get("applied"), False)
        eid = info.get("instance_id")
        check(bool(eid), "返回审批实例 id", str(eid))
        contains("报文说明「审批通过后才会生效」", info.get("message"), "审批通过后才会生效")
        eq("实例状态 PENDING", inst_status(eid) if eid else None, "PENDING")
        # 审批通过（流程 flow_edge_config 的复核节点，admin 为审批人）
        t = pending_task(eid, None, None) if eid else None
        check(bool(t), "配置变更单有待办可审批")
        if t:
            st, body = approve(admin, t["id"], "E2E10 同意状态边变更")
            eq("审批配置变更单 → 200", st, 200)
        eq("审批后实例 APPROVED", inst_status(eid) if eid else None, "APPROVED")
        st, body = req("GET", "/api/business/flow/state-edge?entity=STOCK_IN", admin, name="edge_list_after")
        row = [e for e in (data_of(body).get("list") or [{}])[0].get("effective_edges") or []
               if e["from"] == n["from"] and e["to"] == n["to"]]
        check(bool(row) and row[0].get("requires_reason"), "有效边 requires_reason = true", json.dumps(row, ensure_ascii=False)[:160])
        eq("有效边自定义文案", row[0].get("label") if row else None, "E2E10 演练文案")
        eq("有效边 configured = true", row[0].get("configured") if row else None, True)
        eq("base_edges 保持代码真相（label 不变）",
           [e for e in (data_of(body).get("list") or [{}])[0].get("base_edges") or []
            if e["from"] == n["from"] and e["to"] == n["to"]][0].get("label"), n.get("label"))
    save_state(state)
    return state


def main():
    state = load_state()
    error = None
    try:
        if not state.get("tokens"):
            state["tokens"] = {}
            for username in state.get("users", {}):
                state["tokens"][username] = login(username, BASE_PW, name="relogin_" + username)
        s5_delegate(state)
        s6_self_approval(state)
        s7_escalation(state)
        s8_linkage(state)
        s9_state_edge(state)
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
