#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P11 端到端验收：任务取消 → 审批实例与待办一起撤（真实 HTTP + 真库）

老板 2026-10-31 报的现象（TSK-202610-000149）：
    超管把一个工作任务取消了，但审批人（henry.liao）的「待我审批」里
    还挂着一条「等待审批」的待办，点进去什么也做不了。

本脚本按三个相位跑，配合 `scripts/verify/p11_flow_orphan.sh` 的顺序调用：

    --phase before    ① 存量脏数据还在库里时，看 henry.liao 的待办 API
                        （代码已修：`my_todo` 只认实例还在 PENDING 的待办，
                         所以这一相位应当「看不到 TSK-149」——这就是后端防御在起作用）
    --phase after     ② 数据修复之后：库里 0 条孤儿，henry.liao 待办仍然干净
    --phase codepath  ③ 新建任务 → 提交验收（自动发起 TASK_ACCEPT 审批）→ 取消任务
                        查库断言：实例 CANCELLED / 待办 CANCELLED / 节点 CANCELLED

前置：
  * 一个**已迁移到最新**的隔离实例（**不要指向 prod**），库里有一份 prod 副本数据
  * 环境变量见下（密码从 /etc/cloudpivot/*.env 注入，不落盘）

环境变量：
    P11_BASE        默认 http://127.0.0.1:18120
    P11_DB          默认 cloudpivot_p11t
    P11_DB_USER / P11_DB_PASS   库账号
    P11_ADMIN_PW    admin 口令（必须是这个隔离库里的真口令）
    P11_HENRY_PW    给 henry.liao **临时**改成的口令（只在隔离库上改，见 reset 说明）

退出码：任一断言 FAIL → 1。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("P11_BASE", "http://127.0.0.1:18120")
DB = os.environ.get("P11_DB", "cloudpivot_p11t")
DB_USER = os.environ.get("P11_DB_USER", "p11role")
DB_PASS = os.environ.get("P11_DB_PASS", "p11_drill")
ADMIN_PW = os.environ.get("P11_ADMIN_PW", "")
HENRY_PW = os.environ.get("P11_HENRY_PW", "P11-Henry@123456")

# 老板报的那条：实例 FLW-20261030-1BAE2858 / 任务 TSK-202610-000149 / 审批人 henry.liao
BOSS_INSTANCE_NO = "FLW-20261030-1BAE2858"
BOSS_BIZ_NO = "TSK-202610-000149"

FEEDBACK = {"pass": 0, "fail": 0}


def psql(sql):
    env = dict(os.environ, PGPASSWORD=DB_PASS)
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", DB_USER, "-d", DB, "-tA", "-F", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def one(sql, default=""):
    rows = psql(sql)
    return rows[0][0] if rows and rows[0] else default


def http(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw}


def expect(name, ok, detail=""):
    if ok:
        FEEDBACK["pass"] += 1
        print(f"[PASS] {name}" + (f" — {detail}" if detail else ""))
    else:
        FEEDBACK["fail"] += 1
        print(f"[FAIL] {name}" + (f" — {detail}" if detail else ""))


def login(username, password):
    # 登录限流（10005「登录尝试过于频繁」）是进程内内存态：一次 e2e 会登好几次，
    # 撞上了就退避重试，别把限流当成「口令错」。
    last = None
    for attempt in range(4):
        status, body = http("POST", "/api/system/auth/login",
                            {"username": username, "password": password})
        if status == 200 and body.get("code") == 0:
            data = body["data"]
            return data["access_token"], bool(data.get("must_change_password"))
        last = (status, body)
        if status == 429 or (isinstance(body, dict) and body.get("code") == 10005):
            time.sleep(10 * (attempt + 1))
            continue
        break
    status, body = last
    raise RuntimeError(f"登录 {username} 失败：HTTP {status} {json.dumps(body, ensure_ascii=False)[:200]}")


def admin_token():
    return login_ready("admin", ADMIN_PW)


def reset_henry_password(admin):
    """把 henry.liao 的口令临时改成 `P11_HENRY_PW`（**只在隔离库上做**）。

    为什么要改：本脚本要用真令牌走 `/business/flow/task/todo`，而生产口令无人知晓。
    卡面允许在**副本库**上做这种改动的先例（T8-C：把 laoliu 的口令换成测试号的哈希）。
    走的是系统自带的管理员重置口令接口（`PATCH /system/user/{id}/password`），
    **不写库、不碰哈希**；重置后目标账号 `must_change_password = true`，
    所以紧接的登录要按「首登强制改密」的流程走一遍。
    生产库上本脚本**不会**执行这一步（before/after 相位只在隔离实例上跑）。
    """
    rows = psql("select id from sys_user where username = 'henry.liao' and deleted_at is null")
    if not rows:
        raise RuntimeError("隔离库里没有 henry.liao（precondition 不满足）")
    uid = rows[0][0]
    status, body = http("PATCH", f"/api/system/user/{uid}/password", {"password": HENRY_PW}, admin)
    if not (status == 200 and body.get("code") == 0):
        raise RuntimeError(f"重置 henry.liao 口令失败：HTTP {status} {json.dumps(body, ensure_ascii=False)[:300]}")
    return uid


def login_ready(username, password):
    """登录并对「首登强制改密」做一次自愈，返回可用令牌。"""
    token, must_change = login(username, password)
    if not must_change:
        return token
    new_pw = password + "-p11x"
    status, body = http("PUT", "/api/system/auth/password",
                        {"old_password": password, "new_password": new_pw}, token)
    if not (status == 200 and body.get("code") == 0):
        raise RuntimeError(f"{username} 首登改密失败：{json.dumps(body, ensure_ascii=False)[:200]}")
    if username == "henry.liao":
        globals()["HENRY_PW"] = new_pw
    else:
        globals()["ADMIN_PW"] = new_pw
    token, _ = login(username, new_pw)
    return token


def todo_instances(token):
    status, body = http("GET", "/api/business/flow/task/todo?page=1&page_size=200", token=token)
    if status != 200 or body.get("code") != 0:
        raise RuntimeError(f"查待办失败：HTTP {status} {json.dumps(body, ensure_ascii=False)[:300]}")
    return body["data"]["list"]


def phase_todo(tag):
    admin = admin_token()
    reset_henry_password(admin)
    token = login_ready("henry.liao", HENRY_PW)
    rows = todo_instances(token)
    hit = [r for r in rows if r.get("instance_no") == BOSS_INSTANCE_NO
           or r.get("biz_no") == BOSS_BIZ_NO]
    db_row = one(
        "select ft.status || '/' || i.status from biz_flow_task ft "
        f"join biz_flow_instance i on i.id = ft.instance_id "
        f"join sys_user u on u.id = ft.assignee_id "
        f"where i.instance_no = '{BOSS_INSTANCE_NO}' and u.username = 'henry.liao'")
    print(f"       henry.liao 待办共 {len(rows)} 条；库里那条待办/实例状态 = {db_row}")
    expect(f"{tag} henry.liao 待办里看不到 {BOSS_BIZ_NO}", not hit,
           f"命中 {len(hit)} 条" if hit else "0 条命中")
    # 「已办」不受 P11 的 live_only 约束：实例结束与否都要能看到历史（否则审批人查不到自己批过什么）
    status, body = http("GET", "/api/business/flow/task/done?page=1&page_size=200", token=token)
    expect(f"{tag} 已办接口仍可访问（不受 live_only 影响）",
           status == 200 and body.get("code") == 0,
           f"HTTP {status}，{len(body.get('data', {}).get('list', []))} 条")
    if tag.startswith("②"):
        # 数据修复后：库里也不该再有「待办 PENDING + 实例终态」的组合
        dirty = one("select count(*) from biz_flow_task ft join biz_flow_instance i "
                    "on i.id = ft.instance_id where ft.status = 'PENDING' "
                    "and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN') "
                    "and i.deleted_at is null")
        expect("② 库里孤儿待办 = 0", dirty == "0", f"dirty={dirty}")


def phase_codepath():
    admin = admin_token()
    uid = one("select coalesce((select id from sys_user where username='admin' "
              "and deleted_at is null)::text,'')")
    marker = "P11-取消撤审批-e2e"
    title = marker + "-" + os.urandom(3).hex()
    status, body = http("POST", "/api/business/task", {
        "title": title,
        "description": "P11 e2e：提交验收生成审批 → 取消任务 → 审批实例与待办必须一起撤",
        "priority": "P2",
        "need_review": True,
        "owner_id": uid,
    }, admin)
    expect("建任务 200", status == 200 and body.get("code") == 0,
           json.dumps(body, ensure_ascii=False)[:160])
    if status != 200 or body.get("code") != 0:
        return
    task_id = body["data"]["id"]
    task_no = body["data"]["task_no"]

    for to in ("DOING", "REVIEW"):
        status, body = http("PATCH", f"/api/business/task/{task_id}/status", {"to": to}, admin)
        expect(f"{task_no} 推进到 {to}", status == 200 and body.get("code") == 0,
               json.dumps(body, ensure_ascii=False)[:200])

    inst_no = one("select instance_no from biz_flow_instance where biz_id = '%s' "
                  "and deleted_at is null order by created_at desc limit 1" % task_id)
    expect("提交验收后生成审批实例", bool(inst_no), f"instance_no={inst_no}")
    if not inst_no:
        return
    before = psql(
        "select i.status, coalesce(sum(case when ft.status='PENDING' then 1 else 0 end),0)::text "
        "from biz_flow_instance i left join biz_flow_task ft on ft.instance_id = i.id "
        f"where i.instance_no = '{inst_no}' group by i.status")
    print(f"       取消前：实例状态/待办 PENDING 数 = {before}")
    expect("取消前：实例 PENDING 且有待办 PENDING",
           bool(before) and before[0][0] == "PENDING" and int(before[0][1]) >= 1,
           str(before))

    status, body = http("PATCH", f"/api/business/task/{task_id}/status",
                        {"to": "CANCELLED", "reason": "P11 e2e 验证撤销审批"}, admin)
    expect("取消任务 200", status == 200 and body.get("code") == 0,
           json.dumps(body, ensure_ascii=False)[:200])

    inst = one(f"select status from biz_flow_instance where instance_no = '{inst_no}'")
    todo = one("select status || '/' || (completed_at is not null)::text from biz_flow_task "
               f"where instance_id = (select id from biz_flow_instance where instance_no = '{inst_no}') "
               "order by created_at limit 1")
    # 节点：发起节点 `s` 在发起时就是 APPROVED（自动放行），真正的待审节点是 `n_creator`。
    # 所以判据不是「第一个节点 = CANCELLED」，而是「不再有 ACTIVE/PENDING 的节点」，
    # 且**原来在等审批的那个节点**必须是 CANCELLED（历史节点一个字都不改）。
    nodes = psql("select node_key, status, (ended_at is not null)::text from biz_flow_instance_node "
                 f"where instance_id = (select id from biz_flow_instance where instance_no = '{inst_no}') "
                 "order by created_at")
    live_nodes = [n for n in nodes if n[1] in ("ACTIVE", "PENDING")]
    node = " ".join(f"{n[0]}:{n[1]}" for n in nodes)
    cmt = one(f"select coalesce(result_comment,'') from biz_flow_instance where instance_no = '{inst_no}'")
    print(f"       取消后：实例={inst} 待办={todo} 节点=[{node}] 文案={cmt}")
    expect("取消后：实例 CANCELLED", inst == "CANCELLED", inst)
    expect("取消后：待办 CANCELLED 且 completed_at 有值", todo == "CANCELLED/true", todo)
    expect("取消后：实例不再有 ACTIVE/PENDING 节点（审批节点已 CANCELLED 且 ended_at 有值）",
           not live_nodes and any(n[1] == "CANCELLED" and n[2] == "true" for n in nodes),
           node)
    expect("取消后：审批单文案是「任务已取消」而不是「已删除」",
           "取消" in cmt and "删除" not in cmt, cmt)

    # 幂等：再取消一次不报错、不改数据
    status, body = http("PATCH", f"/api/business/task/{task_id}/status",
                        {"to": "CANCELLED", "reason": "P11 e2e 幂等复核"}, admin)
    expect("重复取消幂等（200 或 409 合法拒绝，不 500）", status in (200, 409),
           f"HTTP {status} {json.dumps(body, ensure_ascii=False)[:120]}")

    # 收尾：把本条 e2e 任务软删（只删自己造的）
    status, body = http("DELETE", f"/api/business/task/{task_id}", None, admin)
    expect("收尾：软删本条 e2e 任务", status == 200 and body.get("code") == 0,
           f"HTTP {status}")
    print(f"       e2e 任务：{task_no}（{task_id}）实例 {inst_no}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", choices=["before", "after", "codepath"], required=True)
    args = ap.parse_args()
    if not ADMIN_PW:
        print("缺少 P11_ADMIN_PW")
        return 2
    print(f"==== P11 e2e 相位 {args.phase}  base={BASE} db={DB} ====")
    if args.phase == "before":
        phase_todo("① 修复前（代码已修、存量数据未修）")
    elif args.phase == "after":
        phase_todo("② 数据修复后")
    else:
        phase_codepath()
    print(f"---- 相位 {args.phase}：PASS={FEEDBACK['pass']} FAIL={FEEDBACK['fail']}")
    return 0 if FEEDBACK["fail"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
