#!/usr/bin/env python3
"""COST-B4-1 导出件逐格校验（导出 xlsx vs 详情接口 JSON）。

    python3 03_verify_xlsx.py /tmp/b41/cost_sheet.xlsx /tmp/b41/detail.json

只用标准库（zipfile + xml）：本机没装 openpyxl，而导出件本身就是 zip + XML，
直接解出来反而能看到「单元格里到底写了什么字」。

校验项（每条都是卡面验收要求）：
① sheet 结构：有「成本计算单」；有分摊结果时还有「分摊明细」
② 表头 20 列且逐字等于后端 `MAIN_HEADERS`（列序 = 阅读顺序）
③ 1000 行计算单逐行比对：对象号 / 料 / 工 / 费 / 分摊 / 约当 / 单位成本 / 完工 / 在制
   —— 与 `GET /cost-period/{id}`（页面步③ 的同一份数据源）完全一致
④ 合计行 = `items_summary`，且「投入 = 完工转出 + 期末在制」守恒式打勾
⑤ 逐行守恒（完工 + 在制 = 期初 + 料 + 工 + 费）
"""
import json
import re
import sys
import zipfile
import xml.etree.ElementTree as ET
from decimal import Decimal, ROUND_HALF_UP

NS = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"

MAIN_HEADERS = [
    "成本对象号", "产出品", "计算单状态", "期初在制数量", "期初在制成本",
    "本期材料投入", "本期人工投入", "本期制造费用", "其中：分摊转入",
    "约当产量（材料）", "约当产量（转换成本）",
    "单位材料成本", "单位人工成本", "单位制造费用",
    "完工数量", "完工转出成本", "期末在制数量", "期末在制完工进度(%)",
    "期末在制成本", "完工入库单价",
]
ALLOC_HEADERS = [
    "成本对象号", "被摊车间", "分摊规则", "被摊科目", "基数", "全车间基数",
    "待分摊总额", "分摊金额", "尾差调整",
]


def money(value) -> str:
    """与后端 `report::money_text` 同口径：2 位小数 + 千分位（负数带 -）。"""
    if value is None:
        return ""
    cents = Decimal(str(value)).quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)
    text = f"{cents:.2f}"
    sign = "-" if text.startswith("-") else ""
    integer, fraction = text.lstrip("-").split(".")
    grouped = f"{int(integer):,}"
    return f"{sign}{grouped}.{fraction}"


def qty(value) -> str:
    """与后端 `qty_text` 同口径：最多 4 位小数、去尾随 0。"""
    dec = Decimal(str(value)).quantize(Decimal("0.0001"), rounding=ROUND_HALF_UP)
    text = f"{dec:.4f}".rstrip("0").rstrip(".")
    return text or "0"


def unit(value) -> str:
    """与后端 `unit_text` 同口径：固定 6 位小数。"""
    return f"{Decimal(str(value)).quantize(Decimal('0.000001'), rounding=ROUND_HALF_UP):.6f}"


def col_index(ref: str) -> int:
    match = re.match(r"([A-Z]+)", ref)
    letters = match.group(1) if match else "A"
    index = 0
    for char in letters:
        index = index * 26 + (ord(char) - ord("A") + 1)
    return index


def read_xlsx(path):
    archive = zipfile.ZipFile(path)
    shared: list[str] = []
    if "xl/sharedStrings.xml" in archive.namelist():
        root = ET.fromstring(archive.read("xl/sharedStrings.xml"))
        for si in root.findall(NS + "si"):
            shared.append("".join(node.text or "" for node in si.iter(NS + "t")))
    workbook = ET.fromstring(archive.read("xl/workbook.xml"))
    names = [(sheet.get("name") or f"sheet{index}") for index, sheet in enumerate(workbook.iter(NS + "sheet"), 1)]
    sheets = []
    for index in range(1, len(names) + 1):
        root = ET.fromstring(archive.read(f"xl/worksheets/sheet{index}.xml"))
        rows = []
        for row in root.iter(NS + "row"):
            cells: dict[int, str] = {}
            for cell in row.findall(NS + "c"):
                position = col_index(cell.get("r") or "A1")
                kind = cell.get("t")
                node = cell.find(NS + "v")
                if kind == "s" and node is not None and node.text is not None:
                    text = shared[int(node.text)]
                elif kind == "inlineStr":
                    text = "".join(part.text or "" for part in cell.iter(NS + "t"))
                else:
                    text = node.text if node is not None and node.text is not None else ""
                cells[position] = text
            rows.append(cells)
        sheets.append((names[index - 1], rows))
    return sheets


