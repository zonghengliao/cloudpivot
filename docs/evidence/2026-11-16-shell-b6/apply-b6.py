#!/usr/bin/env python3
"""B6 落地脚本：26 块 → 提基线 1 块 / 弃用（死代码）4 块 / 并入基线 `__side` 2 块 / 改名 19 块。

做法：先就地解析每条 (file, old, new, count) 规则，**全部命中次数校验通过**才写盘；
任一条不命中 → 打印差异并退出码 1（不留半改状态）。
"""
import sys
from pathlib import Path

ROOT = Path('/opt/cloudpivot/frontend/src')
V = 'views/business/erp'
RULES = []


def r(path, old, new, count=1):
    RULES.append((path, old, new, count))


# ============ A 组：`--second` 收进基线（1 块）+ 4 块死代码删除 ============
r('style/shell.less',
  """.tool-row--dense {
  gap: 8px;
}
""",
  """.tool-row--dense {
  gap: 8px;
}

// 工具行第二排（`report-item` 的筛选条）：位于另一条工具行 / 卡片头之下，要与上一块拉开一档。
// 取 12px 而不是 8/10px —— 那 4 个页面（invoice / receipt / receivable / receipt-refund）的
// `.tool-row--second` 是**死代码**（模板里没有任何元素带这个类，B6 探针实测 0 个元素命中），
// 全站唯一真在用的就是这一页，12px 是它今天的实测值（零观感变化）。
.tool-row--second {
  margin-top: 12px;
}
""", 1)

for f, mt in [('invoice/index.vue', 8), ('receipt/index.vue', 8), ('receivable/index.vue', 8), ('receipt-refund/index.vue', 10)]:
    r(f'{V}/{f}', '.tool-row--second {\n  margin-top: %dpx;\n}\n' % mt, '', 1)

# report-item：`--filter` 与 `--second` 同义（都是「第二排工具行」），合并到已登记的 `--second`，12px 原样
r(f'{V}/report/report-item/index.vue',
  '<div class="tool-row tool-row--filter">',
  '<div class="tool-row tool-row--second">')
r(f'{V}/report/report-item/index.vue', '.tool-row--filter {\n  margin-top: 12px;\n}\n\n', '', 1)

# ============ B 组：两个「卡片头右侧槽」并入基线 `panel-head__side`（0 新类） ============
r(f'{V}/budget/report.vue', '<div class="panel-head__actions">', '<div class="panel-head__side">')
r(f'{V}/budget/report.vue', '.panel-head__actions {\n  display: flex;\n  gap: 8px;\n}\n\n', '', 1)
r(f'{V}/cost-period/steps/UncollectedPanel.vue', '<div class="panel-head__ops">', '<div class="panel-head__side">')
r(f'{V}/cost-period/steps/UncollectedPanel.vue', '.panel-head__ops {\n  margin-left: auto;\n  display: flex;\n  gap: 8px;\n}\n\n', '', 1)

