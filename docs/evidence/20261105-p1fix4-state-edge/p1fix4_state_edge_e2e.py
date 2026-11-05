#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-FIX-4 端到端验收：配一条边 → 提交审批 → 通过 → biz_flow_state_edge 有行 → 该边生效。

真机口径（**不是** mock）：起一个真实例（库 cloudpivot_p1fix4，端口 18119），全部走 HTTP：
  ① PUT /business/flow/state-edge        （禁用 STOCK_OUT DRAFT→POSTED，必须送审）
  ② POST /business/flow/task/{id}/approve（单条同意，走 approve_task 的完整回调链）
  ③ 查 biz_flow_state_edge               （必须 1 行，enabled=false）
  ④ GET /business/flow/state-edge        （effective_edges 里必须没有这条边）
  ⑤ 真单据证明边生效：建出库单草稿 →
       PATCH status=POSTED   → 必须 4xx（非法转换被拒，因为这条边被配置禁用了）
       PATCH status=PENDING_POST → 必须 2xx（合法转换照走）

三条审批通道各跑一遍（同一套断言）：
  - single     : POST /business/flow/task/{id}/approve           （单条）
  - batch-new  : POST /business/flow/task/batch-approve          （D7，逐条委托 approve_task）
  - batch-old  : POST /business/flow/task/batch                  （旧批量端点 —— 本卡修的就是它）

用法：
  python3 p1fix4_state_edge_e2e.py --reset            # 清夹具 + 跑全部三段
  python3 p1fix4_state_edge_e2e.py --only batch-old   # 只跑旧批量端点（复现断链）
