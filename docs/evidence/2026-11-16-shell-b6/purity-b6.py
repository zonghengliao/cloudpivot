#!/usr/bin/env python3
"""B6 纯净性断言（改名字节级证明）。

只看 `git diff -U0`（零上下文 hunk），对 20 个 .vue 逐 hunk 断言三件事：
  1. **成对 hunk**（#增 == #减）：把两侧行里的「本批类名」（新旧一起）都抹成占位符后，
     必须**逐字节相同** —— 也就是这行只可能改了类名，属性、标点、缩进一个字节都没动；
  2. **纯删除 hunk**（#增 == 0）：内容必须**逐字等于** 7 个已登记删除块之一
     （4 份 `.tool-row--second` 死代码 + `panel-head__actions` + `panel-head__ops` + `.tool-row--filter`）；
  3. **纯新增 hunk**：不允许（本批不存在「只加行」的改动）。
任何一条不满足 → 退出码 1。

用法：python3 purity-b6.py            # 打印每文件 hunk 归类与违规
"""
import re
import subprocess
import sys

REPO = '/opt/cloudpivot'

# 本批出现过的全部类名（旧名 + 新名）。归一化时一律抹平成同一个占位符。
NAMES = [
    # 改名（旧 → 新）
    'tool-row__category', 'account-category',
    'tool-row__dept', 'mapping-dept', 'report-dept',
    'tool-row__year', 'report-year', 'yearend-year',
    'tool-row__month', 'co-month',
    'tool-row__item', 'coh-item',
    'tool-row__tip', 'pay-toolbar-tip',
    'tool-row__switch', 'inv-only-toggle',
    'tool-row__overdue', 'transfer-overdue-toggle',
    'panel-head__account', 'ledger-account',
    'panel-head__link', 'warehouse-link',
    'panel-head__warn', 'serial-warn',
    'panel-head__sub', 'selected-warehouse',
    'panel-head__overdue', 'overdue-badge',
    'list-card__pager', 'list-pager',
    'inv-code',
    # 并入基线（旧名也要抹平，因为新名是同族基线类）
    'panel-head__actions', 'panel-head__ops', 'panel-head__side',
    # inventory 的 tip>code 规则：基线类 `__tip` 在这条规则里当「祖先」用，改名后不再出现
    'panel-head__tip',
    # 提基线 / 弃用
    'tool-row--filter', 'tool-row--second',
]

# 已登记的 7 个「整块删除」（git diff -U0 里的纯删除 hunk，空行忽略后逐字比对）
DELETED_BLOCKS = [
    '.tool-row--second {\n  margin-top: 8px;\n}',
    '.tool-row--second {\n  margin-top: 10px;\n}',
    '.panel-head__actions {\n  display: flex;\n  gap: 8px;\n}',
    '.panel-head__ops {\n  margin-left: auto;\n  display: flex;\n  gap: 8px;\n}',
    '.tool-row--filter {\n  margin-top: 12px;\n}',
]

FILES = [
    'frontend/src/views/business/erp/account/index.vue',
    'frontend/src/views/business/erp/budget/mapping.vue',
    'frontend/src/views/business/erp/budget/report.vue',
    'frontend/src/views/business/erp/cost-object/index.vue',
    'frontend/src/views/business/erp/cost-overhead/index.vue',
    'frontend/src/views/business/erp/cost-period/steps/UncollectedPanel.vue',
    'frontend/src/views/business/erp/expense/pay/index.vue',
    'frontend/src/views/business/erp/inventory/index.vue',
    'frontend/src/views/business/erp/invoice/index.vue',
    'frontend/src/views/business/erp/ledger/index.vue',
    'frontend/src/views/business/erp/location/index.vue',
    'frontend/src/views/business/erp/receipt-refund/index.vue',
    'frontend/src/views/business/erp/receipt/index.vue',
    'frontend/src/views/business/erp/receivable/index.vue',
    'frontend/src/views/business/erp/report/report-item/index.vue',
    'frontend/src/views/business/erp/serial/index.vue',
    'frontend/src/views/business/erp/transfer/index.vue',
    'frontend/src/views/business/erp/warehouse/index.vue',
    'frontend/src/views/business/erp/year-end/index.vue',
    'frontend/src/views/business/help/center/index.vue',
]

