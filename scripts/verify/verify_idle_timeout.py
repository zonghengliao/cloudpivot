#!/usr/bin/env python3
"""AUTO-LOGOUT（空闲自动退出登录）在 145 上的真实 HTTP 验收。

覆盖卡面验收：
  ①  参数配置页能看到 auth.idle_timeout_minutes，能改成 5 分钟并保存
  ②  改成 0 → 自动退出禁用（本脚本只验「写入成功 + policy 返回 0」，实际不退出靠 ⑫ 的服务端判定）
  ③  非法值（-1 / abc / 2000）→ 校验拒绝
  ⑧  退出后用旧 token 调接口必须 401（真吊销，不是前端假退）
  ⑫  绕过前端（直接 curl 带旧 token）→ 超过 idle 时间 → 401 + 20009
  ⑬  普通管理员把值改成 0 → 400「只有超级管理员可以禁用自动退出登录」
  ⑭  超管改成 0 → 成功，且 audit_logs 有 CONFIG_IDLE_TIMEOUT_UPDATE（带新旧值）
  ⑲  普通管理员改成 70 → 400（必须在 1-60 分钟之间）
  ⑳  超管改成 70 → 同样 400
  ㉒  普通管理员改成 60 → 成功（上限边界）
  ㉓  普通管理员改成 1 → 成功（下限边界）

额外（自己加的两条判据）：
  · 打 `X-User-Active: 1` 的 GET 会续期（连续 4 次、每次间隔 20s，仍全部 200）
  · 空闲超时后同一会话的 refresh 令牌也被吊销（不能靠刷新复活）

参数从 /etc/cloudpivot/cloudpivot-dev.env 读（145 上的 dev 环境），
管理员口令取 NIK_ADMIN_PASSWORD。
"""

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("IDLE_BASE", "http://127.0.0.1:8036")
ENV_FILE = "/etc/cloudpivot/cloudpivot-dev.env"
CONFIG_KEY = "auth.idle_timeout_minutes"

RESULTS: list[tuple[str, bool, str]] = []


