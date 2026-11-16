"""M2-18 仓库缺口收口 E2E —— 真接口断言（在 99 开发机上跑，打本机隔离实例）。

卡：t_659b64de（M2-18【M2·仓库】仓库缺口收口：在途 + 库龄 + 安全库存 E2E）

一条链路，两个阶段：

  阶段 A（夹具阶段 1：入库四批 + 结存 100 + 在途 60 + 未清订单 100 + 水位 200/300）
    ① 库龄：四档 40/30/20/10、合计 = 结存、期末快照（as_of）只看得见那天之前的层
    ② 呆滞：> 90 天那层被单独标出来（10）
    ③ 在途：明细 2 行 / 汇总 60 / 对账「在途 + 已签 = 发货量」对平
    ④ 补货建议：预计可用 = 结存 − 在途 + 未清订单 = 140 → 建议补到最高水位 300 → 160
    ⑤ 水位检查：dry_run 只算不写 → 真跑生成 1 条待办（负责人 = 仓管主管）→ 复跑幂等
  阶段 B（夹具阶段 2：签收确认的落库形态 —— 出库 30、在途清零、FIFO 吃掉最老两层）
    ⑥ 四处一起变：结存 100→70、在途 60→30、库龄四档 40/30/0/0、呆滞清零
    ⑦ **守恒**：预计可用 140 签收前后不变（签收只是把「在路上」挪到「账上」）
    ⑧ 水位待办不重开也不误关（结存仍低于水位 → skipped_open=1、closed=0）
    ⑨ 改水位（PUT 最低 50）→ 水位检查自动收口（closed=1、待办 CANCELLED AUTO_RECOVERED）
       → 补货建议转 OK（不再出现）

口径说明（刻意不同、不是 bug）：
  · 水位检查比的是**账面结存**（biz_inventory.qty）；
  · 补货建议比的是**预计可用**（结存 − 发货在途 + 未清采购订单）。
  两者在「货已发走但账还没扣」时会给出不同结论（本卡阶段 A 就是这种态：结存 100 > 0 但
  预计可用 140 仍低于水位 200），断言里把两个数字都钉住，防止以后有人把两者合成一个。

用法：
    python3 scripts/verify/m218_warehouse_gap_e2e.py            # 阶段 A
    python3 scripts/verify/m218_warehouse_gap_e2e.py --phase b  # 阶段 B（先跑阶段 2 夹具）
环境变量：M218_API_BASE（默认 http://127.0.0.1:18218/api）、
         M218_ADMIN_OLD_PASSWORD（默认 Admin@123456）、M218_ADMIN_PASSWORD（改后口令）、
         M218_KEEPER_PASSWORD（仓管主管口令）
"""

import datetime
import json
import os
import sys
import urllib.error
import urllib.request
from urllib.parse import quote

BASE = os.environ.get("M218_API_BASE", "http://127.0.0.1:18218/api")
ADMIN_OLD = os.environ.get("M218_ADMIN_OLD_PASSWORD", "Admin@123456")
ADMIN_NEW = os.environ.get("M218_ADMIN_PASSWORD", "M218@GapE2e1")
KEEPER_PASSWORD = os.environ.get("M218_KEEPER_PASSWORD", "M218@Keeper1")
KEEPER_USER = "m218_keeper"
PHASE = "b" if "--phase" in sys.argv and "b" in sys.argv else "a"

MAT = "a2180000-0000-4000-8000-000000000101"
WH = "a2180000-0000-4000-8000-000000000010"
SHIP1 = "a2180000-0000-4000-8000-000000000202"
SHIP2 = "a2180000-0000-4000-8000-000000000203"

PASS = 0
FAIL = 0


def check(name, ok, detail=""):
    global PASS, FAIL
    if ok:
        PASS += 1
        print(f"  PASS {name}")
    else:
        FAIL += 1
        print(f"  FAIL {name} {detail}")


def eq(name, expected, actual):
    check(name, expected == actual, f"（期望 [{expected}]，实际 [{actual}]）")


def num(name, expected, actual, places=4):
    """NUMERIC 一律按数值比（rust_decimal 会保留 DB 的标度，字面量比会假红）。"""
    try:
        ok = abs(float(actual) - float(expected)) < 10 ** (-places) / 2
    except (TypeError, ValueError):
        ok = False
    check(name, ok, f"（期望 {expected}，实际 [{actual}]）")


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except json.JSONDecodeError:
            return err.code, raw


