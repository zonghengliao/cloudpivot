"""从 Noto Sans SC 可变字体实例化出静态 Regular / Bold（glyf 轮廓）。

用法（由 scripts/pdf/install-pdf-infra.sh 调用）：

    python3 instance_noto_sc.py /tmp/NotoSansSC-VF.ttf /usr/share/fonts/truetype/noto-sc

为什么要实例化（PDF-01 实测结论）：
  · fonts-noto-cjk 装的是 NotoSansCJK-*.ttc（字体集合 + CFF 轮廓）。Chromium 的
    Skia PDF 后端无法子集嵌入它，每个字形退化成独立的 Type3 字体对象：20 行对账单
    单页 311KB。
  · 直接把可变字体（VF）装进去也不行——Skia 对 VF 同样走 Type3：13 行的小票 38KB。
  · 用本脚本实例化出的**静态 glyf TTF** 才会被嵌成 CID TrueType 子集：同一份
    对账单 13KB（24 倍差距），500 行 14 页共 1.27MB（约 90KB/页）。
"""

import sys

from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

# 每个字重 → 输出文件名
INSTANCES = {
    400: "NotoSansSC-Regular.ttf",
    700: "NotoSansSC-Bold.ttf",
}


def main() -> int:
    if len(sys.argv) < 3:
        print("用法: instance_noto_sc.py <variable-font.ttf> <output-dir>", file=sys.stderr)
        return 2
    src, out_dir = sys.argv[1], sys.argv[2]

    for weight, filename in INSTANCES.items():
        font = TTFont(src)
        if "fvar" not in font:
            print(f"{src} 不是可变字体（缺 fvar 表），拒绝继续", file=sys.stderr)
            return 1
        instance = instantiateVariableFont(
            font, {"wght": weight}, inplace=True, updateFontNames=True
        )
        target = f"{out_dir}/{filename}"
        instance.save(target)

        # 自检：必须是 glyf 轮廓，否则 Chromium 还会退回 Type3
        check = TTFont(target)
        if "glyf" not in check:
            print(f"{target} 不是 glyf 轮廓（CFF），PDF 会退化成 Type3", file=sys.stderr)
            return 1
        print(
            f"{target}: family={check['name'].getDebugName(1)!r} "
            f"subfamily={check['name'].getDebugName(2)!r} fsType={check['OS/2'].fsType}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
