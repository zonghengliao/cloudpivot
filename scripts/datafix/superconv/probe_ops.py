#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""OPS_ADMIN 运维动作清单探针（【P0-FIX-2B-C】t_db2506d2 · 验收 C3）。

它回答一个问题：**摘掉 SUPER_ADMIN 之后，laoliu 还能不能干运维的活？**

清单（卡面 C3 逐条）：
  1. `GET /api/system/user/page`     （system:user:list）   —— 且 total 必须是**全量** sys_user 行数
  2. `GET /api/system/role/page`     （system:role:list）
  3. `GET /api/system/dept/tree`     （system:dept:list）
  4. `GET /api/system/menu/tree`     （system:menu:list）
  5. `GET /api/support/config/page`  （support:config:list）
  6. `GET /api/support/log/page`     （support:log:list）
  7. 写操作：`POST /api/support/config` + `DELETE /api/support/config/{id}`（建完即删，净零）
  8. 反向断言：`GET /api/business/erp/account/page` 必须 **403**
     —— 证明「业务数据的边界靠权限码收，不靠行级范围收」（OPS_ADMIN 的 data_scope=1 不是业务后门）
  9. `/api/system/auth/me` 里 roles 必须含 OPS_ADMIN、**不含 SUPER_ADMIN**

两种取票方式（都要，生产用第 2 种）：
  A. 真登录：`--username laoliu --password ***`（隔离副本里用；生产口令已轮换且不猜）
  B. 已有令牌：`--token ***`
  令牌里的权限码快照就是「登录时会从库里装出来的那套」（auth/model.rs::load_permissions_uncached
  = sys_user_role ⋈ sys_role_menu ⋈ sys_menu），所以自签令牌与真登录在**权限码集合**上等价；
  但 SUPER_ADMIN 的通配短路另有库侧复核（middleware/session_guard.rs::verify_wildcard_claims），
  自签的超管令牌会被 401 —— 本探针因此**不能**用自签令牌伪造超管，只能验证「非超管能做什么」。

用法：
    SUPERCONV_BASE=http://127.0.0.1:18097 python3 probe_ops.py --username laoliu --password '***'
    SUPERCONV_BASE=http://127.0.0.1:8023 python3 probe_ops.py --token '***'

退出码：0 = 清单全过；1 = 有一条失败（调用方必须停手、回退绑定，不许「先摘了再说」）。
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("SUPERCONV_BASE", "http://127.0.0.1:8023").rstrip("/")
TIMEOUT = 30


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
            text = resp.read().decode(errors="replace")
            return resp.status, (json.loads(text) if text.strip() else {})
    except urllib.error.HTTPError as exc:
        text = exc.read().decode(errors="replace")
        try:
            return exc.code, (json.loads(text) if text.strip() else {})
        except json.JSONDecodeError:
            return exc.code, {"raw": text[:300]}


