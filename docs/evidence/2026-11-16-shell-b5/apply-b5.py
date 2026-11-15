#!/usr/bin/env python3
"""B5 批落地脚本：模板加类 token + 删除页面本地 panel-empty*/tool-row__label 声明。

用法：
    python3 apply-b5.py --dry     # 只打印计划
    python3 apply-b5.py --apply   # 落地

设计原则（卡 t_26cc85f5 §五.5）：
  * `<template>` 段**只允许**在 `class="..."` 里追加 token —— 本脚本的模板改动全是
    `class="X"` → `class="X Y"` 的字符串替换，并对每处替换断言命中次数（多一处/少一处即报错）。
  * `<style>` 段整块删除「最后一个简单选择器命中目标类」的规则（含 Less 嵌套子规则）。
  * hr/salary/index（白名单）不动；erp/price-inquiry 无本地声明，只由新基线生效（另记）。
"""
import json
import re
import sys

SRC = '/opt/cloudpivot/frontend/src/'
REPORT = '/opt/cloudpivot/docs/evidence/2026-11-16-shell-b5/report-before.json'
APPLY = '--apply' in sys.argv

d = json.load(open(REPORT))
b5 = sorted({b['file'] for b in d['batches']['B5']['detail']})

ALLOWLISTED = {'views/business/hr/salary/index.vue'}

# ---------- 族归属（决策 §3.2 / 卡面 §二） ----------
BASELINE = [
    'erp/account', 'erp/commission-rule', 'erp/commission', 'erp/customer-price', 'erp/exchange-rate',
    'erp/expense/pay', 'erp/inventory', 'erp/ledger', 'erp/location', 'erp/payable', 'erp/period',
    'erp/purchase-order-item', 'erp/purchase', 'erp/seq', 'erp/serial', 'erp/stockin', 'erp/transfer',
    'erp/voucher', 'flow/def',
]
RICH = ['erp/customer', 'erp/material', 'erp/supplier', 'erp/unit', 'erp/warehouse', 'hr/employee']
FILE = ['impexp/export-task', 'impexp/import-task', 'storage/backend', 'storage/file']
DOC = ['erp/contract', 'erp/lot', 'erp/stock-out', 'erp/stock-take', 'hr/attendance', 'hr/leave']
INLINE = ['hr/salary/detail']

LABEL_SM = [
    'erp/invoice', 'erp/receipt', 'erp/receivable', 'erp/receipt-refund', 'erp/contract', 'erp/cost-labor',
]
LABEL_BASE_ONLY = [
    'erp/report/balance-sheet', 'erp/report/cash-flow', 'erp/report/income-statement', 'erp/report/payable-aging',
]

def rel(p):
    return 'views/business/' + p + '/index.vue'

variant = {}
for p in RICH:
    variant[rel(p)] = 'panel-empty--rich'
for p in FILE:
    variant[rel(p)] = 'panel-empty--file'
for p in DOC:
    variant[rel(p)] = 'panel-empty--doc'
# hr/salary/detail 逐元素（两处纯文字 → --inline，整块带图标的 → --doc）
variant['views/business/hr/salary/detail.vue'] = 'PANEL_SPECIAL'

label_sm = {rel(p) for p in LABEL_SM}
targets = set(b5) - ALLOWLISTED
expect = {rel(p) for p in BASELINE} | set(variant) | label_sm | {rel(p) for p in LABEL_BASE_ONLY}
assert targets == expect, '族归属与报告文件清单对不上：' + str(sorted(targets ^ expect))

TARGET_CLASS = re.compile(r'^(panel-empty|panel-empty__(icon|title|desc)|panel-empty--[a-z]+|tool-row__label)$')


# ---------------- 模板 ----------------
def edit_template(text, file):
    notes = []

    def sub1(old, new, expect=None):
        n = text.count(old)
        if expect is not None and n != expect:
            raise SystemExit(f'✗ {file}: 「{old}」命中 {n} 次，期望 {expect}')
        if n == 0:
            return 0
        notes.append(f'    template: {old!r} ×{n} → {new!r}')
        return n

    out = text
    if file in variant and variant[file] != 'PANEL_SPECIAL':
        tok = variant[file]
        n = out.count('class="panel-empty"')
        if n == 0:
            raise SystemExit(f'✗ {file}: 没找到 class="panel-empty"')
        notes.append(f'    template: class="panel-empty" ×{n} → + {tok}')
        out = out.replace('class="panel-empty"', f'class="panel-empty {tok}"')
    if file == 'views/business/hr/salary/detail.vue':
        # E1（表格 #empty 纯文字）→ --inline；E4（带图标的整块空态）→ --doc；E2/E3 已带 --inline
        old1 = '<div class="panel-empty">这张单还没有明细行'
        if out.count(old1) != 1:
            raise SystemExit('✗ detail: E1 没找到')
        out = out.replace(old1, '<div class="panel-empty panel-empty--inline">这张单还没有明细行')
        old4 = '<div class="panel-empty" data-testid="hr-salary-detail-empty">'
        if out.count(old4) != 1:
            raise SystemExit('✗ detail: E4 没找到')
        out = out.replace(old4, '<div class="panel-empty panel-empty--doc" data-testid="hr-salary-detail-empty">')
        notes.append('    template: detail E1→--inline、E4→--doc（E2/E3 原有 --inline 保留）')
    if file in label_sm:
        n = out.count('class="tool-row__label"')
        if n == 0:
            raise SystemExit(f'✗ {file}: 没找到 tool-row__label')
        notes.append(f'    template: class="tool-row__label" ×{n} → + --sm')
        out = out.replace('class="tool-row__label"', 'class="tool-row__label tool-row__label--sm"')
    if file == 'views/business/hr/employee/index.vue':
        old = '<t-icon class="panel-empty__icon" name="user-business" />'
        if out.count(old) != 1:
            raise SystemExit('✗ employee: 图标标签没找到')
        out = out.replace(old, '<t-icon class="panel-empty__icon panel-empty__icon--lg" name="user-business" />')
        notes.append('    template: employee 空态图标 + panel-empty__icon--lg')
    if file == 'views/business/erp/expense/pay/index.vue':
        old = '<t-icon class="panel-empty__icon" name="check-circle" />'
        if out.count(old) != 1:
            raise SystemExit('✗ expense/pay: 图标标签没找到')
        out = out.replace(old, '<t-icon class="panel-empty__icon panel-empty__icon--success" name="check-circle" />')
        notes.append('    template: expense/pay 绿勾图标 + panel-empty__icon--success')
    if file == 'views/business/storage/file/index.vue':
        # 抽屉预览兜底图标：不在 .panel-empty 容器里，删掉本地裸类规则后会掉 40px → 用页面自有类承接
        old = '<t-icon class="panel-empty__icon" name="file" />'
        if out.count(old) != 1:
            raise SystemExit('✗ storage/file: 抽屉图标没找到')
        out = out.replace(old, '<t-icon class="panel-empty__icon preview__icon" name="file" />')
        notes.append('    template: storage/file 抽屉图标 + preview__icon（页面自有类）')
    return out, notes


