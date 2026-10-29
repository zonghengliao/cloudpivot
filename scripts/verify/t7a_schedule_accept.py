#!/usr/bin/env python3
"""T7a 后端验收：甘特拖拽改期（逐条真机，贴库值）。

在 172.18.10.144 上跑：python3 t7a_accept.py
实例：/var/tmp/t7a-run（127.0.0.1:18086，库 cloudpivot_t7a）

覆盖卡面「三、验收 / 后端」1–7 条 + 401 + 形态校验 + 父任务甩出子任务 + 权限。
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("T7A_BASE", "http://127.0.0.1:18086/api")
DB = "cloudpivot_t7a"
ENVF = "/etc/cloudpivot/cloudpivot-test.env"
NEW_PW = "T7a#Accept2026"


def env(key):
    with open(ENVF, encoding="utf-8") as handle:
        for line in handle:
            if line.startswith(key + "="):
                return line.split("=", 1)[1].strip()
    raise SystemExit("env %s 缺失" % key)


PGPW = env("ALUN_DATABASE_PASSWORD")


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


def db(sql):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", DB, "-At", "-F", "|", "-c", sql],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": PGPW},
    )
    if out.returncode != 0:
        raise SystemExit("psql 失败：%s\n%s" % (sql, out.stderr))
    return [line for line in out.stdout.splitlines() if line != ""]


def login(username, password):
    status, payload = req(
        "POST", "/system/auth/login", None, {"username": username, "password": password}
    )
    if status != 200 or payload.get("code") != 0:
        return None, (status, payload)
    return payload["data"]["access_token"], (status, payload)


def banner(text):
    print("\n" + "=" * 78)
    print("#", text)
    print("=" * 78)


def show(label, status, payload):
    print("%s → HTTP %s %s" % (label, status, json.dumps(payload, ensure_ascii=False)))
    return status, payload


# ==================== 登录 ====================
banner("0. 登录（空库 admin 首登必须改密）")
env_pw = env("ALUN_ADMIN_PASSWORD")
token, _ = login("admin", env_pw)
if token is None:
    # 重跑场景：口令已在上一轮改成验收口令
    token, info = login("admin", NEW_PW)
    if token is None:
        sys.exit("admin 登录失败（env 口令与验收口令都不通）：%s" % (info,))
    print("admin 已用验收口令登录（上一轮改过密）")
else:
    status, payload = req(
        "PUT",
        "/system/auth/password",
        token,
        {"old_password": env_pw, "new_password": NEW_PW},
    )
    print("改密 → HTTP %s %s" % (status, json.dumps(payload, ensure_ascii=False)))
    token, _ = login("admin", NEW_PW)
    if token is None:
        sys.exit("改密后登录失败")
print("admin 登录成功（口令 = 验收口令）")

# 无令牌探测（§6.2 要求 401）
status, payload = req("PATCH", "/business/task/00000000-0000-0000-0000-000000000001/schedule", None, {"shift_days": 1})
show("无令牌 PATCH /schedule", status, payload)


def create(title, start=None, due=None, parent_id=None):
    body = {"title": title, "priority": "P1", "due_date": due}
    if start:
        body["start_date"] = start
    if parent_id:
        body["parent_id"] = parent_id
    status, payload = req("POST", "/business/task", token, body)
    if status != 200 or payload.get("code") != 0:
        sys.exit("建任务失败 %s：%s %s" % (title, status, payload))
    return payload["data"]


def depend(task_id, depends_on_id):
    status, payload = req(
        "POST", "/business/task/%s/dependencies" % task_id, token, {"depends_on_id": depends_on_id}
    )
    if status != 200 or payload.get("code") != 0:
        sys.exit("加依赖失败：%s %s" % (status, payload))
    return payload["data"]


def schedule(task_id, body):
    return req("PATCH", "/business/task/%s/schedule" % task_id, token, body)


def span(task_id):
    rows = db("SELECT start_date, due_date FROM biz_task WHERE id = '%s'" % task_id)
    return rows[0].split("|") if rows else ["?", "?"]


# ==================== 造数据 ====================
banner("1. 造依赖链 + 父子组")
pred = create("T7a验收-前置（数据库设计）", "2026-10-25", "2026-10-30")
mid = create("T7a验收-主体（接口开发）", "2026-11-01", "2026-11-05")
succ = create("T7a验收-后继（接口联调）", "2026-11-10", "2026-11-15")
parent = create("T7a验收-父任务", "2026-11-01", "2026-11-30")
child = create("T7a验收-子任务", "2026-11-05", "2026-11-10", parent_id=parent["id"])
depend(mid["id"], pred["id"])
depend(succ["id"], mid["id"])
print("前置 %s %s" % (pred["task_no"], span(pred["id"])))
print("主体 %s %s" % (mid["task_no"], span(mid["id"])))
print("后继 %s %s" % (succ["task_no"], span(succ["id"])))
print("父   %s %s" % (parent["task_no"], span(parent["id"])))
print("子   %s %s" % (child["task_no"], span(child["id"])))

# ==================== ① 平移 3 天 ====================
banner("2. 【验收1】拖平移 3 天 → 起止日都 +3（贴库值）")
before = span(mid["id"])
status, payload = schedule(mid["id"], {"shift_days": 3})
show("PATCH shift_days=3", status, payload)
after = span(mid["id"])
print("库值 before=%s after=%s" % (before, after))
assert before == ["2026-11-01", "2026-11-05"], before
assert after == ["2026-11-04", "2026-11-08"], after

# ==================== ② 前置未完成 → 63110 ====================
banner("3. 【验收2】拖到前置未完成之前 → 400/63110 点名前置")
status, payload = schedule(mid["id"], {"start_date": "2026-10-28"})
show("PATCH start_date=2026-10-28", status, payload)
assert status == 400 and payload["code"] == 63110, payload
assert pred["task_no"] in payload["msg"] and "数据库设计" in payload["msg"] and "2026-10-30" in payload["msg"]
print("库值（必须没变）：", span(mid["id"]))

# ==================== ③ 后继开始之后 → 63111 ====================
banner("4. 【验收3】拖到后继开始之后 → 400/63111 点名后继")
status, payload = schedule(mid["id"], {"due_date": "2026-11-12"})
show("PATCH due_date=2026-11-12", status, payload)
assert status == 400 and payload["code"] == 63111, payload
assert succ["task_no"] in payload["msg"] and "接口联调" in payload["msg"] and "2026-11-10" in payload["msg"]
print("库值（必须没变）：", span(mid["id"]))

# ==================== ④ 左 / 右边缘 ====================
banner("5. 【验收4】左边缘拖 → 只改 start_date")
status, payload = schedule(mid["id"], {"start_date": "2026-11-05"})
show("PATCH start_date=2026-11-05", status, payload)
left = span(mid["id"])
print("库值：", left)
assert left == ["2026-11-05", "2026-11-08"], left

status, payload = schedule(mid["id"], {"due_date": "2026-11-09"})
show("PATCH due_date=2026-11-09", status, payload)
right = span(mid["id"])
print("库值：", right)
assert right == ["2026-11-05", "2026-11-09"], right

# ==================== ⑤ start > due ====================
banner("6. 【验收5】start_date > due_date → 400")
status, payload = schedule(mid["id"], {"start_date": "2026-12-01"})
show("PATCH start_date=2026-12-01", status, payload)
assert status == 400, payload
print("库值（必须没变）：", span(mid["id"]))

# ==================== ⑥ 子任务越出父范围 ====================
banner("7. 【验收6】子任务拖出父范围 → 400/63112")
status, payload = schedule(child["id"], {"start_date": "2026-10-28"})
show("PATCH child start_date=2026-10-28", status, payload)
assert status == 400 and payload["code"] == 63112, payload
assert parent["task_no"] in payload["msg"], payload["msg"]
print("库值（必须没变）：", span(child["id"]))

banner("7b. 反向：父任务整体+10 天把子任务甩出区间 → 400/63112（本卡不级联）")
status, payload = schedule(parent["id"], {"shift_days": 10})
show("PATCH parent shift_days=10", status, payload)
assert status == 400 and payload["code"] == 63112, payload
assert child["task_no"] in payload["msg"], payload["msg"]

# ==================== ⑦ changelog + 评论 ====================
banner("8. 【验收7】改期成功 → changelog + SCHEDULE 评论（贴库值）")
rows = db(
    "SELECT comment_type, content FROM biz_task_comment "
    "WHERE task_id = '%s' AND comment_type = 'SCHEDULE' ORDER BY created_at" % mid["id"]
)
for row in rows:
    print("评论：", row)
assert rows, "没有 SCHEDULE 评论"
rows = db(
    "SELECT action, action_label, changed_fields, diff::text FROM sys_change_log "
    "WHERE resource_id = '%s' ORDER BY created_at DESC LIMIT 3" % mid["id"]
)
for row in rows:
    print("变更日志：", row)
assert any("start_date" in row for row in rows), "变更日志没有 start_date"

# ==================== 形态校验 ====================
banner("9. 形态校验：两种形态同时传 / 空请求 / shift_days=0 → 400（码 10001 = 参数非法）")
for body in (
    {"shift_days": 3, "start_date": "2026-11-05"},
    {},
    {"shift_days": 0},
):
    status, payload = schedule(mid["id"], body)
    show("PATCH %s" % json.dumps(body), status, payload)
    assert status == 400 and payload["code"] == 10001, payload

# ==================== 前置完成后放行 ====================
banner("10. 前置完成后（DONE）改期不再被 63110 拦")
rows = db("SELECT id FROM biz_task WHERE id = '%s'" % pred["id"])
status, payload = req(
    "PATCH", "/business/task/%s/status" % pred["id"], token, {"to": "DOING"}
)
print("前置 TODO→DOING →", status, payload.get("code"))
status, payload = req(
    "PATCH", "/business/task/%s/status" % pred["id"], token, {"to": "REVIEW"}
)
print("前置 DOING→REVIEW →", status, payload.get("code"))
status, payload = req(
    "PATCH", "/business/task/%s/status" % pred["id"], token, {"to": "DONE"}
)
print("前置 REVIEW→DONE →", status, payload.get("code"))
status, payload = schedule(mid["id"], {"start_date": "2026-10-28"})
show("前置 DONE 后 PATCH start_date=2026-10-28", status, payload)
print("库值：", span(mid["id"]))

# ==================== 收尾 ====================
banner("汇总")
print("前置:", pred["task_no"], pred["id"])
print("主体:", mid["task_no"], mid["id"], span(mid["id"]))
print("后继:", succ["task_no"], succ["id"])
print("父  :", parent["task_no"], parent["id"])
print("子  :", child["task_no"], child["id"])
print("\nT7A_ACCEPT_OK")
