#!/usr/bin/env python3
"""故意造错文件上传：验证报错是否精确到行号 + 列名。

用法: uv run --with openpyxl python impexp_bad_upload.py <base> <biz_type> <outdir>
"""
import json
import os
import sys
import urllib.request
import uuid

import openpyxl

BASE = sys.argv[1]
BIZ = sys.argv[2] if len(sys.argv) > 2 else "business.erp.material"
OUT = sys.argv[3] if len(sys.argv) > 3 else "/tmp/impexp-bad"
os.makedirs(OUT, exist_ok=True)
TOKEN = open("/tmp/impexp-token.txt").read().strip()


def http(path, data=None, headers=None, method=None):
    h = {"Authorization": "Bearer " + TOKEN}
    if headers:
        h.update(headers)
    req = urllib.request.Request(BASE + path, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(req, timeout=300) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


# 1) 下载模板
status, body = http(f"/api/business/impexp/import/template?biz_type={BIZ}")
print("TEMPLATE status =", status, "bytes =", len(body))
tpl_path = os.path.join(OUT, "tpl.xlsx")
open(tpl_path, "wb").write(body)

# 2) 改「数据」sheet：删示例行，写入故意错的 5 行
wb = openpyxl.load_workbook(tpl_path)
ws = wb["数据"]
headers = [c.value for c in ws[1]]
print("HEADERS =", headers)
ws.delete_rows(2, ws.max_row)

bad_rows = [
    # 物料编码, 物料名称, 规格型号, 基本单位, 分类, 成本法, 安全库存, 状态
    ["IMP-TEST-0001", "正常行-不该报错", "M8", "PCS", "原料", "移动加权平均", "10", "启用"],
    ["IMP-TEST-0002", "", "缺名称", "PCS", "原料", "移动加权平均", "10", "启用"],
    ["IMP-TEST-0003", "错误枚举", "", "PCS", "原料", "先进先出", "10", "启用"],
    ["IMP-TEST-0004", "错误数字", "", "PCS", "原料", "移动加权平均", "abc", "启用"],
    ["IMP-TEST-" + "X" * 40, "编码超长", "", "PCS", "原料", "移动加权平均", "10", "启用"],
    ["IMP-TEST-0006", "单位不存在", "", "NOPE", "原料", "移动加权平均", "10", "启用"],
]
for r in bad_rows:
    ws.append(r)
bad_path = os.path.join(OUT, "bad.xlsx")
wb.save(bad_path)
print("BAD FILE =", bad_path, os.path.getsize(bad_path), "bytes; data rows =", len(bad_rows))

# 3) multipart 上传
boundary = "----probe" + uuid.uuid4().hex
with open(bad_path, "rb") as f:
    content = f.read()
parts = (
    f"--{boundary}\r\n"
    f'Content-Disposition: form-data; name="file"; filename="bad.xlsx"\r\n'
    f"Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\r\n\r\n"
).encode() + content + f"\r\n--{boundary}--\r\n".encode()
url = f"/api/business/impexp/import/upload?biz_type={BIZ}&mode=PARTIAL&duplicate=UPSERT"
status, body = http(url, data=parts, headers={"Content-Type": f"multipart/form-data; boundary={boundary}"}, method="POST")
print("UPLOAD status =", status)
try:
    payload = json.loads(body)
except Exception:
    print("RAW:", body[:800])
    sys.exit(1)
json.dump(payload, open(os.path.join(OUT, "upload_res.json"), "w"), ensure_ascii=False, indent=2)
print("code =", payload.get("code"), "msg =", payload.get("msg"))
data = payload.get("data") or {}
print("task_id =", data.get("task_id"), "task_no =", data.get("task_no"), "status =", data.get("status"))
rep = data.get("report") or {}
print("REPORT total=%s valid=%s error=%s skipped_example=%s skipped_blank=%s truncated=%s" % (
    rep.get("total"), rep.get("valid"), rep.get("error"), rep.get("skipped_example"), rep.get("skipped_blank"), rep.get("truncated")))
print("WARNINGS:", json.dumps(rep.get("warnings"), ensure_ascii=False))
print("ERRORS:")
for e in rep.get("errors") or []:
    print("  行%-3s 列=%-10s 值=%-16s 码=%-8s 原因=%s" % (
        e.get("row_no"), e.get("column_header") or e.get("column_key") or "（整行）",
        (e.get("raw_value") or "")[:16], e.get("kind") or e.get("error_code"), e.get("message") or e.get("error_message")))
