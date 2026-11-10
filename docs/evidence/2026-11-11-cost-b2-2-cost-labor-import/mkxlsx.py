#!/usr/bin/env python3
"""生成最小可用 xlsx（inlineStr），用来做导入冒烟测试。

刻意用**标准库**手写 xlsx（而不是本项目自己的写库）：
这样「解析器能否读懂外来文件」才是被真正验证的，而不是自己写自己读。
"""
import sys
import zipfile

HEADERS = ["成本对象号", "日期", "员工工号", "员工姓名", "工作中心（部门）", "人工工时", "机器工时", "工资率"]


def esc(text: str) -> str:
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def col_name(index: int) -> str:
    name = ""
    index += 1
    while index:
        index, rem = divmod(index - 1, 26)
        name = chr(65 + rem) + name
    return name


def sheet_xml(rows):
    lines = []
    for r, row in enumerate(rows, start=1):
        cells = []
        for c, value in enumerate(row):
            if value is None or value == "":
                continue
            ref = f"{col_name(c)}{r}"
            cells.append(
                f'<c r="{ref}" t="inlineStr"><is><t xml:space="preserve">{esc(str(value))}</t></is></c>'
            )
        lines.append(f'<row r="{r}">{"".join(cells)}</row>')
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<sheetData>{"".join(lines)}</sheetData></worksheet>'
    )


CONTENT_TYPES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
    '<Default Extension="xml" ContentType="application/xml"/>'
    '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
    '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
    "</Types>"
)
ROOT_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
    "</Relationships>"
)
WORKBOOK = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
    'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
    '<sheets><sheet name="数据" sheetId="1" r:id="rId1"/></sheets></workbook>'
)
WORKBOOK_RELS = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
    "</Relationships>"
)


def build(path, rows):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", CONTENT_TYPES)
        zf.writestr("_rels/.rels", ROOT_RELS)
        zf.writestr("xl/workbook.xml", WORKBOOK)
        zf.writestr("xl/_rels/workbook.xml.rels", WORKBOOK_RELS)
        zf.writestr("xl/worksheets/sheet1.xml", sheet_xml([HEADERS] + rows))
    print("wrote", path)


if __name__ == "__main__":
    out = sys.argv[1]
    kind = sys.argv[2]
    object_no = sys.argv[3]
    dept = sys.argv[4]
    if kind == "clean":
        rows = [
            [object_no, "2026-11-02", "", "李四", dept, "7.5", "3", "48.5"],
            [object_no, "2026-11-03", "", "王五", dept, "8", "", "50"],
            [object_no, "2026-11-04", "", "赵六", dept, "70", "0", "33.333333"],
        ]
    elif kind == "bad":
        # 第 3 行：对象号不存在；第 4 行：工资率留空（B2-3 未上线）
        rows = [
            [object_no, "2026-11-05", "", "孙七", dept, "8", "", "50"],
            ["CB-NOT-EXIST-9", "2026-11-05", "", "周八", dept, "8", "", "50"],
            [object_no, "2026-11-06", "", "吴九", dept, "8.555", "", "50"],
            [object_no, "2026-11-07", "", "郑十", dept, "8", "", ""],
        ]
    else:
        raise SystemExit("kind must be clean|bad")
    build(out, rows)
