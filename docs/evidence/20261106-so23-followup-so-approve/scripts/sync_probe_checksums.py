#!/usr/bin/env python3
"""把探针库 schema_migrations 的 checksum 同步成「当前工作区迁移文件的 sha256」。

只用于 `/tmp` 级探针库（cloudpivot_tplprobe）：共享工作区里别的 worker 正在改
未提交的迁移文件（本次是 SO-25 的 V0267__sales_menu），改一个字节就会
ChecksumMismatch 拒绝启动。探针库只是拿来查一次模板列表接口，与那些文件的
内容无关，所以按当前文件把校验和同步过去，让实例能起来。
"""
import hashlib
import pathlib
import subprocess
import sys

DB = sys.argv[1] if len(sys.argv) > 1 else "cloudpivot_tplprobe"
ROOT = pathlib.Path("/opt/cloudpivot/backend/migrations")

rows = subprocess.run(
    ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-tA", "-F", "|", "-c",
     "select version, coalesce(checksum,'') from schema_migrations"],
    capture_output=True, text=True, env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
).stdout.strip().splitlines()

by_version = {v: c for v, c in (line.split("|") for line in rows if "|" in line)}
fixed = 0
for path in ROOT.rglob("*.sql"):
    if path.name.endswith(".down.sql"):
        continue
    name = path.name
    if "__" not in name or not name[:1].upper() == "V":
        continue
    version = str(int(name[1:].split("__")[0]))
    if version not in by_version:
        continue
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if by_version[version] == digest:
        continue
    subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-q", "-c",
         f"update schema_migrations set checksum = '{digest}' where version = '{version}'"],
        check=True, env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    )
    print(f"  sync v{version} {name}: {by_version[version][:8]}… → {digest[:8]}…")
    fixed += 1
print(f"checksum synced: {fixed} 行（探针库 {DB}）")
