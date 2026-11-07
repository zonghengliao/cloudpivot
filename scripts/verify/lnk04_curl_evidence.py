#!/usr/bin/env python3
# ============================================================================
# LNK-04 真机取证：盘点单过账 / 作废 → 联动通知真的发出来，且库存**只按差异落一次账**
#
# 走的是生产同款 HTTP 链路（不是直接调函数）：
#
#   配规则（配置页 API）→ POST /stock-take 建单 → PUT 填实盘数
#   → PATCH /status PENDING_POST（显式跳过审批）→ PATCH /status POSTED
#     → post_tx ③ 三本账 ④ 差异合计 ⑤ 差异凭证 ⑥ 通用引擎 run_excluding
#     → commit → run_deferred_after_commit → sys_notice 落 LNK:STOCK_TAKE:POSTED:<id>
#   → PATCH /status CANCELLED（已过账作废：账回滚 + 红冲）
#     → cancel_tx ③ 通用引擎 run_excluding → commit → run_deferred
#     → sys_notice 落 LNK:STOCK_TAKE:CANCELLED:<id>
#
# 用法：python3 scripts/verify/lnk04_curl_evidence.py [base_url]
# 前置：bash scripts/verify/lnk04_start.sh（实例在 18051）+ 本机能 psql 连 cloudpivot_so44
#
# 夹具全部自建自清（仓库 / 物料 / 库存 / 盘点单 / 凭证 / 流水 / 规则 / 通知）：
# 取证库 cloudpivot_so44 是 ERP 验收库（LNK-03 也用它取证），跑完不留业务脏数据。
# `stock_take_post` 流程临时置 DRAFT（要验的是「无流程 + 显式跳过」的过账路径），
# atexit 兜底恢复原状态。
# ============================================================================
import atexit
import json
import subprocess
import sys
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18051"
OUT = "/opt/cloudpivot/docs/evidence/20261107-lnk04-stocktake/curl_evidence.txt"
DB = "cloudpivot_so44"

WAREHOUSE = "d1a40400-0000-4000-8000-000000000001"
MATERIAL = "d1a40400-0000-4000-8000-000000000011"

_STATE = {"flow": None, "token": None, "rules": [], "take_id": None, "done": False}
lines = []


def say(text=""):
    print(text)
    lines.append(text)


def call(method, path, body=None, token=None, expect: "int | None" = 0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            payload = json.loads(resp.read().decode())
            http = resp.status
    except urllib.error.HTTPError as error:
        payload = json.loads(error.read().decode())
        http = error.code
    code = payload.get("code")
    if expect is not None and code != expect:
        say(f"!! {method} {path} 期望 code={expect}，实际 http={http} {json.dumps(payload, ensure_ascii=False)[:800]}")
        raise SystemExit(1)
    return payload, http


def psql(sql):
    out = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", "alun", "-d", DB, "-Atqc", sql],
        capture_output=True,
        text=True,
        env={"PGPASSWORD": "alun", "PATH": "/usr/bin:/bin"},
    )
    if out.returncode != 0:
        say(f"!! psql 失败：{out.stderr.strip()}")
        raise SystemExit(1)
    return out.stdout.strip()


def login():
    for password in ("Admin@123456", "Admin@123456so44"):
        payload, _ = call(
            "POST",
            "/api/system/auth/login",
            {"username": "admin", "password": password},
            expect=None,
        )
        token = (payload.get("data") or {}).get("access_token")
        if token:
            return token
    say("!! 登录失败")
    raise SystemExit(1)


def seed_fixtures():
    unit = psql("SELECT id FROM biz_unit ORDER BY unit_code LIMIT 1")
    psql(
        "DELETE FROM biz_inventory_location WHERE material_id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_inventory WHERE material_id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_inventory_txn WHERE material_id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_stock_take WHERE warehouse_id = "
        f"'{WAREHOUSE}'::uuid; "
        "DELETE FROM biz_material WHERE id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_warehouse WHERE id = "
        f"'{WAREHOUSE}'::uuid"
    )
    psql(
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, "
        "is_leaf, status, allow_negative) VALUES "
        f"('{WAREHOUSE}'::uuid, 'LNK04-WH', 'LNK04 取证仓', '/{WAREHOUSE}/', 1, 1, 1, 0)"
    )
    psql(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) "
        f"VALUES ('{MATERIAL}'::uuid, 'LNK04-M1', 'LNK04 取证物料', '{unit}'::uuid, 1, false)"
    )
    # 账面 100 @ 8 = 800（成本账 + 库位账；未分配库位桶）
    psql(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, "
        "avg_cost, amount) VALUES "
        f"(gen_random_uuid(), '{MATERIAL}'::uuid, '{WAREHOUSE}'::uuid, NULL, 100, 0, 8, 800)"
    )
    psql(
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty) "
        f"VALUES (gen_random_uuid(), '{MATERIAL}'::uuid, '{WAREHOUSE}'::uuid, NULL, 100, 0)"
    )
    return unit


