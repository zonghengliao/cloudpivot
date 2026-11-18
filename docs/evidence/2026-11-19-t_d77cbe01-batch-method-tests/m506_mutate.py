#!/usr/bin/env python3
"""M5-06 反证用的变异器：只在两种形态之间切换「未归集领料」判据。

mutate  : 明细行反查单头 → 单头 id 直比（错误形状，期望用例变红）
restore : 单头 id 直比 → 明细行反查单头（正确形状）
"""
import sys

FIXED = """                AND NOT EXISTS (SELECT 1 FROM biz_cost_collect cc \\
                                  JOIN biz_stock_out_item i ON i.id = cc.source_id \\
                                 WHERE cc.source_type = 'STOCK_OUT' AND i.stock_out_id = so.id \\
                                   AND cc.element = 'MATERIAL' AND cc.deleted_at IS NULL) \\
"""

BROKEN = """                AND NOT EXISTS (SELECT 1 FROM biz_cost_collect cc \\
                                 WHERE cc.source_type = 'STOCK_OUT' AND cc.source_id = so.id \\
                                   AND cc.element = 'MATERIAL' AND cc.deleted_at IS NULL) \\
"""


def main() -> int:
    mode, path = sys.argv[1], sys.argv[2]
    text = open(path, encoding="utf-8").read()
    if mode == "mutate":
        if text.count(FIXED) != 1:
            print(f"变异失败：正确形状出现 {text.count(FIXED)} 次（应为 1）")
            return 1
        text = text.replace(FIXED, BROKEN)
    elif mode == "restore":
        if text.count(BROKEN) != 1:
            print(f"还原失败：错误形状出现 {text.count(BROKEN)} 次（应为 1）")
            return 1
        text = text.replace(BROKEN, FIXED)
    else:
        print(f"未知模式 {mode}")
        return 1
    open(path, "w", encoding="utf-8").write(text)
    print(f"{mode} done")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