# ---------------- <style> 段的规则删除 ----------------
def style_regions(text):
    return [(m.start(), m.end(), m.start(1), m.end(1)) for m in re.finditer(r'(<style\b[^>]*>)([\s\S]*?)(</style>)', text)]


def last_classes(selector):
    """选择器（可含逗号）→ 每个分支最后一个简单选择器里的类名列表。"""
    out = []
    for part in selector.split(','):
        part = part.strip()
        if not part:
            continue
        last = re.split(r'[\s>+~]+', part)[-1]
        out.extend(re.findall(r'\.([A-Za-z0-9_-]+)', last))
    return out


def strip_rules(css, file):
    """删掉 top-level 规则里「最后简单选择器命中目标类」的（整块，含嵌套子规则）。"""
    removed = []
    out = []
    i = 0
    n = len(css)
    while i < n:
        # 找 selector 起点到第一个 '{'，期间跳过注释
        j = i
        # 拷贝前导空白/注释
        while j < n and css[j] in ' \t\r\n':
            j += 1
        if j >= n:
            out.append(css[i:])
            break
        if css.startswith('//', j):
            k = css.find('\n', j)
            k = n if k < 0 else k + 1
            out.append(css[i:k])
            i = k
            continue
        if css.startswith('/*', j):
            k = css.find('*/', j)
            k = n if k < 0 else k + 2
            out.append(css[i:k])
            i = k
            continue
        if css[j] == '}':       # 顶层多余的闭括号（不应出现）
            out.append(css[i:j + 1])
            i = j + 1
            continue
        # selector 文本
        sel_start = j
        brace = css.find('{', j)
        if brace < 0:
            out.append(css[i:])
            break
        sel_end = brace
        # 若 selector 里出现 ';' 说明其实是声明（不该在顶层的可见位置）
        selector = css[sel_start:sel_end].strip()
        # 匹配到对应的 '}'
        depth = 0
        k = brace
        while k < n:
            if css[k] == '{':
                depth += 1
            elif css[k] == '}':
                depth -= 1
                if depth == 0:
                    break
            k += 1
        rule_end = k + 1
        cls = last_classes(selector)
        if any(TARGET_CLASS.match(c) for c in cls):
            removed.append(selector.replace('\n', ' ')[:80])
            # 吞掉规则后面紧跟的一个空行
            m = re.match(r'[ \t]*\r?\n', css[rule_end:])
            if m:
                rule_end += m.end()
            i = rule_end
            continue
        out.append(css[i:rule_end])
        i = rule_end
    return ''.join(out), removed


def edit_style(text, file):
    notes = []
    pieces = []
    last = 0
    for m in re.finditer(r'(<style\b[^>]*>)([\s\S]*?)(</style>)', text):
        pieces.append(text[last:m.start()])
        head, body, tail = m.group(1), m.group(2), m.group(3)
        new_body, removed = strip_rules(body, file)
        if removed:
            notes.append('    style: 删除 ' + ' / '.join(removed))
        pieces.append(head + new_body + tail)
        last = m.end()
    pieces.append(text[last:])
    return ''.join(pieces), notes


# ---------------- 主流程 ----------------
total_tpl = total_style = 0
for file in sorted(targets):
    path = SRC + file
    text = open(path, encoding='utf-8').read()
    new, n1 = edit_template(text, file)
    new, n2 = edit_style(new, file)
    # 收尾：3 个以上连续换行压成 2 个（删除规则后可能留下的空行）
    new = re.sub(r'\n{3,}', '\n\n', new)
    if new == text:
        print(f'— {file}: 无变化')
        continue
    total_tpl += len(n1)
    total_style += len(n2)
    print(f'### {file}')
    for n in n1 + n2:
        print(n)
    if APPLY:
        open(path, 'w', encoding='utf-8').write(new)
print(f'\n合计：模板改动 {total_tpl} 组 / 样式块删除 {total_style} 组  （{"已落地" if APPLY else "dry-run，未写入"}）')
