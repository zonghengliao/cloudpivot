#!/usr/bin/env python3
"""B2 解析人集合 before/after —— 走应用内 `POST /business/flow/node/task/preview`（与运行期同一个 `assignee::resolve`）。"""
import json, subprocess, urllib.request, urllib.error, pathlib, sys

BASE = "http://127.0.0.1:18096/api"
DB = "cloudpivot_deadmin"
OUT = pathlib.Path("/opt/cloudpivot/docs/evidence/20261105-p0fix2b-deadmin")
lines = []


def say(m):
    print(m)
    lines.append(m)


def call(method, path, body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw or "{}")
        except Exception:
            return e.code, {"raw": raw}


def psql(sql):
    r = subprocess.run(["psql", "-h", "127.0.0.1", "-U", "e2erole", "-d", DB, "-X", "-q", "-t", "-A", "-F", "|", "-c", sql],
                       capture_output=True, text=True, env={"PGPASSWORD": "e2eiso_2026", "PATH": "/usr/bin:/bin"})
    assert r.returncode == 0, r.stderr
    return [l.split("|") for l in r.stdout.splitlines() if l]


st, res = call("POST", "/system/auth/login", {"username": "admin", "password": "Admin@123456"})
tok = res["data"]["access_token"]

NODES = [
    ("payable_pay", "n_dept", "部门主管审批"),
    ("payable_pay", "n_fin", "财务经理审批"),
    ("payable_pay", "n_board", "财务总监+总经理会签"),
    ("stock_in_cancel", "n_review", "作废复核"),
    ("stock_transfer", "n_approve", "仓管主管审批"),
]

say("# B2 解析人集合 before/after（应用内 preview，与运行期同一个 assignee::resolve）")
say("")
say("> 隔离副本库 cloudpivot_deadmin（= 生产数据 + v2 已发布）；角色成员按**生产现状**：")
say("> SUPER_ADMIN=4（admin/henry.liao/laoliu/laoliu_test）、GENERAL_MANAGER=2（henry.liao/laoliu_test，后者停用）、")
say("> WAREHOUSE_MANAGER=2（同上）、DEPT_LEADER=2（同上）、FINANCE_MANAGER=0、FINANCE_DIRECTOR=0")
say("")

rows_out = []
for flow, key, label in NODES:
    # v2 的真实配置
    cfg = psql(f"""select n.assignee_type, n.empty_assignee, n.approval_mode,
                          coalesce(string_agg(r.role_code, ',' order by a.sort_order), '')
                     from biz_flow_node n
                     join biz_flow_def_version v on v.id=n.version_id
                     join biz_flow_def d on d.id=v.def_id and v.is_current
                     left join biz_flow_node_assignee a on a.node_id=n.id
                     left join sys_role r on r.id=a.ref_id
                    where d.flow_code='{flow}' and n.node_key='{key}'
                    group by 1,2,3""")[0]
    atype, empty, mode, codes = cfg
    role_codes = [c for c in codes.split(",") if c]

    def preview(assignee):
        st, res = call("POST", "/business/flow/node/task/preview",
                       {"config": {"title_template": "P0-FIX-2B 解析对照", "on_complete": "DIRECT"}, "assignee": assignee,
                        "biz_type": "TASK_ACCEPT"}, tok)
        if st != 200: print("   !! preview", st, json.dumps(res, ensure_ascii=False)[:300])
        return st, res

    st_b, before = preview({"assignee_type": "ADMIN", "empty_assignee": "FALLBACK_ADMIN"})
    st_a, after = preview({"assignee_type": atype, "role_codes": role_codes, "empty_assignee": empty})
    b_names = [x["name"] + "(" + x["source"] + ")" for x in (before.get("data") or {}).get("assignees", [])]
    a_names = [x["name"] + "(" + x["source"] + ")" for x in (after.get("data") or {}).get("assignees", [])]
    b_trace = [t for t in (before.get("data") or {}).get("trace", []) if "命中" in t or "ADMIN" in t]
    a_trace = [t for t in (after.get("data") or {}).get("trace", []) if "命中" in t or "空" in t or "FALLBACK" in t]
    rows_out.append((flow, key, label, atype, role_codes, empty, mode, b_names, a_names))
    say(f"## {flow}.{key} —— {label}")
    say(f"- 配置：v2 `assignee_type={atype}`、绑定 `{','.join(role_codes) or '-'}`、`empty_assignee={empty}`、`approval_mode={mode or '（缺省）'}`")
    say(f"- **before**（ADMIN）：HTTP {st_b} → {b_names}")
    if b_trace:
        say(f"    · trace：{' | '.join(b_trace)}")
    say(f"- **after**（{'ROLE ' + ','.join(role_codes) if role_codes else atype}）：HTTP {st_a} → {a_names}")
    if a_trace:
        say(f"    · trace：{' | '.join(a_trace)}")
    if not a_names:
        say("- ⚠️ 解析出 0 人 → 走 `empty_assignee` 兜底")
    say("")

(OUT / "b2_resolve_before_after.txt").write_text("\n".join(lines) + "\n")
print("saved", OUT / "b2_resolve_before_after.txt")
