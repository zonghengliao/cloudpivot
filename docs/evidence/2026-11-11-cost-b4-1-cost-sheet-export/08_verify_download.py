#!/usr/bin/env python3
"""浏览器真实下载的导出件 vs 页面数据源（详情接口）—— 逐格校验（2 对象小样本）。

    python3 08_verify_download.py 12_browser_download.xlsx /tmp/b41/detail_small.json

与 `03_verify_xlsx.py` 的区别：
- 03 校验的是 **curl 直接打的导出接口**（1000 行性能用例，分摊列硬编码 3000 的那份造数）；
- 本脚本校验的是 **浏览器点「导出 xlsx」真实落盘的那个文件**（`01b_seed_small.sql` 的两对象样本），
  用来证明「前端下载链路拿到的字节 = 页面看到的数据」，而不仅仅是接口能返回。
解析/格式化函数直接复用 03（同一个解析器，避免两套读取逻辑互相掩护）。
"""
import importlib.util
import json
import sys
from decimal import Decimal

SRC = __file__.replace("08_verify_download.py", "03_verify_xlsx.py")
spec = importlib.util.spec_from_file_location("verify_xlsx", SRC)
assert spec is not None and spec.loader is not None, "找不到 03_verify_xlsx.py"
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)

# 01b_seed_small.sql 的造数：待摊 6.0000 / 基数 10+10 ⇒ 每个对象摊 3.0000
PER_OBJECT_ALLOC = "3.0000"


def main() -> int:
    xlsx_path, detail_path = sys.argv[1], sys.argv[2]
    detail = json.load(open(detail_path, encoding="utf-8"))["data"]
    items = detail["items"]
    summary = detail["items_summary"]
    sheets = verify.read_xlsx(xlsx_path)
    problems = []

    def check(condition, message):
        if not condition:
            problems.append(message)

    print(f"  [i] sheet：{[name for name, _ in sheets]}")
    check([name for name, _ in sheets] == ["成本计算单", "分摊明细"], f"sheet 名：{[n for n, _ in sheets]}")

    main_rows = sheets[0][1]
    check(len(main_rows) == len(items) + 3, f"主表行数 {len(main_rows)} ≠ {len(items) + 3}")
    headers = [main_rows[0].get(index, "") for index in range(1, 21)]
    check(headers == verify.MAIN_HEADERS, f"表头 20 列不一致：{headers}")

    mismatched = 0
    for index, item in enumerate(items):
        row = main_rows[index + 2]
        expected = {
            1: item["object_no"],
            3: "已计算" if item["status"] == "CALC" else "已结算",
            6: verify.money(item["mat_cur"]),
            7: verify.money(item["labor_cur"]),
            8: verify.money(item["oh_cur"]),
            9: verify.money(PER_OBJECT_ALLOC),
            10: verify.qty(item["equiv_material"]),
            11: verify.qty(item["equiv_conversion"]),
            12: verify.unit(item["unit_material"]),
            13: verify.unit(item["unit_labor"]),
            14: verify.unit(item["unit_overhead"]),
            15: verify.qty(item["completed_qty"]),
            16: verify.money(item["finished_cost"]),
            17: verify.qty(item["wip_end_qty"]),
            18: f"{verify.qty(item['completion_pct'])}%",
            19: verify.money(item["wip_end_cost"]),
            20: verify.unit(item["unit_finished_cost"]),
        }
        for column, want in expected.items():
            got = row.get(column)
            if got != want:
                mismatched += 1
                problems.append(f"第 {index + 3} 行第 {column} 列（{verify.MAIN_HEADERS[column - 1]}）导出「{got}」≠ 详情「{want}」")
    check(mismatched == 0, f"逐行比对 {mismatched} 处不一致")

    total = main_rows[-1]
    check(total.get(1) == f"合计（{summary['object_count']} 个对象）", f"合计行对象数：{total.get(1)}")
    for column, key in ((6, "total_material"), (7, "total_labor"), (8, "total_overhead"),
                        (16, "total_finished"), (19, "total_wip")):
        want = verify.money(summary[key])
        check(total.get(column) == want, f"合计行第 {column} 列（{verify.MAIN_HEADERS[column - 1]}）「{total.get(column)}」≠ 「{want}」")
    check(total.get(9) == verify.money(Decimal(PER_OBJECT_ALLOC) * len(items)), f"合计行分摊：{total.get(9)}")
    check(str(total.get(3, "")).endswith("✓"), f"合计行守恒式没打勾：{total.get(3)}")

    if problems:
        print(f"  [FAIL] {len(problems)} 条问题：")
        for problem in problems[:20]:
            print(f"         - {problem}")
        return 1
    print(f"  [OK] 浏览器下载件：20 列表头逐字一致；{len(items)} 行逐格等于页面数据源；"
          f"合计 {verify.money(summary['total_cost'])} = 完工 {verify.money(summary['total_finished'])}"
          f" + 在制 {verify.money(summary['total_wip'])} ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())
