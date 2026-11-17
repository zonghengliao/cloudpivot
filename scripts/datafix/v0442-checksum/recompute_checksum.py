#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""按**框架口径**算迁移文件的内容指纹（`up` 脚本的 SHA-256）。

## 为什么不能直接 `sha256sum`

nik-db 的指纹不是对文件字节做哈希，而是
`vendor/nik-db/src/migrate/history.rs::checksum::content_checksum`：

    for line in sql.lines() { normalized.push_str(line); normalized.push('\\n'); }

即 **逐行哈希、每行补一个 `\\n`** —— 于是「文件末尾有没有换行」「CRLF 还是 LF」
都不影响指纹（只反映语义内容）。直接 `sha256sum` 会得到不同的值，
写进 `schema_migrations.checksum` 之后启动仍报 `MigrationChecksum`。

## 用法

    python3 scripts/datafix/v0442-checksum/recompute_checksum.py <up.sql 路径>

输出一行十六进制串（无前后缀），可直接用于 SQL 或被 shell 捕获。

## 校准依据

拿**未修改**的 V0442（commit e759628b 版本）实算，得到
`2e6fbee6a98c6ca081ada172f91623b922bad2ea459f431db50cce98b11cdafe`，
与 cloudpivot_dev 里 `schema_migrations` 记录的 `checksum` 逐字节一致
（见 docs/evidence/2026-11-17-v0442-empty-db/proof_checksum_calibration.txt）——
说明本脚本与框架实现同口径，可以放心用它算「改后」的新值。
"""

import hashlib
import sys
from pathlib import Path


def content_checksum(sql: str) -> str:
    """与 nik-db `content_checksum` 等价：逐行 + `\\n` 后 SHA-256。"""
    parts = sql.split("\n")
    if parts and parts[-1] == "":
        parts.pop()                       # 末尾换行不产生额外空行（等价 `str::lines`）
    parts = [p[:-1] if p.endswith("\r") else p for p in parts]  # CRLF → LF
    normalized = "".join(p + "\n" for p in parts)
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("按框架口径算迁移文件（up 脚本）的内容指纹。", file=sys.stderr)
        print(f"用法：{argv[0]} <up.sql 路径>", file=sys.stderr)
        return 2
    path = Path(argv[1])
    if not path.is_file():
        print(f"文件不存在：{path}", file=sys.stderr)
        return 2
    print(content_checksum(path.read_text(encoding="utf-8")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
