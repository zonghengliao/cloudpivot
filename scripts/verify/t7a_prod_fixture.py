#!/usr/bin/env python3
"""T7a 生产真机验收夹具：造甘特拖拽需要的 5 条任务（含依赖 / 终态 / 单边日期）。

在 172.18.10.144 上跑：python3 t7a_prod_fixture.py
产出：/var/tmp/t7a-prod-fixture.json（任务 id + 编号 + 登录令牌），供浏览器脚本与清理使用。
"""

import json
import os
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1/api"
ENVF = "/etc/cloudpivot/cloudpivot.env"
OUT = "/var/tmp/t7a-prod-fixture.json"


def env(key):
    with open(ENVF, encoding="utf-8") as handle:
        for line in handle:
            if line.startswith(key + "="):
                return line.split("=", 1)[1].strip()
    raise SystemExit("env %s 缺失" % key)


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


def must(step, status, payload):
    if status != 200 or payload.get("code") != 0:
        sys.exit("%s 失败：%s %s" % (step, status, payload))
    return payload["data"]


# ---------- 真实登录（生产运维账号口令来自 /etc/cloudpivot/cloudpivot.env） ----------
# 口径见 docs/运维规程-凭据与自查账号.md（卡 t_d4e92b99）：只试一次，失败即停
OPS_USER = env("CLOUDPIVOT_OPS_USER") or "admin"
OPS_PASS = (
    env("CLOUDPIVOT_OPS_PASSWORD")
    or env("ALUN_ADMIN_PASSWORD")
)
if not OPS_PASS:
    sys.exit("缺 CLOUDPIVOT_OPS_PASSWORD（不要用 NIK_ADMIN_PASSWORD 猜 admin 口令）")
status, payload = req(
    "POST", "/system/auth/login", None, {"username": OPS_USER, "password": OPS_PASS}
)
data = must("%s 登录" % OPS_USER, status, payload)
token = data["access_token"]
print("真实登录成功：token 长度 %d" % len(token))


def create(title, start=None, due=None):
    body = {"title": title, "priority": "P1"}
    if start:
        body["start_date"] = start
    if due:
        body["due_date"] = due
    return must("建任务 " + title, *req("POST", "/business/task", token, body))


def depend(task_id, depends_on_id):
    return must(
        "加依赖",
        *req("POST", "/business/task/%s/dependencies" % task_id, token, {"depends_on_id": depends_on_id})
    )


def move(task_id, to):
    return must(
        "流转 " + to, *req("PATCH", "/business/task/%s/status" % task_id, token, {"to": to})
    )


pred = create("T7a真机-前置（数据库设计）", "2026-10-25", "2026-10-30")
mid = create("T7a真机-主体（接口开发）", "2026-11-01", "2026-11-05")
succ = create("T7a真机-后继（接口联调）", "2026-11-12", "2026-11-16")
only_due = create("T7a真机-仅截止日", None, "2026-11-08")
cancel = create("T7a真机-已取消", "2026-11-02", "2026-11-06")
depend(mid["id"], pred["id"])
depend(succ["id"], mid["id"])
move(cancel["id"], "CANCELLED")

fixture = {
    "token": token,
    "pred": {"id": pred["id"], "task_no": pred["task_no"]},
    "mid": {"id": mid["id"], "task_no": mid["task_no"]},
    "succ": {"id": succ["id"], "task_no": succ["task_no"]},
    "only_due": {"id": only_due["id"], "task_no": only_due["task_no"]},
    "cancelled": {"id": cancel["id"], "task_no": cancel["task_no"]},
}
with open(OUT, "w", encoding="utf-8") as handle:
    json.dump(fixture, handle, ensure_ascii=False, indent=2)

for key in ("pred", "mid", "succ", "only_due", "cancelled"):
    print("%-10s %s %s" % (key, fixture[key]["task_no"], fixture[key]["id"]))
print("夹具已写入", OUT)
