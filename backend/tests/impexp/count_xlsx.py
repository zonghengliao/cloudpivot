#!/usr/bin/env python3
"""统计 xlsx 的行数与表头（stdlib 实现：zipfile + 正则读 sheet XML）。

用法：python3 count_xlsx.py <file.xlsx>
输出： JSON {rows, header, first_data}
"""
import json
import re
import sys
import zipfile


def cells_of(row_xml: str, shared):
    out = []
    for attrs, body in re.findall(r"<c\b([^>]*)>(.*?)</c>", row_xml, re.S):
        value = ""
        inline = re.search(r"<is>.*?<t[^>]*>(.*?)</t>", body, re.S)
        if inline:
            value = inline.group(1)
        elif 't="s"' in attrs:
            match = re.search(r"<v>(\d+)</v>", body)
            if match and int(match.group(1)) < len(shared):
                value = shared[int(match.group(1))]
        else:
            match = re.search(r"<v>(.*?)</v>", body, re.S)
            if match:
                value = match.group(1)
        ref = re.search(r'r="([A-Z]+\d+)"', attrs)
        out.append({"cell": ref.group(1) if ref else "", "value": value})
    return out


def main(path: str) -> int:
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        shared = []
        if "xl/sharedStrings.xml" in names:
            raw = archive.read("xl/sharedStrings.xml").decode("utf-8")
            shared = [
                "".join(re.findall(r"<t[^>]*>(.*?)</t>", item, re.S))
                for item in re.findall(r"<si>(.*?)</si>", raw, re.S)
            ]
        sheet = archive.read("xl/worksheets/sheet1.xml").decode("utf-8")

    rows = re.findall(r"<row\b.*?</row>", sheet, re.S)
    report = {
        "rows": len(rows),
        "header": cells_of(rows[0], shared) if rows else [],
        "first_data": cells_of(rows[1], shared) if len(rows) > 1 else [],
        "zip_entries": len(names),
    }
    print(json.dumps(report, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else ""))