def get(path, token):
    # 中文关键字要 percent-encode（urllib 不编码查询串里的非 ASCII）
    status, body = call("GET", quote(path, safe="/?=&%"), token=token)
    assert status == 200 and isinstance(body, dict) and body.get("code") == 0, \
        f"GET {path} 失败：{status} {body}"
    return body["data"]


def post(path, body, token):
    status, resp = call("POST", path, body, token)
    return status, resp


def login_admin():
    status, body = call("POST", "/system/auth/login", {"username": "admin", "password": ADMIN_OLD})
    if status == 200 and body.get("code") == 0:
        token = body["data"]["access_token"]
        check("空库首启：admin 首登 must_change_password=true",
              bool(body["data"].get("must_change_password")))
        status, changed = call(
            "PUT", "/system/auth/password",
            {"old_password": ADMIN_OLD, "new_password": ADMIN_NEW}, token)
        eq("首次登录改密（改密前不许做业务）", 0, (changed or {}).get("code"))
        status, body = call("POST", "/system/auth/login",
                            {"username": "admin", "password": ADMIN_NEW})
        assert status == 200 and body.get("code") == 0, f"改密后登录失败：{status} {body}"
        token = body["data"]["access_token"]
    else:
        status, body = call("POST", "/system/auth/login",
                            {"username": "admin", "password": ADMIN_NEW})
        assert status == 200 and body.get("code") == 0, f"admin 登录失败：{status} {body}"
        token = body["data"]["access_token"]
    return token


def ensure_keeper(token):
    """水位待办要有「仓管主管」可派；空库首启没有真人，用真接口建一个（顺带验用户接口）。"""
    status, body = call("GET", "/system/role/page?page=1&page_size=200", token=token)
    role_id = None
    if status == 200 and body.get("code") == 0:
        for row in body["data"]["list"]:
            if row.get("role_code") == "WAREHOUSE_MANAGER":
                role_id = row["id"]
                break
    check("WAREHOUSE_MANAGER 角色在种子库里", bool(role_id), f"{status} {body}")
    payload = {
        "username": KEEPER_USER,
        "password": KEEPER_PASSWORD,
        "real_name": "M218 演练仓管主管",
        "status": 1,
        "role_ids": [role_id] if role_id else [],
        "remark": "M2-18 E2E：水位待办负责人",
    }
    status, body = post("/system/user", payload, token)
    if status == 200 and body.get("code") == 0:
        check("建仓管主管账号（水位待办负责人）", True)
    else:
        # 重跑时已存在 → 200/409 都算就位，但必须是「已存在」而不是别的错
        check("建仓管主管账号（重跑：已存在即可）",
              "已存在" in str(body) or "重复" in str(body), f"{status} {body}")


