#!/usr/bin/env python3
"""gallery 77 套配色方案 → `frontend/src/style/palettes.ts` 生成器。

数据源：`docs/design/palette-schemes-77.json`（从设计 gallery 提取，1~4 色/方案）。
目标：把每个方案的 1~4 个源色映射成云枢的 5 个槽位（brand / brandLight / merge / border / accent），
      源色原样使用，缺的槽位按规则派生。

映射规则（按 HSL 亮度 L 升序排序后）：

  4 色：brand=最暗 · brandLight=次暗 · merge=最亮 · accent=次亮 · border=merge 压暗 8%
  3 色：brand=最暗 · merge=最亮 · accent=中间 · brandLight=brand 提亮 22% · border=merge 压暗 8%
  2 色：brand=最暗 · merge=最亮 · brandLight=brand 提亮 22% · border=merge 压暗 8% · accent=brand 补色(+180°)
  1 色：brand=该色 · brandLight=brand 提亮 22% · merge=brand 提到 L=95 · border=brand 压到 L=82 · accent=补色

对比度兜底：merge 上正文（#1A1A1A 或 #F5F5F5）必须 ≥ 4.5:1，否则 merge 沿明度轴按 1% 步进微调，
            只有这条兜底允许动 merge，且每次微调都会写进映射报告。

用法：
  python3 scripts/verify/gen_palette_schemes.py            # 生成并写回 palettes.ts + 映射报告
  python3 scripts/verify/gen_palette_schemes.py --check    # 只校验，不写文件（CI / 提交前）
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / 'docs/design/palette-schemes-77.json'
TARGET = ROOT / 'frontend/src/style/palettes.ts'
REPORT = ROOT / 'docs/evidence/palette-77/mapping-report.md'

# ---------------------------------------------------------------- id / 名称表
# id 一旦写入 localStorage 就不能改；同名方案（SPORTS&OUTDOOR 11 套、青花游鱼转心瓶 2 套、
# 无标题色卡若干）靠 -2/-3 后缀区分。名称取原 title，无 title 的用色名/来源文件编号。
ID_TABLE: dict[int, tuple[str, str, str]] = {
    1: ('nat-canglan-naiyou', '藏蓝奶油', 'fashion'),
    2: ('nat-yunhui-tuo', '云灰驼', 'fashion'),
    3: ('nat-shenhai-wulan', '深海雾蓝', 'fashion'),
    4: ('guci-jiutao', '粉彩九桃天球瓶', 'porcelain'),
    5: ('guci-hulu', '金地粉彩缠枝莲葫芦瓶', 'porcelain'),
    6: ('guci-zhuangxin', '青花游鱼转心瓶', 'porcelain'),
    7: ('guci-zhuangxin-2', '青花游鱼转心瓶·龙纹', 'porcelain'),
    8: ('guci-wanhua', '粉彩万花葫芦瓶', 'porcelain'),
    9: ('guci-huaniao', '青花粉彩花鸟扁瓶', 'porcelain'),
    10: ('guci-meiping', '元青花四爱图梅瓶', 'porcelain'),
    11: ('guci-fushou', '粉彩镂空福寿双耳瓶', 'porcelain'),
    12: ('nat-dizhonghai', '地中海红蓝', 'fashion'),
    13: ('guci-qingluan', '晴峦叠翠', 'porcelain'),
    14: ('guci-qianli', '青绿千里', 'porcelain'),
    15: ('guci-yuanshan', '远山入黛', 'porcelain'),
    16: ('nat-danxia', '丹霞赭石', 'fashion'),
    17: ('pms-sports-outdoor', '咖啡里昂蓝', 'fashion'),
    18: ('pms-popular', '沙灰肉桂', 'fashion'),
    19: ('pms-sports-outdoor-2', '灰紫波塞冬', 'fashion'),
    20: ('pms-sports-outdoor-3', '早霜阿拉斯加', 'fashion'),
    21: ('pms-sports-outdoor-4', '榛子翠蓝', 'fashion'),
    22: ('pms-shuangzi', '霜紫宇宙紫', 'fashion'),
    23: ('pms-chengbao', '城堡波特酒', 'fashion'),
    24: ('pms-xinchao', '新潮空间', 'fashion'),
    25: ('pms-sports-outdoor-5', '海洋洞穴', 'fashion'),
    26: ('nat-img9151', '色卡 9151', 'fashion'),
    27: ('nat-img9152', '色卡 9152', 'fashion'),
    28: ('nat-img9153', '色卡 9153', 'fashion'),
    29: ('nat-img9154', '色卡 9154', 'fashion'),
    30: ('nat-img9155', '色卡 9155', 'fashion'),
    31: ('nat-img9156', '色卡 9156', 'fashion'),
    32: ('nat-img9157', '色卡 9157', 'fashion'),
    33: ('nat-img9158', '色卡 9158', 'fashion'),
    34: ('nat-img9159', '色卡 9159', 'fashion'),
    35: ('guci-youlibai', '釉里白', 'porcelain'),
    36: ('guci-ciyun', '青花瓷韵', 'porcelain'),
    37: ('guci-haishui', '海水江崖', 'porcelain'),
    38: ('guci-yingyue', '夜瓷映月', 'porcelain'),
    39: ('nat-wulan-qing', '浅雾蓝青', 'fashion'),
    40: ('nat-img9165', '色卡 9165', 'fashion'),
    41: ('nat-img9166', '色卡 9166', 'fashion'),
    42: ('nat-img9167', '色卡 9167', 'fashion'),
    43: ('nat-img9168', '色卡 9168', 'fashion'),
    44: ('nat-img9169', '色卡 9169', 'fashion'),
    45: ('nat-img9170', '色卡 9170', 'fashion'),
    46: ('pms-sports-outdoor-6', '羚羊皮翠蓝', 'fashion'),
    47: ('pms-sports-outdoor-7', '橡金橘探戈', 'fashion'),
    48: ('pms-shanye', '到山野间', 'fashion'),
    49: ('pms-sports-outdoor-8', '方钠石金猕猴', 'fashion'),
    50: ('pms-migua', '蜜瓜色', 'fashion'),
    51: ('pms-sports-outdoor-9', '瓦蓝三色堇', 'fashion'),
    52: ('pms-shenshuilan', '深水蓝', 'fashion'),
    53: ('pms-yueliang', '月亮蓝', 'fashion'),
    54: ('pms-miaomiao', '妙妙王国', 'fashion'),
    55: ('pms-kaoguo', '烤果馅饼', 'fashion'),
    56: ('pms-xingrenlao', '杏仁酪', 'fashion'),
    57: ('pms-heimei', '黑莓果酱', 'fashion'),
    58: ('pms-sports-outdoor-10', '褐菇海洋洞穴', 'fashion'),
    59: ('pms-hejin', '时尚合金', 'fashion'),
    60: ('pms-shanchuang', '山创空间', 'fashion'),
    61: ('pms-sports-outdoor-11', '云米山鹑', 'fashion'),
    62: ('pms-dama', '大麻色', 'fashion'),
    63: ('pms-shanxi', '山系色会', 'fashion'),
    64: ('nat-persimmon', '柿子色', 'fashion'),
    65: ('nat-crested-ibis', '朱鹮', 'fashion'),
    66: ('nat-bee-eater', '蓝喉蜂虎', 'fashion'),
    67: ('nat-osmanthus', '桂花', 'fashion'),
    68: ('nat-muhly-grass', '芒草', 'fashion'),
    69: ('nat-huxiu', '狐锈红', 'fashion'),
    70: ('nat-oak', '橡木', 'fashion'),
    71: ('nat-maple', '枫', 'fashion'),
    72: ('nat-chanterelle', '鸡油菌', 'fashion'),
    73: ('guci-fenqiang', '粉墙桂蕊', 'porcelain'),
    74: ('guci-chishui', '池水太湖', 'porcelain'),
    75: ('guci-xuanzhi', '宣纸晚霞', 'porcelain'),
    76: ('guci-yuedong', '月洞桂香', 'porcelain'),
    77: ('guci-gongming', '暮雪宫门', 'porcelain'),
}

HEX6 = re.compile(r'^#[0-9A-Fa-f]{6}$')


# ---------------------------------------------------------------- 颜色运算（与 palettes.ts 同源）
def parse_hex(hex_str: str) -> tuple[int, int, int]:
    raw = hex_str.strip().lstrip('#')
    if raw.lower().startswith('0x'):
        raw = raw[2:]
    if len(raw) == 3:
        raw = ''.join(ch + ch for ch in raw)
    if not re.fullmatch(r'[0-9a-fA-F]{6}', raw):
        raise ValueError(f'非法十六进制色值：{hex_str}')
    return int(raw[0:2], 16), int(raw[2:4], 16), int(raw[4:6], 16)


def clamp(v: float, lo: float = 0.0, hi: float = 100.0) -> float:
    return lo if v < lo else hi if v > hi else v


def to_hex(rgb: tuple[float, float, float]) -> str:
    parts = [f'{int(round(clamp(c, 0, 255))):02x}' for c in rgb]
    return '#' + ''.join(parts)


def rgb_to_hsl(rgb: tuple[int, int, int]) -> tuple[float, float, float]:
    r, g, b = (c / 255 for c in rgb)
    mx, mn = max(r, g, b), min(r, g, b)
    l = (mx + mn) / 2
    if mx == mn:
        return 0.0, 0.0, l * 100
    d = mx - mn
    s = d / (2 - mx - mn) if l > 0.5 else d / (mx + mn)
    if mx == r:
        h = ((g - b) / d + (6 if g < b else 0)) / 6
    elif mx == g:
        h = ((b - r) / d + 2) / 6
    else:
        h = ((r - g) / d + 4) / 6
    return h * 360, s * 100, l * 100


def hsl_to_rgb(h: float, s: float, l: float) -> tuple[float, float, float]:
    hh = ((h % 360) + 360) % 360 / 360
    ss = clamp(s) / 100
    ll = clamp(l) / 100
    if ss == 0:
        v = ll * 255
        return v, v, v
    q = ll * (1 + ss) if ll < 0.5 else ll + ss - ll * ss
    p = 2 * ll - q

    def hue(t: float) -> float:
        tt = t
        if tt < 0:
            tt += 1
        if tt > 1:
            tt -= 1
        if tt < 1 / 6:
            return p + (q - p) * 6 * tt
        if tt < 1 / 2:
            return q
        if tt < 2 / 3:
            return p + (q - p) * (2 / 3 - tt) * 6
        return p

    return hue(hh + 1 / 3) * 255, hue(hh) * 255, hue(hh - 1 / 3) * 255


def hex_to_hsl(hex_str: str) -> tuple[float, float, float]:
    return rgb_to_hsl(parse_hex(hex_str))


def hsl_to_hex(h: float, s: float, l: float) -> str:
    return to_hex(hsl_to_rgb(h, s, l))


def with_l(hex_str: str, l: float, s: float | None = None, h: float | None = None) -> str:
    hh, ss, _ = hex_to_hsl(hex_str)
    return hsl_to_hex(hh if h is None else h, clamp(ss if s is None else s), clamp(l))


def with_hue(hex_str: str, dh: float) -> str:
    hh, ss, ll = hex_to_hsl(hex_str)
    return hsl_to_hex(hh + dh, ss, ll)


def luminance(hex_str: str) -> float:
    ch = []
    for v in parse_hex(hex_str):
        x = v / 255
        ch.append(x / 12.92 if x <= 0.03928 else ((x + 0.055) / 1.055) ** 2.4)
    return 0.2126 * ch[0] + 0.7152 * ch[1] + 0.0722 * ch[2]


def contrast(a: str, b: str) -> float:
    la, lb = luminance(a), luminance(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


BODY_DARK = '#1A1A1A'
BODY_LIGHT = '#F5F5F5'
TEXT_MIN = 4.5


def best_text_contrast(bg: str) -> float:
    return max(contrast(BODY_DARK, bg), contrast(BODY_LIGHT, bg))


def fix_merge_contrast(merge: str) -> tuple[str, int, str]:
    """merge 上调正文对比度 < 4.5 时按 1% 明度步进微调，返回 (色值, 步数, 方向)。"""
    if best_text_contrast(merge) >= TEXT_MIN:
        return merge, 0, ''
    _, _, l = hex_to_hsl(merge)
    # 底色偏亮 → 需要更亮（#1A1A1A 对比更高）；反之压暗
    lighter = contrast(BODY_DARK, merge) >= contrast(BODY_LIGHT, merge)
    for step in range(1, 41):
        cand = with_l(merge, l + step if lighter else l - step)
        if best_text_contrast(cand) >= TEXT_MIN:
            return cand, step, '提亮' if lighter else '压暗'
    return merge, 0, '未能修正'


# ---------------------------------------------------------------- 映射
def map_scheme(colors: list[dict], title: str) -> dict:
    hexes = [c['hex'].upper() for c in colors]
    for label, value in zip((c['name'] for c in colors), hexes):
        if not HEX6.match(value):
            raise SystemExit(f'[{title}] 非法色值 {value}（{label}）')

    n = len(hexes)
    ranked = sorted(range(n), key=lambda i: (hex_to_hsl(hexes[i])[2], i))
    by_l = [hexes[i] for i in ranked]

    derived: list[str] = []
    if n >= 5:
        brand, brand_light, merge, border, accent = hexes[:5]
        derived = []
    elif n == 4:
        brand, brand_light, merge, accent = by_l[0], by_l[1], by_l[3], by_l[2]
        border = with_l(merge, hex_to_hsl(merge)[2] - 8)
        derived = ['border']
    elif n == 3:
        brand, merge, accent = by_l[0], by_l[2], by_l[1]
        brand_light = with_l(brand, hex_to_hsl(brand)[2] + 22)
        border = with_l(merge, hex_to_hsl(merge)[2] - 8)
        derived = ['brandLight', 'border']
    elif n == 2:
        brand, merge = by_l[0], by_l[1]
        brand_light = with_l(brand, hex_to_hsl(brand)[2] + 22)
        border = with_l(merge, hex_to_hsl(merge)[2] - 8)
        accent = with_hue(brand, 180)
        derived = ['brandLight', 'border', 'accent']
    else:  # 1 色
        brand = by_l[0]
        brand_light = with_l(brand, hex_to_hsl(brand)[2] + 22)
        merge = with_l(brand, 95)
        border = with_l(brand, 82)
        accent = with_hue(brand, 180)
        derived = ['brandLight', 'merge', 'border', 'accent']

    merge_raw = merge
    merge, steps, direction = fix_merge_contrast(merge)
    if steps:
        derived = sorted(set(derived + ['merge']))
    # 统一大写 #RRGGBB（与 FEATURED_PALETTES 的书写风格一致）
    out = {
        'brand': brand,
        'brandLight': brand_light,
        'merge': merge,
        'border': border,
        'accent': accent,
    }
    return {
        'n': n,
        'ranked': by_l,
        **{k: v.upper() for k, v in out.items()},
        'derived': derived,
        'merge_raw': merge_raw,
        'merge_steps': steps,
        'merge_dir': direction,
    }


def build_note(colors: list[dict], title: str, source: str) -> str:
    labels = [c['name'] if not c['name'].startswith('#') else c['hex'].upper() for c in colors]
    clean = title.strip()
    tail = clean if clean and clean != '无标题' else f'来源 {source}'
    refs = [c['ref'].replace('PANTONE ', '').replace(' TCX', '').replace(' TSX', '') for c in colors]
    refs = [r for r in refs if r]
    if refs:
        tail = f'{tail}（{" / ".join(refs)}）'
    return f'{" × ".join(labels)} —— {tail}'


def ts_escape(text: str) -> str:
    return text.replace('\\', '\\\\').replace("'", "\\'")


def main() -> int:
    check_only = '--check' in sys.argv[1:]
    data = json.loads(SRC.read_text(encoding='utf-8'))
    if len(data) != 77:
        raise SystemExit(f'源数据应为 77 套，实际 {len(data)}')

    entries: list[dict] = []
    seen_ids: set[str] = set()
    for item in data:
        idx = item['index']
        if idx not in ID_TABLE:
            raise SystemExit(f'index {idx} 缺少 id/名称映射')
        pid, name, group = ID_TABLE[idx]
        if pid in seen_ids:
            raise SystemExit(f'id 重复：{pid}')
        seen_ids.add(pid)
        mapped = map_scheme(item['colors'], item['title'] or item['source_file'])
        entries.append(
            {
                'index': idx,
                'id': pid,
                'name': name,
                'group': group,
                'title': item['title'],
                'source': item['source_file'],
                'sourceColors': [c['hex'].upper() for c in item['colors']],
                'note': build_note(item['colors'], item['title'], item['source_file']),
                **mapped,
            }
        )

    # -------- TS 片段的两个块
    id_lines = [f"  | '{e['id']}'" for e in entries]
    ids_block = '\n'.join(['  // <gallery-ids:begin>', *id_lines, '  // <gallery-ids:end>'])

    body: list[str] = []
    for e in entries:
        derived_txt = '、'.join(e['derived']) if e['derived'] else '无（5 色直取）'
        body.append('  {')
        body.append(f"    id: '{e['id']}',")
        body.append(f"    name: '{ts_escape(e['name'])}',")
        body.append(f"    note: '{ts_escape(e['note'])}',")
        body.append(f"    brand: '{e['brand']}',")
        body.append(f"    brandLight: '{e['brandLight']}',")
        body.append(f"    merge: '{e['merge']}',")
        body.append(f"    border: '{e['border']}',")
        body.append(f"    accent: '{e['accent']}',")
        body.append(f"    group: '{e['group']}',")
        body.append(f"    source: '{e['source']}',")
        body.append(f"    sourceColors: [{', '.join(chr(39) + c + chr(39) for c in e['sourceColors'])}],")
        body.append(f'  }}, // #{e["index"]} 源 {e["n"]} 色 · 派生：{derived_txt}')
    data_block = '\n'.join(
        [
            '// <gallery-data:begin>',
            '/** gallery 77 套（由 scripts/verify/gen_palette_schemes.py 从 docs/design/palette-schemes-77.json 生成，勿手改） */',
            'export const GALLERY_PALETTES: readonly PaletteDef[] = [',
            *body,
            '];',
            '// <gallery-data:end>',
        ]
    )

    report_lines = [
        '# gallery 77 套 → 云枢 5 槽位映射报告',
        '',
        f'- 数据源：`docs/design/palette-schemes-77.json`（{len(data)} 套 / {sum(e["n"] for e in entries)} 个源色）',
        '- 生成器：`scripts/verify/gen_palette_schemes.py`（本报告与 `palettes.ts` 的 gallery 段同源生成）',
        '- 校验器：`python3 scripts/verify/check_palettes.py`',
        '',
        '## 规则',
        '',
        '| 源色数 | brand | brandLight | merge | border | accent |',
        '|---|---|---|---|---|---|',
        '| 4 | 最暗 | 次暗 | 最亮 | merge −8% L | 次亮 |',
        '| 3 | 最暗 | brand +22% L | 最亮 | merge −8% L | 中间 |',
        '| 2 | 最暗 | brand +22% L | 最亮 | merge −8% L | brand 补色 +180° |',
        '| 1 | 该色 | brand +22% L | brand → L95 | brand → L82 | brand 补色 +180° |',
        '',
        '源色一律原样使用（派生槽位单独标出）；merge 上正文（#1A1A1A / #F5F5F5）不足 4.5:1 时才微调 merge。',
        '',
        '## 逐条映射',
        '',
        '| # | id | 名称 | 分组 | 源色（按 L 升序） | brand | brandLight | merge | border | accent | 派生槽位 | merge 兜底 |',
        '|---|---|---|---|---|---|---|---|---|---|---|---|',
    ]
    for e in entries:
        nudge = f'{e["merge_dir"]} {e["merge_steps"]}%（原 {e["merge_raw"]}）' if e['merge_steps'] else '—'
        report_lines.append(
            '| {} | `{}` | {} | {} | {} | `{}` | `{}` | `{}` | `{}` | `{}` | {} | {} |'.format(
                e['index'],
                e['id'],
                e['name'],
                {'porcelain': '古典釉色', 'fashion': '时尚配色', 'featured': '云枢精选'}[e['group']],
                ' · '.join(f'`{c}`' for c in e['ranked']),
                e['brand'],
                e['brandLight'],
                e['merge'],
                e['border'],
                e['accent'],
                '、'.join(e['derived']) if e['derived'] else '—',
                nudge,
            )
        )

    nudged = [e for e in entries if e['merge_steps']]
    groups = {}
    for e in entries:
        groups[e['group']] = groups.get(e['group'], 0) + 1
    report_lines += [
        '',
        '## 汇总',
        '',
        f'- 方案数：{len(entries)}（古典釉色 {groups.get("porcelain", 0)} · 时尚配色 {groups.get("fashion", 0)}）',
        f'- 源色总数：{sum(e["n"] for e in entries)}（1 色 {sum(1 for e in entries if e["n"] == 1)} · 2 色 {sum(1 for e in entries if e["n"] == 2)} · 3 色 {sum(1 for e in entries if e["n"] == 3)} · 4 色 {sum(1 for e in entries if e["n"] == 4)}）',
        f'- merge 对比度兜底触发：{len(nudged)} 套',
        '- 数据疑点：第 50/52/55/63 套源数据只有一个色值且都是 `#DDDDDD`（色名分别为蜜瓜色 / 深水蓝 / 烤果馅饼 / 树皮色，PANTONE 参考号各不相同）——'
        ' 判断为提取件的占位色而非真实取色。按「色值原样保留」红线未擅自改值，等需求方提供真实色值后替换 JSON 重跑生成器即可。',
        '',
    ]
    if nudged:
        report_lines.append('`merge` 兜底明细：')
        report_lines.append('')
        for e in nudged:
            report_lines.append(
                f'- {e["name"]}（`{e["id"]}`）：{e["merge_raw"]} → {e["merge"]}（{e["merge_dir"]} {e["merge_steps"]}%），'
                f'正文对比度 {contrast(BODY_DARK, e["merge"]):.2f} / {contrast(BODY_LIGHT, e["merge"]):.2f}'
            )
        report_lines.append('')

    target_src = TARGET.read_text(encoding='utf-8')
    if '// <gallery-ids:begin>' not in target_src or '// <gallery-data:begin>' not in target_src:
        raise SystemExit('palettes.ts 缺少 gallery 标记，无法注入')

    new_src = re.sub(
        r'  // <gallery-ids:begin>.*?  // <gallery-ids:end>',
        ids_block,
        target_src,
        flags=re.S,
    )
    new_src = re.sub(
        r'// <gallery-data:begin>.*?// <gallery-data:end>',
        data_block,
        new_src,
        flags=re.S,
    )

    if check_only:
        drift = new_src != target_src
        print('[gen_palette_schemes] --check：', '与生成结果不一致（需要重跑生成器）' if drift else '与生成结果一致')
        return 1 if drift else 0

    TARGET.write_text(new_src, encoding='utf-8')
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text('\n'.join(report_lines), encoding='utf-8')
    print(f'[gen_palette_schemes] 已写入 {TARGET.relative_to(ROOT)}（{len(entries)} 套）')
    print(f'[gen_palette_schemes] 映射报告 {REPORT.relative_to(ROOT)}')
    print(f'[gen_palette_schemes] merge 兜底触发 {len(nudged)} 套')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