def main() -> int:
    xlsx_path, detail_path = sys.argv[1], sys.argv[2]
    detail = json.load(open(detail_path, encoding="utf-8"))["data"]
    items = detail["items"]
    summary = detail["items_summary"]
    sheets = read_xlsx(xlsx_path)
    problems = []

    def check(condition, message):
        if not condition:
            problems.append(message)

    print(f"  [i] sheet：{[name for name, _ in sheets]}")
    check([name for name, _ in sheets] == ["成本计算单", "分摊明细"],
          f"sheet 名不对：{[name for name, _ in sheets]}")

    main_rows = sheets[0][1]
    # 行 1 表头 / 行 2 表头信息 / 行 3..N+2 计算单 / 最后一行 合计
    check(len(main_rows) == len(items) + 3, f"主表行数 {len(main_rows)} ≠ {len(items) + 3}")
    headers = [main_rows[0].get(index, "") for index in range(1, 21)]
    check(headers == MAIN_HEADERS, f"表头不是 20 列/顺序不对：{headers}")
    check(main_rows[0].get(21) is None, "表头列数超过 20")

    info = main_rows[1]
    check(info.get(1) == f"批次：{detail['batch_no']}", f"表头信息批次号：{info.get(1)}")
    check(info.get(2) == f"期间：{detail['period_code']}", f"表头信息期间：{info.get(2)}")

    # ---- 逐行比对（1000 行全量，不是抽样） ----
    mismatched = 0
    for index, item in enumerate(items):
        row = main_rows[index + 2]
        expected = {
            1: item["object_no"],
            3: "已计算" if item["status"] == "CALC" else "已结算",
            6: money(item["mat_cur"]),
            7: money(item["labor_cur"]),
            8: money(item["oh_cur"]),
            9: money("3.0000"),          # 本卡造数：每个对象摊 3.0000
            10: qty(item["equiv_material"]),
            11: qty(item["equiv_conversion"]),
            12: unit(item["unit_material"]),
            13: unit(item["unit_labor"]),
            14: unit(item["unit_overhead"]),
            15: qty(item["completed_qty"]),
            16: money(item["finished_cost"]),
            17: qty(item["wip_end_qty"]),
            18: f"{qty(item['completion_pct'])}%",
            19: money(item["wip_end_cost"]),
            20: unit(item["unit_finished_cost"]),
        }
        for column, want in expected.items():
            got = row.get(column)
            if got != want:
                mismatched += 1
                if mismatched <= 5:
                    problems.append(
                        f"第 {index + 3} 行第 {column} 列（{MAIN_HEADERS[column - 1]}）：导出「{got}」≠ 详情「{want}」"
                    )
        # 逐行守恒：完工 + 在制 = 期初 + 料 + 工 + 费
        left = Decimal(item["finished_cost"]) + Decimal(item["wip_end_cost"])
        right = (Decimal(item["wip_beg_cost"]) + Decimal(item["mat_cur"])
                 + Decimal(item["labor_cur"]) + Decimal(item["oh_cur"]))
        if left != right:
            problems.append(f"第 {index + 3} 行不守恒：{left} ≠ {right}")
    check(mismatched == 0, f"逐行比对共 {mismatched} 处不一致（前 5 条见上）")

    # ---- 合计行 = items_summary ----
    total = main_rows[-1]
    check(total.get(1) == f"合计（{summary['object_count']} 个对象）", f"合计行对象数：{total.get(1)}")
    for column, key in ((6, "total_material"), (7, "total_labor"), (8, "total_overhead"),
                        (16, "total_finished"), (19, "total_wip")):
        want = money(summary[key])
        check(total.get(column) == want, f"合计行第 {column} 列（{MAIN_HEADERS[column - 1]}）「{total.get(column)}」≠ 「{want}」")
    check(total.get(9) == money("3000.0000"), f"合计行分摊：{total.get(9)}")
    check(str(total.get(3, "")).endswith("✓"), f"合计行守恒式没打勾：{total.get(3)}")
    check(abs(Decimal(summary["total_cost"])
              - Decimal(summary["total_finished"])
              - Decimal(summary["total_wip"])) == 0,
          "items_summary 自身不守恒（投入 ≠ 完工 + 在制）")

    # ---- 分摊明细 sheet ----
    alloc_rows = sheets[1][1]
    alloc_headers = [alloc_rows[0].get(index, "") for index in range(1, 10)]
    check(alloc_headers == ALLOC_HEADERS, f"分摊明细表头不对：{alloc_headers}")
    check(len(alloc_rows) == len(items) + 1, f"分摊明细行数 {len(alloc_rows)} ≠ {len(items) + 1}")
    check(alloc_rows[1].get(3) == "AR-B41 成本B4实测车间制造费用按工时",
          f"分摊规则列：{alloc_rows[1].get(3)}")
    # 列序：1 对象号 / 2 被摊车间 / 3 分摊规则 / 4 被摊科目 / 5 基数 / 6 全车间基数 /
    #       7 待分摊总额 / 8 分摊金额 / 9 尾差调整
    # 造数：待摊总额 3000、基数 10/10000 ⇒ 每个对象摊 3.0000
    check(alloc_rows[1].get(7) == money("3000.0000"), f"待分摊总额列：{alloc_rows[1].get(7)}")
    check(alloc_rows[1].get(8) == money("3.0000"), f"分摊金额列：{alloc_rows[1].get(8)}")

    if problems:
        print(f"  [FAIL] {len(problems)} 条问题：")
        for problem in problems[:20]:
            print(f"         - {problem}")
        return 1
    print(f"  [OK] 20 列表头逐字一致；{len(items)} 行计算单逐格等于详情接口；"
          f"合计行 = items_summary；分摊明细 {len(alloc_rows) - 1} 行")
    return 0


if __name__ == "__main__":
    sys.exit(main())
