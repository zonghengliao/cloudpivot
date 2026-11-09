"""V0355 之后校验 budget_approve v2 的「行 ↔ 快照 ↔ graph_hash」三者一致。

判据与 Rust `graph::FlowGraph::snapshot()` / `snapshot_json()` / `graph_hash()` 逐字同义：
  · 节点按 key 升序、边按 (from,to,key) 升序、审批人按 (assignee_type,ref_id) 升序；
  · approve_ratio 序列化成 3 位小数；fallback_user_id / fallback_role_id 为 None 时不出现；
  · canonical JSON = 递归按键字典序、紧凑无空白；sha256 小写十六进制。

用法：psql ... -f fix1_graph_consistency.sql > dump.json && python3 fix1_graph_check.py dump.json
"""
import hashlib
import json
import sys

NODE_FIELDS_ORDER = [
    "key", "name", "node_type", "assignee_type", "approval_mode", "approve_ratio",
    "reject_veto", "join_mode", "fork_mode", "form_var_key", "superior_levels",
    "empty_assignee", "self_approval", "allow_robot", "fallback_user_id", "fallback_role_id",
    "timeout_hours", "timeout_action", "remind_before_hours", "max_remind_times",
    "cc_on_approved", "x", "y", "sort_order", "config", "assignees",
]


def canon(v):
    if isinstance(v, dict):
        return "{" + ",".join(json.dumps(k, ensure_ascii=False) + ":" + canon(v[k]) for k in sorted(v)) + "}"
    if isinstance(v, list):
        return "[" + ",".join(canon(x) for x in v) + "]"
    if v is None:
        return "null"
    if v is True:
        return "true"
    if v is False:
        return "false"
    if isinstance(v, (int, float)):
        return json.dumps(v)
    return json.dumps(v, ensure_ascii=False)


def build_snapshot(rows):
    nodes = []
    for n in rows["nodes"]:
        node = {}
        for field in NODE_FIELDS_ORDER:
            value = n.get(field)
            if field == "approve_ratio" and value is not None:
                value = f"{float(value):.3f}"
            if field in ("fallback_user_id", "fallback_role_id") and value is None:
                continue
            if field == "assignees":
                value = sorted(
                    (
                        {"assignee_type": a["assignee_type"], "ref_id": a["ref_id"],
                         "sort_order": a["sort_order"]}
                        for a in (n.get("assignees") or [])
                    ),
                    key=lambda a: (a["assignee_type"], a["ref_id"]),
                )
            node[field] = value
        nodes.append(node)
    nodes.sort(key=lambda x: x["key"])

    edges = []
    for e in rows["edges"]:
        edges.append({
            "key": e["key"], "from": e["from"], "to": e["to"], "condition": e["condition"],
            "label": e["label"], "priority": e["priority"], "is_default": e["is_default"],
        })
    edges.sort(key=lambda x: (x["from"], x["to"], x["key"]))
    return {"schema_version": 1, "nodes": nodes, "edges": edges}


def main():
    raw = open(sys.argv[1], encoding="utf-8").read()
    doc = json.loads(raw[raw.index("{"):])
    rebuilt = build_snapshot(doc)
    stored = doc["snapshot"]
    hash_rows = hashlib.sha256(canon(rebuilt).encode()).hexdigest()
    hash_stored = hashlib.sha256(canon(stored).encode()).hexdigest()

    print(f"stored graph_hash      = {doc['hash']}")
    print(f"sha256(行重建快照)      = {hash_rows}")
    print(f"sha256(库里快照)        = {hash_stored}")
    ok = True
    if hash_rows != doc["hash"]:
        ok = False
        print("❌ 行重建出来的 hash 与 graph_hash 不一致（行被直接改过 / 快照与行漂移）")
    if hash_stored != doc["hash"]:
        ok = False
        print("❌ 库里的 graph_snapshot 与 graph_hash 不一致")
    if rebuilt != stored:
        ok = False
        print("❌ 行重建快照与库里快照结构不等（逐节点/边比对见下）")
        for a, b in zip(rebuilt["nodes"], stored["nodes"]):
            if a != b:
                print(f"   · 节点 {a.get('key')} 行={json.dumps(a, ensure_ascii=False, sort_keys=True)}")
                print(f"     快照={json.dumps(b, ensure_ascii=False, sort_keys=True)}")
        if rebuilt["edges"] != stored["edges"]:
            print("   · 边不一致")
    node_dept = [n for n in rebuilt["nodes"] if n["key"] == "n_dept"][0]
    print(f"n_dept: assignee_type={node_dept['assignee_type']} approval_mode={node_dept['approval_mode']} "
          f"superior_levels={node_dept['superior_levels']} assignees={node_dept['assignees']}")
    print(f"节点 {len(rebuilt['nodes'])} / 边 {len(rebuilt['edges'])}")
    print("RESULT:", "PASS 行↔快照↔hash 三者一致" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
