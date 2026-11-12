#!/usr/bin/env python3
"""云枢配色方案机械校验器（对应 T-ac0466d8 验收 ③④⑤）。

判据：
  ③ 方案数量：`frontend/src/style/palettes.ts` 里的 PaletteDef 条数 = 22 精选 + 77 gallery = 99
  ④ 色值合法性：每套的 5 个槽位必须是 7 位 `#RRGGBB`；id 唯一；两个 id 联合类型与数据一致
  ⑤ 对比度：merge 底色上正文（#1A1A1A / #F5F5F5 取较优）≥ 4.5:1
  附加（色值红线）：`docs/design/palette-schemes-77.json` 里的 264 个源色必须原样出现在 palettes.ts 里

用法：
  python3 scripts/verify/check_palettes.py            # 打印结论，失败退出码 1
  python3 scripts/verify/check_palettes.py --json     # 机器可读
  python3 scripts/verify/check_palettes.py --md       # markdown 表（写证据文档用）
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TS = ROOT / 'frontend/src/style/palettes.ts'
SRC = ROOT / 'docs/design/palette-schemes-77.json'

HEX6 = re.compile(r'^#[0-9A-Fa-f]{6}$')
BODY_TEXT = ('#1A1A1A', '#F5F5F5')
TEXT_MIN = 4.5
FEATURED_COUNT = 22
GALLERY_COUNT = 77

SLOTS = ('brand', 'brandLight', 'merge', 'border', 'accent')


# ---------------------------------------------------------------- 颜色运算
def parse_hex(value: str) -> tuple[int, int, int]:
    raw = value.strip().lstrip('#')
    return int(raw[0:2], 16), int(raw[2:4], 16), int(raw[4:6], 16)


def luminance(value: str) -> float:
    out = []
    for v in parse_hex(value):
        x = v / 255
        out.append(x / 12.92 if x <= 0.03928 else ((x + 0.055) / 1.055) ** 2.4)
    return 0.2126 * out[0] + 0.7152 * out[1] + 0.0722 * out[2]


def contrast(a: str, b: str) -> float:
    la, lb = luminance(a), luminance(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)


# ---------------------------------------------------------------- 解析
def parse_defs(source: str) -> list[dict]:
    """只解析两个方案数组区段（FEATURED_PALETTES / GALLERY_PALETTES）里的 PaletteDef 字面量。

    不整文件扫：文件里还有 `PALETTE_GROUPS` 这种同样的 `{ id: ... }` 单行对象，会误匹配。
    """
    regions: list[str] = []
    featured = re.search(r'export const FEATURED_PALETTES[^=]*=\s*\[(.*?)\n\];', source, re.S)
    if featured:
        regions.append(featured.group(1))
    gallery = re.search(r'export const GALLERY_PALETTES[^=]*=\s*\[(.*?)\n\];', source, re.S)
    if gallery:
        regions.append(gallery.group(1))

    defs: list[dict] = []
    for region in regions:
        for block in re.finditer(r'\{\n    id: \'([^\']+)\'(.*?)\n  \},', region, re.S):
            body = block.group(0)
            item = {'id': block.group(1)}
            for slot in SLOTS:
                m = re.search(rf"{slot}:\s*'([^']*)'", body)
                item[slot] = m.group(1) if m else None
            m = re.search(r"group:\s*'([^']+)'", body)
            item['group'] = m.group(1) if m else 'featured'
            m = re.search(r'source:\s*\'([^\']+)\'', body)
            item['source'] = m.group(1) if m else None
            m = re.search(r'sourceColors:\s*\[([^\]]*)\]', body)
            item['sourceColors'] = re.findall(r"'([^']+)'", m.group(1)) if m else []
            defs.append(item)
    return defs


def main() -> int:
    args = set(sys.argv[1:])
    as_json = '--json' in args
    as_md = '--md' in args

    source = TS.read_text(encoding='utf-8')
    defs = parse_defs(source)
    errors: list[str] = []
    rows: list[dict] = []

    # ---------- ③ 数量
    if len(defs) != FEATURED_COUNT + GALLERY_COUNT:
        errors.append(
            f'方案数量应为 {FEATURED_COUNT + GALLERY_COUNT}（{FEATURED_COUNT} 精选 + {GALLERY_COUNT} gallery），实际 {len(defs)}'
        )

    featured = [d for d in defs if d['group'] == 'featured']
    gallery = [d for d in defs if d['group'] != 'featured']
    if len(featured) != FEATURED_COUNT:
        errors.append(f'精选方案应为 {FEATURED_COUNT} 套，实际 {len(featured)}')
    if len(gallery) != GALLERY_COUNT:
        errors.append(f'gallery 方案应为 {GALLERY_COUNT} 套，实际 {len(gallery)}')

    # ---------- ④ 色值合法性 / id 唯一 / 联合类型一致
    ids = [d['id'] for d in defs]
    duplicates = sorted({i for i in ids if ids.count(i) > 1})
    if duplicates:
        errors.append(f'id 重复：{", ".join(duplicates)}')

    for item in defs:
        for slot in SLOTS:
            value = item[slot]
            if value is None:
                errors.append(f"{item['id']}.{slot} 缺失")
            elif not HEX6.match(value):
                errors.append(f"{item['id']}.{slot} 非法色值：{value}")
        if item['group'] not in ('featured', 'porcelain', 'fashion'):
            errors.append(f"{item['id']}.group 非法：{item['group']}")

    # id 必须都出现在 PaletteId 联合类型里（否则 setPalette/persist 类型对不上）
    union = re.search(r'export type GalleryPaletteId =(.*?);', source, re.S)
    gallery_union = set(re.findall(r"'([^']+)'", union.group(1))) if union else set()
    union_featured = re.search(r'export type FeaturedPaletteId =(.*?);', source, re.S)
    featured_union = set(re.findall(r"'([^']+)'", union_featured.group(1))) if union_featured else set()
    missing_in_union = sorted(set(ids) - (gallery_union | featured_union))
    if missing_in_union:
        errors.append(f'以下 id 未写进 PaletteId 联合类型：{", ".join(missing_in_union)}')
    extra_in_union = sorted((gallery_union | featured_union) - set(ids))
    if extra_in_union:
        errors.append(f'联合类型里有、数据里没有的 id：{", ".join(extra_in_union)}')

    # ---------- ⑤ 对比度 + 逐套汇总
    for item in defs:
        merge = item['merge']
        ratio_dark = contrast(BODY_TEXT[0], merge)
        ratio_light = contrast(BODY_TEXT[1], merge)
        best = max(ratio_dark, ratio_light)
        ok = best >= TEXT_MIN
        if not ok:
            errors.append(
                f"{item['id']}：merge {merge} 上正文对比度最高 {best:.2f} < {TEXT_MIN}"
                f"（#1A1A1A {ratio_dark:.2f} / #F5F5F5 {ratio_light:.2f}）"
            )
        rows.append(
            {
                'id': item['id'],
                'group': item['group'],
                'brand': item['brand'],
                'merge': merge,
                'contrast_dark': round(ratio_dark, 2),
                'contrast_light': round(ratio_light, 2),
                'best': round(best, 2),
                'ok': ok,
            }
        )

    # ---------- 附加：264 个源色一条不丢
    src_data = json.loads(SRC.read_text(encoding='utf-8'))
    src_hexes = [c['hex'].upper() for scheme in src_data for c in scheme['colors']]
    ts_hexes = set(m.upper() for m in re.findall(r'#[0-9A-Fa-f]{6}', source))
    lost = sorted({h for h in src_hexes if h not in ts_hexes})
    if lost:
        errors.append(f'以下源色在 palettes.ts 里找不到（色值红线被破坏）：{", ".join(lost)}')

    # gallery 的 sourceColors 必须与 JSON 逐条一致
    by_source = {scheme['source_file']: [c['hex'].upper() for c in scheme['colors']] for scheme in src_data}
    for item in gallery:
        want = by_source.get(item['source'] or '')
        if want is None:
            errors.append(f"{item['id']}：source {item['source']} 不在 JSON 里")
        elif [c.upper() for c in item['sourceColors']] != want:
            errors.append(
                f"{item['id']}：sourceColors 与 JSON 不一致（{item['sourceColors']} vs {want}）"
            )

    # ---------- 输出
    payload = {
        'palettes': len(defs),
        'featured': len(featured),
        'gallery': len(gallery),
        'sourceColors': len(src_hexes),
        'minContrast': min((r['best'] for r in rows), default=0),
        'errors': errors,
        'passed': not errors,
    }

    if as_json:
        print(json.dumps({**payload, 'rows': rows}, ensure_ascii=False, indent=2))
    elif as_md:
        print('| # | id | 分组 | brand | merge | #1A1A1A/merge | #F5F5F5/merge | 取优 | 结论 |')
        print('|---|---|---|---|---|---|---|---|---|')
        for index, row in enumerate(rows, 1):
            print(
                f"| {index} | `{row['id']}` | {row['group']} | `{row['brand']}` | `{row['merge']}` | "
                f"{row['contrast_dark']} | {row['contrast_light']} | {row['best']} | "
                f"{'达标' if row['ok'] else '不达标'} |"
            )
        print()
        print(f"合计 {len(rows)} 套 · 最低对比度 {payload['minContrast']} · 错误 {len(errors)} 条")
    else:
        print('配色方案校验（T-ac0466d8 验收 ③④⑤）')
        print(f"  方案数量：{len(defs)}（精选 {len(featured)} + gallery {len(gallery)}）—— 判据 ≥ {FEATURED_COUNT + GALLERY_COUNT}")
        print(f'  色值合法性：{len(defs) * len(SLOTS)} 个槽位，非法 0' if not any('非法色值' in e for e in errors) else '  色值合法性：存在非法色值')
        print(f"  merge 正文对比度：最低 {payload['minContrast']}（门槛 {TEXT_MIN}）")
        print(f'  源色保真：JSON {len(src_hexes)} 个源色全部在 palettes.ts 中（缺失 {len(lost)} 个）')
        if errors:
            print(f'\n✗ 校验未通过（{len(errors)} 项）：')
            for error in errors:
                print(f'  - {error}')
        else:
            print('\n✓ 全部通过')

    return 1 if errors else 0


if __name__ == '__main__':
    raise SystemExit(main())