"""
import argparse
import json
import subprocess
import sys
import urllib.error
import urllib.request

DB = dict(host="127.0.0.1", port="5432", user="alun", password="alun", name="cloudpivot_p1fix4")
ADMIN = ("admin", "Admin@123456")

# 实体 → (禁用哪条可选边)；三段用三条不同的边，互不干扰
EDGE_SINGLE = ("STOCK_OUT", "DRAFT", "POSTED")
EDGE_BATCH_NEW = ("STOCK_IN", "DRAFT", "POSTED")
EDGE_BATCH_OLD = ("PURCHASE_ORDER", "SUBMITTED", "CLOSED")
ALL_EDGES = [EDGE_SINGLE, EDGE_BATCH_NEW, EDGE_BATCH_OLD]

PASS = 0
FAIL = 0
LINES = []


def check(ok, label, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        line = f"PASS  {label}"
    else:
        FAIL += 1
        line = f"FAIL  {label}"
    if detail:
        line += f"   [{detail}]"
    LINES.append(line)
    print(line, flush=True)
    return ok


def psql(sql):
    env = {"PGPASSWORD": DB["password"], "PATH": "/usr/local/bin:/usr/bin:/bin"}
    run = subprocess.run(
        ["psql", "-h", DB["host"], "-p", DB["port"], "-U", DB["user"], "-d", DB["name"],
         "-Atc", sql],
        capture_output=True, text=True, env=env, timeout=60,
    )
    if run.returncode != 0:
        raise RuntimeError("psql 失败：" + run.stderr.strip()[:400])
    return run.stdout.strip()


BASE = "http://127.0.0.1:18119"


def api(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        raw = error.read().decode()
        try:
            return error.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw}


def login():
    status, body = api("POST", "/api/system/auth/login",
                       {"username": ADMIN[0], "password": ADMIN[1]})
    if status != 200 or body.get("code") != 0:
        raise SystemExit(f"登录失败：{status} {body}")
    return body["data"]["access_token"]


# ---------------- 夹具 ----------------

def reset():
    """清掉本脚本碰过的三样东西：配置行 / 配置审批单 / 审计行 / 出库单草稿。"""
    for entity, frm, to in ALL_EDGES:
        psql(f"DELETE FROM biz_flow_state_edge WHERE entity='{entity}' "
             f"AND from_status='{frm}' AND to_status='{to}'")
    psql("DELETE FROM biz_flow_task WHERE instance_id IN "
         "(SELECT id FROM biz_flow_instance WHERE biz_type='FLOW_EDGE_CFG')")
    psql("DELETE FROM biz_flow_instance_node WHERE instance_id IN "
         "(SELECT id FROM biz_flow_instance WHERE biz_type='FLOW_EDGE_CFG')")
    psql("DELETE FROM biz_flow_cc WHERE instance_id IN "
         "(SELECT id FROM biz_flow_instance WHERE biz_type='FLOW_EDGE_CFG')")
    psql("DELETE FROM biz_flow_instance WHERE biz_type='FLOW_EDGE_CFG'")
    psql("DELETE FROM biz_status_log WHERE entity_type='FLOW_EDGE_CFG'")
    psql("DELETE FROM biz_stock_out_item WHERE stock_out_id IN "
         "(SELECT id FROM biz_stock_out WHERE remark = 'P1-FIX-4 e2e')")
    psql("DELETE FROM biz_status_log WHERE entity_id IN "
         "(SELECT id FROM biz_stock_out WHERE remark = 'P1-FIX-4 e2e')")
    psql("DELETE FROM biz_stock_out WHERE remark = 'P1-FIX-4 e2e'")
    # 隔离库的验收夹具：种子 admin 带 must_change_password=TRUE（CODE-06 首登强制改密），
    # 不改就所有业务接口 403「首次登录必须先修改初始密码」。这是**测试库**，直接放行。
    psql("UPDATE sys_user SET must_change_password = FALSE WHERE username = 'admin'")


def config_rows(entity, frm, to):
    return int(psql(
        "SELECT count(*) FROM biz_flow_state_edge "
        f"WHERE entity='{entity}' AND from_status='{frm}' AND to_status='{to}' "
        "AND deleted_at IS NULL") or 0)


def instance_status(instance_id):
    return psql(f"SELECT status FROM biz_flow_instance WHERE id='{instance_id}'")


def pending_task(instance_id):
    return psql(
        "SELECT id FROM biz_flow_task "
        f"WHERE instance_id='{instance_id}' AND status='PENDING' "
        "ORDER BY created_at LIMIT 1")


# ---------------- 业务动作 ----------------

def submit(token, entity, frm, to, enabled=False):
    status, body = api("PUT", "/api/business/flow/state-edge", {
        "entity": entity, "from_status": frm, "to_status": to,
        "enabled": enabled, "requires_reason": False,
        "label": None, "auth_code": None, "confirm": True,
        "reason": "P1-FIX-4 端到端：验证配置审批通过后才落边",
    }, token)
    return status, body


def approve_single(token, task_id):
    return api("POST", f"/api/business/flow/task/{task_id}/approve",
               {"comment": "同意（P1-FIX-4 e2e）"}, token)


def approve_batch_new(token, task_id):
    return api("POST", "/api/business/flow/task/batch-approve",
               {"ids": [task_id], "decision": "APPROVE", "comment": "同意"}, token)


def approve_batch_old(token, task_id):
    return api("POST", "/api/business/flow/task/batch",
               {"task_ids": [task_id], "action": "APPROVE", "comment": "同意"}, token)


def effective_edges(token, entity):
    status, body = api("GET", f"/api/business/flow/state-edge?entity={entity}", None, token)
    data = body.get("data") or {}
    rows = data.get("list") or []
    if not rows:
        return None, None, status, body
    return rows[0].get("effective_edges") or [], rows[0].get("configs") or [], status, body


def has_edge(edges, frm, to):
    return any(edge.get("from") == frm and edge.get("to") == to for edge in (edges or []))


def create_stock_out(token):
    warehouse = psql("SELECT id FROM biz_warehouse WHERE warehouse_code='WH01' LIMIT 1")
    material = psql("SELECT id FROM biz_material WHERE material_code='M-W1A-001' LIMIT 1")
    status, body = api("POST", "/api/business/erp/stock-out", {
        "out_type": "ISSUE", "warehouse_id": warehouse,
        "reason": "P1-FIX-4 e2e", "remark": "P1-FIX-4 e2e",
        "items": [{"material_id": material, "qty": "1"}],
    }, token)
    if status != 200 or body.get("code") != 0:
        return None, status, body
    return body["data"]["id"], status, body


def patch_stock_out(token, out_id, target):
    return api("PATCH", f"/api/business/erp/stock-out/{out_id}/status",
               {"status": target, "reason": "P1-FIX-4 e2e"}, token)


# ---------------- 三段 ----------------

def run_case(token, label, edge, approver):
    """配一条边 → 送审 → 通过 → 落行 → 边生效。返回 True/False 由 check 累计。"""
    entity, frm, to = edge
    print(f"\n===== 场景 {label}：{entity} {frm}→{to}（审批通道 {approver.__name__}）=====",
          flush=True)

    edges, configs, _, _ = effective_edges(token, entity)
    check(has_edge(edges, frm, to), f"[{label}] 基线：这条边在 effective_edges 里（未配置前可用）")
    check(config_rows(entity, frm, to) == 0, f"[{label}] 基线：biz_flow_state_edge 0 行")

    status, body = submit(token, entity, frm, to)
    data = body.get("data") or {}
    if not check(status == 200 and data.get("applied") is False,
                 f"[{label}] 提交配置 → 必须送审不直接生效", f"http={status} {body.get('msg')}"):
        return
    instance_id = data["instance_id"]
    check(str(data.get("instance_status")).upper() == "PENDING",
          f"[{label}] 审批单状态 PENDING", data.get("instance_no", ""))
    check(config_rows(entity, frm, to) == 0,
          f"[{label}] 审批通过前：配置 0 行（没提前生效）")

    task_id = pending_task(instance_id)
    if not check(bool(task_id), f"[{label}] 审批人待办已生成（FALLBACK_ADMIN → 超管）"):
        return

    status, body = approver(token, task_id)
    check(status == 200 and body.get("code") == 0,
          f"[{label}] 审批通过 HTTP 200", f"http={status} {body.get('msg')}")
    check(instance_status(instance_id) == "APPROVED", f"[{label}] 审批单落终态 APPROVED")

    # ★ 本卡的核心断言：审批通过 → 落边
    rows = config_rows(entity, frm, to)
    check(rows == 1, f"[{label}] 审批通过后 biz_flow_state_edge 有 1 行（配置落库）",
          f"count={rows}")
    if rows == 1:
        enabled = psql("SELECT enabled FROM biz_flow_state_edge "
                       f"WHERE entity='{entity}' AND from_status='{frm}' AND to_status='{to}' "
                       "AND deleted_at IS NULL")
        check(enabled == "f", f"[{label}] 落库行的 enabled=false", f"enabled={enabled}")
        applied_log = psql(
            "SELECT count(*) FROM biz_status_log WHERE entity_type='FLOW_EDGE_CFG' "
            f"AND entity_no='{entity} {frm}>{to}' AND from_status='APPLIED'")
        check(int(applied_log or 0) >= 1,
              f"[{label}] 审计里有 APPLIED 行（能回答「是哪张审批单让它生效」）",
              f"rows={applied_log}")

    edges, configs, _, _ = effective_edges(token, entity)
    check(not has_edge(edges, frm, to),
          f"[{label}] 生效：effective_edges 里这条边消失")
    check(any(row.get("from") == frm and row.get("to") == to and row.get("enabled") is False
              for row in (configs or [])),
          f"[{label}] 管理面 configs 里能看到 enabled=false 的配置行")


def run_stock_out_proof(token):
    """真单据证明「配置的边真的生效」：非法转换被拒、合法转换通过。"""
    print("\n===== 真单据证明：STOCK_OUT DRAFT→POSTED 被禁用后 =====", flush=True)
    out_id, status, body = create_stock_out(token)
    if not check(bool(out_id), "建出库单草稿（HTTP 200）", f"http={status} {body.get('msg')}"):
        return
    status, body = patch_stock_out(token, out_id, "POSTED")
    msg = str(body.get("msg") or "")
    check(status >= 400,
          "非法转换 DRAFT→POSTED 被拒（配置禁用的边不能走）", f"http={status} msg={msg[:90]}")
    check("POSTED" in msg or "边" in msg or "流转" in msg,
          "拒绝原因指向「这条流转不允许」", msg[:90])
    status, body = patch_stock_out(token, out_id, "PENDING_POST")
    check(status == 200 and body.get("code") == 0,
          "合法转换 DRAFT→PENDING_POST 通过（只禁了那一条边）",
          f"http={status} {body.get('msg')}")


def main():
    global BASE
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default=BASE)
    parser.add_argument("--reset", action="store_true", help="先清夹具")
    parser.add_argument("--reset-only", action="store_true",
                        help="只清夹具就退出（随后请重启实例：首登改密标记在进程内缓存 60s）")
    parser.add_argument("--only", default="", help="single / batch-new / batch-old")
    args = parser.parse_args()
    BASE = args.base.rstrip("/")

    if args.reset or args.reset_only:
        reset()
        print("夹具已清理（admin.must_change_password → FALSE）", flush=True)
        if args.reset_only:
            return 0
    token = login()
    psql_out = psql("SELECT count(*) FROM biz_flow_state_edge WHERE deleted_at IS NULL")
    print(f"库指纹：biz_flow_state_edge 存活行 = {psql_out}", flush=True)

    cases = [
        ("single", EDGE_SINGLE, approve_single),
        ("batch-new", EDGE_BATCH_NEW, approve_batch_new),
        ("batch-old", EDGE_BATCH_OLD, approve_batch_old),
    ]
    for label, edge, approver in cases:
        if args.only and args.only != label:
            continue
        run_case(token, label, edge, approver)
        if label == "single" and not args.only:
            run_stock_out_proof(token)

    print(f"\n===== 汇总：PASS={PASS} FAIL={FAIL} =====", flush=True)
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
