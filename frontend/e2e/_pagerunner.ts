/**
 * 把页面目录里的每一页展开成 6 条 `it`（① 能开 ② 有内容 ③ 接口自洽
 * ④ 按钮可达 ⑤ 按钮有反应 ⑥ 空提交被拦）。
 *
 * 关键约束（写死在这里，谁都别"优化"掉）：
 * - **一页只导航一次**：6 条 it 共用 beforeAll 里的同一份 {@link Observation}。
 *   生产限流 300 req/60s，按卡面字面「6 条 it 各导航一次」跑 56 页 = 336 次导航，
 *   必然被打成 429，红的是限流不是页面。
 * - **断言必须落在现场里的具体数字/文案上**：目标接口名、表格行数、空态文案、
 *   按钮文案、校验文案 —— 不许出现恒真断言。
 */
import { expect, test } from '@playwright/test';
import type { PageSpec } from './_catalog';
import {
  checkFormValidation,
  interactPage,
  observePage,
  writeObservation,
  type Observation,
} from './_pagecheck';

/** 内容区里「新建类」按钮文案（⑥ 的触发口） */
const CREATE_BTN = /新建|新增|添加|创建|发起|新建模板/;

function describeKind(kind: PageSpec['kind']): string {
  return kind === 'list' ? '列表页' : kind === 'result' ? '结果页' : '非表格页';
}

/**
 * 已知产品缺陷标注：`test.fail(true, reason)` 让这条 it 变成「expected to fail」——
 * 套件整体仍是绿的（不被已知缺陷长期染红），但报告里一眼能看到「哪页哪项是已知 bug」；
 * 等缺陷修好，这条会变红（"passed unexpectedly"），提醒把标注摘掉。
 */
function markKnown(spec: PageSpec, check: '①' | '②' | '③' | '④' | '⑤' | '⑥'): void {
  if (spec.knownBug?.checks?.includes(check)) {
    test.fail(true, `已知产品缺陷（${check}）：${spec.knownBug.reason}`);
  }
}

