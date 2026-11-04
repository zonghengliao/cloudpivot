#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""P9 端到端验收：建单 → 过账 → 作废 → 三账回基线（真实 HTTP + 真库）

跑在 144 上，实例指向 cloudpivot_p9e（prod 修复后的副本）。
四个快照必须满足：④ 作废后 == ① 基线（逐字段相等）。
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
import urllib.request

BASE = os.environ.get("P9_BASE", "http://127.0.0.1:18091")
DB = os.environ.get("P9_DB", "cloudpivot_p9e")
USER = "cloudpivot"
PW = os.environ["NIK_DATABASE_PASSWORD"]
ADMIN_PW = os.environ.get("P9_ADMIN_PW") or os.environ["NIK_ADMIN_PASSWORD"]


def psql(sql):
    env = dict(os.environ, PGPASSWORD=PW)
    r = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-U", USER, "-d", DB, "-tA", "-F", "\x1f", "-c", sql],
        capture_output=True, text=True, env=env, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip()[:400])
    return [line.split("\x1f") for line in r.stdout.strip().split("\n") if line]


def http(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw}


def snapshot(tag):
    """三账 + 凭证 + 流水（按本卡测试物料/仓库过滤，避免被别的数据干扰）"""
    row = psql(
        "select "
        "coalesce((select sum(qty)::text from biz_inventory where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select sum(amount)::text from biz_inventory where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select sum(qty)::text from biz_inventory_location where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select count(*)::text from biz_inventory_txn where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select sum(case when txn_type='IN' then qty else -qty end)::text "
        "          from biz_inventory_txn where material_id = {m} and warehouse_id = {w}),'0'),"
        "coalesce((select count(*)::text from biz_voucher where deleted_at is null),'0'),"
        "coalesce((select sum(coalesce(e.debit_amount,0)-coalesce(e.credit_amount,0))::text "
        "          from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id and v.deleted_at is null "
        "          join biz_account a on a.id=e.account_id where a.account_code like '1405%'),'0'),"
        "coalesce((select count(*)::text from biz_voucher where deleted_at is null and is_red=1),'0'),"
        "coalesce((select count(*)::text from biz_voucher where deleted_at is null and source_no = {doc}),'0')"
        .format(m="'" + MAT + "'::uuid", w="'" + WH + "'::uuid", doc="'" + DOC_NO[0] + "'")
    )[0]
    names = ["成本账数量", "成本账金额", "库位账数量", "流水条数", "流水净额",
             "凭证总数", "1405净额", "红字凭证数", "本单凭证数"]
    snap = dict(zip(names, row))
    print(f"\n【{tag}】")
    for k, v in snap.items():
        print(f"    {k:>10} = {v}")
    return snap


