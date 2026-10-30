#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""A6 SSE 实时推送 —— 三条硬指标的实测脚本（只在 cloudpivot_test 上跑）。

用法（在 172.18.10.144 上，先起 test 实例到 8026）：
    python3 /root/work/a6/acceptance.py

三条硬指标：
  ① per-user 隔离：A、B 两个 SSE 客户端同时在线，只给 A 发通知 → 只有 A 收到；
  ② 断连清理：A 断开后 `/stream/stats` 里 A 消失（receiver_count 1 → 0），
     且服务端日志出现「receiver_count 归零，频道已回收」；
  ③ 通道未配置记 SKIPPED：模板只配 INBOX，投递 EMAIL → 投递记录 status=SKIPPED（不是失败）。

额外留证：心跳注释帧 `: keep-alive`、B 不受 A 断连影响、`dispatch` 返回的通道汇总。
"""

import http.client
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

PORT = 8026
BASE = f"http://127.0.0.1:{PORT}/api"
ENV_FILE = "/etc/cloudpivot/cloudpivot-test.env"
SERVER_LOG = "/var/tmp/a6-run/server.log"

USER_A = "a6000000-0000-0000-0000-0000000000a1"
USER_B = "a6000000-0000-0000-0000-0000000000b1"
TEMPLATE_CODE = "A6_INBOX_ONLY"

FAILURES = []


# ---------- 基础设施 ----------


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
PG = [
    "psql", "-h", "127.0.0.1", "-U", ENV["NIK_DATABASE_USER"], "-d", "cloudpivot_test",
    "-v", "ON_ERROR_STOP=1", "-tA", "-X",
]


def sql(query):
    proc = subprocess.run(
        PG + ["-c", query],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": ENV["NIK_DATABASE_PASSWORD"]},
    )
    if proc.returncode != 0:
        raise RuntimeError(f"SQL 失败: {query}\n{proc.stderr}")
    return (proc.stdout or "").strip()


def sql_file(path):
    proc = subprocess.run(
        PG + ["-f", path],
        capture_output=True,
        text=True,
        env={**os.environ, "PGPASSWORD": ENV["NIK_DATABASE_PASSWORD"]},
    )
    if proc.returncode != 0:
        raise RuntimeError(f"SQL 脚本失败: {path}\n{proc.stderr}")
    return (proc.stdout or "").strip()


def api(method, path, body=None, token=None, timeout=30):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read().decode()
            return response.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username, password):
    status, body = api("POST", "/system/auth/login", {"username": username, "password": password})
    if status != 200 or body.get("code") != 0:
        raise RuntimeError(f"登录失败 {username}: {status} {body}")
    data = body["data"]
    return data.get("access_token") or data.get("token")


def banner(title):
    print()
    print("=" * 78)
    print(title)
    print("=" * 78)


def check(label, ok, detail=""):
    print(f"  [{'PASS' if ok else 'FAIL'}] {label}{('：' + detail) if detail else ''}")
    if not ok:
        FAILURES.append(label)
    return ok


# ---------- SSE 客户端 ----------


class SseClient(threading.Thread):
    """一个真实的长连接客户端：走 HTTP + Authorization 头（与浏览器 fetch 同路径）。"""

    def __init__(self, label, token):
        super().__init__(daemon=True)
        self.label = label
        self.token = token
        self.lines = []
        self.events = []
        self.status = None
        self.content_type = None
        self.error = None
        self.connected = threading.Event()
        self._conn = None

    def run(self):
        try:
            conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=90)
            conn.request(
                "GET",
                "/api/business/notice/stream",
                headers={
                    "Authorization": f"Bearer {self.token}",
                    "Accept": "text/event-stream",
                },
            )
            response = conn.getresponse()
            self._conn = conn
            self.status = response.status
            self.content_type = response.getheader("Content-Type")
            self.connected.set()
            while True:
                raw = response.readline()
                if not raw:
                    break
                line = raw.decode("utf-8", "replace").rstrip("\r\n")
                if not line:
                    continue
                self.lines.append(line)
                if line.startswith("data:"):
                    self.events.append(line[5:].strip())
        except Exception as error:  # noqa: BLE001 - 断连就是这里的正常出口
            self.error = repr(error)
        finally:
            self.connected.set()

    def close(self):
        try:
            if self._conn is not None:
                self._conn.close()
        except Exception:  # noqa: BLE001
            pass


def wait_for(predicate, timeout=8.0, interval=0.2):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


def stats(token):
    status, body = api("GET", "/business/notice/stream/stats", None, token)
    if status != 200 or body.get("code") != 0:
        raise RuntimeError(f"读取 SSE 统计失败: {status} {body}")
    return body["data"]


def receivers_of(stats_data, user_id):
    for user in stats_data["users"]:
        if user["user_id"] == user_id:
            return user["receivers"]
    return 0


# ---------- ① 造数据 ----------


def seed():
    banner("① 造数据（cloudpivot_test）：两个可登录用户（SUPER_ADMIN）+ 只配 INBOX 的模板")
    script = f"""
