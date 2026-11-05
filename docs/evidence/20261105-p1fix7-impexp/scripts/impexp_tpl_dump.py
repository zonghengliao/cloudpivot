#!/usr/bin/env python3
"""下载全部可导入定义的模板并逐 sheet 打印内容（含字段说明）。"""
import json
import os
import sys
import urllib.request

BASE = sys.argv[1]
TOKEN = open("/tmp/impexp-token.txt").read().strip()
OUT = sys.argv[2] if len(sys.argv) > 2 else "/tmp/impexp-templates"
os.makedirs(OUT, exist_ok=True)


def get_json(path):
    req = urllib.request.Request(BASE + path, headers={"Authorization": "Bearer " + TOKEN})
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read())


def get_bytes(path):
    req = urllib.request.Request(BASE + path, headers={"Authorization": "Bearer " + TOKEN})
    with urllib.request.urlopen(req, timeout=120) as resp:
        return resp.read(), dict(resp.headers)


try:
    import openpyxl
except ImportError:
    print("NO openpyxl")
    sys.exit(2)

defs = get_json("/api/business/impexp/def/list")["data"]
for d in defs:
    if not d["supports_import"]:
        continue
    bt = d["biz_type"]
    body, headers = get_bytes("/api/business/impexp/import/template?biz_type=" + bt)
    path = os.path.join(OUT, bt.replace(".", "_") + ".xlsx")
    open(path, "wb").write(body)
    print("=" * 70)
    print("BIZ", bt, "| bytes", len(body), "| content-type", headers.get("Content-Type"), "| disp", headers.get("Content-Disposition"))
    wb = openpyxl.load_workbook(path)
    print(" sheets:", wb.sheetnames)
    for name in wb.sheetnames:
        ws = wb[name]
        print(f" --- sheet {name} dims={ws.dimensions} rows={ws.max_row} cols={ws.max_column}")
        for row in ws.iter_rows(min_row=1, max_row=min(ws.max_row, 4), values_only=True):
            print("   ", [("" if c is None else str(c))[:28] for c in row])
