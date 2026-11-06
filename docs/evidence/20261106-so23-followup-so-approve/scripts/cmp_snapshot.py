#!/usr/bin/env python3
"""比对 so_approve v1（种下的）/ v2（应用重新发布生成）的快照，生成 canonical 文本。"""
import hashlib
import json
import subprocess

DB = "cloudpivot_pflow1a"


def q(sql):
    return subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-tA", "-c", sql],
        capture_output=True, text=True, env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    ).stdout


def canon(v):
    return json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


v1 = json.loads(q("select graph_snapshot::text from biz_flow_def_version where id='20000000-0000-4000-8000-0000000d0282'"))
v2 = json.loads(q("select graph_snapshot::text from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000d0281' and version_no=2"))

print("v1 edges order:", [e["key"] for e in v1["edges"]])
print("v2 edges order:", [e["key"] for e in v2["edges"]])

c1 = canon(v1)
c2 = canon(v2)
print("v1 canon hash:", hashlib.sha256(c1.encode()).hexdigest())
print("v2 canon hash:", hashlib.sha256(c2.encode()).hexdigest())
print("stored v1 hash:", q("select graph_hash from biz_flow_def_version where id='20000000-0000-4000-8000-0000000d0282'").strip())
print("stored v2 hash:", q("select graph_hash from biz_flow_def_version where def_id='20000000-0000-4000-8000-0000000d0281' and version_no=2").strip())

v1s = {"schema_version": v1["schema_version"],
       "nodes": sorted(v1["nodes"], key=lambda n: n["key"]),
       "edges": sorted(v1["edges"], key=lambda e: (e["from"], e["to"], e["key"]))}
c1s = canon(v1s)
print("v1+sort canon hash:", hashlib.sha256(c1s.encode()).hexdigest())
print("len v1sorted/v2:", len(c1s), len(c2))
print("v1sorted == v2:", c1s == c2)

open("/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81/v2_canon.txt", "w").write(c2)
open("/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81/v1_sorted.txt", "w").write(c1s)