def main():
    global MAT, WH, DOC_NO
    MAT = psql("select id from biz_material where material_code='M-W1A-001' and deleted_at is null")[0][0]
    WH = psql("select id from biz_warehouse where warehouse_code='WH01'")[0][0]
    SUP = psql("select id from biz_supplier where deleted_at is null order by created_at limit 1")[0][0]
    DOC_NO = ["(尚未建单)"]

    st, res = http("POST", "/api/system/auth/login", {"username": "admin", "password": ADMIN_PW})
    if st != 200 or res.get("code") != 0:
        print(f"登录失败：{st} {res}")
        return 1
    token = res["data"]["access_token"]
    print(f"登录成功（must_change_password={res['data'].get('must_change_password')}）")

    base = snapshot("① 基线（建单前）")

    st, res = http("POST", "/api/business/erp/stock-in", {
        "supplier_id": SUP, "warehouse_id": WH, "remark": "P9 e2e：作废回滚验证",
        "items": [{"material_id": MAT, "qty": "9", "price_excl": "7", "tax_rate": "13"}],
    }, token)
    if st != 200 or res.get("code") != 0:
        print(f"建单失败：{st} {res}")
        return 1
    doc_id = res["data"]["header"]["id"]
    DOC_NO[0] = res["data"]["header"]["in_no"]
    print(f"\n建单成功：{DOC_NO[0]}（id={doc_id}，9 件 @7 不含税）")
    after_create = snapshot("② 建单后（应 == 基线：草稿不碰库存）")

    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status", {"status": "POSTED"}, token)
    if st != 200 or res.get("code") != 0:
        print(f"过账失败：{st} {res}")
        return 1
    after_post = snapshot("③ 过账后（成本账 +9 件 / +63 元，库位账同步 +9，凭证 +1）")

    st, res = http("PATCH", f"/api/business/erp/stock-in/{doc_id}/status",
                   {"status": "CANCELLED", "reason": "P9 e2e：作废必须退库存"}, token)
    if st != 200 or res.get("code") != 0:
        print(f"作废失败：{st} {res}")
        return 1
    after_cancel = snapshot("④ 作废后（必须 == ① 基线）")

    print("\n===== 断言 =====")
    ok = True
    # ② 建单是草稿：三账 + 凭证一个都不许动
    for k in base:
        if k == "本单凭证数":
            continue
        if base[k] != after_create[k]:
            print(f"  [FAIL] ② 建单后 {k} 变了：基线 {base[k]} → {after_create[k]}")
            ok = False
    print("  [PASS] ② 建单后 三账/凭证全部未动（草稿不碰账）" if ok else "")

    checks = [
        ("③ 过账后 成本账数量 = 基线 + 9",
         f"{float(base['成本账数量']) + 9:.4f}", f"{float(after_post['成本账数量']):.4f}"),
        ("③ 过账后 成本账金额 = 基线 + 63",
         f"{float(base['成本账金额']) + 63:.4f}", f"{float(after_post['成本账金额']):.4f}"),
        ("③ 过账后 库位账数量 = 基线 + 9（P6 两账同步）",
         f"{float(base['库位账数量']) + 9:.4f}", f"{float(after_post['库位账数量']):.4f}"),
        ("③ 过账后 1405 净额 = 基线 + 63",
         f"{float(base['1405净额']) + 63:.4f}", f"{float(after_post['1405净额']):.4f}"),
        ("③ 过账后 流水条数 = 基线 + 1", str(int(base["流水条数"]) + 1), after_post["流水条数"]),
        # ④ 作废：库存三账回基线；凭证侧留痕（原凭证 + 红字各一张）
        ("④ 作废后 成本账数量 == 基线", base["成本账数量"], after_cancel["成本账数量"]),
        ("④ 作废后 成本账金额 == 基线", base["成本账金额"], after_cancel["成本账金额"]),
        ("④ 作废后 库位账数量 == 基线", base["库位账数量"], after_cancel["库位账数量"]),
        ("④ 作废后 流水净额 == 基线", base["流水净额"], after_cancel["流水净额"]),
        ("④ 作废后 1405 净额 == 基线（红冲后净额归零）",
         f"{float(base['1405净额']):.4f}", f"{float(after_cancel['1405净额']):.4f}"),
        ("④ 作废后 流水条数 = 基线 + 2（IN + 冲销 OUT）",
         str(int(base["流水条数"]) + 2), after_cancel["流水条数"]),
        ("④ 作废后 凭证总数 = 基线 + 2（蓝字 + 红字）",
         str(int(base["凭证总数"]) + 2), after_cancel["凭证总数"]),
        ("④ 作废后 红字凭证数 = 基线 + 1",
         str(int(base["红字凭证数"]) + 1), after_cancel["红字凭证数"]),
        ("④ 本单凭证数 = 2（一蓝一红）", "2", after_cancel["本单凭证数"]),
    ]
    for name, want, got in checks:
        if str(want) == str(got):
            print(f"  [PASS] {name}")
        else:
            print(f"  [FAIL] {name}\n         期望 {want}\n         实际 {got}")
            ok = False
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
