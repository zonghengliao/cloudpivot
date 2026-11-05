"""在隔离副本库上用**应用内真实接口**发 v2：另存草稿 → 改图 → 发布。

- payable_pay   v2: n_dept=DEPT_LEADER / n_fin=FINANCE_MANAGER / n_board=FINANCE_DIRECTOR+GENERAL_MANAGER
- stock_in_cancel v2: n_review=WAREHOUSE_MANAGER（empty_assignee 保持 V0248 的 ERROR）
- stock_transfer  v2: n_approve=WAREHOUSE_MANAGER，节点名改「仓管主管审批」
"""
import json, urllib.request, urllib.error, pathlib

BASE = "http://127.0.0.1:18096/api"
OUT = pathlib.Path("/opt/cloudpivot/docs/evidence/20261105-p0fix2b-deadmin")
OUT.mkdir(parents=True, exist_ok=True)
TOK = None

ROLE = {
    "DEPT_LEADER": "20000000-0000-4000-8000-0000000f0001",
    "FINANCE_MANAGER": "20000000-0000-4000-8000-0000000f0005",
    "FINANCE_DIRECTOR": "20000000-0000-4000-8000-0000000f0006",
    "GENERAL_MANAGER": "20000000-0000-4000-8000-0000000f0003",
    "WAREHOUSE_MANAGER": "20000000-0000-4000-8000-0000000f0004",
}


def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if TOK:
        req.add_header("Authorization", "Bearer " + TOK)
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return r.status, json.loads(r.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw or "{}")
        except Exception:
            return e.code, {"raw": raw}


def login():
    global TOK
    st, res = call("POST", "/system/auth/login", {"username": "admin", "password": "Admin@123456"})
    assert st == 200, (st, res)
    TOK = res["data"]["access_token"]


def role_bind(code, order=1):
    return {"type": "ROLE", "ref_id": ROLE[code], "sort_order": order}


PLAN = {
    "payable_pay": {
        "n_dept": {"assignee_type": "ROLE", "assignees": [role_bind("DEPT_LEADER")]},
        "n_fin": {"assignee_type": "ROLE", "assignees": [role_bind("FINANCE_MANAGER")]},
        "n_board": {
            "assignee_type": "ROLE",
            "assignees": [role_bind("FINANCE_DIRECTOR", 1), role_bind("GENERAL_MANAGER", 2)],
        },
    },
    "stock_in_cancel": {
        "n_review": {"assignee_type": "ROLE", "assignees": [role_bind("WAREHOUSE_MANAGER")]},
    },
    "stock_transfer": {
        "n_approve": {
            "assignee_type": "ROLE",
            "assignees": [role_bind("WAREHOUSE_MANAGER")],
            "name": "仓管主管审批",
        },
    },
}
NOTE = "P0-FIX-2B-B 去 ADMIN 化：审批人来源由「全部超管」改为业务角色（不含其它语义改动）"

login()
st, defs = call("GET", "/business/flow/def/page?page=1&page_size=100")
bycode = {d["flow_code"]: d for d in (defs.get("data") or {}).get("list") or []}
report = {}
for code, plan in PLAN.items():
    d = bycode[code]
    st, before = call("GET", "/business/flow/version/" + d["current_version_id"])
    assert st == 200, (code, st, before)
    b = before["data"]
    (OUT / f"v1_{code}.json").write_text(json.dumps(b, ensure_ascii=False, indent=2))
    print(f"[{code}] v1 hash={b['graph_hash']} nodes={b['node_count']} edges={b['edge_count']}")

    st, draft = call("POST", f"/business/flow/def/{d['id']}/draft", {"change_note": NOTE})
    assert st == 200, (code, "draft", st, draft)
    vid = draft["data"]["version_id"]
    vno = draft["data"]["version_no"]
    print(f"[{code}] draft v{vno} {vid}")

    st, det = call("GET", "/business/flow/version/" + vid)
    assert st == 200, (code, "detail", st, det)
    g = det["data"]
    nodes = g["nodes"]
    touched = []
    for n in nodes:
        if n["key"] in plan:
            patch = plan[n["key"]]
            for k, v in patch.items():
                n[k] = v
            touched.append(n["key"])
    assert sorted(touched) == sorted(plan.keys()), (code, touched)
    payload = {"nodes": nodes, "edges": g["edges"], "change_note": NOTE}
    st, saved = call("PUT", f"/business/flow/version/{vid}/graph", payload)
    assert st == 200, (code, "save", st, saved)
    print(f"[{code}] saved warnings={saved['data'].get('warnings')}")

    st, pub = call("POST", f"/business/flow/def/{d['id']}/publish")
    assert st == 200, (code, "publish", st, pub)
    print(f"[{code}] published v{pub['data']['version_no']} hash={pub['data'].get('graph_hash')}")
    report[code] = {"def_id": d["id"], "v1_id": d["current_version_id"], "v1_hash": b["graph_hash"],
                    "v2_id": vid, "v2_no": vno, "publish": pub["data"]}

(OUT / "build_v2_report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
print(json.dumps(report, ensure_ascii=False, indent=2))
