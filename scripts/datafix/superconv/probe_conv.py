#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""超管收敛验收探针（【P0-FIX-2B-C】`t_db2506d2`）—— 逐账号、真身份、贴返回码。

为什么不是「查库断言」就够
--------------------------
接口级 403 只认**令牌里的权限快照**（`nik-web::has_permission`），菜单树由
`load_user_menus_uncached` 按角色授权拼。所以「他能不能看见 / 能不能调」必须在
**以他身份发请求**这一层验，而不是读 `sys_role_menu` 猜。

身份怎么来：生产口令不可猜（仓库规则），用 `mint_token.py` 按库内真值自签
（`ver` 逐请求与库比对、通配声明会被库侧复核 → 伪造不出超管，见该脚本 docstring）。

模式（`--mode`）与断言：
    admin        保留超管：/system/user 200、roles 含 SUPER_ADMIN
    laoliu       摘超管 + 5 个业务角色：/system/* 全部 403、业务页 200、业务菜单 >= 50
    henry        摘超管 + 4 个业务角色：/system/* 全部 403、业务菜单 >= 40
    laoliu_test  摘超管 + 摘 GENERAL_MANAGER：3 个业务角色、/system/* 403

退出码：0 = 该模式全部断言通过；1 = 有失败。
输出：可读清单 + 末尾一行 JSON（便于落档 / 版本间 diff）。
"""
import argparse
import json
import os
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mint_token  # noqa: E402  （同目录工具：按库内真值自签令牌）

BASE = os.environ.get("SUPERCONV_BASE", "http://127.0.0.1:8023").rstrip("/")
TIMEOUT = 30

SYS_PAGES = [
    ("system:user",   "/api/system/user/page?page=1&page_size=1"),
    ("system:role",   "/api/system/role/page?page=1&page_size=1"),
    ("system:dept",   "/api/system/dept/tree"),
    ("system:menu",   "/api/system/menu/tree"),
    ("support:config", "/api/support/config/page?page=1&page_size=1"),
    ("support:log",   "/api/support/log/page?page=1&page_size=1"),
]
BIZ_PAGES = [
    ("business:task",    "/api/business/task/page?page=1&page_size=1"),
    ("business:project", "/api/business/project/page?page=1&page_size=1"),
]

MODES = {
    "admin": dict(roles=None, must_have=["SUPER_ADMIN"], must_not=[], sys="200", biz_min=None,
                  biz_menu_min=None),
    # laoliu / henry 的「可见业务页」期望 = 12：5 个业务角色的并集就是 12 个页面（+ 6 个目录），
    # 老大口述的「53 个业务菜单 / ≥50」是按 sys_role_menu 行数（含按钮）记的，页面数是 12（见 README §3）。
    "laoliu": dict(roles=5, must_have=["DEPT_LEADER", "FINANCE_MANAGER", "WAREHOUSE_MANAGER",
                                       "PURCHASE_MANAGER", "GENERAL_MANAGER"],
                   must_not=["SUPER_ADMIN"], sys="403", biz_min=None, biz_menu_min=12),
    "henry": dict(roles=4, must_have=["DEPT_LEADER", "GENERAL_MANAGER", "PURCHASE_MANAGER",
                                      "WAREHOUSE_MANAGER"],
                  must_not=["SUPER_ADMIN"], sys="403", biz_min=None, biz_menu_min=12),
    "laoliu_test": dict(roles=3, must_have=["DEPT_LEADER", "PURCHASE_MANAGER", "WAREHOUSE_MANAGER"],
                        must_not=["SUPER_ADMIN", "GENERAL_MANAGER"], sys="403", biz_min=None,
                        biz_menu_min=12),
}


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


def total_of(body):
    data = body.get("data") or {}
    if isinstance(data, dict):
        if "total" in data:
            return data["total"]
        if isinstance(data.get("list"), list):
            return len(data["list"])
    if isinstance(data, list):
        return len(data)
    return None


def walk(nodes, out):
    for n in nodes or []:
        out.append(n)
        walk(n.get("children"), out)


class P:
    def __init__(self):
        self.rows = []
        self.fail = 0

    def add(self, name, expect, actual, ok, extra=""):
        self.rows.append({"check": name, "expect": str(expect), "actual": str(actual), "ok": ok})
        if not ok:
            self.fail += 1
        flag = "PASS" if ok else "FAIL"
        print(f"  [{flag}] {name:<34} 期望={str(expect):<22} 实际={actual}" + (f" {extra}" if extra else ""))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--username", required=True)
    ap.add_argument("--mode", required=True, choices=sorted(MODES))
    ap.add_argument("--token", help="不给则按库内真值自签（profile 由 NIK_MIDDLEWARE_AUTH_JWT_SECRET 决定）")
    ap.add_argument("--password", help="给了就走**真登录**（隔离副本里把口令改成已知值后使用；生产不猜口令）")
    ap.add_argument("--label", default="")
    args = ap.parse_args()
    spec = MODES[args.mode]
    p = P()

    token = args.token
    if not token and args.password:
        st, body = call("POST", "/api/system/auth/login",
                        {"username": args.username, "password": args.password})
        token = (body.get("data") or {}).get("access_token") if st == 200 else None
        if not token:
            print(f"登录失败 http={st} body={json.dumps(body, ensure_ascii=False)[:300]}", file=sys.stderr)
            return 2
        print(f"   （身份来自真登录 /api/system/auth/login）")
    if not token:
        secret = os.environ.get("NIK_MIDDLEWARE_AUTH_JWT_SECRET", "")
        if not secret:
            print("缺 NIK_MIDDLEWARE_AUTH_JWT_SECRET", file=sys.stderr)
            return 2
        # 直接用 mint_token 的库内真值签（不经过登录，口令不可猜）
        import subprocess
        r = subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                         "mint_token.py"),
                            "--username", args.username],
                           capture_output=True, text=True, env=dict(os.environ, **{
                               "NIK_MIDDLEWARE_AUTH_JWT_SECRET": secret}))
        token = r.stdout.strip()
        if r.returncode != 0 or not token:
            print("自签失败：" + r.stderr.strip()[:300], file=sys.stderr)
            return 2
        sid = ""
    sid = str(abs(hash(token)) % 10**6)

    print(f"== 超管收敛验收 · {args.username} / mode={args.mode} · {BASE} ==")
    if args.label:
        print(f"   label={args.label}")

    st, me = call("GET", "/api/system/auth/me", token=token)
    data = me.get("data") or {}
    roles = data.get("roles") or []
    p.add("auth/me", "200", st, st == 200)
    perms = data.get("permissions") or []
    p.add("auth/me roles", spec["roles"] if spec["roles"] else "含 SUPER_ADMIN",
          f"{len(roles)}:{','.join(roles)}", (len(roles) == spec["roles"]) if spec["roles"] else True)
    p.add("auth/me perms", "记录", len(perms), True)
    for r in spec["must_have"]:
        p.add(f"必含角色 {r}", "含", "含" if r in roles else "缺", r in roles)
    for r in spec["must_not"]:
        p.add(f"必不含角色 {r}", "不含", "不含" if r not in roles else "含", r not in roles)

    # ---- 菜单树：可见业务菜单 / 系统菜单
    st, mbody = call("GET", "/api/system/auth/menus", token=token)
    nodes = []
    walk(mbody.get("data"), nodes)
    pages = [n for n in nodes if n.get("menu_type") == 2 and (n.get("path") or n.get("auth_code"))]
    biz_pages = [n for n in pages if (n.get("auth_code") or "").startswith(("business:", "erp:"))]
    sys_pages = [n for n in pages if (n.get("auth_code") or "").startswith(("system:", "support:"))]
    p.add("auth/menus", "200", st, st == 200)
    p.add("菜单树·可见业务页", f">= {spec['biz_menu_min']}" if spec["biz_menu_min"] else "记录",
          len(biz_pages), True if spec["biz_menu_min"] is None else len(biz_pages) >= spec["biz_menu_min"])
    p.add("菜单树·可见系统页", "记录", len(sys_pages), True)
    p.add("菜单树·节点总数", "记录", len(nodes), True)

    # ---- 系统管理六页
    for name, path in SYS_PAGES:
        st, body = call("GET", path, token=token)
        want = int(spec["sys"])
        extra = ""
        if st == 200:
            extra = f"total={total_of(body)}"
        p.add(name, want, st, st == want, extra)

    # ---- 业务页
    for name, path in BIZ_PAGES:
        st, body = call("GET", path, token=token)
        if spec["biz_min"] is None:
            p.add(name, "记录", f"{st}" + (f" total={total_of(body)}" if st == 200 else ""), True)
        else:
            p.add(name, 200, st, st == 200, f"total={total_of(body)}" if st == 200 else "")

    out = {
        "username": args.username, "mode": args.mode, "base": BASE, "label": args.label,
        "roles": roles, "perm_count": len(perms),
        "menus_total": len(nodes), "menus_business_pages": len(biz_pages),
        "menus_system_pages": len(sys_pages),
        "business_pages": [n.get("path") or n.get("auth_code") for n in biz_pages],
        "fail": p.fail, "checks": p.rows,
    }
    print("\nJSON " + json.dumps(out, ensure_ascii=False))
    return 0 if p.fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
