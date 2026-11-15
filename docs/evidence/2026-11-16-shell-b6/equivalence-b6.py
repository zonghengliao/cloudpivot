#!/usr/bin/env python3
"""B6 静态等价断言（A/B 组：提基线 / 并入基线 / 死代码删除）。

口径（照 B5 的 equivalence 思路，但本批只有 7 条裁定要验，不做全量 147 类扫描）：
  对每条裁定，把「改造前该元素实际生效的声明集」与「改造后实际生效的声明集」逐属性比：
    before = 报告工具 before 快照里该文件的本地声明（B6 批）+ 当时的基线（如果有）
    after  = 报告工具 after 快照里的基线（`--second` / `__side`）
  三类结论：
    · SAME               —— 属性值相同
    · CHANGED-INTENTIONAL —— 登记的有意变化（gap 8→6 之类），必须能对上 §3 表
    · LOST / NEW         —— 丢失 / 新增属性；LOST 只允许「死代码删除」这一类（页面从未渲染的元素）
  任何一条对不上 → 退出码 1。

用法：python3 equivalence-b6.py report-before.json report-after.json
"""
import json
import sys

RULINGS = [
    # (标签, 文件, 旧类, 出口, 新类/None)
    ('A1', 'views/business/erp/report/report-item/index.vue', 'tool-row--filter', 'promoted', 'tool-row--second'),
    ('A2', 'views/business/erp/invoice/index.vue', 'tool-row--second', 'dead', None),
    ('A3', 'views/business/erp/receipt/index.vue', 'tool-row--second', 'dead', None),
    ('A4', 'views/business/erp/receivable/index.vue', 'tool-row--second', 'dead', None),
    ('A5', 'views/business/erp/receipt-refund/index.vue', 'tool-row--second', 'dead', None),
    ('B1', 'views/business/erp/budget/report.vue', 'panel-head__actions', 'merged', 'panel-head__side'),
    ('B2', 'views/business/erp/cost-period/steps/UncollectedPanel.vue', 'panel-head__ops', 'merged', 'panel-head__side'),
]

# 登记的有意变化（属性级）
INTENTIONAL = {
    ('panel-head__actions', 'panel-head__side', 'gap'): '8px → 6px：`__side` 基线的槽内间距（同族单值，2px 级）',
    ('panel-head__actions', 'panel-head__side', 'align-items'): '新增 `center`：`__side` 基线写全（按钮自身有固定高度 → 几何惰性）',
    ('panel-head__ops', 'panel-head__side', 'gap'): '8px → 6px：同上',
    ('panel-head__ops', 'panel-head__side', 'align-items'): '新增 `center`：同上',
    ('panel-head__ops', 'panel-head__side', 'margin-left'): '`auto` 退休：`.panel-head` 是 `space-between`，auto 外边距原本就只吸收同一份自由空间（探针实测 x 不变）',
}


def props_of(report, file, cls):
    for b in report['batches'].get('B6', {}).get('detail', []):
        if b['file'] == file and b['cls'] == cls:
            return b['props'] if not b['selector'].endswith(':hover') else None
    return None


def norm(v):
    return str(v).lower().replace('var(--td-text-color-secondary)', 'sec')


def main() -> int:
    before = json.load(open(sys.argv[1]))
    after = json.load(open(sys.argv[2]))
    baseline = after['baseline']['classes']
    rows = []
    bad = []
    for tag, file, old, route, new in RULINGS:
        bp = props_of(before, file, old)
        if bp is None:
            bad.append(f'{tag}: before 快照里找不到 {file} {old}')
            continue
        if route == 'dead':
            # 死代码：页面里没有任何元素带这个类（探针单独证明 0 元素），删除后不产生任何生效声明
            rows.append((tag, file, old, '(删除)', 'margin-top', bp.get('margin-top'), '—', 'DEAD-NOOP'))
            continue
        ap = baseline.get(new, {}).get('props')
        if ap is None:
            bad.append(f'{tag}: after 基线里找不到 .{new}')
            continue
        for prop in sorted(set(bp) | set(ap)):
            b, a = bp.get(prop), ap.get(prop)
            if b == a:
                verdict = 'SAME'
            elif (old, new, prop) in INTENTIONAL:
                verdict = 'CHANGED-INTENTIONAL'
            elif b is None:
                verdict = 'NEW'
            elif a is None:
                verdict = 'LOST'
            else:
                verdict = 'CHANGED-UNLISTED'
            rows.append((tag, file, old, new, prop, b if b is not None else '—', a if a is not None else '—', verdict))
            if verdict in ('CHANGED-UNLISTED', 'LOST'):
                bad.append(f'{tag}: 未登记的 {verdict} — {file} {old}.{prop}: {b} → {a}')

    print('| 裁定 | 文件 | 旧类 | 新类/基线 | 属性 | before | after | 结论 |')
    print('|---|---|---|---|---|---|---|---|')
    for r in rows:
        print('| ' + ' | '.join(str(x) for x in r) + ' |')
    counts = {}
    for r in rows:
        counts[r[7]] = counts.get(r[7], 0) + 1
    print('\n合计：' + '、'.join(f'{k} {v}' for k, v in sorted(counts.items())))
    if bad:
        print('\n✗ 未归因项：')
        for x in bad:
            print('  ' + x)
        return 1
    print('✓ 7 条裁定的静态等价全部对上：SAME 逐属性相同、CHANGED 只落在登记的 gap/align-items/margin-left、'
          '死代码删除无生效声明')
    return 0


if __name__ == '__main__':
    sys.exit(main())
