"""把 xlsx 的「前几行」按单元格坐标打印出来（纯标准库）。

用途：A3 验收 1（模板三要素）/ 导出结果的肉眼核对，不依赖 openpyxl。
"""

import re
import sys
import zipfile


def column_name(index: int) -> str:
    name = ""
    index += 1
    while index:
        index, rem = divmod(index - 1, 26)
        name = chr(ord("A") + rem) + name
    return name


def load(path: str, limit: int = 6) -> None:
    with zipfile.ZipFile(path) as zf:
        names = zf.namelist()
        shared = []
        if "xl/sharedStrings.xml" in names:
            xml = zf.read("xl/sharedStrings.xml").decode("utf-8")
            for si in re.findall(r"<si>(.*?)</si>", xml, re.S):
                text = "".join(re.findall(r"<t[^>]*>(.*?)</t>", si, re.S))
                shared.append(
                    text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
                )
        print(f"-- 文件: {path}")
        print(f"-- 压缩包内条目: {[n for n in names if n.endswith('.xml')]}")
        print(f"-- sharedStrings 条数: {len(shared)}")
        sheets = sorted(n for n in names if n.startswith("xl/worksheets/sheet"))
        sheet_names = re.findall(
            r'<sheet name="([^"]+)"', zf.read("xl/workbook.xml").decode("utf-8")
        )
        print(f"-- 工作表: {sheet_names}")
        for position, sheet in enumerate(sheets):
            xml = zf.read(sheet).decode("utf-8")
            print(f"\n== {sheet_names[position] if position < len(sheet_names) else sheet} ==")
            rows = re.findall(r"<row[^>]*>(.*?)</row>", xml, re.S)
            for row in rows[:limit]:
                cells = []
                for ref, attrs, body in re.findall(
                    r'<c r="([A-Z]+\d+)"([^>]*)>(.*?)</c>', row, re.S
                ):
                    value = re.search(r"<v>(.*?)</v>", body, re.S)
                    if value is None:
                        inline = re.search(r"<t[^>]*>(.*?)</t>", body, re.S)
                        text = inline.group(1) if inline else ""
                    elif 't="s"' in attrs:
                        text = shared[int(value.group(1))]
                    else:
                        text = value.group(1)
                    cells.append(f"{ref}={text!r}")
                if cells:
                    print("  " + " | ".join(cells))


if __name__ == "__main__":
    load(sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 6)
