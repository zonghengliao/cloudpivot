#!/usr/bin/env python3
"""HR-46 E2E 用：把二维表写成**真 .xlsx**（不依赖 openpyxl，纯标准库）。

为什么自己拼 xlsx 而不是 `pip install openpyxl`
----------------------------------------------
E2E 脚本要在**任何**一台测试机上一键跑通（145 / CI / 新环境），不能把
`pip install` 当成前置条件；而 impexp 的导入通道只吃 `.xlsx`
（`backend/src/business/impexp/engine.rs` 用 calamine 0.36 读整表，没有 CSV 分支），
所以这里用 `zipfile` + OOXML 手写一个最小工作簿：
`[Content_Types].xml` + `_rels/.rels` + `workbook.xml` + 一张 sheet + `sharedStrings.xml`。

覆盖范围（够用即止，不做通用库）
------------------------------
* 单元格只有两种类型：**字符串**（走 sharedStrings）与**数字**（`<v>`）；
* 单 sheet、无样式、无合并、无公式 —— 导入模板只要求「第 1 行表头 + 数据行」。

用法
----
    python3 hr_e2e_xlsx.py <输出.xlsx> <行数据.json>

`行数据.json` = `[["表头1","表头2"], ["值1", 21], ...]`（首行是表头，后续是数据行；
字符串按 UTF-8 原样写入，数字写数值单元格）。退出码：0 成功 / 2 参数或数据有误。
"""

from __future__ import annotations

import json
import sys
import zipfile
from xml.sax.saxutils import escape

# xlsx 里字符串单元格的长度上限是 32767（Excel 硬约束），越界说明数据造错了
MAX_CELL_CHARS = 32767


def col_name(index: int) -> str:
    """0 → A，25 → Z，26 → AA（列号 → Excel 列名）。"""
    name = ""
    index += 1
    while index > 0:
        index, rem = divmod(index - 1, 26)
        name = chr(ord("A") + rem) + name
    return name


def is_number(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def build_sheet(rows: list[list[object]]) -> tuple[str, list[str]]:
    """返回 (sheet1.xml, sharedStrings 列表)。"""
    shared: list[str] = []
    index_of: dict[str, int] = {}
    xml_rows = []
    for r, row in enumerate(rows, start=1):
        cells = []
        for c, value in enumerate(row):
            ref = f"{col_name(c)}{r}"
            if value is None or value == "":
                continue  # 空单元格整格不写：calamine 读到的是 None，与 Excel 空着等价
            if is_number(value):
                cells.append(f'<c r="{ref}"><v>{value}</v></c>')
                continue
            text = str(value)
            if len(text) > MAX_CELL_CHARS:
                raise ValueError(f"单元格 {ref} 超过 {MAX_CELL_CHARS} 字符")
            if text not in index_of:
                index_of[text] = len(shared)
                shared.append(text)
            cells.append(f'<c r="{ref}" t="s"><v>{index_of[text]}</v></c>')
        xml_rows.append(f'<row r="{r}">{"".join(cells)}</row>')
    sheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<sheetData>{"".join(xml_rows)}</sheetData>'
        "</worksheet>"
    )
    return sheet, shared


def build_shared_strings(items: list[str]) -> str:
    body = "".join(f"<si><t>{escape(text)}</t></si>" for text in items)
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'count="{len(items)}" uniqueCount="{len(items)}">{body}</sst>'
    )


CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    '<Override PartName="/xl/sharedStrings.xml" '
    'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
    "</Types>"
)

ROOT_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" '
    'Target="xl/workbook.xml"/>'
    "</Relationships>"
)

WORKBOOK = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
    'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
    '<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>'
    "</workbook>"
)

WORKBOOK_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" '
    'Target="worksheets/sheet1.xml"/>'
    '<Relationship Id="rId2" '
    'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" '
    'Target="sharedStrings.xml"/>'
    "</Relationships>"
)


def write_xlsx(path: str, rows: list[list[object]]) -> None:
    if not rows:
        raise ValueError("至少要有表头行")
    sheet, shared = build_sheet(rows)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", CONTENT_TYPES)
        zf.writestr("_rels/.rels", ROOT_RELS)
        zf.writestr("xl/workbook.xml", WORKBOOK)
        zf.writestr("xl/_rels/workbook.xml.rels", WORKBOOK_RELS)
        zf.writestr("xl/worksheets/sheet1.xml", sheet)
        zf.writestr("xl/sharedStrings.xml", build_shared_strings(shared))


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("用法：python3 hr_e2e_xlsx.py <输出.xlsx> <行数据.json>", file=sys.stderr)
        return 2
    out, source = argv[1], argv[2]
    try:
        with open(source, encoding="utf-8") as handle:
            rows = json.load(handle)
        if not isinstance(rows, list) or not all(isinstance(row, list) for row in rows):
            raise ValueError("行数据必须是二维数组")
        write_xlsx(out, rows)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"生成 xlsx 失败：{error}", file=sys.stderr)
        return 2
    print(f"{out}（{len(rows) - 1} 行数据 + 1 行表头）")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
