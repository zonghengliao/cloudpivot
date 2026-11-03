"""截图像素取证：浮层区域是否真的画了东西（视觉配额用尽时的替代证据）。

对比「浮层区域」与「同尺寸空白区域」的像素方差/边缘密度：
浮层有边框、文字、分割线 → 方差与边缘数显著更高；浮层收起后该区域应与背景一致。
"""
from PIL import Image
import sys

OUT = '/opt/cloudpivot/docs/evidence/20261103-notify-pop'

# 浮层实测 box: x=1124, y=64, width=300（不同条数高度不同）
REGIONS = {
    '10-toast-online-single.png': (1124, 64, 1424, 234),
    '11-toast-auto-closed.png': (1124, 64, 1424, 234),
    '12-toast-hover-pause.png': (1124, 64, 1424, 234),
    '13-toast-merged-3.png': (1124, 64, 1424, 294),
    '15-bell-still-works.png': (1124, 56, 1424, 226),
    '01-login-dialog-admin.png': (480, 250, 960, 650),
    '06-login-dialog-empty-skip.png': (480, 250, 960, 650),
}

def stats(img, box):
    crop = img.crop(box).convert('L')
    px = list(crop.getdata())
    w, h = crop.size
    mean = sum(px) / len(px)
    var = sum((p - mean) ** 2 for p in px) / len(px)
    # 边缘密度：相邻像素差 > 24 的比例（文字/边框/分割线）
    edges = 0
    for y in range(h):
        row = px[y * w:(y + 1) * w]
        for x in range(1, w):
            if abs(row[x] - row[x - 1]) > 24:
                edges += 1
    return round(mean, 1), round(var, 1), round(edges / (w * h) * 100, 2)

for name, box in REGIONS.items():
    try:
        img = Image.open(f'{OUT}/{name}')
    except FileNotFoundError:
        print(f'{name}: MISSING')
        continue
    m, v, e = stats(img, box)
    # 对照：同一行左边同等大小的区域（页面内容区，通常较空）
    ctrl = (box[0] - 620, box[1], box[2] - 620, box[3])
    cm, cv, ce = stats(img, ctrl)
    print(f'{name}: size={img.size} box_mean={m} box_var={v} edge%={e} | control_var={cv} control_edge%={ce}')
