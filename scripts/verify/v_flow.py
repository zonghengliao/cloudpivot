#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""批次 D 审批流读写验证：建流程 → 存整图 → 发布 → 发起 → 审批 / 驳回 / 会签。

字段契约全部走 common 的构造器（`graph()` / `node()` / `edge()` / `assignee()`）：
  - `nodes[].key`（不是 `node_key`）
  - `edges[].key` **必填**，端点用 `from` / `to`
  - 审批人 `assignees: [{type, ref_id}]`

# 为什么要有「临时审批人」

节点默认 `self_approval=SKIP`：发起人自己就是审批人时，待办直接落 `SKIPPED` 并自动前进，
实例一步到 APPROVED、根本不会留下 PENDING 待办 —— 这样测不到「待办 → 同意/驳回」。
所以这里现建两个临时审批人（授超管角色拿 `business:flow:approve` 权限），
用它俩的 token 去审批，发起人仍是 TEST_USER。跑完把临时用户删掉。
"""

import time

from common import STAMP, assignee, draft_version, graph, linear_graph

APPROVER_PASS = "Vrf@123456"


def _login_as(ctx, username, password):
    """以指定用户登录，返回 token（不动 ctx.token）。"""
    st, resp = ctx.req("POST", "/api/system/auth/login",
                       {"username": username, "password": password}, token="")
    data = ctx.data(resp) if isinstance(ctx.data(resp), dict) else {}
    return data.get("access_token")


def make_approver(ctx, tag):
    """建临时审批人：建用户 → 授超管角色（拿 flow:approve 权限）→ 登录拿 token。"""
    name = "vrf_%s_%s" % (tag, STAMP)
    st, resp = ctx.post("/api/system/user", {
        "username": name, "password": APPROVER_PASS,
        "real_name": "验证审批人 %s" % tag, "status": 1,
    })
    uid = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if not uid:
        print("     \u26a0 建临时审批人失败：%s" % str(resp)[:120])
        return None
    ctx.on_cleanup("删临时审批人 %s" % name,
                   lambda user_id=uid: ctx.delete("/api/system/user/%s" % user_id))
    super_rid = ctx.q("select id::text from sys_role where role_code='SUPER_ADMIN' and deleted_at is null limit 1")
    if super_rid:
        ctx.put("/api/system/user/%s/roles" % uid, {"role_ids": [super_rid]})
    token = _login_as(ctx, name, APPROVER_PASS)
    return {"id": uid, "name": name, "token": token}


def _publish_flow(ctx, code, name, g, biz_type="OTHER"):
    """建定义 → 存图 → 校验 → 发布，返回 did。"""
    st, resp = ctx.post("/api/business/flow/def", {
        "flow_code": code, "flow_name": name, "biz_type": biz_type,
        "description": "verify 自动化流程",
    })
    did = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if not did:
        ctx.chk("建流程定义 %s" % code, False, resp)
        return None
    ctx.on_cleanup("删流程定义 %s" % code,
                   lambda def_id=did: ctx.delete("/api/business/flow/def/%s" % def_id))
    vid = (ctx.data(resp) or {}).get("version_id") or draft_version(ctx, did)
    if not vid:
        ctx.chk("拿到草稿版本 %s" % code, False, "version_id 为空")
        return did
    st, resp = ctx.put("/api/business/flow/version/%s/graph" % vid, g)
    ctx.ok("保存整图 %s" % code, resp)
    st, resp = ctx.post("/api/business/flow/version/%s/validate" % vid, {})
    ctx.ok("图校验通过 %s" % code, resp)
    st, resp = ctx.post("/api/business/flow/def/%s/publish" % did, {})
    ctx.ok("发布 %s" % code, resp)
    return did


def _start_instance(ctx, code, title):
    st, resp = ctx.post("/api/business/flow/instance", {
        "flow_code": code, "title": title, "form_data": {"reason": "verify"},
    })
    data = ctx.data(resp) or {}
    iid = (data.get("id") or data.get("instance_id")) if isinstance(data, dict) else None
    if iid:
        def _cancel(instance_id=iid):
            st2, r2 = ctx.post("/api/business/flow/instance/%s/withdraw" % instance_id, {})
            # 撤回只能撤 PENDING；已终态/退回中的会拒。为了不留下「在跑审批单」
            # （否则流程定义 409 删不掉），统一把本次自建实例收到 CANCELLED。
            # 注意库约束 ck_biz_flow_instance_end：(status='PENDING') = (ended_at IS NULL)，
            # 所以终态必须同时写 ended_at，否则整条 UPDATE 被拒。
            ctx.q("update biz_flow_instance set status='CANCELLED', ended_at=coalesce(ended_at, now()) "
                  "where id='%s' and status not in ('APPROVED','REJECTED')" % instance_id)
            ctx.q("update biz_flow_task set status='CANCELLED' "
                  "where instance_id='%s' and status='PENDING'" % instance_id)
            return 200, {"code": 0}
        ctx.on_cleanup("撤销实例 %s" % title, _cancel)
    return iid, resp


def scenario_single(ctx, approver):
    """场景1：单签 建→存图→发布→发起→同意→APPROVED。"""
    print("\n【场景1】单签流程全链路")
    code = "vrf_single_" + STAMP
    did = _publish_flow(ctx, code, "验证单签", linear_graph(assignees=[assignee(approver["id"])]))
    if not did:
        return
    ctx.chk("状态变 PUBLISHED",
            ctx.q("select status from biz_flow_def where id='%s'" % did) == "PUBLISHED",
            ctx.q("select status from biz_flow_def where id='%s'" % did))
    iid, resp = _start_instance(ctx, code, "验证单签实例")
    ctx.chk("发起实例", bool(iid), resp)
    if not iid:
        return
    print("     instance_id =", iid)
    ctx.chk("实例 PENDING",
            ctx.q("select status from biz_flow_instance where id='%s'" % iid) == "PENDING",
            ctx.q("select status from biz_flow_instance where id='%s'" % iid))
    ft = ctx.q("select id::text from biz_flow_task where instance_id='%s' and status='PENDING' limit 1" % iid)
    ctx.chk("生成待办", bool(ft), ft)
    if not ft:
        return
    st, resp = ctx.post("/api/business/flow/task/%s/approve" % ft, {"comment": "单签通过"},
                        token=approver["token"])
    ctx.ok("审批人同意", resp)
    time.sleep(1)
    ctx.chk("实例 → APPROVED",
            ctx.q("select status from biz_flow_instance where id='%s'" % iid) == "APPROVED",
            ctx.q("select status from biz_flow_instance where id='%s'" % iid))
    # 审批留痕在待办轨迹里（biz_flow_task），流程实例不写 sys_change_log
    row = ctx.q("select status||'|'||coalesce(comment,'-')||'|'||"
                "case when completed_at is null then 'null' else 'done' end "
                "from biz_flow_task where id='%s'" % ft)
    ctx.chk("待办轨迹记录结论（APPROVED + 意见 + 完成时间）",
            row.startswith("APPROVED|") and row.endswith("|done"), row)


def scenario_reject(ctx, approver):
    """场景2：驳回 → 实例 REJECTED。"""
    print("\n【场景2】驳回")
    code = "vrf_reject_" + STAMP
    did = _publish_flow(ctx, code, "验证驳回", linear_graph(assignees=[assignee(approver["id"])]))
    if not did:
        return
    iid, resp = _start_instance(ctx, code, "验证驳回实例")
    ctx.chk("驳回实例创建", bool(iid), resp)
    if not iid:
        return
    ft = ctx.q("select id::text from biz_flow_task where instance_id='%s' and status='PENDING' limit 1" % iid)
    ctx.chk("驳回前有待办", bool(ft), ft)
    if not ft:
        return
    st, resp = ctx.post("/api/business/flow/task/%s/reject" % ft,
                        {"comment": "材料不全，请补充后重提"}, token=approver["token"])
    ctx.ok("驳回成功", resp)
    time.sleep(1)
    node_row = ctx.q("select status from biz_flow_task where id='%s'" % ft)
    ctx.chk("审批节点标记 REJECTED", node_row == "REJECTED", node_row)
    # 默认「退回发起人」：实例不终态，而同实例 round+1 生成 START 节点的退回待办
    back = ctx.q("select round||'|'||node_key from biz_flow_task "
                 "where instance_id='%s' and round > 1 and status='PENDING' limit 1" % iid)
    ctx.chk("退回发起人（round+1 的 START 待办）", back.startswith("2|"), back)
    ctx.chk("实例仍 PENDING（退回而非终态）",
            ctx.q("select status from biz_flow_instance where id='%s'" % iid) == "PENDING",
            ctx.q("select status from biz_flow_instance where id='%s'" % iid))


def scenario_countersign(ctx, approvers):
    """场景3：会签 ALL（2 名非发起人审批人）。"""
    print("\n【场景3】会签 ALL（%d 人）" % len(approvers))
    if len(approvers) < 2:
        ctx.skip("临时审批人不足 2 个，会签无法验证")
        return
    code = "vrf_cs_" + STAMP
    g = linear_graph(mid_type="COUNTERSIGN", mid_name="会签审批",
                     assignees=[assignee(a["id"]) for a in approvers],
                     approval_mode="ALL", reject_veto=True)
    did = _publish_flow(ctx, code, "验证会签", g)
    if not did:
        return
    iid, resp = _start_instance(ctx, code, "验证会签实例")
    ctx.chk("会签实例创建", bool(iid), resp)
    if not iid:
        return
    n = ctx.q("select count(*) from biz_flow_task where instance_id='%s' and status='PENDING'" % iid)
    ctx.chk("会签生成 %d 条待办" % len(approvers), n and int(n) == len(approvers),
            "实际=%s 期望=%d" % (n, len(approvers)))
    ft = ctx.q("select id::text from biz_flow_task where instance_id='%s' and status='PENDING' "
               "and assignee_id='%s' limit 1" % (iid, approvers[0]["id"]))
    if not ft:
        ctx.skip("拿不到第一个审批人的待办")
        return
    st, resp = ctx.post("/api/business/flow/task/%s/approve" % ft, {"comment": "1/2 同意"},
                        token=approvers[0]["token"])
    ctx.ok("第 1 人同意", resp)
    time.sleep(1)
    row = ctx.q("select status from biz_flow_instance where id='%s'" % iid)
    ctx.chk("ALL 模式部分同意仍 PENDING", row == "PENDING", row)
    # 越权：审批人 B 去批审批人 A 的待办（已被批掉，仍应被拒）
    st, resp = ctx.post("/api/business/flow/task/%s/approve" % ft, {"comment": "越权重批"},
                        token=approvers[1]["token"])
    ctx.chk("非本人待办被拒（65028/65024）", resp.get("code") != 0 or st >= 400,
            "code=%s msg=%s" % (resp.get("code"), str(resp.get("msg"))[:50]))


def scenario_invalid(ctx):
    """场景4：非法图（真·空图发布被拒）。"""
    print("\n【场景4】非法操作")
    code = "vrf_empty_" + STAMP
    st, resp = ctx.post("/api/business/flow/def", {
        "flow_code": code, "flow_name": "验证空图", "biz_type": "OTHER",
    })
    did = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if not did:
        ctx.chk("建空图流程", False, resp)
        return
    ctx.on_cleanup("删空图流程 %s" % code,
                   lambda def_id=did: ctx.delete("/api/business/flow/def/%s" % def_id))
    vid = (ctx.data(resp) or {}).get("version_id") or draft_version(ctx, did)
    if not vid:
        ctx.skip("拿不到草稿版本")
        return
    ctx.put("/api/business/flow/version/%s/graph" % vid, graph([], []))
    st, resp = ctx.post("/api/business/flow/def/%s/publish" % did, {})
    ctx.chk("真·空图发布被拒（校验兜底）", resp.get("code") not in (0, None) or st >= 400,
            "code=%s msg=%s" % (resp.get("code"), str(resp.get("msg"))[:60]))


def run(ctx):
    """跑批次 D 读写验证。"""
    print("=" * 66)
    print("批次 D 审批流读写（BASE_URL=%s）" % ctx.base_url)
    print("=" * 66)
    approvers = [a for a in (make_approver(ctx, "a1"), make_approver(ctx, "a2")) if a]
    if not approvers:
        ctx.chk("准备临时审批人", False, "建不出临时用户，读写场景无法验证")
        return ctx
    print("     临时审批人：%s" % ", ".join(a["name"] for a in approvers))
    for step in (
        lambda: scenario_single(ctx, approvers[0]),
        lambda: scenario_reject(ctx, approvers[0]),
        lambda: scenario_countersign(ctx, approvers),
        lambda: scenario_invalid(ctx),
    ):
        try:
            step()
        except Exception as exc:  # noqa: BLE001
            ctx.chk("场景执行异常", False, str(exc)[:120])
    return ctx
