"""在截图里对某个候选图标做「尺寸 + 偏移」搜索，找与参考图标最匹配的裁切。

用途：书签栏 / 历史记录里的图标尺寸不一定是 16x16（Chrome 会按 CSS 像素缩放），
      直接按包围盒裁会差 1~3px，导致像素差虚高。这里把尺寸和偏移都扫一遍。

用法: python3 scripts/verify/favicon_align.py <截图> <cx,cy> [--ref png] [--span 24]
"""

from __future__ import annotations

import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageStat

RES = Image.Resampling.LANCZOS


def mean_diff(a: Image.Image, b: Image.Image) -> float:
    a = a.convert("RGBA").resize(b.size, RES)
    st = ImageStat.Stat(ImageChops.difference(a, b))
    return sum(st.mean) / 4


def main() -> int:
    shot = Path(sys.argv[1])
    cx, cy = (int(v) for v in sys.argv[2].split(","))
    ref_path = Path("/opt/cloudpivot/frontend/public/favicon-16x16.png")
    span = 24
    if "--ref" in sys.argv:
        ref_path = Path(sys.argv[sys.argv.index("--ref") + 1])
    if "--span" in sys.argv:
        span = int(sys.argv[sys.argv.index("--span") + 1])

    im = Image.open(shot).convert("RGBA")
    ref = Image.open(ref_path).convert("RGBA")
    best = None
    for size in range(10, span + 1):
        for dx in range(-3, 4):
            for dy in range(-3, 4):
                box = (cx + dx - size // 2, cy + dy - size // 2, cx + dx - size // 2 + size, cy + dy - size // 2 + size)
                if box[0] < 0 or box[1] < 0:
                    continue
                d = mean_diff(im.crop(box), ref)
                if best is None or d < best[0]:
                    best = (d, size, dx, dy, box)
    assert best is not None
    d, size, dx, dy, box = best
    print(f"{shot.name}: 最匹配 size={size} offset=({dx},{dy}) box={box} 平均像素差 {d:.2f}/255")
    print("判定:", "PASS（同一图标，抗锯齿差异范围内）" if d < 40 else "FAIL")
    # 打一份最佳裁切的字符图，便于纯文本核对
    crop = im.crop(box)
    px = crop.load()
    print(f"\n最佳裁切 {size}x{size} 字符图（# 深 / + 中 / . 浅）：")
    for y in range(crop.height):
        row = []
        for x in range(crop.width):
            r, g, b, a = px[x, y]  # type: ignore[misc]
            lum = (r + g + b) // 3
            row.append("#" if lum < 110 else ("+" if lum < 200 else "."))
        print("".join(row))
    return 0 if d < 40 else 1


if __name__ == "__main__":
    raise SystemExit(main())
