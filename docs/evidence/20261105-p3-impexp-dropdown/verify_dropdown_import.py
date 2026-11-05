#!/usr/bin/env python3
"""真机验证：模板枚举列有 Excel 下拉，且新模板仍能被导入（P3 卡 t_1ffde110）。

用法: uv run --with openpyxl python verify_dropdown_import.py <base> <outdir>
"""
import json
import os
import sys
import urllib.error
import urllib.request

import openpyxl

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:18142"
OUT = sys.argv[2] if len(sys.argv) > 2 else "/root/.hermes/profiles/backend-developer/cache/scratch/dv1"
os.makedirs(OUT, exist_ok=True)
BIZ = "business.erp.material"


def req(path, data=None, headers=None, method=None):
    h = dict(headers or {})
    r = urllib.request.Request(BASE + path, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(r, timeout=300) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def login():
    for pwd in ("Admin@123456", "Nereus792379"):
        body = json.dumps({"username": "admin", "password": pwd}).encode()
        status, raw = req(
            "/api/system/auth/login",
            body,
            {"Content-Type": "application/json"},
            "POST",
        )
        data = json.loads(raw)
        if status == 200 and data.get("code") == 0:
            print(f"LOGIN ok (password={pwd})")
            return data["data"]["access_token"]
        print(f"LOGIN failed for {pwd}: {status} {data.get('msg')}")
    raise SystemExit("登录失败")


token = login()
auth = {"Authorization": "Bearer " + token}

# 1) 下载物料模板
status, body = req(f"/api/business/impexp/import/template?biz_type={BIZ}", headers=auth)
tpl = os.path.join(OUT, "tpl_dropdown.xlsx")
open(tpl, "wb").write(body)
print("TEMPLATE", status, len(body), "bytes ->", tpl)
assert status == 200 and body[:4] == b"PK\x03\x04"

# 2) 回读：openpyxl（第三方解析器）看到的数据验证
wb = openpyxl.load_workbook(tpl)
ws = wb["数据"]
dvs = list(ws.data_validations.dataValidation)
print(f"DATA VALIDATIONS = {len(dvs)}")
for dv in dvs:
    print(f"  sqref={dv.sqref!s:16} type={dv.type:8} operator={dv.operator} "
          f"formula1={dv.formula1!r} promptTitle={dv.promptTitle!r}")

by_col = {}
for dv in dvs:
    for cell_range in str(dv.sqref).split():
        col = "".join(ch for ch in cell_range.split(":")[0] if ch.isalpha())
        by_col[col] = dv
print("COLUMNS WITH VALIDATION =", sorted(by_col))

headers = [c.value for c in ws[1]]
# 必填列表头带 `*`（如「物料编码*」），比对时按去掉星标后的名字
clean = [h.rstrip("*") for h in headers]
print("HEADERS =", headers)


def col_letter(index):  # 1 基 -> 字母
    letters = ""
    while index:
        index, rem = divmod(index - 1, 26)
        letters = chr(65 + rem) + letters
    return letters


checks = {
    "成本法": "移动加权平均",
    "分类": "原料,半成品,成品,服务",
    "状态": "启用,停用",
}
for name, expected in checks.items():
    idx = clean.index(name) + 1
    letter = col_letter(idx)
    dv = by_col.get(letter)
    assert dv is not None, f"{name}（{letter} 列）没有数据验证"
    assert dv.type == "list", f"{name} 的验证类型是 {dv.type}，应为 list"
    got = (dv.formula1 or "").strip('"')
    assert got == expected, f"{name} 下拉项 {got!r} != {expected!r}"
    print(f"OK 下拉 {name}（{letter} 列）→ {got}  sqref={dv.sqref}")

# 数值范围列（安全库存）
letter = col_letter(clean.index("安全库存") + 1)
dv = by_col.get(letter)
assert dv is not None, "安全库存没有范围验证"
print(f"OK 范围 安全库存（{letter} 列）→ type={dv.type} operator={dv.operator} formula1={dv.formula1}")

# 文本列不该有验证
text_letter = col_letter(clean.index("物料编码") + 1)
assert text_letter not in by_col, "物料编码列不该有数据验证"
print(f"OK 文本列 {text_letter}（物料编码）没有验证")

# 验证区必须从第 3 行起
first_dv = by_col[col_letter(clean.index("分类") + 1)]
assert str(first_dv.sqref).startswith("E3:"), f"验证区应从第 3 行起，实际 {first_dv.sqref}"
print("OK 验证区从第 3 行起（表头 + 示例行不受限）")

# 3) 填一行合法数据后真的上传（编码带时间戳，便于反复跑）
import time

code = "IMP-DV-" + time.strftime("%H%M%S")
ws.delete_rows(2, ws.max_row)
row = {
    "物料编码": code,
    "物料名称": "下拉验证行",
    "规格型号": "M6x20",
    "基本单位": "BAG",
    "分类": "原料",
    "成本法": "移动加权平均",
    "安全库存": 12.5,
    "状态": "启用",
}
ws.append([row.get(h) for h in clean])
good = os.path.join(OUT, "good_dropdown.xlsx")
wb.save(good)
print("GOOD FILE =", good, os.path.getsize(good), "bytes")

boundary = "----dvboundary"
payload = (
    f"--{boundary}\r\n"
    f'Content-Disposition: form-data; name="file"; filename="物料导入模板_v1.xlsx"\r\n'
    "Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\r\n\r\n"
).encode() + open(good, "rb").read() + f"\r\n--{boundary}--\r\n".encode()
status, raw = req(
    f"/api/business/impexp/import/upload?biz_type={BIZ}&mode=STRICT&duplicate=SKIP",
    payload,
    {**auth, "Content-Type": f"multipart/form-data; boundary={boundary}"},
    "POST",
)
res = json.loads(raw)
print("UPLOAD", status, json.dumps(res, ensure_ascii=False)[:600])
assert status == 200 and res.get("code") == 0, "上传预检失败"
task = res["data"]
assert task["report"]["error"] == 0, f"预检应 0 错误，实际 {task['report']}"
print("PRECHECK total=%s valid=%s error=%s" % (
    task["report"]["total"], task["report"]["valid"], task["report"]["error"]))

status, raw = req(f"/api/business/impexp/import/{task['task_id']}/confirm", b"", auth, "POST")
res = json.loads(raw)
print("CONFIRM", status, json.dumps(res, ensure_ascii=False)[:400])
assert status == 200 and res.get("code") == 0, "确认入库失败"
assert res["data"].get("imported_rows") == 1, f"应入库 1 行，实际 {res['data']}"
print("RESULT task_no=%s status=%s imported_rows=%s" % (
    task["task_no"], res["data"].get("status"), res["data"].get("imported_rows")))
print("ALL CHECKS PASSED")
