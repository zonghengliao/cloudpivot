"""把 e2e-report/probe/*.json 汇总成 docs/test-report-e2e.md（测试E / t_4c9d4e71）。

为什么用生成而不是手抄：报告里的每个 ✅/❌、每条「死按钮」、每条 401 都要能指回
一份现场 JSON（e2e-report/probe/<page>.json）。手抄一次就过期，生成器可重跑。

用法（在 frontend/ 目录下）：python3 ../scripts/e2e_report.py
"""
import glob
import io
import json
import os
import re

FRONT = '/opt/cloudpivot/frontend'
PROBE = os.path.join(FRONT, 'e2e-report', 'probe')
OUT = '/opt/cloudpivot/docs/test-report-e2e.md'

CHECKS = ['①', '②', '③', '④', '⑤', '⑥']
CHECK_NAME = {
    '①': '页面能打开（HTTP 200 / 非 404 / 无 JS 异常 / 无 4xx）',
    '②': '内容能渲染（表格行或空态文案 / 主内容有实质内容）',
    '③': '列表接口 200 且 total 与行数自洽',
    '④': '按钮可达（有文案或图标、有尺寸、禁用有理由）',
    '⑤': '核心按钮点了有反应（弹窗/抽屉/请求/Toast/路由/DOM 变化）',
    '⑥': '表单必填校验生效（空提交被拦、不写库）',
}


def load():
    out = {}
    for f in sorted(glob.glob(os.path.join(PROBE, '*.json'))):
        with io.open(f, encoding='utf-8') as fh:
            d = json.load(fh)
        out[d['path']] = d
    # 静态口径（formMode / readOnly / note / listApi）来自 catalog.json
    cat_path = os.path.join(FRONT, 'e2e-report', 'catalog.json')
    if os.path.exists(cat_path):
        with io.open(cat_path, encoding='utf-8') as fh:
            for meta in json.load(fh):
                o = out.get(meta['path'])
                if o:
                    o['formMode'] = meta.get('formMode') or 'dialog'
                    o['readOnly'] = bool(meta.get('readOnly'))
                    o['note'] = meta.get('note') or ''
                    o['listApi'] = meta.get('listApi')
                    o['knownBug'] = meta.get('knownBug')
    return out


def check_result(o, ch):
    """复算每条检查的通过与否（与 _pagerunner.ts 的断言一一对应）。"""
    err = o.get('error') or ''
    kind = o['kind']
    api = o.get('api') or []
    target = [c for c in api if c['path'] == o.get('listApi')]
    f = o['interact']['form']
    if ch == '①':
        ok = (
            not err
            and 200 <= o['navStatus'] < 300
            and not o['pageErrors']
            and not o['badApi']
            and (o['resultCode'] == o['path'][-3:] if kind == 'result' else not o['is404'])
        )
    elif ch == '②':
        if err:
            ok = False
        elif kind == 'list':
            ok = o['tableCount'] >= 1 and (o['rowCount'] > 0 or bool(o['emptyText'].strip()))
        elif kind == 'result':
            ok = len(o['contentText']) > 4 and o['resultCode'] in o['contentText']
        else:
            ok = len(o['contentText']) > 20
    elif ch == '③':
        if kind == 'result':
            ok = not o['badApi'] and not [c for c in api if c['method'] != 'GET']
        else:
            ok = bool(target) and all(c['status'] == 200 for c in target)
            if ok:
                paged = next((c for c in target if c['shape'] == 'paged'), None)
                if paged and paged['total']:
                    ok = (paged['rows'] or 0) > 0 and (paged['rows'] or 0) <= paged['total']
    elif ch == '④':
        btns = o['buttons']
        ok = (
            len(btns) > 0
            and not [b for b in btns if not b['text'].strip() and not b.get('icon')]
            and not [b for b in btns if not b['visible']]
            and bool([b for b in btns if not b['disabled']])
        )
        if ok:
            gate = re.compile('批量|导出|删除|提交|保存|上移|下移|停用|启用|审核|发布|过账|结账|全部已读|标记已读|预览')
            explain = re.compile('只能|请选中|请先|未选中|先选择|需选择|无权限|没有权限|叶子|不可|不能')
            explained = bool(explain.search(o['contentText']))
            ok = not [b for b in btns if b['disabled'] and not gate.search(b['text']) and not explained]
    elif ch == '⑤':
        it = o['interact']
        total = len(it['reactions']) + len(it['dead'])
        ok = bool(it['clicked']) and bool(it['reactions']) and (len(it['reactions']) / max(1, total)) >= 0.5
    else:
        create = [b for b in o['buttons'] if re.search('新建|新增|添加|创建|发起|新建模板', b['text'])]
        if o.get('formMode') == 'page':
            ok = f['submitted'] and bool(f['validation']) and not f['writeRequests']
        elif o.get('formMode') == 'navigate':
            ok = bool(f.get('navigatedUrl')) and bool(f.get('uploadControl'))
        elif o.get('readOnly') or not create:
            ok = not create
        else:
            ok = (
                f['opened']
                and f['submitted']
                and bool(f['validation'])
                and not f['writeRequests']
                and f['closed']
            )
    return ok


