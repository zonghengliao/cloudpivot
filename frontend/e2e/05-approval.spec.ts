import { expect, test } from '@playwright/test';
import { assertNavigationOk, newApi, openAuthed, toast, RUN, type Api } from './support';

/**
 * 流程 5：审批待办 → 审批（同意）
 *
 * 待办用**真接口预置**（建流程 → 存图 → 发布 → 发起实例），页面侧全是真实点击：
 * 「审批中心 → 待我审批」→ 点卡片 → 详情 → 填审批意见 → 同意 → 断言状态变化。
 *
 * 图：开始 → 审批(审批人=发起人本人, 允许自审) → 结束，
 * 于是本账号发起的单子会落在自己的「待我审批」里，一个人就能跑完整条链路。
 */
test.describe('流程5 审批待办 → 同意', () => {
  let api: Api;
  let defId = '';
  let instanceId = '';
  const title = `[E2E]审批单-${RUN}`;

  test.beforeAll(async () => {
    api = await newApi();
    const def = await api.createFlowDef({
      flow_code: `e2e_ap_${RUN}`,
      flow_name: `[E2E]审批单流程-${RUN}`,
      biz_type: `E2E_AP_${RUN.toUpperCase()}`,
    });
    defId = def.id;
    await api.saveFlowGraph(
      def.version_id,
      [
        { key: 'n_start', name: '开始', type: 'START', x: 80, y: 140 },
        {
          key: 'n_approve',
          name: '测试审批',
          type: 'APPROVAL',
          x: 300,
          y: 120,
          assignee_type: 'INITIATOR',
          self_approval: 'ALLOW',
        },
        { key: 'n_end', name: '结束', type: 'END', x: 600, y: 140 },
      ],
      [
        { key: 'e1', from: 'n_start', to: 'n_approve' },
        { key: 'e2', from: 'n_approve', to: 'n_end' },
      ],
    );
    await api.publishFlowDef(defId);
    const inst = await api.createInstance(defId, title);
    instanceId = inst.instance_id;
  });

  test.afterAll(async () => {
    // 清理：先把仍在跑的实例撤回（有在跑实例时流程定义删不掉，会留下脏数据），再删流程
    if (instanceId) {
      const inst = await api.getInstance(instanceId).catch(() => null);
      const running = inst?.instance?.status && ['RUNNING', 'PENDING'].includes(inst.instance.status);
      if (running) {
        await api.withdrawInstance(instanceId).catch(() => undefined);
      }
    }
    if (defId) {
      await api.deleteFlowDef(defId).catch(() => undefined);
    }
    await api.dispose();
  });

  test('待办卡片 → 详情 → 填意见 → 同意 → 状态变化', async ({ page }) => {

    await openAuthed(page, '/flow/task');
    await expect(page.locator('.t-tabs__nav-item', { hasText: '待我审批' })).toBeVisible();

    // ---------- ① 待办列表里找到预置的这条 ----------
    const card = page.locator('.task-card').filter({ hasText: title }).first();
    await expect(card).toBeVisible({ timeout: 20_000 });
    await expect(card).toContainText('待处理');

    // ---------- ② 点卡片进详情 ----------
    await card.locator('.task-card__title').click();
    await expect(page).toHaveURL(new RegExp(`/flow/instance/${instanceId}`));
    await expect(page.locator('.section-title', { hasText: '节点进度' })).toBeVisible();
    await expect(page.locator('.node-item').first()).toBeVisible();
    await expect(page.getByText(title).first()).toBeVisible();

    // ---------- ③ 填审批意见并同意 ----------
    const actionCard = page.locator('.action-card').first();
    await expect(actionCard.getByText('我的审批')).toBeVisible();
    await actionCard.locator("textarea[placeholder*='审批意见']").fill(`E2E 同意意见-${RUN}`);
    await actionCard.getByRole('button', { name: '同意' }).click();

    await expect(toast(page, /已同意/)).toBeVisible({ timeout: 20_000 });

    // ---------- ④ 断言状态变化（页面 + 后端两处都看一眼） ----------
    // 页面：我的审批操作区消失（已不是我的待办），时间线里出现审批意见
    await expect(page.locator('.action-card').filter({ hasText: '我的审批' })).toHaveCount(0, { timeout: 20_000 });
    await expect(page.getByText(`E2E 同意意见-${RUN}`).first()).toBeVisible({ timeout: 20_000 });

    const inst = await api.getInstance(instanceId);
    expect(inst.instance.status, '实例应已通过').toBe('APPROVED');

    // 待办列表里也应该没有它了
    assertNavigationOk(await page.goto('/flow/task'), '/flow/task');
    await expect(page.locator('.task-card').filter({ hasText: title })).toHaveCount(0, { timeout: 20_000 });
    // 「我发起的」里能找到它且是已通过
    await page.locator('.t-tabs__nav-item', { hasText: '我发起的' }).click();
    const mine = page.locator('.task-card').filter({ hasText: title }).first();
    await expect(mine).toBeVisible({ timeout: 20_000 });
    await expect(mine).toContainText('已通过');
  });
});
