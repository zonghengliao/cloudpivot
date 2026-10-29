#!/usr/bin/env python3
"""L1 修复 E2E —— 阶段 A：登录 + 通过 API 建 AI 用户 + 检查 profile .env"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BASE = os.environ.get("CP_BASE", "http://127.0.0.1:18206")
ADMIN_PASS = os.environ["ALUN_ADMIN_PASSWORD"]
PROFILE = "fix-check-ai"
PROFILES_ROOT = "/root/.hermes/profiles"


def call(method, path, body=None, token=None, timeout=60):
    url = BASE + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw}


def login():
    # 优先用「已知可用」的口令（首登改密后的那个），避免每次都用旧口令撞限流
    candidates = [ADMIN_PASS + "-L1fix9", ADMIN_PASS]
    creds_path = "/tmp/l1fix/creds.json"
    if os.path.exists(creds_path):
        try:
            saved = json.load(open(creds_path)).get("password")
            if saved:
                candidates = [saved] + [c for c in candidates if c != saved]
        except Exception:
            pass
    st, r = (0, {})
    data = {}
    for idx, password in enumerate(candidates):
        st, r = call("POST", "/api/system/auth/login", {"username": "admin", "password": password})
        print(f"登录 admin（候选 {idx + 1}）-> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
        if st == 200 and r.get("code") == 0:
            data = r["data"]
            json.dump({"username": "admin", "password": password}, open(creds_path, "w"))
            break
        if st == 429:
            break
    if not data:
        print(json.dumps(r, ensure_ascii=False)[:500])
        sys.exit(1)
    print("  登录返回字段:", sorted(data.keys()))
    if data.get("must_change_password"):
        print("  must_change_password=true → 先改密（PUT /api/system/auth/password）")
        new_pass = ADMIN_PASS + "-L1fix9"
        st2, r2 = call(
            "PUT",
            "/api/system/auth/password",
            {"old_password": ADMIN_PASS, "new_password": new_pass},
            token=data["access_token"],
        )
        print(f"  改密 -> HTTP {st2} code={r2.get('code')} msg={r2.get('msg')}")
        st, r = call("POST", "/api/system/auth/login", {"username": "admin", "password": new_pass})
        print(f"  重新登录 -> HTTP {st} code={r.get('code')}")
        data = r["data"]
    return data["access_token"]


def env_report(profile):
    path = f"{PROFILES_ROOT}/{profile}/.env"
    print(f"  .env 路径: {path}")
    if not os.path.exists(path):
        print("  [FAIL] .env 不存在")
        return None
    st = os.stat(path)
    mode = oct(st.st_mode & 0o777)
    names = []
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        v = v.strip().strip('"').strip("'")
        masked = (v[:6] + "…" + v[-4:]) if len(v) > 12 else ("<empty>" if not v else "***")
        names.append((k.strip(), len(v), masked))
    print(f"  权限: {mode}（要求 0o600）")
    print("  键名列表（值脱敏）:")
    for k, n, masked in names:
        print(f"    - {k}  len={n}  value={masked}")
    print("  [{}] 含 DEEPSEEK_API_KEY".format(
        "PASS" if any(k == "DEEPSEEK_API_KEY" for k, _, _ in names) else "FAIL"))
    print("  [{}] 权限 600".format("PASS" if mode == "0o600" else "FAIL"))
    return names


def main():
    token = login()

    # 幂等清理：同名账号已存在则先删（上一轮残留）
    st, r = call("GET", f"/api/system/user/page?page=1&size=100&username={PROFILE}", token=token)
    rows = (r.get("data") or {}).get("list") or (r.get("data") or {}).get("records") or []
    for row in rows:
        if row.get("username") == PROFILE:
            uid = row.get("id")
            st2, r2 = call("DELETE", f"/api/system/user/ai/{uid}", token=token)
            print(f"[0] 清理残留 {PROFILE} -> HTTP {st2} code={r2.get('code')}")

    print(f"\n[1] POST /api/system/user/ai  建 {PROFILE}")
    st, r = call(
        "POST",
        "/api/system/user/ai",
        {
            "username": PROFILE,
            "real_name": "L1修复验收机器人",
            "agent_description": "L1 修复验收：验证新建 AI 用户的 profile 级凭据可用",
        },
        token=token,
    )
    print(f"  -> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
    if st != 200 or r.get("code") != 0:
        print(json.dumps(r, ensure_ascii=False)[:800])
        sys.exit(2)
    data = r["data"]
    uid = data["user_id"]
    print(f"  user_id={uid} profile={data['hermes_profile']} gateway={data.get('gateway_status')}")
    print(f"  soul_path={data.get('soul_path')}")

    print(f"\n[2] 检查 {PROFILES_ROOT}/{PROFILE}/.env")
    env_report(PROFILE)

    print("\n[3] GET /api/system/user/ai/{id}/status（凭据自检字段）")
    st, r = call("GET", f"/api/system/user/ai/{uid}/status", token=token)
    d = r.get("data") or {}
    print(f"  -> HTTP {st} code={r.get('code')}")
    for key in (
        "healthy",
        "credentials_ok",
        "credential_keys",
        "credential_required",
        "env_file_mode",
        "credential_detail",
        "gateway",
    ):
        print(f"    {key} = {d.get(key)!r}")

    with open("/tmp/l1fix/state.json", "w") as f:
        json.dump({"user_id": uid, "profile": PROFILE, "token": token}, f)
    print("\n阶段 A 完成，状态已存 /tmp/l1fix/state.json")


if __name__ == "__main__":
    main()
