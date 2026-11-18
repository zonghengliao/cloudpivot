#!/usr/bin/env python3
"""report/mod.rs 反证用：把 purge_fixtures 的两条腿之一改成永不匹配（幂等，带备份与还原校验）。

用法：
  python3 mutate.py a           # 把「删 VOUCHER 状态日志」那条腿的 entity_type 改成永不匹配
  python3 mutate.py b           # 把「删夹具凭证」那条腿的 voucher_word 改成永不匹配
  python3 mutate.py restore     # 从备份还原，并打印 sha256 供逐字节对照
  python3 mutate.py sha         # 只打印当前 sha256
"""
import hashlib
import pathlib
import sys

SRC = pathlib.Path("/opt/cloudpivot/backend/app/src/business/erp/report/mod.rs")
BAK = pathlib.Path("/root/.hermes/profiles/backend-developer/cache/scratch/mod.rs.orig")

LOGS_LEG = """            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' \\
              AND entity_id IN (SELECT id FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}')"
"""
LOGS_LEG_MUT = """            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER-NEVER' \\
              AND entity_id IN (SELECT id FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}')"
"""

VOUCHER_LEG = """        let vouchers_sql = format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'");
"""
VOUCHER_LEG_MUT = """        let vouchers_sql = format!("DELETE FROM biz_voucher WHERE voucher_word = 'NEVER-{FIXTURE_WORD}'");
"""


def sha(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "sha"
    text = SRC.read_text(encoding="utf-8")
    if mode == "sha":
        print(sha(SRC))
        return 0
    if mode == "restore":
        SRC.write_bytes(BAK.read_bytes())
        print(f"restored sha256={sha(SRC)} bak_sha256={sha(BAK)}")
        return 0
    if not BAK.exists():
        BAK.write_text(text, encoding="utf-8")
        print(f"backup -> {BAK} sha256={sha(BAK)}")
        text = BAK.read_text(encoding="utf-8")
    if mode == "a":
        if LOGS_LEG not in text:
            print("ERROR: 找不到『删 VOUCHER 状态日志』那条腿（源码已变？）")
            return 2
        SRC.write_text(text.replace(LOGS_LEG, LOGS_LEG_MUT, 1), encoding="utf-8")
        print("mutated A: logs leg -> entity_type = 'VOUCHER-NEVER'")
        return 0
    if mode == "b":
        if VOUCHER_LEG not in text:
            print("ERROR: 找不到『删夹具凭证』那条腿（源码已变？）")
            return 2
        SRC.write_text(text.replace(VOUCHER_LEG, VOUCHER_LEG_MUT, 1), encoding="utf-8")
        print("mutated B: voucher leg -> voucher_word = 'NEVER-ERPR1'")
        return 0
    print(f"unknown mode: {mode}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
