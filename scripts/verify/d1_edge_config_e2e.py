#!/usr/bin/env python3
"""D1 真机验收：状态机边配置化（禁用一条 optional 边 → 真机被拒 → 恢复）。

用法（隔离实例已起在 18094 且库 = cloudpivot_d1）：

    python3 scripts/verify/d1_edge_config_e2e.py

覆盖四件事（每条都打 [PASS]/[FAIL]，结尾汇总，失败 exit 1）：

  ① 基线：库里**没有**配置时，采购单 SUBMITTED → CLOSED 走真机接口成功（200）
  ② 配一条禁用（`biz_flow_state_edge(entity=PURCHASE_ORDER, SUBMITTED→CLOSED, enabled=false)`）
     → 同一接口 409，报文点名「已被状态机配置禁用」，且单据状态**没有被改动**
  ③ fail-safe：把配置表改名（模拟「表不存在 / 迁移没跑」）→ 接口恢复 200（退化到 BASE_EDGES，
     系统不崩），实例日志里留下 WARN「读取 biz_flow_state_edge 失败」
  ④ 恢复：软删配置 → 行为回到基线（200）

夹具：三张采购单（`D1-E2E-*`）+ 一行配置，跑完全部清理（打印清理后的计数）。
"""
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
import uuid

BASE = os.environ.get("D1_BASE_URL", "http://127.0.0.1:18094")
DB = os.environ.get("D1_DB", "cloudpivot_d1")
LOG = os.environ.get("D1_LOG", "/root/d1-e2e-server.log")
PREFIX = "D1-E2E-"

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
    env = dict(os.environ, PGPASSWORD=os.environ.get("D1_PGPASS", "alun"))
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


def login():
    """依次尝试口令；首登强制改密时先改密再重登（这是仓库既有约定）。"""
    # 上一轮 e2e 可能已经把初始口令改成 <初口令>D1!（首登强制改密），也可能因连跑把账号锁了
    psql("UPDATE sys_user SET login_fail_count = 0, is_locked = false, lock_time = NULL "
         "WHERE username = 'admin'")
    candidates = []
    if os.environ.get("D1_PASSWORD"):
        candidates.append(("admin", os.environ["D1_PASSWORD"]))
    candidates += [
        ("admin", "Admin@123456D1!"),
        ("admin", "Admin@123456"),
        ("admin", "Admin123456"),
    ]
    for user, password in candidates:
        status, payload = http("POST", "/api/system/auth/login", body={"username": user, "password": password})
        if status != 200:
            continue
        data = payload.get("data") if isinstance(payload, dict) else None
        data = data or {}
        token = data.get("access_token")
        if not token:
            continue
        if data.get("must_change_password"):
            new_password = password + "D1!"
            st, _ = http("PUT", "/api/system/auth/password", token=token,
                         body={"old_password": password, "new_password": new_password})
            if st != 200:
                continue
            st, payload = http("POST", "/api/system/auth/login", body={"username": user, "password": new_password})
            if st != 200:
                continue
            fresh = payload.get("data") if isinstance(payload, dict) else None
            token = (fresh if isinstance(fresh, dict) else {}).get("access_token")
            os.environ["D1_PASSWORD"] = new_password
        return token
    raise SystemExit("登录失败：admin 口令都不是已知候选（先跑 d1_dbsetup 重种库）")


def create_po(order_no, status="SUBMITTED"):
    supplier = psql("SELECT id FROM biz_supplier WHERE deleted_at IS NULL ORDER BY supplier_code LIMIT 1")
    warehouse = psql("SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY warehouse_code LIMIT 1")
    if not supplier or not warehouse:
        raise SystemExit("库里没有供应商 / 仓库种子数据，无法造采购单")
    psql(
        "INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id, status, "
        "total_qty, total_amount, total_tax, total_with_tax) VALUES "
        f"('{order_no}', '{supplier}'::uuid, CURRENT_DATE, '{warehouse}'::uuid, '{status}', 10, 100, 0, 100)"
    )
    return psql(f"SELECT id FROM biz_purchase_order WHERE order_no = '{order_no}'")


def po_status(po_id):
    return psql(f"SELECT status FROM biz_purchase_order WHERE id = '{po_id}'::uuid")


def patch_close(po_id, token):
    return http("PATCH", f"/api/business/erp/purchase-order/{po_id}/status", token=token,
                body={"status": "CLOSED"})


def cleanup():
    psql("DELETE FROM biz_status_log WHERE entity_no LIKE 'D1-E2E-%'")
    psql(f"DELETE FROM biz_purchase_order WHERE order_no LIKE '{PREFIX}%'")
    psql(f"DELETE FROM biz_flow_state_edge WHERE entity = 'PURCHASE_ORDER' AND from_status = 'SUBMITTED' AND to_status = 'CLOSED'")