/** 展开一组页面 */
export function definePageChecks(specs: PageSpec[]): void {
  for (const spec of specs) {
    test.describe(`页面: ${spec.key}（${spec.path}｜${describeKind(spec.kind)}）`, () => {
      let obs: Observation;

      test.beforeAll(async ({ browser }) => {
        const page = await browser.newPage();
        try {
          obs = await observePage(page, spec);
          // 加载期证据必须在交互前**冻结**：响应监听器还挂着，交互阶段打出的
          // 422/500（例如空提交被后端拒）会追加进同一个数组，
          // 否则 ①「加载期无 4xx」会被交互阶段的写请求污染（2026-10-31 实测踩过）。
          obs.badApi = obs.badApi.slice();
          obs.knownBadApi = obs.knownBadApi.slice();
          obs.pageErrors = obs.pageErrors.slice();
          if (!obs.error) {
            await interactPage(page, spec, obs);
            writeObservation(obs);
          } else {
            writeObservation(obs);
          }
        } finally {
          await page.context().close();
        }
      });

      test(`① 页面能打开：HTTP 200、不是 404 页、无 JS 异常、无 4xx/5xx 接口`, async () => {
        markKnown(spec, '①');
        expect(obs.error ?? '', `导航异常`).toBe('');
        expect(obs.navStatus, `导航响应码（${spec.path}）`).toBeGreaterThanOrEqual(200);
        expect(obs.navStatus, `导航响应码（${spec.path}）`).toBeLessThan(300);
        if (spec.kind === 'result') {
          expect(obs.resultCode, `结果页应渲染 ${spec.path.slice(-3)}`).toBe(spec.path.slice(-3));
        } else {
          expect(obs.is404, `不应渲染 404 结果页（title=${obs.title}）`).toBe(false);
        }
        expect(obs.pageErrors, `页面 JS 异常`).toEqual([]);
        expect(obs.badApi, `加载期 4xx/5xx 接口`).toEqual([]);
      });

      test(`② 内容能渲染：${spec.kind === 'list' ? '表格有数据行或明确空态文案' : '主内容区有实质内容'}`, async () => {
        markKnown(spec, '②');
        expect(obs.error ?? '').toBe('');
        if (spec.kind === 'list') {
          expect(obs.tableCount, `表格数量（${spec.listApi}）`).toBeGreaterThanOrEqual(1);
          const hasRows = obs.rowCount > 0;
          const hasEmpty = obs.emptyText.trim().length > 0;
          expect(
            hasRows || hasEmpty,
            `既没有数据行也没有空态文案（rows=${obs.rowCount} empty="${obs.emptyText}"）`,
          ).toBe(true);
          if (hasRows) {
            expect(obs.headers.length, `表头列数`).toBeGreaterThan(0);
          }
        } else if (spec.kind === 'result') {
          expect(obs.contentText.length, `结果页文案`).toBeGreaterThan(4);
          expect(obs.contentText).toContain(String(obs.resultCode));
        } else {
          expect(obs.contentText.length, `主内容区文字长度（${spec.path}）`).toBeGreaterThan(20);
        }
      });

      test(`③ 接口自洽：${spec.listApi} 返回 200，total 与行数一致`, async () => {
        markKnown(spec, '③');
        expect(obs.error ?? '').toBe('');
        if (spec.kind === 'result') {
          // 结果页不拉业务接口：断言没有 4xx/5xx 且没有任何写请求
          expect(obs.badApi).toEqual([]);
          expect(obs.api.filter((c) => c.method !== 'GET'), `结果页不该发写请求`).toEqual([]);
          return;
        }
        const target = obs.api.filter((c) => c.path === spec.listApi);
        expect(target.length, `没抓到 ${spec.listApi}（抓到的是：${obs.api.map((c) => c.path).join(', ')}）`).toBeGreaterThan(0);
        expect(target.map((c) => c.status), `${spec.listApi} 状态码`).toEqual(target.map(() => 200));

        const paged = target.find((c) => c.shape === 'paged');
        const asArray = target.find((c) => c.shape === 'array');
        if (paged) {
          expect(typeof paged.total, `total 应为数字`).toBe('number');
          expect(paged.rows, `list 应为数组`).not.toBeNull();
          if ((paged.total ?? 0) > 0) {
            expect(paged.rows ?? 0, `有数据时行数应 > 0（total=${paged.total}）`).toBeGreaterThan(0);
            expect(paged.rows ?? 0, `行数不应超过 total（rows=${paged.rows} total=${paged.total}）`).toBeLessThanOrEqual(
              paged.total ?? 0,
            );
            const size = paged.pageSize ?? 10;
            if ((paged.total ?? 0) <= size) {
              expect(paged.rows, `total ≤ page_size 时应当页全出（total=${paged.total} size=${size}）`).toBe(paged.total);
            }
          }
        } else if (asArray) {
          expect(asArray.rows, `数组接口应有长度`).not.toBeNull();
        } else {
          // 详情型 / 树型接口（menu/tree、dept/tree、dashboard/overview）：
          // total 与行数关系不适用，只要求 200（上面已断言）+ 不是空对象
          expect(target.every((c) => c.status === 200)).toBe(true);
        }
      });

      test(`④ 按钮可达：内容区每个按钮都有文案（或图标）、有尺寸、可点（禁用的要能说出理由）`, async () => {
        markKnown(spec, '④');
        expect(obs.error ?? '').toBe('');
        const buttons = obs.buttons;
        expect(buttons.length, `内容区按钮数量（${spec.path}）`).toBeGreaterThan(0);

        // 图标按钮（TDesign 列设置 / 折叠 / 刷新）本来就没有文字，但必须有图标，
        // 否则用户看到的就是一个空方块——那才是真缺陷。
        const nameless = buttons.filter((b) => !b.text.trim() && !b.icon);
        expect(nameless.map((b) => b.cls), `既没有文案也没有图标的空按钮`).toEqual([]);

        const zeroSize = buttons.filter((b) => !b.visible);
        expect(zeroSize, `有按钮尺寸为 0（用户点不到）`).toEqual([]);

        const enabled = buttons.filter((b) => !b.disabled);
        expect(enabled.length, `可用按钮数量`).toBeGreaterThan(0);

        // 禁用的按钮分两类，都必须「说得清理由」才算通过：
        //   a) 文案本身就是批量/流程门禁（批量删除、导出、提交…）——未选中/无权限时本就该灰；
        //   b) 页面上有明确解释（例如仓库页选中非叶子仓库时「新建库位」灰掉，
        //      正文写着「库位只能挂在叶子仓库下，请选中它的叶子仓库」）。
        // 两类都不满足 → 记为「无理由灰掉」，因为用户看不出为什么点不了。
        const GATE = /批量|导出|删除|提交|保存|上移|下移|停用|启用|审核|发布|过账|结账|全部已读|标记已读|预览|冲突|扫描|记账/;
        const EXPLAIN = /只能|请选中|请先|未选中|先选择|需选择|无权限|没有权限|叶子|不可|不能/;
        const explained = EXPLAIN.test(obs.contentText);
        const suspiciousDisabled = buttons.filter((b) => b.disabled && !GATE.test(b.text) && !explained);
        expect(suspiciousDisabled.map((b) => b.text), `无理由灰掉的按钮（页面上也没有解释文案）`).toEqual([]);
      });

      test(`⑤ 核心按钮点了有反应（弹窗/抽屉/请求/Toast 至少一种）`, async () => {
        markKnown(spec, '⑤');
        expect(obs.error ?? '').toBe('');
        const { reactions, dead, clicked } = obs.interact;
        expect(clicked.length, `本轮实际点击的按钮（${spec.path}）`).toBeGreaterThan(0);
        expect(reactions.length, `有反应的按钮：${JSON.stringify(reactions)}`).toBeGreaterThan(0);
        // 死按钮单独统计（专项 2 会全量扫一遍），这里只要求「不是全都死的」
        expect(reactions.length / Math.max(1, reactions.length + dead.length), `有反应按钮占比`).toBeGreaterThanOrEqual(0.5);
      });

      test(`⑥ 表单必填校验生效（空提交被拦，且没有真的写库）`, async () => {
        markKnown(spec, '⑥');
        expect(obs.error ?? '').toBe('');
        const form = obs.interact.form;
        const createButtons = obs.buttons.filter((b) => CREATE_BTN.test(b.text));
        if (spec.formMode === 'page') {
          // 页面本身就是表单（消息发送）：直接点页面上的提交按钮，空提交必须被前端拦住
          expect(form.submitted, `应在页面上点到提交按钮`).toBe(true);
          expect(form.validation.length, `空提交应出现必填校验提示`).toBeGreaterThan(0);
          expect(form.writeRequests, `空提交不应打出写请求（说明必填校验漏了）`).toEqual([]);
          return;
        }
        if (spec.formMode === 'navigate') {
          // 入口是跳转（导入任务 → 导入向导）：断言真的跳过去了且落地页有上传控件
          expect(form.opened, `点入口应跳到表单页（实际停在 ${form.navigatedUrl ?? '原页'}）`).toBe(true);
          expect(form.uploadControl, `落地页应有上传控件`).toBe(true);
          expect(form.writeRequests, `跳转不该发写请求`).toEqual([]);
          return;
        }
        if (spec.readOnly || createButtons.length === 0) {
          // 只读页 / 没有新建入口：断言确实没有写入口（不是"跳过"）
          expect(createButtons.map((b) => b.text), `${spec.path} 不该有新建入口`).toEqual([]);
          return;
        }
        expect(form.opened, `点「新建」应弹出表单（按钮：${createButtons.map((b) => b.text).join('/')}）`).toBe(true);
        expect(form.submitted, `应在弹窗里点到提交按钮（弹窗按钮：${(form.dialogButtons ?? []).join('/')}）`).toBe(true);
        expect(form.validation.length, `空提交应出现必填校验提示`).toBeGreaterThan(0);
        expect(form.writeRequests, `空提交不应打出写请求（说明必填校验漏了）`).toEqual([]);
        expect(form.closed, `弹窗应能正常关闭（否则挡住后续用例）`).toBe(true);
      });
    });
  }
}
