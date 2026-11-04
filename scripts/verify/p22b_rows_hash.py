#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""复刻 `flow::maintain` 的「行 → 快照 → hash」漂移自检（纯 SQL + Python，不依赖 Rust）。

`maintain.rs` 的判据是：把 `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee`
按 `graph.rs::snapshot()` 的确定性排序重建成快照 → canonical JSON（递归按键升序、紧凑）
→ sha256，必须等于 `biz_flow_def_version.graph_hash`。本脚本用同一套口径在库里复算一遍，
用来证明「迁移/设计器写进去的行」与「声明的 hash」一致 —— 否则每日自检会报
`RowsEdited` / `SnapshotEdited`。

canonical 实现已用 V0182 种子的 v1 hash（8f65819e…）校准过：字节级一致。

用法：
    python3 scripts/verify/p22b_rows_hash.py <db> [def_id]
"""
import hashlib
import json
import os
import subprocess
import sys

DB = sys.argv[1] if len(sys.argv) > 1 else "cloudpivot_p16md"
DEF = sys.argv[2] if len(sys.argv) > 2 else "20000000-0000-4000-8000-0000000a0182"


def canonical(value):
    """等价 `flow::graph::canonical_json`：对象键递归升序、无多余空白、数组保持既有顺序。"""
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return json.dumps(value)
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, list):
        return "[" + ",".join(canonical(v) for v in value) + "]"
    if isinstance(value, dict):
        return "{" + ",".join(
            json.dumps(k, ensure_ascii=False) + ":" + canonical(value[k]) for k in sorted(value)
        ) + "}"
    raise TypeError(type(value))


def psql(sql):
    env = dict(os.environ, PGPASSWORD=os.environ.get("PGPASSWORD", "alun"))
    r = subprocess.run(["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB,
                        "-tAF", "\x1f", "-c", sql],
                       capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise SystemExit(r.stderr)
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def snapshot_from_rows(version_id):
    nodes = []
    for (key, name, ntype, atype, levels, fvk, amode, aratio, rveto, empty_a, self_a,
         robot, join, fork, thours, taction, rbefore, rmax, cc, x, y, sort, cfg) in psql(f"""
            select node_key, node_name, node_type, coalesce(assignee_type,'∅'), superior_levels,
                   coalesce(form_var_key,'∅'), coalesce(approval_mode,'∅'),
                   coalesce(to_char(approve_ratio,'FM9999990.000'),'∅'), reject_veto,
                   empty_assignee, self_approval, allow_robot, join_mode, fork_mode,
                   coalesce(timeout_hours::text,'∅'), timeout_action,
                   coalesce(remind_before_hours::text,'∅'), max_remind_times,
                   cc_on_approved, position_x, position_y, sort_order, config::text
              from biz_flow_node where version_id='{version_id}' order by node_key"""):
        norm = lambda v: None if v == "∅" else v
        nodes.append({
            "key": key, "name": name, "node_type": ntype, "assignee_type": norm(atype),
            "approval_mode": norm(amode), "approve_ratio": norm(aratio),
            "reject_veto": rveto == "t", "join_mode": join, "fork_mode": fork,
            "form_var_key": norm(fvk), "superior_levels": int(levels),
            "empty_assignee": empty_a, "self_approval": self_a, "allow_robot": robot == "t",
            "timeout_hours": (int(thours) if thours != "∅" else None), "timeout_action": taction,
            "remind_before_hours": (int(rbefore) if rbefore != "∅" else None),
            "max_remind_times": int(rmax), "cc_on_approved": cc == "t",
            "x": int(x), "y": int(y), "sort_order": int(sort), "config": json.loads(cfg),
            "assignees": [
                {"assignee_type": a[0], "ref_id": a[1], "sort_order": int(a[2])}
                for a in psql(f"""select assignee_type, ref_id::text, sort_order
                                    from biz_flow_node_assignee
                                   where node_id = (select id from biz_flow_node
                                                     where version_id='{version_id}'
                                                       and node_key='{key}')
                                   order by assignee_type, ref_id""")
            ],
        })
    edges = []
    for (key, fr, to, cond, label, pri, isdef) in psql(f"""
            select e.edge_key, fn.node_key, tn.node_key, e.condition_expr::text,
                   coalesce(e.condition_label,'∅'), e.priority, e.is_default
              from biz_flow_edge e
              join biz_flow_node fn on fn.id = e.from_node_id
              join biz_flow_node tn on tn.id = e.to_node_id
             where e.version_id='{version_id}'
             order by fn.node_key, tn.node_key, e.edge_key"""):
        edges.append({"key": key, "from": fr, "to": to,
                      "condition": (json.loads(cond) if cond else None),
                      "label": None if label == "∅" else label,
                      "priority": int(pri), "is_default": isdef == "t"})
    return {"schema_version": 1, "nodes": nodes, "edges": edges}


def main():
    rows = psql(f"select id, version_no, version_status, graph_hash from biz_flow_def_version "
                f"where def_id='{DEF}' order by version_no")
    if not rows:
        print(f"流程定义 {DEF} 没有版本行")
        return 2
    drifted = 0
    for vid, vno, status, stored in rows:
        snap = snapshot_from_rows(vid)
        computed = hashlib.sha256(canonical(snap).encode()).hexdigest()
        same = computed == stored
        drifted += 0 if same else 1
        print(f"v{vno} [{status}] 节点 {len(snap['nodes'])} 边 {len(snap['edges'])}")
        print(f"  stored   = {stored}")
        print(f"  computed = {computed}")
        print(f"  一致     = {same}")
    print(f"\n drifted = {drifted}")
    return 0 if drifted == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