class Probe:
    def __init__(self):
        self.rows = []
        self.failed = 0

    def record(self, name, method, path, st, body, ok, note=""):
        code = body.get("code") if isinstance(body, dict) else None
        msg = (body.get("message") or body.get("msg")) if isinstance(body, dict) else None
        flag = "PASS" if ok else "FAIL"
        if not ok:
            self.failed += 1
        print(f"  [{flag}] {name:<28} {method:<6} {path:<34} http={st} code={code}"
              + (f" msg={msg}" if msg else "") + (f" · {note}" if note else ""))
        self.rows.append({"name": name, "method": method, "path": path,
                          "http": st, "code": code, "ok": ok, "note": note})

    def get_ok(self, name, path, token, expect_status=200, note=""):
        st, body = call("GET", path, token=token)
        ok = st == expect_status and (expect_status != 200 or body.get("code") == 0)
        self.record(name, "GET", path, st, body, ok, note)
        return body

    def get_expect_403(self, name, path, token, note=""):
        st, body = call("GET", path, token=token)
        ok = st == 403
        self.record(name, "GET", path, st, body, ok, note)
        return body


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--username")
    ap.add_argument("--password")
    ap.add_argument("--token")
    ap.add_argument("--expect-user-total", type=int,
                    help="sys_user 全量的期望行数（OPS_ADMIN 的 data_scope=1 必须看到全部）；不给则只看 total>1")
    args = ap.parse_args()

    p = Probe()
    print(f"== OPS_ADMIN 运维动作清单 · {BASE} ==")

    token = args.token
    if not token:
        if not (args.username and args.password):
            print("需要 --token 或 --username/--password", file=sys.stderr)
            return 2
        st, body = call("POST", "/api/system/auth/login",
                        {"username": args.username, "password": args.password})
        token = (body.get("data") or {}).get("access_token") if st == 200 else None
        p.record("登录", "POST", "/api/system/auth/login", st, body, bool(token))
        if not token:
            print("✗ 登录失败，后续清单无法执行", file=sys.stderr)
            return 1

    # ---- 身份：OPS_ADMIN 在，SUPER_ADMIN 不在
    st, me = call("GET", "/api/system/auth/me", token=token)
    data = me.get("data") or {}
    roles = data.get("roles") or []
    perms = data.get("permissions") or []
    has_ops = "OPS_ADMIN" in roles
    has_super = "SUPER_ADMIN" in roles
    p.record("身份 · roles 含 OPS_ADMIN", "GET", "/api/system/auth/me", st, me,
             st == 200 and has_ops, f"roles={roles} perms={len(perms)}")
    p.record("身份 · roles 不含 SUPER_ADMIN", "GET", "/api/system/auth/me", st, me,
             st == 200 and not has_super, f"roles={roles}")

    # ---- 六个读接口
    body = p.get_ok("读 · 用户管理", "/api/system/user/page?page=1&page_size=5", token)
    total = ((body.get("data") or {}).get("total"))
    want = args.expect_user_total
    ok = total is not None and (total == want if want is not None else total > 1)
    p.record("用户管理 · total 覆盖全量", "GET", "/api/system/user/page", 200, body, ok,
             f"total={total}" + (f" 期望={want}" if want is not None else ""))

    p.get_ok("读 · 角色管理", "/api/system/role/page?page=1&page_size=20", token)
    p.get_ok("读 · 部门管理", "/api/system/dept/tree", token)
    p.get_ok("读 · 菜单管理", "/api/system/menu/tree", token)
    p.get_ok("读 · 系统参数", "/api/support/config/page?page=1&page_size=10", token)
    p.get_ok("读 · 操作日志", "/api/support/log/page?page=1&page_size=10", token)

    # ---- 侧边栏（前端 tree 只认 sys_role_menu 里显式授予的 menu_type=1/2 行，父级不自动补）
    st, mn = call("GET", "/api/system/auth/menus", token=token)
    nodes = mn.get("data") or []
    names = [str(n.get("name")) for n in nodes if isinstance(n, dict)]
    p.record("读 · 侧边栏菜单树", "GET", "/api/system/auth/menus", st, mn,
             st == 200 and len(nodes) > 0, f"顶层 {len(nodes)} 个：{','.join(names[:6])}")

    # ---- 写操作（建完即删，净零）
    key = f"superconv.probe.{int(time.time())}"
    st, body = call("POST", "/api/support/config", {
        "config_key": key,
        "config_value": "1",
        "config_type": "STRING",
        "description": "P0-FIX-2B-C 运维探针（建完即删）",
        "category": "superconv",
    }, token=token)
    created_id = ((body.get("data") or {}).get("id")) if st == 200 else None
    p.record("写 · 新建系统参数", "POST", "/api/support/config", st, body, st == 200 and bool(created_id),
             f"config_key={key}")
    if created_id:
        st2, body2 = call("DELETE", f"/api/support/config/{created_id}", token=token)
        p.record("写 · 删除系统参数(清理)", "DELETE", f"/api/support/config/{{id}}", st2, body2,
                 st2 == 200 and body2.get("code") == 0, "净零：探针行不留库")

    # ---- 反向断言：业务接口必须 403
    p.get_expect_403("边界 · 业务接口必须 403", "/api/business/erp/account/page?page=1&page_size=5", token,
                     "data_scope=1 不是业务后门：业务边界靠权限码收")

    print()
    print(f"== 结果：{len(p.rows)} 条断言，FAIL={p.failed} ==")
    if p.failed:
        print("✗ 清单未全过 → 停手：保持/回退 SUPER_ADMIN 绑定，不许先摘了再说", file=sys.stderr)
        return 1
    print("✓ 运维动作清单逐条通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
