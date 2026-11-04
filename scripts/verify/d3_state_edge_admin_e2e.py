#!/usr/bin/env python3
"""D3 真机验收：状态机配置的「管理面 + 审批化 + 前端按钮联动」（卡 t_217a95ad）。

用法（隔离实例已起在 18096 且库 = cloudpivot_d3cfg）：

    python3 scripts/verify/d3_state_edge_admin_e2e.py

覆盖七件事（每条打 [PASS]/[FAIL]，结尾汇总，失败 exit 1）：

  ① 三个接口都在：GET / PUT / DELETE `/api/business/flow/state-edge`
  ② **无码 403**：只有 `:list` 码的账号能查（200）、改（403）——权限码与注解一致
  ③ **审批化**：PUT 只提交审批单（配置一分不动），审批通过才生效（有效边里 POSTED 消失）
  ④ **驳回不生效**：再来一次并驳回 → 配置行一条都没有、有效边不变
  ⑤ **前端按钮联动**：出库单详情的 `next_statuses` 从有效边来（禁用后不再出现 POSTED），
     且真点 POSTED 仍然 409（文案点名「已被状态机配置禁用」）
  ⑥ **删除也走审批**：DELETE → 审批通过 → 配置消失（回到 BASE_EDGES，按钮回来）
  ⑦ **同一条边不许两张并行审批单**：重复提交 409，报文带审批单号

夹具：一张出库单（`D3-E2E-SO-*`）+ 一个只读账号（`d3_e2e_reader`）+ 两条边的审批单，
跑完全部清理（打印清理后的计数）。
"""

# ---- E2ERES 环境守卫（t_61dca492）：e2e 不许打到生产库，库名是生产就直接退出 ----
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from db_guard import require_non_prod as _require_non_prod  # noqa: E402
_require_non_prod(script=_os.path.basename(__file__))
import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("D3_BASE_URL", "http://127.0.0.1:18096")
DB = os.environ.get("D3_DB", "cloudpivot_d3cfg")
PREFIX = "D3-E2E-"
READER = "d3_e2e_reader"
READER_ROLE = "D3_E2E_READER"

PASS = 0
FAIL = 0


def say(tag, msg):
    print(f"[{tag}] {msg}", flush=True)


