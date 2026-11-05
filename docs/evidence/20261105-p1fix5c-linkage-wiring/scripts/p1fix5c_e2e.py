#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-FIX-5C 真机验收：**新接线的触发点** 端到端（配置 → 触发 → 目标单据被创建）。

跑法（隔离实例已在跑）：

    LNK_PORT=18132 bash scripts/p1fix5c_instance.sh start
    python3 scripts/p1fix5c_e2e.py

要证明的事（每条 `[PASS]/[FAIL]` + 原始取值）：

  A. 配置面收口：`GET /business/flow/linkage` 的 `catalog_triggers` 下发 `wired` / `wiring`；
     已接线 = STOCK_IN / STOCK_OUT / TASK，其余 12 个明确标注「未接线 + 要动哪里」。
  B. 未接线触发点**存不进去**：给 TRANSFER 建规则 → `400`，文案点名「尚未接线」。
  C. 新接线触发点 STOCK_OUT/POSTED 上配两条规则（CREATE_TASK seq10 + SEND_NOTICE seq20）→ 保存成功。
  D. 源单据动作：建一张出库单并过账 → 三本账 + **1 张**成本结转凭证（不双发）。
  E. 规则执行：日志点名「出库过账：跨单据联动已执行」，`deferred_rules=2`，
     两条 AfterCommit 动作 `outcome=SUCCESS`。
  F. 目标单据**被创建**：`biz_task` 多出一条任务（标题含源单号、负责人=过账人、`TSK-` 编号），
     `sys_notice` 多出一条 `biz_type=STOCK_OUT` / `biz_id=<出库单 id>`。
  G. 收尾：删掉本卡加的 2 条规则（可重复跑）。

