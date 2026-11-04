#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""ERP-S1 真机验收（V0187 标准基础数据）——对**生产实例**做只增不删的接口实测 + 自清理。

用法（144 上）：
    set -a; . /etc/cloudpivot/cloudpivot.env; set +a
    BASE=http://127.0.0.1:8023 python3 scripts/verify/erps1_prod_e2e.py

断言项（对应卡面 §五 真机验收）：
  ① 计量单位页接口返回 ≥ 20 个（实际 28），并逐个带 is_system / precision
  ② 物料单位下拉（同 `/erp/unit/page`，page_size=200）能选到「千克」「米」「箱」
  ③ 系统预置 vs 客户自建可区分：预置 is_system=true 且打不了删除（409），自建 false 可删
  ④ 物料分类 7 值可写（PACKAGING 建得动），非法值 400
  ⑤ 结算方式字典 pay_method 7 项
自清理：脚本自己造的物料 / 单位全部软删 + 物理清掉（只删本脚本前缀的行）。
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

BASE = os.environ.get("BASE", "http://127.0.0.1:8023").rstrip("/")
ADMIN_USER = os.environ.get("S1_ADMIN_USER", "admin")
ADMIN_PASSWORD = os.environ.get("NIK_ADMIN_PASSWORD") or os.environ.get("ALUN_ADMIN_PASSWORD") or ""
TAG = "S1-E2E"

passed = 0
failed = 0


def ok(msg):
    global passed
    passed += 1
    print(f"PASS  {msg}")


def no(msg):
    global failed
    failed += 1
    print(f"FAIL  {msg}")


def call(method, path, body=None, token=None, expect=200):
    url = f"{BASE}{path}"
    data = json.dumps(body, ensure_ascii=False).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            payload = json.loads(resp.read().decode() or "{}")
            code = resp.status
    except urllib.error.HTTPError as err:  # 4xx/5xx 也要能看到响应体
        payload = json.loads(err.read().decode() or "{}")
        code = err.code
    return code, payload


def psql(sql):
    """清理用：直连库（走 NIK_DATABASE_* 环境）。"""
    env = dict(os.environ)
    env["PGPASSWORD"] = env.get("NIK_DATABASE_PASSWORD", "")
    cmd = [
        "psql", "-At", "-U", env.get("NIK_DATABASE_USER", "cloudpivot"),
        "-h", env.get("NIK_DATABASE_HOST", "127.0.0.1"),
        "-p", env.get("NIK_DATABASE_PORT", "5432"),
        "-d", env.get("NIK_DATABASE_NAME", "cloudpivot"),
        "-c", sql,
    ]
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


print("=" * 54)
print(f"ERP-S1 真机验收：{BASE}")
print("=" * 54)

if not ADMIN_PASSWORD:
    print("缺 NIK_ADMIN_PASSWORD（先 set -a; . /etc/cloudpivot/cloudpivot.env; set +a）")
    sys.exit(2)

# ---------- 登录 ----------
code, payload = call("POST", "/api/system/auth/login", {"username": ADMIN_USER, "password": ADMIN_PASSWORD})
token = (payload.get("data") or {}).get("access_token")
if code == 200 and token:
    ok("管理员登录成功（access_token 已取到）")
else:
    print(f"登录失败：{code} {payload}")
    sys.exit(2)

# ---------- ① 单位列表 ----------
code, payload = call("GET", "/api/business/erp/unit/page?page=1&page_size=100", token=token)
data = payload.get("data") or {}
rows = data.get("list") or []
total = data.get("total")
if code == 200 and total and total >= 20:
    ok(f"① 计量单位页 total = {total}（≥ 20，卡面验收口径）")
else:
    no(f"① 单位列表异常：code={code} total={total}")

fields_ok = all("is_system" in r and "precision" in r for r in rows)
ok(f"① 每条出参都带 is_system / precision（{len(rows)} 条）") if fields_ok else no("① 出参缺 is_system / precision")

system_rows = [r for r in rows if r.get("is_system")]
customer_rows = [r for r in rows if not r.get("is_system")]
if len(system_rows) >= 24:
    ok(f"① 系统预置单位 {len(system_rows)} 个（is_system = true）")
else:
    no(f"① 系统预置单位只有 {len(system_rows)} 个（期望 ≥ 24）")

codes = {r["unit_code"]: r for r in rows}
for code_name, name in (("KG", "千克"), ("M", "米"), ("BOX", "箱")):
    row = codes.get(code_name)
    if row and row["unit_name"] == name:
        ok(f"② 单位下拉能选到「{name}」（{code_name}，precision={row['precision']}）")
    else:
        no(f"② 下拉缺「{name}」（{code_name}）")

