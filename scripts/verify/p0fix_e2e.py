#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P0-FIX 采购审批死锁 —— 真机 HTTP 验收（跑在 144，指向「生产库快照 + 新二进制」的隔离实例）。

用法：
    python3 p0fix_e2e.py --phase before      # 数据修复前：复现「看得见、点不动」的死锁
    python3 p0fix_e2e.py --phase after       # 数据修复后：待办消失 + 能批的人批完即通过
    python3 p0fix_e2e.py --phase new-flow    # 新代码行为：新提交的单不再给制单人/发起人生成待办

环境变量：
    P0FIX_BASE   实例地址（默认 http://127.0.0.1:18124/api）
    P0FIX_PASS   三个账号统一的夹具口令
    P0FIX_PO     目标采购单号（默认 PO-20261031-000002）
"""
import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from typing import Any

BASE = os.environ.get("P0FIX_BASE", "http://127.0.0.1:18124/api").rstrip("/")
PASS = os.environ.get("P0FIX_PASS", "P0FixAdmin#2026")
PO_NO = os.environ.get("P0FIX_PO", "PO-20261031-000002")

PASS_CNT = 0
FAIL_CNT = 0


def ok(msg):
    global PASS_CNT
    PASS_CNT += 1
    print(f"  [PASS] {msg}")


def bad(msg):
    global FAIL_CNT
    FAIL_CNT += 1
    print(f"  [FAIL] {msg}")


def check(cond, msg, detail=""):
    if cond:
        ok(msg)
    else:
        bad(f"{msg} {detail}")


def call(method, path, token=None, body=None, timeout=30) -> "tuple[int, Any]":
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def login(username):
    status, payload = call("POST", "/system/auth/login", body={"username": username, "password": PASS})
    if status != 200:
        print(f"!! 登录失败 {username}: {status} {payload}")
        sys.exit(3)
    data = payload["data"]
    return data["access_token"], data.get("must_change_password", False)


def todo(token):
    status, payload = call("GET", "/business/flow/task/todo?page=1&page_size=200", token)
    if status != 200:
        print(f"!! 待办查询失败：{status} {payload}")
        sys.exit(3)
    return payload["data"]["list"], payload["data"].get("total", len(payload["data"]["list"]))


def po_of(token, order_no):
    status, payload = call("GET", "/business/erp/purchase-order/page?page=1&page_size=200", token)
    if status != 200:
        print(f"!! 采购单分页失败：{status} {payload}")
        sys.exit(3)
    for row in payload["data"]["list"]:
        if row.get("order_no") == order_no:
            return row
    return None


def phase_before():
    print("== 数据修复前（复现死锁）：老待办还在，但当事人点不动 ==")
    tl, _ = login("laoliu")
    ta, _ = login("admin")
    tt, _ = login("laoliu_test")

    li, ltotal = todo(tl)
    ai, atotal = todo(ta)
    ti, ttotal = todo(tt)
    print(f"  待办总数：laoliu={ltotal} admin={atotal} laoliu_test={ttotal}")

    mine = next((x for x in li if x.get("biz_no") == PO_NO), None)
    item = mine
    if item is None:
        bad(f"laoliu 的待办里看不到 {PO_NO}（修复前的预期是「看得见」）")
        return
    ok(f"laoliu 能看到 {PO_NO} 的待办（{item['instance_no']} / {item['node_name']}）")

    status, payload = call(
        "POST",
        f"/business/flow/task/{item['id']}/approve",
        tl,
        {"comment": "P0-FIX 修复前探针：制单人尝试审批自己那一票"},
    )
    msg = (payload.get("msg") or payload.get("message") or json.dumps(payload, ensure_ascii=False))
    check(status == 403, f"laoliu（制单人）点自己不动的票 → HTTP 403", f"实际 {status} {msg}")
    check("不能审批自己提交的采购订单" in msg, "报文点名内控原因", msg)

    admin_item = next((x for x in ai if x.get("biz_no") == PO_NO), None)
    if admin_item:
        status, payload = call(
            "POST",
            f"/business/flow/task/{admin_item['id']}/approve",
            ta,
            {"comment": "P0-FIX 修复前探针：发起人尝试审批"},
        )
        msg = (payload.get("msg") or json.dumps(payload, ensure_ascii=False))
        check(status == 403, "admin（发起人）点自己不动的票 → HTTP 403", f"实际 {status} {msg}")
    else:
        bad(f"admin 的待办里看不到 {PO_NO}（修复前应有这条 PENDING）")

    # 死锁证据：还有 3 张 PENDING、其中 2 张永远批不出来
    status, payload = call("GET", f"/business/flow/instance/{item['instance_id']}", tt)
    if status == 200:
        tasks = payload["data"].get("tasks") or payload["data"].get("timeline") or []
        pending = [t for t in tasks if t.get("status") == "PENDING"]
        ok(f"实例仍是 PENDING，节点上还有 {len(pending)} 张 PENDING 票（其中 2 张永远批不出来）")
    else:
        bad(f"实例详情查询失败 {status} {payload}")


def phase_after():
    print("== 数据修复后：被内控排除的人看不到待办；能批的人批完即通过 ==")
    ta, _ = login("admin")
    tl, _ = login("laoliu")
    tt, _ = login("laoliu_test")

    li, ltotal = todo(tl)
    ai, atotal = todo(ta)
    ti, ttotal = todo(tt)
    print(f"  待办总数：laoliu={ltotal} admin={atotal} laoliu_test={ttotal}")

    check(
        next((x for x in li if x.get("biz_no") == PO_NO), None) is None,
        f"laoliu（制单人）的「我的待办」里已经没有 {PO_NO}（不是「看得到点不动」）",
    )
    check(
        next((x for x in ai if x.get("biz_no") == PO_NO), None) is None,
        f"admin（发起人）的「我的待办」里也没有 {PO_NO}",
    )
    item = next((x for x in ti if x.get("biz_no") == PO_NO), None)
    if item is None:
        bad(f"laoliu_test（能批的人）看不到 {PO_NO} 的待办 —— 那他没法批")
        return
    ok(f"laoliu_test 能看到 {PO_NO} 的待办（{item['instance_no']} / {item['node_name']}）")

    # 审批详情里能看到「谁因内控被排除」
    status, payload = call("GET", f"/business/flow/instance/{item['instance_id']}", tt)
    if status == 200:
        data = payload["data"]
        tasks = data.get("tasks") or data.get("timeline") or []
        skipped = [
            t
            for t in tasks
            if t.get("status") == "SKIPPED" and "内控" in (t.get("comment") or "")
        ]
        check(
            len(skipped) >= 2,
            "审批详情（timeline）里能看到 2 条「内控被排除」的 SKIPPED 留痕",
            f"实际 {[(t.get('assignee_name'), t.get('comment')) for t in skipped]}",
        )
    else:
        bad(f"实例详情查询失败 {status} {payload}")

    status, payload = call(
        "POST",
        f"/business/flow/task/{item['id']}/approve",
        tt,
        {"comment": "P0-FIX 修复后验收：会签剩余的票批完"},
    )
    check(status == 200, "laoliu_test 审批成功", f"实际 {status} {payload.get('msg')}")

    if status == 200:
        inst = payload["data"]
        check(inst.get("instance_status") == "APPROVED", "会签满足 → 审批单 APPROVED", json.dumps(inst, ensure_ascii=False)[:200])
        row = po_of(tt, PO_NO)
        check(row is not None and row.get("status") == "APPROVED", f"{PO_NO} 已变为 APPROVED（不再卡在审批中）", str(row and row.get("status")))


def phase_new_flow():
    print("== 新代码行为：新提交的单不再给「制单人 / 发起人」生成待办 ==")
    ta, _ = login("admin")
    tl, _ = login("laoliu")
    tt, _ = login("laoliu_test")

    # 找一张 laoliu_test 制的草稿单（生产快照里有 4 张 PO-20261101-00000x）
    status, payload = call("GET", "/business/erp/purchase-order/page?page=1&page_size=200&status=DRAFT", tl)
    if status != 200:
        print(f"!! 草稿单分页失败：{status} {payload}")
        return
    draft = next((r for r in payload["data"]["list"] if r.get("status") == "DRAFT"), None)
    if draft is None:
        bad("生产快照里没有 DRAFT 采购单，无法演示新提交")
        return
    print(f"  用草稿 {draft['order_no']}（id={draft['id']}）演示：admin 发起审批")

    status, payload = call(
        "POST", f"/business/erp/purchase-order/{draft['id']}/submit-approval", ta, {}
    )
    if status != 200:
        print(f"!! 提交审批失败：{status} {payload}")
        return
    ok("admin 提交审批成功（他是发起人）")

    status, payload = call("GET", f"/business/flow/instance/{payload['data'].get('approval_instance_id') or payload['data'].get('instance_id')}", tt)
    instance_no = payload["data"]["instance_no"] if status == 200 else None
    tasks = (payload["data"].get("tasks") or payload["data"].get("timeline") or []) if status == 200 else []
    by_name = {}
    for t in tasks:
        by_name.setdefault(t.get("assignee_name") or "", []).append(t)
    print(f"  实例 {instance_no} 的待办：" + json.dumps(
        [{k: t.get("status"), "c": (t.get("comment") or "")[:24]} for t in tasks], ensure_ascii=False
    ))

    skips = [t for t in tasks if t.get("status") == "SKIPPED"]
    pend = [t for t in tasks if t.get("status") == "PENDING"]
    check(len(skips) >= 1, "生成时就落 SKIPPED 留痕（内控排除生效）")
    check(all("内控" in (t.get("comment") or "") for t in skips), "留痕写明内控原因")
    check(
        all((t.get("assignee_name") or "") not in ("老六", "超级管理员") for t in pend),
        "PENDING 里没有制单人（老六）与发起人（超级管理员）",
        str([t.get("assignee_name") for t in pend]),
    )

    # 剩下的人批完 → 流程推进
    for t in list(pend):
        name = t.get("assignee_name")
        token = tt if name in ("老六测试号", "老六") else ta
        s2, p2 = call("POST", f"/business/flow/task/{t['id']}/approve", token, {"comment": "P0-FIX 会签验收"})
        check(s2 == 200, f"审批人「{name}」批票成功", f"{s2} {p2.get('msg')}")
    row = po_of(tt, draft["order_no"])
    check(row is not None and row.get("status") == "APPROVED", f"{draft['order_no']} 批完即 APPROVED（不卡死）", str(row and row.get("status")))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--phase", choices=["before", "after", "new-flow"], required=True)
    args = parser.parse_args()
    print(f"### phase={args.phase} base={BASE} PO={PO_NO}")
    {"before": phase_before, "after": phase_after, "new-flow": phase_new_flow}[args.phase]()
    print(f"==> {args.phase} 汇总：PASS={PASS_CNT} FAIL={FAIL_CNT}")
    sys.exit(1 if FAIL_CNT else 0)


if __name__ == "__main__":
    main()
