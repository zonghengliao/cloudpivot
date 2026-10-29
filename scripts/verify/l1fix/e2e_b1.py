#!/usr/bin/env python3
"""L1 修复 E2E —— 阶段 B1：用「模板 profile 无 key」的现场重建 fix-check-ai，验证 .env 被补齐"""
import json
import os
import sys

sys.path.insert(0, "/tmp/l1fix")
from e2e_a import PROFILES_ROOT, call, login  # noqa: E402

PROFILE = "fix-check-ai"
TEMPLATE = "l1fix-tmpl"


def env_report(profile):
    path = f"{PROFILES_ROOT}/{profile}/.env"
    print(f"  .env 路径: {path}")
    if not os.path.exists(path):
        print("  [FAIL] .env 不存在")
        return [], None
    mode = oct(os.stat(path).st_mode & 0o777)
    rows = []
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        v = v.strip().strip('"').strip("'")
        rows.append((k.strip(), len(v), (v[:6] + "…" + v[-4:]) if len(v) > 12 else "***"))
    print(f"  权限: {mode}（要求 0o600）")
    print("  键名列表（值脱敏）:")
    for k, n, masked in rows:
        print(f"    - {k}  len={n}  value={masked}")
    print("  [{}] 含 DEEPSEEK_API_KEY".format(
        "PASS" if any(k == "DEEPSEEK_API_KEY" for k, _, _ in rows) else "FAIL"))
    print("  [{}] 权限 600".format("PASS" if mode == "0o600" else "FAIL"))
    return rows, mode


def main():
    token = login()
    state = json.load(open("/tmp/l1fix/state.json"))
    old_uid = state["user_id"]

    print(f"[B1-1] 删除阶段 A 建的 {PROFILE}（要用「模板无 key」现场重建）")
    st, r = call("DELETE", f"/api/system/user/ai/{old_uid}", token=token)
    print(f"  -> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
    print(f"  profile 目录还存在？ {os.path.exists(f'{PROFILES_ROOT}/{PROFILE}')}")

    print(f"\n[B1-2] POST /api/system/user/ai  clone_from={TEMPLATE}（该模板 .env 无任何 key）")
    st, r = call(
        "POST",
        "/api/system/user/ai",
        {
            "username": PROFILE,
            "real_name": "L1修复验收机器人",
            "agent_description": "L1 修复验收：验证新建 AI 用户的 profile 级凭据可用",
            "clone_from": TEMPLATE,
        },
        token=token,
    )
    print(f"  -> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
    if st != 200 or r.get("code") != 0:
        print(json.dumps(r, ensure_ascii=False)[:900])
        sys.exit(2)
    data = r["data"]
    uid = data["user_id"]
    print(f"  user_id={uid} profile={data['hermes_profile']} gateway={data.get('gateway_status')}")

    print(f"\n[B1-3] 检查 {PROFILES_ROOT}/{PROFILE}/.env（模板无 key → 必须由后端补齐）")
    rows, _ = env_report(PROFILE)

    print("\n[B1-4] GET /status 凭据字段")
    st, r = call("GET", f"/api/system/user/ai/{uid}/status", token=token)
    d = r.get("data") or {}
    for key in ("healthy", "credentials_ok", "credential_keys", "credential_required",
                "env_file_mode", "credential_detail"):
        print(f"    {key} = {d.get(key)!r}")

    json.dump({"user_id": uid, "profile": PROFILE, "token": token},
              open("/tmp/l1fix/state.json", "w"))
    print("\n阶段 B1 完成")


if __name__ == "__main__":
    main()
