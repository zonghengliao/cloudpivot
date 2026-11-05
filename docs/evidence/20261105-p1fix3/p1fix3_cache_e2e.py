#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-FIX-3 真机验收：授权 / 撤销授权后权限缓存的失效行为（隔离实例，不重启）。

用法（同一份脚本跑两种模式，做「改前 / 改后」对比）：

    # 改后（新代码）
    P1FIX3_EXPECT=fix      python3 docs/evidence/20261105-p1fix3/p1fix3_cache_e2e.py
    # 改前（/root/p1fix3-baseline-bin 起的实例，复现评审现场）
    P1FIX3_EXPECT=baseline python3 docs/evidence/20261105-p1fix3/p1fix3_cache_e2e.py

环境变量：
    P1FIX3_BASE  默认 http://127.0.0.1:18116
    P1FIX3_DB    默认 cloudpivot_p1fix3_a
    P1FIX3_EXPECT fix（默认）| baseline

三条链路（全部**不重启**实例）：
  A. 评审现场复现：授权 / 撤销由**直连 SQL** 写入（app 内没有任何写点能清缓存），
     随后重新登录 —— 改前：缓存里的旧权限码继续生效（授权后 403 / 撤销后仍 200）；
     改后：签发令牌时直读库（写穿缓存），授权后 200、撤销后 403。
  B. 走**接口**授权 / 撤销：库侧 `token_version` 推进 → 旧令牌立刻 401（20008 权限已变更，
     不会静默 403），重新登录立刻生效 —— 改前改后都必须成立（回归保护）。
  C. `GET /api/system/auth/me` 的权限码与刚签发的令牌快照一致（界面侧不再出现
     「按钮没了 / 按钮在但接口 403」）。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = os.environ.get("P1FIX3_BASE", "http://127.0.0.1:18116")
DB = os.environ.get("P1FIX3_DB", "cloudpivot_p1fix3_a")
EXPECT = os.environ.get("P1FIX3_EXPECT", "fix").strip().lower()
INIT_PASS = os.environ.get("NIK_ADMIN_PASSWORD", "Admin@123456")
NEW_PASS = INIT_PASS + "-p1fix3"
PERM_CODE = "support:dict:list"
PROBE = "/api/support/dict/type/page?page=1&page_size=1"  # 需要 support:dict:list

RESULTS = []


def psql(sql):
    env = dict(os.environ, PGPASSWORD="alun")
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-tAF", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60,
    )
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:500])
    return [ln.split("\x1f") for ln in r.stdout.strip().split("\n") if ln]


def scalar(sql):
    rows = psql(sql)
    return rows[0][0] if rows else None


def api(method, path, body=None, token=None):
    data = None
    headers = {}
    if token:
        headers["Authorization"] = "Bearer " + token
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw[:300]}


def ok(cond, label, detail=""):
    RESULTS.append((bool(cond), label, detail))
    print(("[PASS] " if cond else "[FAIL] ") + label + (f"   {detail}" if detail else ""))
    return cond


def expect(code, want, label, extra=""):
    return ok(code == want, label, f"期望 HTTP {want} 实际 {code} {extra}".strip())


def login(user, password):
    """登录（429 自动退避重试）。

    仓里的登录限流是 `LOGIN_RATE_MAX_ATTEMPTS = 10 / LOGIN_RATE_WINDOW_SECS = 60`
    （按 `用户名:IP` 计），本脚本一轮要登录 6 次，连跑两轮必然撞上第 11 次 →
    这里按窗口长度退避重试，避免把「限流」误判成「权限不生效」。
    """
    for attempt in range(3):
        code, resp = api("POST", "/api/system/auth/login", {"username": user, "password": password})
        if code == 429:
            wait = 62
            print(f"   ! 登录被限流（429），{wait}s 后重试（第 {attempt + 1} 次）")
            time.sleep(wait)
            continue
        if code == 200 and (resp.get("data") or {}).get("access_token"):
            d = resp["data"]
            return (d["access_token"], bool(d.get("must_change_password"))), 200, resp
        return None, code, resp
    return None, 429, {"msg": "登录限流重试仍失败"}