def cleanup():
    if _STATE["done"]:
        return
    _STATE["done"] = True
    if _STATE["flow"]:
        psql(f"UPDATE biz_flow_def SET status = '{_STATE['flow']}' WHERE flow_code = 'stock_take_post'")
        say(f"# stock_take_post 流程恢复 {_STATE['flow']}")
    for rule_id in _STATE["rules"]:
        try:
            call("DELETE", f"/api/business/flow/linkage?id={rule_id}&confirm=true", token=_STATE["token"])
            say(f"# 规则 {rule_id} 已删除")
        except SystemExit:
            psql(f"DELETE FROM biz_flow_linkage_rule WHERE id = '{rule_id}'::uuid")
            say(f"# 规则 {rule_id} 用 SQL 删除（API 失败）")
    take_id = _STATE["take_id"]
    if take_id:
        psql(f"DELETE FROM biz_status_log WHERE entity_id = '{take_id}'::uuid")
        psql(f"DELETE FROM biz_voucher WHERE source_type = 'STOCK_TAKE' AND source_id = '{take_id}'::uuid")
    psql(
        "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:STOCK_TAKE:%'; "
        "DELETE FROM biz_inventory_txn WHERE material_id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_inventory_location WHERE material_id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_inventory WHERE material_id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_stock_take WHERE warehouse_id = "
        f"'{WAREHOUSE}'::uuid; "
        "DELETE FROM biz_material WHERE id = "
        f"'{MATERIAL}'::uuid; "
        "DELETE FROM biz_warehouse WHERE id = "
        f"'{WAREHOUSE}'::uuid"
    )
    say("# 取证夹具已清（盘点单 / 凭证 / 流水 / 库存 / 物料 / 仓库 / 规则 / 通知）")


atexit.register(cleanup)


def rule_upsert(token, status, name):
    payload, _ = call(
        "PUT",
        "/api/business/flow/linkage",
        {
            "name": name,
            "trigger_entity": "STOCK_TAKE",
            "trigger_status": status,
            "action_code": "SEND_NOTICE",
            "action_params": {"mode": "TAKEOVER", "recipient": "CREATOR"},
            "enabled": True,
            "seq": 40,
            "confirm": True,
        },
        token,
    )
    return payload["data"]


