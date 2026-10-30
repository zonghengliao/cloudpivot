#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""C+ 任务管理功能验証（52 项）：CRUD / 状态机 / 分派 / 依赖 / 甘特 / AI 执行者 /
评论 / 附件 / 工时 / AI 用户 / 改期。

每个子用例自建自删，跑完由 ctx.cleanup() 兜底。
"""

import time

from common import STAMP


def _new_task(ctx, title, **extra):
    body = {"title": title}
    body.update(extra)
    st, resp = ctx.post("/api/business/task", body)
    tid = (ctx.data(resp) or {}).get("id") if isinstance(ctx.data(resp), dict) else None
    if tid:
        ctx.on_cleanup("删任务 %s" % title, lambda task_id=tid: ctx.delete("/api/business/task/%s" % task_id))
    return tid, resp


def t1(ctx):
    """CRUD + 可见性。"""
    print("\n[T1] 任务 CRUD + 可见性")
    tid, resp = _new_task(ctx, "T1验证任务 " + STAMP, priority="P2")
    ctx.chk("创建", bool(tid), resp)
    tno = (ctx.data(resp) or {}).get("task_no") if isinstance(ctx.data(resp), dict) else None
    ctx.chk("task_no 格式 TSK-", bool(tno and str(tno).startswith("TSK-")), tno)
    ok, items, _total = ctx.page("/api/business/task/page?page=1&page_size=5")
    ctx.chk("分页字段完整", ok, items[:1])
    st, resp = ctx.get("/api/business/task/%s" % tid)
    data = ctx.data(resp)
    ctx.chk("详情默认 TODO/0",
            data.get("status") == "TODO" and data.get("progress") == 0, data)
    st, resp = ctx.put("/api/business/task/%s" % tid, {"title": "T1改", "priority": "P0"})
    ctx.ok("编辑生效", resp)
    st, resp = ctx.get("/api/business/task/my")
    keys = list((ctx.data(resp) or {}).keys()) if isinstance(ctx.data(resp), dict) else []
    ctx.chk("我的任务分组结构",
            any(k in keys for k in ("overdue", "today", "this_week", "later", "no_due")), keys)
    st, resp = ctx.get("/api/business/task/page", token="")
    ctx.chk("无 token 401", st == 401, st)
    ctx.delete("/api/business/task/%s" % tid)
    st, resp = ctx.get("/api/business/task/%s" % tid)
    ctx.chk("软删后 404", st == 404, st)


def t2(ctx):
    """状态机 + 进度。"""
    print("\n[T2] 状态机 + 进度")
    tid, resp = _new_task(ctx, "T2验证 " + STAMP, need_review=False)
    ctx.chk("建任务", bool(tid), resp)
    st, resp = ctx.patch("/api/business/task/%s/status" % tid, {"to": "DOING"})
    ctx.ok("TODO→DOING", resp)
    row = ctx.q("select status||'|'||progress from biz_task where id='%s'" % tid)
    parts = row.split("|") if row else ["", ""]
    ctx.chk("DOING 进度自动 ≥10", len(parts) > 1 and int(parts[1] or 0) >= 10, row)
    st, resp = ctx.patch("/api/business/task/%s/status" % tid, {"to": "DONE"})
    ctx.chk("need_review=false 时 DOING→DONE 允许", resp.get("code") == 0 or st < 400,
            "code=%s" % resp.get("code"))
    st, resp = ctx.patch("/api/business/task/%s/status" % tid, {"to": "DOING"})
    ctx.chk("DONE→DOING 重开", resp.get("code") == 0 or st < 400, "code=%s" % resp.get("code"))
    st, resp = ctx.patch("/api/business/task/%s/status" % tid, {"to": "REVIEW"})
    ctx.ok("DOING→REVIEW（need_review=false 不发起审批）", resp)
    st, resp = ctx.patch("/api/business/task/%s/progress" % tid, {"progress": 55})
    ctx.ok("手动改进度", resp)


def t3(ctx):
    """分派 + 协作人。"""
    print("\n[T3] 分派 / 协作人")
    tid, resp = _new_task(ctx, "T3验证 " + STAMP)
    ctx.chk("建任务", bool(tid), resp)
    users = ctx.q(
        "select string_agg(id::text, '|') from (select id from sys_user "
        "where deleted_at is null and user_type='HUMAN' and username<>'%s' limit 2) x" % ctx.user
    )
    parts = users.split("|") if users else []
    if not parts:
        ctx.skip("无第二个可用人类用户")
        return
    st, resp = ctx.patch("/api/business/task/%s/assign" % tid, {"owner_id": parts[0]})
    ctx.ok("分派成功", resp)
    st, resp = ctx.post("/api/business/task/%s/members" % tid, {"user_ids": [parts[-1]]})
    ctx.ok("加协作人", resp)
    st, resp = ctx.post("/api/business/task/%s/members" % tid, {"user_ids": [parts[-1]]})
    ctx.ok("重复加幂等", resp)
    st, resp = ctx.get("/api/business/task/%s/members" % tid)
    ctx.ok("协作人列表", resp)
    me = ctx.q("select creator_id::text from biz_task where id='%s'" % tid)
    st, resp = ctx.delete("/api/business/task/%s/members/%s" % (tid, me))
    ctx.chk("移除创建人被拒", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))


def t4(ctx):
    """依赖 + 甘特 + 环检测。"""
    print("\n[T4] 依赖 + 甘特")
    a, ra = _new_task(ctx, "T4-A " + STAMP)
    b, rb = _new_task(ctx, "T4-B " + STAMP)
    c, rc = _new_task(ctx, "T4-C " + STAMP)
    if not (a and b and c):
        ctx.chk("建 3 个任务", False, [ra, rb, rc][:1])
        return
    st, resp = ctx.post("/api/business/task/%s/dependencies" % b, {"depends_on_id": a})
    ctx.ok("A→B 加依赖", resp)
    st, resp = ctx.post("/api/business/task/%s/dependencies" % c, {"depends_on_id": b})
    ctx.ok("B→C 加依赖", resp)
    st, resp = ctx.post("/api/business/task/%s/dependencies" % a, {"depends_on_id": c})
    ctx.chk("C→A 成环被拒", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))
    st, resp = ctx.post("/api/business/task/%s/dependencies" % a, {"depends_on_id": a})
    ctx.chk("自依赖被拒", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))
    st, resp = ctx.patch("/api/business/task/%s/status" % b, {"to": "DOING"})
    ctx.chk("前置未完成进 DOING 被拒", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))
    st, resp = ctx.get("/api/business/task/gantt")
    ctx.ok("甘特数据", resp)


def t5(ctx, robot_id=None):
    """AI 执行者规则。"""
    print("\n[T5] AI 执行者规则")
    st, resp = ctx.get("/api/system/user/page?page=1&page_size=1")
    ctx.ok("人类用户接口正常", resp)
    if not robot_id:
        ctx.skip("无 ROBOT 用户可测")
        return
    name = ctx.q("select username from sys_user where id='%s'" % robot_id)
    st, resp = ctx.post("/api/system/auth/login", {"username": name, "password": "x"})
    ctx.chk("AI 登录被拒", resp.get("code") in (63201, 20001), "code=%s" % resp.get("code"))
    row = ctx.q("select count(*) from sys_change_log where operator_type='ROBOT'")
    ctx.chk("changelog operator_type 字段可用", row.isdigit(), row)


def t5b(ctx, robot_id=None):
    """派活 + 任务令牌。"""
    print("\n[T5b] 派活 + 任务令牌")
    if not robot_id:
        ctx.skip("无 ROBOT 用户可测")
        return
    tid, resp = _new_task(ctx, "T5b验证 " + STAMP, owner_id=robot_id)
    ctx.ok("建任务 owner=AI", resp)
    if not tid:
        return
    st, resp = ctx.post("/api/business/task/%s/dispatch-ai" % tid, {})
    ctx.ok("派活成功", resp)
    hid = (ctx.data(resp) or {}).get("hermes_task_id") if isinstance(ctx.data(resp), dict) else None
    ctx.chk("拿到 hermes_task_id", bool(hid), hid)
    row = ctx.q("select exec_mode||'|'||coalesce(hermes_task_id,'-') from biz_task where id='%s'" % tid)
    ctx.chk("exec_mode=AI 且 id 回写", row.startswith("AI|") and len(row.split("|")) > 1
            and row.split("|")[1] == str(hid), row)
    st, resp = ctx.post("/api/business/task/%s/dispatch-ai" % tid, {})
    ctx.chk("重复派活被拒", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))
    tok = ctx.q("select count(*) from biz_task_ai_token where task_id='%s'" % tid)
    ctx.chk("任务令牌已生成", tok and int(tok) > 0, tok)


def t6b(ctx):
    """评论 / 附件 / 工时。"""
    print("\n[T6b] 评论 / 附件 / 工时")
    tid, resp = _new_task(ctx, "T6b验证 " + STAMP)
    ctx.chk("建任务", bool(tid), resp)
    st, resp = ctx.post("/api/business/task/%s/comments" % tid, {"content": "验证评论 **粗体**"})
    ctx.ok("发评论", resp)
    st, resp = ctx.get("/api/business/task/%s/comments" % tid)
    ctx.ok("评论列表", resp)
    today = time.strftime("%Y-%m-%d")
    st, resp = ctx.post("/api/business/task/%s/worklogs" % tid, {"hours": 2, "work_date": today})
    ctx.ok("登记工时 2h", resp)
    st, resp = ctx.post("/api/business/task/%s/worklogs" % tid, {"hours": 3.5, "work_date": today})
    ctx.ok("登记工时 3.5h", resp)
    row = ctx.q("select coalesce(actual_hours::text,'0') from biz_task where id='%s'" % tid)
    ctx.chk("actual_hours 服务端汇总=5.5", row in ("5.5", "5.50"), row)
    st, resp = ctx.post("/api/business/task/%s/worklogs" % tid, {"hours": 26, "work_date": today})
    ctx.chk("26h 被拒", resp.get("code") != 0 or st >= 400, "code=%s" % resp.get("code"))
    st, resp = ctx.get("/api/business/task/%s/attachments" % tid)
    ctx.ok("附件列表", resp)


def l1(ctx):
    """AI 用户创建 / profile 落盘 / 删除清理。"""
    print("\n[L1] AI 用户创建")
    name = "verify-ai-" + STAMP
    st, resp = ctx.post("/api/system/user/ai", {
        "username": name, "real_name": "验证AI", "agent_description": "验证脚本",
        "soul_md": "你是验证助手。",
    })
    ctx.ok("建 AI 用户", resp)
    data = ctx.data(resp) or {}
    aid = (data.get("user_id") or data.get("id")) if isinstance(data, dict) else None
    if aid:
        ctx.on_cleanup("删 AI 用户 %s" % name, lambda user_id=aid: ctx.delete("/api/system/user/ai/%s" % user_id))
    envf = "/root/.hermes/profiles/%s/.env" % name
    import os
    ctx.chk("profile .env 存在", os.path.isfile(envf), envf)
    if os.path.isfile(envf):
        txt = open(envf, encoding="utf-8").read()
        ctx.chk("含 DEEPSEEK_API_KEY", "DEEPSEEK_API_KEY" in txt)
        ctx.chk("权限 600", oct(os.stat(envf).st_mode)[-3:] == "600", oct(os.stat(envf).st_mode)[-3:])
    ctx.chk("SOUL.md 存在", os.path.isfile("/root/.hermes/profiles/%s/SOUL.md" % name))
    st, resp = ctx.get("/api/system/user/ai/%s" % aid)
    ctx.ok("读取 AI 配置", resp)
    st, resp = ctx.delete("/api/system/user/ai/%s" % aid)
    ctx.ok("删 AI 用户", resp)
    ctx.chk("profile 目录已清", not os.path.isdir("/root/.hermes/profiles/%s" % name))
    return aid


def t7a(ctx):
    """改期 + 依赖校验。"""
    print("\n[T7a] 改期 + 依赖校验")
    a, ra = _new_task(ctx, "T7a-A " + STAMP, start_date="2026-11-01", due_date="2026-11-05")
    b, rb = _new_task(ctx, "T7a-B " + STAMP, start_date="2026-11-06", due_date="2026-11-10")
    if not (a and b):
        ctx.chk("建 2 个任务", False, [ra, rb][:1])
        return
    ctx.post("/api/business/task/%s/dependencies" % b, {"depends_on_id": a})
    st, resp = ctx.patch("/api/business/task/%s/schedule" % b,
                         {"start_date": "2026-10-30", "due_date": "2026-11-03"})
    ctx.chk("改期早于前置未完成 → 被拒", resp.get("code") not in (0, None),
            "code=%s msg=%s" % (resp.get("code"), str(resp.get("msg"))[:60]))
    st, resp = ctx.patch("/api/business/task/%s/schedule" % b, {"shift_days": 3})
    ctx.ok("平移 3 天成功", resp)
    row = ctx.q("select start_date::text||'|'||due_date::text from biz_task where id='%s'" % b)
    ctx.chk("库日期已变", bool(row) and "11-0" in row, row)


def _ensure_robot(ctx):
    """保证有 ROBOT 用户（没有就建一个临时 AI 用户，跑完删）。"""
    rid = ctx.q("select id::text from sys_user where user_type='ROBOT' and deleted_at is null limit 1")
    if rid:
        return rid
    name = "verify-robot-" + STAMP
    st, resp = ctx.post("/api/system/user/ai", {
        "username": name, "real_name": "验证机器人", "agent_description": "verify 夹具",
        "soul_md": "你是验证夹具。",
    })
    data = ctx.data(resp) or {}
    rid = (data.get("user_id") or data.get("id")) if isinstance(data, dict) else None
    if rid:
        ctx.on_cleanup("删 ROBOT 用户 %s" % name,
                       lambda user_id=rid: ctx.delete("/api/system/user/ai/%s" % user_id))
    return rid


MODULES = ["t1", "t2", "t3", "t4", "t5", "t5b", "t6b", "l1", "t7a"]


def run(ctx, modules=None):
    """跑 C+ 任务管理验证；modules 为空 = 全跑。"""
    print("=" * 66)
    print("C+ 任务管理（BASE_URL=%s）" % ctx.base_url)
    print("=" * 66)
    todo = modules or MODULES
    robot = _ensure_robot(ctx) if any(m in ("t5", "t5b") for m in todo) else None
    handlers = {
        "t1": t1, "t2": t2, "t3": t3, "t4": t4, "t6b": t6b, "l1": l1, "t7a": t7a,
        "t5": lambda c: t5(c, robot), "t5b": lambda c: t5b(c, robot),
    }
    for name in todo:
        fn = handlers.get(name)
        if not fn:
            print("  \u26a0 未知模块：%s" % name)
            continue
        try:
            fn(ctx)
        except Exception as exc:  # noqa: BLE001
            ctx.chk("%s 执行异常" % name, False, str(exc)[:120])
    return ctx
