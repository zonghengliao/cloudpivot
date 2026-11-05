#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""自签访问令牌 —— 只给「数据修复验收探针」用（P0-FIX-2B-C `t_db2506d2`）。

为什么需要它
------------
生产账号口令已轮换/不可猜（仓库规则：**绝不猜生产口令**），而验收要贴的是
「这个账号以他自己身份请求时，接口返回多少」。登录接口会按库里的授权集签发令牌
（`system/auth/handler.rs::issue_token` → `model::load_principal_snapshot`），
所以只要用**同一套库内真值 + 同一个 JWT 密钥**签一张，权限集合与真登录等价。

它不是后门，边界有三条：
  1. `ver` 必须等于库里的 `sys_user.token_version` —— 会话守卫逐请求比对
     （`middleware/session_guard.rs:1`），改一次角色/改密即整体作废；
  2. 声明了通配权限（`*:*:*`）的令牌会被库侧复核（BUG-SYS-04
     `verify_wildcard_claims`）：库里不是 `SUPER_ADMIN` → 401。
     **所以本工具伪造不出超管**，只能复现「该账号当前真有的权限」；
  3. 密钥只从环境变量读（`NIK_MIDDLEWARE_AUTH_JWT_SECRET`），不落盘、不打印。

用法
----
    # 生产（在 144 上跑，密钥来自 /etc/cloudpivot/cloudpivot.env）
    set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    TOKEN=$(python3 mint_token.py --username laoliu)

    # 隔离副本
    set -a; . /root/superconv-run/iso.env; set +a
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET=...   # 见 start_iso.sh
    TOKEN=$(python3 mint_token.py --username laoliu_test)

输出：一行 JWT 到 stdout；`--json` 时输出 {username, uid, ver, roles, perms, token}（token 已截断）。
"""
import argparse
import base64
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
import uuid

DB = dict(
    host=os.environ.get("NIK_DATABASE_HOST", "127.0.0.1"),
    port=os.environ.get("NIK_DATABASE_PORT", "5432"),
    user=os.environ.get("NIK_DATABASE_USER", "cloudpivot"),
    db=os.environ.get("NIK_DATABASE_NAME", "cloudpivot"),
)


def q(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("NIK_DATABASE_PASSWORD", "")
    r = subprocess.run(
        ["psql", "-h", DB["host"], "-p", str(DB["port"]), "-U", DB["user"],
         "-d", DB["db"], "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise SystemExit("psql 失败：" + r.stderr.strip()[:300])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def b64(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--username", required=True)
    ap.add_argument("--ttl", type=int, default=1800)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    secret = os.environ.get("NIK_MIDDLEWARE_AUTH_JWT_SECRET", "")
    if not secret:
        raise SystemExit("缺 NIK_MIDDLEWARE_AUTH_JWT_SECRET（先 source 环境文件）")

    rows = q(
        "SELECT u.id, u.username, u.token_version FROM sys_user u "
        " WHERE u.username = '%s' AND u.deleted_at IS NULL" % args.username.replace("'", "''"))
    if len(rows) != 1:
        raise SystemExit("账号不存在或重名：%s" % args.username)
    uid, username, ver = rows[0][0], rows[0][1], int(rows[0][2])

    # 与 model::load_roles_uncached 同口径（sys_role 软删/停用由模型层默认过滤）
    roles = [r[0] for r in q(
        "SELECT r.role_code FROM sys_user_role ur JOIN sys_role r ON r.id = ur.role_id "
        " WHERE ur.user_id = '%s' AND (ur.expire_time IS NULL OR ur.expire_time > now()) "
        "   AND r.deleted_at IS NULL AND r.status = 1 ORDER BY r.role_code" % uid)]

    if "SUPER_ADMIN" in roles:
        perms = ["*:*:*"]           # 与 load_principal_snapshot 的超管短路一致
    else:
        # 与 model::load_permissions_uncached 同口径
        perms = [r[0] for r in q(
            "SELECT DISTINCT m.auth_code FROM sys_menu m "
            " JOIN sys_role_menu rm ON rm.menu_id = m.id "
            " JOIN sys_user_role ur ON ur.role_id = rm.role_id "
            " WHERE ur.user_id = '%s' AND m.deleted_at IS NULL AND m.status = 1 "
            "   AND m.auth_code <> '' AND (ur.expire_time IS NULL OR ur.expire_time > now())"
            " ORDER BY 1" % uid)]

    now = int(time.time())
    claims = {
        "jti": str(uuid.uuid4()),
        "sub": uid,
        "username": username,
        "roles": roles,
        "permissions": perms,
        "token_type": "access",
        "exp": now + args.ttl,
        "iat": now,
        "ver": ver,
        "sid": "probe-" + uuid.uuid4().hex[:12],
        "siat": now,
    }
    header = {"typ": "JWT", "alg": "HS256"}
    signing_input = b64(json.dumps(header, separators=(",", ":")).encode()) + "." + \
        b64(json.dumps(claims, separators=(",", ":"), ensure_ascii=False).encode())
    sig = hmac.new(secret.encode(), signing_input.encode(), hashlib.sha256).digest()
    token = signing_input + "." + b64(sig)

    if args.json:
        print(json.dumps({"username": username, "uid": uid, "ver": ver,
                          "roles": roles, "perm_count": len(perms),
                          "token_prefix": token[:24] + "..."}, ensure_ascii=False))
    else:
        print(token)
    return 0


if __name__ == "__main__":
    sys.exit(main())
