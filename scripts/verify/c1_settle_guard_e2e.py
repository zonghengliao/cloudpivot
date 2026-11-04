#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""C1 真机验收：审批通过时校验目标单据状态（修「任务已取消但审批通过」的孤儿）

老板报的形态（TSK-202610-000813）：

    20:00:26 任务状态 → CANCELLED
    20:09:06 审批实例状态 → APPROVED

根因：`flow::task_bridge::settle` 只看【审批实例状态】，单据侧被静默跳过
（`debug` 级，既没 WARN 也没留痕）。C1 的修法：写目标状态之前先问状态机
「单据此刻还在不在能接受这个结论的路径上」，不在就
**不落状态 + WARN + 审批实例留痕**（实例本身仍是 APPROVED）。

本脚本在**隔离实例**上按真实 HTTP 复现这个形态：

  ① 建任务 → 推进 DOING → REVIEW（自动发起 TASK_ACCEPT 审批）
  ② 直接改库把任务置 CANCELLED（模拟 P11 没把审批撤掉的孤儿 —— 这正是事故现场）
  ③ 走真实接口审批通过
  ④ 断言：实例 APPROVED / 任务仍 CANCELLED / 进度没被改写 / 留痕一条 / 日志有 WARN

环境变量：
    C1_BASE        默认 http://127.0.0.1:18098
    C1_DB          默认 cloudpivot_c1e
    C1_DB_USER / C1_DB_PASS   默认 alun / alun
    C1_ADMIN_PW    admin 口令（隔离库的口令，必填）
    C1_LOG         隔离实例日志（默认 /root/c1-e2e-server.log）

退出码：任一断言 FAIL → 1。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("C1_BASE", "http://127.0.0.1:18098")
DB = os.environ.get("C1_DB", "cloudpivot_c1e")
DB_USER = os.environ.get("C1_DB_USER", "alun")
DB_PASS = os.environ.get("C1_DB_PASS", "alun")
ADMIN_PW = os.environ.get("C1_ADMIN_PW", "")
LOG = os.environ.get("C1_LOG", "/root/c1-e2e-server.log")

# 留痕正文前缀，与 backend/src/business/flow/settle_guard.rs 的常量同值
TRACE_PREFIX = "审批结论未落单据"

FEEDBACK = {"pass": 0, "fail": 0}


def psql(sql):
    env = dict(os.environ, PGPASSWORD=DB_PASS)
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", DB_USER, "-d", DB,
         "-tA", "-F", "\x1f", "-c", sql],
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
    last = None
    for attempt in range(4):
        status, body = http("POST", "/api/system/auth/login",
                            {"username": username, "password": password})
        if status == 200 and body.get("code") == 0:
            data = body["data"]
            return data["access_token"], bool(data.get("must_change_password"))
        last = (status, body)
        if status == 429 or (isinstance(body, dict) and body.get("code") == 10005):
            time.sleep(5 * (attempt + 1))
            continue
        break
    status, body = last
    raise RuntimeError(
        f"登录 {username} 失败：HTTP {status} {json.dumps(body, ensure_ascii=False)[:200]}")


def login_ready(username, password):
    """登录 + 「首登强制改密」自愈（隔离库专用，见 P11 脚本同样的做法）。"""
    global ADMIN_PW
    token, must_change = login(username, password)
    if not must_change:
        return token
    new_pw = password + "-c1x"
    status, body = http("PUT", "/api/system/auth/password",
                        {"old_password": password, "new_password": new_pw}, token)
    if not (status == 200 and body.get("code") == 0):
        raise RuntimeError(
            f"{username} 首登改密失败：{json.dumps(body, ensure_ascii=False)[:200]}")
    ADMIN_PW = new_pw
    token, _ = login(username, new_pw)
    return token


