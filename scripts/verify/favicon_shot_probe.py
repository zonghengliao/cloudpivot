"""把窗口截图里的「标签页 / 历史记录 / 书签栏」小图标抠出来，和生成的 favicon 做像素比对。

本会话没有视觉配额（看不到图），所以用数据说话：
  1. 在指定区域做「品牌色连通域」标注（#51697C ± 容差），列出每个色块的位置与尺寸；
  2. 取尺寸最接近 16x16 的那块，缩放到 16x16 与 frontend/public/favicon-16x16.png 逐像素比；
  3. 打一份 ASCII 灰度图，纯文本也能看出是不是同一朵云涡。

用法:
  python3 scripts/verify/favicon_shot_probe.py <截图> [--region x1,y1,x2,y2] [--ref png] [--want 16]
"""

from __future__ import annotations

import sys
from collections import deque
from pathlib import Path

from PIL import Image, ImageChops, ImageStat

BRAND = (81, 105, 124)  # #51697C
TOL = 30
RESAMPLING = Image.Resampling.LANCZOS


def brand_mask(im: Image.Image, region: tuple[int, int, int, int]) -> list[list[bool]]:
    crop = im.convert("RGB").crop(region)
    px = crop.load()
    mask = [[False] * crop.width for _ in range(crop.height)]
    for y in range(crop.height):
        for x in range(crop.width):
            r, g, b = px[x, y]  # type: ignore[misc]
            if abs(r - BRAND[0]) <= TOL and abs(g - BRAND[1]) <= TOL and abs(b - BRAND[2]) <= TOL:
                mask[y][x] = True
    return mask


def blobs(mask: list[list[bool]], region: tuple[int, int, int, int]) -> list[dict]:
    """4-邻接连通域，返回 bbox/像素数（坐标已换回整图坐标系）。"""
    h = len(mask)
    w = len(mask[0]) if h else 0
    seen = [[False] * w for _ in range(h)]
    out = []
    for y in range(h):
        for x in range(w):
            if not mask[y][x] or seen[y][x]:
                continue
            q = deque([(x, y)])
            seen[y][x] = True
            xs, ys, n = [], [], 0
            while q:
                cx, cy = q.popleft()
                xs.append(cx)
                ys.append(cy)
                n += 1
                for dx, dy in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                    nx, ny = cx + dx, cy + dy
                    if 0 <= nx < w and 0 <= ny < h and mask[ny][nx] and not seen[ny][nx]:
                        seen[ny][nx] = True
                        q.append((nx, ny))
            out.append(
                {
                    "n": n,
                    "box": (region[0] + min(xs), region[1] + min(ys), region[0] + max(xs) + 1, region[1] + max(ys) + 1),
                }
            )
    out.sort(key=lambda b: -b["n"])
    return out


def art(im: Image.Image, size: int = 16) -> str:
    g = im.convert("L").resize((size, size), RESAMPLING)
    chars = " .:-=+*#%@"
    lines = []
    for y in range(size):
        row = []
        for x in range(size):
            v = g.getpixel((x, y))
            assert isinstance(v, int)
            row.append(chars[min(len(chars) - 1, v * len(chars) // 256)])
        lines.append("".join(row))
    return "\n".join(lines)


def diff_stats(a: Image.Image, b: Image.Image) -> tuple[float, int]:
    a = a.convert("RGBA").resize((16, 16), RESAMPLING)
    b = b.convert("RGBA").resize((16, 16), RESAMPLING)
    st = ImageStat.Stat(ImageChops.difference(a, b))
    mean = sum(st.mean) / 4
    extrema = st.extrema
    mx = max(e[1] for e in extrema)
    return mean, int(mx)


def main() -> int:
    shot = Path(sys.argv[1])
    region = (0, 0, 400, 90)
    ref = Path("/opt/cloudpivot/frontend/public/favicon-16x16.png")
    want = 16
    args = sys.argv[2:]
    for i, a in enumerate(args):
        if a == "--region":
            vals = [int(v) for v in args[i + 1].split(",")]
            region = (vals[0], vals[1], vals[2], vals[3])
        elif a == "--ref":
            ref = Path(args[i + 1])
        elif a == "--want":
            want = int(args[i + 1])

    im = Image.open(shot)
    print(f"截图 {shot.name}: {im.size} {im.mode}")
    m = brand_mask(im, region)
    found = blobs(m, region)
    print(f"区域 {region} 内品牌色连通域（按像素数降序，前 8 个）：")
    for b in found[:8]:
        x1, y1, x2, y2 = b["box"]
        print(f"  n={b['n']:>6}  box=({x1},{y1})-({x2},{y2})  {x2 - x1}x{y2 - y1}")

    # 选尺寸最接近 want×want 的那块（排除整片背景/大块文字）
    cands = [b for b in found if 8 <= (b["box"][2] - b["box"][0]) <= want + 8 and 8 <= (b["box"][3] - b["box"][1]) <= want + 8]
    if not cands:
        print("FAIL: 没有找到 8~%dpx 的品牌色块（图标可能没显示）" % (want + 8))
        return 1
    pick = cands[0]
    print(f"\n选中色块: box={pick['box']} n={pick['n']}")
    icon = im.convert("RGBA").crop(pick["box"])
    print("\n--- 截图里的图标（%dx%d 灰度 ASCII）---" % (want, want))
    print(art(icon, want))
    print(f"\n--- {ref.name} ---")
    print(art(Image.open(ref), want))

    mean, mx = diff_stats(icon, Image.open(ref))
    print(f"\n像素差: 平均 {mean:.2f}/255，最大 {mx}/255")
    ok = mean < 45
    print("判定:", "PASS（同一图标）" if ok else "FAIL（差异过大，可能不是新图标）")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
