#!/usr/bin/env python3
"""再补一条 DONE 任务（验证「已完成任务不可拖拽」）。"""

import json
import urllib.error
import urllib.request

BASE = "http://127.0.0.1/api"
ENVF = "/etc/cloudpivot/cloudpivot.env"
FIX = "/var/tmp/t7a-prod-fixture.json"


def env(key):
    with open(ENVF, encoding="utf-8") as handle:
        for line in handle:
            if line.startswith(key + "="):
                return line.split("=", 1)[1].strip()
    raise SystemExit(key)


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
        return error.code, json.loads(error.read().decode())


token = json.load(open(FIX, encoding="utf-8"))["token"]
status, payload = req(
    "POST",
    "/business/task",
    token,
    {"title": "T7a真机-已完成", "priority": "P2", "start_date": "2026-11-03", "due_date": "2026-11-07"},
)
assert payload.get("code") == 0, payload
task = payload["data"]
for to in ("DOING", "REVIEW", "DONE"):
    status, payload = req("PATCH", "/business/task/%s/status" % task["id"], token, {"to": to})
    assert payload.get("code") == 0, (to, payload)
    print("→", to, status)

fix = json.load(open(FIX, encoding="utf-8"))
fix["done"] = {"id": task["id"], "task_no": task["task_no"]}
json.dump(fix, open(FIX, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("done task:", task["task_no"], task["id"])
