#!/usr/bin/env python3
"""生成 svc_selfcheck 的凭据文件（口令 + argon2id PHC 串）。

为什么是 argon2id：后端用 `nik::prelude::Crypto`（RustCrypto `argon2` 0.6）校验口令，
`Crypto::verify_password` 按 **PHC 串自带的参数** 解析，所以只要产出的串是合法的
`$argon2id$v=19$m=…,t=…,p=…$salt$hash` 就能验通。这里刻意用与生产既有账号
（`sys_user.admin`：`$argon2id$v=19$m=19456,t=2,p=1$…`）**完全相同的参数**，
避免出现「能生成但后端不认」的第二种形态。

安全口径：
  · 口令用 `secrets.token_urlsafe(24)`（≈32 字符，≥24 的要求），**只写进凭据文件**；
  · 本脚本只打印长度，不打印口令/哈希；
  · 凭据文件权限 600，路径默认 /root/svc_selfcheck.cred（不进仓库、不进日志）。

依赖：argon2-cffi（`python3 -m venv /root/.venvs/svcchk && /root/.venvs/svcchk/bin/pip install argon2-cffi`，
见 setup_hash_env.sh）。

用法：
  python3 gen_credential.py --out /root/svc_selfcheck.cred [--rotate]
退出码：0 成功；2 环境缺 argon2-cffi；3 参数错误
"""
from __future__ import annotations

import argparse
import os
import secrets
import sys
from datetime import datetime, timezone, timedelta

USER = "svc_selfcheck"

# 与 sys_user.admin 现有哈希同参数（读自生产库，只取了 PHC 的头部参数，非密文）
ARGON2_TIME_COST = 2
ARGON2_MEMORY_KIB = 19456
ARGON2_PARALLELISM = 1
ARGON2_HASH_LEN = 32
ARGON2_SALT_LEN = 16


def load_hasher():
    try:
        from argon2 import PasswordHasher
        from argon2.low_level import Type
    except Exception as exc:  # noqa: BLE001
        print(f"✗ 缺 argon2-cffi：{type(exc).__name__}: {exc}", file=sys.stderr)
        print("  装法：bash scripts/ops/svc_selfcheck/setup_hash_env.sh", file=sys.stderr)
        raise SystemExit(2)
    return PasswordHasher(
        time_cost=ARGON2_TIME_COST,
        memory_cost=ARGON2_MEMORY_KIB,
        parallelism=ARGON2_PARALLELISM,
        hash_len=ARGON2_HASH_LEN,
        salt_len=ARGON2_SALT_LEN,
        type=Type.ID,
    )


def main() -> int:
    ap = argparse.ArgumentParser(description="生成 svc_selfcheck 凭据文件（口令 + argon2id PHC）")
    ap.add_argument("--out", default="/root/svc_selfcheck.cred", help="凭据文件路径（600）")
    ap.add_argument("--rotate", action="store_true", help="即使文件已存在也重新生成（= 改密）")
    ap.add_argument("--user", default=USER, help="账号名（默认 svc_selfcheck）")
    args = ap.parse_args()

    out = os.path.abspath(args.out)
    if os.path.exists(out) and not args.rotate:
        print(f"= 凭据文件已存在，沿用（{out}）；要改密请加 --rotate")
        return 0

    ph = load_hasher()
    password = secrets.token_urlsafe(24)
    if len(password) < 24:
        print(f"✗ 生成的口令只有 {len(password)} 字符（要求 ≥24）", file=sys.stderr)
        return 3
    pwhash = ph.hash(password)

    now = datetime.now(timezone(timedelta(hours=8))).strftime("%Y-%m-%d %H:%M:%S%z")
    payload = (
        "# svc_selfcheck 凭据（600，勿进仓库/勿进日志/勿贴聊天）\n"
        "# 卡 t_882d48c8 —— 由 scripts/ops/svc_selfcheck/gen_credential.py 生成\n"
        f"# created_at={now}\n"
        f"SVCC_USER='{args.user}'\n"
        f"SVCC_PASSWORD='{password}'\n"
        f"SVCC_PWHASH='{pwhash}'\n"
        f"SVCC_ARGON2='m={ARGON2_MEMORY_KIB},t={ARGON2_TIME_COST},p={ARGON2_PARALLELISM}'\n"
    )

    os.makedirs(os.path.dirname(out) or "/", mode=0o700, exist_ok=True)
    fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(payload)
    os.chmod(out, 0o600)

    print(f"✅ 凭据已写 {out}（600）")
    print(f"   账号={args.user} 口令长度={len(password)} PHC={pwhash.split('$')[1]}/m={ARGON2_MEMORY_KIB},t={ARGON2_TIME_COST},p={ARGON2_PARALLELISM}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
