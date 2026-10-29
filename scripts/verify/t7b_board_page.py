#!/usr/bin/env python3
"""T7b 加载更多用例：造 55 条 TODO 任务（标题前缀 T7b分页-），验证每列 50 条 + 加载更多。"""

import json
import os
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("T7B_BASE", "http://127.0.0.1/api")
PW = os.environ.get("ALUN_ADMIN_PASSWORD", "")


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
        return error.code, json.loads(error.read().decode() or "{}")


def main():
    status, payload = req("POST", "/system/auth/login", None, {"username": "admin", "password": PW})
    if status != 200:
        sys.exit(f"登录失败 {status}")
    token = payload["data"]["access_token"]

    count = int(sys.argv[1]) if len(sys.argv) > 1 else 55
    for index in range(1, count + 1):
        body = {
            "title": f"T7b分页-{index:02d}",
            "priority": ["P0", "P1", "P2", "P3"][index % 4],
            "due_date": f"2026-11-{(index % 27) + 1:02d}",
            "estimate_hours": 1 + index % 5,
        }
        status, payload = req("POST", "/business/task", token, body)
        if status != 200:
            sys.exit(f"第 {index} 条建失败：{status} {payload}")
    print(f"已建 {count} 条 T7b分页-* 任务")


if __name__ == "__main__":
    main()