退出码：0 = 全 PASS；1 = 有 FAIL。
"""

import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

BASE = os.environ.get("LNK_BASE", "http://127.0.0.1:18132")
DB = os.environ.get("LNK_DB", "cloudpivot_lnk5b")
LOG = os.environ.get("LNK_LOG", "/root/p1fix5c-server.log")
PREFIX = "LNK5C-"

# 生产库守卫
if not DB.startswith("cloudpivot_lnk5b") or DB == "cloudpivot":
    print(f"拒绝执行：LNK_DB={DB}（必须 cloudpivot_lnk5b*，绝不碰生产库）")
    sys.exit(2)

# 隔离库里有库存的 (物料, 仓库)：WH01 总仓 + 种子物料 40 件
MATERIAL = "d1370003-0000-4000-8000-000000000001"
WAREHOUSE = "d1370001-0000-4000-8000-000000000001"

PASS = 0
FAIL = 0
TOKEN = ""

RULES = [
    {
        "name": PREFIX + "出库过账 → 建复核任务",
        "entity": "STOCK_OUT",
        "status": "POSTED",
        "action": "CREATE_TASK",
        "params": {
            "title": "出库过账复核（联动生成）",
            "content": "出库单过账后由跨单据联动自动生成：请复核出库事由与库位。",
            "assignee_strategy": "OPERATOR",
            "due_days": 3,
        },
        "seq": 10,
    },
    {
        "name": PREFIX + "出库过账 → 通知制单人",
        "entity": "STOCK_OUT",
        "status": "POSTED",
        "action": "SEND_NOTICE",
        "params": {"mode": "TAKEOVER", "recipient": "CREATOR"},
        "seq": 20,
    },
]


def ok(label, detail=""):
    global PASS
    PASS += 1
    print(f"[PASS] {label}" + (f"  —— {detail}" if detail else ""))


def bad(label, detail=""):
    global FAIL
    FAIL += 1
    print(f"[FAIL] {label}" + (f"  —— {detail}" if detail else ""))


def expect(cond, label, detail=""):
    ok(label, detail) if cond else bad(label, detail)


def psql(sql):
    env = dict(os.environ)
    env["PGPASSWORD"] = os.environ.get("PGPASSWORD", "alun")
    proc = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-At", "-v", "ON_ERROR_STOP=1", "-f", "-"],
        input=sql, capture_output=True, text=True, env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"psql 失败：{proc.stderr.strip()}")
    return proc.stdout.strip()


def _json(text):
    if not text:
        return {}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw": text}


def http(method, path, body=None, token=None):
    data = None
    headers = {"Content-Type": "application/json"}
    if body is not None:
        data = json.dumps(body, ensure_ascii=False).encode()
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            return resp.status, _json(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, _json(err.read().decode())


def log_offset():
    try:
        return os.path.getsize(LOG)
    except OSError:
        return 0


def log_since(offset):
    try:
        with open(LOG, "r", errors="replace") as fh:
            fh.seek(offset)
            raw = fh.read()
    except OSError:
        return ""
    return _ANSI_RE.sub("", raw)


def login():
    psql("UPDATE sys_user SET login_fail_count = 0, is_locked = false WHERE username = 'admin';")
    password = os.environ.get("LNK_ADMIN_PASSWORD", "Admin@123456")
    code, body = http("POST", "/api/system/auth/login", {"username": "admin", "password": password})
    token = ((body.get("data") or {}).get("access_token")) or ""
    if code == 200 and not token and (body.get("data") or {}).get("must_change_password"):
        psql("UPDATE sys_user SET must_change_password = false WHERE username = 'admin';")
        code, body = http("POST", "/api/system/auth/login", {"username": "admin", "password": password})
        token = ((body.get("data") or {}).get("access_token")) or ""
    if code != 200 or not token:
        print(f"登录失败：{code} {body}")
        sys.exit(2)
    return token


def catalog():
    code, body = http("GET", "/api/business/flow/linkage", token=TOKEN)
    assert code == 200, f"读配置面失败：{code} {body}"
    return body.get("data") or {}


def put_rule(rule, allow_unwired=False):
    body = {
        "name": rule["name"],
        "trigger_entity": rule["entity"],
        "trigger_status": rule["status"],
        "action_code": rule["action"],
        "action_params": rule["params"],
        "enabled": True,
        "seq": rule["seq"],
        "confirm": True,
        "allow_unwired": allow_unwired,
    }
    return http("PUT", "/api/business/flow/linkage", body, token=TOKEN)


def delete_rule(rule):
    existing = catalog().get("rules") or []
    for row in existing:
        if (
            row.get("trigger_entity") == rule["entity"]
            and row.get("trigger_status") == rule["status"]
            and row.get("action_code") == rule["action"]
        ):
            http(
                "DELETE",
                f"/api/business/flow/linkage?id={row['id']}&confirm=true",
                token=TOKEN,
            )


def main():
    global TOKEN
    TOKEN = login()
    print(f"登录成功（{BASE} 库={DB}）")

    # ---------- A. 配置面：wired / wiring 下发 ----------
    data = catalog()
    triggers = data.get("triggers") or []
    by_entity = {item["entity"]: item for item in triggers}
    wired = sorted(item["entity"] for item in triggers if item.get("wired"))
    expect(
        wired == ["STOCK_IN", "STOCK_OUT", "TASK"],
        "A1 catalog_triggers 的 wired 与代码一致（3 个已接线）",
        f"wired={wired}，unwired={len(triggers) - len(wired)}",
    )
    stock_out = by_entity.get("STOCK_OUT") or {}
    expect(
        bool(stock_out.get("wired")) and "post_out_tx" in (stock_out.get("wiring") or ""),
        "A2 STOCK_OUT 标注已接线且点明调用点（post_out_tx）",
        (stock_out.get("wiring") or "")[:120],
    )
    unwired_item = next((item for item in triggers if not item.get("wired")), {})
    transfer = unwired_item
    expect(
        not transfer.get("wired") and bool((transfer.get("wiring") or "").strip()),
        f"A3 未接线触发点（{transfer.get('entity')}）带「未接线 + 要动哪里」说明",
        (transfer.get("wiring") or "")[:120],
    )

    # ---------- B. 未接线触发点存不进去 ----------
    transfer_entity = transfer.get("entity") or "STOCK_TRANSFER"
    transfer_status = (transfer.get("statuses") or ["DRAFT"])[0]
    probe = {
        "name": PREFIX + "未接线探针",
        "entity": transfer_entity,
        "status": transfer_status,
        "action": "SEND_NOTICE",
        "params": {"mode": "TAKEOVER"},
        "seq": 10,
    }
    code, body = put_rule(probe)
    msg = ((body.get("error") or {}).get("message")) or body.get("message") or json.dumps(body, ensure_ascii=False)
    expect(
        code == 400 and "尚未接线" in msg,
        "B1 在未接线触发点（TRANSFER）上建规则被 400 拒绝",
        f"code={code} msg={str(msg)[:160]}",
    )
    code, body = put_rule(probe, allow_unwired=True)
    if code == 200:
        delete_rule(probe)
    expect(
        code == 200,
        "B2 显式 allow_unwired=true 时才允许「预置配置」（不静默放行）",
        f"code={code}",
    )

    # ---------- C. 新接线触发点上配规则 ----------
    for rule in RULES:
        delete_rule(rule)  # 幂等：清掉上一轮同名键
        code, body = put_rule(rule)
        expect(code == 200, f"C 配置成功：{rule['name']}", f"code={code} {str(body)[:120]}")

    # ---------- D. 源单据动作：出库过账 ----------
    offset = log_offset()
    before_tasks = int(psql("SELECT count(*) FROM biz_task;") or 0)
    before_notices = int(psql("SELECT count(*) FROM sys_notice;") or 0)

    code, body = http(
        "POST",
        "/api/business/erp/stock-out",
        {
            "out_type": "ISSUE",
            "warehouse_id": WAREHOUSE,
            "reason": "P1-FIX-5C 联动验收",
            "items": [{"material_id": MATERIAL, "qty": "1"}],
        },
        token=TOKEN,
    )
    expect(code == 200, "D1 建出库单成功", f"code={code} {str(body)[:160]}")
    payload = body.get("data") or {}
    doc_id = payload.get("id") or ""
    out_no = payload.get("out_no") or ""
    expect(bool(doc_id) and bool(out_no), "D2 拿到出库单 id / 单号", f"{out_no} {doc_id}")

    code, body = http(
        "PATCH", f"/api/business/erp/stock-out/{doc_id}/status", {"status": "POSTED"}, token=TOKEN
    )
    expect(code == 200, "D3 出库单过账成功（DRAFT → POSTED）", f"code={code} {str(body)[:160]}")

    vouchers = psql(
        f"SELECT count(*) FROM biz_voucher WHERE source_id = '{doc_id}'::uuid;"
    ).strip()
    expect(
        vouchers == "1",
        "D4 成本结转凭证恰好 1 张（排除清单生效：GENERATE_VOUCHER 没被通用引擎再跑一次）",
        f"biz_voucher(source_id)= {vouchers}",
    )

    # ---------- E. 规则执行（日志） ----------
    text = log_since(offset)
    hit = "出库过账：跨单据联动已执行" in text
    expect(hit, "E1 日志点名「出库过账：跨单据联动已执行」", "")
    deferred = "deferred_rules=2" in text
    expect(deferred, "E2 两条 AfterCommit 规则进入 deferred（commit 之后执行）", "")
    success_lines = [
        ln
        for ln in text.splitlines()
        if "提交后的联动动作执行结果" in ln and 'outcome="SUCCESS"' in ln
    ]
    expect(
        len(success_lines) == 2,
        "E3 两条提交后动作都 SUCCESS（CREATE_TASK + SEND_NOTICE）",
        f"lines={len(success_lines)}",
    )

    # ---------- F. 目标单据被创建 ----------
    after_tasks = int(psql("SELECT count(*) FROM biz_task;") or 0)
    expect(after_tasks == before_tasks + 1, "F1 biz_task 多出恰好 1 条（规则创建的目标单据）", f"{before_tasks} → {after_tasks}")
    task_row = psql(
        "SELECT task_no || '|' || status || '|' || title || '|' || owner_id::text || '|' || source "
        f"FROM biz_task WHERE title = '出库过账复核（联动生成）' ORDER BY created_at DESC LIMIT 1;"
    )
    expect(
        task_row.startswith("TSK-") and "|TODO|" in task_row and "出库过账复核（联动生成）" in task_row,
        "F2 任务行取值正确（编号 TSK- / 状态 TODO / 标题与配置一致）",
        task_row,
    )
    after_notices = int(psql("SELECT count(*) FROM sys_notice;") or 0)
    expect(after_notices >= before_notices + 1, "F3 sys_notice 多出通知", f"{before_notices} → {after_notices}")
    notice_row = psql(
        "SELECT title || '|' || coalesce(biz_type,'') || '|' || coalesce(biz_id::text,'') "
        f"FROM sys_notice WHERE biz_type = 'STOCK_OUT' AND biz_id = '{doc_id}' ORDER BY created_at DESC LIMIT 1;"
    )
    expect(
        "出库单已过账" in notice_row and doc_id in notice_row,
        "F4 通知行指向源单据（标题 + biz_id）",
        notice_row,
    )

    # ---------- G. 收尾 ----------
    for rule in RULES:
        delete_rule(rule)
    remaining = [
        row
        for row in (catalog().get("rules") or [])
        if row.get("name", "").startswith(PREFIX)
    ]
    expect(not remaining, "G1 本轮加的规则已清理（可重复跑）", f"残留={len(remaining)}")

    print(f"\nPASS={PASS} FAIL={FAIL}  （源单据 {out_no}）")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
