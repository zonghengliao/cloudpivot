#!/usr/bin/env python3
"""B5 静态等价的「有意变化」归因表生成器。

读 equivalence-raw.json（equivalence-b5.mjs 无白名单跑的产物），把每条 LOST/CHANGED/NEW
按「决策文档 §3.3 的 11 条」+「本批实测新增登记项」分类；分类不上的行原样打印到 stderr
并以退出码 1 结束（= 有未归因的差异，不能白名单掉）。

输出：allow-intentional.txt（equivalence-b5.mjs --allow 的入参）+ README 用的三张表（markdown）
"""
import json
import sys
import collections

RAW = 'equivalence-raw.json'
d = json.load(open(RAW))
rows = d['rows']

DOC = ['erp/contract', 'erp/lot', 'erp/stock-out', 'erp/stock-take', 'hr/attendance', 'hr/leave']
RICH = ['erp/customer', 'erp/material', 'erp/supplier', 'erp/unit', 'erp/warehouse', 'hr/employee']
FILE = ['impexp/export-task', 'impexp/import-task', 'storage/backend', 'storage/file']
LABEL_SM = ['erp/invoice', 'erp/receipt', 'erp/receivable', 'erp/receipt-refund', 'erp/contract', 'erp/cost-labor']


def short(f):
    return f.replace('views/business/', '').replace('/index.vue', '').replace('.vue', '')


# ---------------- 归因规则 ----------------
def categorize(r):
    f, cls, prop, kind = short(r['file']), r['cls'], r['prop'], r['kind']
    b, a = r.get('before'), r.get('after')

    # ① 决策 §3.3 已登记的 11 条 ----------------
    if kind == 'CHANGED' and cls == 'panel-empty' and prop in ('padding-top', 'padding-bottom'):
        if f in ('erp/contract', 'erp/stock-out'):
            return '§3.3-1 doc 族 padding 36→40'
        if f == 'erp/lot':
            return '§3.3-1 doc 族 padding 32→40'
        if f == 'hr/attendance':
            return '§3.3-1 doc 族 padding 28→40'
        if f == 'hr/salary/detail':
            return 'B5-新登记-6 detail 的 inline 族 padding 32px 12px→16px 12px'
    if kind == 'CHANGED' and cls == 'panel-empty__icon' and prop == 'font-size':
        if f in ('hr/attendance', 'hr/leave'):
            return '§3.3-2 doc 族 icon 32→40'
        if f == 'erp/contract':
            return '§3.3-2 doc 族 icon 28→40'
    if kind == 'CHANGED' and cls == 'panel-empty__desc' and prop == 'max-width' and f in ('erp/material', 'erp/warehouse'):
        return '§3.3-3 rich 族 desc max-width 420→460'
    if kind == 'CHANGED' and cls == 'panel-empty__desc' and prop == 'line-height' and f == 'hr/employee':
        return '§3.3-4 rich 族 desc line-height 20px→1.6'
    if kind == 'CHANGED' and cls == 'tool-row__label' and prop == 'font-size' and f == 'erp/cost-labor':
        return '§3.3-9 label 12→13'
    if kind == 'NEW' and cls == 'panel-empty__title' and prop == 'font-weight' and f in ('erp/stock-out', 'erp/stock-take'):
        return '§3.3-10 doc 族 title 加粗到 600'
    if kind == 'CHANGED' and cls == 'panel-empty__desc' and prop in ('color', 'font-size') and f in DOC:
        return '§3.3-11 doc 族 desc 归一到 12px/placeholder'

    # ② 本批实测**新登记**的变化（决策 §3.3 未列，但同一族口径内，逐页截图入卡）----
    if kind == 'CHANGED' and cls == 'panel-empty__title' and prop == 'color' and f in DOC:
        return 'B5-新登记-1 doc 族 title 色 placeholder→primary（4 页原先吃 ListPage 空槽的继承色）'
    if kind == 'CHANGED' and cls == 'panel-empty__desc' and prop in ('margin-top', 'margin-bottom') and f == 'erp/lot':
        return 'B5-新登记-2 lot 的 desc UA 边距（12px）归一到 0（doc 族 desc margin:0）'
    if kind == 'NEW' and cls == 'panel-empty__desc' and prop in ('margin-top', 'margin-bottom') and f == 'erp/lot':
        return 'B5-新登记-2 lot 的 desc UA 边距（12px）归一到 0（doc 族 desc margin:0）'
    if kind == 'NEW' and cls == 'panel-empty__title' and prop == 'font-weight' and f == 'hr/salary/detail':
        return 'B5-新登记-3 detail 的整块空态并入 doc 族（title 加粗 600）'
    if kind == 'CHANGED' and cls == 'panel-empty__desc' and prop == 'color' and f == 'hr/salary/detail':
        return 'B5-新登记-4 detail 空态 desc 色 secondary→placeholder（doc 族口径）'
    if kind == 'NEW' and cls == 'panel-empty__desc' and prop == 'color' and f == 'hr/salary/detail':
        return 'B5-新登记-4 detail 空态 desc 色 secondary→placeholder（doc 族口径）'
    if kind == 'NEW' and cls == 'panel-empty' and prop == 'font-size' and f == 'hr/salary/detail':
        return 'B5-新登记-5 detail 的纯文字内嵌空态并入 inline 族（13px）'

    # ③ 声明显式化但**计算值不变 / 几何惰性**（block 变体里的 flex 系属性；占位边距 0）----
    if kind == 'NEW' and cls == 'panel-empty' and prop in ('display', 'flex-direction', 'align-items', 'gap'):
        return '声明显式化·block 布局下惰性（仅 doc/file/inline 变体页出现）'
    if kind == 'NEW' and cls == 'panel-empty__desc' and prop in ('margin-left', 'margin-right'):
        return '声明显式化·横向边距 0→0'
    if kind == 'NEW' and cls == 'panel-empty__desc' and prop == 'margin-top' and f in FILE:
        return '声明显式化·保留 <p> 的 UA 上边距 13px（实测几何零变化）'
    if kind == 'NEW' and cls == 'panel-empty__desc' and prop == 'margin-bottom' and f in FILE:
        return '声明显式化·desc 下边距 16px（原样）'
    if kind == 'NEW' and cls == 'panel-empty__title' and prop in ('font-size', 'color') and f in DOC:
        return '声明显式化·从继承拿到同值（14px / primary，探针实测仅色相有差的那 4 页另记）'
    if kind == 'NEW' and cls == 'tool-row__label' and prop == 'white-space':
        return '声明显式化·标签 nowrap（短词，实测几何零变化）'
    if kind == 'NEW' and cls == 'panel-empty--inline' and prop in ('display', 'text-align', 'color'):
        return '声明显式化·inline 变体里与旧值相同（block/center/secondary）'
    return None