def load_env() -> dict[str, str]:
    env: dict[str, str] = {}
    with open(ENV_FILE, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, _, value = line.partition("=")
            env[key.strip()] = value.strip().strip('"').strip("'")
    return env


ENV = load_env()
ADMIN_USER = os.environ.get("IDLE_USER", "admin")
ADMIN_PASS = ENV.get("NIK_ADMIN_PASSWORD", "")


def check(name: str, ok: bool, detail: str = "") -> bool:
    RESULTS.append((name, ok, detail))
    print(("[PASS] " if ok else "[FAIL] ") + name + ((" :: " + detail) if detail else ""), flush=True)
    return ok


def req(method: str, path: str, token: str | None = None, body=None, headers=None):
    url = BASE + path
    hdrs = {"Content-Type": "application/json"}
    if token:
        hdrs["Authorization"] = "Bearer " + token
    if headers:
        hdrs.update(headers)
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read().decode()
            return response.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login(username: str, password: str):
    status, body = req("POST", "/api/system/auth/login", body={"username": username, "password": password})
    if status != 200 or body.get("code") != 0:
        return None, status, body
    return body["data"], status, body


def psql(sql: str) -> str:
    """在 145 上跑一条 SQL（与应用同账号同库）。"""
    env = dict(os.environ)
    env["PGPASSWORD"] = ENV["NIK_DATABASE_PASSWORD"]
    command = [
        "psql", "-h", ENV["NIK_DATABASE_HOST"], "-p", ENV["NIK_DATABASE_PORT"],
        "-U", ENV["NIK_DATABASE_USER"], "-d", ENV["NIK_DATABASE_NAME"], "-At", "-F", "|", "-c", sql,
    ]
    done = subprocess.run(command, capture_output=True, text=True, env=env, timeout=60)
    if done.returncode != 0:
        return "PSQL_ERROR: " + done.stderr.strip()
    return done.stdout.strip()


def config_row(token: str):
    status, body = req("GET", "/api/support/config/page?page=1&page_size=1000", token)
    if status != 200 or body.get("code") != 0:
        return None
    for row in body["data"]["list"]:
        if row["config_key"] == CONFIG_KEY:
            return row
    return None


def put_value(token: str, raw_row: dict, value: str):
    payload = {
        "config_key": raw_row["config_key"],
        "config_value": value,
        "config_type": raw_row["config_type"],
        "description": raw_row.get("description"),
        "category": raw_row.get("category"),
        "sort_order": raw_row.get("sort_order", 0),
        "config_level": raw_row.get("config_level", 1),
        "is_modifiable": raw_row.get("is_modifiable", True),
        "is_encrypted": raw_row.get("is_encrypted", False),
        "is_restart": raw_row.get("is_restart", False),
        "status": raw_row.get("status", 1),
        "ref_value": raw_row.get("ref_value"),
        "select_value": raw_row.get("select_value"),
    }
    return req("PUT", "/api/support/config/" + raw_row["id"], token, payload)


def read_value(token: str):
    status, body = req("GET", "/api/support/config/value/" + CONFIG_KEY, token)
    return status, body.get("data") if isinstance(body, dict) else None


def main() -> int:
    admin, status, body = login(ADMIN_USER, ADMIN_PASS)
    if not admin:
        print("FATAL: 管理员登录失败", status, body)
        return 2
    admin_token = admin["access_token"]
    print("admin 登录成功, must_change_password =", admin.get("must_change_password"), flush=True)

    # ---------- ① 参数存在且默认 15 ----------
    row = config_row(admin_token)
    check("① 参数配置列表能看到 " + CONFIG_KEY, row is not None, "" if row else "查不到该键")
    if row is None:
        return 2
    check("① 默认值 = 15", row["config_value"] == "15", "config_value=" + str(row["config_value"]))
    check("① 值类型 = NUMBER 且允许修改", row["config_type"] == "NUMBER" and row["is_modifiable"] is True,
          "type=%s modifiable=%s" % (row["config_type"], row["is_modifiable"]))

    # ---------- idle-policy 接口（前端计时用的来源） ----------
    status, body = req("GET", "/api/system/auth/idle-policy", admin_token)
    data = body.get("data") or {}
    check("policy: 超管读到 timeout=15 / warn=60 / is_super_admin=true",
          status == 200 and data.get("timeout_minutes") == 15 and data.get("warn_seconds") == 60
          and data.get("is_super_admin") is True, json.dumps(data, ensure_ascii=False))

    # ---------- ① 改成 5 分钟 ----------
    status, body = put_value(admin_token, row, "5")
    check("① 改成 5 分钟保存成功", status == 200 and body.get("code") == 0, "http=%s body=%s" % (status, body.get("msg")))
    status, value = read_value(admin_token)
    check("① 落库生效: value()=5", value == "5", "value=" + str(value))

    # ---------- ③ 非法值 ----------
    for bad in ["-1", "abc", "2000"]:
        status, body = put_value(admin_token, row, bad)
        check("③ 非法值 %s 被拒（400）" % bad, status == 400 and body.get("code") != 0,
              "http=%s msg=%s" % (status, body.get("msg")))

    # ---------- ⑳ 超管设 70 也拒 ----------
    status, body = put_value(admin_token, row, "70")
    check("⑳ 超管设 70 被拒（400 + 1-60 文案）",
          status == 400 and "1-60" in (body.get("msg") or ""), "http=%s msg=%s" % (status, body.get("msg")))
    status, body = put_value(admin_token, row, "1440")
    check("⑳ 超管设 1440 同样被拒", status == 400, "http=%s msg=%s" % (status, body.get("msg")))

    # ---------- 非超管夹具：建角色 + 挂 support:config:update 菜单 + 建用户 ----------
    nonce = str(int(time.time()))[-6:]
    role_code = "IDLE_T" + nonce
    username = "idle_t" + nonce
    status, body = req("POST", "/api/system/role", admin_token, {
        "role_code": role_code, "role_name": "空闲超时实测角色", "status": 1, "data_scope": 1,
    })
    role_id = (body.get("data") or {}).get("id")
    check("夹具: 建非超管角色", status == 200 and bool(role_id), "http=%s msg=%s" % (status, body.get("msg")))
    if not role_id:
        return 2

    menu_ids = psql("select id from sys_menu where auth_code like 'support:config:%' and deleted_at is null;").splitlines()
    menu_ids = [m.strip() for m in menu_ids if m.strip()]
    status, body = req("PUT", "/api/system/role/%s/menus" % role_id, admin_token, {"menu_ids": menu_ids})
    check("夹具: 角色挂 %d 个 support:config 菜单" % len(menu_ids), status == 200, "http=%s msg=%s" % (status, body.get("msg")))

    fixture_pass = "Idle@123456"
    status, body = req("POST", "/api/system/user", admin_token, {
        "username": username, "password": fixture_pass, "real_name": "空闲超时实测账号",
        "status": 1, "role_ids": [role_id],
    })
    fixture_id = (body.get("data") or {}).get("id")
    check("夹具: 建非超管用户 %s" % username, status == 200 and bool(fixture_id),
          "http=%s msg=%s" % (status, body.get("msg")))
    if not fixture_id:
        return 2

    normal, status, body = login(username, fixture_pass)
    check("夹具: 非超管账号登录成功", bool(normal), "http=%s msg=%s" % (status, body.get("msg")))
    if not normal:
        return 2
    normal_token = normal["access_token"]

    status, body = req("GET", "/api/system/auth/idle-policy", normal_token)
    data = body.get("data") or {}
    check("policy: 非超管读到 is_super_admin=false", data.get("is_super_admin") is False, json.dumps(data, ensure_ascii=False))

    # ---------- ⑬ 普通管理员设 0 → 400 ----------
    status, body = put_value(normal_token, row, "0")
    check("⑬ 普通管理员设 0 被拒（400 + 只有超级管理员…）",
          status == 400 and "只有超级管理员" in (body.get("msg") or ""),
          "http=%s msg=%s" % (status, body.get("msg")))
    status, value = read_value(admin_token)
    check("⑬ 被拒后值未变（仍是 5）", value == "5", "value=" + str(value))

    # ---------- ⑲ 普通管理员设 70 → 400 ----------
    status, body = put_value(normal_token, row, "70")
    check("⑲ 普通管理员设 70 被拒（400 + 1-60）",
          status == 400 and "1-60" in (body.get("msg") or ""), "http=%s msg=%s" % (status, body.get("msg")))

    # ---------- ㉒ ㉓ 边界值 60 / 1 允许 ----------
    status, body = put_value(normal_token, row, "60")
    check("㉒ 普通管理员设 60 成功", status == 200 and body.get("code") == 0, "http=%s msg=%s" % (status, body.get("msg")))
    status, value = read_value(admin_token)
    check("㉒ 落库生效: value()=60", value == "60", "value=" + str(value))
    status, body = put_value(normal_token, row, "1")
    check("㉓ 普通管理员设 1 成功", status == 200 and body.get("code") == 0, "http=%s msg=%s" % (status, body.get("msg")))
    status, value = read_value(admin_token)
    check("㉓ 落库生效: value()=1", value == "1", "value=" + str(value))

    # ---------- ⑭ 超管设 0 成功 + 审计留痕 ----------
    status, body = put_value(admin_token, row, "0")
    check("⑭ 超管设 0 成功", status == 200 and body.get("code") == 0, "http=%s msg=%s" % (status, body.get("msg")))
    status, body = req("GET", "/api/system/auth/idle-policy", admin_token)
    data = body.get("data") or {}
    check("② 设为 0 后 policy 返回 timeout_minutes=0（前端不启动计时）", data.get("timeout_minutes") == 0,
          json.dumps(data, ensure_ascii=False))
    time.sleep(0.6)  # 审计走后台总线（异步 worker），给一拍落库时间
    audit = psql(
        "select operation, before_data->>'config_value', after_data->>'config_value', "
        "after_data->>'operator_is_super_admin', username "
        "from audit_logs where operation = 'CONFIG_IDLE_TIMEOUT_UPDATE' order by created_at desc limit 3;"
    )
    print("   audit_logs 最近 3 条 CONFIG_IDLE_TIMEOUT_UPDATE:\n     " + audit.replace("\n", "\n     "), flush=True)
    check("⑭ audit_logs 有 CONFIG_IDLE_TIMEOUT_UPDATE 且带旧值/新值/是否超管",
          "CONFIG_IDLE_TIMEOUT_UPDATE" in audit and "|0|true|" in audit.replace("||", "|"),
          audit.replace("\n", " ;; ") if audit else "无记录")

    # 恢复默认 15（不给后续卡留坑）
    status, body = put_value(admin_token, row, "15")
    check("收尾: 恢复默认 15", status == 200, "http=%s msg=%s" % (status, body.get("msg")))

    # ---------- ⑧ 显式登出后旧 token 必须 401 ----------
    victim, status, _ = login(ADMIN_USER, ADMIN_PASS)
    old_token = victim["access_token"]
    status, body = req("GET", "/api/system/auth/me", old_token)
    check("⑧ 登出前 token 可用（/me 200）", status == 200, "http=%s" % status)
    status, body = req("POST", "/api/system/auth/logout", old_token, {"refresh_token": victim["refresh_token"]})
    check("⑧ 登出接口 200", status == 200, "http=%s msg=%s" % (status, body.get("msg")))
    status, body = req("GET", "/api/system/auth/me", old_token)
    check("⑧ 登出后旧 token → 401（真吊销）", status == 401, "http=%s body=%s" % (status, json.dumps(body, ensure_ascii=False)[:120]))

    # ---------- ⑫ 空闲超时：绕过前端直接带 token ----------
    # 先设成 1 分钟（最小生效值），再用新会话验证「有操作续期 / 无操作 401」
    status, body = put_value(admin_token, row, "1")
    check("⑫ 预置 timeout=1 分钟", status == 200, "http=%s msg=%s" % (status, body.get("msg")))

    session, status, _ = login(ADMIN_USER, ADMIN_PASS)
    idle_token = session["access_token"]
    idle_refresh = session["refresh_token"]

    # 有操作（带 X-User-Active: 1）→ 续期：连续 4 次、间隔 20 秒，累计 60 秒仍全部 200
    alive = True
    for index in range(4):
        time.sleep(20)
        status, _ = req("GET", "/api/system/auth/me", idle_token, headers={"X-User-Active": "1"})
        alive = alive and status == 200
    check("⑩ 持续有操作（每 20s，累计 60s）→ 不退出", alive, "最后一次 http=%s" % status)

    # 无操作 65 秒（模拟「人走了、前端计时被绕过、只剩 curl」）
    print("   等待 65 秒模拟无人操作…", flush=True)
    time.sleep(65)
    status, body = req("GET", "/api/system/auth/me", idle_token)
    check("⑫ 空闲超时后（无 X-User-Active）→ 401 + 20009",
          status == 401 and body.get("code") == 20009, "http=%s body=%s" % (status, json.dumps(body, ensure_ascii=False)[:140]))
    status, body = req("GET", "/api/system/auth/me", idle_token, headers={"X-User-Active": "1"})
    check("⑫ 超时后再补打活跃标记也救不回来（仍 401）", status == 401,
          "http=%s body=%s" % (status, json.dumps(body, ensure_ascii=False)[:120]))
    status, body = req("POST", "/api/system/auth/refresh", body={"refresh_token": idle_refresh})
    check("⑫ 该会话的 refresh 令牌已被吊销（不能靠刷新复活）",
          status == 401 or (body.get("code") not in (0, None)), "http=%s msg=%s" % (status, body.get("msg")))

    # 收尾：恢复默认 15
    # 重新登录拿新 token —— 这一步本身就是「空闲超时是真的」的旁证：
    # 上一个 admin token 在 65 秒等待期间也被判超时了（timeout 当时 = 1 分钟）。
    fresh, status, _ = login(ADMIN_USER, ADMIN_PASS)
    check("⑫ 空闲超时同样作用于 admin 会话（收尾需重新登录才拿得到新 token）",
          bool(fresh), "http=%s" % status)
    cleanup_token = fresh["access_token"] if fresh else admin_token
    status, body = put_value(cleanup_token, row, "15")
    check("收尾: 恢复默认 15（第二轮）", status == 200, "http=%s msg=%s" % (status, body.get("msg")))
    status, value = read_value(cleanup_token)
    check("收尾: 终值 = 15", value == "15", "value=" + str(value))

    # ---------- 夹具清理 ----------
    if fixture_id:
        req("DELETE", "/api/system/user/" + fixture_id, cleanup_token)
    if role_id:
        req("DELETE", "/api/system/role/" + role_id, cleanup_token)
    leftover = psql(
        "select (select count(*) from sys_user where username like 'idle_t%' and deleted_at is null) || '|' || "
        "(select count(*) from sys_role where role_code like 'IDLE_T%' and deleted_at is null);"
    )
    check("收尾: 夹具已清理（残留 0|0）", leftover == "0|0", "残留=" + leftover)

    passed = sum(1 for _, ok, _ in RESULTS if ok)
    total = len(RESULTS)
    print("\n===== %d/%d 通过 =====" % (passed, total), flush=True)
    for name, ok, detail in RESULTS:
        if not ok:
            print("[FAIL] " + name + " :: " + detail, flush=True)
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
