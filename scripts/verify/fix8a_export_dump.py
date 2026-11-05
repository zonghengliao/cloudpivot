#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把导出的 xlsx 拆开看一眼：是不是真的有「供应商汇总 + 合计行」，不是空文件。

用法：python3 scripts/verify/fix8a_export_dump.py <xlsx> [out.txt]
（不依赖 openpyxl：xlsx 是 zip，直接读 xl/workbook.xml 与 xl/worksheets/sheet1.xml）
"""
import re
import sys
import zipfile

path = sys.argv[1]
out = sys.argv[2] if len(sys.argv) > 2 else None

with zipfile.ZipFile(path) as zf:
    names = zf.namelist()
    workbook = zf.read("xl/workbook.xml").decode("utf-8", "ignore")
    shared = []
    if "xl/sharedStrings.xml" in names:
        raw = zf.read("xl/sharedStrings.xml").decode("utf-8", "ignore")
        shared = [re.sub(r"<[^>]+>", "", item) for item in re.findall(r"<si>(.*?)</si>", raw, re.S)]
    sheet = zf.read("xl/worksheets/sheet1.xml").decode("utf-8", "ignore")

lines = []
lines.append(f"文件：{path}")
lines.append(f"zip 条目：{len(names)} 个 → {', '.join(names[:6])}")
lines.append("工作表：" + ", ".join(re.findall(r'<sheet name="([^"]+)"', workbook)))
rows = re.findall(r"<row[^>]*>(.*?)</row>", sheet, re.S)
lines.append(f"数据行数（含表头）：{len(rows)}")
for index, row in enumerate(rows):
    cells = []
    for attrs, body in re.findall(r"<c([^>]*)>(.*?)</c>", row, re.S):
        kind = "s" if 't="s"' in attrs else ("inlineStr" if 't="inlineStr"' in attrs else "")
        value = re.search(r"<v>(.*?)</v>", body, re.S)
        value = value.group(1) if value else ""
        if value == "":
            inline = re.search(r"<t[^>]*>(.*?)</t>", body, re.S)
            value = inline.group(1) if inline else ""
        if value == "":
            continue
        if kind == "s" and value.isdigit() and int(value) < len(shared):
            cells.append(shared[int(value)])
        else:
            cells.append(value)
    lines.append(f"  第{index + 1}行：" + " | ".join(cells))

text = "\n".join(lines)
print(text)
if out:
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(text + "\n")