PH = '@C@'          # 类名占位符

# 归一后仍允许的成对差异（登记制，每条写理由）
ALLOWED_PAIRS = [
    # inventory：`.panel-head__tip code` → `.inv-code`。规则从「卡片头 tip 里的 code 后代」收成
    # 「页面私有 token」，后代选择器天然变成裸类；属性 4 条逐字节不变（见 §3 表）
    ('.@C@ code {', '.@C@ {', 'inventory 的 tip>code 后代选择器收成页面私有裸类'),
]


def strip_names(line: str) -> str:
    for n in sorted(NAMES, key=len, reverse=True):
        line = line.replace(n, PH)
    # 只含占位符的 class 属性整个抹掉：`<code class="inv-code">` 归一成 `<code>`
    line = re.sub(r'\s*class="\s*' + re.escape(PH) + r'\s*"', '', line)
    return line


def hunks(path: str):
    out = subprocess.run(['git', 'diff', '-U0', 'HEAD', '--', path],
                         cwd=REPO, capture_output=True, text=True, check=True).stdout
    cur = None
    for ln in out.split('\n'):
        if ln.startswith('@@'):
            if cur is not None:
                yield cur
            cur = []
        elif cur is not None and (ln.startswith('+') or ln.startswith('-')):
            cur.append(ln)
    if cur is not None:
        yield cur


def main() -> int:
    stat = {'files': 0, 'hunks': 0, 'paired': 0, 'deleted': 0}
    bad = []
    for f in FILES:
        stat['files'] += 1
        for h in hunks(f):
            stat['hunks'] += 1
            minus = [x[1:] for x in h if x.startswith('-')]
            plus = [x[1:] for x in h if x.startswith('+')]
            if minus and plus:
                if len(minus) != len(plus):
                    bad.append((f, '成对 hunk 行数不等', minus, plus))
                    continue
                norm_m = [strip_names(x) for x in minus]
                norm_p = [strip_names(x) for x in plus]
                if norm_m != norm_p:
                    # 单行 hunk 允许命中登记例外
                    hit = None
                    if len(norm_m) == 1 == len(norm_p):
                        for a, b, why in ALLOWED_PAIRS:
                            if norm_m[0].strip() == a and norm_p[0].strip() == b:
                                hit = why
                                break
                    if hit:
                        stat['allowed'] = stat.get('allowed', 0) + 1
                        print(f'  例外命中（{hit}）：{f}')
                    else:
                        bad.append((f, '归一后仍不同（不是纯改名）', minus, plus))
                elif any(a == b for a, b in zip(minus, plus)):
                    bad.append((f, '两侧完全相同（无意义改动）', minus, plus))
                else:
                    stat['paired'] += 1
            elif minus and not plus:
                body = '\n'.join(x for x in minus if x.strip())
                if body not in DELETED_BLOCKS:
                    bad.append((f, '纯删除 hunk 不属于已登记删除块', minus, []))
                else:
                    stat['deleted'] += 1
            elif plus and not minus:
                bad.append((f, '出现纯新增 hunk（本批不允许）', [], plus))

    print(f'审查 {stat["files"]} 个 .vue / {stat["hunks"]} 个 hunk：'
          f'成对（纯类名替换）{stat["paired"]}、整块删除 {stat["deleted"]}')
    if bad:
        print(f'\n✗ 违规 {len(bad)} 处：')
        for f, why, minus, plus in bad:
            print(f'  {f}  —  {why}')
            for x in minus:
                print('     -', x)
            for x in plus:
                print('     +', x)
        return 1
    print('✓ 通过：所有成对 hunk 归一后逐字节相同（`<style>` 属性一字未改，模板只换 class token），'
          '所有纯删除 hunk 都是登记的 7 个块之一，无纯新增')
    return 0


if __name__ == '__main__':
    sys.exit(main())
