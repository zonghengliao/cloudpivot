"""极简 xlsx 生成器（纯标准库，无 openpyxl 依赖）。

产出最小合法 OOXML 工作簿：一个 sheet（名为「数据」），所有单元格走 sharedStrings。
用途：A3 验收要造 5000 行 / 10 万行的真实 xlsx 测试文件。
"""

import sys
import zipfile
from xml.sax.saxutils import escape

CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="数据" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""

WORKBOOK_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"""


def column_name(index: int) -> str:
    name = ""
    index += 1
    while index:
        index, rem = divmod(index - 1, 26)
        name = chr(ord("A") + rem) + name
    return name


def build(path: str, headers: list[str], rows: list[list[str]]) -> None:
    strings: list[str] = []
    index_of: dict[str, int] = {}

    def sid(value: str) -> int:
        if value not in index_of:
            index_of[value] = len(strings)
            strings.append(value)
        return index_of[value]

    row_xml = []
    cells = "".join(
        f'<c r="{column_name(c)}1" t="s"><v>{sid(h)}</v></c>' for c, h in enumerate(headers)
    )
    row_xml.append(f'<row r="1">{cells}</row>')
    for r, values in enumerate(rows, start=2):
        cells = "".join(
            f'<c r="{column_name(c)}{r}" t="s"><v>{sid(v)}</v></c>'
            for c, v in enumerate(values)
            if v != ""
        )
        row_xml.append(f'<row r="{r}">{cells}</row>')

    sheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<sheetData>{"".join(row_xml)}</sheetData></worksheet>'
    )
    sst_items = "".join(f'<si><t xml:space="preserve">{escape(s)}</t></si>' for s in strings)
    sst = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        f'<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'count="{len(strings)}" uniqueCount="{len(strings)}">{sst_items}</sst>'
    )
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", CONTENT_TYPES)
        zf.writestr("_rels/.rels", ROOT_RELS)
        zf.writestr("xl/workbook.xml", WORKBOOK)
        zf.writestr("xl/_rels/workbook.xml.rels", WORKBOOK_RELS)
        zf.writestr("xl/worksheets/sheet1.xml", sheet)
        zf.writestr("xl/sharedStrings.xml", sst)


HEADERS = ["字典编码*", "字典项值*", "字典项名称*", "描述", "排序号", "是否默认", "状态"]


def gen_bad(path: str, total: int) -> None:
    rows = []
    for i in range(1, total + 1):
        rows.append(
            ["user_status", f"V{i:05d}", f"标签{i}", "", str(i % 100), "否", "启用"]
        )
    # 3 行必填缺失（字典项名称为空）→ 3 条 REQUIRED
    for offset in (10, 20, 30):
        rows[offset][2] = ""
    # 2 组编码重复（各重复一次）→ 2 条 UNIQUE
    rows[41][1] = rows[40][1]
    rows[43][1] = rows[42][1]
    build(path, HEADERS, rows)
    print(f"{path}: {len(rows)} 行（3 必填缺失 + 2 行编码重复）")


def gen_perf(path: str, total: int) -> None:
    rows = []
    for i in range(1, total + 1):
        rows.append(
            ["user_status", f"P{i:06d}", f"性能标签{i}", f"备注{i}", str(i % 1000), "否", "启用"]
        )
    build(path, HEADERS, rows)
    print(f"{path}: {len(rows)} 行")


if __name__ == "__main__":
    kind, path, total = sys.argv[1], sys.argv[2], int(sys.argv[3])
    (gen_bad if kind == "bad" else gen_perf)(path, total)
