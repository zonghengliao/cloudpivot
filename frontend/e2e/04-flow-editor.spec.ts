import { expect, test, type Page } from '@playwright/test';
import { assertNavigationOk, centerOf, newApi, openAuthed, toast, RUN, type Api } from './support';

/**
 * 流程 4：流程编辑器画布（VUE-FLOW-3 起默认 Vue Flow；`?canvas=native` 可回退老 SVG 画布）
 *
 * 新建流程 → 进编辑器 → 从左侧节点面板**真拖** 3 个节点到画布 → 拖锚点连线 →
 * 保存 → 刷新重开断言整图还原 → 校验 → 发布 → 断言状态变 PUBLISHED。
 *
 * 图结构（在默认「开始 → 结束」上扩展，保证过 R1–R15）：
 *   开始 → 审批 → 抄送 → 结束(新)，原来的「结束」仍由「开始」直达，全部可达。
 * 两个审批类节点都要配审批人，否则 R12 会拦发布 → 用属性面板选「发起人本人」。
 *
 * ★ 选择器一律走**两套画布都有的**契约属性（`[data-node-key]` / `[data-node-type]` /
 *   `.flow-node__port--out` / `.flow-edge__hit`），而不是老画布的 `.flow-node` 根类：
 *   老画布的 `data-node-key` 在节点 `<g>` 上、Vue Flow 在卡片 div 上，但两者都是
 *   「一节点一元素」，所以计数 / 取 key / 取中心点的语义相同 —— 同一条用例能同时
 *   跑新画布与 `?canvas=native` 回退路径（VUE-FLOW-3 的验收要求：改断言不改意图）。
 */