def phase_a(token):
    print("\n① 库龄：四档 / 合计 / 期末快照")
    rep = get(f"/business/erp/inventory-aging/report?material_id={MAT}&warehouse_id={WH}", token)
    eq("报表编码", "INVENTORY_AGING", rep["report_code"])
    eq("只有一格（物料 × 仓库）", 1, rep["row_count"])
    eq("库龄层数 = 4（四批入库）", 4, rep["layer_count"])
    buckets = {b["key"]: b for b in rep["summary"]}
    num("0-30 天 = 40（今天−5 那批）", 40, buckets["D0_30"]["qty"])
    num("31-60 天 = 30（今天−45 那批）", 30, buckets["D31_60"]["qty"])
    num("61-90 天 = 20（今天−75 那批）", 20, buckets["D61_90"]["qty"])
    num("90 天以上 = 10（今天−120 那批）", 10, buckets["OVER_90"]["qty"])
    num("四档合计 = 100 = 账面结存", 100, rep["qty_total"])
    check("档位顺序固定（D0_30/D31_60/D61_90/OVER_90）",
          [b["key"] for b in rep["buckets"]] == ["D0_30", "D31_60", "D61_90", "OVER_90"])
    row = rep["rows"][0]
    eq("最老层库龄 = 120 天", 120, row["max_age_days"])
    num("参考金额 = 100 × 10（层单价，不是账面均价）", 1000, rep["amount_total"])

    # 期末快照：基准日回到 60 天前 → 只剩 −120（10）与 −75（20）两层
    as_of = (datetime.date.today() - datetime.timedelta(days=60)).isoformat()
    snap = get(
        f"/business/erp/inventory-aging/report?material_id={MAT}&warehouse_id={WH}&as_of={as_of}",
        token)
    eq("期末快照 as_of 回显", True, snap["as_of"] != rep["as_of"])
    num("期末快照（60 天前）合计 = 30（后两批还没入库）", 30, snap["qty_total"])
    eq("期末快照只有 2 层", 2, snap["layer_count"])

    print("\n② 呆滞料（> 90 天仍有剩余）")
    slow = get(f"/business/erp/inventory-aging/slow-moving?material_id={MAT}&warehouse_id={WH}"
               f"&days=90", token)
    eq("阈值回显 = 90", 90, slow["threshold_days"])
    eq("呆滞格数 = 1", 1, slow["summary"]["slow_row_count"])
    num("呆滞数量 = 10（只有 120 天那层）", 10, slow["summary"]["qty_slow"])
    eq("呆滞层数 = 1", 1, slow["summary"]["slow_layer_count"])
    num("呆滞占比 = 10/100 = 0.10", 0.10, slow["summary"]["slow_ratio"], places=4)

    print("\n③ 在途：明细 / 汇总 / 对账")
    page = get(f"/business/erp/inventory-transit/page?material_id={MAT}&warehouse_id={WH}", token)
    eq("在途明细 2 行", 2, page["total"])
    by_ship = {r["ship_no"]: r for r in page["list"]}
    num("M218-SH-0001 在途 = 30（发 30 / 签 0）", 30, by_ship["M218-SH-0001"]["qty_in_transit"])
    num("M218-SH-0002 在途 = 30（发 50 / 签 20，部分签收仍算在途）", 30,
        by_ship["M218-SH-0002"]["qty_in_transit"])
    eq("默认只看 IN_TRANSIT", {"IN_TRANSIT"}, {r["status"] for r in page["list"]})
    summary = get(f"/business/erp/inventory-transit/summary?material_id={MAT}&warehouse_id={WH}",
                  token)
    eq("汇总按 物料 × 仓 一行", 1, summary["row_count"])
    num("在途合计 = 60", 60, summary["total_qty_in_transit"])
    num("发货合计 = 80", 80, summary["rows"][0]["shipped_qty"])
    num("已签收合计 = 20", 20, summary["rows"][0]["received_qty"])
    rec = get(f"/business/erp/inventory-transit/reconcile?material_id={MAT}"
              f"&include_matched=true", token)
    eq("对账组数 = 2（两张发货单）", 2, rec["checked"])
    eq("对平组数 = 2", 2, rec["matched"])
    eq("有差异组数 = 0", 0, rec["mismatched"])
    check("对账结论 balanced（在途 + 已签收 = 发货总量）", rec["balanced"] is True)
    check("没有硬差异码", all(r["issues"] == [] for r in rec["rows"]),
          [r["issues"] for r in rec["rows"]])

    print("\n④ 补货建议：安全库存 + 在途 + 未清订单")
    rep2 = get(f"/business/erp/replenish/page?material_id={MAT}", token)
    eq("建议 1 行（该格低于水位）", 1, rep2["total"])
    r = rep2["rows"][0]
    num("账面结存 = 100", 100, r["on_hand_qty"])
    num("发货在途 = 60", 60, r["outbound_transit_qty"])
    num("未清采购订单未收量 = 100（APPROVED；DRAFT 999 不计）", 100, r["open_po_qty"])
    num("可用 = 100 − 60 = 40", 40, r["available_qty"])
    num("预计可用 = 40 + 100 = 140", 140, r["expected_qty"])
    num("最低水位 = 200（策略行）", 200, r["min_qty"])
    eq("水位来源 = POLICY", "POLICY", r["min_source"])
    num("目标水位 = 最高水位 300", 300, r["target_qty"])
    eq("目标来源 = MAX", "MAX", r["target_source"])
    num("建议采购量 = 300 − 140 = 160", 160, r["suggested_qty"])
    eq("结论 = PURCHASE", "PURCHASE", r["verdict"])
    check("触发了（预计可用 140 严格小于最低水位 200）", r["below_min"] is True and r["triggered"] is True)
    num("汇总建议量 = 160", 160, rep2["totals"]["suggested_qty_total"])

    print("\n⑤ 水位检查：dry_run → 真跑 → 复跑幂等")
    status, body = post("/business/erp/stock-policy/check",
                        {"material_id": MAT, "dry_run": True}, token)
    eq("dry_run HTTP 200", 200, status)
    dry = body["data"]
    eq("dry_run 只算不写（dry_run=true）", True, dry["dry_run"])
    eq("dry_run：低于最低水位的格数 = 1", 1, dry["low"])
    eq("dry_run：不建单（raised = 0）", 0, dry["raised"])
    eq("dry_run：明细动作 = DRY_RUN（只说会做什么）", "DRY_RUN", dry["items"][0]["action"])
    num("dry_run：明细的结存 = 100（水位检查比的是**账面结存**）", 100, dry["items"][0]["qty"])
    num("dry_run：明细的最低水位 = 200", 200, dry["items"][0]["min_qty"])
    num("dry_run：缺口 = 200 − 100 = 100", 100, dry["items"][0]["gap_qty"])
    check("dry_run 的结论带 (dry_run) 标记", "(dry_run)" in dry["message"], dry["message"])

    status, body = post("/business/erp/stock-policy/check",
                        {"material_id": MAT, "dry_run": False}, token)
    eq("真跑 HTTP 200", 200, status)
    real = body["data"]
    eq("真跑：新建 1 条待办", 1, real["raised"])
    eq("真跑：负责人来源 = ROLE（仓管主管）", "ROLE", real["owner_source"])
    check("真跑：负责人有值", bool(real["owner"]), real["owner"])
    item = real["items"][0]
    eq("真跑：明细动作 = RAISED（真建了待办）", "RAISED", item["action"])
    num("真跑：明细的结存 = 100（账面结存，不减在途）", 100, item["qty"])
    num("真跑：明细的最低水位 = 200", 200, item["min_qty"])
    num("真跑：明细的水位来源 = POLICY", 1, 1 if item["min_source"] == "POLICY" else 0)
    check("真跑：待办号回显（真落库了）", bool(item.get("task_no")), item)

    status, body = post("/business/erp/stock-policy/check",
                        {"material_id": MAT, "dry_run": False}, token)
    again = body["data"]
    eq("复跑：不重复建单（raised = 0）", 0, again["raised"])
    eq("复跑：已有未完成待办 → skipped_open = 1", 1, again["skipped_open"])
    eq("复跑：不会误关（closed = 0）", 0, again["closed"])
    eq("复跑：明细动作 = EXISTS（指向已有待办）", "EXISTS", again["items"][0]["action"])