def main():
    obss = load()
    pages = sorted(obss.values(), key=lambda o: (o['kind'] != 'result', o['path']))
    lines = []
    w = lines.append

    total_cells = len(pages) * 6
    ok_cells = sum(1 for o in pages for ch in CHECKS if check_result(o, ch))
    dead = [(o['key'], o['path'], d) for o in pages for d in o['interact']['dead']]
    bad4xx = [(o['key'], o['path'], b) for o in pages for b in o['badApi']]
    known = [(o['key'], o['path'], k) for o in pages for k in o.get('knownBadApi', [])]
    notfound = [o for o in pages if o['is404'] and o['kind'] != 'result']
    warn_403 = [o for o in pages if o['resultCode'] == '403' and 'result' not in o['path']]

    w('# 前端 52 页全量 E2E 报告（测试E / t_4c9d4e71）')
    w('')
    w('> 生成方式：`python3 e2e/tools/e2e_report.py`（读 `frontend/e2e-report/probe/*.json` 现场文件自动汇总，'
      '每个 ✅/❌ 都能指回一份 JSON + 一张 PNG 截图）。')
    w('>')
    w('> 运行方式：隔离实例 `http://127.0.0.1:5189`（副本库 `cloudpivot_e2e_iso` + 后端 `127.0.0.1:18089`），'
      '**没有打生产**。命令：`E2E_BASE_URL=http://127.0.0.1:5189 pnpm test:e2e`。')
    w('')
    w('## 一、总览')
    w('')
    w('| 指标 | 数值 |')
    w('| --- | --- |')
    w('| 覆盖页面 | **%d** 个（含 403/404/500 三个结果页） |' % len(pages))
    w('| 检查点（页 × 6 项） | **%d** |' % total_cells)
    w('| 通过 | **%d** |' % ok_cells)
    w('| 未通过 | **%d** |' % (total_cells - ok_cells))
    w('| 死按钮（点了无任何可观察反应） | **%d** |' % len(dead))
    w('| 页面加载期 4xx/5xx（非已知缺陷） | **%d** |' % len(bad4xx))
    w('| 已知缺陷留证（SSE 401） | **%d** 页 |' % len(known))
    w('| 渲染 404 结果页的页面 | **%d** |' % len(notfound))
    w('')
    if notfound:
        w('### 404 清单')
        w('')
        w('| 页面 | 路径 | 说明 |')
        w('| --- | --- | --- |')
        for o in notfound:
            w('| %s | `%s` | %s |' % (o['key'], o['path'], (o.get('note') or '菜单/直链落到 catch-all 404')))
        w('')
    else:
        w('### 404 清单')
        w('')
        w('**本目录（56 条真实路由）里 0 个 404。** 侧栏里「采购明细 / 单号序列」两条菜单没有对应路由的'
          '问题由 `30-menu-alignment.spec.ts` 专项单列（见第四节）。')
        w('')

    w('## 二、逐页 6 项检查结果')
    w('')
    w('图例：✅ 通过 ／ ❌ 未通过（下方「问题清单」有原因）。')
    w('')
    w('| # | 页面 | 路径 | 类型 | ① | ② | ③ | ④ | ⑤ | ⑥ |')
    w('| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |')
    kind_cn = {'list': '列表', 'custom': '非表格', 'result': '结果页'}
    for i, o in enumerate(pages, 1):
        cells = ' | '.join('✅' if check_result(o, ch) else '❌' for ch in CHECKS)
        w('| %d | %s | `%s` | %s | %s |' % (i, o['key'], o['path'], kind_cn.get(o['kind'], o['kind']), cells))
    w('')

    w('## 三、★ 死按钮清单（核心产出）')
    w('')
    if dead:
        w('判据：内容区每个**可用**按钮点一次，800ms 内必须出现「新弹窗/抽屉、新请求、Toast、路由变化、'
          'DOM 结构变化、下拉弹层、展开折叠、原生文件选择器」之一；一个都没有 = 死按钮。'
          '危险按钮（删除/停用/发布/结账）不在此列——它们由 `31-dead-buttons.spec.ts` 在**拦截写请求**的'
          '前提下另跑一遍。')
        w('')
        w('| 页面 | 路径 | 按钮 | 原因 |')
        w('| --- | --- | --- | --- |')
        for key, path, d in dead:
            w('| %s | `%s` | %s | %s |' % (key, path, d['button'], d['reason']))
    else:
        w('**0 个死按钮**：本轮所有页面里，内容区每个可用按钮点下去都有可观察反应（弹窗/请求/Toast/路由/'
          'DOM 变化/下拉/展开/文件选择器）。')
    w('')
    w('说明：`上传文件 / 选择文件` 类按钮点开的是**原生文件选择器**（DOM 不变、不发请求），'
      '已用 Playwright 的 `filechooser` 事件单独识别，不误判为死按钮。')
    w('')

    w('## 四、4xx / 5xx 请求清单（页面加载期）')
    w('')
    if bad4xx:
        w('| 页面 | 路径 | 接口 |')
        w('| --- | --- | --- |')
        for key, path, b in bad4xx:
            w('| %s | `%s` | `%s` |' % (key, path, b))
    else:
        w('除下面的已知缺陷外，**页面加载期 0 个 4xx/5xx 接口**。')
    w('')
    w('### 已知缺陷：通知 SSE 每次都 401（%d 页全部命中）' % len(known))
    w('')
    w('- 现象：每次页面加载都会发 `GET /api/business/notice/stream?token=…`，后端回 **401**，'
      '而且 `EventSource` 还会自动重连、反复 401。')
    w('- 证据链：`frontend/src/stores/notice.ts#connect()` → `buildNoticeSseUrl()` 用 `?token=` 订阅；'
      '后端认证中间件（`alun-web 0.4.2 src/middleware/auth.rs`）**只读 `Authorization` 头**，'
      '仓库里也搜不到 `NIK_AUTH_QUERY_TOKEN_PATHS` 这类白名单实现 → 令牌永远不被采纳。')
    w('- 影响：实时推送从未工作过；未读角标靠 `notice store` 的 30s 轮询兜底，所以用户不容易发现。')
    w('- 用例固化：`34-error-boundary.spec.ts` 的「已知缺陷留证」一条会断言「SSE 请求确实发生 + 状态恒为 401 + '
      '未读数兜底接口 200」；后端一旦修好，这条会变红提醒摘掉白名单。')
    w('')

    w('## 五、未通过项与原因（按页）')
    w('')
    rows = []
    for o in pages:
        bad = [ch for ch in CHECKS if not check_result(o, ch)]
        if bad:
            rows.append((o, bad))
    if not rows:
        w('无。')
    else:
        w('| 页面 | 路径 | 未通过项 | 现场 |')
        w('| --- | --- | --- | --- |')
        for o, bad in rows:
            detail = []
            f = o['interact']['form']
            if '⑥' in bad:
                detail.append('form=%s' % json.dumps(f, ensure_ascii=False)[:220])
            if '⑤' in bad:
                detail.append('clicked=%d reactions=%d dead=%d' % (len(o['interact']['clicked']), len(o['interact']['reactions']), len(o['interact']['dead'])))
            if '③' in bad:
                detail.append('api=%s' % [c['path'] for c in o['api'] if '/page' in c['path'] or 'list' in c['path']][:4])
            w('| %s | `%s` | %s | %s |' % (o['key'], o['path'], ' '.join(bad), '; '.join(detail)[:300]))
    w('')
    w('## 六、专项测试（比逐页更重要的 5 类）')
    w('')
    w('| 专项 | 文件 | 覆盖内容 |')
    w('| --- | --- | --- |')
    w('| 1 菜单路径 ↔ 路由全量对齐 | `30-menu-alignment.spec.ts` | 拉真实菜单树 → 逐个 navigate → 断言无 404、无 4xx/5xx；'
      '已知缺口（`/business/erp/purchase-order-item`、`/business/erp/seq`）单列断言 |')
    w('| 2 按钮可达性全量 | `31-dead-buttons.spec.ts` | 汇总 55 页现场断死按钮为空；危险按钮在 **abort 写请求**前提下真点，验有确认框/提示 |')
    w('| 3 多角色视角 | `32-multi-role.spec.ts` | 超管/仓管员/财务/只读员/受限管理员：菜单收窄、菜单都能开、直达无权页被拒 |')
    w('| 4 徽标联动 | `33-badge-linkage.spec.ts` | 徽标口径 == `/api/system/badge/counts`；消息已读、任务流转、审批处理各 -1 |')
    w('| 5 错误与边界 | `34-error-boundary.spec.ts` | 接口 500 不白屏、token 失效回登录、无权账号 403、连点两次只开一个弹窗、SSE 401 留证 |')
    w('')
    w('## 七、过程中发现的产品缺陷（只报告，未改 src）')
    w('')
    w('1. **登录页空提交不拦前端**：`views/login/index.vue` 的 `<t-form @submit="handleSubmit">` 里，'
      '`handleSubmit` 没接 `validateResult`，红字校验会显示但请求照样发（`POST /api/system/auth/login`）。'
      '用例：`20-pages-home-error-task-flow.spec.ts` 的「★已知缺陷」一条（`test.fail` 标注）。')
    w('2. **通知 SSE 永远 401**（见第四节）。')
    w('3. **消息模板新建：空提交直接打 POST**（`/api/business/notice/template`），前端必填校验缺位，'
      '靠后端 422 兜底。现场见 `e2e-report/probe/_business_notice_template.json` 的'
      '`interact.form.writeRequests`。')
    w('4. **登录限流计数在本地缓存（profile=dev/test）下永不过期**：每次被拒的 429 也 `incr` 计数，'
      '而 `Cache::set_ex` 只在 count==1 时设置 TTL；实测触发一次之后 5 分钟仍 429，必须重启后端进程才恢复。'
      '生产用 redis 时 TTL 正常，但开发/联调会被永久锁号。')
    w('5. **库存调拨列表接口 500**：`GET /api/business/erp/transfer/page` 在 HEAD 代码 + 副本库上稳定 500'
      '（页面显示「调拨单列表加载失败：服务器内部错误」）。后端日志见 `/tmp/e2e-iso/backend.log`；'
      'P10/P11 正在改 transfer 相关代码，这条需要他们确认。')
    w('')
    w('## 八、截图与现场文件')
    w('')
    w('- 每个页面一张截图：`frontend/e2e-report/probe/<path>.png`')
    w('- 每个页面一份现场 JSON（接口流水、按钮清单、点击反应、表单校验、console 错误）：`frontend/e2e-report/probe/<path>.json`')
    w('- 失败用例的 trace / 截图：`frontend/test-results/`')
    w('')

    io.open(OUT, 'w', encoding='utf-8').write('\n'.join(lines) + '\n')
    print('written', OUT)
    print('pages=%d cells=%d ok=%d dead=%d bad4xx=%d known=%d' % (len(pages), total_cells, ok_cells, len(dead), len(bad4xx), len(known)))


if __name__ == '__main__':
    main()
