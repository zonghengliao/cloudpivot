#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""A6 补充验证：SSE 端点的鉴权不可穿透。

做三件事：
1. 不带 Authorization 访问 /business/notice/stream → 必须 401；
2. 用 **B 的 token** + `?user_id=<A>` 访问 → 频道仍属于 B（A 的频道不能被伪造 query 建出来）；
3. 用 A 的 token 正常订阅后，推给 B 的通知不会到达这条连接。

用法（在 172.18.10.144 上，8026 测试实例已起）：
    python3 /root/work/a6/auth_probe.py
"""

import http.client
import json
import sys
import threading
import time
import urllib.error
import urllib.request

PORT = 8026
BASE = f"http://127.0.0.1:{PORT}/api"
ENV_FILE = "/etc/cloudpivot/cloudpivot-test.env"
USER_A = "a6000000-0000-0000-0000-0000000000a1"
USER_B = "a6000000-0000-0000-0000-0000000000b1"

FAILURES = []


def load_env(path):
    env = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            env[key.strip()] = value.strip()
    return env


ENV = load_env(ENV_FILE)


def api(method, path, body=None, token=None):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            raw = response.read().decode()
            return response.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username):
    status, body = api("POST", "/system/auth/login",
                       {"username": username, "password": ENV["ALUN_ADMIN_PASSWORD"]})
    if status != 200 or body.get("code") != 0:
        raise RuntimeError(f"登录失败 {username}: {status} {body}")
    data = body["data"]
    return data.get("access_token") or data.get("token")


def check(label, ok, detail=""):
    print(f"  [{'PASS' if ok else 'FAIL'}] {label}{('：' + detail) if detail else ''}")
    if not ok:
        FAILURES.append(label)


def receivers_of(stats_data, user_id):
    for user in stats_data["users"]:
        if user["user_id"] == user_id:
            return user["receivers"]
    return 0


def open_stream(token, query=""):
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    path = "/api/business/notice/stream" + query
    conn.request("GET", path, headers={"Authorization": f"Bearer {token}",
                                       "Accept": "text/event-stream"})
    response = conn.getresponse()
    return conn, response


class Reader(threading.Thread):
    def __init__(self, response):
        super().__init__(daemon=True)
        self.response = response
        self.lines = []

    def run(self):
        try:
            while True:
                raw = self.response.readline()
                if not raw:
                    break
                line = raw.decode("utf-8", "replace").rstrip("\r\n")
                if line:
                    self.lines.append(line)
        except Exception:  # noqa: BLE001
            pass


def main():
    admin = login("admin")
    token_a = login("a6_a")
    token_b = login("a6_b")

    print("=" * 78)
    print("① 不带 Authorization 访问 SSE 端点")
    print("=" * 78)
    status, body = api("GET", "/business/notice/stream")
    print(f"  HTTP {status} {json.dumps(body, ensure_ascii=False)[:120]}")
    check("未登录访问 SSE 返回 401", status == 401)

    print()
    print("=" * 78)
    print("② 用 B 的 token 拼 ?user_id=A（试图抢 A 的频道）")
    print("=" * 78)
    conn, response = open_stream(token_b, f"?user_id={USER_A}&userId={USER_A}")
    print(f"  响应：HTTP {response.status}，Content-Type={response.getheader('Content-Type')}")
    reader = Reader(response)
    reader.start()
    time.sleep(0.8)
    status, stats_body = api("GET", "/business/notice/stream/stats", None, admin)
    stats = stats_body["data"]
    print(f"  stats：total_users={stats['total_users']} total_receivers={stats['total_receivers']}")
    for user in stats["users"]:
        print(f"    - {user['user_id']} → receivers={user['receivers']}")
    check("频道归属 B（token 的真实身份）", receivers_of(stats, USER_B) == 1, f"{receivers_of(stats, USER_B)}")
    check("A 的频道没有被 query 参数建出来", receivers_of(stats, USER_A) == 0, f"{receivers_of(stats, USER_A)}")

    print()
    print("=" * 78)
    print("③ 推给 A 的通知不会到达这条（B 的）连接")
    print("=" * 78)
    status, body = api("POST", "/business/notice", {
        "title": "A6 鉴权补充验证：只发给 A",
        "content": "B 的连接不该收到",
        "target_type": "USER",
        "target_ids": [USER_A],
        "channel_mask": 1,
    }, admin)
    print(f"  POST /business/notice → HTTP {status}，notice_id={(body.get('data') or {}).get('notice_id')}")
    time.sleep(1.2)
    events = [line for line in reader.lines if line.startswith("data:")]
    print(f"  B 的连接收到的数据帧数：{len(events)}")
    for line in reader.lines:
        print(f"  B| {line}")
    check("B 的连接一帧都没收到", len(events) == 0, f"{len(events)} 帧")

    conn.close()
    time.sleep(0.5)
    status, stats_body = api("GET", "/business/notice/stream/stats", None, admin)
    final = stats_body["data"]
    print(f"  断开后 stats：total_users={final['total_users']} total_receivers={final['total_receivers']}")
    check("断开后频道表为空", final["total_receivers"] == 0, f"{final['total_receivers']}")

    print()
    if FAILURES:
        print(f"❌ {len(FAILURES)} 条断言失败：{FAILURES}")
        return 1
    print("✅ 鉴权不可穿透：无 token 401、query 不能伪造频道归属、A 的通知不会到 B 的连接")
    return 0


if __name__ == "__main__":
    sys.exit(main())