# ============ C 组：19 块 → 改名去壳前缀（属性逐字节不变） ============
# 1) 科目类型下拉
r(f'{V}/account/index.vue', 'class="tool-row__category"', 'class="account-category"')
r(f'{V}/account/index.vue', '.tool-row__category {', '.account-category {')
# 2) 预算映射：部门下拉（180）
r(f'{V}/budget/mapping.vue', 'class="tool-row__dept"', 'class="mapping-dept"')
r(f'{V}/budget/mapping.vue', '.tool-row__dept {', '.mapping-dept {')
# 3) 预算执行：年度（112）/ 部门（160）—— 与 mapping 同名不同值，正是「壳类撒谎」的样本，各自归页面
r(f'{V}/budget/report.vue', 'class="tool-row__year"', 'class="report-year"')
r(f'{V}/budget/report.vue', 'class="tool-row__dept"', 'class="report-dept"')
r(f'{V}/budget/report.vue', '.tool-row__year {', '.report-year {')
r(f'{V}/budget/report.vue', '.tool-row__dept {', '.report-dept {')
# 4) 成本对象：开工期间
r(f'{V}/cost-object/index.vue', 'class="tool-row__month"', 'class="co-month"')
r(f'{V}/cost-object/index.vue', '.tool-row__month {', '.co-month {')
# 5) 费用台账：费用项目
r(f'{V}/cost-overhead/index.vue', 'class="tool-row__item"', 'class="coh-item"')
r(f'{V}/cost-overhead/index.vue', '.tool-row__item {', '.coh-item {')
# 6) 报销付款：工具行口径说明
r(f'{V}/expense/pay/index.vue', 'class="tool-row__tip"', 'class="pay-toolbar-tip"')
r(f'{V}/expense/pay/index.vue', '.tool-row__tip {', '.pay-toolbar-tip {')
# 7) 库存：只在 ListPage 自定义筛选槽里出现的「隐藏零库存」开关
r(f'{V}/inventory/index.vue', 'class="tool-row__switch"', 'class="inv-only-toggle"')
r(f'{V}/inventory/index.vue', '.tool-row__switch {', '.inv-only-toggle {')
# 8) 库存巡检：卡片头 tip 里的行内代码块（装饰，按 §2.2 末行「装饰留在页面」）
r(f'{V}/inventory/index.vue', '.panel-head__tip code {', '.inv-code {')
r(f'{V}/inventory/index.vue', '<code>SUM(biz_inventory.qty)</code>', '<code class="inv-code">SUM(biz_inventory.qty)</code>')
r(f'{V}/inventory/index.vue', "<code>biz_material_serial.status='IN_STOCK'</code>", "<code class=\"inv-code\">biz_material_serial.status='IN_STOCK'</code>")
r(f'{V}/inventory/index.vue', '<code>dry_run=false</code>', '<code class="inv-code">dry_run=false</code>')
r(f'{V}/inventory/index.vue', '<code>biz_inventory_location</code>', '<code class="inv-code">biz_inventory_location</code>')
r(f'{V}/inventory/index.vue', '<code>biz_inventory_txn</code>', '<code class="inv-code">biz_inventory_txn</code>')
# 9) 总账：卡片头「当前科目」
r(f'{V}/ledger/index.vue', 'class="panel-head__account"', 'class="ledger-account"')
r(f'{V}/ledger/index.vue', '.panel-head__account {', '.ledger-account {')
# 10) 库位：卡片头里指向仓库管理的链接（含 hover）
r(f'{V}/location/index.vue', 'class="panel-head__link"', 'class="warehouse-link"')
r(f'{V}/location/index.vue', '.panel-head__link {', '.warehouse-link {')
r(f'{V}/location/index.vue', '.panel-head__link:hover {', '.warehouse-link:hover {')
# 11) 序列号：扫码命中不在本页的提示
r(f'{V}/serial/index.vue', 'class="panel-head__warn"', 'class="serial-warn"')
r(f'{V}/serial/index.vue', '.panel-head__warn {', '.serial-warn {')
# 12) 调拨：超期在途开关 + 页头超期角标（含 hover）
r(f'{V}/transfer/index.vue', 'class="tool-row__overdue"', 'class="transfer-overdue-toggle"')
r(f'{V}/transfer/index.vue', '.tool-row__overdue {', '.transfer-overdue-toggle {')
r(f'{V}/transfer/index.vue', 'class="panel-head__overdue"', 'class="overdue-badge"')
r(f'{V}/transfer/index.vue', '.panel-head__overdue {', '.overdue-badge {')
# hover 是嵌套的 `&:hover`（在同一个块里），改父选择器即覆盖，无需单列一条规则
# 13) 仓库：卡片头「选中仓库」的副标题
r(f'{V}/warehouse/index.vue', 'class="panel-head__sub"', 'class="selected-warehouse"')
r(f'{V}/warehouse/index.vue', '.panel-head__sub {', '.selected-warehouse {')
# 14) 年末结转：会计年度下拉（160）
r(f'{V}/year-end/index.vue', 'class="tool-row__year"', 'class="yearend-year"')
r(f'{V}/year-end/index.vue', '.tool-row__year {', '.yearend-year {')
# 15) 帮助中心：中列分页条
r('views/business/help/center/index.vue', 'class="list-card__pager"', 'class="list-pager"')
r('views/business/help/center/index.vue', '.list-card__pager {', '.list-pager {')


def main() -> int:
    texts = {}
    problems = []
    for path, old, new, count in RULES:
        p = ROOT / path
        if p not in texts:
            texts[p] = p.read_text(encoding='utf-8')
        got = texts[p].count(old)
        if got != count:
            problems.append(f'{path}: 期望命中 {count} 次，实际 {got} 次 —— {old[:60]!r}')
            continue
        texts[p] = texts[p].replace(old, new)
    if problems:
        print('✗ 规则未全部命中，未写盘：')
        for x in problems:
            print('  ' + x)
        return 1
    for p, t in texts.items():
        p.write_text(t, encoding='utf-8')
    print(f'✓ {len(RULES)} 条规则全部命中；改写 {len(texts)} 个文件')
    for p in sorted(texts):
        print('  ' + str(p.relative_to(ROOT)))
    return 0


if __name__ == '__main__':
    sys.exit(main())
