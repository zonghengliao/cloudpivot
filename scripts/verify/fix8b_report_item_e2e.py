#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P1-FIX-8B 报表项维护：隔离实例真机验收（HTTP 层，可重复跑）。

针对 `scripts/verify/fix8b_report_item_db.sh` 造出来的库 `cloudpivot_ri8`（空库 + 迁移 +
一张「借 1405 库存商品 1000 / 贷 2202 应付账款 1000」的平衡凭证），实例
`scripts/verify/fix8b_report_item_instance.sh`（端口 18160）。

用法：
    BASE_URL=http://127.0.0.1:18160 python3 scripts/verify/fix8b_report_item_e2e.py

验的是卡面两条验收标准：
  A. 维护页的五件事在接口层成立：列表 / 新增 / 编辑 / 排序 / 启停；
  B. **改一个报表项，报表立即生效** —— 改名、改取数模式都当场反映到资产负债表金额上，
     并且改回来金额也复原（不是一次性快照）。

每条都是硬断言，红就是红。脚本可重复跑：测试用的那一行按编码复用，跑完停用。
"""

import json
import os
import sys
import urllib.error
import urllib.request
from decimal import Decimal

BASE_URL = os.environ.get("BASE_URL", "http://127.0.0.1:18160").rstrip("/")
USER = os.environ.get("TEST_USER", "admin")
PASSWORD = os.environ.get("TEST_PASS", "Admin@123456")
# 首登强制改密后的口令（隔离库专用）
NEW_PASSWORD = os.environ.get("NEW_PASS", "ReportItem@2026Fix8B")
API = f"{BASE_URL}/api"

PERIOD = "2026-11"
TEST_CODE = "FIX8B_TEST_ROW"
# V0183 种的标准取数模式（改完必须能改回原值）
INVENTORY_PATTERN = "1401,1402,1403,1404,1405,1408,4001,4101"

FAILS = []
CHECKS = 0


def check(name, cond, detail=""):
    global CHECKS
    CHECKS += 1
    if cond:
        print(f"[PASS] {name}")
        return True
    print(f"[FAIL] {name}  {detail}")
    FAILS.append(f"{name} {detail}")
    return False


def http(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{API}{path}", data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
        req.add_header("x_access_token", token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as err:
        return err.code, err.read()


def body_of(status, raw):
    try:
        return json.loads(raw)
    except Exception:
        return {"_raw": raw.decode("utf-8", "ignore")[:400]}


def _login(password):
    status, raw = http("POST", "/system/auth/login", body={"username": USER, "password": password})
    if status != 200:
        return None
    return (body_of(status, raw).get("data") or {}).get("access_token")


def login():
    """登录；顺带处理隔离库 bootstrap 的「首登强制改密」。"""
    token = _login(PASSWORD)
    if not token:
        token = _login(NEW_PASSWORD)
        if not token:
            print(f"登录失败：{USER} 的两个口令都不通")
            sys.exit(2)
        return token
    status, raw = http("GET", f"/business/erp/report-item/list?report_code=BALANCE_SHEET", token=token)
    if status == 403 and "首次登录" in raw.decode("utf-8", "ignore"):
        status, body = http(
            "PUT",
            "/system/auth/password",
            token,
            {"old_password": PASSWORD, "new_password": NEW_PASSWORD},
        )
        if status != 200:
            print(f"修改初始密码失败 HTTP {status}: {body[:300]!r}")
            sys.exit(2)
        token = _login(NEW_PASSWORD)
        if not token:
            print("改密后用新口令仍登录失败")
            sys.exit(2)
        print("首次登录：已按强制改密把初始密码改成 NIK_ADMIN_PASSWORD→NEW_PASS")
    return token


def call(method, path, token, payload=None, expect=200, label=""):
    status, raw = http(method, path, token, payload)
    body = body_of(status, raw)
    if status != expect:
        raise SystemExit(f"{label or path} 期望 HTTP {expect}，实际 {status}：{json.dumps(body, ensure_ascii=False)[:600]}")
    return body


def dec(value):
    return Decimal(str(value if value is not None else "0"))


def item_rows(token, report_code):
    body = call("GET", f"/business/erp/report-item/list?report_code={report_code}", token, label="列表")
    return body["data"]


def find_item(rows, code):
    return next((row for row in rows if row["item_code"] == code), None)


def balance_sheet(token):
    body = call("GET", f"/business/erp/report/balance-sheet?period={PERIOD}", token, label="资产负债表")
    sheet = body["data"]
    lines = {}
    for side in ("assets", "liabilities", "equity"):
        for line in sheet.get(side) or []:
            lines[line["item_code"]] = line
    return sheet, lines


def patch_item(token, item, fields):
    payload = {"item_name": item["item_name"], "item_level": item["item_level"],
               "parent_code": item["parent_code"], "side": item["side"],
               "account_pattern": item["account_pattern"], "formula": item["formula"],
               "is_deduction": item["is_deduction"], "is_active": item["is_active"],
               "sort_order": item["sort_order"], "remark": item["remark"]}
    payload.update(fields)
    body = call("PUT", f"/business/erp/report-item/{item['id']}", token, payload, label="编辑")
    return body["data"]


def main():
    token = login()
    print(f"登录成功 user={USER} base={BASE_URL}")

    # ==================== ① 字典 + 53 行 ====================
    catalog = call("GET", "/business/erp/report-item/catalog", token, label="字典")["data"]
    codes = [report["report_code"] for report in catalog["reports"]]
    check("字典含两张报表（BALANCE_SHEET / INCOME_STATEMENT）",
          codes == ["BALANCE_SHEET", "INCOME_STATEMENT"], codes)
    check("所属方字典 = ASSET / LIABILITY / EQUITY",
          [side["value"] for side in catalog["sides"]] == ["ASSET", "LIABILITY", "EQUITY"],
          [side["value"] for side in catalog["sides"]])
    check("取数模式语法提示 ≥ 4 条（编码清单 / 含下级 / 前缀 / 按类别）",
          len(catalog["pattern_tips"]) >= 4, len(catalog["pattern_tips"]))

    bs_rows = item_rows(token, "BALANCE_SHEET")
    is_rows = item_rows(token, "INCOME_STATEMENT")
    # 脚本可重复跑：测试行（FIX8B_TEST_ROW）留在库里（停用），统计时要把它剔掉才等于初始口径
    extra = 1 if find_item(bs_rows, TEST_CODE) else 0
    check("报表项总数 = 53（评审实测口径；不含本卡测试行）",
          catalog["total_items"] - extra == 53, catalog["total_items"] - extra)
    check("资产负债表报表项 = 41 行", len(bs_rows) - extra == 41, len(bs_rows) - extra)
    check("利润表报表项 = 12 行", len(is_rows) == 12, len(is_rows))
    inventory = find_item(bs_rows, "INVENTORY")
    check("INVENTORY 行存在且有取数模式", inventory is not None and inventory["account_pattern"] == INVENTORY_PATTERN,
          inventory and inventory["account_pattern"])
    check("INVENTORY 命中科目数 ≥ 1", (inventory["matched_account_count"] or 0) >= 1,
          inventory["matched_account_count"])
    check("INVENTORY 被合计行引用（referenced_by 非空）", bool(inventory["referenced_by"]),
          inventory["referenced_by"])

    # ==================== ② 起点：报表金额 ====================
    sheet, lines = balance_sheet(token)
    check("资产负债表 存货 = 1000.0000", dec(lines["INVENTORY"]["amount"]) == Decimal("1000"),
          lines["INVENTORY"]["amount"])
    check("资产负债表 资产总计 = 1000.0000", dec(lines["TOTAL_ASSETS"]["amount"]) == Decimal("1000"),
          lines["TOTAL_ASSETS"]["amount"])
    check("资产负债表勾稽平（资产 ≡ 负债+权益）", sheet["check"]["balanced"] is True, sheet["check"])

    # ==================== ③ 取数预览 ====================
    preview = call("POST", "/business/erp/report-item/preview", token,
                   {"report_code": "BALANCE_SHEET", "account_pattern": "1405", "exclude_id": inventory["id"]})["data"]
    check("预览 1405 命中 1 个科目", preview["matched_count"] == 1, preview)
    check("预览命中的是 1405 库存商品",
          [account["account_code"] for account in preview["matched_accounts"]] == ["1405"],
          preview["matched_accounts"])
    bad_preview = call("POST", "/business/erp/report-item/preview", token,
                       {"report_code": "BALANCE_SHEET", "account_pattern": "@CATEGORY:FOO"})["data"]
    check("预览非法类别 → problems 非空（保存会被拒）", bool(bad_preview["problems"]), bad_preview)
    zero_preview = call("POST", "/business/erp/report-item/preview", token,
                        {"report_code": "BALANCE_SHEET", "account_pattern": "9999"})["data"]
    check("预览未命中 → warnings 明确告警「恒为 0」",
          zero_preview["matched_count"] == 0 and any("恒为 0" in w for w in zero_preview["warnings"]),
          zero_preview)

    # ==================== ④ 改名 → 报表立即生效 ====================
    saved = patch_item(token, inventory, {"item_name": "存货（P1-FIX-8B 改名验证）"})
    check("改名接口返回新名字", saved["item"]["item_name"] == "存货（P1-FIX-8B 改名验证）",
          saved["item"]["item_name"])
    _, lines = balance_sheet(token)
    check("报表立即显示新名字（同一份配置，无缓存）",
          lines["INVENTORY"]["item_name"] == "存货（P1-FIX-8B 改名验证）",
          lines["INVENTORY"]["item_name"])

    # ==================== ⑤ 改取数模式 → 报表金额跟着走 ====================
    saved = patch_item(token, saved["item"], {
        "item_name": "存货",
        "account_pattern": "1403",
    })
    check("改模式后命中科目数变 1（1403）", saved["item"]["matched_account_count"] == 1,
          saved["item"]["matched_account_count"])
    sheet2, lines2 = balance_sheet(token)
    check("报表 存货 = 0（1403 无余额）", dec(lines2["INVENTORY"]["amount"]) == Decimal("0"),
          lines2["INVENTORY"]["amount"])
    unmapped_codes = [row["account_code"] for row in sheet2["check"]["unmapped"]]
    check("1405 变成「未归类科目」（口径真的换了，不是显示层）", "1405" in unmapped_codes, unmapped_codes)
    check("资产总计仍 = 1000（未归类行兜住了差额）",
          dec(lines2["TOTAL_ASSETS"]["amount"]) == Decimal("1000"), lines2["TOTAL_ASSETS"]["amount"])

    # ==================== ⑥ 改回原值 → 金额复原 ====================
    saved = patch_item(token, saved["item"], {"account_pattern": INVENTORY_PATTERN})
    sheet3, lines3 = balance_sheet(token)
    check("改回原模式后 存货 复原 = 1000.0000", dec(lines3["INVENTORY"]["amount"]) == Decimal("1000"),
          lines3["INVENTORY"]["amount"])
    check("改回后未归类科目里不再有 1405",
          "1405" not in [row["account_code"] for row in sheet3["check"]["unmapped"]],
          [row["account_code"] for row in sheet3["check"]["unmapped"]])

    # ==================== ⑦ 新增 → 出现在报表里 → 停用后消失 ====================
    existing = find_item(item_rows(token, "BALANCE_SHEET"), TEST_CODE)
    if existing is None:
        created = call("POST", "/business/erp/report-item", token, {
            "report_code": "BALANCE_SHEET",
            "item_code": TEST_CODE,
            "item_name": "测试行（可停用）",
            "item_level": 2,
            "side": "ASSET",
            "account_pattern": "1403",
            "sort_order": 9990,
            "is_active": True,
        }, label="新增")["data"]
        test_item = created["item"]
        check("新增返回 warnings 为空（1403 有科目）", created["warnings"] == [], created["warnings"])
    else:
        test_item = existing
        if not test_item["is_active"]:
            test_item = call("PUT", f"/business/erp/report-item/{test_item['id']}/toggle", token,
                             {"is_active": True}, label="重新启用")["data"]
    _, lines4 = balance_sheet(token)
    check("新增项立即出现在资产负债表",
          lines4.get(TEST_CODE) is not None and dec(lines4[TEST_CODE]["amount"]) == Decimal("0"),
          lines4.get(TEST_CODE))

    toggled = call("PUT", f"/business/erp/report-item/{test_item['id']}/toggle", token,
                   {"is_active": False}, label="停用")["data"]
    check("停用返回 is_active=false", toggled["is_active"] is False, toggled["is_active"])
    _, lines5 = balance_sheet(token)
    check("停用后报表不再显示该行", TEST_CODE not in lines5, list(lines5)[:5])

    # ==================== ⑧ 停用被引用的项 → 409 ====================
    status, raw = http("PUT", f"/business/erp/report-item/{inventory['id']}/toggle",
                       token, {"is_active": False})
    body = body_of(status, raw)
    check("停用被合计行引用的 INVENTORY → 409", status == 409, status)
    check("409 文案点名引用者", "CURRENT_ASSETS_TOTAL" in json.dumps(body, ensure_ascii=False),
          body)

    # ==================== ⑨ 排序：整表校验 ====================
    rows = item_rows(token, "BALANCE_SHEET")
    ids = [row["id"] for row in rows]
    # 非法顺序：把 INVENTORY 挪到最后 → 合计行公式引用落到后面 → 400
    bad_order = [item_id for item_id in ids if item_id != inventory["id"]] + [inventory["id"]]
    status, raw = http("PUT", "/business/erp/report-item/sort", token,
                       {"report_code": "BALANCE_SHEET", "ids": bad_order})
    body = body_of(status, raw)
    check("把被引用项挪到最后的排序 → 400", status == 400, status)
    check("400 文案说明「必须排在前面」",
          "排在前面" in json.dumps(body, ensure_ascii=False), body)

    # 合法顺序：交换 MONETARY_FUNDS / NOTES_RECEIVABLE 两行的位置再换回来
    codes_now = [row["item_code"] for row in rows]
    i1, i2 = codes_now.index("MONETARY_FUNDS"), codes_now.index("NOTES_RECEIVABLE")
    swapped = ids[:]
    swapped[i1], swapped[i2] = swapped[i2], swapped[i1]
    ok_rows = call("PUT", "/business/erp/report-item/sort", token,
                   {"report_code": "BALANCE_SHEET", "ids": swapped}, label="排序")["data"]
    new_codes = [row["item_code"] for row in ok_rows]
    check("合法交换后顺序生效", new_codes[i1] == "NOTES_RECEIVABLE" and new_codes[i2] == "MONETARY_FUNDS",
          new_codes[max(i1 - 1, 0):i2 + 2])
    check("重排后 sort_order 重新编号为 10 的整数倍",
          [row["sort_order"] for row in ok_rows] == [(index + 1) * 10 for index in range(len(ok_rows))],
          [row["sort_order"] for row in ok_rows][:8])
    restored = call("PUT", "/business/erp/report-item/sort", token,
                    {"report_code": "BALANCE_SHEET", "ids": ids}, label="排序复原")["data"]
    check("换回原顺序后逐行编码与初始一致",
          [row["item_code"] for row in restored] == codes_now,
          [row["item_code"] for row in restored][:5])

    # ==================== ⑩ 校验：公式顺序 / 编码重复 ====================
    status, raw = http("POST", "/business/erp/report-item", token, {
        "report_code": "BALANCE_SHEET",
        "item_code": "FIX8B_BAD_FORMULA",
        "item_name": "非法公式行",
        "item_level": 2,
        "side": "ASSET",
        "formula": "TOTAL_ASSETS + INVENTORY",
        # 排在最前面，于是它引用的两个项都落在它后面 → 必须被拒
        "sort_order": 1,
    })
    body = body_of(status, raw)
    check("公式引用排在后面的项 → 400", status == 400, status)
    check("400 点名「排在自己后面」", "排在自己后面" in json.dumps(body, ensure_ascii=False), body)

    status, raw = http("POST", "/business/erp/report-item", token, {
        "report_code": "BALANCE_SHEET",
        "item_code": "inventory",
        "item_name": "重复编码",
        "item_level": 2,
        "side": "ASSET",
        "account_pattern": "1405",
    })
    check("编码重复（大小写归一后）→ 409", status == 409, status)

    status, raw = http("POST", "/business/erp/report-item", token, {
        "report_code": "BALANCE_SHEET",
        "item_code": "FIX8B_EMPTY_SOURCE",
        "item_name": "没有任何取数来源",
        "item_level": 2,
        "side": "ASSET",
    })
    check("层级 2 却没有取数模式 / 公式 → 400", status == 400, status)

    status, raw = http("PUT", f"/business/erp/report-item/{inventory['id']}", token,
                       {"item_code": "INVENTORY_RENAMED"})
    check("改编码 → 400（引用键不可变）", status == 400, status)

    # ==================== ⑪ 连跑两轮：只读结果逐字一致 ====================
    first = json.dumps(item_rows(token, "BALANCE_SHEET"), ensure_ascii=False, sort_keys=True)
    second = json.dumps(item_rows(token, "BALANCE_SHEET"), ensure_ascii=False, sort_keys=True)
    check("同一份配置连读两次逐字一致（无隐式写入 / 无缓存抖动）", first == second, "两次响应不同")

    print()
    print(f"== 断言 {CHECKS} 条，FAIL {len(FAILS)}")
    if FAILS:
        for failure in FAILS:
            print(f"   ✗ {failure}")
        sys.exit(1)
    print("== 全部通过")


if __name__ == "__main__":
    main()