def main():
    token = login()
    say("INFO", f"登录成功，BASE={BASE}，DB={DB}")
    cleanup()

    po1, po2, po3 = (f"{PREFIX}{suffix}" for suffix in ("BASE", "CFG", "FAILSAFE"))
    id1 = create_po(po1)
    id2 = create_po(po2)
    id3 = create_po(po3)
    say("INFO", f"夹具：{po1}={id1} / {po2}={id2} / {po3}={id3}（均 SUBMITTED）")

    # ① 基线
    status, payload = patch_close(id1, token)
    check(status == 200 and payload.get("code") in (0, None),
          f"① 无配置时 SUBMITTED→CLOSED 走真机接口成功（HTTP {status}）", str(payload)[:300])
    check(po_status(id1) == "CLOSED", "① 单据状态真的变成 CLOSED", po_status(id1))

    # ② 配一条禁用 → 真机被拒
    psql(
        "INSERT INTO biz_flow_state_edge (entity, from_status, to_status, enabled) "
        "VALUES ('PURCHASE_ORDER', 'SUBMITTED', 'CLOSED', false)"
    )
    say("INFO", "已写入配置：" + psql(
        "SELECT entity, from_status, to_status, enabled FROM biz_flow_state_edge "
        "WHERE entity = 'PURCHASE_ORDER'"))
    status, payload = patch_close(id2, token)
    message = json.dumps(payload, ensure_ascii=False)
    say("INFO", f"② 409 原文：{message}")
    check(status == 409, f"② 禁用后同一接口被拒（HTTP {status}，期望 409）", message[:400])
    check("已被状态机配置禁用" in message, "② 报文点名「已被状态机配置禁用」（不是含糊的非法流转）", message[:400])
    check(po_status(id2) == "SUBMITTED", "② 被拒后单据状态没有被改（409 不写库）", po_status(id2))

    # ③ fail-safe：表不存在 → 退化 BASE_EDGES
    psql("ALTER TABLE biz_flow_state_edge RENAME TO biz_flow_state_edge_d1_bak")
    try:
        status, payload = patch_close(id3, token)
        message = json.dumps(payload, ensure_ascii=False)
        check(status == 200, f"③ 配置表不存在时接口仍成功（退化 BASE_EDGES，HTTP {status}）", message[:400])
        check(po_status(id3) == "CLOSED", "③ 单据状态正常推进（系统不崩）", po_status(id3))
    finally:
        psql("ALTER TABLE biz_flow_state_edge_d1_bak RENAME TO biz_flow_state_edge")
    try:
        with open(LOG, encoding="utf-8", errors="ignore") as handle:
            log_tail = handle.read()
        hit = "读取 biz_flow_state_edge 失败" in log_tail
        check(hit, "③ 实例日志留下 WARN「读取 biz_flow_state_edge 失败」（降级可追溯）",
              "日志里没找到该 WARN（看 " + LOG + "）")
    except FileNotFoundError:
        say("WARN", f"日志文件不存在，跳过 WARN 断言：{LOG}")

    # ④ 恢复：软删配置 → 行为恢复
    psql("UPDATE biz_flow_state_edge SET deleted_at = now() "
         "WHERE entity = 'PURCHASE_ORDER' AND from_status = 'SUBMITTED' AND to_status = 'CLOSED'")
    status, payload = patch_close(id2, token)
    message = json.dumps(payload, ensure_ascii=False)
    check(status == 200, f"④ 软删配置后行为恢复（HTTP {status}）", message[:400])
    check(po_status(id2) == "CLOSED", "④ 单据状态正常变成 CLOSED", po_status(id2))

    print("\n---- 清理前计数 ----")
    print("purchase_order(D1-E2E)=", psql(f"SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE '{PREFIX}%'"))
    print("flow_state_edge(存活)=", psql("SELECT count(*) FROM biz_flow_state_edge WHERE deleted_at IS NULL"))
    cleanup()
    print("---- 清理后计数 ----")
    print("purchase_order(D1-E2E)=", psql(f"SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE '{PREFIX}%'"))
    print("flow_state_edge(整表)=", psql("SELECT count(*) FROM biz_flow_state_edge"))
    print("status_log(FLOW_EDGE_CFG)=", psql("SELECT count(*) FROM biz_status_log WHERE entity_type = 'FLOW_EDGE_CFG'"))

    print(f"\nRESULT: {PASS} passed, {FAIL} failed")
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    main()
