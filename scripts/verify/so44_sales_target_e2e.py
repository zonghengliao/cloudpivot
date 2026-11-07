#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""SO-44 销售目标与预测：真实 token + 真实 HTTP 验收（隔离实例 127.0.0.1:18044）。

前置：bash scripts/verify/so44_start.sh（库 cloudpivot_so44，迁移含 V0318）
      PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so44 -f scripts/verify/so44_seed.sql

口径期望（夹具见 so44_seed.sql 头注释）：
    2026-11 订单口径：SALESPERSON 11300 / DEPT 11300 / MATERIAL(原料A) 6780 / ALL 11300
    2026-11 收款口径：SALESPERSON 4000 / DEPT 4000 / ALL 4000
                       MATERIAL(原料A) 2400.00、原料B 1600.00（按含税占比分摊）
    2026-Q4  季度口径：ALL 订单 20188 = 11300（11 月单）+ 8888（10 月单也在 Q4 内）、
                      收款 4000（Q4 只有 11 月那笔收款）
"""
import json
import sys
import urllib.error
import urllib.parse
import urllib.request

BASE = "http://127.0.0.1:18044"
SALES_A = "aa440000-0000-4000-8000-000000000002"
DEPT = "aa440000-0000-4000-8000-000000000001"
MATERIAL_A = "d1370003-0000-4000-8000-000000000001"
MATERIAL_B = "d1370003-0000-4000-8000-000000000002"
CUSTOMER = "d0260000-0000-4000-8000-000000000001"
ADMIN = "admin"
PASSWORD = "Admin@123456"
PASSWORD_CHANGED = "Admin@123456so44"

PASS, FAIL = [], []


def check(name, ok, detail=""):
    if ok:
        PASS.append(name)
        print(f"[PASS] {name}" + (f" | {detail}" if detail else ""))
    else:
        FAIL.append(name)
        print(f"[FAIL] {name} | {detail}")


def money(value):
    """把 '11300.0000' / 11300 统一成两位小数字符串。"""
    try:
        return f"{float(value):.2f}"
    except (TypeError, ValueError):
        return str(value)


def req(method, path, token=None, body=None):
    data = None
    headers = {}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = "Bearer " + token
    request = urllib.request.Request(BASE + path, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as error:
        payload = error.read()
        try:
            return error.code, json.loads(payload.decode())
        except Exception:
            return error.code, {"raw": payload.decode("utf-8", "replace")}


def login():
    token = None
    current = PASSWORD
    for candidate in (PASSWORD, PASSWORD_CHANGED):
        status, body = req("POST", "/api/system/auth/login",
                           body={"username": ADMIN, "password": candidate})
        if status == 200:
            token = (body.get("data") or {}).get("access_token")
            current = candidate
            break
    if not token:
        print("登录失败：", status, body)
        sys.exit(2)
    data = body.get("data") or {}
    if data.get("must_change_password"):
        req("PUT", "/api/system/auth/password", token,
            {"old_password": current, "new_password": PASSWORD_CHANGED})
        status, body = req("POST", "/api/system/auth/login",
                           body={"username": ADMIN, "password": PASSWORD_CHANGED})
        token = (body.get("data") or {}).get("access_token")
    return token


def q(params):
    return "?" + urllib.parse.urlencode(params)


TARGET_IDS = {}


def create_target(token, name, scope_type, scope_id, period_code, amount, qty="0"):
    payload = {
        "scope_type": scope_type,
        "scope_id": scope_id,
        "period_code": period_code,
        "target_amount": amount,
        "target_qty": qty,
        "remark": f"SO-44 e2e {name}",
    }
    status, body = req("POST", "/api/business/erp/sales-target", token, payload)
    check(f"POST 目标 {name} 200", status == 200, f"status={status} body={str(body)[:240]}")
    data = body.get("data") or {}
    if data.get("id"):
        TARGET_IDS[name] = data["id"]
    return data


def achievement(token, period, scope_type, basis=None):
    params = {"period_code": period, "scope_type": scope_type}
    if basis:
        params["basis"] = basis
    status, body = req("GET", "/api/business/erp/sales-target/achievement" + q(params), token)
    return status, body


def row_of(body, scope_name):
    for row in body.get("data") or []:
        if row.get("scope") == scope_name:
            return row
    return None


def main():
    token = login()
    print("token 已获取（长度 %d）" % len(token))

    # ---------- ① 目标 5 条（4 种范围 + 季度） ----------
    alice = create_target(token, "salesperson", "SALESPERSON", SALES_A, "2026-11", "10000.00", "100")
    check("目标编号 STG- 前缀 + 日期流水格式",
          # 编号形状 {prefix}-{yyyyMMdd}-{6 位流水} ⇒ 3 + 1 + 8 + 1 + 6 = 19 位
          str(alice.get("target_no", "")).startswith("STG-2026")
          and len(alice.get("target_no", "")) == 19
          and alice.get("target_no", "")[-7] == "-",
          f"target_no={alice.get('target_no')}")
    check("目标回读 scope_name = 销售员甲", alice.get("scope_name") == "销售员甲",
          f"scope_name={alice.get('scope_name')}")
    check("目标回读金额 10000.00", money(alice.get("target_amount")) == "10000.00",
          f"amount={alice.get('target_amount')}")
    check("目标回读状态 ACTIVE", alice.get("status") == "ACTIVE", f"status={alice.get('status')}")

    dept = create_target(token, "dept", "DEPT", DEPT, "2026-11", "20000.00")
    check("部门目标 scope_name = 销售一部", dept.get("scope_name") == "销售一部",
          f"scope_name={dept.get('scope_name')}")

    mat_a = create_target(token, "material_a", "MATERIAL_CATEGORY", MATERIAL_A, "2026-11", "5000.00")
    check("物料目标 scope_name = 原料A（种子）", str(mat_a.get("scope_name", "")).startswith("原料A"),
          f"scope_name={mat_a.get('scope_name')}")

    create_target(token, "all", "ALL", None, "2026-11", "11300.00")
    create_target(token, "q4", "ALL", None, "2026-Q4", "50000.00")

    # 同范围同期间再建一条 → 409（库层部分唯一索引）
    status, body = req("POST", "/api/business/erp/sales-target", token, {
        "scope_type": "SALESPERSON", "scope_id": SALES_A, "period_code": "2026-11",
        "target_amount": "1.00"})
    check("同范围同期间重复建目标 → 409", status == 409, f"status={status} body={str(body)[:200]}")
    check("409 报文点明唯一口径", "唯一" in str(body.get("msg", "")), f"msg={str(body.get('msg'))[:160]}")

    # 非法入参
    status, body = req("POST", "/api/business/erp/sales-target", token, {
        "scope_type": "ALL", "scope_id": SALES_A, "period_code": "2026-11",
        "target_amount": "1.00"})
    check("ALL 带 scope_id → 400", status == 400, f"status={status}")
    status, body = req("POST", "/api/business/erp/sales-target", token, {
        "scope_type": "SALESPERSON", "scope_id": SALES_A, "period_code": "2026-13",
        "target_amount": "1.00"})
    check("非法期间 2026-13 → 400", status == 400, f"status={status}")
    status, body = req("POST", "/api/business/erp/sales-target", token, {
        "scope_type": "SALESPERSON", "scope_id": SALES_A, "period_code": "2026-11",
        "target_amount": "-5.00"})
    check("负目标 → 400", status == 400, f"status={status}")
    status, body = req("POST", "/api/business/erp/sales-target", token, {
        "scope_type": "SALESPERSON", "scope_id": SALES_A, "period_code": "2026-11",
        "target_amount": "1.005"})
    check("3 位小数金额 → 400", status == 400, f"status={status}")
    status, body = req("POST", "/api/business/erp/sales-target", token, {
        "scope_type": "REGION", "scope_id": SALES_A, "period_code": "2026-11",
        "target_amount": "1.00"})
    check("非法 scope_type → 400", status == 400, f"status={status}")
    status, body = req("POST", "/api/business/erp/sales-target", token, {
        "scope_type": "SALESPERSON", "scope_id": "d0000000-0000-4000-8000-0000000000ff",
        "period_code": "2026-11", "target_amount": "1.00"})
    check("范围对象不存在 → 400", status == 400, f"status={status}")

    # ---------- ② 目标分页 ----------
    status, body = req("GET", "/api/business/erp/sales-target/page" + q(
        {"period_code": "2026-11", "page": 1, "page_size": 20}), token)
    check("目标分页 200", status == 200, f"status={status}")
    data = body.get("data") or {}
    check("2026-11 目标 4 条", data.get("total") == 4, f"total={data.get('total')} list={len(data.get('list') or [])}")
    status, body = req("GET", "/api/business/erp/sales-target/page" + q(
        {"scope_type": "SALESPERSON", "keyword": "销售员甲"}), token)
    check("关键字（范围名）能命中", (body.get("data") or {}).get("total") == 1,
          f"total={(body.get('data') or {}).get('total')}")

    # ---------- ③ 看板：订单口径 ----------
    status, body = achievement(token, "2026-11", "SALESPERSON", "ORDER")
    check("看板 SALESPERSON/ORDER 200", status == 200, f"status={status} body={str(body)[:200]}")
    row = row_of(body, "销售员甲")
    check("看板行存在（销售员甲）", row is not None, f"rows={[r.get('scope') for r in (body.get('data') or [])]}")
    if row:
        check("订单口径实际 = 11300.00", money(row.get("actual_amount")) == "11300.00",
              f"actual={row.get('actual_amount')}")
        check("达成率 = 113.00", money(row.get("achievement_rate")) == "113.00",
              f"rate={row.get('achievement_rate')}")
        check("缺口 = -1300.00（超额）", money(row.get("gap")) == "-1300.00", f"gap={row.get('gap')}")
        check("同一条行里也带收款口径 = 4000.00",
              money((row.get("by_receipt") or {}).get("actual_amount")) == "4000.00",
              f"by_receipt={row.get('by_receipt')}")
        check("收款口径达成率 = 40.00",
              money((row.get("by_receipt") or {}).get("achievement_rate")) == "40.00",
              f"rate={ (row.get('by_receipt') or {}).get('achievement_rate')}")
        check("目标金额回显 10000.00", money(row.get("target_amount")) == "10000.00",
              f"target={row.get('target_amount')}")
        check("看板行带 target_no", str(row.get("target_no", "")).startswith("STG-"),
              f"target_no={row.get('target_no')}")

    # 口径切换：basis=RECEIPT
    status, body = achievement(token, "2026-11", "SALESPERSON", "RECEIPT")
    row = row_of(body, "销售员甲")
    check("basis=RECEIPT 时 actual = 4000.00", row and money(row.get("actual_amount")) == "4000.00",
          f"actual={row and row.get('actual_amount')}")
    check("basis=RECEIPT 时 gap = 6000.00", row and money(row.get("gap")) == "6000.00",
          f"gap={row and row.get('gap')}")
    check("basis=RECEIPT 时 by_order 仍是 11300.00",
          row and money((row.get("by_order") or {}).get("actual_amount")) == "11300.00",
          f"by_order={row and row.get('by_order')}")

    # 部门口径
    status, body = achievement(token, "2026-11", "DEPT", "ORDER")
    row = row_of(body, "销售一部")
    check("DEPT 订单口径 = 11300.00", row and money(row.get("actual_amount")) == "11300.00",
          f"actual={row and row.get('actual_amount')}")
    check("DEPT 达成率 = 56.50（目标 20000）", row and money(row.get("achievement_rate")) == "56.50",
          f"rate={row and row.get('achievement_rate')}")
    check("DEPT 缺口 = 8700.00", row and money(row.get("gap")) == "8700.00",
          f"gap={row and row.get('gap')}")

    # 物料口径
    status, body = achievement(token, "2026-11", "MATERIAL_CATEGORY", "ORDER")
    row = row_of(body, "原料A（种子）")
    check("MATERIAL 订单口径 = 6780.00（6000+780）",
          row and money(row.get("actual_amount")) == "6780.00", f"actual={row and row.get('actual_amount')}")
    check("MATERIAL 达成率 = 135.60（目标 5000）",
          row and money(row.get("achievement_rate")) == "135.60",
          f"rate={row and row.get('achievement_rate')}")
    check("MATERIAL 缺口 = -1780.00", row and money(row.get("gap")) == "-1780.00",
          f"gap={row and row.get('gap')}")
    check("MATERIAL 收款口径分摊 = 2400.00",
          row and money((row.get("by_receipt") or {}).get("actual_amount")) == "2400.00",
          f"by_receipt={row and row.get('by_receipt')}")

    # 全公司口径
    status, body = achievement(token, "2026-11", "ALL", "ORDER")
    row = row_of(body, "全公司")
    check("ALL 订单口径 = 11300.00", row and money(row.get("actual_amount")) == "11300.00",
          f"actual={row and row.get('actual_amount')}")
    check("ALL 达成率 = 100.00（目标 11300）", row and money(row.get("achievement_rate")) == "100.00",
          f"rate={row and row.get('achievement_rate')}")
    check("ALL 缺口 = 0.00", row and money(row.get("gap")) == "0.00", f"gap={row and row.get('gap')}")
    check("ALL 收款口径 = 4000.00",
          row and money((row.get("by_receipt") or {}).get("actual_amount")) == "4000.00",
          f"by_receipt={row and row.get('by_receipt')}")

    # ---------- ④ 季度期间 + 干扰数据 ----------
    status, body = achievement(token, "2026-Q4", "ALL", "ORDER")
    row = row_of(body, "全公司")
    # Q4 = 10/01–12/31：夹具里的 11 月单 11300 与「期外」的 10 月单 8888 **都在季度内**
    # （10 月单是「月度口径的干扰数据」，不是「季度口径的干扰数据」）⇒ 20188
    check("2026-Q4 订单口径 = 20188.00（11 月 11300 + 10 月 8888）",
          row and money(row.get("actual_amount")) == "20188.00", f"actual={row and row.get('actual_amount')}")
    check("2026-Q4 达成率 = 40.38（目标 50000）",
          row and money(row.get("achievement_rate")) == "40.38",
          f"rate={row and row.get('achievement_rate')}")
    status, body = achievement(token, "2026-10", "SALESPERSON", "ORDER")
    check("2026-10 期间无目标 → 空列表（不报错）", status == 200 and body.get("data") == [],
          f"status={status} data={body.get('data')}")

    # ---------- ⑤ 看板入参校验 ----------
    status, body = achievement(token, "2026-11", "SALESPERSON", "CASH")
    check("非法 basis → 400", status == 400, f"status={status}")
    status, body = req("GET", "/api/business/erp/sales-target/achievement", token)
    check("缺 period_code → 400", status == 400, f"status={status}")
    status, body = achievement(token, "2026-Q9", "SALESPERSON")
    check("非法季度 → 400", status == 400, f"status={status}")
    status, body = achievement(token, "2026-11", "REGION")
    check("非法 scope_type → 400", status == 400, f"status={status}")
    status, body = req("GET", "/api/business/erp/sales-target/achievement" + q(
        {"period_code": "2026-11", "scope_type": "SALESPERSON"}))
    check("无 token → 401", status == 401, f"status={status}")

    # ---------- ⑥ 目标编辑 / 停用 / 删除 ----------
    if "salesperson" not in TARGET_IDS or "dept" not in TARGET_IDS:
        check("目标创建全部成功（后续编辑 / 删除依赖 id）", False, f"ids={list(TARGET_IDS)}")
        print(f"PASS = {len(PASS)} / FAIL = {len(FAIL)}")
        return 1
    target_id = TARGET_IDS["salesperson"]
    status, body = req("PUT", f"/api/business/erp/sales-target/{target_id}", token,
                       {"target_amount": "12500.50"})
    check("编辑目标金额 200", status == 200, f"status={status} body={str(body)[:200]}")
    check("编辑后金额 = 12500.50", money((body.get("data") or {}).get("target_amount")) == "12500.50",
          f"amount={(body.get('data') or {}).get('target_amount')}")
    status, body = achievement(token, "2026-11", "SALESPERSON", "ORDER")
    row = row_of(body, "销售员甲")
    check("改目标后达成率重算 = 90.40", row and money(row.get("achievement_rate")) == "90.40",
          f"rate={row and row.get('achievement_rate')}")

    status, body = req("PUT", f"/api/business/erp/sales-target/{target_id}", token,
                       {"status": "INACTIVE"})
    check("停用目标 200", status == 200, f"status={status}")
    status, body = achievement(token, "2026-11", "SALESPERSON", "ORDER")
    check("停用后看板不再出现该目标", body.get("data") == [],
          f"data={body.get('data')}")
    req("PUT", f"/api/business/erp/sales-target/{target_id}", token, {"status": "ACTIVE"})

    status, body = req("DELETE", f"/api/business/erp/sales-target/{TARGET_IDS['dept']}", token)
    check("删除部门目标 200", status == 200, f"status={status} body={str(body)[:160]}")
    status, body = req("DELETE", f"/api/business/erp/sales-target/{TARGET_IDS['dept']}", token)
    check("重复删除 → 404", status == 404, f"status={status}")
    status, body = req("GET", "/api/business/erp/sales-target/page" + q(
        {"scope_type": "DEPT", "period_code": "2026-11"}), token)
    check("删除后分页 0 条", (body.get("data") or {}).get("total") == 0,
          f"total={(body.get('data') or {}).get('total')}")
    status, body = req("PUT", "/api/business/erp/sales-target/not-a-uuid-editor", token, {})
    check("非法 UUID → 400", status == 400, f"status={status}")

    # ---------- ⑦ 预测 ----------
    status, body = req("POST", "/api/business/erp/sales-forecast", token, {
        "customer_id": CUSTOMER, "material_id": MATERIAL_A, "period_code": "2026-11",
        "forecast_amount": "88888.00", "confidence": "HIGH", "version_no": 1,
        "remark": "SO-44 e2e 客户×物料预测"})
    check("POST 预测 200", status == 200, f"status={status} body={str(body)[:240]}")
    forecast = body.get("data") or {}
    check("预测编号 SFC- 前缀 + 19 位", str(forecast.get("forecast_no", "")).startswith("SFC-2026")
          and len(forecast.get("forecast_no", "")) == 19, f"no={forecast.get('forecast_no')}")
    check("预测回读客户名", str(forecast.get("customer_name", "")).startswith("北京云枢"),
          f"customer_name={forecast.get('customer_name')}")
    check("预测回读信心度 HIGH", forecast.get("confidence") == "HIGH",
          f"confidence={forecast.get('confidence')}")
    check("预测回读版本 1", forecast.get("version_no") == 1, f"version={forecast.get('version_no')}")

    status, body = req("POST", "/api/business/erp/sales-forecast", token, {
        "customer_id": CUSTOMER, "material_id": MATERIAL_A, "period_code": "2026-11",
        "forecast_amount": "1.00", "version_no": 1})
    check("同维度同期间同版本 → 409", status == 409, f"status={status}")

    status, body = req("POST", "/api/business/erp/sales-forecast", token, {
        "period_code": "2026-11", "forecast_amount": "500000.00", "confidence": "LOW"})
    global_forecast = body.get("data") or {}
    check("全局预测（客户与物料都空）200", status == 200, f"status={status} body={str(body)[:200]}")
    check("全局预测回读客户为空", global_forecast.get("customer_id") is None,
          f"customer_id={global_forecast.get('customer_id')}")

    status, body = req("POST", "/api/business/erp/sales-forecast", token, {
        "period_code": "2026-11", "forecast_amount": "1.00", "confidence": "SURE"})
    check("非法信心度 → 400", status == 400, f"status={status}")
    status, body = req("POST", "/api/business/erp/sales-forecast", token, {
        "period_code": "2026-11", "forecast_amount": "1.00", "version_no": 0})
    check("版本号 0 → 400", status == 400, f"status={status}")

    status, body = req("GET", "/api/business/erp/sales-forecast/page" + q(
        {"period_code": "2026-11"}), token)
    check("预测分页 200 且 2 条", status == 200 and (body.get("data") or {}).get("total") == 2,
          f"status={status} total={(body.get('data') or {}).get('total')}")

    status, body = req("PUT", f"/api/business/erp/sales-forecast/{forecast.get('id')}", token,
                       {"forecast_amount": "99999.00", "confidence": "MEDIUM"})
    check("编辑预测 200", status == 200, f"status={status} body={str(body)[:200]}")
    check("编辑后金额 = 99999.00", money((body.get("data") or {}).get("forecast_amount")) == "99999.00",
          f"amount={(body.get('data') or {}).get('forecast_amount')}")

    status, body = req("DELETE", f"/api/business/erp/sales-forecast/{global_forecast.get('id')}", token)
    check("删除预测 200", status == 200, f"status={status}")
    status, body = req("GET", "/api/business/erp/sales-forecast/page" + q(
        {"period_code": "2026-11"}), token)
    check("删除后预测 1 条", (body.get("data") or {}).get("total") == 1,
          f"total={(body.get('data') or {}).get('total')}")

    # ---------- ⑧ 汇总 ----------
    print()
    print("=" * 72)
    print(f"PASS = {len(PASS)} / FAIL = {len(FAIL)}")
    if FAIL:
        print("失败项：")
        for name in FAIL:
            print("  -", name)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
