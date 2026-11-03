#!/usr/bin/env python3
"""P1-6 主数据导入验收用：把数据填进**真实模板**（表头从第 1 行读，必填列带 `*`）。

用法：
    python p16_fill_template.py <模板.xlsx> <输出.xlsx> <supplier3|supplier_bad|material100>

为什么要基于模板填：表头顺序 / 必填星号 / sheet 结构都由后端 `template.rs` 决定，
用模板本身当底稿，等于顺手验证了「下载的模板能被自己导入」。
"""

from __future__ import annotations

import sys

from openpyxl import load_workbook

SUPPLIERS = [
    ["P16MD-SUP1", "北京甲供应链有限公司", "甲供应链", "张三", "13800000001", "91110108MA01ABCD12", "100000", "启用"],
    ["P16MD-SUP2", "上海乙贸易有限公司", "", "李四", "13900000002", "", "50000", "启用"],
    ["P16MD-SUP3", "广州丙实业有限公司", "", "", "", "", "0", "停用"],
]

SUPPLIERS_BAD = [
    ["P16MD-BAD1", "正常的一行", "", "", "", "", "", "启用"],
    ["", "这一行故意缺供应商编码", "", "", "", "", "", "启用"],
    ["P16MD-BAD3", "信用额度写成负数", "", "", "", "", "-100", "启用"],
]

# ---------- 另外 4 个定义（列顺序与各定义 `static COLUMNS` 一致） ----------

UNITS = [
    # 编码 / 名称 / 是否基本单位 / 基准单位编码 / 换算率 / 状态
    # 辅助单位排在基础单位前面：验证「同一份文件里的基准单位」也能解析
    ["P16MD-U-BOX2", "箱（验收）", "否", "P16MD-U-PCS2", "12", "启用"],
    ["P16MD-U-PCS2", "个（验收）", "是", "", "", "启用"],
]

ACCOUNTS = [
    # 科目编码 / 名称 / 类别 / 借贷方向 / 是否末级 / 上级科目编码 / 状态
    ["P16MD-A01", "验收二级科目", "资产", "借", "是", "P16MD-A", "启用"],
    ["P16MD-A", "验收一级科目", "资产", "借", "否", "", "启用"],
]

WAREHOUSES = [
    # 编码 / 名称 / 类型 / 地址 / 负责人 / 是否允许负库存 / 状态
    ["P16MD-W1", "验收仓", "普通仓", "北京市海淀区", "王五", "否", "启用"],
    ["P16MD-W2", "验收仓二号", "原料仓", "", "", "是", ""],
]

LOCATIONS = [
    # 库位编码 / 所属仓库编码 / 名称 / 类型 / 是否默认 / 容量 / 状态
    ["P16MD-LC1", "P16MD-W1", "验收库位一", "存储", "是", "100", "启用"],
    ["P16MD-LC2", "P16MD-W1", "验收库位二", "", "否", "", "启用"],
]


def supplier_rows(n: int) -> list[list[str]]:
    rows = []
    for i in range(1, n + 1):
        rows.append(
            [
                f"P16MD-MAT{i:03d}",
                f"验收物料 {i}",
                "M8x40 不锈钢",
                "PCS",
                "原料",
                "移动加权平均",
                str(i * 10),
                "启用",
            ]
        )
    return rows


def fill(template: str, out: str, rows: list[list[str]]) -> None:
    workbook = load_workbook(template)
    sheet = workbook["数据"]
    headers = []
    for cell in sheet[1]:
        headers.append(str(cell.value or "").strip().rstrip("*").strip())
    assert len(headers) == len(rows[0]), f"模板列数 {len(headers)} 与数据列数 {len(rows[0])} 不一致：{headers}"
    # 第 2 行是模板示例行（框架会自动跳过），数据从第 3 行开始写
    for offset, row in enumerate(rows):
        for index, value in enumerate(row, start=1):
            sheet.cell(row=3 + offset, column=index, value=value)
    workbook.save(out)
    print(f"写出 {out}：{len(rows)} 行，表头 {headers}")


def main() -> int:
    if len(sys.argv) != 4:
        print(__doc__)
        return 2
    template, out, mode = sys.argv[1], sys.argv[2], sys.argv[3]
    if mode == "supplier3":
        fill(template, out, SUPPLIERS)
    elif mode == "supplier_bad":
        fill(template, out, SUPPLIERS_BAD)
    elif mode == "material100":
        fill(template, out, supplier_rows(100))
    elif mode == "unit2":
        fill(template, out, UNITS)
    elif mode == "account2":
        fill(template, out, ACCOUNTS)
    elif mode == "warehouse2":
        fill(template, out, WAREHOUSES)
    elif mode == "location2":
        fill(template, out, LOCATIONS)
    else:
        print(f"未知模式 {mode}")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
