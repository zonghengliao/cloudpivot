#!/usr/bin/env python3
"""T7b 看板验收：造 6 条任务覆盖 5 状态 + 一条「他人创建 / qa.boarder 负责」的待验收任务。

在 172.18.10.144 上跑：python3 t7b_board_setup.py
清理由 t7b_board_cleanup.py 完成（软删 T7b验收-* 任务 + 删临时账号）。
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("T7B_BASE", "http://127.0.0.1/api")
PW_ADMIN = os.environ.get("ALUN_ADMIN_PASSWORD", "")
PW_BOARDER = os.environ.get("ALUN_BOARDER_PASSWORD", "")
DB = "cloudpivot"


def req(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(BASE + path, data=data, method=method)
    request.add_header("Content-Type", "application/json")
    if token:
        request.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, json.loads(response.read().decode())
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username, password):
    status, payload = req("POST", "/system/auth/login", None, {"username": username, "password": password})
    if status != 200 or payload.get("code") != 0:
        sys.exit(f"登录失败 {username}: {status} {payload}")
    return payload["data"]["access_token"]


def create(token, **body):
    status, payload = req("POST", "/business/task", token, body)
    if status != 200 or payload.get("code") != 0:
        sys.exit(f"建任务失败：{status} {payload}")
    return payload["data"]


def move(token, task_id, to, reason=None):
    body = {"to": to}
    if reason:
        body["reason"] = reason
    status, payload = req("PATCH", f"/business/task/{task_id}/status", token, body)
    if status != 200 or payload.get("code") != 0:
        sys.exit(f"流转失败 {to}：{status} {payload}")
    return payload["data"]


def user_id(username):
    sql = f"SELECT id FROM sys_user WHERE username='{username}' AND deleted_at IS NULL"
    out = subprocess.run(
        ["sudo", "-u", "postgres", "psql", "-d", DB, "-Atc", sql],
        capture_output=True,
        text=True,
    )
    return out.stdout.strip()


def main():
    admin = login("admin", PW_ADMIN)
    boarder_id = user_id("qa.boarder")
    if not boarder_id:
        sys.exit("qa.boarder 不存在：先灌 t7b_fixture.sql")

    admin_id = user_id("admin")
    made = []

    # ① TODO + 逾期（2026-10-27，今天 10-29）+ P1
    t1 = create(admin, title="T7b验收-待开始-逾期", priority="P1", due_date="2026-10-27",
                estimate_hours=4, description="看板验收：逾期标红用")
    made.append(("TODO/逾期/P1", t1))

    # ② TODO + P0（校验列内排序：P0 在 P1 之前）
    t2 = create(admin, title="T7b验收-待开始-正常", priority="P0", due_date="2026-11-15", estimate_hours=6)
    made.append(("TODO/P0", t2))

    # ③ DOING
    t3 = create(admin, title="T7b验收-进行中", priority="P2", due_date="2026-11-02", estimate_hours=8)
    move(admin, t3["id"], "DOING")
    made.append(("DOING/P2", t3))

    # ④ REVIEW（需验收）
    t4 = create(admin, title="T7b验收-待验收", priority="P1", due_date="2026-11-05", need_review=True,
                estimate_hours=10)
    move(admin, t4["id"], "DOING")
    move(admin, t4["id"], "REVIEW")
    made.append(("REVIEW/P1", t4))

    # ⑤ DONE（不需验收 → DOING 可直接完成）
    t5 = create(admin, title="T7b验收-已完成", priority="P3", due_date="2026-10-20", estimate_hours=2)
    move(admin, t5["id"], "DOING")
    move(admin, t5["id"], "DONE")
    made.append(("DONE/P3", t5))

    # ⑥ CANCELLED
    t6 = create(admin, title="T7b验收-已取消", priority="P2", due_date="2026-11-20", estimate_hours=3)
    move(admin, t6["id"], "CANCELLED", "T7b 看板验收：取消列用")
    made.append(("CANCELLED/P2", t6))

    # ⑦ 403 用：admin 创建、qa.boarder 负责、状态 REVIEW
    #    → qa.boarder 是「负责人」但不是创建人/分派人/管理员，推 REVIEW→DONE 必 403+63011
    t7 = create(admin, title="T7b验收-待验收-他人创建", priority="P1", due_date="2026-11-08",
                need_review=True, owner_id=boarder_id, estimate_hours=5)
    move(admin, t7["id"], "DOING")                 # 管理员位可推
    boarder = login("qa.boarder", PW_BOARDER)
    move(boarder, t7["id"], "REVIEW")              # 负责人提交验收
    made.append(("REVIEW/他人创建/负责人=qa.boarder", t7))

    print("=" * 78)
    print("造数完成（admin id=%s，qa.boarder id=%s）" % (admin_id, boarder_id))
    for label, task in made:
        print(f"  {label:34s} {task['task_no']}  {task['id']}")
    print("=" * 78)


if __name__ == "__main__":
    main()