def check(cond, msg, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        say("PASS", msg)
    else:
        FAIL += 1
        say("FAIL", msg + (f"  << {detail}" if detail else ""))


def psql(sql, tuples_only=True):
    env = dict(os.environ, PGPASSWORD=os.environ.get("D3_PGPASS", "alun"))
    args = ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-At", "-F", "|"]
    if tuples_only:
        args.append("-t")
    args += ["-c", sql]
    out = subprocess.run(args, env=env, capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(f"psql 失败：{sql}\n{out.stderr}")
    return out.stdout.strip()


def http(method, path, token=None, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def login(username="admin", candidates=None):
    """依次尝试口令；首登强制改密时先改密再重登（仓库既有约定）。"""
    psql(f"UPDATE sys_user SET login_fail_count = 0, is_locked = false, lock_time = NULL "
         f"WHERE username = '{username}'")
    if candidates is None:
        candidates = ["Admin@123456D3!", "Admin@123456", "Admin123456"]
        if os.environ.get("D3_PASSWORD"):
            candidates.insert(0, os.environ["D3_PASSWORD"])
    for password in candidates:
        status, payload = http("POST", "/api/system/auth/login",
                               body={"username": username, "password": password})
        if status != 200:
            continue
        data = payload.get("data") or {}
        token = data.get("access_token")
        if not token:
            continue
        if data.get("must_change_password"):
            new_password = password + "D3!"
            st, _ = http("PUT", "/api/system/auth/password", token=token,
                         body={"old_password": password, "new_password": new_password})
            if st != 200:
                continue
            st, payload = http("POST", "/api/system/auth/login",
                               body={"username": username, "password": new_password})
            if st != 200:
                continue
            token = ((payload.get("data") or {}).get("access_token"))
            if username == "admin":
                os.environ["D3_PASSWORD"] = new_password
        return token
    raise SystemExit(f"登录失败：{username} 的口令都不是已知候选")


# ---------- 管理面 ----------

def state_edge(token, entity="STOCK_OUT"):
    return http("GET", f"/api/business/flow/state-edge?entity={entity}", token=token)


def view_of(payload):
    data = (payload or {}).get("data") or {}
    items = data.get("list") or []
    return items[0] if items else {}


def effective_targets(payload, status="DRAFT"):
    view = view_of(payload)
    return [edge["to"] for edge in view.get("effective_edges", []) if edge["from"] == status]


def put_edge(token, entity, frm, to, **over):
    body = {
        "entity": entity,
        "from_status": frm,
        "to_status": to,
        "enabled": True,
        "requires_reason": False,
        "confirm": True,
        "reason": "D3 e2e：验收审批化",
    }
    body.update(over)
    return http("PUT", "/api/business/flow/state-edge", token=token, body=body)


def delete_edge(token, entity, frm, to):
    query = urllib.parse.urlencode({
        "entity": entity,
        "from_status": frm,
        "to_status": to,
        "confirm": "true",
        "reason": "D3 e2e：恢复默认",
    })
    return http("DELETE", f"/api/business/flow/state-edge?{query}", token=token)


def pending_task(instance_id):
    """实例上的 PENDING 待办 id（走 psql：审批中心的「我的待办」分页排序与本用例无关）。"""
    return psql(
        "SELECT id FROM biz_flow_task WHERE instance_id = '%s'::uuid "
        "AND status = 'PENDING' ORDER BY created_at LIMIT 1" % instance_id)


def approve(token, task_id):
    return http("POST", f"/api/business/flow/task/{task_id}/approve",
                token=token, body={"comment": "同意（D3 e2e）"})


def reject(token, task_id, comment="不同意：这条捷径要保留（D3 e2e）"):
    return http("POST", f"/api/business/flow/task/{task_id}/reject",
                token=token, body={"comment": comment, "return_to": "NONE"})


# ---------- 夹具 ----------

def ensure_stock_out(out_no):
    warehouse = psql("SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY warehouse_code LIMIT 1")
    material = psql("SELECT m.id || '|' || m.unit_id FROM biz_material m "
                    "WHERE m.deleted_at IS NULL AND m.status = 1 ORDER BY material_code LIMIT 1")
    if not warehouse or not material:
        raise SystemExit("库里没有仓库 / 物料种子数据，无法造出库单")
    material_id, unit_id = material.split("|")
    psql(
        "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, "
        " total_qty, total_amount, remark) VALUES "
        f"('{SO_ID}'::uuid, '{out_no}', 'ISSUE', '{warehouse}'::uuid, CURRENT_DATE, 'DRAFT', 2, 0, '{PREFIX}')"
    )
    psql(
        "INSERT INTO biz_stock_out_item (stock_out_id, line_no, material_id, unit_id, qty) VALUES "
        f"('{SO_ID}'::uuid, 1, '{material_id}'::uuid, '{unit_id}'::uuid, 2)"
    )
    return SO_ID


def ensure_reader():
    """只授 `business:flow:state-edge:list` 的账号（密码抄 admin 的哈希，登录即用）。"""
    psql(
        "INSERT INTO sys_role (id, role_code, role_name, status, sort_order, remark, created_at, updated_at) "
        f"VALUES ('{ROLE_ID}'::uuid, '{READER_ROLE}', 'D3 e2e 只读角色', 1, 99, 'D3 e2e', now(), now()) "
        "ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING"
    )
    psql(
        "INSERT INTO sys_user (id, username, password, real_name, status, created_at, updated_at) "
        f"VALUES ('{USER_ID}'::uuid, '{READER}', "
        "(SELECT password FROM sys_user WHERE username = 'admin'), 'D3 只读', 1, now(), now()) "
        "ON CONFLICT (username) WHERE deleted_at IS NULL DO NOTHING"
    )
    psql(f"DELETE FROM sys_user_role WHERE user_id = '{USER_ID}'::uuid")
    psql(f"INSERT INTO sys_user_role (user_id, role_id) VALUES ('{USER_ID}'::uuid, '{ROLE_ID}'::uuid)")
    psql(
        "INSERT INTO sys_role_menu (id, role_id, menu_id) "
        f"SELECT gen_random_uuid(), '{ROLE_ID}'::uuid, m.id FROM sys_menu m "
        "WHERE m.auth_code = 'business:flow:state-edge:list' "
        "  AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm "
        f"                  WHERE rm.role_id = '{ROLE_ID}'::uuid AND rm.menu_id = m.id)"
    )


def cleanup():
    psql("DELETE FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM sys_notice "
         "WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN (SELECT id::text FROM biz_flow_instance "
         "WHERE biz_type = 'FLOW_EDGE_CFG'))")
    psql("DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN "
         "(SELECT id::text FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG')")
    psql("UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE biz_type = 'FLOW_EDGE_CFG'")
    psql("DELETE FROM biz_flow_task WHERE instance_id IN "
         "(SELECT id FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG')")
    psql("DELETE FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'")
    psql("DELETE FROM biz_flow_state_edge WHERE entity IN ('STOCK_OUT', 'PURCHASE_ORDER') AND "
         "((entity = 'STOCK_OUT' AND from_status = 'DRAFT' AND to_status = 'POSTED') OR "
         " (entity = 'PURCHASE_ORDER' AND from_status = 'SUBMITTED' AND to_status = 'CLOSED'))")
    psql("DELETE FROM biz_status_log WHERE entity_type = 'FLOW_EDGE_CFG' "
         "OR entity_no LIKE 'D3-E2E-%'")
    psql(f"DELETE FROM biz_stock_out_item WHERE stock_out_id = '{SO_ID}'::uuid")
    psql(f"DELETE FROM biz_stock_out WHERE out_no LIKE '{PREFIX}%'")
    psql(f"DELETE FROM sys_user_role WHERE user_id = '{USER_ID}'::uuid")
    psql(f"DELETE FROM sys_role_menu WHERE role_id = '{ROLE_ID}'::uuid")
    psql(f"DELETE FROM sys_user WHERE username = '{READER}'")
    psql(f"DELETE FROM sys_role WHERE role_code = '{READER_ROLE}'")


SO_ID = "d3e2e000-0000-4000-8000-000000000001"
ROLE_ID = "d3e2e000-0000-4000-8000-000000000002"
USER_ID = "d3e2e000-0000-4000-8000-000000000003"


def main():
    token = login()
    say("INFO", f"登录成功（admin），BASE={BASE}，DB={DB}")
    cleanup()
    ensure_reader()
    reader_token = login(READER, ["Admin@123456D3!", "Admin@123456", "Admin123456"])
    out_id = ensure_stock_out(f"{PREFIX}SO-1")
    say("INFO", f"夹具：出库单 {out_id}（DRAFT）/ 只读账号 {READER}")

    # ① 三个接口都在：GET 200
    status, payload = state_edge(token)
    check(status == 200 and payload.get("code") == 0,
          f"① GET /api/business/flow/state-edge 可用（HTTP {status}）", str(payload)[:300])
    view = view_of(payload)
    check(view.get("entity") == "STOCK_OUT", "① 返回的是 STOCK_OUT 的视图", str(view)[:200])
    check(len(view.get("base_edges") or []) == 6, "① 带上了 BASE 边（出库单 6 条）",
          str(len(view.get("base_edges") or [])))
    check(len(view.get("effective_edges") or []) == 6, "① 带上了有效边（无配置时 == BASE）",
          str(len(view.get("effective_edges") or [])))
    check("POSTED" in effective_targets(payload), "① 基线：DRAFT 能直接过账（optional 边默认启用）",
          str(effective_targets(payload)))

    # ② 无码 403（只有 :list 的账号能查不能改）
    status, payload = state_edge(reader_token)
    check(status == 200, f"② 持 :list 的账号能查配置（HTTP {status}）", str(payload)[:300])
    status, payload = put_edge(reader_token, "STOCK_OUT", "DRAFT", "POSTED", enabled=False)
    check(status == 403, f"② 无 :manage 码改配置 → 403（HTTP {status}）", str(payload)[:300])
    status, payload = delete_edge(reader_token, "STOCK_OUT", "DRAFT", "POSTED")
    check(status == 403, f"② 无 :manage 码删配置 → 403（HTTP {status}）", str(payload)[:300])

    # ③ 审批化：提交只落审批单
    status, payload = put_edge(token, "STOCK_OUT", "DRAFT", "POSTED", enabled=False)
    say("INFO", f"③ PUT 原文：{json.dumps(payload, ensure_ascii=False)[:400]}")
    data = (payload or {}).get("data") or {}
    check(status == 200 and data.get("applied") is False,
          f"③ PUT 只提交审批（applied=false，HTTP {status}）", str(payload)[:400])
    instance_id = data.get("instance_id") or ""
    instance_no = data.get("instance_no") or ""
    check(bool(instance_id) and instance_no.startswith("FLW-"),
          "③ 返回审批单号（可直接去审批中心搜）", f"{instance_no}")
    check(data.get("instance_status") == "PENDING", "③ 审批单状态 PENDING（等复核）",
          str(data.get("instance_status")))
    check(psql("SELECT count(*) FROM biz_flow_state_edge WHERE entity = 'STOCK_OUT' "
               "AND from_status = 'DRAFT' AND to_status = 'POSTED' AND deleted_at IS NULL") == "0",
          "③ 审批通过之前**一行配置都没写**（唯一入口仍是内核）")
    status, payload = state_edge(token)
    check("POSTED" in effective_targets(payload), "③ 提交后有效边不变（还没生效）",
          str(effective_targets(payload)))
    pending = view_of(payload).get("pending_changes") or []
    check(len(pending) == 1 and pending[0].get("instance_no") == instance_no,
          "③ GET 能看到「这条边正在审批中」", str(pending)[:300])

    # ⑦ 同一条边不许两张并行审批单
    status, payload = put_edge(token, "STOCK_OUT", "DRAFT", "POSTED", enabled=False)
    message = json.dumps(payload, ensure_ascii=False)
    check(status == 409 and instance_no in message,
          f"⑦ 重复提交同一条边 → 409 且报文带审批单号（HTTP {status}）", message[:400])

    # ③ 审批通过 → 生效（走真机审批接口，回调在 handler 里）
    task_id = pending_task(instance_id)
    status, payload = approve(token, task_id)
    check(status == 200, f"③ 审批通过（HTTP {status}）", str(payload)[:300])
    status, payload = state_edge(token)
    check("POSTED" not in effective_targets(payload),
          "③ 审批通过后有效边里 `DRAFT → POSTED` 消失", str(effective_targets(payload)))
    check("PENDING_POST" in effective_targets(payload),
          "③ 更严的那条路（提交待过账）还在", str(effective_targets(payload)))
    check(psql("SELECT count(*) FROM biz_flow_state_edge WHERE entity = 'STOCK_OUT' "
               "AND from_status = 'DRAFT' AND to_status = 'POSTED' "
               "AND deleted_at IS NULL AND enabled = false") == "1",
          "③ 配置行真的落库（enabled = false）")
    audits = psql("SELECT count(*) FROM biz_status_log WHERE entity_type = 'FLOW_EDGE_CFG' "
                  "AND to_status = 'APPLIED'")
    check(int(audits) >= 1, f"③ 审计留痕：审批通过生效写了 biz_status_log（APPLIED×{audits}）")

    # ⑤ 前端按钮联动：单据详情的 next_statuses 读有效边
    status, payload = http("GET", f"/api/business/erp/stock-out/{out_id}", token=token)
    detail = (payload or {}).get("data") or {}
    targets = detail.get("next_statuses") or []
    say("INFO", f"⑤ 出库单详情 next_statuses = {targets}")
    check(status == 200, f"⑤ 出库单详情可用（HTTP {status}）", str(payload)[:300])
    check("POSTED" not in targets,
          "⑤ 配置禁用后按钮不显示：详情的 next_statuses 不再含 POSTED（前端按钮直接照它渲染）")
    check("PENDING_POST" in targets, "⑤ 提交待过账按钮仍在（更严的那条路）", str(targets))
    legal = detail.get("legal_edges") or []
    check(all(edge[1] != "POSTED" for edge in legal if edge[0] == "DRAFT"),
          "⑤ legal_edges 与 next_statuses 同源", str(legal))

    # ⑤ 真点 POSTED 仍然 409（后端也不给它开后门）
    status, payload = http("PATCH", f"/api/business/erp/stock-out/{out_id}/status",
                           token=token, body={"status": "POSTED"})
    message = json.dumps(payload, ensure_ascii=False)
    check(status == 409, f"⑤ 直接点 POSTED 仍被拒（HTTP {status}）", message[:300])
    check("已被状态机配置禁用" in message, "⑤ 报文点名「已被状态机配置禁用」", message[:300])

    # ④ 驳回不生效
    status, payload = put_edge(token, "PURCHASE_ORDER", "SUBMITTED", "CLOSED", enabled=False)
    data = (payload or {}).get("data") or {}
    check(status == 200 and data.get("applied") is False,
          f"④ 第二条边提交审批（HTTP {status}）", str(payload)[:300])
    reject_task = pending_task(data.get("instance_id") or "")
    status, payload = reject(token, reject_task)
    check(status == 200, f"④ 驳回（HTTP {status}）", str(payload)[:300])
    check(psql("SELECT count(*) FROM biz_flow_state_edge WHERE entity = 'PURCHASE_ORDER' "
               "AND from_status = 'SUBMITTED' AND to_status = 'CLOSED'") == "0",
          "④ 驳回之后一条配置行都没有（配置一分不动）")
    status, payload = state_edge(token, "PURCHASE_ORDER")
    check("CLOSED" in effective_targets(payload, "SUBMITTED"),
          "④ 驳回后有效边不变（仍能直接关闭）", str(effective_targets(payload, "SUBMITTED")))

    # ⑥ 删除也走审批
    status, payload = delete_edge(token, "STOCK_OUT", "DRAFT", "POSTED")
    data = (payload or {}).get("data") or {}
    check(status == 200 and data.get("applied") is False,
          f"⑥ DELETE 也提交审批（HTTP {status}）", str(payload)[:300])
    delete_task = pending_task(data.get("instance_id") or "")
    status, payload = approve(token, delete_task)
    check(status == 200, f"⑥ 删除审批通过（HTTP {status}）", str(payload)[:300])
    status, payload = state_edge(token)
    check("POSTED" in effective_targets(payload),
          "⑥ 删除生效后行为回到 BASE_EDGES（直接过账按钮回来）", str(effective_targets(payload)))
    check(psql("SELECT count(*) FROM biz_flow_state_edge WHERE entity = 'STOCK_OUT' "
               "AND from_status = 'DRAFT' AND to_status = 'POSTED' AND deleted_at IS NULL") == "0",
          "⑥ 存活配置行 0（软删 = 回到 BASE_EDGES）")
    status, payload = http("GET", f"/api/business/erp/stock-out/{out_id}", token=token)
    targets = ((payload or {}).get("data") or {}).get("next_statuses") or []
    check("POSTED" in targets, "⑥ 详情 next_statuses 里 POSTED 回来了（按钮联动是双向的）", str(targets))

    # 收尾：审批中已无残留
    print("\n---- 清理前计数 ----")
    for label, sql in [
        ("flow_state_edge(存活)", "SELECT count(*) FROM biz_flow_state_edge WHERE deleted_at IS NULL"),
        ("审批单(FLOW_EDGE_CFG)", "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'"),
        ("审计(FLOW_EDGE_CFG)", "SELECT count(*) FROM biz_status_log WHERE entity_type = 'FLOW_EDGE_CFG'"),
        ("出库单(D3-E2E)", f"SELECT count(*) FROM biz_stock_out WHERE out_no LIKE '{PREFIX}%'"),
    ]:
        print(f"{label} = {psql(sql)}")
    cleanup()
    print("---- 清理后计数 ----")
    for label, sql in [
        ("flow_state_edge(整表)", "SELECT count(*) FROM biz_flow_state_edge"),
        ("审批单(FLOW_EDGE_CFG)", "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'FLOW_EDGE_CFG'"),
        ("审计(FLOW_EDGE_CFG)", "SELECT count(*) FROM biz_status_log WHERE entity_type = 'FLOW_EDGE_CFG'"),
        ("出库单(D3-E2E)", f"SELECT count(*) FROM biz_stock_out WHERE out_no LIKE '{PREFIX}%'"),
    ]:
        print(f"{label} = {psql(sql)}")

    print(f"\nRESULT: {PASS} passed, {FAIL} failed")
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