test.describe('流程4 SVG 流程编辑器', () => {
  const code = `e2e_${RUN}`;
  const name = `[E2E]流程编辑器-${RUN}`;
  let api: Api;
  let defId = '';

  test.beforeAll(async () => {
    api = await newApi();
  });

  test.afterAll(async () => {
    if (defId) {
      await api.deleteFlowDef(defId).catch(() => undefined);
    }
    await api.dispose();
  });

  test('拖节点 / 连线 / 保存还原 / 校验 / 发布', async ({ page }) => {

    await openAuthed(page, '/flow/def');

    // ---------- ① 新建流程（真实弹窗） ----------
    await page.getByRole('button', { name: '新建流程' }).click();
    const dlg = page.locator('.t-dialog').filter({ hasText: '新建流程' }).first();
    await expect(dlg).toBeVisible();
    await dlg.locator("input[placeholder*='小写字母开头']").fill(code);
    await dlg.locator("input[placeholder*='如 任务验收']").fill(name);
    await dlg.locator("input[placeholder*='TASK_ACCEPT']").fill(`E2E_${RUN.toUpperCase()}`);
    await dlg.getByRole('button', { name: '创建并进入编辑器' }).click();

    await expect(page).toHaveURL(/\/flow\/def\/[0-9a-f-]+\/edit/, { timeout: 30_000 });
    defId = page.url().match(/\/flow\/def\/([0-9a-f-]+)\/edit/)![1];
    // 默认图：开始 + 结束 两个节点
    await expect(page.locator('[data-node-key]')).toHaveCount(2, { timeout: 20_000 });

    // ---------- ② 从节点面板拖 3 个节点进画布 ----------
    const canvas = page.locator('.flow-editor__canvas');
    const cbox = (await canvas.boundingBox())!;
    const at = (fx: number, fy: number) => ({ x: cbox.x + cbox.width * fx, y: cbox.y + cbox.height * fy });

    const dropNode = async (type: string, fx: number, fy: number, expectCount: number) => {
      const from = await centerOf(page.locator(`.palette-item[data-node-type="${type}"]`));
      const to = at(fx, fy);
      await page.mouse.move(from.x, from.y);
      await page.mouse.down();
      await page.mouse.move(to.x, to.y, { steps: 15 });
      await page.mouse.up();
      await expect(page.locator('[data-node-key]')).toHaveCount(expectCount, { timeout: 10_000 });
    };

    // 先记下默认图的 key，后面用它把「新拖进来的结束」与默认「结束」区分开
    const initialKeys = await page.$$eval('[data-node-key]', (els) => els.map((el) => (el as HTMLElement).dataset.nodeKey!));

    await dropNode('APPROVAL', 0.35, 0.55, 3);
    await dropNode('CC', 0.6, 0.55, 4);
    await dropNode('END', 0.8, 0.55, 5);

    // 落点可能和默认「开始 / 结束」重叠（叠在一起就点不到锚点）→ 点一次「自动布局」按 dagre 分层铺开
    // （P12：按钮在画布右上角，且会先弹二次确认）
    await page.getByRole('button', { name: '自动布局' }).click();
    await page.locator('.t-dialog').filter({ hasText: '重排所有节点的位置' }).getByRole('button', { name: '开始重排' }).click();
    await expect(toast(page, '已重排')).toBeVisible({ timeout: 10_000 });
    await page.waitForTimeout(800);

    // 读出画布上的节点 key（真实 DOM 属性 → 后面连线要用）
    const nodes = await page.$$eval('[data-node-key]', (els) =>
      els.map((el) => {
        const box = el.getBoundingClientRect();
        return {
          key: (el as HTMLElement).dataset.nodeKey!,
          type: (el as HTMLElement).dataset.nodeType!,
          x: box.x,
          y: box.y,
        };
      }),
    );
    const startKey = nodes.find((n) => n.type === 'START')!.key;
    const approvalKey = nodes.find((n) => n.type === 'APPROVAL')!.key;
    const ccKey = nodes.find((n) => n.type === 'CC')!.key;
    // 两个「结束」：默认那个在打开编辑器时就有了，用 key 差集挑出新拖进来的那个
    const endKey = nodes.filter((n) => n.type === 'END' && !initialKeys.includes(n.key)).pop()!.key;

    // ---------- ③ 拖锚点连线 ----------
    const connect = async (fromKey: string, toKey: string, expectEdges: number) => {
      const port = await centerOf(page.locator(`.flow-node__port--out[data-port-node-key="${fromKey}"], [data-node-key="${fromKey}"] .flow-node__port--out`));
      const target = await centerOf(page.locator(`[data-node-key="${toKey}"]`));
      await page.mouse.move(port.x, port.y);
      await page.mouse.down();
      await page.mouse.move(target.x, target.y, { steps: 15 });
      await page.mouse.up();
      await expect(toast(page, '已连线')).toBeVisible({ timeout: 10_000 });
      await expect(page.locator('.flow-edge__hit')).toHaveCount(expectEdges);
    };

    await connect(startKey, approvalKey, 2); // 默认已有 开始→结束 1 条
    await connect(approvalKey, ccKey, 3);
    await connect(ccKey, endKey, 4);

    // ---------- ④ 给两个审批类节点配审批人（否则 R12 拦发布） ----------
    const setInitiator = async (key: string) => {
      await page.locator(`[data-node-key="${key}"]`).click();
      const item = page.locator('.flow-editor__props .t-form__item').filter({ hasText: '审批人来源' });
      await expect(item).toBeVisible();
      await item.locator('.t-select').click();
      await page.locator('.t-select-option').filter({ hasText: '发起人本人' }).first().click();
      // 选中态：单选框的 input value 就是选项文案
      await expect(item.locator('input').first()).toHaveValue('发起人本人');
    };
    await setInitiator(approvalKey);
    await setInitiator(ccKey);

    // ---------- ⑤ 保存 ----------
    await page.getByRole('button', { name: '保存' }).click();
    await expect(toast(page, '已保存')).toBeVisible({ timeout: 20_000 });

    // ---------- ⑥ 重新打开：整图还原 ----------
    assertNavigationOk(await page.reload(), '流程编辑器整图（reload 还原）');
    await expect(page.locator('[data-node-key]')).toHaveCount(5, { timeout: 20_000 });
    await expect(page.locator('.flow-edge__hit')).toHaveCount(4);
    // 节点名称与类型都在（不是只还原了坐标）
    await expect(page.locator('[data-node-key][data-node-type="APPROVAL"]')).toHaveCount(1);
    await expect(page.locator('[data-node-key][data-node-type="CC"]')).toHaveCount(1);
    await expect(page.locator('[data-node-key][data-node-type="END"]')).toHaveCount(2);

    // ---------- ⑦ 校验（服务端 R1–R15） ----------
    await page.getByRole('button', { name: '校验' }).click();
    await expect(toast(page, '校验通过')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.issue-board')).toHaveCount(0);

    // ---------- ⑧ 发布 ----------
    await page.getByRole('button', { name: '发布' }).click();
    await expect(toast(page, /已发布 v\d+/)).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('.flow-editor__title .t-tag').filter({ hasText: '已发布' })).toBeVisible();
    // 发布后进只读态（这是发布真的落库的证据：版本 editable=false 由后端驱动）
    await expect(page.locator('.flow-editor__alert').filter({ hasText: '已发布' })).toBeVisible();
  });
});
