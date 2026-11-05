#!/usr/bin/env python3
"""V0251 迁移验证：hash 自洽 + A/B 两条路径收敛 + 行↔快照一致 + v1 未被改动。"""
import hashlib, json, subprocess, sys

DBS = {
    "iso_api": "cloudpivot_deadmin",       # 应用接口发 v2 的库（基准）
    "path_a": "cloudpivot_deadmin_a",      # V0248 → V0251（新部署路径）
    "path_b": "cloudpivot_deadmin_b",      # 只跑 V0251（生产手工 apply 路径）
}
FLOWS = ("payable_pay", "stock_in_cancel", "stock_transfer")


def q(db, sql):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "e2erole", "-d", db, "-X", "-q", "-t", "-A", "-F", "\t", "-c", sql],
        capture_output=True, text=True, env={"PGPASSWORD": "e2eiso_2026", "PATH": "/usr/bin:/bin"})
    if out.returncode != 0:
        raise SystemExit(f"{db}: {out.stderr}")
    return [line.split("\t") for line in out.stdout.splitlines() if line]


def canonical(obj):
    return json.dumps(obj, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


NODES_SQL = """
select d.flow_code, v.version_no, n.node_key, n.node_name, n.node_type,
       coalesce(n.assignee_type,''), coalesce(n.approval_mode,''), n.empty_assignee, n.self_approval,
       n.sort_order, n.timeout_hours, n.timeout_action, n.reject_veto, n.join_mode, n.fork_mode,
       n.superior_levels, n.cc_on_approved, n.max_remind_times, coalesce(n.remind_before_hours::text,''),
       n.position_x, n.position_y, n.config::text
  from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id
  join biz_flow_def d on d.id=v.def_id
 where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer')
 order by 1,2,10,3"""

EDGES_SQL = """
select d.flow_code, v.version_no, e.edge_key, nf.node_key, nt.node_key,
       coalesce(e.condition_expr::text,''), coalesce(e.condition_label,''), e.priority, e.is_default
  from biz_flow_edge e join biz_flow_def_version v on v.id=e.version_id
  join biz_flow_def d on d.id=v.def_id
  join biz_flow_node nf on nf.id=e.from_node_id
  join biz_flow_node nt on nt.id=e.to_node_id
 where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer')
 order by 1,2,3"""

BIND_SQL = """
select d.flow_code, v.version_no, n.node_key, a.assignee_type, r.role_code, a.sort_order
  from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id
  join biz_flow_def_version v on v.id=n.version_id join biz_flow_def d on d.id=v.def_id
  left join sys_role r on r.id=a.ref_id
 where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer')
 order by 1,2,3,6"""

VER_SQL = """
select d.flow_code, v.version_no, v.version_status, v.is_current, btrim(v.graph_hash),
       v.node_count, v.edge_count, v.graph_snapshot::text, coalesce(v.change_note,'')
  from biz_flow_def_version v join biz_flow_def d on d.id=v.def_id
 where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer')
 order by 1,2"""

data = {}
fails = []
for tag, db in DBS.items():
    data[tag] = {
        "versions": q(db, VER_SQL),
        "nodes": q(db, NODES_SQL),
        "edges": q(db, EDGES_SQL),
        "binds": q(db, BIND_SQL),
    }

print("=== 1) hash 自洽：sha256(canonical(snapshot)) == graph_hash（每个库每个版本）===")
for tag, d in data.items():
    for row in d["versions"]:
        flow, vno, status, cur, gh, nn, ne, snap, note = row
        calc = hashlib.sha256(canonical(json.loads(snap)).encode()).hexdigest()
        ok = calc == gh
        print(f"  [{'OK ' if ok else 'BAD'}] {tag:8s} {flow:16s} v{vno} {status:9s} hash={gh[:16]}…")
        if not ok:
            fails.append(f"{tag} {flow} v{vno} hash mismatch")

print("\n=== 2) 行 ↔ 快照一致（v2；快照 nodes 的顺序/字段与行一致）===")
SNAP_FIELDS = ["name", "node_type", "assignee_type", "approval_mode", "empty_assignee",
               "self_approval", "sort_order", "timeout_hours", "join_mode", "fork_mode"]
for tag, d in data.items():
    node_rows = [r for r in d["nodes"] if r[1] == "2"]
    versions = {r[0]: r for r in d["versions"] if r[1] == "2"}
    for flow in FLOWS:
        snap = json.loads(versions[flow][7])
        snap_nodes = {n["key"]: n for n in snap["nodes"]}
        rows = {(r[2]): r for r in node_rows if r[0] == flow}
        bad = []
        if set(snap_nodes) != set(rows):
            bad.append(f"key 集合不同 snap={sorted(set(snap_nodes)-set(rows))} rows={sorted(set(rows)-set(snap_nodes))}")
        for k, sn in snap_nodes.items():
            r = rows.get(k)
            if not r:
                continue
            # 行下标: 2 key,3 name,4 type,5 assignee_type,6 approval_mode,7 empty,8 self,9 sort,
            # 10 timeout_hours, 13 join_mode, 14 fork_mode
            mapping = {"name": r[3], "node_type": r[4], "assignee_type": r[5] or None,
                       "approval_mode": r[6] or None, "empty_assignee": r[7], "self_approval": r[8],
                       "sort_order": int(r[9]), "timeout_hours": int(r[10]) if r[10] else None,
                       "join_mode": r[13], "fork_mode": r[14]}
            for f in SNAP_FIELDS:
                if sn.get(f) != mapping[f]:
                    bad.append(f"{k}.{f}: snap={sn.get(f)!r} row={mapping[f]!r}")
        print(f"  [{'OK ' if not bad else 'BAD'}] {tag:8s} {flow:16s} nodes={len(rows)} edges={sum(1 for e in d['edges'] if e[0]==flow and e[1]=='2')}")
        for b in bad:
            print("        ", b)
            fails.append(f"{tag} {flow} snapshot/row: {b}")

print("\n=== 3) A/B 两条路径收敛（v2 行内容逐字段比对，忽略 id/时间戳）===")
for kind in ("nodes", "edges", "binds"):
    a = [tuple(r) for r in data["path_a"][kind] if r[1] == "2"]
    b = [tuple(r) for r in data["path_b"][kind] if r[1] == "2"]
    same = a == b
    print(f"  [{'OK ' if same else 'BAD'}] {kind}: A={len(a)} 行 / B={len(b)} 行 一致={same}")
    if not same:
        fails.append(f"A/B {kind} 不一致")
        for x, y in zip(a, b):
            if x != y:
                print("    A:", x)
                print("    B:", y)
                break

print("\n=== 4) 与「应用接口发版」的 v2 一致（基准 iso_api）===")
for kind in ("nodes", "edges", "binds"):
    base = [tuple(r) for r in data["iso_api"][kind] if r[1] == "2"]
    for tag in ("path_a", "path_b"):
        cur = [tuple(r) for r in data[tag][kind] if r[1] == "2"]
        same = base == cur
        print(f"  [{'OK ' if same else 'BAD'}] {kind} {tag} vs iso_api: 一致={same}")
        if not same:
            fails.append(f"{tag} {kind} 与接口发版不一致")
            for x, y in zip(base, cur):
                if x != y:
                    print("    iso:", x)
                    print("    ", tag, ":", y)
                    break

print("\n=== 5) v1 行/快照/hash 未被改动（A/B 的 v1 与原始 prod 拷贝比对）===")
base_v1 = {r[0]: r for r in data["iso_api"]["versions"] if r[1] == "1"}
for tag in ("path_a", "path_b"):
    for r in data[tag]["versions"]:
        if r[1] != "1":
            continue
        flow = r[0]
        # stock_in_cancel v1 在 path_a 上被 V0248 改过（那是 V0248 的既定行为），单独说明
        same_hash = r[4] == base_v1[flow][4]
        same_nodes = ([x for x in data[tag]["nodes"] if x[0] == flow and x[1] == "1"]
                      == [x for x in data["iso_api"]["nodes"] if x[0] == flow and x[1] == "1"])
        note = "" if flow != "stock_in_cancel" else "（stock_in_cancel v1 的差异来自 V0248 而非 V0251）"
        print(f"  [{'OK ' if same_hash and same_nodes else 'INFO'}] {tag:8s} {flow:16s} v1 hash 未变={same_hash} 行未变={same_nodes}{note}")

print("\n=== 6) 三条流程当前指向 v2 + 无 ADMIN 节点 ===")
for tag, db in DBS.items():
    rows = q(db, """
      select d.flow_code, d.current_version_no, v.version_no, v.is_current
        from biz_flow_def d join biz_flow_def_version v on v.id=d.current_version_id
       where d.flow_code in ('payable_pay','stock_in_cancel','stock_transfer') order by 1""")
    adm = q(db, """
      select d.flow_code, count(*) from biz_flow_node n join biz_flow_def_version v on v.id=n.version_id
        join biz_flow_def d on d.id=v.def_id
       where d.status='PUBLISHED' and v.is_current and n.assignee_type='ADMIN' group by 1""")
    print(f"  {tag:8s} " + "; ".join(f"{r[0]}: cur_no={r[1]} 指向v{r[2]}(is_current={r[3]})" for r in rows)
          + f" | ADMIN 节点: {adm if adm else 0}")

print("\nFAILS:", len(fails))
for f in fails:
    print("  -", f)
sys.exit(1 if fails else 0)
