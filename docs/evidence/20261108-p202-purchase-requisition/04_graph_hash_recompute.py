#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
从数据库里的 pr_approve 节点/边重建 canonical 快照，与 biz_flow_def_version.graph_hash 对拍。

口径与 `flow::graph::canonical_json` 一致：对象键**递归升序**、无空白、数组按
(nodes: key) / (edges: from,to,key) 排序。任何字段映射写错（比如把 `approve_ratio`
写成数字、或漏掉 `assignees`）都会让 hash 对不上。
"""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys

DB = os.environ.get("P202_DB", "cloudpivot_prq03")
VERSION_ID = "20000000-0000-4000-8000-0000000d0332"


def sql(statement: str) -> str:
    env = dict(os.environ, PGPASSWORD="alun")
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atc", statement],
        capture_output=True, text=True, env=env, check=False,
    )
    if out.returncode != 0:
        raise RuntimeError(out.stderr)
    return out.stdout.strip()


def main() -> int:
    nodes_raw = sql(f"""
    SELECT coalesce(json_agg(json_build_object(
             'key', n.node_key, 'name', n.node_name, 'node_type', n.node_type,
             'assignee_type', n.assignee_type, 'approval_mode', n.approval_mode,
             'approve_ratio', n.approve_ratio, 'reject_veto', n.reject_veto,
             'join_mode', n.join_mode, 'fork_mode', n.fork_mode,
             'form_var_key', n.form_var_key, 'superior_levels', n.superior_levels,
             'empty_assignee', n.empty_assignee, 'self_approval', n.self_approval,
             'allow_robot', n.allow_robot, 'timeout_hours', n.timeout_hours,
             'timeout_action', n.timeout_action, 'remind_before_hours', n.remind_before_hours,
             'max_remind_times', n.max_remind_times, 'cc_on_approved', n.cc_on_approved,
             'x', n.position_x, 'y', n.position_y, 'sort_order', n.sort_order,
             'config', n.config,
             'assignees', coalesce((
                SELECT json_agg(json_build_object(
                        'assignee_type', a.assignee_type, 'ref_id', a.ref_id::text,
                        'sort_order', a.sort_order) ORDER BY a.assignee_type, a.ref_id::text)
                  FROM biz_flow_node_assignee a WHERE a.node_id = n.id), '[]'::json)
           ) ORDER BY n.node_key), '[]'::json)
      FROM biz_flow_node n WHERE n.version_id = '{VERSION_ID}'::uuid
    """)
    edges_raw = sql(f"""
    SELECT coalesce(json_agg(json_build_object(
             'key', e.edge_key, 'from', f.node_key, 'to', t.node_key,
             'condition', e.condition_expr, 'label', e.condition_label,
             'priority', e.priority, 'is_default', e.is_default)
             ORDER BY f.node_key, t.node_key, e.edge_key), '[]'::json)
      FROM biz_flow_edge e
      JOIN biz_flow_node f ON f.id = e.from_node_id
      JOIN biz_flow_node t ON t.id = e.to_node_id
     WHERE e.version_id = '{VERSION_ID}'::uuid
    """)
    nodes = json.loads(nodes_raw)
    edges = json.loads(edges_raw)
    for node in nodes:
        # approve_ratio 是**字符串**（保留 0.600 精度），数据库 numeric 出来的可能是数字
        if node["approve_ratio"] is not None:
            node["approve_ratio"] = f"{float(node['approve_ratio']):.3f}"
        node["assignees"] = sorted(
            node["assignees"], key=lambda a: (a["assignee_type"], a["ref_id"])
        )
    nodes.sort(key=lambda n: n["key"])
    edges.sort(key=lambda e: (e["from"], e["to"], e["key"]))
    snapshot = {"schema_version": 1, "nodes": nodes, "edges": edges}
    canonical = json.dumps(snapshot, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    recomputed = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
    stored = sql(f"SELECT graph_hash FROM biz_flow_def_version WHERE id = '{VERSION_ID}'::uuid")
    print(f"节点数 {len(nodes)} / 边数 {len(edges)}")
    print(f"库内 graph_hash   : {stored}")
    print(f"从图重建 graph_hash: {recomputed}")
    if stored != recomputed:
        print("FAIL：种子里的快照与库内真实图不一致（发布时会被判 hash 漂移）")
        return 1
    print("PASS：种子快照 = canonical 序列化(库内真实图)，重新发布不会改变 hash")
    return 0


if __name__ == "__main__":
    sys.exit(main())