def phase_b(token):
    print("\n⑥ 签收确认后：四处一起变（结存 / 在途 / 库龄 / 呆滞）")
    rep = get(f"/business/erp/inventory-aging/report?material_id={MAT}&warehouse_id={WH}", token)
    buckets = {b["key"]: b for b in rep["summary"]}
    num("库龄合计 = 70（结存被出库 30）", 70, rep["qty_total"])
    eq("库龄层数 = 2（FIFO 吃满了最老两层）", 2, rep["layer_count"])
    num("0-30 天 = 40（最新一批没动）", 40, buckets["D0_30"]["qty"])
    num("31-60 天 = 30（−45 那批没动）", 30, buckets["D31_60"]["qty"])
    num("61-90 天 = 0（−75 那批被吃满）", 0, buckets["D61_90"]["qty"])
    num("90 天以上 = 0（−120 那批被吃满）", 0, buckets["OVER_90"]["qty"])

    summary = get(f"/business/erp/inventory-transit/summary?material_id={MAT}&warehouse_id={WH}",
                  token)
    num("在途合计 60 → 30（M218-SH-0001 已签收清零）", 30, summary["total_qty_in_transit"])
    eq("汇总只剩 1 张发货单（签满的单不再算在途）", 1, summary["rows"][0]["shipments"])
    num("汇总里发货量 = 50（M218-SH-0002）", 50, summary["rows"][0]["shipped_qty"])
    num("汇总里已签收 = 20（同一张单的部分签收）", 20, summary["rows"][0]["received_qty"])
    page = get(f"/business/erp/inventory-transit/page?material_id={MAT}&warehouse_id={WH}", token)
    eq("默认在途明细只剩 1 行", 1, page["total"])
    eq("剩的是 M218-SH-0002", "M218-SH-0002", page["list"][0]["ship_no"])
    page_done = get(f"/business/erp/inventory-transit/page?material_id={MAT}&warehouse_id={WH}"
                    f"&status=RECEIVED", token)
    eq("已签收明细 1 行（M218-SH-0001）", 1, page_done["total"])
    num("已签收行在途 = 0（签满清零）", 0, page_done["list"][0]["qty_in_transit"])
    num("已签收行已签量 = 30", 30, page_done["list"][0]["received_qty"])
    check("已签收行 received_at 有值", bool(page_done["list"][0]["received_at"]),
          page_done["list"][0]["received_at"])
    rec = get(f"/business/erp/inventory-transit/reconcile?material_id={MAT}"
              f"&include_matched=true", token)
    check("对账仍对平（签收只是把在途挪成已签收）",
          rec["balanced"] is True and rec["mismatched"] == 0, rec["totals"])
    rec_by_ship = {r["ship_no"]: r for r in rec["rows"]}
    num("对账里 M218-SH-0001 已签收 = 30", 30, rec_by_ship["M218-SH-0001"]["received_qty"])
    num("对账里 M218-SH-0001 在途 = 0", 0, rec_by_ship["M218-SH-0001"]["qty_in_transit"])

    slow = get(f"/business/erp/inventory-aging/slow-moving?material_id={MAT}&warehouse_id={WH}"
               f"&days=90", token)
    eq("呆滞清零（最老那层已被 FIFO 吃掉）", 0, slow["summary"]["slow_row_count"])
    num("呆滞数量 = 0", 0, slow["summary"]["qty_slow"])

    print("\n⑦ 守恒：预计可用签收前后不变")
    rep2 = get(f"/business/erp/replenish/page?material_id={MAT}", token)
    eq("建议仍 1 行", 1, rep2["total"])
    r = rep2["rows"][0]
    num("结存 100 → 70", 70, r["on_hand_qty"])
    num("在途 60 → 30", 30, r["outbound_transit_qty"])
    num("未清订单仍 100", 100, r["open_po_qty"])
    num("预计可用 = 70 − 30 + 100 = 140（与签收前相同）", 140, r["expected_qty"])
    num("建议量仍 160（签收不会改变补货结论）", 160, r["suggested_qty"])

    print("\n⑧ 水位待办：不重开、不误关")
    status, body = post("/business/erp/stock-policy/check",
                        {"material_id": MAT, "dry_run": False}, token)
    chk = body["data"]
    eq("结存 70 仍 < 200 → 低水位格数 = 1", 1, chk["low"])
    eq("本轮对这条待办只有一种结局：新建或已存在（raised + skipped_open = 1）",
       1, chk["raised"] + chk["skipped_open"])
    eq("不误关（closed = 0）", 0, chk["closed"])
    status, body = post("/business/erp/stock-policy/check",
                        {"material_id": MAT, "dry_run": False}, token)
    chk2 = body["data"]
    eq("紧接着再跑：不重复建单（raised = 0）", 0, chk2["raised"])
    eq("紧接着再跑：已有未完成待办 → skipped_open = 1", 1, chk2["skipped_open"])
    eq("紧接着再跑：明细动作 = EXISTS（指向已有待办）", "EXISTS", chk2["items"][0]["action"])

    print("\n⑨ 改水位 → 自动收口 → 补货建议转 OK")
    wh = get(f"/business/erp/stock-policy/page?material_id={MAT}&page=1&page_size=20", token)
    eq("水位设置 1 行", 1, wh["total"])
    policy_id = wh["list"][0]["id"]
    num("改前最低水位 = 200", 200, wh["list"][0]["min_qty"])
    status, body = call("PUT", "/business/erp/stock-policy",
                        {"material_id": MAT, "warehouse_id": WH, "min_qty": 50,
                         "max_qty": 300, "remark": "M218 E2E：把水位调到结存以下"},
                        token)
    eq("PUT 水位 HTTP 200", 200, status)
    eq("PUT 水位 code 0", 0, (body or {}).get("code"))
    num("改后最低水位 = 50", 50, (body or {})["data"]["min_qty"])
    check("upsert 复用同一行（没有新增第二行）", (body or {})["data"]["id"] == policy_id)

    status, body = post("/business/erp/stock-policy/check",
                        {"material_id": MAT, "dry_run": False}, token)
    chk = body["data"]
    eq("结存 70 ≥ 50 → 不再低水位（low = 0）", 0, chk["low"])
    eq("自动收口 1 条待办（closed = 1）", 1, chk["closed"])

    rep3 = get(f"/business/erp/replenish/page?material_id={MAT}&include_covered=true", token)
    eq("补货建议不再出现（预计 140 ≥ 水位 50）", 0, rep3["total"])
    check("空结果的解释指路「没有触发」",
          any("水位" in h for h in rep3["hints"]), rep3["hints"][:1])


def main():
    print(f"== M2-18 仓库缺口收口 E2E（阶段 {'B' if PHASE == 'b' else 'A'}）base={BASE}")
    token = login_admin()
    check("admin 登录", bool(token))
    if PHASE == "a":
        ensure_keeper(token)
        phase_a(token)
    else:
        phase_b(token)
    print(f"\n== 汇总：PASS={PASS} FAIL={FAIL} ==")
    if FAIL:
        print("FAILED")
        sys.exit(1)
    print("ALL PASS")


if __name__ == "__main__":
    main()