cats = collections.Counter()
allow_lines = []
unattributed = []
tables = collections.defaultdict(list)
for r in rows:
    c = categorize(r)
    if not c:
        unattributed.append(r)
        continue
    cats[c] += 1
    key = (r['kind'], r['cls'] + '.' + r['prop'])
    tables[c].append(r)
    allow_lines.append(f"{r['kind']} {r['file']}::{r['cls']}.{r['prop']}")

print(f'共 {len(rows)} 条差异，归入 {len(cats)} 类：')
for c, n in cats.most_common():
    print(f'  {n:3d}  {c}')
if unattributed:
    print(f'\n✗ 未归因 {len(unattributed)} 条（必须处理，不许白名单掉）：')
    for r in unattributed:
        print('   ', json.dumps(r, ensure_ascii=False))
    sys.exit(1)

# 加 file 限定的白名单行（同一 (kind, cls.prop) 但在不同页有不同归因时，file 限定不会错放行；
# 统一写带 file 的形式，宁严不宽）
header = """# B5 有意变化白名单（`equivalence-b5.mjs --allow` 入参；由 attrib-allow.py 生成，勿手改）
#
# 分三组，每行都能在 docs/evidence/2026-11-16-shell-b5/README.md 的「有意变化表」里找到同一行依据：
#   ① §3.3-*     决策文档已登记的 11 条（本批落实）
#   ② B5-新登记-* 本批实测发现、同族口径内新增登记（附逐页截图）
#   ③ 声明显式化 变体把属性写全后「新出现的声明」，探针实测计算值相同或几何惰性
"""
open('allow-intentional.txt', 'w').write(header + '\n'.join(sorted(set(allow_lines))) + '\n')
print(f'\nallow-intentional.txt 写入 {len(set(allow_lines))} 行（去重后）')

# README 用的三张表
with open('equivalence-tables.md', 'w') as fh:
    for kind in ('LOST', 'CHANGED', 'NEW'):
        sub = [r for r in rows if r['kind'] == kind]
        fh.write(f'\n### {kind}（{len(sub)} 条）\n\n')
        if not sub:
            fh.write('（0 条）\n')
            continue
        fh.write('| 页 | 类.属性 | 前 | 后 | 归因 |\n|---|---|---|---|---|\n')
        for r in sorted(sub, key=lambda x: (short(x['file']), x['cls'], x['prop'])):
            fh.write(f"| {short(r['file'])} | `{r['cls']}.{r['prop']}` | {r.get('before')} | {r.get('after')} | {categorize(r)} |\n")
print('equivalence-tables.md 写入')
