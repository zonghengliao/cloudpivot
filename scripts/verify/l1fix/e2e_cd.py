#!/usr/bin/env python3
"""L1 修复 E2E —— 阶段 C：失败回滚（全局 .env 无 key → 建号必须失败且零残留）
                    阶段 D：已有用户的凭据修复接口（POST /repair）
"""
import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, "/tmp/l1fix")
from e2e_a import call, login  # noqa: E402

PROFILES_ROOT = "/root/.hermes/profiles"
WORKSPACE_ROOT = "/root/.hermes/workspace"
GLOBAL_ENV = "/root/.hermes/.env"
GLOBAL_BAK = "/root/.hermes/.env.l1fix-bak"


def psql(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ["ALUN_DATABASE_PASSWORD"]
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "cloudpivot", "-d", "cloudpivot_l1fix", "-Atc", sql],
        capture_output=True, text=True, env=env, timeout=60,
    )
    return out.stdout.strip() or out.stderr.strip()


def main():
    token = login()

    # ---------------- 阶段 C：失败回滚 ----------------
    print("=" * 70)
    print("[C-1] 制造「拿不到 key」现场：临时移走全局 /root/.hermes/.env")
    shutil.move(GLOBAL_ENV, GLOBAL_BAK)
    print(f"  {GLOBAL_ENV} -> {GLOBAL_BAK}；存在？ {os.path.exists(GLOBAL_ENV)}")
    try:
        print("\n[C-2] POST /api/system/user/ai 建 fix-fail-ai（期望 500 + 64014）")
        st, r = call("POST", "/api/system/user/ai",
                     {"username": "fix-fail-ai", "real_name": "回滚验证",
                      "agent_description": "缺 key 时必须整体失败"}, token=token)
        print(f"  -> HTTP {st} code={r.get('code')}")
        print(f"  msg = {r.get('msg')}")
    finally:
        shutil.move(GLOBAL_BAK, GLOBAL_ENV)
        os.chmod(GLOBAL_ENV, 0o600)
        print(f"\n  [恢复] 全局 .env 已还原（存在？ {os.path.exists(GLOBAL_ENV)}，"
              f"权限 {oct(os.stat(GLOBAL_ENV).st_mode & 0o777)}）")

    print("\n[C-3] 残留检查（磁盘 + 库）")
    prof = f"{PROFILES_ROOT}/fix-fail-ai"
    ws = f"{WORKSPACE_ROOT}/fix-fail-ai"
    print(f"  [{'PASS' if not os.path.exists(prof) else 'FAIL'}] profile 目录不存在：{prof}")
    print(f"  [{'PASS' if not os.path.exists(ws) else 'FAIL'}] workspace 目录不存在：{ws}")
    rows = psql("SELECT count(*) FROM sys_user WHERE username='fix-fail-ai' AND deleted_at IS NULL")
    print(f"  有效（未软删）sys_user 行数 = {rows}（要求 0）")
    rows_all = psql("SELECT count(*) FROM sys_user WHERE username='fix-fail-ai'")
    rows_del = psql("SELECT coalesce(deleted_at::text,'NULL') FROM sys_user WHERE username='fix-fail-ai'")
    print(f"  sys_user 总行数（含软删）= {rows_all}；deleted_at = {rows_del}")
    ai_rows = psql(
        "SELECT count(*) FROM sys_ai_agent a WHERE a.hermes_profile='fix-fail-ai' AND a.deleted_at IS NULL")
    print(f"  有效 sys_ai_agent 行数 = {ai_rows}（要求 0）")
    inv = psql(
        "SELECT count(*) FROM sys_ai_agent a JOIN sys_user u ON u.id=a.user_id "
        "WHERE a.deleted_at IS NULL AND (u.user_type<>'ROBOT' OR u.deleted_at IS NOT NULL)")
    print(f"  不变式违规行数（ROBOT⇔配置行）= {inv}（要求 0）")
    ok_c = (not os.path.exists(prof) and not os.path.exists(ws) and rows == "0"
            and ai_rows == "0" and inv == "0")
    print(f"  [{'PASS' if ok_c else 'FAIL'}] C 阶段：失败 + 磁盘零残留 + 库内无有效行")

    # ---------------- 阶段 D：修复接口 ----------------
    print("\n" + "=" * 70)
    state = json.load(open("/tmp/l1fix/state.json"))
    uid, profile = state["user_id"], state["profile"]
    env_path = f"{PROFILES_ROOT}/{profile}/.env"

    print(f"[D-1] 人为破坏 {env_path}：去掉 key + 权限改成 644")
    open(env_path, "w").write(
        "# Per-profile secrets for this Hermes profile.\n"
        "# API keys and tokens set here override the shell environment.\n"
        "# Behavioral settings belong in config.yaml, not here.\n")
    os.chmod(env_path, 0o644)
    print(f"  现有键：{[]}  权限：{oct(os.stat(env_path).st_mode & 0o777)}")

    st, r = call("GET", f"/api/system/user/ai/{uid}/status", token=token)
    d = r.get("data") or {}
    print(f"\n[D-2] GET /status -> HTTP {st} code={r.get('code')}")
    print(f"    credentials_ok = {d.get('credentials_ok')}（应为 False）")
    print(f"    credential_detail = {d.get('credential_detail')!r}")
    print(f"    env_file_mode = {d.get('env_file_mode')}（644=420）")

    print(f"\n[D-3] POST /api/system/user/ai/{uid}/repair -> 修复")
    st, r = call("POST", f"/api/system/user/ai/{uid}/repair", {}, token=token)
    print(f"  -> HTTP {st} code={r.get('code')} msg={r.get('msg')}")
    cred = r.get("data") or {}
    for k in ("env_path", "provider", "required_key", "present_keys", "env_file_mode",
              "credentials_ok", "repaired_keys", "detail"):
        print(f"    {k} = {cred.get(k)!r}")

    print("\n[D-4] 修复后磁盘复核")
    mode = oct(os.stat(env_path).st_mode & 0o777)
    keys = [ln.split("=", 1)[0] for ln in open(env_path) if ln.strip() and not ln.startswith("#")
            and "=" in ln]
    print(f"  键名 = {keys}；权限 = {mode}")
    st, r = call("POST", f"/api/system/user/ai/{uid}/repair", {}, token=token)
    again = (r.get("data") or {}).get("repaired_keys")
    print(f"  再点一次（幂等）-> repaired_keys = {again!r}（应为空数组）")

    ok_d = (cred.get("credentials_ok") is True and "DEEPSEEK_API_KEY" in (cred.get("present_keys") or [])
            and mode == "0o600" and again == [])
    print(f"  [{'PASS' if ok_d else 'FAIL'}] D 阶段：修复接口补齐 key + 600 + 幂等")
    if not (ok_c and ok_d):
        sys.exit(6)


if __name__ == "__main__":
    main()