kg = codes.get("KG")
if kg and kg.get("precision") == 3:
    ok("② 千克 precision = 3（重量类 3 位小数）")
else:
    no(f"② 千克 precision 不等于 3：{kg and kg.get('precision')}")

# ---------- ③ 系统预置不能删 / 自建可删 ----------
if kg is None:
    no("③ 列表里没有 KG 行，跳过「预置不可删」实测")
else:
    code, payload = call("DELETE", f"/api/business/erp/unit/{kg['id']}", token=token)
    msg = payload.get("msg") or payload.get("message") or ""
    if code == 409 and "系统预置" in msg:
        ok(f"③ 系统预置单位（千克）删除被拒 409：{msg}")
    else:
        no(f"③ 系统预置单位删除没被拦住：code={code} msg={msg}")

code, payload = call(
    "POST",
    "/api/business/erp/unit",
    {"unit_code": f"{TAG}-U1", "unit_name": "S1 自建单位", "is_base": True, "status": 1, "precision": 2},
    token=token,
)
created = payload.get("data") or {}
if code == 200 and created.get("is_system") is False and created.get("precision") == 2:
    ok("③ 客户自建单位 is_system = false、precision = 2（接口不开放 is_system 开关）")
    unit_id = created["id"]
else:
    no(f"③ 自建单位创建异常：code={code} data={created}")
    unit_id = None

if unit_id:
    code, payload = call("DELETE", f"/api/business/erp/unit/{unit_id}", token=token)
    ok("③ 客户自建单位可以删除（200）") if code == 200 else no(f"③ 自建单位删不掉：code={code} {payload}")

# ---------- ④ 物料分类 7 值 ----------
material_id = None
if kg:
    code, payload = call(
        "POST",
        "/api/business/erp/material",
        {
            "material_code": f"{TAG}-M1",
            "material_name": "S1 包装物测试",
            "unit_id": kg["id"],
            "material_type": "PACKAGING",
            "costing_method": "MOVING_AVG",
            "status": 1,
        },
        token=token,
    )
    data = payload.get("data") or {}
    if code == 200 and data.get("material_type") == "PACKAGING":
        ok("④ 物料分类新增值 PACKAGING（包装物）建得动")
        material_id = data["id"]
    else:
        no(f"④ PACKAGING 建不动：code={code} msg={payload.get('msg') or payload.get('message')}")

    code, payload = call(
        "POST",
        "/api/business/erp/material",
        {
            "material_code": f"{TAG}-M2",
            "material_name": "S1 非法分类",
            "unit_id": kg["id"],
            "material_type": "BOGUS",
            "costing_method": "MOVING_AVG",
            "status": 1,
        },
        token=token,
    )
    msg = payload.get("msg") or payload.get("message") or ""
    if code == 400 and "material_type" in msg:
        ok(f"④ 非法分类被拒 400：{msg}")
    else:
        no(f"④ 非法分类没被拒：code={code} msg={msg}")

# ---------- ⑤ 结算方式字典 ----------
code, payload = call("GET", "/api/support/dict/items/pay_method", token=token)
items = payload.get("data") or []
values = [i.get("item_value") for i in items]
expect = {"现金", "银行转账", "承兑汇票", "支票", "微信", "支付宝", "赊账"}
if code == 200 and set(values) == expect:
    ok(f"⑤ 结算方式字典 pay_method 7 项：{', '.join(values)}")
else:
    no(f"⑤ 结算方式字典异常：code={code} values={values}")

# ---------- 清理 ----------
print("-- 清理本脚本造的数据（只删 S1-E2E 前缀行）")
if material_id:
    call("DELETE", f"/api/business/erp/material/{material_id}", token=token)
res = psql(f"delete from biz_material where material_code like '{TAG}-%'")
res2 = psql(f"delete from biz_unit where unit_code like '{TAG}-%'")
left = psql(f"select count(*) from biz_material where material_code like '{TAG}-%'").stdout.strip()
left2 = psql(f"select count(*) from biz_unit where unit_code like '{TAG}-%'").stdout.strip()
if left == "0" and left2 == "0":
    ok("清理完成：物料 / 单位无 S1-E2E 残留行")
else:
    no(f"清理后仍有残留：material={left} unit={left2}")

print("=" * 54)
print(f"PASS={passed} FAIL={failed}")
print("=" * 54)
sys.exit(0 if failed == 0 else 1)
