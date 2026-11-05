#!/usr/bin/env python3
"""7 个内置导入模板：逐个从真机下载，并用 openpyxl 回读数据验证。"""
import json
import os
import urllib.error
import urllib.request

import openpyxl

BASE = "http://127.0.0.1:18142"


def req(path, data=None, headers=None, method=None):
    r = urllib.request.Request(BASE + path, data=data, headers=headers or {}, method=method)
    try:
        with urllib.request.urlopen(r, timeout=120) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


body = json.dumps({"username": "admin", "password": "Nereus792379"}).encode()
_, raw = req("/api/system/auth/login", body, {"Content-Type": "application/json"}, "POST")
tok = json.loads(raw)["data"]["access_token"]
auth = {"Authorization": "Bearer " + tok}

_, raw = req("/api/business/impexp/def/list", headers=auth)
defs = json.loads(raw)["data"]
print("DEF SAMPLE =", json.dumps(defs[0], ensure_ascii=False))

os.makedirs("all7", exist_ok=True)
ok = 0
for d in defs:
    biz = d["biz_type"]
    status, payload = req(f"/api/business/impexp/import/template?biz_type={biz}", headers=auth)
    if status != 200:
        print(f"{biz:34} skip (template {status})")
        continue
    path = f"all7/{biz}.xlsx"
    open(path, "wb").write(payload)
    wb = openpyxl.load_workbook(path)
    ws = wb["数据"]
    dvs = list(ws.data_validations.dataValidation)
    kinds = {}
    for dv in dvs:
        col = "".join(c for c in str(dv.sqref).split(":")[0] if c.isalpha())
        kinds[col] = f'{dv.type}={(dv.formula1 or "").strip(chr(34))}'
    ok += 1
    print(f"{biz:34} {status} {len(payload):6}B sheets={wb.sheetnames} DV={len(dvs)} {kinds}")
print("IMPORTABLE TEMPLATES =", ok)