def main():
    say("## LNK-04 真机取证：盘点单（STOCK_TAKE）过账 / 作废 → 联动")
    say()
    token = login()
    _STATE["token"] = token
    say(f"# 超管登录成功，token 长度 = {len(token)}")
    unit = seed_fixtures()
    say(f"# 夹具：仓 {WAREHOUSE} / 物料 {MATERIAL} / 单位 {unit} / 账面 100 @ 8 = 800")

    # ---------- ① 配置页：STOCK_TAKE 必须显示 wired=true ----------
    say()
    say("## ① GET /api/business/flow/linkage?trigger_entity=STOCK_TAKE —— 配置页显示 wired=true")
    data, _ = call("GET", "/api/business/flow/linkage?trigger_entity=STOCK_TAKE", token=token)
    trigger = next(item for item in data["data"]["triggers"] if item["entity"] == "STOCK_TAKE")
    say(json.dumps({
        "entity": trigger["entity"],
        "wired": trigger["wired"],
        "wiring": trigger["wiring"][:200] + "…",
    }, ensure_ascii=False, indent=1))
    if not trigger["wired"]:
        say("!! 配置页仍显示 STOCK_TAKE 未接线")
        raise SystemExit(1)

    # ---------- ② 两条规则（走写入口：未接线触发点会被 400，这里必须 problems=[]） ----------
    say()
    say("## ② PUT /api/business/flow/linkage —— STOCK_TAKE/POSTED + /CANCELLED → SEND_NOTICE")
    posted_rule = rule_upsert(token, "POSTED", "LNK04 盘点已过账 → 通知制单人")
    cancel_rule = rule_upsert(token, "CANCELLED", "LNK04 盘点已作废 → 通知制单人")
    _STATE["rules"] = [posted_rule["id"], cancel_rule["id"]]
    for rule in (posted_rule, cancel_rule):
        say(json.dumps({
            "id": rule["id"],
            "trigger": rule["trigger_entity"] + "/" + rule["trigger_status"],
            "action": rule["action_code"],
            "phase": rule["phase"],
            "implemented": rule["implemented"],
            "problems": rule["problems"],
        }, ensure_ascii=False))
        if rule["problems"]:
            say("!! 写入口报 problems，规则不可用")
            raise SystemExit(1)

    # ---------- ③ 取证前置：stock_take_post 流程临时下线（验「无流程 + 显式跳过」的过账路径） ----------
    say()
    old_flow = psql("SELECT status FROM biz_flow_def WHERE flow_code = 'stock_take_post'")
    _STATE["flow"] = old_flow
    psql("UPDATE biz_flow_def SET status = 'DRAFT' WHERE flow_code = 'stock_take_post'")
    say(f"# stock_take_post 流程 {old_flow} → DRAFT（取证完毕恢复）")

    period_row = psql(
        "SELECT period_code || '|' || start_date FROM biz_accounting_period "
        "WHERE status = 'OPEN' ORDER BY period_code LIMIT 1"
    )
    period, take_date = period_row.split("|")
    say(f"# 盘点日期取未结账期间 {period} 的首日 {take_date}（期间闸只读已结账月份）")

    # ---------- ④ 建单 + 填实盘（全盘快照 100，实盘 92 = 盘亏 8） ----------
    say()
    say("## ④ POST /api/business/erp/stock-take + PUT 填实盘数 92（盘亏 8）")
    created, http = call(
        "POST",
        "/api/business/erp/stock-take",
        {
            "warehouse_id": WAREHOUSE,
            "take_date": take_date,
            "take_type": "FULL",
            "remark": "LNK04 真机取证：盘点单联调用例",
        },
        token,
    )
    take = created["data"]
    take_id = take["id"]
    _STATE["take_id"] = take_id
    say(f"# HTTP {http} 单号 {take["take_no"]} id={take_id} 明细 {len(take['items'])} 行")
    item = take["items"][0]
    say(f"# 明细：物料 {item['material_id']} 账面 {item['book_qty']} 实盘 {item['actual_qty']}")
    if item["material_id"] != MATERIAL:
        say(f"!! 全盘明细应只含夹具物料 {MATERIAL}")
        raise SystemExit(1)

    updated, _ = call(
        "PUT",
        f"/api/business/erp/stock-take/{take_id}",
        {"lines": [{"item_id": item["id"], "actual_qty": 92}]},
        token,
    )
    say(f"# 实盘写入后：账面合计 {updated["data"]["total_book_qty"]} / "
        f"实盘合计 {updated["data"]["total_actual_qty"]} / "
        f"差异 {updated['data']['items'][0]['diff_qty']}")

    # ---------- ⑤ 提交（显式跳过审批）→ 过账 ----------
    say()
    say("## ⑤ PATCH /status PENDING_POST（skip_approval）→ PATCH /status POSTED")
    call(
        "PATCH",
        f"/api/business/erp/stock-take/{take_id}/status",
        {
            "status": "PENDING_POST",
            "skip_approval": True,
            "skip_reason": "LNK04 真机取证：取证库临时下线 stock_take_post 流程，显式跳过审批",
        },
        token,
    )
    posted, http = call(
        "PATCH",
        f"/api/business/erp/stock-take/{take_id}/status",
        {"status": "POSTED"},
        token,
    )
    posted = posted["data"]
    say(f"# HTTP {http} 状态 {posted["status"]} 凭证 {posted["voucher_no"]}")
    say(json.dumps({
        "total_gain_qty": posted["total_gain_qty"],
        "total_loss_qty": posted["total_loss_qty"],
        "total_loss_amount": posted["total_loss_amount"],
    }, ensure_ascii=False))

    # ---------- ⑥ 库内物证：库存只按差异落一次 + 通知真的发出来 ----------
    say()
    say("## ⑥ 库内物证（after 过账）")
    cost = psql(
        "SELECT (qty::numeric(18,4)) || '|' || (amount::numeric(18,4)) FROM biz_inventory "
        f"WHERE material_id = '{MATERIAL}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
    )
    loc = psql(
        "SELECT qty::numeric(18,4) FROM biz_inventory_location "
        f"WHERE material_id = '{MATERIAL}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
    )
    txn = psql(
        "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END), 0)::numeric(18,4) "
        f"FROM biz_inventory_txn WHERE doc_type = 'STOCK_TAKE' AND material_id = '{MATERIAL}'::uuid"
    )
    v_cnt = psql(
        "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_TAKE' "
        f"AND source_id = '{take_id}'::uuid AND deleted_at IS NULL"
    )
    notice_title = psql(
        "SELECT title FROM sys_notice WHERE idempotency_key = "
        f"'LNK:STOCK_TAKE:POSTED:{take_id}'"
    )
    say(f"- 成本账 qty|amount = {cost}（期望 92.0000|736.0000：只按差异落一次）")
    say(f"- 库位账 qty = {loc}（期望 92.0000）")
    say(f"- 盘点流水净额 = {txn}（期望 -8.0000）")
    say(f"- 盘点差异凭证 = {v_cnt} 张（期望 1：联动不许再生成一张）")
    say(f"- 过账联动通知标题 = 「{notice_title}」（期望「盘点单已过账」）")
    if cost != "92.0000|736.0000" or loc != "92.0000" or txn != "-8.0000" or v_cnt != "1":
        say("!! 库存 / 凭证口径不符（联动可能重复落账）")
        raise SystemExit(1)
    if notice_title != "盘点单已过账":
        say("!! 过账联动通知没发出来（STOCK_TAKE/POSTED 没接线或没 flush）")
        raise SystemExit(1)

    # ---------- ⑦ 已过账作废：三本账回滚 + 红冲 + 作废联动 ----------
    say()
    say("## ⑦ PATCH /status CANCELLED（已过账作废：账回滚 + 红冲）")
    cancelled, http = call(
        "PATCH",
        f"/api/business/erp/stock-take/{take_id}/status",
        {"status": "CANCELLED", "reason": "LNK04 真机取证：实盘数填错，作废重盘（账须回到基线）"},
        token,
    )
    cancelled = cancelled["data"]
    say(f"# HTTP {http} 状态 {cancelled["status"]} 红字凭证 {cancelled["reverse_voucher_no"]}")

    say()
    say("## ⑧ 库内物证（after 作废）")
    cost2 = psql(
        "SELECT (qty::numeric(18,4)) || '|' || (amount::numeric(18,4)) FROM biz_inventory "
        f"WHERE material_id = '{MATERIAL}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
    )
    loc2 = psql(
        "SELECT qty::numeric(18,4) FROM biz_inventory_location "
        f"WHERE material_id = '{MATERIAL}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
    )
    txn2 = psql(
        "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END), 0)::numeric(18,4) "
        f"FROM biz_inventory_txn WHERE doc_type = 'STOCK_TAKE' AND material_id = '{MATERIAL}'::uuid"
    )
    red = psql(
        "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_TAKE' "
        f"AND source_id = '{take_id}'::uuid AND is_red = 1"
    )
    notice_cancel = psql(
        "SELECT title FROM sys_notice WHERE idempotency_key = "
        f"'LNK:STOCK_TAKE:CANCELLED:{take_id}'"
    )
    say(f"- 成本账 qty|amount = {cost2}（期望 100.0000|800.0000：回到基线）")
    say(f"- 库位账 qty = {loc2}（期望 100.0000）")
    say(f"- 盘点流水净额 = {txn2}（期望 0.0000：正向 + 冲销）")
    say(f"- 红字凭证 = {red} 张（期望 1）")
    say(f"- 作废联动通知标题 = 「{notice_cancel}」（期望「盘点单已作废」）")
    if cost2 != "100.0000|800.0000" or loc2 != "100.0000" or txn2 != "0.0000" or red != "1":
        say("!! 作废回滚口径不符")
        raise SystemExit(1)
    if notice_cancel != "盘点单已作废":
        say("!! 作废联动通知没发出来（STOCK_TAKE/CANCELLED 没接线或没 flush）")
        raise SystemExit(1)

    say()
    say("## ⑨ 结论")
    say("过账 / 作废两条边都真的触发了联动通知；库存按实盘只落一次账（92 / 736），")
    say("作废后原路回滚（100 / 800）；每张盘点单差异凭证恰好 1 张（联动排除清单生效）。")
    say("ALL PASS")


try:
    main()
finally:
    with open(OUT, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")
    print(f"# 证据已写入 {OUT}")