BEGIN;
DELETE FROM sys_notice_receiver WHERE user_id IN ('{USER_A}'::uuid, '{USER_B}'::uuid);
DELETE FROM sys_user_role WHERE user_id IN ('{USER_A}'::uuid, '{USER_B}'::uuid);
DELETE FROM sys_user WHERE id IN ('{USER_A}'::uuid, '{USER_B}'::uuid);
-- 口令直接复用 admin 的哈希（测试库专用，避免走「管理员重置口令」把 must_change_password 置真）
INSERT INTO sys_user (id, username, password, real_name, email, mobile, status, must_change_password)
SELECT '{USER_A}'::uuid, 'a6_a', u.password, 'A6 用户A', 'a6_a@example.com', '13900000001', 1, FALSE
FROM sys_user u WHERE u.username = 'admin';
INSERT INTO sys_user (id, username, password, real_name, email, mobile, status, must_change_password)
SELECT '{USER_B}'::uuid, 'a6_b', u.password, 'A6 用户B', 'a6_b@example.com', '13900000002', 1, FALSE
FROM sys_user u WHERE u.username = 'admin';
-- SUPER_ADMIN 短路授权（SSE 端点权限码 business:notice:mine 对超管全通）
INSERT INTO sys_user_role (user_id, role_id)
SELECT v.uid::uuid, r.id FROM (VALUES ('{USER_A}'), ('{USER_B}')) AS v(uid), sys_role r
WHERE r.role_code = 'SUPER_ADMIN';
-- 通道未配置（本卡验收③要求 EMAIL 落 SKIPPED，而不是 PENDING）
DELETE FROM sys_config WHERE config_key IN ('notice.email.enabled', 'notice.webhook.url', 'notice.sms.enabled');
-- 模板：**只配 INBOX**
DELETE FROM sys_notice WHERE template_code = '{TEMPLATE_CODE}';
DELETE FROM sys_notice_template WHERE template_code = '{TEMPLATE_CODE}';
COMMIT;
"""
    with open("/tmp/a6_seed.sql", "w", encoding="utf-8") as handle:
        handle.write(script)
    sql_file("/tmp/a6_seed.sql")

    print(sql(
        "SELECT 'A=' || a.id || '（' || a.username || '，status=' || a.status || '） B=' || b.id "
        "|| '（' || b.username || '）' FROM sys_user a, sys_user b "
        f"WHERE a.id = '{USER_A}'::uuid AND b.id = '{USER_B}'::uuid"
    ))
    print("通道配置：" + sql(
        "SELECT coalesce(string_agg(config_key || '=' || coalesce(config_value, ''), ', '), '(无 → 所有非站内信通道未配置)') "
        "FROM sys_config WHERE config_key LIKE 'notice.%'"
    ))


# ---------- ①② 隔离 + 断连 ----------


def sse_checks(admin_token):
    banner("② 登录 A / B 并同时建立 SSE 长连接（Authorization 头，走现有 AuthLayer）")
    password = ENV["NIK_ADMIN_PASSWORD"]
    token_a = login("a6_a", password)
    token_b = login("a6_b", password)
    print(f"  A 的 token 前缀：{token_a[:20]}…")
    print(f"  B 的 token 前缀：{token_b[:20]}…")

    client_a = SseClient("A", token_a)
    client_b = SseClient("B", token_b)
    client_a.start()
    client_b.start()
    if not (client_a.connected.wait(5) and client_b.connected.wait(5)):
        raise RuntimeError("SSE 连接未建立")
    time.sleep(0.6)  # 等 handler 完成 subscribe（stats 立刻可读）
    print(f"  A 响应：HTTP {client_a.status}，Content-Type={client_a.content_type}")
    print(f"  B 响应：HTTP {client_b.status}，Content-Type={client_b.content_type}")

    before = stats(admin_token)
    print(f"  连接后 stats：total_users={before['total_users']} total_receivers={before['total_receivers']}")
    for user in before["users"]:
        print(f"    - {user['user_id']} → receivers={user['receivers']}")
    check("A 的 receiver_count = 1", receivers_of(before, USER_A) == 1, f"实测 {receivers_of(before, USER_A)}")
    check("B 的 receiver_count = 1", receivers_of(before, USER_B) == 1, f"实测 {receivers_of(before, USER_B)}")

    banner("③ 硬指标①：只给 A 发通知 → 只有 A 收到")
    status, body = api(
        "POST",
        "/business/notice",
        {
            "notice_type": "SYSTEM",
            "level": "INFO",
            "title": "A6 隔离验证：只发给 A",
            "content": "这条通知的接收人只有 A，B 的频道不该有任何帧",
            "target_type": "USER",
            "target_ids": [USER_A],
            "channel_mask": 1,
        },
        admin_token,
    )
    print(f"  POST /business/notice → HTTP {status}，响应：{json.dumps(body, ensure_ascii=False)[:300]}")
    notice_id_a = (body.get("data") or {}).get("notice_id")
    check("发送接口返回 200", status == 200 and body.get("code") == 0)

    wait_for(lambda: len(client_a.events) >= 1, timeout=8)
    time.sleep(1.2)  # 给「本不该到达」的帧留出串门时间

    print("\n  --- A 实际收到的原始帧 ---")
    for line in client_a.lines:
        print(f"  A| {line}")
    print("  --- B 实际收到的原始帧 ---")
    if client_b.lines:
        for line in client_b.lines:
            print(f"  B| {line}")
    else:
        print("  B| （无任何帧 —— 隔离成立）")

    check("A 收到 1 条事件", len(client_a.events) == 1, f"实际 {len(client_a.events)} 条")
    check(
        "A 收到的正是给 A 的那条",
        len(client_a.events) >= 1 and "只发给 A" in client_a.events[0],
        client_a.events[0][:120] if client_a.events else "(空)",
    )
    check("B 一条事件都没收到", len(client_b.events) == 0, f"实际 {len(client_b.events)} 条")
    if client_a.events:
        payload = json.loads(client_a.events[0])
        check(
            "载荷带该用户自己的 receiver_id",
            payload.get("receiver_id") not in (None, ""),
            payload.get("receiver_id", ""),
        )
        check("载荷 notice_id 与发送返回一致", payload.get("notice_id") == notice_id_a)

    banner("④ 心跳：等待服务端 `: keep-alive` 注释帧（间隔 15 s）")
    started = time.time()
    got_keepalive = wait_for(
        lambda: any(line.strip() == ": keep-alive" for line in client_a.lines),
        timeout=20,
        interval=0.5,
    )
    print(f"  等待 {time.time() - started:.1f} s 后采集到的心跳帧："
          + (": keep-alive" if got_keepalive else "（未采集到）"))
    check("采集到 : keep-alive 注释帧", got_keepalive)

    banner("⑤ 硬指标②：A 断开 → receiver_count 1 → 0，频道被回收")
    client_a.close()
    disappeared = wait_for(lambda: receivers_of(stats(admin_token), USER_A) == 0, timeout=8)
    after = stats(admin_token)
    print("  断开后 stats：")
    print(f"    total_users={after['total_users']} total_receivers={after['total_receivers']} "
          f"subscribe_total={after['subscribe_total']} published_total={after['published_total']} "
          f"delivered_total={after['delivered_total']}")
    for user in after["users"]:
        print(f"    - {user['user_id']} → receivers={user['receivers']}")
    print(sql(
        "SELECT 'SQL 侧接收行仍在（SSE 只影响内存频道，不动库）：' || count(*) "
        f"FROM sys_notice_receiver WHERE notice_id = '{notice_id_a}'::uuid"
    ))
    check("A 的 receiver_count 归零", receivers_of(after, USER_A) == 0, f"实测 {receivers_of(after, USER_A)}")
    check("A 已从频道表中消失", all(u["user_id"] != USER_A for u in after["users"]))
    check("B 仍在在线（不受 A 断连影响）", receivers_of(after, USER_B) == 1, f"实测 {receivers_of(after, USER_B)}")

    banner("⑥ B 断连后的收尾 + 服务端日志（断连清理的原始日志）")
    client_b.close()
    wait_for(lambda: receivers_of(stats(admin_token), USER_B) == 0, timeout=8)
    final = stats(admin_token)
    print(f"  最终 stats：total_users={final['total_users']} total_receivers={final['total_receivers']}")
    print("  服务端日志（grep SSE）：")
    log_lines = grep_log(["SSE 订阅建立", "receiver_count 归零", "SSE 订阅请求已受理"])
    for line in log_lines:
        print(f"    {line}")
    check(
        "日志出现「receiver_count 归零，频道已回收」",
        any("receiver_count 归零" in line for line in log_lines),
    )
    check("全部连接断开后频道表为空", final["total_receivers"] == 0, f"实测 {final['total_receivers']}")


def grep_log(keywords, limit=40):
    try:
        with open(SERVER_LOG, encoding="utf-8", errors="replace") as handle:
            lines = handle.readlines()
    except FileNotFoundError:
        return []
    # tracing 带 ANSI 颜色码（重定向到文件后仍在），归档证据前先剥掉
    ansi = re.compile(r"\x1b\[[0-9;]*m")
    hit = [ansi.sub("", line).rstrip() for line in lines if any(key in line for key in keywords)]
    return hit[-limit:]


# ---------- ③ 通道 SKIPPED ----------


def channel_checks(admin_token, token_a):
    banner("⑦ 硬指标③：模板只配 INBOX + 投递 EMAIL → 记录 SKIPPED（不是失败）")
    status, body = api(
        "POST",
        "/business/notice/template",
        {
            "template_code": TEMPLATE_CODE,
            "template_name": "A6 只配站内信的模板",
            "notice_type": "SYSTEM",
            "default_level": "INFO",
            "title_template": "模板通知：{{name}} 的待办",
            "content_template": "正文：{{name}} 请处理",
            "content_format": "TEXT",
            "channels": "INBOX",
            "variables": [{"name": "name", "label": "姓名", "sample": "张三"}],
        },
        admin_token,
    )
    print(f"  建模板（channels=INBOX）→ HTTP {status}：{json.dumps(body, ensure_ascii=False)[:200]}")
    check("模板创建成功", status == 200 and body.get("code") == 0)
    print("  DB 中的模板通道：" + sql(
        f"SELECT 'channels=' || channels FROM sys_notice_template WHERE template_code = '{TEMPLATE_CODE}'"
    ))

    before_publish = stats(admin_token)["published_total"]
    status, body = api(
        "POST",
        "/business/notice",
        {
            "template_code": TEMPLATE_CODE,
            "template_vars": {"name": "张三"},
            "target_type": "USER",
            "target_ids": [USER_A],
            "channel_mask": 2,
        },
        admin_token,
    )
    after_publish = stats(admin_token)["published_total"]
    data = body.get("data") or {}
    print(f"  发通知（channel_mask=2 → EMAIL）→ HTTP {status}")
    print("  响应：" + json.dumps(body, ensure_ascii=False)[:400])
    check("发送接口 200（未配置通道不导致失败）", status == 200 and body.get("code") == 0)

    channels = {item["channel"]: item for item in data.get("channels", [])}
    for name in sorted(channels):
        print(f"    - 通道 {name}：status={channels[name]['status']} rows={channels[name]['rows']}")
    check("INBOX 为 DELIVERED", channels.get("INBOX", {}).get("status") == "DELIVERED")
    check("EMAIL 为 SKIPPED", channels.get("EMAIL", {}).get("status") == "SKIPPED")

    notice_id = data.get("notice_id")
    print("\n  通知主体（生效掩码 = INBOX|EMAIL = 3）：")
    print("  " + sql(
        "SELECT notice_no || ' | channel_mask=' || channel_mask || ' | title=' || title "
        f"FROM sys_notice WHERE id = '{notice_id}'::uuid"
    ))
    print("  投递记录（DB 实查 sys_notice_delivery）：")
    print(sql(
        "SELECT channel || ' | ' || status || ' | rows=' || count(*) FROM sys_notice_delivery "
        f"WHERE notice_id = '{notice_id}'::uuid GROUP BY channel, status ORDER BY channel"
    ))
    failed = int(sql(
        f"SELECT count(*) FROM sys_notice_delivery WHERE notice_id = '{notice_id}'::uuid AND status = 'FAILED'"
    ))
    email_rows = int(sql(
        "SELECT count(*) FROM sys_notice_delivery "
        f"WHERE notice_id = '{notice_id}'::uuid AND channel = 'EMAIL' AND status = 'SKIPPED'"
    ))
    check("EMAIL 投递记录 status=SKIPPED", email_rows >= 1, f"{email_rows} 行")
    check("没有任何 FAILED 记录", failed == 0, f"FAILED={failed}")

    # A 已断连：这条通知不会推到任何频道（离线语义）
    print("  离线语义（A 已断连，只落库不推送）：")
    print(f"    published_total：{before_publish} → {after_publish}（无在线订阅端，未发生推送）")
    println_sql = sql(
        "SELECT '接收行=' || count(*) || '，未读=' || count(*) FILTER (WHERE read_flag = FALSE) "
        f"FROM sys_notice_receiver WHERE notice_id = '{notice_id}'::uuid"
    )
    print("    " + println_sql)
    check("离线用户不消耗推送（published_total 未增长）", before_publish == after_publish)
    status_unread, unread = api("GET", "/business/notice/unread-count", None, token_a)
    print(f"  A 的未读数接口：HTTP {status_unread} → {json.dumps(unread, ensure_ascii=False)[:120]}")


def main():
    admin_token = login("admin", ENV["NIK_ADMIN_PASSWORD"])
    print("admin 登录成功，token 前缀：", admin_token[:20], "…")
    seed()
    sse_checks(admin_token)
    token_a = login("a6_a", ENV["NIK_ADMIN_PASSWORD"])
    channel_checks(admin_token, token_a)

    banner("结论")
    if FAILURES:
        print(f"❌ {len(FAILURES)} 条断言失败：")
        for item in FAILURES:
            print(f"  - {item}")
        return 1
    print("✅ 三条硬指标全部通过（隔离帧 / 计数归零 / SKIPPED 记录），详见上方逐条输出")
    return 0


if __name__ == "__main__":
    sys.exit(main())