def main():
    print("=" * 100)
    print(f"P1-FIX-3 权限缓存失效验收   接口={BASE}   库={DB}   期望模式={EXPECT}")
    print("=" * 100)
    if EXPECT not in ("fix", "baseline"):
        print(f"!! P1FIX3_EXPECT 只能是 fix / baseline，当前 {EXPECT}")
        return 2

    code, _ = api("GET", "/health")
    ok(code == 200, "健康检查 /health", str(code))

    # ── 0. admin 登录（首启强制改密，脚本内置幂等） ─────────────────────────
    admin_pass = INIT_PASS
    got, code, resp = login("admin", admin_pass)
    if not got:
        admin_pass = NEW_PASS
        got, code, resp = login("admin", admin_pass)
        ok(bool(got), "admin 登录（已改密状态，幂等重跑）", f"HTTP {code}")
    else:
        ok(True, "admin 登录（初始口令）", f"HTTP {code}")
    if not got:
        print("!! admin 登录失败，验收无法继续")
        return 1
    admin_tok, must_change = got
    if must_change:
        code, resp = api("PUT", "/api/system/auth/password",
                         {"old_password": admin_pass, "new_password": NEW_PASS}, token=admin_tok)
        ok(code == 200, "admin 首次改密", f"HTTP {code} {resp.get('msg','')}")
        admin_pass = NEW_PASS
        got, code, _ = login("admin", admin_pass)
        ok(bool(got), "改密后重新登录", f"HTTP {code}")
        if not got:
            return 1
        admin_tok = got[0]
    print(f"   · admin 口令口径已就绪（must_change_password 已清）")

    # ── 1. 夹具：角色 + 用户（口令哈希与 admin 对齐，幂等重建） ─────────────
    psql("DELETE FROM sys_role_menu rm USING sys_role r "
         "WHERE rm.role_id = r.id AND r.role_code = 'P1FIX3_CACHE_ROLE';")
    psql("DELETE FROM sys_user_role ur USING sys_role r "
         "WHERE ur.role_id = r.id AND r.role_code = 'P1FIX3_CACHE_ROLE';")
    psql("DELETE FROM sys_user_role ur USING sys_user u "
         "WHERE ur.user_id = u.id AND u.username = 'p1fix3_cache_user';")
    psql("DELETE FROM sys_user WHERE username = 'p1fix3_cache_user';")
    psql("DELETE FROM sys_role WHERE role_code = 'P1FIX3_CACHE_ROLE';")
    psql("INSERT INTO sys_role (role_code, role_name, description, status, data_scope) "
         "VALUES ('P1FIX3_CACHE_ROLE', 'P1FIX3 缓存验证角色', 'P1-FIX-3 夹具', 1, 4);")
    psql("INSERT INTO sys_user (username, password, real_name, must_change_password, status) "
         f"VALUES ('p1fix3_cache_user', (SELECT password FROM sys_user WHERE username='admin'), "
         f"'P1FIX3 缓存用户', false, 1);")
    psql("INSERT INTO sys_user_role (user_id, role_id) SELECT u.id, r.id FROM sys_user u, sys_role r "
         "WHERE u.username='p1fix3_cache_user' AND r.role_code='P1FIX3_CACHE_ROLE';")
    role_id = scalar("SELECT id FROM sys_role WHERE role_code='P1FIX3_CACHE_ROLE' AND deleted_at IS NULL")
    user_id = scalar("SELECT id FROM sys_user WHERE username='p1fix3_cache_user' AND deleted_at IS NULL")
    menu_id = scalar(f"SELECT id FROM sys_menu WHERE auth_code='{PERM_CODE}' AND deleted_at IS NULL LIMIT 1")
    ok(bool(role_id and user_id and menu_id), "夹具就绪（角色 / 用户 / 权限点菜单）",
       f"role={role_id} user={user_id} menu={menu_id} code={PERM_CODE}")
    if not (role_id and user_id and menu_id):
        return 1
    granted = scalar(f"SELECT count(*) FROM sys_role_menu WHERE role_id='{role_id}'::uuid "
                     f"AND menu_id='{menu_id}'::uuid")
    ok(granted == "0", "基线状态：该角色尚未持有该权限点", f"role_menu 行数={granted}")

    def user_login(tag):
        got2, code2, resp2 = login("p1fix3_cache_user", admin_pass)
        ok(bool(got2), f"{tag}：用户重新登录（实例未重启）",
           f"HTTP {code2} token={'有' if got2 else '无'}")
        return got2[0] if got2 else None

    # ══════════════════════════════════════════════ A. 评审现场：直连 SQL 授权 ══
    print("\n## A. 直连 SQL 授权 / 撤销（app 内没有写点能清缓存 —— 评审现场就是这条）")
    tok = user_login("A0 授权前")
    code, resp = api("GET", PROBE, token=tok)
    expect(code, 403, "A1 授权前探测接口（无权限）", f"msg={resp.get('msg','')}")

    psql(f"INSERT INTO sys_role_menu (role_id, menu_id) VALUES ('{role_id}'::uuid, '{menu_id}'::uuid) "
         f"ON CONFLICT DO NOTHING;")
    print("   · 已用 psql 直接补授权（不经过任何业务接口）")
    code, _ = api("GET", "/health")
    ok(code == 200, "A2 授权后实例仍然健康（**没有重启**）", f"/health {code}")
    tok = user_login("A2 授权后")
    code, resp = api("GET", PROBE, token=tok)
    want = 200 if EXPECT == "fix" else 403
    expect(code, want, f"A2 授权后重新登录即可访问（{EXPECT} 期望 {want}）", f"msg={resp.get('msg','')}")

    psql(f"DELETE FROM sys_role_menu WHERE role_id='{role_id}'::uuid AND menu_id='{menu_id}'::uuid;")
    print("   · 已用 psql 直接撤销授权")
    tok = user_login("A3 撤销后")
    code, resp = api("GET", PROBE, token=tok)
    # 两种模式都必须被拒，但原因不同：
    #   fix       → 签发时直读库，撤销立刻反映（真正的「撤销立即生效」）；
    #   baseline  → 缓存里仍是 A0 那次登录的旧值（本来就没有这条权限），
    #               属「碰巧对」，不能算撤销生效的证据。
    expect(code, 403, "A3 撤销后重新登录立即被拒（两种模式都应 403）", f"msg={resp.get('msg','')}")

    # ══════════════════════════════════════════════ B. 走接口授权（回归保护） ══
    print("\n## B. 走接口授权 / 撤销（PUT /system/role/{id}/menus）")
    tok = user_login("B0 授权前")
    code, _ = api("GET", PROBE, token=tok)
    expect(code, 403, "B1 授权前探测接口（无权限）")

    code, resp = api("PUT", f"/api/system/role/{role_id}/menus",
                     {"menu_ids": [menu_id]}, token=admin_tok)
    ok(code == 200, "B2 管理员接口授权成功", f"HTTP {code} {resp.get('msg','')}")

    code, resp = api("GET", PROBE, token=tok)
    # 框架的权限闸在会话守卫**外层**：旧令牌里没有这个权限码 → 外层先回 403，
    # 根本走不到「令牌版本比对」。两条都算「旧令牌被拒」，关键是不能静默 200。
    # （撤销方向相反：令牌里有码 → 外层放行 → 会话守卫按 `token_version` 回 401，
    #   见 B6 —— 两个方向合起来证明「旧令牌不会带着旧权限继续用」。）
    ok(code in (401, 403), "B3 授权后**旧令牌**立刻被拒（不会静默 200）",
       f"HTTP {code} code={(resp.get('code'))} msg={resp.get('msg','')}")

    tok = user_login("B4 授权后")
    code, resp = api("GET", PROBE, token=tok)
    expect(code, 200, "B4 重新登录后立即能访问（实例未重启）", f"msg={resp.get('msg','')}")

    # C. me 的权限码与令牌快照一致
    code, resp = api("GET", "/api/system/auth/me", token=tok)
    perms = (resp.get("data") or {}).get("permissions") or []
    ok(code == 200 and PERM_CODE in perms, "C1 me 的权限码含刚授予的权限点",
       f"HTTP {code} perms={len(perms)} 含={PERM_CODE in perms}")

    code, resp = api("PUT", f"/api/system/role/{role_id}/menus", {"menu_ids": []}, token=admin_tok)
    ok(code == 200, "B5 管理员接口撤销授权成功", f"HTTP {code} {resp.get('msg','')}")
    code, resp = api("GET", PROBE, token=tok)
    ok(code == 401, "B6 撤销后旧令牌立刻失效", f"HTTP {code} msg={resp.get('msg','')}")
    tok = user_login("B7 撤销后")
    code, resp = api("GET", PROBE, token=tok)
    expect(code, 403, "B7 重新登录后立即被拒（实例未重启）", f"msg={resp.get('msg','')}")

    code, resp = api("GET", "/api/system/auth/me", token=tok)
    perms = (resp.get("data") or {}).get("permissions") or []
    ok(code == 200 and PERM_CODE not in perms, "C2 撤销后 me 的权限码不再含该权限点",
       f"HTTP {code} 含={PERM_CODE in perms}")

    # ── 收尾：清夹具 ────────────────────────────────────────────────────────
    psql("DELETE FROM sys_role_menu rm USING sys_role r "
         "WHERE rm.role_id = r.id AND r.role_code = 'P1FIX3_CACHE_ROLE';")
    psql("DELETE FROM sys_user_role ur USING sys_user u "
         "WHERE ur.user_id = u.id AND u.username = 'p1fix3_cache_user';")
    psql("DELETE FROM sys_user WHERE username = 'p1fix3_cache_user';")
    psql("DELETE FROM sys_role WHERE role_code = 'P1FIX3_CACHE_ROLE';")
    left = scalar("SELECT (SELECT count(*) FROM sys_user WHERE username='p1fix3_cache_user')::text")
    ok(left == "0", "收尾：夹具已清理（用户 0 行）", f"残留={left}")

    passed = sum(1 for r in RESULTS if r[0])
    failed = len(RESULTS) - passed
    print("\n" + "=" * 100)
    print(f"汇总：PASS={passed} FAIL={failed}  （期望模式 {EXPECT}）")
    if failed:
        for good, label, detail in RESULTS:
            if not good:
                print(f"  FAIL: {label}  {detail}")
    print("=" * 100)
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
