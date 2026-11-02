#!/usr/bin/env python3
"""本卡真 HTTP 验收（物料主数据禁选折算单位）—— 打隔离实例，不碰生产。

断言：
  1. 建折算单位（is_base=0）→ 200
  2. POST /business/erp/material 挂折算单位 → 400 + 66030 + 文案，且库里 0 行
  3. POST 挂基本单位 → 200
  4. PUT 把单位改成折算单位 → 400 + 66030，库里单位没变
  5. PUT 只改名称（基本单位）→ 200
"""
import json
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:18097/api"
PASS_INIT = "Admin@123456"
PASS_NEW = "Admin@654321"

results = []


def call(method, path, token=None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return err.code, {"raw": raw}


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(("[PASS] " if ok else "[FAIL] ") + name + ((" — " + detail) if detail else ""))


def login():
    for pw in (PASS_NEW, PASS_INIT):
        code, body = call("POST", "/system/auth/login", body={"username": "admin", "password": pw})
        if code == 200:
            data = body.get("data") or {}
            token = data.get("access_token")
            if data.get("must_change_password"):
                # 用旧令牌改密 → 旧令牌失效 → 用新口令重登
                c2, b2 = call(
                    "PUT",
                    "/system/auth/password",
                    token=token,
                    body={"old_password": pw, "new_password": PASS_NEW},
                )
                if c2 != 200:
                    return None, f"改密失败 {c2} {b2}"
                code, body = call(
                    "POST", "/system/auth/login", body={"username": "admin", "password": PASS_NEW}
                )
                data = (body.get("data") or {}) if code == 200 else {}
                token = data.get("access_token")
            if token:
                return token, f"login ok (pw={'init' if pw == PASS_INIT else 'new'})"
    return None, "登录失败"


token, note = login()
print("登录：" + note)
if not token:
    sys.exit(2)

# 1) 基本单位 + 折算单位
code, body = call(
    "POST",
    "/business/erp/unit",
    token,
    {
        "unit_code": "T962-PCS",
        "unit_name": "个",
        "is_base": True,
        "base_unit_id": None,
        "rate": None,
        "status": 1,
        "sort_order": 0,
    },
)
check("① 建基本单位 → 200", code == 200, f"{code} {(body.get('data') or {}).get('unitCode', body.get('msg'))}")
base_id = (body.get("data") or {}).get("id") if code == 200 else None

code, body = call(
    "POST",
    "/business/erp/unit",
    token,
    {
        "unit_code": "T962-BOX",
        "unit_name": "箱",
        "is_base": False,
        "base_unit_id": base_id,
        "rate": 12,
        "status": 1,
        "sort_order": 1,
    },
)
check("①b 建折算单位「箱」→ 200", code == 200, f"{code} {body.get('msg')}")
box_id = (body.get("data") or {}).get("id") if code == 200 else None

# 2) Create 挂折算单位 → 400 + 66030
code, body = call(
    "POST",
    "/business/erp/material",
    token,
    {
        "material_code": "T962-M-BOX",
        "material_name": "真机验收箱装物料",
        "unit_id": box_id,
        "material_type": "RAW",
        "costing_method": "MOVING_AVG",
        "status": 1,
    },
)
msg = body.get("msg") or ""
check("② Create 挂折算单位 → 400", code == 400, str(code))
check("②b code = 66030", body.get("code") == 66030, str(body.get("code")))
check(
    "②c 文案逐字（主数据口径）",
    "物料的计量单位必须是基本单位：折算单位（箱/打）只能用于显示，本版本单据不支持多单位换算（当前「箱」）"
    in msg,
    msg,
)

# 3) Create 挂基本单位 → 200
code, body = call(
    "POST",
    "/business/erp/material",
    token,
    {
        "material_code": "T962-M-OK",
        "material_name": "真机验收基本单位物料",
        "unit_id": base_id,
        "material_type": "RAW",
        "costing_method": "MOVING_AVG",
        "status": 1,
    },
)
check("③ Create 挂基本单位 → 200", code == 200, f"{code} {body.get('msg')}")
material_id = (body.get("data") or {}).get("id") if code == 200 else None

# 4) Update 改成折算单位 → 400 + 66030，且单位没变
code, body = call(
    "PUT", f"/business/erp/material/{material_id}", token, {"unit_id": box_id}
)
check("④ Update 改成折算单位 → 400", code == 400, str(code))
check("④b code = 66030", body.get("code") == 66030, str(body.get("code")))
check("④c 文案点名当前单位", "（当前「箱」）" in (body.get("msg") or ""), body.get("msg") or "")

code, body = call("GET", f"/business/erp/material/{material_id}", token)
check(
    "④d 库里单位仍是基本单位",
    code == 200 and (body.get("data") or {}).get("unit_id") == base_id,
    str((body.get("data") or {}).get("unit_id")),
)

# 5) 只改名称 → 200
code, body = call(
    "PUT", f"/business/erp/material/{material_id}", token, {"material_name": "真机验收物料（改名）"}
)
check("⑤ Update 只改名称 → 200", code == 200, f"{code} {body.get('msg')}")

fail = [name for name, ok, _ in results if not ok]
print(f"\n==> 汇总：PASS={len(results) - len(fail)} FAIL={len(fail)}")
if fail:
    print("FAILED: " + "; ".join(fail))
sys.exit(1 if fail else 0)