def main():
    if not ADMIN_PW:
        print("缺少 C1_ADMIN_PW")
        return 2
    print(f"==== C1 真机验收 base={BASE} db={DB} ====")

    # ---------- 收尾残留（只清自己造的：标题前缀） ----------
    psql("SELECT count(*) FROM biz_task WHERE title LIKE 'C1-审批校验-e2e-%' AND deleted_at IS NULL")

    token = login_ready("admin", ADMIN_PW)
    uid = one("SELECT id::text FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL")
    print(f"       admin id = {uid}")

    marker = "C1-审批校验-e2e"
    title = marker + "-" + os.urandom(3).hex()
    status, body = http("POST", "/api/business/task", {
        "title": title,
        "description": "C1 e2e：任务提交验收后取消 → 审批通过不许把任务改回 DONE",
        "priority": "P2",
        "need_review": True,
        "owner_id": uid,
    }, token)
    expect("① 建任务 200", status == 200 and body.get("code") == 0,
           json.dumps(body, ensure_ascii=False)[:200])
    if status != 200 or body.get("code") != 0:
        print(f"---- PASS={FEEDBACK['pass']} FAIL={FEEDBACK['fail']}")
        return 1
    task_id = body["data"]["id"]
    task_no = body["data"]["task_no"]
    print(f"       任务 {task_no}（{task_id}）")

    for to in ("DOING", "REVIEW"):
        status, body = http("PATCH", f"/api/business/task/{task_id}/status", {"to": to}, token)
        expect(f"① {task_no} 推进到 {to}", status == 200 and body.get("code") == 0,
               json.dumps(body, ensure_ascii=False)[:200])

    inst_no = one("SELECT instance_no FROM biz_flow_instance "
                  f"WHERE biz_id = '{task_id}'::uuid AND deleted_at IS NULL "
                  "ORDER BY created_at DESC LIMIT 1")
    inst_status = one("SELECT status FROM biz_flow_instance "
                      f"WHERE instance_no = '{inst_no}'")
    todo_id = one("SELECT ft.id::text FROM biz_flow_task ft "
                  "JOIN biz_flow_instance i ON i.id = ft.instance_id "
                  f"WHERE i.instance_no = '{inst_no}' AND ft.status = 'PENDING' "
                  "ORDER BY ft.created_at LIMIT 1")
    expect("① 提交验收后生成 PENDING 审批实例 + 待办",
           bool(inst_no) and inst_status == "PENDING" and bool(todo_id),
           f"instance_no={inst_no} status={inst_status} todo={todo_id}")
    if not inst_no or not todo_id:
        print(f"---- PASS={FEEDBACK['pass']} FAIL={FEEDBACK['fail']}")
        return 1

    # ---------- ② 制造事故形态：任务已取消，审批单还挂着 ----------
    # 生产上 P11 会在任务取消时撤审批；事故里没撤掉（实例 9 分钟后被批），
    # 这里直接改库复现「任务 CANCELLED + 实例 PENDING」这个现场。
    psql(f"UPDATE biz_task SET status = 'CANCELLED', cancel_reason = 'C1 e2e 制造事故形态', "
         f"progress = 30 WHERE id = '{task_id}'::uuid")
    task_after = one(f"SELECT status FROM biz_task WHERE id = '{task_id}'::uuid")
    inst_after = one(f"SELECT status FROM biz_flow_instance WHERE instance_no = '{inst_no}'")
    expect("② 事故形态就位：任务 CANCELLED 且审批实例仍 PENDING",
           task_after == "CANCELLED" and inst_after == "PENDING",
           f"task={task_after} instance={inst_after}")

    # ---------- ③ 走真实接口审批通过 ----------
    status, body = http("POST", f"/api/business/flow/task/{todo_id}/approve",
                        {"comment": "C1 e2e：审批通过"}, token)
    expect("③ 审批通过 200", status == 200 and body.get("code") == 0,
           json.dumps(body, ensure_ascii=False)[:240])

    # ---------- ④ 断言 ----------
    inst_final = one(f"SELECT status FROM biz_flow_instance WHERE instance_no = '{inst_no}'")
    expect("④ 审批实例已按结论结束 = APPROVED（GUE 走完了，红线：不退回去）",
           inst_final == "APPROVED", inst_final)

    task_final = one(f"SELECT status FROM biz_task WHERE id = '{task_id}'::uuid")
    expect("④ 任务保持 CANCELLED（审批通过不许把它改回 DONE）",
           task_final == "CANCELLED", task_final)

    progress = one(f"SELECT progress::text FROM biz_task WHERE id = '{task_id}'::uuid")
    expect("④ 任务进度未被改写（审批通过的 100% 副作用同样不落）",
           progress == "30", progress)

    traces = psql("SELECT ft.status, ft.is_auto::text, coalesce(ft.comment,'') "
                  f"FROM biz_flow_task ft JOIN biz_flow_instance i ON i.id = ft.instance_id "
                  f"WHERE i.instance_no = '{inst_no}' AND ft.comment LIKE '%{TRACE_PREFIX}%'")
    expect("④ 跳过落状态留下一条留痕", len(traces) == 1, f"{len(traces)} 条")
    if traces:
        t_status, t_auto, t_comment = traces[0]
        expect("④ 留痕形态：SKIPPED + is_auto=true",
               t_status == "SKIPPED" and t_auto == "true", f"{t_status}/{t_auto}")
        need = [TRACE_PREFIX, task_no, "CANCELLED", "DONE", inst_no]
        missing = [n for n in need if n not in t_comment]
        expect("④ 留痕正文含 单号/当前状态/目标状态/实例号/时间要素",
               not missing, f"缺失 {missing}：{t_comment[:160]}")
        print(f"       留痕正文：{t_comment}")

    # 服务日志里的 WARN（人肉排障的入口，卡面要求含实例号、任务号、两个状态、时间）
    log_hit = ""
    try:
        with open(LOG, "r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if "审批结论未落单据" in line and inst_no in line:
                    log_hit = line.strip()
    except FileNotFoundError:
        pass
    expect("④ 服务日志有一条 WARN（含实例号）", bool(log_hit), log_hit[-260:] if log_hit else "未命中")
    if log_hit:
        for needle in (task_no, "CANCELLED", "DONE"):
            expect(f"④ WARN 含 {needle}", needle in log_hit, log_hit[-160:])

    # ---------- 收尾：软删自己造的任务 ----------
    status, body = http("DELETE", f"/api/business/task/{task_id}", None, token)
    expect("⑤ 收尾：软删 e2e 任务", status == 200 and body.get("code") == 0, f"HTTP {status}")
    print(f"       e2e 任务：{task_no}（{task_id}）实例 {inst_no}")
    print(f"---- PASS={FEEDBACK['pass']} FAIL={FEEDBACK['fail']}")
    return 0 if FEEDBACK["fail"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
